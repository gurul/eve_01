# eve-bridge

**English** | [简体中文](README.zh-CN.md) | [日本語](README.ja.md)

[![test](https://github.com/SnowWarri0r/eve-bridge/actions/workflows/test.yml/badge.svg)](https://github.com/SnowWarri0r/eve-bridge/actions/workflows/test.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-yellow.svg)](LICENSE)
[![Python: 3.11+](https://img.shields.io/badge/python-3.11%2B-blue.svg)](https://www.python.org/)
[![Platforms](https://img.shields.io/badge/platforms-macOS%20%7C%20Linux%20%7C%20Windows-lightgrey.svg)](#requirements)
[![Status: daily-driven](https://img.shields.io/badge/status-daily--driven-brightgreen.svg)](#status)
[![PRs: Welcome](https://img.shields.io/badge/PRs-welcome-brightgreen.svg)](https://github.com/SnowWarri0r/eve-bridge/issues)

Bridge [Claude Code](https://claude.com/claude-code) CLI sessions to the
[claude-desktop-buddy](https://github.com/anthropics/claude-desktop-buddy) BLE
hardware — without going through the Claude desktop app.

The eve firmware officially pairs with Claude for macOS/Windows. This project
lets you drive the same hardware from a plain terminal running the `claude` CLI,
so your desk pet reacts to CLI sessions: sleeps when idle, gets busy when a
tool call runs, and blinks when a session needs your attention. Permission
decisions happen in Claude Code's own dialog, or on your phone through the
Telegram relay; the robot itself no longer approves or denies.

## What you get

- **Smart matcher** — auto-allow trivial Bash (`ls`/`cat`/`grep`/...) with no prompt at all, mark risky commands (`rm`/`curl`/`git push`/...) always-ask, and leave the rest to Claude Code's own permission flow. TOML-overridable.
- **Permissions from your phone** — with the Telegram relay on (`claude on`), an always-ask command or a Claude Code permission dialog is asked in the chat as a yes/no; silence falls back to the dialog on the Mac, never a deny. Jev can also flag a risky Bash command there (`EVE_COMMAND_RISK`).
- **Live stick HUD** — assistant replies mirror to the stick within ~500 ms via a JSONL tailer (no Stop-hook flush race).
- **Statusline** — `eve-bridge hud` renders battery / encryption / **tokens today** / **estimated USD spend today** in your prompt bar; composes with [claude-hud](https://github.com/jarrodwatts/claude-hud).
- **One-command install + autostart** — `eve-bridge install --service` picks the right backend per OS: launchd (macOS), systemd user unit (Linux), Task Scheduler (Windows).
- **Custom GIF characters** — `eve-bridge push-character ./pack/` uploads a folder of frames over BLE with chunked flow control.
- **Release notifications + self-update** — daemon pings GitHub releases once a day; hud renders `↑ vX.Y.Z` when a newer tag exists. `eve-bridge check-update` for a one-off check, `eve-bridge update` to actually pull + reinstall + restart the daemon. Opt out of polling with `EVE_BRIDGE_NO_UPDATE_CHECK=1`.
- **Optional CJK display on the stick** — fork-only firmware variants render Simplified Chinese and Japanese (Traditional Chinese still planned) at 12×12 px using [Fusion Pixel Font](https://github.com/TakWolf/fusion-pixel-font) (OFL). Bridge auto-switches its wire codec when you set `EVE_CJK_TARGET=zh-CN` (or `ja`). See [CJK display on the stick](#cjk-display-on-the-stick-optional).

## How it works

```
claude CLI ──PreToolUse/Stop/etc hooks──▶ Unix socket ──▶ daemon ──BLE NUS──▶ stick
                                                           ▲
                                                           └── tails ~/.claude/projects/*.jsonl
                                                               for tokens & recent messages
```

* **Hooks** (configured in `~/.claude/settings.json`) fire on session lifecycle
  events, tool calls, permission requests, and turn boundaries.
* Each hook is a small Python script that posts the event payload to a local
  **daemon** over a Unix socket.
* The daemon aggregates per-session state (`total` / `running` / `waiting` /
  `tokens` / `entries`) and pushes heartbeat snapshots to the stick over BLE
  Nordic UART Service, speaking the same JSON wire format as the desktop app.
* For permission prompts, the daemon allows what the matcher auto-allows,
  asks your phone when the Telegram relay is on, and otherwise returns no
  decision so Claude Code's own dialog runs. The robot has no approve/deny
  surface any more.

See [REFERENCE.md in the eve firmware repo](https://github.com/anthropics/claude-desktop-buddy/blob/main/REFERENCE.md)
for the full wire protocol.

## Install

```bash
git clone https://github.com/SnowWarri0r/eve-bridge
cd eve-bridge
python3.12 -m venv .venv
.venv/bin/pip install -e .

# Register hooks into ~/.claude/settings.json (makes a .backup copy first):
.venv/bin/eve-bridge install

# In another terminal, start the daemon:
.venv/bin/eve-bridge daemon
```

**Windows users:** Replace `.venv/bin/` with `.venv\Scripts\` in the commands above.

**Optional — the computer-use fast lane.** Start the daemon with `EVE_FAST_LANE=1` (it ships off)
and the planner gets `delegate`, keyword-decided clicks on labelled controls; nothing to install. Its
`model` mode asks hosted Jev (`EVE_DECIDER=jev`); the local Laya decider it once used was removed.
The lane's **router** ships on (`EVE_LANE_FIRST`, `lane_router.py`): a request
one labelled control fully accounts for ("switch to week view") is clicked before the planner's first turn,
in about a second instead of thirteen. Details and the
measured numbers: [docs/stackchan/voice.md](../docs/stackchan/voice.md#the-fast-lane-clicks-under-the-planner).

**Optional, Apple silicon only — the robot's live eye expressions.** `.venv/bin/pip install -e ".[laya]"`
adds `laya-mlx` for the local Laya checkpoint at `~/.config/eve-bridge/models/laya-multilingual-mlx`
([laya-expressions](../docs/stackchan/laya-expressions/README.md)). `eve-bridge update` installs the
`[laya]` extra automatically on Apple silicon.

Then start any `claude` session. The daemon scans for a BLE device advertising
a name starting with `Claude`, connects, and begins pushing state.

To remove the hooks:

```bash
.venv/bin/eve-bridge uninstall
```

### Auto-start on login

Instead of running `eve-bridge daemon` manually, install it as a
system service so it starts at login and restarts on crashes.

#### macOS (launchd)

Install as a user-level launchd agent:

```bash
.venv/bin/eve-bridge install --service
```

This writes `~/Library/LaunchAgents/com.github.eve-bridge.daemon.plist`
pointed at the venv Python you just installed from, runs it immediately via
`launchctl load`, and redirects stdout/stderr to
`~/Library/Logs/eve-bridge.log`.

To remove it:

```bash
.venv/bin/eve-bridge uninstall --service
```

#### Windows (Task Scheduler)

Install as a Task Scheduler task:

```bash
.venv/Scripts/eve-bridge install --service
```

This creates a task named `eve-bridge-daemon` that runs at logon.
Logs are written to `%LOCALAPPDATA%\eve-bridge\daemon.log`.

To remove it:

```bash
.venv/Scripts/eve-bridge uninstall --service
```

#### Linux (systemd)

The same `--service` flag installs a user-level systemd unit on Linux:

```bash
.venv/bin/eve-bridge install --service
```

This writes `~/.config/systemd/user/eve-bridge.service` pointed at the
venv Python you just installed from, then runs `systemctl --user
daemon-reload` and `systemctl --user enable --now eve-bridge.service`
so the daemon starts immediately and on every login. View logs with:

```bash
journalctl --user -u eve-bridge.service -f
```

To remove it:

```bash
.venv/bin/eve-bridge uninstall --service
```

A few Linux-specific gotchas:

* **BLE needs BlueZ.** Make sure the `bluetooth` service is running
  (`systemctl status bluetooth`) and your user is in the `bluetooth`
  group (`sudo usermod -aG bluetooth $USER`, then log out and back in).
  Without that, you'll see
  `org.freedesktop.DBus.Error.ServiceUnknown ... org.bluez` in the
  journal.
* **Survive logout / start at boot.** The user manager exits with your
  last session by default, which stops the daemon. Run
  `loginctl enable-linger $USER` once if you want the unit to start at
  boot and persist after logout.

Tested on Ubuntu 22.04 LTS. Should work on any distro with a systemd user
manager (Fedora 39+, Debian 12+, Arch, etc.) — please open an issue if
your distro needs a tweak.

---

`eve-bridge status` reports both hook and service status.

### Customizing the IPC transport

The daemon and hook scripts talk over a local IPC channel. The default
fits 99% of setups, but two knobs are exposed for the rest:

| OS      | Default transport                  | Override via `--socket` or `EVE_BRIDGE_SOCK` |
| ------- | ---------------------------------- | ------------------------------------------------- |
| macOS / Linux | Unix socket `/tmp/eve-bridge.sock` | another path, e.g. `~/eve.sock`        |
| Windows | TCP loopback `127.0.0.1:48765`     | another port, e.g. `:49000` or `127.0.0.1:49000`  |

If port 48765 is already in use on Windows, run
`eve-bridge daemon --socket :49000` and pass the same `--socket` to
`hud` invocations (or `export EVE_BRIDGE_SOCK=:49000` to set it once
for all hook scripts).

### Show the stick's state in Claude Code's status line

`eve-bridge hud` prints a compact one-line summary (battery,
encryption, tokens and cost today). Plug it into your `~/.claude/settings.json`:

```json
{
  "statusLine": {
    "type": "command",
    "command": "/path/to/.venv/bin/eve-bridge hud"
  }
}
```

For an ASCII-only terminal: `eve-bridge hud --ascii`.

Already using [claude-hud](https://github.com/jarrodwatts/claude-hud) or
another statusline plugin? You can compose both — wrap them in a small
shell script and concatenate outputs; statusLine accepts multi-line
responses.

Live in iTerm2 — paw, battery bar, encryption lock, today's tokens, today's cost, running session count:

<p align="center"><img src="docs/img/statusline.png" alt="eve-bridge hud — paw, full green battery bar at 98%, lock, 101K tokens, $106.02 cost, 1 running session" width="580"></p>

Other states the same line goes through:

```
🐾 🔋 96% 🔒                          # healthy, encrypted link
🐾 🔋 96% 🔒 12.3K $0.42              # tokens (≥ 1K) and cost (≥ $0.01) today
🐾 🔋 12% 🔒 1.2M $8.50 2run          # low battery, busy day, sessions running
🐾 ∅                                  # stick disconnected (but daemon is alive)
🐾 off                                # daemon not running
```

The tokens segment sums today's `usage.output_tokens` across
`~/.claude/projects/*.jsonl`. The cost segment estimates USD from the
same records using `input + output + cache writes/reads × per-model
rates`. Rates table lives in [`pricing.py`](src/eve_bridge/pricing.py) —
edit to override or add models. Not a billing source of truth; treat
as a heads-up.

### Notes widget (macOS)

When Claude has been idle for 10 minutes the robot explores the room and
writes one-line observations to `~/.config/eve-bridge/notes/YYYY-MM-DD.md`
(`- HH:MM yaw=+20 pitch=40 — <sentence>`). `eve-bridge notes-widget`
shows the newest 40 of those lines — today's and yesterday's, newest first,
one header per day — in a borderless dark panel that sits just above the
desktop icons on every Space. It has no Dock icon and no menu-bar entry.
Drag it by its background to move it (the position is saved in
`~/.config/eve-bridge/widget.json`); right-click for **Open notes
folder** and **Quit**. It re-renders when a note file changes (via
`watchfiles`) and polls every 30 s as a backstop.

```bash
.venv/bin/eve-bridge notes-widget            # run in the foreground
.venv/bin/eve-bridge notes-widget --once     # build, print, exit (smoke test)
.venv/bin/eve-bridge install --notes-widget  # start it at login (second launchd agent)
.venv/bin/eve-bridge uninstall --notes-widget
```

`install --notes-widget` writes
`~/Library/LaunchAgents/com.github.eve-bridge.notes-widget.plist`
pointed at the same Python as the daemon agent (`RunAtLoad`, no
`KeepAlive` — a widget you quit stays quit until next login), logging to
`~/Library/Logs/eve-bridge-notes-widget.log`. It is independent of
`--service`; pass both to install daemon and widget in one go. Set
`EVE_NOTES_DIR` to point the widget at another notes folder.

## Working with Claude Code's `permissions` config

Claude Code's own `~/.claude/settings.json` `permissions` block (`allow` /
`ask` / `deny` lists, plus `defaultMode`) and this bridge's smart matcher
*both* decide what happens on a tool call. The interaction is well-defined,
but worth spelling out so you can pick the right combo.

For every `PreToolUse` event:

```
matcher classify_command(hint)
 ├─ "allow"  → bridge returns permissionDecision=allow  (short-circuit)
 ├─ "ask"    → relay on: asked on your phone as a yes/no; no answer or relay off → Claude Code's own flow
 └─ "default"→ relay on: allowed (the relay is bypass); relay off → Claude Code's settings.json + defaultMode run
```

The robot has no approve/deny surface; it never decides a prompt. "Relay on"
means the Telegram door's `claude on`. With it on, Jev can also judge a Bash
command the matcher let through (`EVE_COMMAND_RISK`: `shadow` only logs,
`ask` sends a risky one to your phone).

**Recommended pairings**

| Claude Code `defaultMode` | Matcher `strict` | Behaviour                                                                                                              |
| ------------------------- | ---------------- | ---------------------------------------------------------------------------------------------------------------------- |
| `ask` (the default)       | `false`          | Trivial bash auto-approved by the matcher; everything else gets Claude Code's prompt (or your phone, with the relay on, for `always_ask` patterns). |
| `bypassPermissions`       | **`true`**       | Trivial bash auto-approved; every other command is an "ask": your phone decides with the relay on. With the relay off nothing asks, so this is the pairing to use only with the relay. |
| `bypassPermissions`       | `false`          | ⚠ Only the matcher's `always_ask` patterns are asked (on your phone, relay on); everything else runs unasked. The daemon logs a warning at startup if it detects this combo. |
| `auto`                    | `false`          | Same as `ask` for our purposes — unmatched commands fall through to Claude Code's flow.                                |

`strict` lives in `~/.config/eve-bridge/matchers.toml`:

```toml
strict = true
```

The daemon logs a one-line summary of both configs at startup so you can
spot misalignments quickly:

```
INFO eve_bridge.daemon: matcher: strict=False auto_allow=46 always_ask=53
INFO eve_bridge.daemon: settings.json: permissions.defaultMode='auto' ask=0
```

## Audit log

Every `PreToolUse` decision is appended to a JSONL file so you can review
later what was let through, denied, or deferred — especially valuable
under `bypassPermissions` where most decisions never reach your eyes.

Default location:

| OS      | Path                                                      |
| ------- | --------------------------------------------------------- |
| macOS   | `~/Library/Logs/eve-bridge-audit.jsonl`              |
| Linux   | `$XDG_DATA_HOME/eve-bridge/audit.jsonl` (or `~/.local/share/...`) |
| Windows | `%LOCALAPPDATA%\eve-bridge\audit.jsonl`              |

Override with the `EVE_BRIDGE_AUDIT` env var.

One line per decision; fields:

```json
{"ts":"2026-05-16T00:15:12.690+08:00","session":"c461b71c","tool":"Bash",
 "hint":"git status -s","matcher":"allow","decision":"allow","source":"auto_allow"}
```

| Field      | Meaning                                                          |
| ---------- | ---------------------------------------------------------------- |
| `ts`       | ISO-8601 local timestamp with offset                             |
| `session`  | First 8 chars of the Claude Code session id                      |
| `tool`     | Tool name (`Bash`, `Edit`, ...)                                  |
| `hint`     | Short summary of what's being run (truncated to 200 chars)       |
| `matcher`  | Matcher classification: `allow` / `ask` / `default`              |
| `decision` | What the bridge returned: `allow` / `deny` / `null` (deferred)   |
| `source`   | `auto_allow` / `telegram` (you answered on the phone) / `telegram_relay` (allowed by the relay) / `jev_safe` / `jev_error` / `jev_risky_deferred` / `jev_shadow` / `defer` / `ble_disconnected` |

### Viewing

`eve-bridge audit` is the friendly viewer — coloured, aligned, with
tail / filter / follow:

```bash
eve-bridge audit                       # last 20 entries
eve-bridge audit -n 100                # last 100
eve-bridge audit -f                    # follow new entries (Ctrl+C to stop)
eve-bridge audit --decision deny       # only the things you blocked
eve-bridge audit --source telegram     # only rounds you answered on the phone
eve-bridge audit --tool Edit -n 50     # last 50 Edit calls
eve-bridge audit --path                # print the file path and exit
eve-bridge audit --ascii               # no colour (pipes / dumb terminals)
```

Sample output:

```
# audit log: /Users/snow/Library/Logs/eve-bridge-audit.jsonl
00:21:09.029 Bash     —     defer       sleep 8 && gh run list --repo ...
00:30:10.212 Bash     allow auto_allow  cat >> tests/test_audit.py <<'EOF' ...
00:34:55.871 Bash     deny  telegram    git push origin main --force
```

Colours: green for `allow`, red for `deny`, dim for `—` (no decision /
deferred). Source column is yellow for `telegram` (you answered on the phone), dim otherwise.

### Raw jq recipes

If you prefer jq:

```bash
# "What did I deny from the phone today?"
jq 'select(.decision=="deny")' ~/Library/Logs/eve-bridge-audit.jsonl

# "Top auto-allowed commands this week"
jq -r 'select(.source=="auto_allow") | .hint' ~/Library/Logs/eve-bridge-audit.jsonl \
  | awk '{print $1}' | sort | uniq -c | sort -rn | head
```

## Update notifications

The daemon polls `https://api.github.com/repos/SnowWarri0r/eve-bridge/releases/latest`
once a day in the background, caches the result, and surfaces a release nudge in
two places:

* Daemon log at startup if a newer tag exists.
* `eve-bridge hud` appends `↑ vX.Y.Z` (or `up vX.Y.Z` in `--ascii`) to the
  statusline. Yellow, end of the line, so it doesn't push the battery/cost
  segments off-screen.

One-shot from the CLI:

```bash
eve-bridge check-update
# Installed:   0.1.0
# Latest:      v0.1.2
#
# Update available: 0.1.0 → v0.1.2
# Pull with:        git pull && pip install -e .
# Then restart:     eve-bridge install --service  (or kickstart the daemon)
```

Exit code is `1` when an update is available, `0` otherwise — handy in scripts.

### Applying the update

```bash
eve-bridge update            # prompts y/N, then does the thing
eve-bridge update -y         # skip the prompt (CI / scripts)
```

Equivalent to `git pull && pip install -e .` from the repo root, followed by
a service restart through whatever backend you installed (launchd / systemd
user unit / Task Scheduler). Bails early and loudly on:

- not running from a git checkout (e.g. you installed from a wheel) — fall
  back to your package manager
- uncommitted local changes — stash or commit first; we will not lose your work
- non-tty without `-y` — refuse to prompt blindly into the void

If the service backend isn't installed (you're running `eve-bridge
daemon` manually), the install step still runs and you'll get a "restart
the daemon yourself" reminder.

### Privacy

One HTTPS request per day to api.github.com. Disable entirely:

```bash
export EVE_BRIDGE_NO_UPDATE_CHECK=1
```

Cache lives at `~/Library/Caches/eve-bridge/update_check.json` on macOS,
`$XDG_CACHE_HOME/eve-bridge/...` on Linux, and
`%LOCALAPPDATA%\eve-bridge\update_check.json` on Windows.

Firmware update detection is out of scope for now — the stick's status ack
doesn't carry a firmware version, and upstream
[anthropics/claude-desktop-buddy](https://github.com/anthropics/claude-desktop-buddy)
has no releases or tags to compare against.

## CJK display on the stick (optional)

Stock firmware ships an ASCII-only 5×7 font — Chinese/Japanese/Korean
content in prompts and responses shows up as rows of `?` on the device
([quirk #1](#1-multi-byte-utf-8-strands-get-truncated-mid-character-on-the-ble-link)).
Upstream's CONTRIBUTING explicitly declines new features, so adding a CJK
font *and* getting it merged isn't realistic. Instead, this is implemented
as a **fork-only firmware build** that you flash yourself if you want
on-device CJK rendering. Stock-firmware users are unaffected — the bridge
keeps its conservative ASCII sanitizer until you tell it otherwise.

### Status

| Target | Firmware build | Bridge codec | Status |
| --- | --- | --- | --- |
| **Simplified Chinese (zh-CN)** | `m5stickc-plus-cjk-zh-cn` | `gbk` | ✅ Ready — GB2312 zones 1-55 (symbols + Level 1 hanzi), ~4300 glyphs |
| Traditional Chinese (zh-TW) | `m5stickc-plus-cjk-zh-tw` | `big5` | 🚧 Planned — bridge codec wired; firmware build still uses Simplified glyphs |
| **Japanese (ja)** | `m5stickc-plus-cjk-ja` | `shift_jis` | ✅ Ready — JIS X 0208 rows 1-47 (kana + Level 1 kanji), ~4400 glyphs; half-width katakana falls through to ASCII |

### How to enable (Simplified Chinese / Japanese)

The worked example below is for Simplified Chinese. For **Japanese**, substitute the
`feat/cjk-display-ja` branch, the `m5stickc-plus-cjk-ja` build, and `EVE_CJK_TARGET=ja`
(wire codec `shift_jis`) — the steps are otherwise identical.

1. **Flash the fork's CJK firmware variant.** Clone
   [SnowWarri0r/claude-desktop-buddy](https://github.com/SnowWarri0r/claude-desktop-buddy),
   check out `feat/cjk-display-zh-cn`, and run
   `pio run -e m5stickc-plus-cjk-zh-cn -t upload --upload-port /dev/cu.usbserial-...`
   (substitute your stick's serial path). The CJK build adds ~120 KB to
   the firmware binary; spiffs partition (where GIF character packs live)
   is untouched.

2. **Tell the bridge to send GBK bytes.** Add `EVE_CJK_TARGET=zh-CN`
   to the daemon's environment. On macOS:

   ```bash
   plutil -insert EnvironmentVariables.EVE_CJK_TARGET -string zh-CN \
       ~/Library/LaunchAgents/com.github.eve-bridge.daemon.plist
   launchctl bootout gui/$(id -u)/com.github.eve-bridge.daemon
   launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/com.github.eve-bridge.daemon.plist
   ```

   On Linux: edit `~/.config/systemd/user/eve-bridge.service`, add
   `Environment=EVE_CJK_TARGET=zh-CN` under `[Service]`, then
   `systemctl --user daemon-reload && systemctl --user restart eve-bridge.service`.

3. **Verify.** The daemon log prints `cjk firmware target=zh-CN, wire codec=gbk`
   at startup. Your next assistant turn with Chinese content should render
   the actual characters on the stick instead of `?`s.

To go back to stock behaviour: remove the env var and re-flash the default
`m5stickc-plus` build.

### What changes behind the scenes

- **Bridge wire format.** `sanitize_for_stick` switches from ASCII-only to
  GBK-encodable; `encode()` builds the heartbeat JSON manually with string
  values encoded as GBK bytes between quotes (keys, numbers, structural
  punctuation stay ASCII). ArduinoJson 7 on the stick accepts this — it
  doesn't validate UTF-8 inside string values.
- **Firmware rendering.** A sprite-aware mixed-script renderer
  (`cjk_render.cpp`) walks each byte: ASCII (< 0x80) draws a 6×12 glyph
  from `Fonts/ASC12.h`; GBK pairs (both bytes 0xA1-0xFE) draw a 12×12
  glyph from `Fonts/GB2312_L1.h`. `drawHUD` runs the line through a
  pixel- and GBK-aware wrap so long entries break cleanly instead of
  clipping at the right edge.

### Font credit

The CJK firmware variants use [Fusion Pixel Font](https://github.com/TakWolf/fusion-pixel-font)
12px monospaced (zh\_hans + Latin), released under the **SIL Open Font
License 1.1**. The OFL text plus per-source attributions for Ark Pixel,
Cubic 11, and Galmuri (which Fusion Pixel merges from) ship in the
firmware fork under `src/Fonts/`. Thank you @TakWolf for making this
available.

### Caveats

- Emoji (supplementary-plane codepoints) are not in any of the three
  byte-pair encodings; the sanitizer replaces them with `?` even in CJK
  mode.
- Level-2 hanzi (GB2312 zones 56-87, rarer characters) aren't shipped to
  save flash; they fall back to `??` at render time. ~99% of daily
  Mandarin use is in Level 1.
- The firmware variant is a downstream fork — there's no auto-update
  story. Re-build & re-flash manually when you pull new firmware changes.

## Listen key

Hold the Option key on the Mac and the board turns to face you and shows
its listening pose. Release it and the board goes back to what it was
doing. Option is the usual dictation hotkey, so the board listens while
you dictate. This only *watches* the key — eve never presses it.
(Hold-the-pet push-to-talk, which used to press it, has been removed.)

The daemon watches the key with a listen-only Quartz event tap and sends
`{"cmd":"listen","on":true}` on key down and `{"cmd":"listen","on":false}`
on key up. It sends each transition once, at most one per 150 ms, and only
while the board is connected. It also sends `on:false` every time the board
connects or reboots, so a reboot mid-hold never leaves the pose stuck.

Pick the key with `EVE_LISTEN_KEY`:

| Value    | Watches                          |
| -------- | -------------------------------- |
| `option` | the Option key (default on macOS) |
| `fn`     | the fn key                        |
| `off`    | nothing — the feature is disabled |

The event tap needs macOS **Input Monitoring** for the daemon's python.
Without it the daemon logs one warning at startup, `listen key: could not
create the event tap — grant Input Monitoring to <python> ...`, and runs
without the feature. To grant it: System Settings > Privacy & Security >
Input Monitoring, add the python binary from `bridge/.venv` (the warning
prints the resolved path), turn its toggle on, then restart the daemon.

## Host vision

The board has a camera but a slow, coarse on-board face detector. While the
Mac is attached the daemon does the looking instead: the board streams
small frames, the Mac runs Apple's Vision framework on each one, and the
board gets back where the largest face is so it can turn to look at you.
On-board tracking stays as the fallback for when no host asks.

Once per connect or reboot, after the time sync, the daemon sends
`{"cmd":"cam","on":true,"fps":5,"w":160,"h":120}`; on shutdown it sends
`{"cmd":"cam","on":false}`. The board answers with one line per frame:

```json
{"frame":{"seq":12,"w":160,"h":120,"fmt":"jpeg","b64":"...","yaw":4.0,"pitch":-2.0}}
```

`fmt` is `jpeg` (baseline JPEG) or `gray` (raw 8-bit luma, `w*h` bytes).
For every frame it processes the daemon replies with

```json
{"cmd":"face","seq":12,"bx":-8,"by":48,"size":36,"conf":59,"yaw":4.0,"pitch":-2.0,"who":"unknown"}
{"cmd":"face","seq":13,"conf":0,"who":"unknown"}
```

`bx`/`by` is the face centre relative to the frame centre in -100..100
(+bx = right of frame, +by = down), `size` is the face width as a
percentage of the frame width, `conf` is the detector's confidence, and
`yaw`/`pitch` echo the head pose the frame was taken at. `conf:0` without
`bx`/`by` means "frame seen, no face". `who` is `owner` when the face
matches the enrolled owner (see [Owner identity](#owner-identity)), else
`unknown`. Detection runs on one worker thread; while it is busy the
newest frame waits and older waiting frames are dropped, so the board
always gets an answer for a recent frame instead of a backlog.

The daemon logs the first frame (`vision: first frame 160x120 jpeg 3.1 KB`)
and then one line every 30 s (`vision: 148 frames, 12 dropped, 4.9 fps,
21 ms/detect, faces 87%`), never per frame. Without macOS Vision (Linux,
or `pyobjc-framework-Vision` missing) it logs one warning at startup and
never asks the board to stream.

Bench tools:

```bash
eve-bridge vision-test photo.jpg          # rects + the face cmd it would send
eve-bridge daemon --save-frames ~/frames  # dump received frames (max 1/s)
EVE_SAVE_FRAMES=~/frames                  # same, as an env var for the service
```

Saved frames are written as they arrived: `.jpg` for JPEG, `.png` for gray.

## Owner identity

The robot learns its owner's face and tags every face it sees. To enrol,
hold the Option key (the listen key) while facing the robot for a few
seconds. While the key is down, the daemon takes one print every 2 s of the
single face in the frame, as long as that face is at least 20 % of the
frame width — so a colleague in the background or a face at the far side of
the room never gets enrolled. Each enrolment logs `identity: enrolled owner
print #3 (size=31%, 3/24 held)`. It keeps up to 24 prints; when full, a new
print replaces the enrolled one most like it, so different poses and
lighting survive. Enrol again whenever recognition gets flaky (new glasses,
a beard, a lamp moved).

After that every face cmd carries `"who":"owner"` when the largest face is
within the distance threshold of any enrolled print and `"who":"unknown"`
otherwise. The daemon logs `identity: owner recognised (d=0.42)` and
`identity: unknown face (d=1.13)` when the answer flips, at most once per
30 s.

Prints are Apple Vision image feature prints (revision 2, 768 floats) of
the face crop padded by 25 %; they are not photos and cannot be turned back
into one. They live in `~/.config/eve-bridge/owner_faceprints.json`
(mode 600, written on every enrolment, loaded at daemon start). Vision's
distance is plain Euclidean over the print, which is what the daemon
computes; the default threshold is 0.9 (`EVE_OWNER_THRESHOLD` to
tune it — lower is stricter; the `d=` in the log lines tells you where your
own face and strangers land).

```bash
eve-bridge identity            # prints held, file, threshold, last face seen
eve-bridge identity reset      # forget the owner (in the daemon and on disk)
```

Without macOS Vision every face stays `unknown` and the daemon logs one
warning at startup.

## "hey eve": voice and computer control

Say **hey eve** and the daemon opens a spoken conversation that can run your
Mac. The wake word is spotted on-device (sherpa-onnx keyword spotting on the
Mac microphone, ~1 % of a core, no cloud until you speak); the conversation is a
Realtime session (`gpt-realtime-2.1-mini`, speech in and out, barge-in); a task
on the computer is a `gpt-6.1-terra` loop that screenshots, writes PyAutoGUI and
runs it in a worker process on the real desktop, up to 25 steps, asking out loud
before anything consequential, stopping on "stop", taking corrections mid-task.
The robot mirrors every phase (`{"cmd":"agent","state":..}`).

Full guide, setup (Microphone / Accessibility / Screen Recording grants, the
keyword model download), knobs and safety:
[docs/stackchan/voice.md](../docs/stackchan/voice.md). Quick checks:

```
eve-bridge ears-check          # mic + model, then listen for the phrase
eve-bridge key-check           # Accessibility state (swipe-to-key and the worker)
tail -f ~/Library/Logs/eve-bridge.log | grep -E "ears|voice|agent"
```

Besides running the computer, the conversation knows one more verb: *"hey
eve, go explore"* sends the robot off to look around the room right away
(same as `eve-bridge explore`; see [Idle explorer](#idle-explorer-and-the-diary)).

| Variable | Default | Purpose |
|---|---|---|
| `EVE_VOICE` | on | `0` disables the microphone and the wake word |
| `EVE_MIC_ALWAYS` | off | `1`: keep the microphone open from boot; default: open only while the robot is connected (`eve-bridge mic off` closes it for good) |
| `EVE_WAKE_WORD` | `hey eve` | any short English phrase (tokenised at startup, no training) |
| `EVE_WAKE_THRESHOLD` | `0.25` | lower = more sensitive |
| `EVE_MIC` | default input | input device name substring |
| `EVE_LIVE_MODEL` / `EVE_VOICE_NAME` / `EVE_VOICE_IDLE_SECS` | `gpt-live-1` / `marin` / `20` | the conversation |
| `EVE_COMPUTER_CONTROL` / `EVE_AGENT_MODEL` / `EVE_AGENT_MAX_TURNS` | on / `gpt-6.1-terra` / `25` | computer use; action logs under `~/.config/eve-bridge/agent-runs/` |

## Idle explorer and the diary

When Claude has been quiet for a while the robot stops waiting and looks
around. After `EVE_EXPLORE_AFTER_MIN` minutes (default 10) with no
session running or waiting, no hook event, no board touch, and the listen
key up, the daemon sends `{"cmd":"mode","explore":true}` and walks the head
through a pan plan: yaw -45, -20, 0, 20, 45 at pitch 40, then the same five
at pitch 60, one `{"cmd":"look","yaw":..,"pitch":..,"hold":6000}` every 6 s.
Two seconds after each look — once the head has settled — it keeps one
camera frame. If that frame differs from the last one it noted at that
waypoint (mean luma difference of 32x24 thumbnails above 12, or the first
visit), it spends a note: the frame goes to an OpenAI vision model and the
one-sentence answer is appended to a dated file. After the ten waypoints it
rests `EVE_EXPLORE_CYCLE_MIN` minutes (default 15) before the next
cycle — but it leaves the board in explore mode. Whenever the host is not
holding a `look` (between waypoints and through the whole rest), the
firmware looks around the room on its own: a random glance within yaw ±45,
pitch 35..65, held 2–4 s, then another, every 4–9 s. So an idle eve keeps
looking around; only the camera notes are rationed.

Anything that means the human is back stops it at once with `mode explore
false`: a hook event (a session running or waiting), the
listen key going down, a touch on the board, or the board disconnecting.
The idle clock then has to reach `EVE_EXPLORE_AFTER_MIN` again.

**Sending it off by hand.** You do not have to wait ten minutes:

```
eve-bridge explore            # go look around now
eve-bridge explore status     # off / exploring / resting, which waypoint, cycles, notes
eve-bridge explore stop       # come back
```

or say *"hey eve, go explore"* (also "look around", "go play"): eve answers
with a two-word send-off, the conversation ends, and the explore starts. A
manual explore ignores the idle clock — a running Claude session or a hook
event does not end it — and keeps panning cycle after cycle (with the usual
rest in between) until something says the human wants the robot back: a touch
on the board, the listen key, the next wake word, the board
disconnecting, or `explore stop`. It cannot start while the
listen key is down, or the board is disconnected (the command says why). It
works even with `EVE_EXPLORE=0`, which only turns off the idle start.

Log lines, at most one per event: `explore: start (idle 10 min)` or
`explore: start (requested by cli|voice)`, `explore: look yaw=-45 pitch=40`,
`explore: note -> <file>`, `explore: stop (<reason>)`.

**The env file.** The service runs with a fixed environment, so the API key
lives in `~/.config/eve-bridge/env` (make it `chmod 600`). Eve calls every
model with one key, `OPENROUTER_API_KEY` ([docs/openrouter.md](../docs/openrouter.md));
only the desk voice still needs `OPENAI_API_KEY`, until it moves to OpenRouter:

```
# KEY=VALUE, one per line; # comments; quotes optional
OPENROUTER_API_KEY=your-openrouter-key
```

Every `eve-bridge` command reads it at startup and fills any variable
that is not already set — a value from the shell always wins. Without a
key the robot still pans but takes no notes (one warning at startup).

**The diary (`diary.py`).** A note is no longer a caption. Each frame that
passes the change detector and the token bucket (`EVE_NOTES_PER_HOUR`,
default 6) becomes one `gpt-6-luna` call (`EVE_NOTES_MODEL`) that gets the
whole memory in context — eve's profile of the room and of you, what you have
starred, the last three thoughts, five older memories retrieved by recency ×
importance × relevance, the same hour on earlier days, today's unwritten
candidates — and returns concrete observations, what changed, three candidate
thoughts with a typicality score each, novelty, importance, tags and a feeling
(valence, arousal, label). The least typical specific candidate is kept; it is
**written to the day's file only if** novelty ≥ 5, importance ≥ 7, or the diary
has been silent for three hours — otherwise it stays in `memory.jsonl` as an
unwritten candidate. The feeling goes to the board as `{"cmd":"emote"}` and
nudges its affect engine. Each evening (or after a busy day) a text-only
**dreams** pass writes insights and ★ candidates under `## Dreams`, and rewrites
`profile.md`; `highlights.md` holds what *you* starred from the widget's diary
window and is always in context. Details and the research behind it:
[docs/stackchan/personality.md](../docs/stackchan/personality.md) § 3. Each call
has a 20 s timeout and no retries; failures are logged once per 10 minutes.

**Photos.** A few views are worth more than a sentence. When one scores high
enough on the *cool factor* — unlike anything in memory, or unlike what this
spot usually looks like, multiplied by how strongly eve felt about it — the
daemon asks the board for one full-resolution frame (`{"cmd":"snap"}`, 320x240)
and keeps it:

```
eve-bridge photos              # what eve has kept, newest first
eve-bridge photos open         # open the newest
```

The picture lands in `photos/YYYY-MM-DD/HHMMSS-<id>.jpg` under the notes
directory, its path goes into `memory.jsonl`, and the day's Markdown gets an
image line under the diary line, which is itself unchanged:

```
- 14:12 yaw=+20 pitch=40 — Someone brought a plant to my desk.
  ![](photos/2026-09-06/141207-118.jpg)
```

When a photo is kept, eve looks at it properly: the full-size picture goes
back at high detail and it works out what is actually in the frame, replacing
the caption, the tags and (when it says so) the sentence it guessed from the
thumbnail. The pan itself stays cheap.

eve also puts its thoughts on its own screen while it explores, but not all
of them — a screen on a desk is a claim on your attention in a way a memory file
is not. A thought reaches it only if it is interesting on its own terms, is not
about the thing eve just talked about, and enough quiet has passed: at most
one every four minutes and six an hour. Pat the robot on the head to call it
back from exploring.

A photo always earns its diary line, even when the write gate would not have.
Habituation stops repeats: the same picture again is refused, each earlier photo
of the same subject is worth less than the last, and a spot goes dull for a few
hours after eve photographs it. The bar moves with how interesting the week has
been, and the budget is 2 an hour and 8 a day. Everything eve has ★-starred in
the diary window tilts what it wants to photograph — the one part of the gate
that grows with its life. `EVE_PHOTOS_MAX` (300) and
`EVE_PHOTOS_MAX_MB` (100) cap the shelf, oldest first; the whole design and
the research behind it is [personality.md](../docs/stackchan/personality.md) § 4.

**Where it lives.** `EVE_NOTES_DIR` (default
`~/.config/eve-bridge/notes`, created mode 700): `YYYY-MM-DD.md` (the
diary, one line per written thought, `## Dreams` at night), `memory.jsonl`
(every record), `profile.md`, `highlights.md`, `photos/`. The dated files:

```
$ eve-bridge notes --last 3
2026-09-05 - 14:07 yaw=+20 pitch=40 — Two mugs on the desk by a window with daylight.
2026-09-05 - 14:13 yaw=+0 pitch=60 — The keyboard is unplugged and pushed aside.
2026-09-05 - 14:31 yaw=-45 pitch=40 — One person walked past the doorway.
```

`eve-bridge notes-test photo.jpg` sends a single image and prints the
sentence — one real call, for checking the key and model.

**Disable the idle start** with `EVE_EXPLORE=0` (in the env file or the
service's environment). The daemon then logs `explore: idle start disabled`
and only explores when told to (`eve-bridge explore`, "go explore").

## Requirements

* macOS 12+ / Windows 10+ / Linux with BlueZ
* Python 3.11+
* A flashed board: the Freenove pet, the CrowPanel e-ink dock, or the M5StackChan robot (M5StickC Plus with the upstream firmware also works)
* Claude Code CLI

## Signal mapping

| Eve field       | Source                                        |
| ----------------- | --------------------------------------------- |
| `total`           | `SessionStart` / `SessionEnd` hooks           |
| `running`         | `UserPromptSubmit` / deferred `Stop` hooks    |
| `waiting`         | `PreToolUse` hook (while decision pending)    |
| `prompt`          | `PreToolUse` hook payload                     |
| `msg`             | Derived summary of current state              |
| `entries`         | Live JSONL tailer (user prompts / tool calls / assistant text) |
| `tokens`/`today`  | Sum of `usage.output_tokens` in JSONL         |

## Firmware quirks we hit (and how we work around them)

The reference firmware has several sharp edges the wire protocol doesn't
warn you about. Documenting them here so you don't re-debug them, and so
the workarounds baked into this codebase have a visible rationale.

### 1. Multi-byte UTF-8 strands get truncated mid-character on the BLE link

A heartbeat carrying CJK (or any UTF-8 multi-byte content) can easily
exceed the default 20-byte ATT Write-Without-Response payload (`MTU − 3`
bytes per packet). [`bleak`](https://github.com/hbldh/bleak)'s
`write_gatt_char()` does not auto-chunk write-without-response — the
overflow is silently dropped. The firmware then sees a JSON message
ending mid-UTF-8 sequence (e.g. a trailing `0xE4` with the two
continuation bytes gone). ArduinoJson rejects the malformed JSON;
TFT_eSPI's `decodeUTF8()` state machine gets stuck waiting for the
continuation bytes that never arrive, corrupting subsequent reads. The
render or BLE task wedges and the link visibly resets ~1 s later.

**Two earlier misdiagnoses we ate, so you don't have to:**

- We first blamed the firmware's ASCII-only 5×7 GFX bitmap font
  (`96740fd`). That would have been right *if* whole CJK byte
  sequences ever reached the firmware — but they didn't.
- We then noticed the `M5StickCPlus` library ships an unused 1.7 MB
  HZK16 GB2312 font with a `loadHzk16(InternalHzk16)` API (`2099de1`).
  True but moot — the corrupted bytes never reach the font lookup
  either way.

The real root cause was diagnosed by
[@omengye](https://github.com/omengye) in their fork; full credit there.

**Fix landed in [`182bfed`](https://github.com/SnowWarri0r/eve-bridge/commit/182bfed)** (PR [#14](https://github.com/SnowWarri0r/eve-bridge/pull/14) by [@omengye](https://github.com/omengye)):
`EveBLE.send()` now chunks writes at `mtu_size − 3` *and* refuses to end
a chunk mid-codepoint (`_utf8_safe_chunks`). `sanitize_for_stick()` passes
all BMP codepoints through; only supplementary-plane (most emoji),
surrogates, and C0/C1 control chars are still replaced. Hook stdin is
decoded as UTF-8 regardless of the OS console codepage, so Windows
`cp936` users no longer see CJK mojibake.

### 2. `entries` wire order is oldest-first, not newest-first

Firmware's `drawHUD` treats `lines[nLines-1]` as the newest (and only
that one gets the highlight colour + bottom-of-window position).
Sending newest-first makes the latest entry land at the top of the
wrapped buffer and clip out of the visible 3-row window.

**Workaround:** the daemon keeps `state.entries` newest-first
internally (cheap prepend) but `reversed()`-iterates when serializing
the heartbeat.

### 3. `evt:"turn"` events are silently discarded

REFERENCE.md defines a `turn` event format, but the firmware's
`_applyJson` only parses heartbeat fields (`time`, `total`, `running`,
`waiting`, `tokens`, `tokens_today`, `msg`, `entries`, `prompt`). Any
`evt` payload is parsed and dropped — no error, no display.

**Workaround:** we mirror the assistant's first text block into the
heartbeat's `entries` list as a synthetic `@ <text>` row. The firmware
already renders `entries`, so no protocol extension is needed.

### 4. Stop hook fires before the assistant record is flushed to disk

Reading the transcript JSONL from the Stop hook returns the PREVIOUS
turn's content — Claude Code's write to disk is async. Naively this
causes every `@`-entry to be one turn behind.

**Workaround:** we ignore Stop for content extraction entirely. The
JSONL tailer already watches transcript files via `watchfiles`; it
fires an `on_assistant_text` callback the moment a new assistant
record lands (typically <500 ms). The callback adds the entry
immediately, so the stick shows the reply before the user even
scrolls up in the terminal.

### 5. Clock mode hides the transcript HUD on turn end

The firmware enters clock-face mode the instant
`running==0 && waiting==0 && on_USB_power`, bypassing `drawHUD`
entirely. Our old `turn_end` handler flipped `running` to 0 the
moment Claude finished — which made the freshly-emitted `@` entry
invisible within the same frame.

**Workaround:** `turn_end` schedules an `asyncio.Task` that sleeps
15 seconds before flipping `running` to 0. A new `turn_begin` cancels
the pending task. The stick stays on HUD long enough to read the
reply, then goes to clock on genuine idle.

### 6. LittleFS is not auto-formatted — `push-character` fails until factory reset

Fresh firmware calls `LittleFS.begin(false)` (no format-on-fail), so an
uninitialised partition mounts as 0/0 bytes. The only code path that
calls `LittleFS.format()` is the on-device **factory reset** menu
(hold **A** → settings → reset → factory reset → tap twice).

`eve-bridge push-character` detects this via the status ack and
logs an `ERROR` with the remediation hint. Factory reset is destructive
(wipes settings, stats, bonds) but needed once per stick.

### 7. `blueutil --unpair` is unreliable on modern macOS

For a clean BLE pairing test you need to clear both sides' bonds.
`blueutil` advertises `--unpair` as `EXPERIMENTAL`; on macOS Sonoma+
it returns success without actually removing the cached LTK, and a
subsequent reconnect fails with `CBErrorDomain Code=14 "Peer removed
pairing information"`.

**Workaround:** `eve-bridge unpair` clears the stick side over
the encrypted channel, but the user has to manually open
**System Settings → Bluetooth → Claude-5C66 → ⓘ → Forget This
Device** on the macOS side. After that, the next reconnect triggers
a fresh 6-digit passkey pairing.

## Status

Daily-driver complete — the author runs it on every Claude Code session.

**Battle-tested infra**

* Fresh BLE pairing — MITM + bonding + DisplayOnly passkey, end-to-end
* Reconnection — exponential backoff + multi-daemon guard (refuse to start if another instance owns the socket)
* Folder push — chunked flow control, 1.8 MB pack cap, per-chunk acks
* Stick status polling — battery / encryption / fs free every 60 s
* Logging — rotating file, per-component levels, structured permission round-trip traces

**Tests + CI**

* 212 unit tests covering state, protocol, installer, hud, matchers, JSONL tailer, folder push, service backends, BLE radio recovery
* GitHub Actions matrix across Python 3.11 / 3.12 / 3.13

**Backlog**

* Open an issue — any rough edge, a quirk you hit, a feature you want, a platform that misbehaves

## Contributing

PRs, bug reports, and "I tried it on $WEIRD_LINUX_DISTRO and it broke" stories
all welcome. For anything larger than a small bug fix, open an issue first so
we can talk through the design.

### Dev setup

```bash
git clone https://github.com/SnowWarri0r/eve-bridge
cd eve-bridge
python3.12 -m venv .venv
.venv/bin/pip install -e '.[dev]'
```

The `[dev]` extra pulls in `pytest` + `ruff` (the only dev deps).

### Test & lint

```bash
.venv/bin/pytest -q                  # ~210 tests, finishes in <1s
.venv/bin/ruff check src/ tests/     # lint (CI runs this on every PR)
```

CI runs the test suite across **macOS / Linux / Windows × Python 3.11 / 3.12 / 3.13**.
A PR turns green only when every cell is happy — if you touch anything
filesystem-y or path-y, expect Windows to surface the quirks first (NTFS
ignores POSIX mode bits, backslash vs forward-slash, etc).

### Before touching the wire protocol

The stick firmware has [7 documented sharp edges](#firmware-quirks-we-hit-and-how-we-work-around-them).
Scan that section before chasing weird BLE behaviour through `bleak`. Most
"the link keeps flapping" issues turn out to be quirk #1 (non-ASCII bytes
crash the BLE stack) or quirk #5 (clock mode racing the HUD).

### Commit messages

Short subject line, lowercase, ≤ 70 chars; then a paragraph explaining **why**.
Browse `git log --oneline` for the register. Don't paste emoji — the
sanitizer would strip them from the stick anyway.

### Translations

The README mirrors across [English](README.md), [简体中文](README.zh-CN.md),
and [日本語](README.ja.md). If you touch user-facing prose, please mirror
across all three when you can; otherwise note in the PR that the other two
need a translation pass — happy to take that as a follow-up.

### Firmware patches

The eve firmware lives at [anthropics/claude-desktop-buddy](https://github.com/anthropics/claude-desktop-buddy).
Changes there need a flashed M5StickC Plus to verify — bridge-side mocks
won't catch wire-protocol misalignments. Be explicit in the PR description
about what you've tested vs. what's still theory; reviewers can't tell from
the diff alone.

## Star history

<a href="https://star-history.com/#SnowWarri0r/eve-bridge&Date">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://api.star-history.com/svg?repos=SnowWarri0r/eve-bridge&type=Date&theme=dark" />
    <source media="(prefers-color-scheme: light)" srcset="https://api.star-history.com/svg?repos=SnowWarri0r/eve-bridge&type=Date" />
    <img alt="Star history chart for SnowWarri0r/eve-bridge" src="https://api.star-history.com/svg?repos=SnowWarri0r/eve-bridge&type=Date" />
  </picture>
</a>

## License

MIT. See [LICENSE](LICENSE).
