use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use maki_agent::SessionMailbox;
use maki_agent::tools::{ToolContext, ToolRegistry, cli_tool_ctx};
use maki_lua::{
    Key, Permission, PluginHost, PluginPermissions, SessionRequest, UiAction, WinCommand, WinEvent,
};
use maki_storage::id::{MakiId, SessionRef};
use serde_json::{Value, json};

const PACKAGE: &str = "maki-essential-plugins";
const WAIT_LIMIT: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(5);

struct Runtime {
    host: PluginHost,
    registry: Arc<ToolRegistry>,
    ctx: ToolContext,
    mailbox: SessionMailbox,
    focused: Arc<Mutex<MakiId>>,
    flashes: flume::Receiver<String>,
    windows: flume::Receiver<Window>,
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

fn wait_until(mut ready: impl FnMut() -> bool) {
    let until = Instant::now() + WAIT_LIMIT;
    while !ready() {
        assert!(
            Instant::now() < until,
            "runtime condition did not become ready"
        );
        smol::block_on(smol::Timer::after(POLL_INTERVAL));
    }
}

impl Runtime {
    fn new() -> Self {
        let registry = Arc::new(ToolRegistry::new());
        let host = PluginHost::new(Arc::clone(&registry)).unwrap();
        let id = MakiId::generate();
        let mailbox = SessionMailbox::register(id);
        let focused = Arc::new(Mutex::new(id));
        let ui_focus = Arc::clone(&focused);
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
                    UiAction::Flash(text) => {
                        flash_tx.send(text).ok();
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
        let manifest: toml::Value =
            toml::from_str(&fs::read_to_string(package.join("plugin.toml")).unwrap()).unwrap();
        let mut permissions = PluginPermissions::denied();
        for permission in Permission::ALL {
            let requested = manifest["permissions"]
                .get(permission.manifest_key())
                .and_then(toml::Value::as_bool)
                .unwrap_or(false);
            permissions.set(*permission, requested);
        }
        host.load_package(PACKAGE, &package, permissions, Default::default())
            .unwrap();
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
        }
    }

    fn call(&self, name: &str, input: Value) -> Result<String, String> {
        let invocation = self.registry.get(name).unwrap().tool.parse(&input).unwrap();
        smol::block_on(async {
            smol::future::or(
                async {
                    invocation
                        .execute(&self.ctx)
                        .await
                        .output
                        .map(|output| output.as_text())
                },
                async {
                    smol::Timer::after(WAIT_LIMIT).await;
                    panic!("tool {name} did not return")
                },
            )
            .await
        })
    }

    fn command(&self, args: &str) -> String {
        while self.flashes.try_recv().is_ok() {}
        self.host.event_handle().run_command(
            Arc::from(PACKAGE),
            Arc::from("/goal"),
            args.into(),
            0,
        );
        self.flashes.recv_timeout(WAIT_LIMIT).unwrap()
    }

    fn fire(&self, event: &str, extra: Value) {
        let mut data = extra.as_object().unwrap().clone();
        data.insert(
            "session_id".into(),
            json!(self.ctx.session_id.as_ref().unwrap().as_str()),
        );
        self.host
            .event_handle()
            .fire_autocmd(event, Value::Object(data));
    }

    fn hints(&self) -> String {
        self.host
            .hint_reader()
            .load_full()
            .entries
            .iter()
            .flat_map(|(_, spans)| spans.iter().map(|(text, _)| text.as_str()))
            .collect()
    }

    fn notices(&self) -> Vec<String> {
        let notices = self.mailbox.drain();
        #[cfg(feature = "notice_envelope")]
        let messages = notices.iter().map(|notice| &notice.message);
        #[cfg(not(feature = "notice_envelope"))]
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

mod goals;
mod monitors;
