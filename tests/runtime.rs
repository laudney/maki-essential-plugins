use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use maki_agent::SessionMailbox;
use maki_agent::tools::{ToolContext, ToolRegistry, cli_tool_ctx};
use maki_lua::test_support::runtime;
use maki_lua::{
    DiscoveredPackage, Key, ModelRequest, Origin, Permission, PluginHost, PluginPermissions,
    Requested, SessionRequest, UiAction, WinCommand, WinEvent,
};
use maki_pack::approvals::{ApprovalKey, Approvals};
use maki_providers::plugin;
use maki_storage::id::{MakiId, SessionRef};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::time::timeout;
use toml::Value as TomlValue;

const PACKAGE: &str = "maki-essential-plugins";
const PACKAGE_SOURCE: &str = "https://github.com/laudney/maki-essential-plugins";
const WAIT_LIMIT: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(5);
const CLAUDE_CONFIG_ENV: &str = "CLAUDE_CONFIG_DIR";
const RUNTIME_DIR_ENV: &str = "XDG_RUNTIME_DIR";
const CLAUDE_DIR: &str = "claude";
const SESSIONS_DIR: &str = "sessions";
const RUN_DIR: &str = "run";
const PRIVATE_DIR: u32 = 0o700;
const VIEW_CWD: &str = "/work/speakerbus";
const IDLE: &str = "idle";

fn isolated_state() -> TempDir {
    let home = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("HOME", home.path()) };
    for (name, subdir) in [
        ("XDG_CONFIG_HOME", "config"),
        ("XDG_DATA_HOME", "data"),
        ("XDG_STATE_HOME", "state"),
        ("XDG_CACHE_HOME", "cache"),
        ("XDG_RUNTIME_DIR", "runtime"),
    ] {
        unsafe { std::env::set_var(name, home.path().join(subdir)) };
    }
    for name in [
        "RUNINFRA_GATEWAY_KEY",
        "RUNINFRA_API_KEY",
        "FIREWORKS_API_KEY",
        "RUNINFRA_BASE_URL",
        "FIREWORKS_AI_BASE_URL",
    ] {
        unsafe { std::env::remove_var(name) };
    }
    home
}

struct Runtime {
    host: PluginHost,
    registry: Arc<ToolRegistry>,
    ctx: ToolContext,
    mailbox: SessionMailbox,
    focused: Arc<Mutex<MakiId>>,
    flashes: flume::Receiver<String>,
    windows: flume::Receiver<Window>,
    models: Arc<Mutex<Models>>,
    #[cfg_attr(not(feature = "unix_sockets"), allow(dead_code))]
    view: Arc<Mutex<SessionView>>,
    /// Holds `CLAUDE_CONFIG_DIR` and, for [`Runtime::with_claude`], the
    /// runtime directory, so no test reaches the user's Claude Code state.
    #[cfg_attr(not(feature = "unix_sockets"), allow(dead_code))]
    claude: TempDir,
}

/// What the UI answers `maki.session.read` and `maki.session.live` with
/// for the focused session.
struct SessionView {
    cwd: String,
    status: String,
    /// False makes the UI leave `live` unanswered, as a headless host does.
    live: bool,
    /// Focus moves here right after the next `read` is answered, so a test
    /// can change the session at an exact point of the plugin's activation.
    focus_after_read: Option<MakiId>,
}

impl Default for SessionView {
    fn default() -> Self {
        Self {
            cwd: VIEW_CWD.to_owned(),
            status: IDLE.to_owned(),
            live: true,
            focus_after_read: None,
        }
    }
}

#[derive(Default)]
struct Models {
    current: String,
    available: Vec<String>,
    selected: Vec<String>,
    error: Option<String>,
}

struct Window {
    events: flume::Sender<WinEvent>,
    commands: flume::Receiver<WinCommand>,
}

impl Window {
    fn key(&self, notation: &str) {
        self.events
            .send(WinEvent::Key {
                key: Key::parse(notation).unwrap(),
            })
            .unwrap();
    }

    fn cursor(&self) -> usize {
        loop {
            if let WinCommand::SetCursor(row) = self.commands.recv_timeout(WAIT_LIMIT).unwrap() {
                return row + 1;
            }
        }
    }

    fn closed(&self) {
        loop {
            if matches!(
                self.commands.recv_timeout(WAIT_LIMIT).unwrap(),
                WinCommand::Close
            ) {
                return;
            }
        }
    }
}

/// Points `CLAUDE_CONFIG_DIR` into a fresh temporary directory before any
/// package loads. When {in_use}, the config's `sessions` directory and a
/// private runtime directory exist there too. nextest runs each test in its
/// own process, so the variables never leak between tests.
fn prepare_claude(in_use: bool) -> TempDir {
    let claude = tempfile::tempdir().unwrap();
    let config = claude.path().join(CLAUDE_DIR);
    unsafe { std::env::set_var(CLAUDE_CONFIG_ENV, &config) };
    if in_use {
        fs::create_dir_all(config.join(SESSIONS_DIR)).unwrap();
        let run = claude.path().join(RUN_DIR);
        fs::create_dir(&run).unwrap();
        fs::set_permissions(&run, fs::Permissions::from_mode(PRIVATE_DIR)).unwrap();
        unsafe { std::env::set_var(RUNTIME_DIR_ENV, &run) };
    }
    claude
}

/// The models.dev catalog warmed, whichever Maki: newer builds load it
/// without blocking.
fn warm_catalog_now() {
    #[cfg(feature = "async_catalog")]
    runtime().block_on(maki_providers::warm_catalog());
    #[cfg(not(feature = "async_catalog"))]
    maki_providers::warm_catalog();
}

fn catalog_now(slug: &str) -> Option<maki_providers::ProviderData> {
    #[cfg(feature = "async_catalog")]
    return runtime().block_on(maki_providers::catalog_provider(slug));
    #[cfg(not(feature = "async_catalog"))]
    maki_providers::catalog_provider(slug)
}

fn wait_until(mut ready: impl FnMut() -> bool) {
    let until = Instant::now() + WAIT_LIMIT;
    while !ready() {
        assert!(
            Instant::now() < until,
            "runtime condition did not become ready"
        );
        std::thread::sleep(POLL_INTERVAL);
    }
}

impl Runtime {
    fn new() -> Self {
        Self::new_with(|_| {})
    }

    fn with_provider_grants(providers: &[&str]) -> Self {
        Self::configured(|_| {}, providers)
    }

    fn new_with(adjust: impl FnOnce(&mut PluginPermissions)) -> Self {
        Self::configured(adjust, &[])
    }

    fn configured(adjust: impl FnOnce(&mut PluginPermissions), providers: &[&str]) -> Self {
        Self::build(adjust, providers, false, SessionView::default(), |_| {})
    }

    /// A runtime whose Claude Code config exists, so claude-code-speakerbus
    /// activates, with a private runtime directory for its socket.
    fn with_claude() -> Self {
        Self::with_claude_and(|_| {})
    }

    /// [`Self::with_claude`], with {prepare} run on the temporary root before
    /// the package loads, for state the plugin must find at activation.
    fn with_claude_and(prepare: impl FnOnce(&std::path::Path)) -> Self {
        Self::build(|_| {}, &[], true, SessionView::default(), prepare)
    }

    /// [`Self::with_claude`] with the UI answering from {view}.
    #[cfg_attr(not(feature = "unix_sockets"), allow(dead_code))]
    fn with_claude_view(view: SessionView) -> Self {
        Self::build(|_| {}, &[], true, view, |_| {})
    }

    fn build(
        adjust: impl FnOnce(&mut PluginPermissions),
        providers: &[&str],
        claude_in_use: bool,
        view: SessionView,
        prepare: impl FnOnce(&std::path::Path),
    ) -> Self {
        let claude = prepare_claude(claude_in_use);
        prepare(claude.path());
        let view = Arc::new(Mutex::new(view));
        let ui_view = Arc::clone(&view);
        let registry = Arc::new(ToolRegistry::new());
        let host = PluginHost::new(Arc::clone(&registry)).unwrap();
        let id = MakiId::generate();
        let mailbox = SessionMailbox::register(id);
        let focused = Arc::new(Mutex::new(id));
        let ui_focus = Arc::clone(&focused);
        let models = Arc::new(Mutex::new(Models::default()));
        let ui_models = Arc::clone(&models);
        let actions = host.ui_action_rx();
        let (flash_tx, flashes) = flume::unbounded();
        let (window_tx, windows) = flume::unbounded();
        std::thread::spawn(move || {
            while let Ok(action) = actions.recv() {
                match action {
                    UiAction::Session {
                        req: SessionRequest::Current,
                        reply_tx,
                    } => {
                        reply_tx
                            .send(Ok(json!(ui_focus.lock().unwrap().to_string())))
                            .ok();
                    }
                    UiAction::Session {
                        req: SessionRequest::Live,
                        reply_tx,
                    } => {
                        let view = ui_view.lock().unwrap();
                        if view.live {
                            let id = ui_focus.lock().unwrap().to_string();
                            let live = json!([{ "id": id, "title": "", "status": view.status, "updated_at": 0, "focused": true }]);
                            reply_tx.send(Ok(live)).ok();
                        }
                    }
                    UiAction::Session {
                        req: SessionRequest::Read { .. },
                        reply_tx,
                    } => {
                        let mut view = ui_view.lock().unwrap();
                        let mut focus = ui_focus.lock().unwrap();
                        let snapshot = json!({ "id": focus.to_string(), "cwd": view.cwd, "status": view.status });
                        reply_tx.send(Ok(snapshot)).ok();
                        if let Some(next) = view.focus_after_read.take() {
                            *focus = next;
                        }
                    }
                    UiAction::Flash(text) => {
                        flash_tx.send(text).ok();
                    }
                    UiAction::Model { req, reply_tx } => {
                        let mut models = ui_models.lock().unwrap();
                        let reply = match req {
                            ModelRequest::Get => Ok(json!({ "spec": models.current })),
                            ModelRequest::Available => Ok(json!(models.available)),
                            ModelRequest::Set { spec, .. } => match &models.error {
                                Some(error) => Err(error.clone()),
                                None => {
                                    let spec = spec.unwrap();
                                    models.current.clone_from(&spec);
                                    models.selected.push(spec);
                                    Ok(json!({ "spec": models.current }))
                                }
                            },
                        };
                        reply_tx.send(reply).ok();
                    }
                    UiAction::OpenWin {
                        event_tx, cmd_rx, ..
                    } => {
                        window_tx
                            .send(Window {
                                events: event_tx,
                                commands: cmd_rx,
                            })
                            .ok();
                    }
                    _ => {}
                }
            }
        });
        let package = PathBuf::from(std::env::var("MAKI_ESSENTIAL_PACKAGE").unwrap());
        let manifest: TomlValue =
            toml::from_str(&fs::read_to_string(package.join("plugin.toml")).unwrap()).unwrap();
        let requested = Requested::from_manifest(&manifest);
        let mut permissions = requested.clone().granted();
        adjust(&mut permissions);
        plugin::begin_load();
        if providers.is_empty() {
            host.load_package(PACKAGE, &package, permissions, Default::default())
                .unwrap();
        } else {
            let mut approvals = Approvals::default();
            approvals.approve(
                &ApprovalKey::new(PACKAGE, PACKAGE_SOURCE),
                Permission::ALL
                    .iter()
                    .filter(|permission| permissions.is_allowed(**permission))
                    .map(ToString::to_string)
                    .collect(),
                requested.net_hosts().map(<[_]>::to_vec),
                providers.iter().map(|slug| (*slug).to_owned()).collect(),
            );
            let path = maki_lua::approvals_path().unwrap();
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, serde_json::to_vec(&approvals).unwrap()).unwrap();
            let warnings = host.load_packages(
                &[DiscoveredPackage {
                    name: PACKAGE.to_owned(),
                    dir: package,
                    eager: true,
                    requested,
                    origin: Origin::Fetched {
                        src: PACKAGE_SOURCE.to_owned(),
                    },
                    revision_guard: None,
                }],
                &maki_config::PluginsConfig {
                    packages: vec![PACKAGE.to_owned()],
                    ..Default::default()
                },
            );
            assert!(warnings.is_empty(), "{warnings:?}");
        }
        plugin::commit_load();
        let mut ctx = cli_tool_ctx();
        ctx.session_id = Some(SessionRef::from_id(id));
        ctx.registry = Arc::clone(&registry);
        Self {
            host,
            registry,
            ctx,
            mailbox,
            focused,
            flashes,
            windows,
            models,
            view,
            claude,
        }
    }

    fn call(&self, name: &str, input: Value) -> Result<String, String> {
        let invocation = self.registry.get(name).unwrap().tool.parse(&input).unwrap();
        runtime().block_on(async {
            timeout(WAIT_LIMIT, invocation.execute(&self.ctx))
                .await
                .unwrap_or_else(|_| panic!("tool {name} did not return"))
                .output
                .map(|output| output.as_text())
        })
    }

    fn command(&self, args: &str) -> String {
        self.flash_of("/goal", args)
    }

    /// Runs {command} and answers the flash it shows.
    fn flash_of(&self, command: &str, args: &str) -> String {
        while self.flashes.try_recv().is_ok() {}
        self.host.event_handle().run_command(
            Arc::from(PACKAGE),
            Arc::from(command),
            args.into(),
            0,
        );
        self.flashes.recv_timeout(WAIT_LIMIT).unwrap()
    }

    fn has_tool(&self, name: &str) -> bool {
        self.registry.get(name).is_some()
    }

    fn fire(&self, event: &str, extra: Value) {
        let mut data = extra.as_object().unwrap().clone();
        data.insert(
            "session_id".into(),
            json!(self.ctx.session_id.as_ref().unwrap().as_str()),
        );
        self.fire_autocmd(event, Value::Object(data));
    }

    // Fires `data` as given. Newer Maki moved the raw call off EventHandle.
    fn fire_autocmd(&self, event: &str, data: Value) {
        let handle = self.host.event_handle();
        #[cfg(feature = "test_support_autocmd")]
        maki_lua::test_support::fire_autocmd(&handle, event, data);
        #[cfg(not(feature = "test_support_autocmd"))]
        handle.fire_autocmd(event, data);
    }

    fn hints(&self) -> String {
        self.host
            .hint_reader()
            .load_full()
            .visible_in(*self.focused.lock().unwrap())
            .flat_map(|entry| entry.spans.iter().map(|(text, _)| text.as_str()))
            .collect()
    }

    fn notices(&self) -> Vec<String> {
        let notices = self.mailbox.drain();
        let messages = notices.iter();
        messages
            .map(|message| message.user_text().unwrap().to_owned())
            .collect()
    }

    fn goal(&self) -> Value {
        let id = self.ctx.session_id.as_ref().unwrap().as_str();
        let path = PathBuf::from(std::env::var("XDG_STATE_HOME").unwrap())
            .join("maki/goals/sessions")
            .join(format!("{id}.json"));
        serde_json::from_str::<Value>(&fs::read_to_string(path).unwrap()).unwrap()["goal"].clone()
    }

    fn goal_running(&self) {
        assert!(!self.command("Ship the feature").contains("Goal error"));
        assert_eq!(self.notices().len(), 1);
        self.fire("TurnStart", json!({"text": "goal"}));
        wait_until(|| self.hints().contains("Pursuing goal"));
    }

    fn picker(&self) -> Window {
        self.host.event_handle().run_command(
            Arc::from(PACKAGE),
            Arc::from("/monitors"),
            String::new(),
            0,
        );
        self.windows.recv_timeout(WAIT_LIMIT).unwrap()
    }
}

struct Gate(PathBuf);

impl Gate {
    fn new() -> Self {
        let path = PathBuf::from(std::env::var("XDG_CACHE_HOME").unwrap())
            .join(MakiId::generate().to_string());
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&path)
                .status()
                .unwrap()
                .success()
        );
        Self(path)
    }

    fn command(&self) -> String {
        format!("read _ < '{}'", self.0.display())
    }
}

impl Drop for Gate {
    fn drop(&mut self) {
        fs::remove_file(&self.0).ok();
    }
}

mod fireworks;
mod goals;
mod monitors;
mod providers;
mod runinfra;
#[cfg(feature = "unix_sockets")]
mod speakerbus;

/// A Maki without `maki.net.listen_unix` keeps claude-code-speakerbus off,
/// even where Claude Code is in use, and shows the model no tools.
#[cfg(not(feature = "unix_sockets"))]
#[test]
fn speakerbus_stays_off_without_unix_sockets() {
    let runtime = Runtime::with_claude();
    assert!(!runtime.has_tool("claude_sessions"));
    assert!(!runtime.has_tool("claude_send"));
    let answer = runtime.flash_of("/speakerbus", "");
    assert!(answer.contains("maki.net.listen_unix"), "{answer}");
}
