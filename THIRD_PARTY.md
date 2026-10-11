# Maki source reused in this package

`plugin/runinfra.lua`, `tests/runinfra/replay.rs`, and the fixed fixtures in
`tests/goldens/runinfra/` adapt code from the maintained
[laudney/maki](https://github.com/laudney/maki) fork at commit
[`47c92b411279733be94061418be937cf86cb9f82`](https://github.com/laudney/maki/commit/47c92b411279733be94061418be937cf86cb9f82):

- `plugins/runinfra/init.lua`
- `maki-lua/tests/provider_replay/golden.rs`
- `maki-lua/tests/goldens/runinfra/`

The code is covered by the MIT license. The original notice is preserved in
[licenses/maki-MIT.txt](licenses/maki-MIT.txt). The RunInfra provider mapping
and fixture expectations are retained; registration and the replay harness
are adapted for this external package.
