# eve in Rust: the hooks

eve's daemon is Python (`bridge/`). Rust runs one part of eve: the Claude Code hooks.

A hook is a process Claude Code starts for every event it reports: each tool call
starts two (PreToolUse and PostToolUse), each prompt one, each session two more.
The hook reads one JSON object on stdin, sends one message to the daemon's socket
(`/tmp/eve-bridge.sock`), and prints Claude Code's answer. Most of its time is
starting up, and starting up is what Rust is fast at.

Measured on the owner's Mac (2026-10-20, 20 runs each, the same input, identical
output and exit code):

| Hook | Median start to exit |
|---|---|
| Python (`python -I -m eve_bridge.hooks.posttooluse`) | about 29 ms |
| Rust (`eve-hook-posttooluse`) | 3 to 4 ms |

That is about 50 ms saved on every tool call. The daemon itself stays Python: it is
long-running, so start-up time does not matter there, and the Python daemon is the one
that is tested live every day.

## What is in `rust/`

| Crate | What it is |
|---|---|
| `crates/eve-hooks` | the eight hook binaries, `eve-hook-<name>`: `pretooluse`, `posttooluse`, `permission-request`, `session-start`, `session-end`, `user-prompt-submit`, `stop`, `notification`. Each one is the Python hook of the same name in `bridge/src/eve_bridge/hooks/`. |
| `crates/eve-ipc` | the socket client the hooks use: the same newline-delimited JSON messages as `bridge/src/eve_bridge/ipc.py`, the same `EVE_BRIDGE_SOCK` override. |

Each crate's `PARITY.md` lists where it matches the Python code and the few
places where it cannot, with the test that covers each one.

## Install

`install.sh` builds the hooks and registers them when `cargo` is installed, and
registers the Python hooks when it is not. By hand, from a checkout:

```bash
brew install rust                                   # or rustup; any current stable toolchain
(cd rust && cargo build --release --locked -p eve-hooks)
bridge/.venv/bin/eve-bridge install --hooks-dir rust/target/release
```

`--hooks-dir` writes each hook into `~/.claude/settings.json` as the binary's path
followed by a comment naming the Python module (`'…/eve-hook-posttooluse' #
eve_bridge.hooks.posttooluse`). The comment is how `install` and `uninstall` know the
entry is eve's. Any later install replaces it: run `eve-bridge install` without
`--hooks-dir` to go back to the Python hooks. If any of the eight binaries is missing,
`install --hooks-dir` writes nothing and says how to build them.

After pulling a change to `rust/`, build again: `(cd rust && cargo build --release
-p eve-hooks)`. Claude Code starts the new binaries on the next event, so nothing
needs to restart.

## Changing eve

A change to `bridge/src/eve_bridge/hooks/` or to the request side of `ipc.py` lands
in the matching crate in the same commit (AGENTS.md rule 11). Check both:

```bash
(cd rust && cargo test --release)
(cd bridge && .venv/bin/pytest -q tests/test_installer.py)
```

`test_installer.py` runs a registered Rust hook against a test socket and checks that
the daemon side receives the event.

## The full port

On 2026-10-19 and 20 the whole daemon was ported to Rust (158 crates, one per Python
module) and ran as the owner's daemon for an afternoon. It passed its parity checks,
but live use found problems the checks did not: macOS privacy prompts that block a
launchd process without any message, a missing Bluetooth usage string that aborted
the process, and a relay that stayed silent. The Python daemon had none of these, so
it stays the daemon. The full port is kept under the git tag `rust-port-full`:

```bash
git checkout rust-port-full -- rust/      # to look at it or pick a crate from it
```
