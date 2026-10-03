# Maki essential plugins

Two Lua plugins for [Maki](https://github.com/tontinton/maki): keep an agent
working toward a goal, and get notified when a background command makes progress
or finishes.

**Requires upstream Maki 0.6.0 or later. No fork is required.** Use Maki's
interactive terminal UI for the examples below. If you have not installed Maki,
start with its [installation guide](https://github.com/tontinton/maki#installation).

## Install

1. Add this line to `~/.config/maki/init.lua`. Create the file and its parent
   directory if they do not exist:

   ```lua
   maki.pack.add({ "https://github.com/laudney/maki-essential-plugins" })
   ```

2. Start Maki, or restart it if it is already open.
3. Review and approve the package permissions when Maki asks. The goal plugin
   reads and writes saved goal state; the monitor plugin runs shell commands.
4. Type `/goal` to check that the package loaded. You should see
   `No goal is set for this session.`

The package is installed in Maki's XDG data directory, normally
`~/.local/share/maki/site/pack/core/`. These plugins are loaded at startup.

## Try a goal

In a Maki session, type a concrete objective:

```text
/goal Fix the failing tests and verify the fix
```

Maki starts work on the objective. If a turn finishes while the goal is still
active, the plugin asks the agent to continue. The agent can mark the goal
complete or blocked, with a summary of the result.

| Command | Action |
| --- | --- |
| `/goal` | Show the objective and its status |
| `/goal pause` | Pause automatic continuation |
| `/goal resume` | Resume a paused, blocked, or interrupted goal |
| `/goal clear` | Remove the saved goal |
| `/goal <objective>` | Set or replace the goal and start work |

The status bar shows the goal's state. Cancelling a goal turn stops its
automatic continuation; use `/goal resume` to continue. Goals have no built-in
turn or spending limit. Pausing a goal does not stop a command already running.

Goals are saved per session. Reopening a session or reloading the package does
not automatically restart its goal.

## Try a monitor

Ask the agent to watch a command. For a Rust project, for example:

```text
Run cargo nextest run in a monitor with wake enabled and match set to
the Lua pattern "Summary". End your turn, then report the test result
when the command finishes.
```

The agent calls the `monitor` tool and asks for permission to run the command.
You can keep using Maki while it runs. With `wake = true`, a reported line or
the command's exit can start a new agent turn. With wake off, reports wait in
the session mailbox until the next turn.

Type `/monitors` or press **Ctrl+M** to see this session's running monitors:

- **Up/Down** selects a monitor.
- **d** stops the selected monitor.
- **Esc** closes the list.

Ctrl+M requires a terminal with Kitty keyboard protocol support; `/monitors`
works without that shortcut. You can also ask the agent to list or stop monitors.

The `match` option filters output with a **Lua pattern**. For example, `ERROR`
reports only lines that contain that text. Lua patterns do not support regex
alternation: use `%|` for a literal pipe. An exit report is sent even if no line
matches. Each monitor reports at most 200 matching output lines, followed by a
limit notice; the exit report is separate.

Monitors belong to the session that started them. They stop when that session
ends, the package reloads, or Maki exits. The picker and automatic wake examples
require the interactive UI; a headless run does not keep a watcher alive after
Maki exits. Some forks also show a compact report in the transcript. Upstream
Maki receives the same report through its session mailbox.

## Update

Use `/packupdate maki-essential-plugins` to review updates.

## Agent tools

| Tool | Purpose |
| --- | --- |
| `get_goal` | Read the current session's goal and status |
| `update_goal` | Mark a matching goal execution complete or blocked |
| `monitor` | Start a background command with optional label, match, and wake |
| `monitor_list` | List this session's monitors and their IDs |
| `monitor_stop` | Stop one of this session's monitors by ID |

## Development

Run the Lua state tests and formatting check from the repository root:

```sh
LUA_PATH='./lua/?.lua;;' lua tests/spec.lua
stylua --check lua plugin tests
```

The integration tests load this package in Maki's real Lua runtime, run shell
jobs, and exercise goal events and picker keys. They use temporary XDG
directories and do not call a model or use your saved Maki configuration.
Install Python 3.11+, Rust 1.99.0, and `cargo-nextest`, then point the runner at
an upstream Maki source checkout:

```sh
python3 tests/runtime.py /path/to/maki --lint
```

Use the same command with a fork checkout to check both targets. When building
with system OpenSSL, set `OPENSSL_NO_VENDOR=1` as described in Maki's build
instructions. Build files are cached in `.cache/runtime/`, or in
`CARGO_TARGET_DIR` if set.

CI tests upstream `v0.6.0` and `main` on each push and pull request, and weekly
to detect upstream API changes. It also checks Lua state tests, Python lint,
and formatting.

## License

[MIT](LICENSE).
