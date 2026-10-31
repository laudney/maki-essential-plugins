# Maki essential plugins

Lua plugins for [Maki](https://github.com/tontinton/maki): keep an agent
working toward a goal, get notified when a background command makes progress
or finishes, switch between providers, and optionally use the official RunInfra
and Fireworks AI model APIs for discovery.

**Requires [Maki 0.7.0](https://github.com/tontinton/maki/releases/tag/v0.7.0)
or later.** This is the first release with the session permissions and
provider ownership APIs this package uses. The manifest enforces this
minimum version.

Use Maki's interactive terminal UI for the examples. For installation steps,
see Maki's [installation guide](https://github.com/tontinton/maki#installation).

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
   wake report arrives. Both post notices into the session, which needs
   `session_write`. The `/provider` command switches the session model, which
   needs `session_settings`. The goal plugin follows turn and tool events, and
   `session_read` lets those events arrive whole, without a warning. It uses
   only their session id and never reads the conversation. RunInfra needs
   environment access for its key and network access to `api.runinfra.ai`.
   Fireworks AI needs network access to `api.fireworks.ai` and environment
   access for its key. The prompt also grants the package the `runinfra` and
   `fireworks-ai` provider names, as shown in
   [Provider names](#provider-names).
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

Maki 0.7 has built-in background commands and subagents. Use a monitor when
you need reports for matching log lines. `/monitors` manages these watchers;
`/tasks` manages Maki's built-in background work.

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
Maki exits. Reports appear as compact transcript notices when Maki accepts
them into a turn. The model receives the full report through the session
mailbox.

## Provider names

`plugin.toml` lists the provider names this package implements:

```toml
providers = ["runinfra", "fireworks-ai"]
```

Maki shows them in the install approval prompt. Once you approve, the package
owns both names and receives the keys saved under them, and the generic
Models.dev client cannot take them over. Nothing goes in `providers.toml`.

An approval recorded before this list existed covers no name. Update the
package and start the interactive UI once, so Maki asks again. A headless
`maki acp` run cannot ask, so approve in the UI first.

If you upgraded from an older setup, remove any
`plugin = "maki-essential-plugins"` line from `~/.config/maki/providers.toml`.
Maki still honours it, but warns at every start until it is gone.

## RunInfra provider

RunInfra works without this package. Maki reaches it through its
[Models.dev](https://models.dev/) entry with the generic OpenAI-compatible
client, so a key saved under `runinfra` keeps working. This package implements
RunInfra, including its authenticated model discovery, once Maki grants it the
name (see [Provider names](#provider-names)).

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

The granted name identifies which installed package may receive the saved
key for `runinfra`. If the installed package fails to load, RunInfra stays
unavailable; Maki does not fall back to the generic client. A package that is
removed or disabled releases the name, and the generic client serves it again.
The network host permission and Maki's existing provider checks still apply.

## Fireworks AI provider

Maki calls Fireworks AI `fireworks-ai`, which is its Models.dev provider name.
This package can replace that catalog entry with authenticated discovery from
the official [Fireworks List Models API](https://docs.fireworks.ai/api-reference/list-models).
It takes over once Maki grants the package the name (see
[Provider names](#provider-names)).

1. Use your saved `fireworks-ai` key, set `FIREWORKS_API_KEY` before startup, or
   log in with `maki auth login fireworks-ai`. Get a key from the
   [Fireworks dashboard](https://app.fireworks.ai/settings/users/api-keys).
2. Open `/model` to refresh discovery. You can also select a model directly:

   ```sh
   maki -m fireworks-ai/accounts/fireworks/models/glm-5p3-flash
   ```

The plugin calls `/v1/accounts/fireworks/models` with the documented
[`supports_serverless=true` filter](https://docs.fireworks.ai/faq-new/models-inference/how-to-check-if-a-model-is-available-on-serverless).
It reads every page, keeps models with a chat configuration, and preserves the
full `accounts/fireworks/models/...` inference IDs. It uses positive context
limits and the published image support flag. Dedicated deployments, embeddings,
and models without chat support do not appear in discovery. You can still select
an explicit model ID, subject to Fireworks account access and Maki's model policy.

Discovery leaves output limits, reasoning capabilities, and token prices
unspecified. Maki can fill these fields from Models.dev or local model settings.
The plugin adds no default output cap for models without a published limit.

Chat requests use the OpenAI-compatible `/inference/v1/chat/completions` endpoint.
To use a gateway, set `FIREWORKS_AI_BASE_URL` to its inference base URL without a
final slash. The plugin changes the final `/inference/v1` to `/v1` for model
discovery. It keeps the same host and any path prefix, so the gateway must also
support the management API.
For example, `https://gateway.example/fireworks/inference/v1` uses
`https://gateway.example/fireworks/v1/accounts/fireworks/models` for discovery.

The granted name binds the saved key to this package. If the installed plugin
fails to load, the provider stays unavailable; Maki does not fall back to its
catalog client. A package that is removed or disabled releases the name.

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
lua tests/fireworks.lua
stylua --check lua plugin tests
```

The integration tests load this package in Maki's real Lua runtime, run shell
jobs, and exercise goal events, provider switching, and picker keys. They also
replay fixed RunInfra responses and check saved keys, stale catalog metadata,
and request effort settings. Fireworks tests check pagination, model filters,
saved and environment keys, gateway paths,
chat requests, and discovery errors. They use temporary XDG
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

CI tests Maki `v0.7.0`, the minimum supported release, and upstream `main` on
each push and pull request, and weekly to detect upstream API changes. It also
checks Lua state tests, Python lint, and formatting. Provider tests run on
both targets. Reused Maki code and fixed fixtures are documented
in [THIRD_PARTY.md](THIRD_PARTY.md).

## License

[MIT](LICENSE).
