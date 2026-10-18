# Maki essential plugins

Lua plugins for [Maki](https://github.com/tontinton/maki): keep an agent
working toward a goal, get notified when a background command makes progress
or finishes, switch between providers, and optionally use RunInfra's own model
discovery.

**Goals, monitors, and provider switching require Maki 0.6.0 or later.**
The optional RunInfra provider requires Maki 0.6.3 or later, which includes the
explicit provider selection API described below. Use Maki's interactive terminal
UI for the examples. If you have not installed Maki, start with its
[installation guide](https://github.com/tontinton/maki#installation).

## Install

1. Add this line to `~/.config/maki/init.lua`. Create the file and its parent
   directory if they do not exist:

   ```lua
   maki.pack.add({ "https://github.com/laudney/maki-essential-plugins" })
   ```

2. Start Maki, or restart it if it is already open.
3. Review and approve the package permissions when Maki asks. The goal plugin
   reads and writes saved goal state and starts the turns that continue a
   goal. The monitor plugin runs shell commands and can start a turn when a
   wake report arrives. RunInfra needs environment access for its key and
   network access to `api.runinfra.ai`. To have this package implement
   RunInfra, select it in `providers.toml` as shown in
   [Optional RunInfra provider](#optional-runinfra-provider).
4. Type `/goal` to check that the package loaded. You should see
   `No goal is set for this session.`

The package is installed in Maki's XDG data directory, normally
`~/.local/share/maki/site/pack/core/`. These plugins are loaded at startup.

## Switch providers

Maki saves credentials separately for each provider. Log in to each provider
once, keeping your existing logins:

```sh
maki auth login anthropic
maki auth login zai
```

In the interactive UI, type `/provider` to choose a provider, then one of its
models. Type to filter either list, use **Up/Down** to select, and press
**Enter** to confirm or **Esc** to cancel. The current provider and model are
marked in their lists.

| Command | Action |
| --- | --- |
| `/provider` | Choose a provider, then a model |
| `/provider zai` | Choose a model from Z.AI |
| `/provider anthropic/claude-sonnet-4-6` | Select a model directly |

The plugin uses Maki's current model list and model policy. If a new login is
missing from the list, open `/model` to refresh discovery, then reopen
`/provider`. Discovery can take a moment after startup. An explicit
`provider/model` also works for models absent from the list, subject to Maki's
normal validation.

Switching selects the model for the focused session and lets Maki save the
last selection. Credentials stay in Maki's own store. A provider change during
an active run takes effect on the next run. Changing sessions closes the
picker.

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

## Optional RunInfra provider

RunInfra works without this package. Maki reaches it through its
[Models.dev](https://models.dev/) entry with the generic OpenAI-compatible
client, so a key saved under `runinfra` keeps working. This package implements
RunInfra, including its authenticated model discovery, only when you select it
in `providers.toml`.

The selection API is `maki.provider.is_selected` and the `providers.toml`
`plugin` field, merged in
[Maki PR #1216](https://github.com/tontinton/maki/pull/1216) and released in
Maki 0.6.3. Goals and monitors still load on Maki releases that lack provider
selection.

To enable it:

1. Add the selection to `~/.config/maki/providers.toml`, keeping any existing
   RunInfra settings in the same entry:

   ```toml
   [runinfra]
   plugin = "maki-essential-plugins"
   ```

2. Reload plugins with `/reload`, or restart Maki. Maki reads the selection
   when plugins load.

Existing `runinfra/<model>` settings and keys saved under `runinfra` still
work. To use an environment key, set `RUNINFRA_GATEWAY_KEY` before starting
Maki. The older `RUNINFRA_API_KEY` alias is also supported; a non-empty gateway
key takes priority when both are set. With neither variable set, Maki uses the
saved RunInfra key. You can set `RUNINFRA_BASE_URL` to send requests through
your own gateway, including its `/v1` path.

[Models.dev](https://github.com/anomalyco/models.dev) is a public database of
model metadata, prices, and capabilities. It does not hold your RunInfra key
or serve inference requests. Its listing can differ from the models available
to your account. This plugin instead calls RunInfra's authenticated
[`/v1/models` endpoint](https://runinfra.ai/docs/api-reference/models).
It uses the returned inference IDs, positive token limits, published input
modalities, and reasoning effort levels. Prices stay in USD per million
tokens. Absent or invalid token limits and incomplete input/output prices
stay unspecified.

The explicit selection identifies which installed package may receive the
saved key for `runinfra`. Installing this package alone does not replace the
catalog-backed implementation. If the selected package is missing or fails to
load, RunInfra stays unavailable; Maki does not fall back to the generic
client. Removing `plugin` and reloading restores it. The network host
permission and Maki's existing provider checks still apply.

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
lua tests/runinfra.lua
stylua --check lua plugin tests
```

The integration tests load this package in Maki's real Lua runtime, run shell
jobs, and exercise goal events, provider switching, and picker keys. With the
provider selection API present, they also replay fixed RunInfra responses and
check saved keys, stale catalog metadata, and request effort settings. They use temporary XDG
directories and recorded local HTTP responses; they do not call a remote model
or use your saved Maki configuration.
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
and formatting. The runner enables provider tests only when the source checkout
contains the selection API. Reused Maki code and fixed fixtures are documented
in [THIRD_PARTY.md](THIRD_PARTY.md).

## License

[MIT](LICENSE).
