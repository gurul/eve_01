# AGENTS.md — working on eve, and setting it up on someone's devices

This file is for an agent (Claude Code, Codex, or any other) that has been asked to
set eve up, change it, or fix it. It says what eve is, how to install it on a
Mac and an iPhone from nothing, how to check each step actually worked, and the
rules of this repository. The human-facing overview is [README.md](README.md).

eve is a personal project. No Era governance applies here: no constitution, no
directives, no `era-code`.

## What eve is, in one paragraph

A daemon on a Mac is the host. It is written twice, as drop-in replacements for
each other: in Rust (`rust/`, the default, what `install.sh` builds) and in Python
(`bridge/`, the alternative; [eve in Rust](docs/rust.md)). It It runs one chat brain for every
door: the **Eve Link** iPhone app (`ios/EveLink/`, chat, photos, voice on the
Action Button, the owner's apps (lights, music, a camera, a TV, a car) and the chief's
jobs; the apps are examples, not a fixed set, and yours can be different), Telegram (optional, muted by
default now), a hold-to-talk desk controller, and an optional robot face. Every
model call goes through one key, `OPENROUTER_API_KEY`. Claude Code sessions on the
same Mac are watched through hooks, so eve can relay their questions to the phone.

## The rules (read before changing anything)

1. **Ask for explicit permission before pushing.** Commit freely on a branch.
2. **This repository is public.** No real email addresses, ids, phone numbers,
   tokens, token-shaped strings, home paths or build output in any commit. Before
   every commit run `git diff --cached --stat | tail -1` and refuse to continue if
   the file count is unexpected; `grep` the staged diff for your own name, `@gmail`,
   `sk-`, `ghp_` and `/Users/`. Build folders (`build/`, `build-sim/`, `.lake/`) are
   ignored; keep them that way.
3. **Secrets live in `~/.config/eve-bridge/env`** (mode 600, `KEY=VALUE`, no
   inline comments). Never print, `cat`, `echo` or `source` that file. To see which
   keys exist: `grep -oE '^[A-Z_]+=' ~/.config/eve-bridge/env`. To use one key
   for one command: `env $(grep -E '^(KEY_NAME)=' ~/.config/eve-bridge/env | xargs) <command>`.
   Never use `export $(grep … | xargs)`: when the key is missing, `export` runs with
   no arguments and prints every exported variable, keys included.
4. **Keep `/docs` and `README.md` current in the same change** as the code. A
   behaviour, switch, command or API that changed without its doc is a defect.
5. **Work in a worktree, not in the main checkout.** The daemon runs from the main
   checkout (`rust/target/release/eve-bridge`, or `bridge/.venv` for the Python
   version); `cargo build --release`, `uv sync` or `pip install` there rebuilds it under
   the running daemon. Use `git worktree add -b <branch> ../eve-<name> origin/main`,
   do everything there, then merge into the live branch and restart the daemon.
6. **Anything a model reads is a prompt, and prompt changes need a blind eval.**
   Write the pass bar down *before* any result exists, score each arm once on a set
   written by someone (an agent) who did not see the change, and ship only if the
   bar holds. Spent sets are development data from then on. The harnesses are in
   `bridge/tools/` (`intent_eval.py`, `toolset_eval.py`, `route_eval.py`); the
   bars and verdicts of past rounds are in `docs/evals/` and the docs they shipped in.
7. **Never write a wrong *name* into a prompt**, not even as a "never say this"
   example: a tool name, a model id, an app name, a file. The model will emit it.
   Bad phrasings may be shown beside better ones; bad names may not.
8. **Code decides safety; a model never does.** A one-way act (sending, deleting,
   paying, pushing) runs only after the owner's typed or tapped yes, and the question
   shows the whole act when the turn has read outside text (`tool_gate.py`). Keep
   that shape when adding a tool.
9. **Lean models mirror the code.** If you change code that a model in
   `verification/Eve/*.lean` states a property of (its docstring names the Python
   functions), update the model or add a theorem, and re-run `node verification/check-all.mjs`.
10. **Logs carry counts, names and types, never what was said.** No message text,
    file contents, recipients or tokens in a log line.
11. **eve lite never changes full eve.** `EVE_PROFILE=lite` is written only by eve lite's
    installer, never on the owner's Mac, and every lite branch asks `profile.is_lite()`.
    With `EVE_PROFILE` unset, what full eve's brain is told and offered, its Composio
    verdicts, its plists and its daemon stay byte for byte as they were, and
    `bridge/tools/owner_golden.py --check` must print `GOLDEN_OK`. A new `EVE_*` switch or
    secret read by a module eve lite imports needs its reason in `profile.LITE_CLASSIFIED`
    (or a secret's shape, so `profile.apply()` removes it), or `test_profile_classified.py`
    fails. A new place that starts a developer tool (git, clang, python3: `toolchain.SHIMS`)
    goes into `test_no_devtool_spawns.py`'s known sites with why it cannot open Apple's
    install dialog in eve lite, or that test fails; a program eve lite's daemon must start
    goes into `profile.EXEC_ALLOW`, or its process firewall refuses it. eve lite's work
    leaves `daemon.py`, `miniapp.py`, `link_chat.py`, `install.sh` and `ios/EveLink/App/`
    alone ([eve lite](docs/lite.md#why-full-eve-does-not-change)).
12. **A behaviour change lands in Rust and Python together.** Both daemons are
    supported, so one commit changes the crate under `rust/crates`, the module under
    `bridge/src/eve_bridge`, both tests and the crate's `PARITY.md`. `cd rust &&
    python3 tools/check_port.py && cargo test -p eve-<crate>` must pass alongside the
    Python suite ([changing eve](docs/rust.md#changing-eve)).

## Set eve up on a Mac and an iPhone

Every step ends with a check. Do not move on while a check fails; the failures in
this project are silent (the daemon runs and nothing happens), so the checks are the
only thing that tells you where you are. The whole path takes about an hour.

### 0. What you need

For someone without Xcode or Homebrew, or who wants no iPhone app, set up **eve lite**
instead: one install line on an Apple-silicon Mac, two sign-ins in the browser, and eve as
a web page on the Mac and on phones on the same Wi-Fi ([eve lite](docs/lite.md); the
latest release, lite-v0.1.3, is published). Everything below is full eve.

- A Mac on macOS 14 or later, with Python 3.12 (`python3.12 --version`), Homebrew,
  Node 22.12 or later with pnpm (the phone app's chat page, eve lite's web page and the
  Lean check), and Xcode 26 with `xcodegen` (`brew install xcodegen`) for the phone app.
- An OpenRouter key (`OPENROUTER_API_KEY`).
- A Telegram bot token and your numeric Telegram user id. eve's brain is wired
  through the Telegram door even when Telegram is muted (`EVE_TELEGRAM=app`):
  talk to **@BotFather** (`/newbot`) for the token, and **@userinfobot** for the id.
  No message ever goes to Telegram in `app` mode.
- `cloudflared` (`brew install cloudflared`) for the phone's tunnel, and `qrencode`
  (`brew install qrencode`) to pair by QR code.
- An iPhone on iOS 18+ with an Action Button, unlocked and on the same Wi-Fi (or a
  cable) when installing.

### 1. The daemon

`install.sh` at the repo root does this section up to the permissions, without
prompts when given `--yes`:
`curl -fsSL https://raw.githubusercontent.com/gurul/eve_01/main/install.sh | bash -s -- --yes`.
It leaves empty `KEY=` lines for the keys it did not get. It builds the Rust daemon
(`--python` for the Python one), and every later `bridge/.venv/bin/eve-bridge` in this
file is then `rust/target/release/eve-bridge`: the commands are the same. The manual
steps below are the Python version's; the Rust build is in [eve in Rust](docs/rust.md#install):

```bash
git clone https://github.com/gurul/eve_01 eve && cd eve
python3.12 -m venv bridge/.venv
bridge/.venv/bin/python -m pip install -e './bridge[dev,miniapp]'
mkdir -p ~/.config/eve-bridge && touch ~/.config/eve-bridge/env
chmod 600 ~/.config/eve-bridge/env
```

Put these in `~/.config/eve-bridge/env` (replace the values; keep the file
0600):

```dotenv
OPENROUTER_API_KEY=...
EVE_TELEGRAM=app
EVE_TELEGRAM_TOKEN=...
EVE_TELEGRAM_OWNER=123456789
EVE_MINIAPP=1
EVE_MEMORY=1
EVE_SHARE=1
```

- `EVE_TELEGRAM=1` instead of `app` makes eve also answer on Telegram.
- `EVE_MEMORY=1` keeps transcripts and the nightly dream
  ([memory](docs/stackchan/memory.md)); it is off in code and on for the owner.
- Notifications to the phone are on by default (`EVE_NOTIFY=0` turns them off).
- Leave `OPENAI_API_KEY` out unless the desk voice (wake word at the Mac) is wanted;
  that is the one feature still on it ([OpenRouter](docs/openrouter.md)).

Install the Claude Code hooks and the login service, then grant the daemon's
Python its permissions:

```bash
bridge/.venv/bin/eve-bridge install                 # hooks into Claude Code's settings.json
bridge/.venv/bin/eve-bridge install --service --serial-port '/dev/cu.usbmodem*'
```

Both commands are needed: `--service` installs the launchd agent and nothing else.
The serial glob is right whether or not a board is attached; without one the daemon
idles on it. The hooks go into `$CLAUDE_CONFIG_DIR` when that is set and `~/.claude`
otherwise; a wrapper that runs Claude Code against a private config home needs
`--config-dir`.

In **System Settings → Privacy & Security**, add the venv's Python
(`bridge/.venv/bin/python3.12`, resolved; `eve-bridge ears-check` prints the
exact binary) to **Microphone**, **Accessibility** and **Screen Recording**. macOS
grants these to the interpreter binary, not to a terminal.

**Check.**

```bash
bridge/.venv/bin/eve-bridge status           # every hook listed; "Service (launchd): loaded"
tail -40 ~/Library/Logs/eve-bridge.log       # a "code: <path> @ <commit>" line, then
                                                  # "miniapp: live at https://….trycloudflare.com"
```

If the log says `telegram: asked for … but off: … not set`, the env file is
missing that variable. If there is no `miniapp: live` line, `cloudflared` is not on
the service's PATH (`/opt/homebrew/bin` is) or `EVE_MINIAPP` is unset. After any
change to the env file:

```bash
launchctl kickstart -k gui/$(id -u)/com.github.eve-bridge.daemon
```

### 2. The phone app

Eve Link is signed with the owner's team in `ios/EveLink/project.yml`
(`DEVELOPMENT_TEAM`). For another person, set that value, in both the app and the
`EveLinkControls` target, to their own team id (a free Apple ID works; its
builds expire after 7 days and need `build.sh --install` again).

```bash
sh ios/EveLink/build.sh --install      # phone unlocked, same network or a cable
```

Then pair it. On the Mac:

```bash
bridge/.venv/bin/eve-bridge link pair      # prints the link and a QR code
```

Scan the code with the iPhone's camera (or AirDrop the link) and tap **Open in Eve
Link**. The link holds the pairing token: send it to no one else. Open the app, go to
voice mode (the button at the top right), and allow the microphone when asked.

Assign the Action Button: **Settings → Action Button → Controls → Eve Link →
Talk to eve**, and keep **Live Activities** on for Eve Link. In voice mode the
Action Button talks; a tap on the face stops eve
([voice mode](docs/voice-mode.md), [the Action Button](docs/action-button.md)).

**Check.** Type "hi" in the app's chat and get an answer. Press the Action Button
in voice mode and say "what time is it": the face looks at you, your words appear
under it, eve answers out loud with its words captioned. On the Mac,
`grep "call:" ~/Library/Logs/eve-bridge.log | tail -3` shows the press and
the reply timing.

### 3. Notifications (one minute)

Install **ntfy** from the App Store. Then:

```bash
bridge/.venv/bin/eve-bridge notify topic   # prints the topic to subscribe to in ntfy
bridge/.venv/bin/eve-bridge notify test    # the phone shows "Notifications from eve work."
```

Details: [notifications](docs/notifications.md).

### 4. Connected accounts (optional)

These are accounts eve signs in to (Google, GitHub and Maps, and Composio for the
rest), documented in [connected accounts](docs/apps.md). They are a different thing
from eve's apps (section 5): an app is a module in this repository that drives a
device or a service; a connected account is someone else's service eve calls on your
behalf.

**Google (Gmail, Calendar, Drive, Contacts).** In a Google Cloud project of your
own: enable the **Gmail**, **Google Calendar**, **Google Drive** and **People**
APIs; under Google Auth Platform set the audience to External, add yourself as a
test user and publish; create an **OAuth client of type Desktop app** and save its
JSON as `~/.config/eve-bridge/google-oauth-client.json`. Then:

```bash
echo 'EVE_APPS=1' >> ~/.config/eve-bridge/env
bridge/.venv/bin/eve-bridge apps login google     # a browser sign-in; allow everything
bridge/.venv/bin/eve-bridge apps status           # each account: up via rest, its tools, its policy
```

Gmail reads, and sends only after the owner's yes on the whole email; the calendar
may change; Drive asks; Contacts only looks ([connected accounts](docs/apps.md)).

**GitHub and Maps.** `EVE_GITHUB_MCP_TOKEN=<fine-grained token>` and
`EVE_GOOGLE_MAPS_KEY=<Maps Grounding Lite key>` in the env file, then
restart. eve lists the owner's repositories once a day so "my car repo" is
found ([connected accounts](docs/apps.md#your-github-repositories-by-name)).

**Slack, Notion, Linear** go through Composio: `EVE_COMPOSIO=1` and
`COMPOSIO_API_KEY` ([Telegram doc, the apps](docs/stackchan/telegram.md#the-apps-composio)).

### 5. Apps (optional): the owner's picks

eve's apps are variable: these are just the ones its owner wants. Each is one module
and one line in `bridge/src/eve_bridge/app_registry.py`; turn any off with its switch.
Every place an app reaches (the chat's and the voice's tools, the code path that
answers "lights off" with no model turn, the tool gate, the phone's Apps list, the
CLI, the intent eval) reads that registry, so an app switched off is gone everywhere.

| App | Module | Switch | Default | Doc |
|---|---|---|---|---|
| Lights (Govee, WiZ, HappyLighting, Tuya) | `bridge/src/eve_bridge/lights.py` | `EVE_LIGHTS` | on once `~/.config/eve-bridge/lights.json` lists lights | [lights](docs/lights.md) |
| Music (Spotify) | `bridge/src/eve_bridge/spotify.py` | `EVE_SPOTIFY` | on once `eve-bridge spotify login` has run | [Spotify](docs/spotify.md) |
| TV remote (Fire TV) | `bridge/src/eve_bridge/firetv.py` | `EVE_FIRETV` | on once `eve-bridge firetv pair` has run | [Fire TV](docs/firetv.md) |
| Camera (OBSBOT webcam) | `bridge/src/eve_bridge/obsbot_panel.py` | `EVE_OBSBOT` | on while an OBSBOT camera is plugged in | [OBSBOT](docs/obsbot.md) |
| Pika (a robot car) | `bridge/src/eve_bridge/pika.py` | `EVE_PIKA` | off; `EVE_PIKA=1` and `~/.config/eve-bridge/pika.json` | [Pika](docs/pika.md) |

A switch set to `0` turns an app off (`EVE_PIKA` is off unless it is `1`).
`bridge/.venv/bin/eve-bridge app list` shows each app, its switch and whether it is on.

Devices on the desk that are not apps (they are eve's own body and doors):

| Device | Doc | Env |
|---|---|---|
| M5StackChan robot (face, head, camera) | [build](docs/stackchan/build.md), [voice](docs/stackchan/voice.md) | the serial glob above; `OPENAI_API_KEY` for the wake-word voice |
| Home Assistant Voice PE (button, dial, speaker) | [Voice PE](docs/voice-pe.md) | `EVE_CONTROLLER_SERIAL=<its USB serial>` |
| Mac menu-bar app and widget | [widget](docs/stackchan/widget.md) | `eve-bridge install --notes-widget` |

### 5a. Build your own app

In a worktree (rule 5), from its root:

```bash
uv run --project bridge eve-bridge app new fan --dry-run   # what it would write
uv run --project bridge eve-bridge app new fan
```

`uv run --project bridge` makes the worktree's own venv on first use; a fresh worktree
has no `bridge/.venv`, so `bridge/.venv/bin/eve-bridge` is only there in the main
checkout.

It writes four things: the module (`bridge/src/eve_bridge/<name>.py`: an on/off switch
kept in memory, a read tool, an acting tool, and a phone sheet), its test
(`bridge/tests/test_<name>.py`), its doc (`docs/<name>.md`) and one line at the end of
the registry. It passes its own tests and the whole suite as written (no other test or
doc needs an edit), and it is off by default, so nothing the chat and voice models read
changes until its switch is on. To turn it on, merge it into the branch the main
checkout runs, then set `EVE_<NAME>=1` in the env file and restart the daemon. The
phone shows it under Apps with eve's generic sheet; no Swift is needed. The
walk-through, from changing its tools to the blind eval before it is on by default, is
[adding an app](docs/adding-an-app.md).

### Resources for building apps

The reverse-engineering tools below are for devices, apps and games you own and may
inspect, nothing else.

- **REA** ([github.com/morluto/rea](https://github.com/morluto/rea)), "Reverse
  Engineer Anything": an MCP for agents that inspects app binaries, Electron/JS apps
  and websites without source, explains how a feature works and helps rebuild it.
  Setup: `npx rea-agents setup`. Use it to learn how a device's own app talks to the
  device when there is no public API, and only on devices and software you own and may
  inspect.
- **Ghidra**
  ([github.com/NationalSecurityAgency/ghidra](https://github.com/NationalSecurityAgency/ghidra)):
  the NSA's free decompiler and disassembler for native binaries; most agent RE setups
  sit on it.
- **GhidraMCP**
  ([github.com/LaurieWired/GhidraMCP](https://github.com/LaurieWired/GhidraMCP)): an
  MCP server that lets an agent drive Ghidra, headless or in the GUI.
- **ImHex** ([github.com/WerWolv/ImHex](https://github.com/WerWolv/ImHex)): a hex
  editor with pattern language and an MCP server, for reading file formats and
  firmware images.
- **mitmproxy**
  ([github.com/mitmproxy/mitmproxy](https://github.com/mitmproxy/mitmproxy)): an
  intercepting HTTP/TLS proxy: watch what a phone or desktop app sends to map an API
  it never documented.
- **Frida** ([github.com/frida/frida](https://github.com/frida/frida)): runtime
  hooking for desktop and mobile apps, scriptable from the CLI: trace the calls an app
  makes while it runs.
- **ILSpy** ([github.com/icsharpcode/ILSpy](https://github.com/icsharpcode/ILSpy)): a
  .NET decompiler to C#, for Windows companion apps.
- **Android RE skill**
  ([github.com/SimoneAvogadro/android-reverse-engineering-skill](https://github.com/SimoneAvogadro/android-reverse-engineering-skill)):
  a Claude Code skill that turns an APK into decompiled code and the API endpoints it
  calls.
- **Cpp2IL**
  ([github.com/SamboyCoding/Cpp2IL](https://github.com/SamboyCoding/Cpp2IL)): turns a
  Unity IL2CPP build back into .NET assemblies that ILSpy can read; work in progress.
- **Universal Modder**
  ([github.com/rehan-remade/universal-modder](https://github.com/rehan-remade/universal-modder)):
  agent skills for recon, reverse engineering and modding PC games you own.
- **hwlog** ([github.com/gurul/hardware-logging](https://github.com/gurul/hardware-logging)):
  the owner's crash-aware serial logging for embedded boards, built for coding agents:
  persistent sessions, decoded backtraces, flash-safe port arbitration and an MCP
  server. Use it when an app has firmware on a board.

### 6. The one check that covers everything

```bash
bridge/.venv/bin/eve-bridge status && bridge/.venv/bin/eve-bridge apps status
tail -5 ~/Library/Logs/eve-bridge.log
```

A healthy daemon logs its commit, `miniapp: live`, `apps: direct up: …` (when apps
are on) and `share: on`. Then send one message from the phone.

## Working on the code

- **Run the tests in a worktree**, never in the main checkout (rule 5), from the
  worktree's root (some tests read `docs/` relative to it; from `bridge/` about 17
  fail on paths):
  `uv run --project bridge --extra dev --extra miniapp --extra tls --extra composio --with matplotlib pytest -q -p no:cacheprovider -n 8 --dist loadgroup -c bridge/pyproject.toml --rootdir bridge bridge/tests`.
  About 6,100 tests in 2.5 minutes on 8 workers (13 minutes serial; `-n` is not
  in the config on purpose, so single-file runs and the docs' gate commands stay
  serial). `cd bridge && uv run --extra dev ruff check src tests tools` for lint.
- **The phone app's logic on the Mac:** `sh ios/EveLink/Tests/run.sh` (PCM,
  turns, pairing, voice-mode captions). Building the app: `sh ios/EveLink/build.sh`.
- **The Lean models:** `PATH=$HOME/.elan/bin:$PATH node verification/check-all.mjs`
  (539 theorems, no `sorry`, no `native_decide`; [verification](docs/verification.md)).
- **eve lite** (rule 11; [eve lite](docs/lite.md)): its gates, each with the command that
  decides it, are in `docs/lite-gates.md`; gate L1, `tools/owner_golden.py --check` run
  from `bridge/`, proves full eve unchanged. Its web page builds with
  `cd ios/EveLink/Web && pnpm build:web`, into `bridge/src/eve_bridge/web_lite/`
  (git-ignored).
- **Restart the daemon after merging:** `launchctl kickstart -k gui/$(id -u)/com.github.eve-bridge.daemon`,
  then confirm the `code: … @ <commit>` line in the log is the commit you merged.
- **Where things are.** The daemon: `bridge/src/eve_bridge/daemon.py`. The
  chat brain and every tool it offers: `telegram.py` (large; grep for `_tool_`).
  Tool safety: `tool_gate.py`, `consent.py`. eve's apps: `app_registry.py` (one line
  each; every wiring site reads it), `app_template/` (`eve-bridge app new`), and the
  phone's generic sheet `ios/EveLink/App/GenericAppSheet.swift`. Routing: `toolset_router.py`,
  `grade_router.py`, `search_router.py`. Connected accounts: `apps_mcp.py`,
  `google_rest.py`, `github_repos.py`, `composio_tools.py`. Sharing files:
  `share.py`. The phone: `link_chat.py`, `phone_call.py`, `stick_link.py`,
  `link_uploads.py`, `link_reactions.py`, `notify.py`. Memory: `memory.py`,
  `transcripts.py`, `chat_log.py`, `mem0_memory.py`, `dream.py`. The iPhone app:
  `ios/EveLink/App` (SwiftUI), `Shared` (logic tested on the Mac), `Web`
  (the chat page, OpenUI). Models of the state machines: `verification/Eve`. eve lite:
  `profile.py` (the switch, what it turns off, the process firewall), `lite.py` (its
  daemon), `lite_web.py` (its two web doors), `lite_rtc.py` (WebRTC), `lite_tls.py`,
  `lan_net.py`, `web_auth.py`, `onboarding.py` (the two sign-ins), `composio_connect.py`;
  its web app is `ios/EveLink/Web/src/web` and its installer `install-lite.sh`.
- **Evals and bars.** `bridge/tools/intent_eval.py CASES.json` scores which tool
  the brain picks (production routing, nothing executes). Pre-register the bar in a
  `BAR.md` beside the case file before running; keep the verdict beside it.
- **Memory for the next agent.** Session notes live outside the repository
  (`~/.claude/projects/.../memory/`); the repository's own record is `docs/` and the
  commit messages. Write the why into both.

## When something is wrong

| Symptom | Look at |
|---|---|
| The phone says "Pair Eve Link first" | `eve-bridge link pair` again; the token file is `~/.config/eve-bridge/stick-link.json` |
| Calls connect but eve never answers | `grep -E "but off|OPENROUTER" ~/Library/Logs/eve-bridge.log`; the brain needs the token, the owner id and the OpenRouter key |
| "No Claude connected" on a widget or board | the launchd agent is disabled: `launchctl print-disabled gui/$(id -u)`, then `launchctl enable` and `bootstrap` |
| A Google app says HTTP 403 "has not been used in project" | enable that API in the Cloud project, wait a minute |
| A phone install fails with CoreDeviceError 4016 | the phone is locked; unlock it and run `build.sh --install` again |
| The daemon runs an old commit | it runs the main checkout's branch; merge there and `kickstart` |
| Tests fail on `docs/...` paths | run pytest from the repository root, not from `bridge/` |
| 10–20 tests fail on missing modules | add `--extra miniapp --extra tls --extra composio --with matplotlib` |
| eve lite misbehaves on someone's Mac | `~/.eve-lite/venv/bin/eve-bridge doctor` marks what to fix; its log is `~/Library/Logs/eve-lite.log` ([troubleshooting](docs/lite.md#troubleshooting)) |

Setup failures seen on a clean machine, with their evidence, are recorded in
[docs/SETUP-ISSUES.md](docs/SETUP-ISSUES.md).
