# Fan

An app for eve, written by `eve-bridge app new fan`. Replace this page's placeholders as the app grows; the
walk-through is [adding an app](adding-an-app.md).

## What it does

Today: an on/off switch kept in memory, so the module, its tests and the phone sheet work with no device. Describe
the real device or service here once the module talks to it.

## Switch

`EVE_FAN=1` in `~/.config/eve-bridge/env` turns it on; anything else leaves it off. Restart the daemon after
changing it (the daemon runs from the main checkout, so merge the app there first). Off, the app is gone
everywhere: no tools in the chat or the voice, no row on the phone, no code path. The intent eval lends every
registered app whatever its switch, so an eval run does not depend on the shell.

## Tools

| Tool | Acts? | What it does |
|---|---|---|
| `fan_status` | no (in `clean`: a read that brings in no outside text) | whether it is on |
| `fan_set` | yes (declared in `acting`, so the tool gate treats it as an act) | turns it on or off |

## The phone

Eve Link lists it under Apps once it is on, and opens eve's generic sheet: the status, a Power toggle and a
Say hello button, from the module's `panel(body)`. The sheet's JSON is described in
[adding an app](adding-an-app.md#the-phone).

## Test it

```bash
uv run --project bridge --extra dev pytest -q -p no:cacheprovider -c bridge/pyproject.toml --rootdir bridge \
  bridge/tests/test_fan.py
```

## Before it is on by default

Turning an app on changes what the models read. Before `default_on=True` in its registry line, pre-register a bar
and score a blind intent-eval set written by someone who did not see the change (AGENTS.md rule 6).

## Resources

The reverse-engineering tools below are for devices, apps and games you own and may inspect, nothing else.

- [REA](https://github.com/morluto/rea) ("Reverse Engineer Anything"): an MCP for agents that inspects app
  binaries, Electron/JS apps and websites without source, and explains how a feature works. Use it to learn how a
  device's own app talks to the device when there is no public API, and only for devices and software you own and
  may inspect. Setup: `npx rea-agents setup`.
- [Ghidra](https://github.com/NationalSecurityAgency/ghidra): the NSA's free decompiler and disassembler for native
  binaries; most agent RE setups sit on it.
- [GhidraMCP](https://github.com/LaurieWired/GhidraMCP): an MCP server that lets an agent drive Ghidra, headless or
  in the GUI.
- [ImHex](https://github.com/WerWolv/ImHex): a hex editor with pattern language and an MCP server, for reading file
  formats and firmware images.
- [mitmproxy](https://github.com/mitmproxy/mitmproxy): an intercepting HTTP/TLS proxy: watch what a phone or desktop
  app sends to map an API it never documented.
- [Frida](https://github.com/frida/frida): runtime hooking for desktop and mobile apps, scriptable from the CLI:
  trace the calls an app makes while it runs.
- [ILSpy](https://github.com/icsharpcode/ILSpy): a .NET decompiler to C#, for Windows companion apps.
- [Android RE skill](https://github.com/SimoneAvogadro/android-reverse-engineering-skill): a Claude Code skill that
  turns an APK into decompiled code and the API endpoints it calls.
- [Cpp2IL](https://github.com/SamboyCoding/Cpp2IL): turns a Unity IL2CPP build back into .NET assemblies that ILSpy
  can read; work in progress.
- [Universal Modder](https://github.com/rehan-remade/universal-modder): agent skills for recon, reverse engineering
  and modding PC games you own.
- [hwlog](https://github.com/gurul/hardware-logging): crash-aware serial logging for embedded boards, built for
  coding agents (persistent sessions, decoded backtraces, flash-safe port arbitration, an MCP server). Use it when
  the app has firmware on a board.
