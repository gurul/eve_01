# Gates: Codex desktop Computer Use integration

Scope: Establish whether Eve can invoke Codex's existing desktop Computer Use through a supported interface before implementing any delegation.

- [x] G1: Eve architecture and existing Codex transport are traced to source.
  EVIDENCE: Reviewed telegram.py, voice_agent.py, daemon.py, retained desktop worker, and existing codex_relay.py; architecture and private follower boundary recorded in README.md.

- [x] G8: Telegram photos and image documents preserve owner checks, captions and active relay routing; bounded downloads reject invalid media and images cannot answer permission prompts.
  CHECK: bridge/.venv/bin/python -m pytest -q -p no:cacheprovider bridge/tests/test_telegram_images.py
  EXPECT: passed
  EVIDENCE: exit=0; shell=/bin/sh; cwd=/Users/gurucharan/Documents/personal/eve; path=5cdd84c66b04/21 entries; output=..............                                                           [100%] | 14 passed in 0.10s

- [x] G2: Current official documentation and installed plugin metadata establish the supported integration boundary.
  EVIDENCE: Official app-server, SDK, Computer Use and Remote Control docs compared with installed unified-computer-use 26.917.51856 metadata and cua-repl README; exact sources and version limits recorded in README.md.

- [x] G3: A supported candidate transport is probed for actual Computer Use tool availability without copying credentials or bypassing permissions.
  EVIDENCE: Actual public app-server inventory on bundled 0.155.0-alpha.16 and Homebrew 0.144.1 exposed cua_repl.js without copied desktop environment. Native execution on bundled runtime produced Calculator permission requests and UI state.

- [x] G4: If available, Eve delegation relays progress, approvals, results and cancellation and a harmless native-app task is verified end to end; otherwise a concrete limitation and smallest supported alternative are documented without implementing a substitute.
  EVIDENCE: native-smoke.json records actual TelegramInlet tool -> Daemon factory -> app-server -> cua_repl.js with 10 scoped permission requests, progress/final relay and Calculator accessibility expression 2 + 2 / result 4. Telegram delivery used an in-memory sink; unit checks cover interruption and disconnect.

- [x] G5: Adapter tests cover discovery, event relay, approval decisions, unavailable tools, disconnect and cancellation; daemon wiring uses Codex for both voice and text.
  CHECK: bridge/.venv/bin/python -m pytest -q -p no:cacheprovider bridge/tests/test_codex_computer.py
  EXPECT: passed
  EVIDENCE: exit=0; shell=/bin/sh; cwd=/Users/gurucharan/Documents/personal/eve; path=5cdd84c66b04/21 entries; output=......                                                                   [100%] | 6 passed in 0.09s

- [x] G6: Changed Python files pass lint and the existing voice, Telegram and daemon tests pass.
  CHECK: bridge/.venv/bin/ruff check bridge/src/eve_bridge/codex_computer.py bridge/src/eve_bridge/telegram.py bridge/src/eve_bridge/telegram_images.py bridge/src/eve_bridge/rundown.py bridge/src/eve_bridge/daemon.py bridge/src/eve_bridge/voice_agent.py bridge/tests/test_codex_computer.py bridge/tests/test_rundown.py bridge/tests/test_telegram_images.py && bridge/.venv/bin/python -m pytest -q -p no:cacheprovider bridge/tests/test_telegram.py bridge/tests/test_voice_agent.py bridge/tests/test_daemon_*.py
  EXPECT: passed
  EVIDENCE: exit=0; shell=/bin/sh; cwd=/Users/gurucharan/Documents/personal/eve; path=5cdd84c66b04/21 entries; output=.......                                                                  [100%] | 223 passed in 25.13s

- [x] G7: Texting rundown loads a packaged skill, reads today's Obsidian todos, requests current email/calendar/Slack through existing app tools, and refuses writes even when Codex relay is selected.
  CHECK: bridge/.venv/bin/python -m pytest -q -p no:cacheprovider bridge/tests/test_rundown.py
  EXPECT: passed
  EVIDENCE: exit=0; shell=/bin/sh; cwd=/Users/gurucharan/Documents/personal/eve; path=5cdd84c66b04/21 entries; output=.....                                                                    [100%] | 5 passed in 0.09s
