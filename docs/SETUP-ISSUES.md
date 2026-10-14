# Setup issues found during a from-scratch install

Recorded 2026-08-07 while bringing up the bridge on a clean machine (macOS 26.5,
Freenove FNK0104B on USB serial, era-code-wrapped Claude Code, Willow Voice).

Everything below is a real failure encountered in order, with the evidence that
identified it. The common thread: **every one of these fails silently.** Nothing
crashed, nothing printed an error at the point of failure — the pet just sat
there working-but-inert, which is the hardest possible thing to debug.

> **Since recorded:** hold-the-pet push-to-talk and the permission swipe card
> were both removed (owner request). Issues **3, 5, 6, 7, 8 and 9** are about
> the push-to-talk hotkey and are now moot — kept as a record of what the
> machine did, not as open work. `voice_trigger.py` is now `key_tap.py` and
> `voice-check` is now `key-check`; the Accessibility findings still apply to
> the swipe-to-key path, which is the only thing that still synthesizes keys.

> **eve lite (2026-10-13):** the issues met while building the variant for a Mac with
> no developer tools, and one of full eve's found on the way, are at the end:
> [eve lite](#eve-lite).

---

## 1. `install --service` silently defaults to BLE

**Severity: high — this alone makes a USB board look dead.**

`eve-bridge install --service` without `--serial-port` bakes a BLE-transport
service. On a USB-serial board the daemon scans Bluetooth forever:

```
INFO eve_bridge.ble: no eve device found, retrying in 3.0s (miss #1)
```

The board is enumerated, powered, and healthy the entire time. Nothing indicates
the transport was chosen wrongly.

**Fix options:** auto-detect an attached supported board by USB VID at install
time and default to serial; or refuse to install a BLE service when a known board
is present on serial without an explicit `--transport ble`.

**Owner:** `claude-pet` — `bridge/src/eve_bridge/_service_launchd.py:51-54`,
`cli.py:39-52`

---

## 2. `install --service` skips hook installation entirely

**Severity: high**

`--service` is documented as installing a service *"instead of registering
hooks"* (`cli.py:46`), and the dispatch is a hard either/or (`cli.py:147-152`).
Running the obvious one-liner leaves you with a running daemon and **zero hooks**
— the board connects and shows battery, so it looks installed, but no session
state ever reaches it.

`eve-bridge status` does report `no eve-bridge hooks installed`, but
only if you think to run it.

**Fix options:** make `--service` additive (install hooks *and* the service, which
is what essentially every user wants); or print a loud "hooks NOT installed — run
`eve-bridge install`" line at the end of a `--service` run.

**Owner:** `claude-pet` — `bridge/src/eve_bridge/cli.py:147-152`

---

## 3. `key-check` (then `voice-check`) reports a trust result that does not apply to the daemon

**Severity: high — this one actively sent debugging in the wrong direction.**

`voice-check` run from a terminal reports the trust of *that* process. macOS
attributes Accessibility to the responsible process, so running it from a granted
terminal (Warp, iTerm) returns:

```
accessibility trusted: True
```

…while the launchd daemon, which has no responsible app and is evaluated as the
bare ad-hoc-signed interpreter, simultaneously logs:

```
WARNING eve_bridge.voice_trigger: key: Accessibility not granted
```

Both readings were true at the same moment. The CLI reported success for a
permission the daemon did not have, and we acted on it — twice.

**Fix options:** have `voice-check` query the *running daemon* over IPC for its
own trust state (add a `{"evt":"voice_trust"}` handler) and report that as the
headline number; demote the in-process check to a secondary line explicitly
labelled "this terminal, not the daemon".

**Owner:** `claude-pet` — `bridge/src/eve_bridge/voice_trigger.py:156-173`,
plus the `voice-check` CLI path

---

## 4. `launchctl kickstart -k` does not pick up plist edits

**Severity: medium — and `voice-check`'s own FIX text recommends it.**

`voice-check` ends with:

```
4. restart the daemon:
   launchctl kickstart -k gui/$(id -u)/com.github.eve-bridge.daemon
```

`kickstart` restarts the process but reuses the job definition cached at load
time. Any edited `EnvironmentVariables` are ignored. Observed directly:

| | |
|---|---|
| plist on disk | `EVE_VOICE_HOTKEY=fn` |
| running process env | *(unset)* → fell back to `option` |

This produces a latent time-bomb: the running daemon behaves one way, and the
next login loads the plist fresh and behaves another, with no error either way.

**Fix:** use `launchctl unload && launchctl load -w` (or `bootout`/`bootstrap`)
wherever a config change is involved, and correct the `voice-check` FIX text.

**Owner:** `claude-pet` — `voice-check` output, `_service_launchd.py`

---

## 5. No way to persist the voice hotkey through `install --service`

**Severity: medium — MOOT: push-to-talk removed, the hotkey no longer exists.**

`--serial-port` is bakeable into the service; the voice hotkey is not
(`_service_launchd.py:51-54`). Setting it requires hand-editing the plist, and
the edit is destroyed by the next `install --service`, silently reverting to
`option`.

**Fix:** add `--voice-hotkey` to `install --service`, threaded exactly like
`--serial-port` (`cli.py` → `service.py` → `_build_plist`).

**Owner:** `claude-pet`

---

## 6. The Accessibility prompt fires at most once per daemon lifetime

**Severity: medium**

`voice_trigger.py:203,220` shows the system dialog once per daemon life
(`self._prompted`). This is correct — prompting on every failed hold would be
awful — but if the dialog is missed, dismissed, or appears while the user is
looking at the board rather than the screen, there is **no way to get it back**
short of restarting the daemon, and nothing says so.

The dialog is by far the easiest path to the grant (it has an "Open System
Settings" button that adds the entry directly), so losing it is costly. We
burned a long stretch on manual `+` / Cmd+Shift+G / drag attempts before
realising a daemon restart re-arms the prompt.

**Fix:** when a voice event arrives and trust is absent and the prompt was
already spent, log the actionable line: *"restart the daemon to re-show the
Accessibility prompt: launchctl kickstart -k …"*.

**Owner:** `claude-pet` — `bridge/src/eve_bridge/voice_trigger.py:203-220`

---

## 7. Manual Accessibility-grant paths are hostile, and the docs suggest the worst one

**Severity: low (documentation)**

Observed, in order of failure:

- **Drag from Finder into the list** — silently rejected. The list does not
  accept bare Unix executables on drop.
- **Cmd+Shift+G before clicking `+`** — does nothing; the shortcut only exists
  inside the file picker.
- **Cmd+Shift+G with focus in the picker's search field** — the path lands in
  the *search box*, which then searches "This Mac" for the literal string and
  returns nothing.

Only `+` → picker → *click out of the search field* → Cmd+Shift+G → paste works,
and `~/.local` being hidden means browsing to it is impossible.

**Fix:** point the FIX text at the system prompt (issue 6) as the primary route
and describe the `+` sequence with the focus caveat as the fallback.

**Owner:** `claude-pet` — `voice-check` output

---

## 8. Accessibility grants are keyed to an ad-hoc signature

**Severity: low, but a guaranteed future outage**

`voice-check` already calls this out well:

```
code signature: ad-hoc / linker-signed
  note: ad-hoc-signed interpreters (uv/pyenv builds) are the
  usual cause of a grant that 'won't stick'
```

Consequence: a `uv tool upgrade` that pulls a new interpreter build invalidates
the grant, and the pet goes quiet with no error. Worth a line in the README's
troubleshooting section, since the symptom is indistinguishable from every other
silent failure here.

**Owner:** `claude-pet` — README / docs

---

## 9. `DEFAULT_HOTKEY` contradicts its own docstring

**Severity: trivial — MOOT: the hotkey table was removed with push-to-talk.**

`voice_trigger.py:9-11` states *"`fn` for Willow Voice (default)"*, but
`DEFAULT_HOTKEY == "option"`. One of the two is wrong. (Empirically on this
machine `option` is what works with Willow, so the docstring is the error.)

**Owner:** `claude-pet`

---

## 10. Serial links inherit a BLE security warning

**Severity: trivial**

```
INFO eve_bridge.daemon: stick link: UNENCRYPTED — transcript sniffable!
```

Emitted on a USB serial link, where the "link" is a physical cable. The warning
is meaningful for BLE and noise for serial.

**Fixed 2026-10-02:** `EveSerial` is marked `wired`, and on a wired link the
daemon ignores the board's `sec` flag. It no longer logs the warning, and the
HUD leaves the lock segment out instead of showing `UNSEC`. BLE links still
warn.

**Owner:** `claude-pet`

---

## Non-issues, recorded so they aren't re-investigated

- **`~/.local/bin` not on `PATH`** after `uv tool install`. uv warns about it;
  the daemon is unaffected (launchd uses an absolute interpreter path). Only
  affects typing `eve-bridge` by hand. `uv tool update-shell` fixes it.
- **`voice_trigger.py` absent from site-packages.** Expected under an editable
  install — the module resolves to `bridge/src/eve_bridge/`.
- **One board reset mid-session.** The pre-reset event ring was dumped to the log
  and the daemon reconnected on its own. The crash-reporting and auto-recovery
  features worked as designed.
- **era-code hook collision.** era-code 3.19.1 preserved the eve hooks when
  regenerating `settings.json`; `governance` / `session-start` and all 8 eve
  hooks coexist. The mitigation described in `claude_home.py:88-94` holds.

---

## Final working configuration

| | |
|---|---|
| Transport | serial, `/dev/cu.usbmodem*` (glob survives replug renumbering) |
| Claude homes | `~/.era/era-code/claude-home` + `~/.claude` |
| Hooks | 8, in the era-code home's `settings.json` |
| Voice hotkey | `option` (default — no plist override) |
| Accessibility | granted to `…/uv/python/cpython-3.12.13-macos-aarch64-none/bin/python3.12` |

Reproducing this from scratch, in the order that actually works:

```bash
uv tool install --editable ./bridge
eve-bridge install --service --serial-port '/dev/cu.usbmodem*'
eve-bridge install          # separate — --service does NOT do this
# hold the pet once → approve the system Accessibility dialog
launchctl unload ~/Library/LaunchAgents/com.github.eve-bridge.daemon.plist
launchctl load -w ~/Library/LaunchAgents/com.github.eve-bridge.daemon.plist
```

---

## eve lite

Recorded 2026-10-13 while building [eve lite](lite.md), the variant installed with one line on a Mac
that has no developer tools. These were found by reading the system, its tools and Apple's notes on
the owner's Mac (macOS 27.0), not on a fresh Mac: manual gate M1 runs the install on one, and is
still pending. The fixes below are in the code; what is not yet seen working says so.

### 11. Every developer tool in `/usr/bin` is the install dialog

**Severity: high for eve lite, which is for exactly these Macs.**

On macOS 27.0, 78 programs in `/usr/bin` are one file under 78 names: `git`, `clang`, `cc`, `make`,
`python3`, `pip3`, `install_name_tool`, `swift` and the rest share `/usr/bin/git`'s inode, and 16
more (`xcodebuild`, `xctrace` and their kin) are a second such file for Xcode, with `xcrun` the
dispatcher both call (counted 2026-10-13: `toolchain.SHIMS`, `toolchain.XCODE_SHIMS`). Each one
looks for Xcode or the Command Line Tools and runs their copy. Finding neither, it opens the dialog
that offers to install the command line developer tools, and runs nothing.

So "is there a git" is not `which git`: on such a Mac `/usr/bin/git` is there, and running it is the
dialog. Full eve runs git for its records on every nightly dream, and for the commit in its start-up
line.

**What the code does now:**
- `toolchain.dev_tools_present()` reads the places `xcode-select` looks (`DEVELOPER_DIR`, the
  `xcode-select -s` link, Xcode in `/Applications`, the Command Line Tools) and starts nothing:
  asking `xcode-select -p` or `xcrun` is itself a way to raise the dialog.
- `records._git` asks `toolchain.git_usable()` before every git. It is false in eve lite. Anywhere
  else, `/usr/bin/git` counts only while the developer tools are present, so full eve on a Mac
  without them keeps its records without history instead of opening the dialog. On a Mac with Xcode
  or Homebrew's git, nothing changes.
- eve lite's process firewall refuses all of them anyway ([the process firewall](lite.md#the-process-firewall)).
- `bridge/tests/test_no_devtool_spawns.py` lists every place eve's code names one of them as a
  program, with why it cannot reach the dialog in eve lite; a new place fails it.

### 12. uv starts `install_name_tool` by name while it installs Python

**Severity: high, and silent until the dialog appears mid-install.**

uv 0.11.27, installing its own CPython on macOS, runs `install_name_tool` by name to set the
library's install name (its `crates/uv-python/src/macos_dylib.rs`, seen in the binary's strings).
With `/usr/bin` on `PATH` that is the shim of issue 11, and the dialog opens during the install.

**Fix, in `install-lite.sh`:** every uv call runs under `env -i` with `PATH` set to
`~/.eve-lite/bin` alone, plus `HOME`, `TMPDIR`, uv's own settings and any proxy settings. uv then
finds no `install_name_tool`, and by its code only warns. `UV_PYTHON_PREFERENCE=only-managed` keeps
uv from looking at `/usr/bin/python3`, another shim, and `UV_NO_CONFIG=1` from reading a uv
configuration the Mac may have. The installer itself starts no shim and no package manager. Not yet
seen on a fresh Mac: spike S6 and manual gate M1 check that the install raises no dialog there.

### 13. macOS knows eve lite as "python3.13"

**Severity: medium: prompts that do not say "eve", and grants tied to one binary.**

eve lite's login service runs uv's CPython 3.13.14, so macOS names python3.13, not eve: in
Background Items Added at install, in the Local Network alert, and in System Settings' lists. macOS
ties a privacy grant to the program's code signature and executable (Apple's TN3179). uv's Python is
signed ad hoc, and every copy of 3.13.14 is the same binary, so where another copy was allowed or
denied before, the alert may not appear at all; and a new Python patch is a new binary whose grants
start over (issue 8 is the same with Accessibility).

**What eve lite does:** the installer pins `PYTHON_VERSION=3.13.14`; the setup page and
**Settings → Phone access** say beforehand that python3.13 is eve; `eve-bridge doctor` reports the
Local Network state eve measured. A signed launcher, with eve's own name in the prompts, needs a paid
Apple developer account (the owner's open question 13 in [eve lite](lite.md#the-owners-open-questions)).

### 14. The Local Network and firewall prompts, at "Add a phone"

**Severity: medium: denied, a phone still chats, but voice is off.**

TN3179: a login service needs the Local Network privilege to send UDP on the local network, and
never to accept TCP. So eve lite's chat never needs it: the Mac's own page is loopback, and a
phone's HTTPS reaches the LAN door as incoming TCP. WebRTC (the direct link and voice) sends UDP, so
it does.

- **The prompt comes on purpose,** when the person is looking at the Mac: at **Add a phone**,
  `lan_net.trigger_local_network_alert()` connects a UDP socket to the mDNS group
  (224.0.0.251:5353) and sends nothing. The default gateway was not used for this: a home gateway is
  usually the network's DNS server, which TN3179 exempts, so it would raise nothing.
- **The state is measured,** not guessed: when a phone pairs or makes a link, eve sends one empty
  datagram to the phone's discard port, and EHOSTUNREACH or EPERM means blocked. The phone's top bar
  then says "Direct link blocked, using HTTPS", and `eve-bridge doctor` says `Local Network: blocked`.
- **The macOS firewall,** when it is on, asks at the LAN door's first connection whether python3.13
  may accept incoming connections. Denied there, no phone reaches eve at all.

Not yet seen on a fresh macOS user: spike S3 (the alert, on Allow and on Deny) and manual gate M6.

### 15. Each phone clicks through a certificate warning once

**Severity: medium: one scary page per device, by design.**

The LAN door has to be HTTPS: a phone's browser lets a page use the microphone only in a secure
context. No public CA signs for `<name>.local` or a private address, so the Mac makes its own CA.
Each phone warns once, and again whenever the LAN certificate is made again (a new address, or a
month before it lapses), unless it installed eve's CA from **Settings → Trust eve on this device**.
The CA's name constraints (critical) allow only `.local`, `localhost` and private ranges, so
installing it cannot make a device trust any other site.

Not yet verified (spike S2, manual gate M2): that iOS Safari, after the click-through, treats the page
as a secure context and keeps its Secure cookie. A browser that drops the cookie shows "This browser
paired but didn't keep eve's sign-in", and the fix is the CA. On first use, the device trusts
whatever answered at that address: pair on a home network, not a café's.

### Full eve: `install.sh` stops on a Mac without `~/.claude`

**Severity: high for full eve on a new Mac. Not eve lite's: its installer is separate, and
`install.sh` is not touched in this work (decision 7 in [eve lite](lite.md#the-owners-open-questions)).**

`install.sh` runs under `set -euo pipefail`, and its step 5 runs `"$eve" install` (line 164) before
the login service. `eve-bridge install` installs Claude Code's hooks, and exits 2 with "Claude config
home not found at …" when that folder does not exist (`installer.install_hooks`). On a Mac where
Claude Code has never run there is no `~/.claude`, so the installer stops there: no login service, and
none of its closing steps.

**Fix (a separate change):** `"$eve" install || warn …`, so a Mac without Claude Code skips the
hooks and the install goes on. Until then, run Claude Code once first (it makes `~/.claude`), or
install with `--no-service` and then run
`~/eve/bridge/.venv/bin/eve-bridge install --service --serial-port '/dev/cu.usbmodem*'` by hand.
