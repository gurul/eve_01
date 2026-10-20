# eve in Rust

eve's daemon is written in Rust. `rust/` is the default daemon, and it is what
`install.sh` builds. `bridge/` holds the Python version, which works the same and
stays supported: pick it with `install.sh --python` or by installing from the venv.

The two are drop-in replacements for each other. They read and write the same files
under `~/.config/eve-bridge` and `~/.claude`, take the same environment variables,
serve the same HTTP routes and socket messages, and send the same prompts, word for
word. You can switch a Mac from one to the other and back without losing anything.

## Install

`install.sh` does all of this (README, [Get started → 1. The Mac](../README.md#1-the-mac)).
By hand, from a checkout:

```bash
brew install rust                         # or rustup; any current stable toolchain
(cd rust && cargo build --release --locked -p eve-bridge -p eve-hooks)
codesign --force --sign "Apple Development: you@example.com (TEAMID)" \
  --identifier com.github.eve-bridge rust/target/release/eve-bridge   # optional, see Permissions
rust/target/release/eve-bridge install
rust/target/release/eve-bridge install --service --serial-port '/dev/cu.usbmodem*'
```

`install` writes the Claude Code hooks as the `eve-hook-*` binaries next to
`eve-bridge`. `install --service` writes the login service to run
`rust/target/release/eve-bridge daemon`. Each install names the binary that ran it.

Build in the checkout, and keep the checkout. The binary reads some of eve's files
(the mini app's pages, the app templates, the self-context sources) from the folder it
was built in, and `eve-bridge update` rebuilds it there.

**Check:** `rust/target/release/eve-bridge status` lists the hooks and
`Service (launchd): loaded`. `tail -40 ~/Library/Logs/eve-bridge.log` shows a
`code: … @ <commit>` line and `miniapp: live at https://….trycloudflare.com`.

## What still runs on Python

The daemon is Rust, but three helpers it starts run on Python, because the
libraries they need exist only in Python:

| Helper | What it is for | Python it uses |
|---|---|---|
| mem0 | memory by meaning: extraction and the local embeddings | `EVE_MEM0_PYTHON`, else `bridge/.venv` |
| Laya | the local typed-decision model (laya-mlx) | `EVE_BROWSER_INTENT_PYTHON`, else `bridge/.venv` |
| The desktop worker | the exec host that runs PyAutoGUI for desktop tasks | `EVE_WORKER_PYTHON`, else `bridge/.venv` |

Each defaults to the venv in the checkout the binary was built from, so `install.sh`
(which builds that venv) needs no setting. A feature whose helper finds no Python
with its library turns itself off and logs it, as in the Python version.

## Permissions

macOS grants Microphone, Accessibility and Screen Recording to the program that
launchd starts: for the Rust daemon, `rust/target/release/eve-bridge`, not the venv's
Python. Add that binary in **System Settings → Privacy & Security**.

A binary signed by nobody (ad hoc) has a new identity after every build, so macOS
forgets its permissions after each update. Signed with an Apple Development
identity, it keeps them. `install.sh` and `eve-bridge update` sign with the first
Apple Development identity in your keychain when there is one; set
`EVE_CODESIGN_IDENTITY` to choose another, or to `-` to sign ad hoc.

The first time the daemon reads `~/Documents` (where a checkout often lives), macOS
asks for access to the Documents folder. Until someone answers, every file read in
that folder waits, the Python helpers included, so the daemon looks frozen with
nothing in the log. Answer the prompt and it carries on.

## Update

`eve-bridge update` pulls the checkout, rebuilds the Rust binaries in place, signs
them, refreshes the venv for the helpers and restarts the daemon. The Python version's
`eve-bridge update` is unchanged.

## Switch between Rust and Python

Each version's `install --service` points the login service at itself:

```bash
rust/target/release/eve-bridge install --service --serial-port '/dev/cu.usbmodem*'     # Rust
bridge/.venv/bin/eve-bridge install --service --serial-port '/dev/cu.usbmodem*'        # Python
```

Run `install` (hooks) from the same version too. Either version's hooks talk to
either daemon, so mixed hooks still work, but one version keeps things simple.

## How it is built

- **One crate per Python module.** `rust/plan.json` maps each of the 206 modules in
  `bridge/src/eve_bridge` to one of 158 crates under `rust/crates`. Modules that
  import each other share a crate. The largest is `eve-brain`: the chat brain,
  voice, the mini app, the app builder and onboarding.
- **The conventions** are in [rust/PORTING.md](../rust/PORTING.md): same
  environment variables, files, wire formats and prompts; no placeholders; the
  Python tests ported as the spec.
- **Every crate has a `PARITY.md`.** It has a row for each Python definition and
  where its Rust lives, the tests ported, and the places Rust differs on purpose.
  Each difference is listed with its reason.
- **The checks** are in [rust/GATES.md](../rust/GATES.md):

```bash
cd rust
python3 tools/check_plan.py                 # every Python module has one crate
python3 tools/check_port.py                 # every crate complete per PARITY.md, no placeholders
cargo build --workspace --all-targets
cargo test --workspace
python3 tools/check_cli.py                  # the Rust CLI offers every command path the Python one does
```

## Changing eve

Both versions are supported, so a change in behavior lands in both, in the same
commit: the Rust crate, the Python module, their tests, and the crate's `PARITY.md`.
`tools/check_port.py` fails when a Python definition has no row, so a new function
cannot be missed silently.

## Measured

On the owner's Mac, 2026-10-20, the same state and the same settings:

| | Python | Rust |
|---|---|---|
| Start to "apps: direct up" | about 10 s | about 1.3 s |
| Daemon memory (resident) | about 160 MB, plus mem0 loaded in process | about 120 MB, plus mem0 in its helper (about 1.2 GB, mostly the embedding model) |

Most of a request's time is the model call and the web, which neither version
changes. Rust starts and restarts faster and has no interpreter pause.
