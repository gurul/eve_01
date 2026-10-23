# Adding an app

eve's apps are variable. The ones in this repository (lights, music, a TV remote, a
camera, a car) are the owner's picks; yours can be different. This page walks through
building one, from the scaffold to turning it on by default.

## What an app is

An app is **a module plus one line in the registry**
(`bridge/src/eve_bridge/app_registry.py`). The line says what the module offers, and
every wiring site reads it, so the app reaches:

- **the chat** (Eve Link, Telegram): its tools and its instruction block
  (`bridge/src/eve_bridge/telegram.py`);
- **the voice**: the same tools and block, minus any you leave out loud
  (`bridge/src/eve_bridge/voice_agent.py`);
- **a code path** (optional): a plain command like "lights off" done by code, with no
  model turn;
- **the tool gate**: which of its tools act, which are reads that bring in no outside
  text (`bridge/src/eve_bridge/tool_gate.py`);
- **the phone**: a row under Apps and a sheet, through `GET /api/link/controls` and
  `POST /api/link/controls/<slot>` (`bridge/src/eve_bridge/link_chat.py`);
- **the CLI** (optional): `eve-bridge <word> ...` (`bridge/src/eve_bridge/cli.py`);
- **the intent eval**: its label and tool prefix (`bridge/tools/intent_eval.py`).

## Quick start

Work in a worktree, not the main checkout (AGENTS.md rule 5), and run these from its
root. `uv run --project bridge` makes the worktree's own venv on first use; a fresh
worktree has no `bridge/.venv`.

```bash
git worktree add -b app-fan ../eve-fan origin/main && cd ../eve-fan
uv run --project bridge eve-bridge app new fan --dry-run   # what it would write
uv run --project bridge eve-bridge app new fan
```

That writes four files. They pass their own tests, and the whole suite passes with
them: no other test, and no other doc, needs an edit for a new app.

| File | What it is |
|---|---|
| the module, `fan.py` in `bridge/src/eve_bridge/` | an on/off switch kept in memory; `fan_status` (a read) and `fan_set` (an act); a phone sheet with a Power toggle and a button |
| its test, `test_fan.py` in `bridge/tests/` | off by default, its tools, set and status, the sheet against the phone's contract, the registry line |
| its doc, `fan.md` in `docs/` | a stub to fill in |
| one line in the registry | above the marker line at the end of `APPS`; `default_on=False` |

Then, in the worktree, run its tests, the whole suite and lint (the suite and lint
commands are in AGENTS.md, "Working on the code"):

```bash
uv run --project bridge --extra dev pytest -q -p no:cacheprovider -c bridge/pyproject.toml --rootdir bridge \
  bridge/tests/test_fan.py
```

Set two placeholders in its registry line: `symbol` (an SF Symbol name; the scaffold's
`square.grid.2x2` is a generic grid) and `detail` (the phone row's subtitle; empty shows
none). The comment the scaffold puts above the line says so.

The daemon runs from the main checkout, so turn the app on only after it is merged
there (AGENTS.md rule 5). Then, on the Mac that runs eve:

```bash
echo 'EVE_FAN=1' >> ~/.config/eve-bridge/env
launchctl kickstart -k gui/$(id -u)/com.github.eve-bridge.daemon
```

The phone's Apps tab now lists Fan, and "turn the fan on" in the chat reaches `fan_set`.
`eve-bridge app list` shows every app, its switch and whether it is on.

A name is 2 to 16 lowercase letters or digits, starting with a letter. The scaffold
refuses, and writes nothing for:

- a Python keyword, an existing app, `memory`, `notes`, `app`, or a name whose files
  already exist;
- a name whose tools would share a prefix with eve's own tools (`watch_`, `meet_`, ...)
  or with another app's (`camera_` is the camera's);
- a name that is already an intent-eval label or one of eve's own tools (`music`,
  `screenshot`);
- a name the wiring already uses for something else: the chat keeps a hub as
  `_<name>`, the voice and the Mini App server as `<name>`, and the daemon as
  `_<name>`, so a name such as `quick` or `link` would overwrite one of their own
  attributes.

## Fill in the module

The module's contract (`app_registry.check` tests it for every app):

| Part | Needed | What it does |
|---|---|---|
| `make()` | always | the hub, or None when the app is off or not set up; reads its own switch and files; makes no network call |
| `TOOLS`, `TOOL_NAMES`, `OFF_REASON` | a tools app | the tool schemas (a module-level list); their names, in the same order; the reason given when the hub is None |
| `INSTRUCTIONS` | a tools app | the block the model reads, as a module-level string. The intent eval lends `TOOLS` and `INSTRUCTIONS` with no hub. An app whose block lists the owner's devices (the lights) keeps the fixed part in `INSTRUCTIONS_HEAD` instead |
| `hub.tools()`, `hub.instructions()`, `async hub.handle(name, args)` | a tools app | what the model is offered and reads; one call |
| `async hub.panel(body)` | an app on the phone | the sheet's answer (below) |
| `async hub.close()` | always | the daemon is stopping: let go of the device |
| `match(text)` and `async hub.run(cmd)` | a code path | a plain command done by code; `match` returns None for anything else |
| `cli(argv)` | a CLI | `eve-bridge <word> ...` |

Put the device code where the scaffold's comment says, in the hub's `__init__` and
`_set`. Keep `make()` free of network calls: the daemon makes every hub at start, and
a slow device must not stall it.

## Change the tools

The scaffold assumes an on/off device: `<name>_status` and `<name>_set`. Most apps are
not one. When you change the tools, change all of these together, or the app's own test
or `app_registry.check` fails:

1. **The module.** `TOOLS` (every name starts with `<name>_`), `INSTRUCTIONS` (name only
   these tools), `hub.handle`, and `hub.panel` with its controls. An app with no on/off
   state (a coin flip, a lookup) drops `_set` and `self.on`; its state is whatever it
   needs.
2. **The registry line.** `acting` lists every tool that changes something; `clean`
   lists every read that brings in no outside text (see Safety). Every tool goes in
   one of the two, unless it reads outside text (a web page, a message, a device's
   free-text name), which taints the turn and goes in neither. Each name in either
   set must be in `TOOL_NAMES`.
3. **The generated test.** It tests `_status` and `_set`; replace those tests with ones
   for your tools, and change `test_it_is_registered` to the new `acting` and `clean`.
4. **The doc.** Its Tools table and its phone section.

Then run the app's test, the whole suite and lint again.

The owner's apps are worked examples of each part:
`bridge/src/eve_bridge/lights.py` (a code path that matches the lights' own names, a
listing for the phone), `bridge/src/eve_bridge/obsbot_panel.py` (a code path, a
video stream, a native sheet) and `bridge/src/eve_bridge/pika.py` (no model tools
at all, only a phone sheet: `tools=False`).

## The registry line

| Field | What it means |
|---|---|
| `slot` | the app's name everywhere: `inlet._<slot>`, the controls key, `POST /api/link/controls/<slot>` |
| `module`, `make` | `eve_bridge.<module>` and its factory |
| `switch`, `default_on` | its env var; on unless `0/false/no/off`, or (`default_on=False`) only when `1/true/yes/on` |
| `title`, `symbol`, `detail` | the phone's row: its title, its SF Symbol, its subtitle |
| `doc` | its doc |
| `no_control` | the phone's line when the app is not on this Mac |
| `tools` | it gives the models tools (False: a phone sheet only, like Pika) |
| `label`, `tool_prefix` | its intent-eval label; every tool name starts with the prefix |
| `acting`, `clean` | its tools that change something; its reads that bring in no outside text |
| `sources`, `say`, `own_words` | a read's name in the owner's question; an act's head out loud; actions that answer in eve's own words |
| `voice_omit`, `voice_kwargs`, `voice_note` | tools not offered out loud; `instructions(**voice_kwargs)` out loud; a line after the voice block (the TV: no screenshot out loud) |
| `code`, `code_taint_tool` | a code path; its line counts as outside words when that tool's call would (Spotify) |
| `controls`, `stream`, `listing`, `native_sheet` | a phone sheet; a picture at `GET /api/link/controls/<slot>/stream`; a list instead of a bool in the legacy key (lights); Swift for its sheet |
| `cli`, `cli_module`, `cli_help` | CLI words, the module whose `cli(argv)` runs, its help line |

## Order is model bytes

Three orders matter, and a new app goes last in all three:

- **Tool order** is the order of `APPS`: each app's tools and block reach the chat
  and the voice in this order. Moving a line changes what the models read.
- **Code-path order** is `CODE_ORDER`: the TV, the camera, the lights, the music. The
  TV comes first so that a light named "TV strip" cannot claim "turn on the tv".
- **Phone order** is `PHONE_ORDER`, after Notes.

An app not named in `CODE_ORDER` or `PHONE_ORDER` follows the named ones in `APPS`
order.

## Safety

- **Declare every tool that acts** in `acting` (AGENTS.md rule 8). The tool gate then
  asks the owner before it runs whenever the turn has read outside text, and says the
  act whole out loud. A read that brings in no outside text goes in `clean`: eve's own
  state (whether the fan is on), or a value the code makes (a random coin side, a
  sum). A read that returns text from outside (a web page, an email, a name someone
  else typed) stays out of `clean`, and it taints the turn after it.
- **Name only your own tools** in your instructions, and never a wrong name, not even as
  "never say this" (rule 7). The scaffold's test checks the first half.
- **Log counts, names and types**, never what was said or a device's secrets (rule 9).
- **Turning an app on by default changes what the models read** (rule 6). Before
  `default_on=True`: write down the pass bar, get a blind case set written by someone
  who did not see the change, score it once with `bridge/tools/intent_eval.py`, and
  ship only if the bar holds.

## Intent eval cases and a blind set

The camera is the worked example. Its development set is
`bridge/tests/fixtures/intent_camera_dev.json`, and its blind set,
`bridge/tests/fixtures/intent_camera_blind.json`, was written from a brief without the
code: 30 messages, each with the label it should reach. Requests for the camera are
`camera_aim`, requests for a picture are `camera_photo`, and hard negatives ("zoom out
for a sec, what's the big picture on interest rates") carry `never: [camera_aim]`, so a
camera move on one of them fails. The bar was set first (`camera_aim` 90%, `camera_photo`
75%) and checked with:

```bash
env $(grep -E '^(OPENROUTER_API_KEY)=' ~/.config/eve-bridge/env | xargs) \
  uv run --project bridge --extra dev --extra miniapp --extra tls --extra composio \
  python bridge/tools/intent_eval.py bridge/tests/fixtures/intent_camera_blind.json \
  --require camera_aim=90 --require camera_photo=75
```

Run it from the repository root (a worktree's, too). It needs the OpenRouter key: the
eval uses production routing.

Do the same for your app with its own label (the registry's `label`). A spent set is
development data from then on.

## The phone

An app without Swift gets **eve's generic sheet**. The phone sends
`{"action": "status"}` when the sheet opens, and draws the answer:

```json
{"ok": true, "ui": 1,
 "line": "The fan is on.",
 "status": "On · speed 3",
 "controls": [
   {"type": "toggle", "id": "power", "label": "Power", "on": true, "action": "power"},
   {"type": "slider", "id": "speed", "label": "Speed", "value": 3, "min": 1, "max": 5, "step": 1, "action": "speed"},
   {"type": "button", "id": "boost", "label": "Boost", "symbol": "bolt", "action": "boost"},
   {"type": "button", "id": "off",   "label": "Turn off", "action": "off", "role": "destructive"}
 ]}
```

- A button sends `{"action": a}`, a toggle `{"action": a, "on": bool}`, a slider
  `{"action": a, "value": number}` when the finger lifts.
- Each answer redraws the sheet; an answer without `controls` keeps the last ones.
  `status` is the headline, `line` the muted line under it. `{"ok": false, "line": ...}`
  shows the line.
- Unknown types are skipped, at most 24 controls are drawn, and a slider needs
  min < max. `ui` is 1.

Build it with `app_registry.panel`, `toggle`, `slider` and `button`, and test it with
`app_registry.check_panel` (the example above is `bridge/tests/fixtures/app_panel_example.json`).

**A native sheet** is for what the generic one cannot do: a control held down (drive,
turn while pressed) or live video. The camera has one: its picture, hold-to-turn
arrows, zoom and tracking. Its Swift is a case in `ControlKind`
(`ios/EveLink/Shared/ControlsClient.swift`), a branch in `ControlSheet`
(`ios/EveLink/App/ControlSheets.swift`) opening `ios/EveLink/App/ObsbotView.swift`,
and its row in `ios/EveLink/App/WorkView.swift`. Then set `native_sheet=True`; until
the phone has the Swift, the generic sheet is used.

## A device with no public API, or with firmware

The reverse-engineering tools below are for devices, apps and games you own and may
inspect, nothing else.

- **REA** ([github.com/morluto/rea](https://github.com/morluto/rea)), "Reverse
  Engineer Anything": an MCP for agents that inspects app binaries, Electron/JS apps and
  websites without source, explains how a feature works and helps rebuild it. Setup:
  `npx rea-agents setup`. Use it to learn how the device's own app talks to the device
  when there is no public API, and **only on devices and software you own and may
  inspect**.
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
  server. Use it when your app has firmware on a board.

## Switching an app off

Its switch at `0` (or unset, for an app that is off by default) removes it everywhere:
no tools or block in the chat or the voice, no code path, and no row on the phone (the
controls listing marks it absent). `bridge/tests/test_app_registry.py` checks this for
every app.

The intent eval is the one exception, on purpose: it lends every registered tools app
whatever its switch says, so an eval run's request is the same in every shell and
scores stay comparable. That is also how a new app is scored before its switch is on.

## Checklist

- [ ] Its tests pass, and the whole suite with them (AGENTS.md, "Working on the code").
      No other test needs an edit; if one fails, the app collides with something.
- [ ] Its registry line has a real `symbol` and `detail`, and `acting` and `clean` match its tools.
- [ ] `cd bridge && uv run --extra dev ruff check src tests tools` is clean.
- [ ] Its doc says what it does, its switch, its tools and which act, and how to test it;
      README.md and AGENTS.md's apps table name it if it ships in this repository.
- [ ] Every path the docs name in backticks exists.
- [ ] If it will be on by default: a bar written first and a blind intent-eval set scored once.
