# eve lite: eve in a browser, with one install line and two sign-ins

eve lite is a second way to install and run eve, for people other than the owner. The owner, 2026-10-13: "create a
variant of eve that can be one click onboarding … use composio and openrouter as the only setup things", "the google
env key setup is unnecessary for this variant, i want it to be one click install", "make the eve app a web app so
that it can just be accessed from same wifi through web rtc", and "this is so that people dont need to have xcode and
stuff setup".

- **Install:** one line in Terminal on an Apple-silicon Mac. No Xcode tools, no Homebrew, no git, no iPhone app.
- **Setup:** two sign-ins in the browser, OpenRouter (the model key) and Composio (the apps, optional). No key is typed
  or pasted anywhere.
- **The app:** the Mac serves eve as a web page. The Mac's own browser opens `http://localhost:48780`. A phone or laptop
  on the same Wi-Fi scans a QR code, opens eve over HTTPS and talks to the Mac over WebRTC: chat on a DataChannel,
  push-to-talk voice on an audio track.

It is the same code as full eve with other switches, chosen by one variable, `EVE_PROFILE=lite`, which only its
installer writes. With `EVE_PROFILE` unset, eve is the owner's full eve, unchanged
([why](#why-full-eve-does-not-change)). The build spec is [plans/eve-lite.md](plans/eve-lite.md); its gates are
[lite-gates.md](lite-gates.md).

## Status

Written 2026-10-13, on branch `feat/one-click-variant`.

- **lite-v0.1.3 is published** (2026-10-14; lite-v0.1.2 the same day), so the install line works. The install line downloads `install-lite.sh`
  from the newest published release of github.com/gurul/eve_01 ([making a release](#making-a-release)). A copy of
  `install-lite.sh` taken from the repository instead of a release refuses to install: it has no release to fetch. The
  release script stamps the repository's copy too; leave that uncommitted, as the tests expect it unstamped.
- **Installed and set up on the owner's Mac (2026-10-14).** The owner installed it from the release's tarball, signed
  in and reported that it works. The other checks that need a phone or a fresh VM are listed under
  [Verified on](#verified-on), still pending.
- **The lite prompts have not passed their blind eval.** The bar and the blind set are written
  ([the eval](evals/lite-prompt.md)); the scored run costs money and waits for the owner (AGENTS rule 6).
- **Thirteen decisions were taken without the owner.** They are at the end, as [open questions](#the-owners-open-questions).

## Install

```bash
curl -fsSL https://github.com/gurul/eve_01/releases/latest/download/install-lite.sh | bash
```

This line installs the newest published release, lite-v0.1.3 at the time of writing ([Status](#status)).

You need a Mac with Apple silicon (M1 or later) on macOS 14 or later, and to run it as yourself, not with `sudo`.
Intel Macs and macOS 13 are not supported: cryptography's macOS wheels are for arm64 only, and av's arm64 wheel needs
macOS 14. The installer checks the hardware itself, so a Terminal running under Rosetta does not fool it.

One status line updates in place, and the details of each run go to `~/.eve-lite/install.log`. It asks no questions.

| Step | Status line | What it does |
|---|---|---|
| 1 | `getting Python (1/5)` | Downloads uv 0.11.27 from its GitHub release, checks its SHA-256 and puts it in `~/.eve-lite/bin`. Then uv installs its own CPython 3.13.14 into `~/.eve-lite/python`. |
| 2 | `getting eve lite <version> (2/5)` | Downloads the release tarball, checks its SHA-256 before opening it, then checks each file in it against the release's `SHA256SUMS`. |
| 3 | `installing packages, a few minutes the first time (3/5)` | Makes `~/.eve-lite/venv` and installs the release's hash-pinned wheels (wheels only, nothing is compiled), then eve-bridge. |
| 4 | `starting eve (4/5)` | Writes `EVE_PROFILE=lite` into `~/.config/eve-bridge/env` (mode 600), makes `~/Applications/Eve.app`, and installs the login service `com.github.eve-bridge.lite`. |
| 5 | `opening setup (5/5)` | Opens the setup page in your default browser, already signed in. |

It ends with one line: `eve lite is running. Setup is open in your browser: http://localhost:48780`. macOS may also
post "Background Items Added" for python3.13: that is eve's login service, and nothing needs a click.

**What it never starts:** git, Homebrew, `xcode-select`, `python3`, `cc`, `clang`, `make`, or any other of the
developer-tool stand-ins in `/usr/bin`. Each of those opens macOS's "install developer tools" dialog on a Mac without
Xcode's tools. uv starts `install_name_tool` by name while it installs Python, so every uv call runs with a `PATH` that
holds only `~/.eve-lite/bin`, where uv finds none and, by its code, only warns; spike S6 checks this on a fresh Mac
([SETUP-ISSUES](SETUP-ISSUES.md#eve-lite)). Proxy and
certificate settings (`HTTPS_PROXY`, `SSL_CERT_FILE` and their kin) are passed on, so uv reaches the internet the way
curl does.

**When it refuses, and changes nothing:**
- full eve is installed for this macOS user (`~/Library/LaunchAgents/com.github.eve-bridge.daemon.plist` exists);
- `~/.config/eve-bridge/env` exists and does not set `EVE_PROFILE=lite`, so it is full eve's.

Full eve and eve lite keep their data in the same `~/.config/eve-bridge`, so one macOS user has one of them.

**Options**, after `bash -s --` when piped:
- `--dry-run`: check this Mac and print what an install would do; nothing is written.
- `--uninstall`: remove eve lite ([update and uninstall](#update-and-uninstall)).
- `--no-start`: install into `~/.eve-lite` only, with no env file, app or login service. The release script checks each
  release this way.

## Setup: two sign-ins

The page is titled "Set up eve" and has two steps. Each sign-in leaves for the provider's page and comes back. eve
saves what it got, then restarts in about 5 seconds, and the page waits for it and carries on by itself.

1. **Sign in with OpenRouter.** openrouter.ai opens: sign in or sign up, then click **Authorize**. OpenRouter names
   the app `localhost:48780`, its name for any app on this Mac, and the setup page says so beforehand. eve gets its
   model key through OAuth PKCE, labelled "eve lite on <this Mac>" in your OpenRouter account
   ([how](openrouter.md#eve-lite-the-key-from-a-browser-sign-in)). Back on the page: "OpenRouter connected", with the
   key's credit when OpenRouter gives a figure. With no credit, the page offers **Add credits** and **Check again**.
2. **Sign in with Composio**, or **Skip for now**. connect.composio.dev opens: sign in or sign up, then click
   **Allow**. Back on the page: "Composio connected", eve restarts again, and the page turns into the chat
   ([how](apps.md#eve-lite-composio-connect)).

With Composio skipped, chat, voice, memory, web search and watches all work, and **Settings → Apps** offers
**Sign in with Composio** later. A sign-in that does not finish (cancelled, refused, or left open past 10 minutes)
shows one plain line, on a short page and on the setup page; start it again from the setup page.

## On the Mac

- **Open Eve** from Applications (or ⌘ Space, then Eve). It runs `eve-bridge web open`, which gets a one-time code
  from the daemon over its private socket and hands it to the browser in a private file (mode 600). The code is never
  shown, typed, put in a command line or logged, works once, and lapses after 2 minutes. The browser then holds a
  cookie (`eve_mac`, 400 days from its last visit).
- **A bookmark** of `http://localhost:48780` works while the browser keeps that cookie. `http://127.0.0.1:48780` moves
  to `localhost`, whose cookie it is.
- **Without Eve.app**, run `~/.eve-lite/venv/bin/eve-bridge web open` in Terminal. eve-bridge is not put on your
  `PATH`; every command below is that full path.

The Mac's page is plain HTTP on loopback only (127.0.0.1 and ::1), on port 48780, or the first free one of
48782–48789 when that is taken; the port is kept in `web.json`, so bookmarks stay put. A browser treats `localhost`
as a secure context, so the microphone works there without a certificate.

## Phones and laptops

On the Mac's page: **Settings → Phone access → Add a phone**.

1. **macOS asks** to allow "python3.13" to find devices on local networks. Click **Allow**: that is eve (its login
   service runs uv's python3.13, and macOS names the program, not the app). If the macOS firewall is on, macOS also
   asks whether python3.13 may accept incoming connections: **Allow** again.
2. **The page shows** two QR codes, a 6-digit code and a 5-minute countdown:
   - **iPhone or iPad:** `https://<the Mac's name>.local:48781/#p=…`
   - **Android or other:** `https://<the Mac's address>:48781/#p=…`
3. **On a phone,** open the Camera, scan the code for it and tap the link. The browser warns once about eve's
   certificate: on an iPhone tap **Show Details**, then **visit this website**; in Chrome tap **Advanced**, then
   **Proceed**. The page then pairs itself: it trades the secret in the link for a device cookie, takes the secret out
   of the address bar and shows the chat.
4. **On a laptop,** type the address shown under a code, click through the same warning, and enter the six digits.
   Safari needs nothing more. Chrome on another Mac asks once for its own Local Network permission: Apple exempts only
   Safari and web views (TN3179).

A pairing window pairs one device, lasts 5 minutes, and closes after 5 wrong tries; a new one replaces the old. The
secret is in the link's fragment (after `#`), which a browser never sends to a server. The phone's cookie (`eve_dev`:
HttpOnly, Secure, SameSite=Strict) lasts 400 days from its last visit, so a bookmark opens straight into the chat.

**Ending the certificate warning for good** (optional): on the phone or laptop, **Settings → Trust eve on this
device**.
- **iPhone or iPad:** **Download eve's profile**, then in Settings tap **Profile Downloaded** and **Install**; then
  **Settings → General → About → Certificate Trust Settings**, and turn on **eve lite local CA**.
- **Android:** **Download eve's certificate**, then **Settings → Security → Encryption & credentials → Install a
  certificate → CA certificate**.
- **A computer:** open the downloaded certificate and trust it for websites.

eve's CA can vouch only for `.local` names, `localhost` and private, link-local and loopback addresses: those name
constraints are marked critical, so installing it cannot make a device trust any other site. Its key never leaves the
Mac. It lasts 5 years; the certificate the LAN door serves lasts 397 days.

**The LAN door** (`LanServer`) is what phones and laptops reach:
- It listens on every network interface, IPv4 and IPv6, on port 48781 (else the first free of 48790–48799), from the
  first **Add a phone** on. It stays on across restarts, and across networks: once on, the Mac listens on any private
  network it joins, a café's included. **Settings → Phone access → Turn off**, or `eve-bridge web lan off`, closes it.
- Before TLS starts, it drops a connection from any address that is not loopback, private (RFC 1918), link-local, ULA
  (`fc00::/7`), or on one of the Mac's own networks (a home's IPv6). Then TLS 1.2 or later, with the Mac's own
  certificate.
- Its certificate names `<the Mac's name>.local`, `localhost`, `127.0.0.1` and every private IPv4 address the Mac has
  had, newest first, up to 16. It is made again only when a new address or name appears, or when it has a month
  left, because each new one brings the browser warning back on a device without eve's CA.

## Chat and voice

| Where | Chat | Voice |
|---|---|---|
| The Mac's own browser | `fetch` over loopback, which nothing on the Mac can block | WebRTC to the same Mac (not verified yet: spike S7) |
| A phone or laptop | the `link` DataChannel over WebRTC; HTTPS to the same door while the link is down | WebRTC audio, plus a `call` DataChannel |

- **No STUN or TURN server.** Both ends are on the same network, and nothing about the link leaves it.
- **The fallback.** A phone waits up to 4 seconds for the direct link, then chats over HTTPS and keeps trying WebRTC,
  5 to 60 seconds apart; new requests go back to the link once it opens. The top bar says "Direct link blocked, using
  HTTPS" meanwhile, and **Settings → Help** says which way chat goes. Photos and files always go over HTTPS.
- **The direct links.** One per device, the newest offer wins, at most 4 direct links at once (the oldest closes for a
  new one).
- **Voice** is push to talk: open it with the waveform button, hold the mic button (or Space or Return), talk, and let
  go. eve answers aloud, with captions. A press while eve speaks interrupts it. Voice needs the direct link: when it
  is blocked, the voice view says how to fix it (Local Network). One call at a time across every device: a new call
  ends the one still open.
- **How it works:** [the chat in a browser](app-chat.md#eve-lite-the-same-chat-in-a-browser),
  [push-to-talk in a browser](voice-mode.md#eve-lite-push-to-talk-in-a-browser).

## Apps

- After the Composio sign-in, **Settings → Apps** lists Gmail, Google Calendar, Google Drive, Slack, Notion, Linear and
  GitHub, each with **Connect**. Connect opens Composio's Connect Link, then the provider's own consent page.
  Back on eve's page, a connected app shows **✓ Connected** in place of the button. The list is read from Composio's
  search answer, the one part of it that is a JSON object (Composio adds notes as parts of their own since 2026-10-14;
  before 0.1.3 those notes made every app read as not connected).
- Or ask: "what's on my calendar today?" When the app is not connected, eve sends its sign-in link in the chat. Any
  other app that Composio signs you in to with its own sign-in (managed auth) connects the same way, by asking.
- Reads run. Gmail drafts and private calendar edits run. Sending, deleting and other one-way acts ask first, showing
  the whole act (a whole email for a send). A change to a connection asks too, unless it only checks or links.
- Details: [Composio Connect in eve lite](apps.md#eve-lite-composio-connect).

## What eve lite has

- **Chat** with eve's brain: `gpt-6-luna`; the composer's Think toggle runs a turn on `gpt-6.1-sol`; `think_hard` on
  `gpt-6-astra`; small talk on Haiku. Replies draw OpenUI cards and tables, with eve's reactions and memory sources,
  as in the iPhone app.
- **Photos, PDFs, text and Office files** in the chat, up to 4 a message and 20 MB each, kept 24 hours. A HEIC or AVIF
  photo is turned into a JPEG in the browser when the browser can read it.
- **Push-to-talk voice:** speech to text with `gpt-4o-transcribe` and eve's voice with `deepgram/aura-2`, both through
  OpenRouter.
- **Memory:** every line kept in transcripts, records (with no git history), the chat log, the mem0 index on
  OpenRouter's embedder, the nightly dream, "remember that" and "forget that".
- **Web search** (Perplexity through OpenRouter, Exa when it fails) and **science search**.
- **Watches** on prices, pages and searches; alerts arrive in the chat.
- **Apps** through Composio Connect.
- **Several phones and laptops** plus the Mac's browser, each one revocable.

## What eve lite leaves out

- The Eve Link iPhone app, its Action Button, Live Activities and widget, calls from a locked phone, and notes on the go.
- Telegram, the Mini App and its tunnel, ntfy notifications and the rendezvous.
- Tasks on the Mac: Codex computer use, the browser, Chrome and desk lanes, screenshots, the Mac's files, the camera,
  room notes.
- Claude Code: its hooks, the relay, coding sessions and the Work tab.
- The desk voice and wake word, the robot, the Voice PE, lights, Spotify, the Fire TV, Pika and the OBSBOT camera.
- The chief of staff, Updates, the app builder, phone location, file sharing, the second-brain vault, AgentCard,
  America, OpenTable, Canvas, the rundown and the learning server.
- Direct Google, GitHub and Maps (no Google keys, no Google OAuth client), TinyFish, Firecrawl, Monid and the hosted
  page reader.
- The toolset, grade and search routers, and the `/` menu's tools.
- Updating itself through git: run the install line again.
- Use away from the home network (there is no tunnel), Intel Macs, and macOS 13 or older.

Asked for one of these, eve says in a sentence that eve lite does not do that, and what it can do instead. The code
words of these features (`claude on`, `codex`, `/meet` and the rest) get one fixed line, and nothing runs.

## Devices and signing out

- **On the Mac:** **Settings → Devices** lists each paired browser (its kind and browser, its address, when it was last
  seen), with **Sign out** for each and **Sign out every phone and laptop**. A device signed out loses its live links
  at once.
- **In any browser:** **Settings → Sign this browser out** on the Mac, **Unpair this device** on a phone or laptop.
- **In Terminal:** `eve-bridge web devices`, `eve-bridge web revoke <id>`, `eve-bridge web revoke all`.

`web-devices.json` (mode 600) keeps only each device token's SHA-256, never the token. At most 64 devices are kept,
and a device unseen for 400 days is forgotten.

## The macOS prompts

| Prompt | When | Why |
|---|---|---|
| Background Items Added (python3.13) | the install | the login service is a new login item; nothing to click |
| Allow "python3.13" to find devices on local networks? | the first **Add a phone** | the direct link and voice send UDP on the network, which a login service needs the Local Network privilege for; eve raises the prompt on purpose at that moment. Chat over HTTPS never needs it. |
| Accept incoming network connections? (python3.13) | the first **Add a phone**, when the macOS firewall is on | the LAN door takes connections from phones |
| The browser's certificate warning | each phone's or laptop's first visit | eve's own certificate; **Trust eve on this device** ends it |
| Chrome's own Local Network prompt | once, in Chrome on another Mac | only Safari and web views are exempt (TN3179) |

The prompts say python3.13, not eve, because the login service runs uv's Python. Denied by mistake: **System Settings
→ Privacy & Security → Local Network**, and turn on python3.13.

eve lite is built never to raise the "install developer tools" dialog, and never to ask for the Microphone, Camera,
Bluetooth, Accessibility, Screen Recording, Contacts or Calendars privileges on the Mac: voice uses the browser's own
microphone permission ([the process firewall](#the-process-firewall)). Manual gate M1 checks this on a fresh Mac; it
is still pending.

## The process firewall

eve lite's daemon runs inside a guard (`profile.py`): a Python audit hook it installs before it builds anything, which
cannot be removed for the life of the process.

- **Programs.** It starts exactly three: `/usr/bin/pmset` (the battery answer), `/sbin/ifconfig` (the watcher's guard
  against pages on the Mac's own network) and `/usr/sbin/scutil` (the Mac's name, for the certificate). Every other
  start is refused: the developer-tool stand-ins first (git, clang, make, python3, install_name_tool and the rest),
  then `osascript`, `screencapture`, `codex`, `claude`, `cloudflared`, `adb` and the shell itself.
- **Folders.** It refuses to open, list, create, move or delete anything in the folders macOS guards with a privacy
  prompt: Desktop, Documents, Downloads, Pictures, Movies, Music, iCloud Drive, Mail, Messages, Calendars, Contacts,
  Safari, other apps' containers, cloud drives, Reminders, HomeKit, cookies, call history, iPhone backups, the privacy
  database and the Trash, and external disks (`/Volumes`).
- **What it says.** It logs each refused program or folder once, by name, never a path or a value; `eve-bridge doctor`
  shows the counts.
- **Its limit.** It is Python's guard: native code (PortAudio, ONNX, ctypes) can go around it.

`records.py` also asks `toolchain.git_usable()` before every git it runs: never in eve lite, whose memory keeps no git
history (a bad dream cannot be undone with git).

## Files

| Path | What it holds |
|---|---|
| `~/.eve-lite/` (mode 700) | `bin/uv`, `python/` (uv's CPython), `venv/`, `release/`, `cache/`, `install.log`, and `ipc.sock`, the private socket `eve-bridge` reaches the daemon on |
| `~/.config/eve-bridge/env` (mode 600) | `EVE_PROFILE=lite` and the OpenRouter key |
| `~/.config/eve-bridge/composio-connect.json` (mode 600) | the Composio Connect sign-in: its client id and tokens |
| `~/.config/eve-bridge/web.json` (mode 600) | the doors' ports, phone access on or off, and whether setup skipped Composio |
| `~/.config/eve-bridge/web-devices.json` (mode 600) | each paired device: its token's SHA-256, label, address, when paired and last seen |
| `~/.config/eve-bridge/web-tls/` (mode 700, files 600) | the CA and the LAN door's certificate, with their keys |
| `~/.config/eve-bridge/memory/`, `chat/log.json`, `watches.json`, `spend/` | eve's memory, the chat log, the watches and the spend ledger, as in full eve |
| `<the temporary folder>/eve-link-uploads-<user id>/` | photos and files sent in the chat, for 24 hours |
| `~/Library/LaunchAgents/com.github.eve-bridge.lite.plist` | the login service |
| `~/Library/Logs/eve-lite.log` | the daemon's log: states, counts, names and types, never what was said |
| `~/Applications/Eve.app` | the shortcut that opens eve |

## What leaves the Mac

- **openrouter.ai:** every model call (chat, thinking, memory and the dream, watch checks), speech to text and eve's
  voice, web search, the OAuth sign-in, and the key's credit.
- **connect.composio.dev and login.composio.dev:** the Composio sign-in and every app call. Composio then reaches the
  apps you connected.
- **api.sciencemadereadable.com** (science search) and **open-meteo.com** (weather answers).
- **The pages you ask eve to watch.**
- **At install only:** github.com (the release, uv and uv's Python) and PyPI's file host (the wheels).

Nothing goes to Telegram, ntfy, Cloudflare, Google's APIs directly, OpenAI directly or TypeSafe. The link between a
phone and the Mac stays on your network; what you say and type still goes on to OpenRouter, as above. mem0's
telemetry is switched off.

## Commands

Each is `~/.eve-lite/venv/bin/eve-bridge <command>`. It reads `EVE_PROFILE=lite` from the env file, so it is eve
lite's own command line, with these commands and no others.

| Command | What it does |
|---|---|
| `web open [--wait SECONDS]` | Opens eve in this Mac's browser, signed in (Eve.app runs this). Waits up to 20 seconds for eve by default. |
| `web devices` | The browsers that can open eve. |
| `web revoke <id>`, `web revoke all` | Signs browsers out; their live links close. |
| `web lan on`, `web lan off` | Phone access on or off, remembered across restarts. |
| `status` | Yes or no for each part: the login service, eve answering, the two sign-ins, chat, voice, phone access. |
| `doctor` | Checks eve lite part by part and says what to fix; exits 1 while anything is to fix. |
| `memory forget "<words>"` | Counts what matches in every memory store, then forgets it on a yes. |
| `install --service`, `uninstall --service` | Installs or removes the login service. |
| `lite uninstall [--dry-run]` | What the install line's `--uninstall` does ([below](#update-and-uninstall)). |
| `update` | Says how eve lite updates: by running the install line again. |
| `daemon` | The daemon itself; the login service runs it. |

## Update and uninstall

- **Update:** run the install line again. It keeps your settings and memory.
- **Uninstall:** `curl -fsSL https://github.com/gurul/eve_01/releases/latest/download/install-lite.sh | bash -s -- --uninstall`,
  or `~/.eve-lite/venv/bin/eve-bridge lite uninstall`. Either stops eve lite and removes its login service,
  `~/.eve-lite`, its log and the Eve.app it made (an Eve.app it did not make stays). It asks before deleting
  `~/.config/eve-bridge`, which holds eve's memory, chats and sign-ins, and keeps it when there is no terminal to ask.
  Kept, the folder loses its `EVE_PROFILE=lite` line, so a full eve installed later never starts as eve lite, and gets a
  `.eve-lite-kept` marker holding the env file's SHA-256: the install line takes the folder back while that file is
  unchanged.
- **Moving to full eve:** uninstall eve lite first. Full eve's `install.sh` does not look for eve lite.

## Troubleshooting

Run `~/.eve-lite/venv/bin/eve-bridge doctor` first: it marks each part ok, `--` (nothing to do) or `!!` (to fix).

- **"OpenRouter has no credit on this key. Add credits at openrouter.ai, then send that again."** The key has no
  credit. **Settings → OpenRouter → Add credits**, then **Check again**. A key made by the sign-in has no spend limit by
  default, and eve lite sets none (decision 11); set one on openrouter.ai if you want one.
- **"Direct link blocked, using HTTPS."** macOS's Local Network privilege is off for python3.13. Chat still works; voice
  does not. **System Settings → Privacy & Security → Local Network**, turn on python3.13. Once a phone has paired or
  made a link, `eve-bridge doctor` says `Local Network: blocked` or `allowed`.
- **A phone cannot reach eve at all.** Guest, office and some mesh networks keep devices apart (client isolation). The
  phone and the Mac must be on the same network, and the Mac awake.
- **eve stops answering when the Mac sleeps.** It cannot answer while the Mac sleeps. Keep the Mac from sleeping while
  you use eve from a phone (System Settings, under Battery or Energy); eve lite does not keep it awake itself
  (decision 9).
- **The Mac's address changed.** An iPhone's `.local` link keeps working. An Android phone needs a new code from
  **Add a phone**. The new address also gets a new certificate, so a device without eve's CA sees the warning once more.
- **"This browser paired but didn't keep eve's sign-in."** The browser did not keep the cookie, which it does for a
  certificate it does not trust: install eve's CA (**Trust eve on this device**), then scan the code again.
- **"This browser isn't signed in to eve."** on the Mac: open Eve from Applications.
- **The install said eve did not answer within 90 seconds.** Open Eve from Applications in a minute. The log is
  `~/Library/Logs/eve-lite.log`.
- **Full eve is installed for this user.** eve lite stays down (its daemon stops at start, and launchd leaves it
  stopped), and `doctor` says so: one macOS user has one edition.
- **A key copied into the env file does nothing.** At start eve lite removes every variable shaped like a secret but
  `OPENROUTER_API_KEY`, so a key from full eve can never switch a feature on.

## How it is built

| Part | What it is |
|---|---|
| `bridge/src/eve_bridge/profile.py` | `EVE_PROFILE`: the switches eve lite forces (`LITE_FORCED`), the defaults it fills, the secrets it removes, every switch its modules read with the reason (`LITE_CLASSIFIED`), its tool set, and the process firewall |
| `bridge/src/eve_bridge/toolchain.py` | Finds Xcode's tools from their files, starting nothing; `git_usable()` |
| `bridge/src/eve_bridge/lite.py` | `LiteDaemon`: what eve lite runs, its IPC, and the restart into a new sign-in (exit 75, launchd starts it again) |
| `bridge/src/eve_bridge/lite_web.py` | The two web doors, `LocalServer` (the Mac's page) and `LanServer` (phones and laptops), their routes and checks |
| `bridge/src/eve_bridge/lite_rtc.py` | WebRTC: the `link` channel around the unchanged `LinkChat`, the `call` channel and audio into the unchanged `PhoneCalls` |
| `bridge/src/eve_bridge/lite_tls.py` | The eve-local CA and the LAN door's certificate |
| `bridge/src/eve_bridge/lan_net.py` | The Mac's LAN addresses, the peer filter, its Bonjour name, the Local Network prompt and probe |
| `bridge/src/eve_bridge/web_auth.py` | Device cookies and their hashes, the pairing window, the Mac's one-time sign-in code |
| `bridge/src/eve_bridge/onboarding.py` | The two OAuth sign-ins |
| `bridge/src/eve_bridge/composio_connect.py` | `ConnectBridge`: Composio Connect's meta tools over MCP |
| `bridge/src/eve_bridge/cli.py` | Applies the profile at start; eve lite's own command line |
| `bridge/src/eve_bridge/_service_launchd.py` | The lite login service |
| `telegram.py`, `composio_tools.py`, `tool_gate.py`, `self_context.py` | The brain's lite branches: its prompts, its tool set and refusals, the Composio gate's lite verdicts, and how it describes itself |
| `install-lite.sh` | The installer |
| `tools/release_lite.sh` | Builds a release and drafts it on GitHub |
| `ios/EveLink/Web/src/web/` | The web app ([its source map](../ios/EveLink/Web/README.md#eve-lites-web-app-srcweb)) |

**The daemon** builds memory (the same steps as full eve's), the chat log, the watcher, the Composio bridge once
Composio is signed in, the brain, one `LinkChat`, `PhoneCalls`, the WebRTC hub, the Mac's page, the LAN door when it was
on, the providers' spend figures and the nightly dream. With no OpenRouter key yet it serves the setup page alone.
After a sign-in it stops gracefully and exits 75, and launchd starts it again about 5 seconds later; a plain stop
exits 0 and stays stopped. The login service pins `EVE_PROFILE=lite` in its own environment, so the env file cannot change the
edition under it.

**The brain** is full eve's chat brain, app-only, with one local chat. It offers exactly these tools: `remember`,
`think_hard`, `recent_messages`, `web_search`, `science_search`, `memory_search`, `memory_read`, `forget_preview`,
`forget_apply`, `recent_conversation`, the six watch tools, and Composio Connect's meta tools. It refuses any other
tool name before anything runs. Its instructions are its own and name only tools a turn offers; the apps block names
no app, only kinds of app. An OpenRouter 402 becomes the plain no-credit line above.

### Why full eve does not change

- Every lite branch asks `profile.is_lite()`. With `EVE_PROFILE` unset, `profile.apply()` changes nothing and the
  firewall is never installed.
- The files that make up full eve's own composition are not edited: `daemon.py`, `miniapp.py`, `link_chat.py`,
  `install.sh` and `ios/EveLink/App/`.
- `bridge/tools/owner_golden.py --check` computes what full eve's brain is told and offered, how its Composio gate
  decides, its launchd plists and what `eve-bridge daemon` builds, and compares them with a golden recorded from main
  (`bridge/tests/test_owner_golden.py`, with a positive control).
- `cli.py` imports full eve's daemon only when `eve-bridge daemon` runs it, so eve lite's command line never loads
  Bluetooth or the rest of full eve's Mac.
- AGENTS rule 11 keeps it so.

## Making a release

The owner runs this on the commit that is to ship, with uv, pnpm and gh installed:

```bash
sh tools/release_lite.sh --dry-run   # checks everything and prints the plan; writes nothing; ends with DRY_RUN_OK
sh tools/release_lite.sh             # builds lite-v<version>, then asks before drafting the GitHub release
```

On a Mac where uv has never fetched a Python, the dry run stops at "uv export failed: uv.lock has no lite extra, or it
is out of date": the export runs `--offline` and finds no interpreter. Run `uv python install` once, then again.

It builds from the commit alone (it refuses while anything it builds from is not committed), in a scratch copy:
1. the web page (`pnpm build:web`);
2. the requirements from `uv.lock`: the core dependencies and the `lite` extra, hash-pinned, without the pyautogui
   family, which has no wheels and which eve lite never imports;
3. the wheel, which must hold the web page;
4. a smoke install of the tarball with `--no-start` into a scratch home, a boot check under the process firewall, and
   a check that this eve-bridge takes every command the installer and Eve.app run;
5. the tarball, `SHA256SUMS`, and `install-lite.sh` stamped with the release's URL and SHA-256 (the release's copy, and
   the repository's, to commit);
6. after a yes, `gh release create lite-v<version> --draft`.

Publishing the draft on GitHub is the owner's own step. The install line asks for the repository's newest published
release (`releases/latest`), so that must be the eve lite release.

## Tests and gates

The gates are in [lite-gates.md](lite-gates.md), each with the command that decides it: the owner golden, the profile
and firewall, the lite brain, TLS and device auth, the sign-ins, Composio Connect, WebRTC, the web doors, the daemon,
the command line and login service, the web client, the installer, the Lean models, the blind set, these docs, an end
to end run in Chromium, no regression in the full suite, lint, and the owner's live service left alone. The Lean
models for eve lite are in [verification.md](verification.md#eve-lite-2026-10-13).

## Verified on

Nothing below has been run yet, except spike S1 and part of M2. Each row is filled in, with its date and evidence, when it runs.

| Check | What it needs | Status |
|---|---|---|
| M1 | A fresh macOS VM with no developer tools and no Homebrew: the install line raises no "install developer tools" dialog and no Bluetooth, Microphone, Accessibility or Screen Recording prompt; record the Background Items text, the install time and the disk used | pending |
| M2 | iPhone Safari: scan, click through the certificate, pair, chat, a hold-to-talk round trip; then the CA install ends the warning | partial, 2026-10-14: an iPhone on the owner's Wi-Fi scanned the iPhone code, clicked through the warning and paired (`web devices`: "iPhone · Safari", `gurucharans-macbook-pro-2.local`, about 35 s after Add a phone); the owner: "paired, chat works". Hold-to-talk and the CA install still pending |
| M3 | Android Chrome: the same | pending |
| M4 | A real OpenRouter sign-in through the setup page, with an account that has no credit and with a funded one | pending |
| M5 | A real Composio Connect sign-in; connect Gmail; a read runs and a send asks | pending |
| M6 | Local Network denied: phone chat still works over HTTPS, and `eve-bridge doctor` reports it blocked | pending |
| S0 | The owner's own python3.13 login service, listed and on under Local Network | pending |
| S1 | Composio Connect takes eve's registration with loopback callbacks | done 2026-10-13: discovery, registration, authorize and token-endpoint checks against Composio's live services; the decision was Composio Connect, not the contingency ([S1](plans/eve-lite-s1.md)). A real sign-in is M5. |
| S2 | On iPhone and Android, after the certificate click-through: a secure context, the microphone, and a Secure cookie that is kept | pending |
| S3 | The Local Network prompt for a login service on a fresh macOS user, on Allow and on Deny | pending |
| S4 | aiortc with Safari and Chrome on a real network | pending; two aiortc peers in one process are tested (`bridge/tests/test_lite_rtc_loopback.py`) |
| S5 | iOS audio during push-to-talk: loud enough at arm's length, on the loudspeaker | pending; the page records per press meanwhile, the safer choice |
| S6 | The installer's uv steps on a fresh VM with the narrowed `PATH`: no developer-tools dialog | pending |
| S7 | Voice from the Mac's own browser to the same Mac | pending |

## The owner's open questions

These decisions were taken without the owner, so the work could go on (the spec's open questions, as
[lite-gates.md](lite-gates.md) records them). Each is the decision word for word, then the question it answers.

1. **Composio: Connect (For You) MCP OAuth is the main path (WP-6). WP-6b (CLI-session login) is built only if the
   dynamic-registration probe fails.** Is it all right to rely on Composio's For You product for other people's
   installs? Its terms and pricing for other people's agents are not verified; For You is documented for "your own
   agent". The probe passed (S1), so WP-6b is not built; [apps.md](apps.md#if-composio-connect-fails-the-contingency)
   says what it would be.
2. **The Mac's own browser chats by loopback fetch; phones and laptops use the WebRTC DataChannel.** Is chat without
   WebRTC acceptable on the Mac itself?
3. **HTTPS chat fallback: on.** Should eve fall back to HTTPS when WebRTC cannot connect (as built), or be WebRTC only,
   as the request was worded?
4. **Two labelled QR codes (`.local` for Apple, IP for Android).** Two codes, or one?
5. **Releases: `tools/release_lite.sh` is built and dry-run only. Nothing is published or pushed.** Publish under
   gurul/eve_01 (public) as `lite-v<version>`, with the owner running the release script?
6. **The Composio remove-connection ask and the 402 line are lite-only.** Should full eve also ask before a Composio
   connection changes, and answer an OpenRouter 402 with the plain no-credit line?
7. **`install.sh` is not touched (its `~/.claude` abort is a separate fix).** Make that fix a separate change? On a Mac
   without `~/.claude`, full eve's installer stops before the login service ([SETUP-ISSUES](SETUP-ISSUES.md#full-eve-installsh-stops-on-a-mac-without-claude)).
8. **The LAN door stays on across networks once enabled; documented.** Keep it so, or turn it off when the Mac joins
   another network?
9. **Mac sleep assertion: v2.** Keep the Mac awake while a phone is connected?
10. **mem0 stays in lite.** Keep the mem0 meaning index? It adds `qdrant-client` and `grpcio` to the install.
11. **No spend cap in v1; documented.** Add a daily cap on eve lite's spending? Keys minted by the sign-in have no
    limit by default.
12. **The deny-by-default switch registry is accepted.** Is that upkeep acceptable: every new switch a module of eve
    lite reads must be classified in `profile.py`, or the tests fail?
13. **Developer ID `.pkg`: v2.** Get a paid Apple developer account for v2? It would give a signed, notarized
    installer and app: a true double-click install, eve's own name in the Local Network and Background Items prompts,
    and a privacy identity that survives updates.
