use std::fs;
use std::io::{ErrorKind, Read, Write};
use std::net::Shutdown;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::Duration;

use maki_agent::SessionMailbox;
use maki_agent::prompt::{PromptId, Slot};
use maki_agent::tools::ToolAudience;
use maki_lua::Permission;
use maki_providers::Message;
use maki_storage::id::MakiId;
use serde_json::{Value, json};
use test_case::test_case;

use super::{PACKAGE, Runtime, SessionView, wait_until};

const HEADER: &str =
    "The message below is not from your user: another session on this machine sent it.";
const OFF: &str = "claude-code-speakerbus is off";
const SOCKET_DIR: &str = "cc-socks";
const CLAUDE_DIR: &str = "claude";
const SESSIONS_DIR: &str = "sessions";
const RUN_DIR: &str = "run";
const PEER_NAME: &str = "claude-peer";
const PEER_SESSION: &str = "0f8fad5b-d9cb-469f-a165-70867728950e";
const OTHER_SESSION: &str = "11111111-2222-4333-8444-555555555555";
const FOREIGN_START: &str = "1";
const DEAD_PID: u32 = 999_999_999;
const SLUG_NAME_PREFIX: &str = "maki-speakerbus-";
const DELIVERED: &str = "Delivered to the inbox of";
const NOT_SENT: &str = "not sent";
const OUTCOME_UNKNOWN: &str = "outcome unknown";
const MAX_FRAME_BYTES: usize = 1024 * 1024;
/// More than a Unix socket buffers by default, so a receiver that never
/// reads parks the write, and still under one frame.
const PARKING_BODY_BYTES: usize = 1_000_000;
const SOCKET_BUFFER_DEFAULT: &str = "/proc/sys/net/core/wmem_default";
const MAX_CONNECTIONS: usize = 8;
const DENIED: &str = "denied";
const NO_UI: &str = "no interactive UI";
const CLOSE_WAIT: Duration = Duration::from_secs(5);
const PRIVATE_DIR: u32 = 0o700;
const READ_ONLY_DIR: u32 = 0o500;
const SHARED_DIR: u32 = 0o755;
/// Writable by everyone and not sticky, so anyone could swap what is in it.
const OPEN_DIR: u32 = 0o777;
const NOT_PRIVATE: &str = "is not a private directory";
/// The test envelope's `from` is no live peer's socket, so the sender is unknown.
const DISPLAY_LINE: &str = "Peer claude-peer: hello maki";
const HINT_HEADING: &str = "## Session bus";
const NO_PEERS: &str = "No other sessions are running.";
const CLAUDE_ENTRYPOINT: &str = "cli";
const MAKI_ENTRYPOINT: &str = "maki";

fn pid() -> u32 {
    std::process::id()
}

fn start_of(pid: u32) -> String {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    let after = &stat[stat.rfind(')').unwrap() + 1..];
    after.split_whitespace().nth(19).unwrap().to_owned()
}

fn sessions_in(root: &Path) -> PathBuf {
    root.join(CLAUDE_DIR).join(SESSIONS_DIR)
}

fn sockets_in(root: &Path) -> PathBuf {
    root.join(RUN_DIR).join(SOCKET_DIR)
}

fn private_dir(path: &Path) {
    fs::create_dir_all(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(PRIVATE_DIR)).unwrap();
}

impl Runtime {
    fn slot(&self) -> PathBuf {
        sessions_in(self.claude.path()).join(format!("{}.json", pid()))
    }

    fn entry(&self) -> Option<Value> {
        let text = fs::read_to_string(self.slot()).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// Waits until the plugin finished activating, and answers its entry.
    fn active(&self) -> Value {
        wait_until(|| self.entry().is_some() && self.call("claude_sessions", json!({})).is_ok());
        self.entry().unwrap()
    }

    fn socket(&self) -> PathBuf {
        PathBuf::from(self.active()["messagingSocketPath"].as_str().unwrap())
    }

    /// Waits until a tool says the plugin is off.
    fn stays_off(&self) -> String {
        let mut answer = String::new();
        wait_until(|| {
            answer = self.call("claude_sessions", json!({})).unwrap_err();
            !answer.contains("starting")
        });
        answer
    }

    /// Writes {lines}, closes the write side, and waits until the plugin
    /// closes the connection, by which time it has handled every line.
    fn send_lines(&self, lines: &[String]) {
        let mut stream = UnixStream::connect(self.socket()).unwrap();
        for line in lines {
            stream.write_all(line.as_bytes()).unwrap();
            stream.write_all(b"\n").unwrap();
        }
        stream.shutdown(Shutdown::Write).unwrap();
        wait_for_close(&mut stream);
    }

    /// The main agent's `tool_usage` prompt hints, as the next run sees them.
    fn tool_usage_hints(&self) -> String {
        let slots = self.host.event_handle().collect_prompt_slots();
        slots
            .get(PromptId::System, Slot::ToolUsage)
            .iter()
            .map(|entry| entry.content.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn woken(&self) -> Vec<Notice> {
        woken(&self.mailbox)
    }

    fn focus(&self, id: MakiId) {
        *self.focused.lock().unwrap() = id;
    }
}

struct Notice {
    text: String,
    display: Option<String>,
}

impl Notice {
    fn all(messages: Vec<Message>) -> Vec<Self> {
        messages
            .into_iter()
            .map(|message| Self {
                text: message.first_text_content().unwrap().to_owned(),
                display: message.display_text.clone(),
            })
            .collect()
    }
}

/// Every notice pending once one of them asked to wake the session.
fn woken(mailbox: &SessionMailbox) -> Vec<Notice> {
    let mut found = Vec::new();
    wait_until(|| {
        found = Notice::all(mailbox.claim_wake());
        !found.is_empty()
    });
    found
}

/// Every notice pending, once [`Runtime::send_lines`] returned, and none of
/// them asked to wake the session.
fn arrived_quietly(mailbox: &SessionMailbox) -> Vec<Notice> {
    assert!(
        mailbox.claim_wake().is_empty(),
        "a quiet notice asked to wake"
    );
    Notice::all(mailbox.drain())
}

/// Reads until the plugin closes or resets {stream}, within the deadline.
fn wait_for_close(stream: &mut UnixStream) {
    stream.set_read_timeout(Some(CLOSE_WAIT)).unwrap();
    let mut rest = Vec::new();
    match stream.read_to_end(&mut rest) {
        Ok(_) => {}
        Err(e) => assert!(
            !matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut),
            "the plugin did not close the connection"
        ),
    }
}

fn only(notices: Vec<Notice>) -> Notice {
    assert_eq!(
        notices.len(),
        1,
        "{:?}",
        notices.iter().map(|n| &n.text).collect::<Vec<_>>()
    );
    notices.into_iter().next().unwrap()
}

fn envelope(body: &str) -> String {
    format!(
        "<cross-session-message from=\"uds:/run/peer.sock\" from-name=\"{PEER_NAME}\" from-mode=\"prompting\">\n{body}\n</cross-session-message>"
    )
}

/// A `next` frame whose `session_id` is {session_id}, of any JSON type.
fn user_frame_with(body: &str, session_id: Value) -> String {
    let mut frame: Value = serde_json::from_str(&user_frame(body, None, None)).unwrap();
    frame["session_id"] = session_id;
    frame.to_string()
}

fn user_frame(body: &str, priority: Option<&str>, session_id: Option<&str>) -> String {
    let mut frame = json!({
        "msgV": 1,
        "msg_id": PEER_SESSION,
        "type": "user",
        "message": { "role": "user", "content": envelope(body) },
        "from": "uds:/run/peer.sock",
    });
    if let Some(priority) = priority {
        frame["priority"] = json!(priority);
    }
    if let Some(session_id) = session_id {
        frame["session_id"] = json!(session_id);
    }
    frame.to_string()
}

/// A live Claude Code session as the registry shows it: a child process for
/// the PID and start time, an entry, and a socket the test reads.
struct Peer {
    child: Child,
    listener: UnixListener,
    socket: PathBuf,
}

impl Peer {
    fn at(root: &Path, name: &str) -> Self {
        Self::with(root, name, CLAUDE_ENTRYPOINT, PEER_SESSION)
    }

    /// A peer that {entrypoint} runs, as the registry shows it.
    fn with(root: &Path, name: &str, entrypoint: &str, session: &str) -> Self {
        let child = Command::new("sleep").arg("60").spawn().unwrap();
        let pid = child.id();
        private_dir(&sockets_in(root));
        let socket = sockets_in(root).join(format!("{pid}.sock"));
        let listener = UnixListener::bind(&socket).unwrap();
        let body = json!({
            "pid": pid,
            "sessionId": session,
            "cwd": "/work/peer",
            "entrypoint": entrypoint,
            "startedAt": 0,
            "procStart": start_of(pid),
            "peerProtocol": 1,
            "messagingSocketPath": socket,
            "name": name,
            "status": "idle",
        });
        fs::write(
            sessions_in(root).join(format!("{pid}.json")),
            body.to_string(),
        )
        .unwrap();
        Self {
            child,
            listener,
            socket,
        }
    }

    /// The lines of a connection that is already waiting, without blocking
    /// when there is none, after which the listener stays non-blocking.
    fn received_now(&self) -> Vec<String> {
        self.listener.set_nonblocking(true).unwrap();
        let (mut stream, _) = self.listener.accept().expect("no connection waiting");
        stream.set_nonblocking(false).unwrap();
        let mut text = String::new();
        stream.read_to_string(&mut text).unwrap();
        text.lines().map(str::to_owned).collect()
    }

    /// The lines one sender wrote, once it closed the connection.
    fn received(&self) -> Vec<String> {
        let (mut stream, _) = self.listener.accept().unwrap();
        let mut text = String::new();
        stream.read_to_string(&mut text).unwrap();
        text.lines().map(str::to_owned).collect()
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        self.child.kill().ok();
        self.child.wait().ok();
    }
}

#[test]
fn the_entry_is_published_and_the_socket_listens() {
    let runtime = Runtime::with_claude();
    let entry = runtime.active();
    assert_eq!(entry["pid"], json!(pid()));
    assert_eq!(entry["procStart"], json!(start_of(pid())));
    assert_eq!(
        entry["sessionId"],
        json!(runtime.ctx.session_id.as_ref().unwrap().as_str())
    );
    assert_eq!(entry["cwd"], json!("/work/speakerbus"));
    assert_eq!(entry["status"], json!("idle"));
    assert_eq!(entry["peerProtocol"], json!(1));
    assert_eq!(entry["peerFeatures"], json!([]));
    assert_eq!(entry["entrypoint"], json!("maki"));
    assert_eq!(entry["name"], json!(format!("{SLUG_NAME_PREFIX}{}", pid())));
    let socket = runtime.socket();
    assert_eq!(
        socket,
        sockets_in(runtime.claude.path()).join(format!("{}.sock", pid()))
    );
    assert!(UnixStream::connect(&socket).is_ok());
}

#[test]
fn without_claude_code_the_plugin_stays_off_and_adds_no_tools() {
    let runtime = Runtime::new();
    assert!(!runtime.has_tool("claude_sessions"));
    assert!(!runtime.has_tool("claude_send"));
    assert!(runtime.flash_of("/speakerbus", "").contains(OFF));
}

#[test_case(Permission::FsRead ; "fs_read")]
#[test_case(Permission::Env ; "env")]
fn a_denied_grant_keeps_the_plugin_off_but_loaded(denied: Permission) {
    let runtime = Runtime::build(
        |permissions| permissions.set(denied, false),
        &[],
        true,
        SessionView::default(),
        |_| {},
    );
    assert!(!runtime.has_tool("claude_sessions"));
    assert!(runtime.flash_of("/speakerbus", "").contains(OFF));
}

#[test]
fn a_next_frame_becomes_a_waking_notice() {
    let runtime = Runtime::with_claude();
    runtime.active();
    runtime.send_lines(&[user_frame("hello maki", Some("next"), None)]);
    let notice = only(runtime.woken());
    assert!(notice.text.starts_with(HEADER), "{}", notice.text);
    assert!(
        notice.text.ends_with(&envelope("hello maki")),
        "{}",
        notice.text
    );
    assert_eq!(notice.display.as_deref(), Some(DISPLAY_LINE));
}

#[test]
fn a_later_frame_does_not_wake() {
    let runtime = Runtime::with_claude();
    runtime.active();
    runtime.send_lines(&[user_frame("not urgent", Some("later"), None)]);
    let notice = only(arrived_quietly(&runtime.mailbox));
    assert!(notice.text.contains("not urgent"));
}

#[test]
fn only_user_frames_are_delivered_whatever_session_they_name() {
    let runtime = Runtime::with_claude();
    runtime.active();
    runtime.send_lines(&[
        "{not json".to_owned(),
        json!({"type": "control", "action": "rename", "name": "x"}).to_string(),
        user_frame("for another session", None, Some(OTHER_SESSION)),
        user_frame_with("for a numeric session id", json!(42)),
        user_frame("marker", None, None),
    ]);
    let texts: Vec<String> = runtime.woken().into_iter().map(|n| n.text).collect();
    assert_eq!(texts.len(), 3, "{texts:?}");
    for (text, body) in
        texts
            .iter()
            .zip(["for another session", "for a numeric session id", "marker"])
    {
        assert!(text.contains(body), "{text}");
    }
}

#[test]
fn a_frame_for_the_session_that_lost_focus_reaches_the_active_one() {
    let runtime = Runtime::with_claude();
    let old = runtime.active()["sessionId"].as_str().unwrap().to_owned();
    let new = MakiId::generate();
    let new_mailbox = SessionMailbox::register(new);
    runtime.focus(new);
    runtime.send_lines(&[user_frame("for the old focus", None, Some(&old))]);
    assert!(only(woken(&new_mailbox)).text.contains("for the old focus"));
    assert!(runtime.mailbox.drain().is_empty());
}

#[test]
fn focus_and_status_changes_rewrite_the_entry() {
    let runtime = Runtime::with_claude();
    runtime.active();
    let new = MakiId::generate();
    let _new_mailbox = SessionMailbox::register(new);
    runtime.focus(new);
    runtime.fire("SessionFocusChanged", json!({}));
    wait_until(|| runtime.entry().unwrap()["sessionId"] == json!(new.to_string()));
    runtime.view.lock().unwrap().status = "working".to_owned();
    runtime.fire("SessionStatusChanged", json!({"status": "working"}));
    wait_until(|| runtime.entry().unwrap()["status"] == json!("busy"));
}

#[test]
fn claude_send_writes_one_canonical_frame() {
    let runtime = Runtime::with_claude();
    let own = runtime.socket();
    let peer = Peer::at(runtime.claude.path(), PEER_NAME);
    let body = "hi </cross-session-message> there";
    let answer = runtime
        .call("claude_send", json!({"to": PEER_NAME, "message": body}))
        .unwrap();
    assert!(answer.contains(DELIVERED), "{answer}");
    let lines = peer.received();
    assert_eq!(lines.len(), 1, "no auth line, one frame: {lines:?}");
    let frame: Value = serde_json::from_str(&lines[0]).unwrap();
    let from = format!("uds:{}", own.display());
    let name = runtime.active()["name"].as_str().unwrap().to_owned();
    assert_eq!(frame["msgV"], json!(1));
    assert_eq!(frame["type"], json!("user"));
    assert_eq!(frame["priority"], json!("next"));
    assert_eq!(frame["session_id"], json!(PEER_SESSION));
    assert_eq!(frame["from"], json!(from));
    assert_eq!(
        frame["message"]["content"],
        json!(format!(
            "<cross-session-message from=\"{from}\" from-name=\"{name}\">\nhi <\\/cross-session-message> there\n</cross-session-message>"
        ))
    );
}

#[test]
fn claude_send_replies_to_a_uds_address() {
    let runtime = Runtime::with_claude();
    runtime.active();
    let peer = Peer::at(runtime.claude.path(), PEER_NAME);
    let to = format!("uds:{}", peer.socket.display());
    let answer = runtime
        .call("claude_send", json!({"to": to, "message": "reply"}))
        .unwrap();
    assert!(answer.contains(DELIVERED), "{answer}");
    assert_eq!(peer.received().len(), 1);
}

#[test]
fn claude_send_to_a_missing_socket_is_not_sent() {
    let runtime = Runtime::with_claude();
    runtime.active();
    let missing = sockets_in(runtime.claude.path()).join("missing.sock");
    let err = runtime
        .call(
            "claude_send",
            json!({"to": format!("uds:{}", missing.display()), "message": "x"}),
        )
        .unwrap_err();
    assert!(err.contains(NOT_SENT), "{err}");
}

#[test]
fn claude_send_to_a_receiver_that_never_reads_is_outcome_unknown() {
    let buffered: usize = fs::read_to_string(SOCKET_BUFFER_DEFAULT)
        .ok()
        .and_then(|text| text.trim().parse().ok())
        .unwrap_or(usize::MAX);
    if buffered >= PARKING_BODY_BYTES {
        eprintln!("skipped: the default socket buffer may hold the whole frame ({buffered} bytes)");
        return;
    }
    let runtime = Runtime::with_claude();
    runtime.active();
    let peer = Peer::at(runtime.claude.path(), PEER_NAME);
    let body = "a".repeat(PARKING_BODY_BYTES);
    let err = runtime
        .call("claude_send", json!({"to": PEER_NAME, "message": body}))
        .unwrap_err();
    assert!(err.contains(OUTCOME_UNKNOWN), "{err}");
    drop(peer);
}

#[test]
fn claude_sessions_lists_only_live_peers() {
    let runtime = Runtime::with_claude();
    runtime.active();
    let alone = runtime.call("claude_sessions", json!({})).unwrap();
    assert_eq!(alone.lines().nth(1), Some(NO_PEERS), "{alone}");
    let peer = Peer::at(runtime.claude.path(), PEER_NAME);
    let sessions = sessions_in(runtime.claude.path());
    let stale = |pid: u32, start: &str, socket: &Path| {
        let entry = json!({
            "pid": pid, "sessionId": OTHER_SESSION, "procStart": start, "peerProtocol": 1,
            "messagingSocketPath": socket, "name": format!("stale-{pid}"),
        });
        fs::write(sessions.join(format!("{pid}.json")), entry.to_string()).unwrap();
    };
    stale(DEAD_PID, FOREIGN_START, &peer.socket);
    let sleeper = Command::new("sleep").arg("60").spawn().unwrap();
    stale(sleeper.id(), FOREIGN_START, &peer.socket);
    let mut no_socket = Command::new("sleep").arg("60").spawn().unwrap();
    stale(
        no_socket.id(),
        &start_of(no_socket.id()),
        &sockets_in(runtime.claude.path()).join("gone.sock"),
    );

    let listed = runtime.call("claude_sessions", json!({})).unwrap();
    assert_eq!(listed.lines().count(), 2, "{listed}");
    assert!(
        listed.lines().nth(1).unwrap().starts_with(PEER_NAME),
        "{listed}"
    );
    let mut sleeper = sleeper;
    for child in [&mut sleeper, &mut no_socket] {
        child.kill().ok();
        child.wait().ok();
    }
}

#[test]
fn anything_at_the_pid_socket_name_is_left_and_a_suffix_used() {
    let mut taken = None;
    let runtime = Runtime::with_claude_and(|root| {
        private_dir(&sockets_in(root));
        let dangling = sockets_in(root).join(format!("{}.sock", pid()));
        symlink(root.join("nowhere"), &dangling).unwrap();
        taken = Some(dangling);
    });
    let socket = runtime.socket();
    let taken = taken.unwrap();
    assert_ne!(socket, taken);
    let name = socket.file_name().unwrap().to_str().unwrap().to_owned();
    assert!(name.starts_with(&format!("{}-", pid())), "{name}");
    assert!(
        fs::symlink_metadata(&taken)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn a_slot_owned_by_another_process_keeps_the_plugin_off() {
    let foreign =
        json!({"pid": pid(), "procStart": FOREIGN_START, "sessionId": OTHER_SESSION}).to_string();
    let written = foreign.clone();
    let runtime = Runtime::with_claude_and(move |root| {
        fs::write(sessions_in(root).join(format!("{}.json", pid())), written).unwrap();
    });
    assert!(runtime.stays_off().contains("owned by another process"));
    assert_eq!(fs::read_to_string(runtime.slot()).unwrap(), foreign);
}

#[test]
fn a_name_another_peer_holds_gets_a_suffix() {
    let mut early = None;
    let wanted = format!("{SLUG_NAME_PREFIX}{}", pid());
    let runtime = Runtime::with_claude_and(|root| early = Some(Peer::at(root, &wanted)));
    assert_eq!(runtime.active()["name"], json!(format!("{wanted}-2")));
    drop(early);
}

#[test]
fn a_failed_first_write_turns_the_plugin_off_and_closes_the_socket() {
    let runtime = Runtime::with_claude_and(|root| {
        fs::set_permissions(sessions_in(root), fs::Permissions::from_mode(READ_ONLY_DIR)).unwrap();
    });
    let off = runtime.stays_off();
    assert!(off.to_lowercase().contains(DENIED), "{off}");
    let socket = sockets_in(runtime.claude.path()).join(format!("{}.sock", pid()));
    assert!(!socket.exists());
    fs::set_permissions(
        sessions_in(runtime.claude.path()),
        fs::Permissions::from_mode(PRIVATE_DIR),
    )
    .unwrap();
}

#[test_case("shutdown" ; "shutdown")]
#[test_case("reload" ; "reload")]
fn session_end_removes_the_entry_and_the_socket(reason: &str) {
    let runtime = Runtime::with_claude();
    let socket = runtime.socket();
    runtime.fire("SessionEnd", json!({"reason": reason}));
    wait_until(|| runtime.entry().is_none() && !socket.exists());
}

#[test]
fn unload_removes_the_socket_and_leaves_the_entry() {
    let runtime = Runtime::with_claude();
    let socket = runtime.socket();
    runtime.host.unload(PACKAGE).unwrap();
    wait_until(|| !socket.exists());
    assert!(runtime.entry().is_some());
}

#[test]
fn a_line_of_a_full_frame_closes_the_connection() {
    let runtime = Runtime::with_claude();
    let socket = runtime.socket();
    let mut big = "x".repeat(MAX_FRAME_BYTES);
    big.push('\n');
    big.push_str(&user_frame("after the big line", None, None));
    big.push('\n');
    let mut stream = UnixStream::connect(socket).unwrap();
    let mut writer = stream.try_clone().unwrap();
    let writing = std::thread::spawn(move || {
        writer.write_all(big.as_bytes()).ok();
    });
    wait_for_close(&mut stream);
    writing.join().unwrap();
    runtime.send_lines(&[user_frame("marker", None, None)]);
    assert!(only(runtime.woken()).text.contains("marker"));
}

#[test]
fn the_entry_stays_in_the_test_directory() {
    let runtime = Runtime::with_claude();
    runtime.active();
    assert!(runtime.slot().starts_with(runtime.claude.path()));
}

#[test]
fn without_an_interactive_ui_the_plugin_stays_off() {
    let runtime = Runtime::with_claude_view(SessionView {
        live: false,
        ..SessionView::default()
    });
    assert!(runtime.stays_off().contains(NO_UI));
    assert!(runtime.entry().is_none());
    // The hint is registered here, so this shows its callback honours off.
    assert!(!runtime.tool_usage_hints().contains(HINT_HEADING));
}

#[test]
fn a_session_change_during_activation_reaches_the_entry() {
    let moved = MakiId::generate();
    let runtime = Runtime::with_claude_view(SessionView {
        focus_after_read: Some(moved),
        ..SessionView::default()
    });
    runtime.active();
    wait_until(|| runtime.entry().unwrap()["sessionId"] == json!(moved.to_string()));
}

#[test]
fn a_ninth_connection_is_closed_until_one_ends() {
    let runtime = Runtime::with_claude();
    let socket = runtime.socket();
    let idle: Vec<UnixStream> = (0..MAX_CONNECTIONS)
        .map(|_| UnixStream::connect(&socket).unwrap())
        .collect();
    let mut ninth = UnixStream::connect(&socket).unwrap();
    ninth.set_read_timeout(Some(CLOSE_WAIT)).unwrap();
    let frame = format!("{}\n", user_frame("from the ninth", None, None));
    ninth.write_all(frame.as_bytes()).ok();
    let mut rest = Vec::new();
    assert!(
        ninth.read_to_end(&mut rest).is_ok(),
        "the ninth connection was not closed"
    );
    drop(idle);
    let mut delivered = Vec::new();
    wait_until(|| {
        let mut stream = UnixStream::connect(&socket).unwrap();
        stream
            .write_all(format!("{}\n", user_frame("marker", None, None)).as_bytes())
            .ok();
        drop(stream);
        delivered.extend(Notice::all(runtime.mailbox.claim_wake()));
        !delivered.is_empty()
    });
    assert!(
        delivered
            .iter()
            .all(|notice| notice.text.contains("marker"))
    );
}

#[test]
fn a_shared_socket_directory_keeps_the_plugin_off() {
    let runtime = Runtime::with_claude_and(|root| {
        fs::create_dir_all(sockets_in(root)).unwrap();
        fs::set_permissions(sockets_in(root), fs::Permissions::from_mode(SHARED_DIR)).unwrap();
    });
    assert!(runtime.stays_off().contains(NOT_PRIVATE));
    assert!(runtime.entry().is_none());
}

#[test]
fn a_shared_directory_above_the_socket_keeps_the_plugin_off() {
    let runtime = Runtime::with_claude_and(|root| {
        fs::set_permissions(root.join(RUN_DIR), fs::Permissions::from_mode(OPEN_DIR)).unwrap();
    });
    assert!(runtime.stays_off().contains(NOT_PRIVATE));
    assert!(runtime.entry().is_none());
}

/// Each `claude_sessions` row as its label, program, and `to:` value.
fn destinations(listed: &str) -> Vec<(String, String, String)> {
    listed
        .lines()
        .skip(1)
        .map(|row| {
            let mut fields = row.split(" · ");
            let label = fields.next().unwrap().to_owned();
            let program = fields.next().unwrap().to_owned();
            let to = row.rsplit_once("to: ").unwrap().1.to_owned();
            (label, program, to)
        })
        .collect()
}

#[test]
fn claude_sessions_names_itself_and_each_program() {
    let runtime = Runtime::with_claude();
    let name = runtime.active()["name"].as_str().unwrap().to_owned();
    let claude = Peer::with(
        runtime.claude.path(),
        "cc-peer",
        CLAUDE_ENTRYPOINT,
        PEER_SESSION,
    );
    let maki = Peer::with(
        runtime.claude.path(),
        "maki-peer",
        MAKI_ENTRYPOINT,
        OTHER_SESSION,
    );
    let listed = runtime.call("claude_sessions", json!({})).unwrap();
    let mut lines = listed.lines();
    assert_eq!(
        lines.next(),
        Some(format!("You are {name} (Maki).").as_str())
    );
    let rows: Vec<&str> = lines.collect();
    let row_of = |label: &str| *rows.iter().find(|row| row.starts_with(label)).unwrap();
    assert!(row_of("cc-peer").contains(" · claude-code · "), "{listed}");
    assert!(row_of("maki-peer").contains(" · maki · "), "{listed}");
    assert!(
        row_of("cc-peer").ends_with(&format!("to: uds:{}", claude.socket.display())),
        "{listed}"
    );
    assert!(
        row_of("maki-peer").ends_with(&format!("to: uds:{}", maki.socket.display())),
        "{listed}"
    );
}

#[test]
fn every_listed_destination_reaches_its_peer() {
    let runtime = Runtime::with_claude();
    runtime.active();
    let root = runtime.claude.path();
    let sessions = [
        "aaaaaaaa-0000-4000-8000-000000000001",
        "aaaaaaaa-0000-4000-8000-000000000002",
        "aaaaaaaa-0000-4000-8000-000000000003",
        "aaaaaaaa-0000-4000-8000-000000000004",
        "aaaaaaaa-0000-4000-8000-000000000005",
    ];
    // Each peer, with the label and program its row must show.
    let peers = [
        (
            Peer::with(root, "gpu<1>", CLAUDE_ENTRYPOINT, sessions[0]),
            "gpu1",
            "claude-code",
        ),
        (
            Peer::with(root, "twin", CLAUDE_ENTRYPOINT, sessions[1]),
            "twin",
            "claude-code",
        ),
        (
            Peer::with(root, "twin", MAKI_ENTRYPOINT, sessions[2]),
            "twin",
            "maki",
        ),
        (
            Peer::with(
                root,
                "uds:/tmp/elsewhere.sock",
                CLAUDE_ENTRYPOINT,
                sessions[3],
            ),
            "uds:/tmp/elsewhere.sock",
            "claude-code",
        ),
        (
            Peer::with(root, sessions[0], CLAUDE_ENTRYPOINT, sessions[4]),
            sessions[0],
            "claude-code",
        ),
    ];
    let listed = runtime.call("claude_sessions", json!({})).unwrap();
    let rows = destinations(&listed);
    assert_eq!(rows.len(), peers.len(), "{listed}");
    for (index, (_, label, program)) in peers.iter().enumerate() {
        let (_, _, to) = rows
            .iter()
            .find(|(row_label, row_program, _)| row_label == label && row_program == program)
            .unwrap_or_else(|| panic!("no row for {label} ({program}): {listed}"));
        let message = format!("ping {index}");
        let sent = runtime.call("claude_send", json!({"to": to, "message": message}));
        assert!(sent.unwrap().starts_with(DELIVERED));
    }
    // claude_send returns once its connection is closed, so each peer holds
    // exactly the one connection that was meant for it.
    for (index, (peer, _, _)) in peers.iter().enumerate() {
        let lines = peer.received_now();
        assert_eq!(lines.len(), 1, "peer {index}");
        assert!(
            lines[0].contains(&format!("ping {index}")),
            "peer {index}: {}",
            lines[0]
        );
        assert!(
            peer.listener.accept().is_err(),
            "peer {index} got a second frame"
        );
    }
}

#[test]
fn the_tools_are_for_the_main_agent_only() {
    let runtime = Runtime::with_claude();
    for name in ["claude_sessions", "claude_send"] {
        let tool = &runtime.registry.get(name).unwrap().tool;
        assert_eq!(tool.audience(), ToolAudience::MAIN, "{name}");
    }
}

/// A `next` frame whose envelope says {name} at {socket} sent {body}.
fn frame_from(body: &str, socket: &Path, name: &str) -> String {
    let from = format!("uds:{}", socket.display());
    let content = format!(
        "<cross-session-message from=\"{from}\" from-name=\"{name}\">\n{body}\n</cross-session-message>"
    );
    json!({
        "msgV": 1,
        "msg_id": PEER_SESSION,
        "type": "user",
        "message": { "role": "user", "content": content },
        "from": from,
    })
    .to_string()
}

#[test]
fn notices_name_the_sender_program() {
    let runtime = Runtime::with_claude();
    runtime.active();
    let root = runtime.claude.path();
    let maki = Peer::with(root, "maki-gpu-7", MAKI_ENTRYPOINT, PEER_SESSION);
    let claude = Peer::with(root, "tensorfold-63", CLAUDE_ENTRYPOINT, OTHER_SESSION);
    let cases = [
        (
            frame_from("from maki", &maki.socket, "maki-gpu-7"),
            "Sender: Maki session \"maki-gpu-7\".",
            "Maki maki-gpu-7: from maki",
        ),
        (
            frame_from("from claude", &claude.socket, "tensorfold-63"),
            "Sender: Claude Code session \"tensorfold-63\".",
            "Claude Code tensorfold-63: from claude",
        ),
        (
            frame_from("from nowhere", &sockets_in(root).join("gone.sock"), "ghost"),
            "Sender: unknown session \"ghost\".",
            "Peer ghost: from nowhere",
        ),
    ];
    for (frame, sender_line, display) in cases {
        runtime.send_lines(&[frame]);
        let notice = only(runtime.woken());
        let mut lines = notice.text.lines();
        assert_eq!(lines.next(), Some(HEADER), "{}", notice.text);
        assert_eq!(lines.next(), Some(sender_line), "{}", notice.text);
        assert_eq!(notice.display.as_deref(), Some(display));
    }
}

#[test]
fn a_registered_peer_name_wins_over_the_envelope_claim() {
    let runtime = Runtime::with_claude();
    runtime.active();
    let peer = Peer::with(
        runtime.claude.path(),
        "real-name",
        CLAUDE_ENTRYPOINT,
        PEER_SESSION,
    );
    runtime.send_lines(&[frame_from("hi", &peer.socket, "your-user")]);
    let notice = only(runtime.woken());
    assert!(
        notice
            .text
            .contains("Sender: Claude Code session \"real-name\"."),
        "{}",
        notice.text
    );
}

#[test]
fn the_prompt_names_this_peer() {
    let runtime = Runtime::with_claude();
    let name = runtime.active()["name"].as_str().unwrap().to_owned();
    let hints = runtime.tool_usage_hints();
    assert!(hints.contains(HINT_HEADING), "{hints}");
    assert!(hints.contains(&format!("reach you as {name}.")), "{hints}");
}

#[test]
fn without_claude_code_there_is_no_session_bus_prompt() {
    let runtime = Runtime::new();
    assert!(!runtime.tool_usage_hints().contains(HINT_HEADING));
}
