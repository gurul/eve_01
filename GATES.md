# Gates: the OBSBOT camera, from the CLI and from Eve Link

OWNS: bridge/src/eve_bridge/obsbot.py, bridge/src/eve_bridge/obsbot_panel.py, bridge/src/eve_bridge/link_chat.py, bridge/src/eve_bridge/daemon.py, bridge/src/eve_bridge/miniapp.py, bridge/src/eve_bridge/cli.py, bridge/tests/test_obsbot*.py, bridge/tools/obsbot_live_check.py, ios/EveLink/** (ObsbotView.swift, ObsbotControl.swift, ControlsClient.swift, ControlSheets.swift, WorkView.swift, PikaView.swift, Tests/main.swift), docs/obsbot.md, docs/app-chat.md, README.md, GATES.md

Scope: the owner, 2026-10-11: "use [REA] to [reverse-engineer the] obsbot camera controller ... for the tiny 3 ... allow it to be interacted [with] via cli so eve can control it", then "after ur confident wit this step, build in an app into eve for obsbot". The CLI is `eve-bridge obsbot` over OBSBOT Center's OSC; the app is an Apps-tab row in Eve Link with the camera's live picture, hold-to-turn arrows, zoom, center, sleep/wake, tracking and AI mode. The ledger this replaces (the rename to eve) is at fcd2987.

- [x] G1: The obsbot module and its Eve Link panel pass their tests (a fake OBSBOT Center that answers the way the real one measured).
  CHECK: cd bridge && .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_obsbot.py tests/test_obsbot_panel.py && echo OBSBOT_TESTS_OK
  EXPECT: OBSBOT_TESTS_OK
  EVIDENCE: exit=0; shell=/bin/sh; cwd=<repo>; path=8ae25fade4c8/16 entries; output=23 passed in 4.25s | OBSBOT_TESTS_OK

- [x] G2: The bridge's full test suite passes.
  CHECK: bridge/.venv/bin/python -m pytest -q -p no:cacheprovider -n auto bridge/tests && echo BRIDGE_TESTS_OK
  EXPECT: BRIDGE_TESTS_OK
  EVIDENCE: exit=0; shell=/bin/sh; cwd=<repo>; path=8ae25fade4c8/16 entries; output=6400 passed, 23 skipped, 2 xfailed, 1 warning in 200.46s (0:03:20) | BRIDGE_TESTS_OK

- [x] G3: Lint is clean.
  CHECK: cd bridge && uv run ruff check src tests tools && echo LINT_OK
  EXPECT: LINT_OK
  EVIDENCE: exit=0; shell=/bin/sh; cwd=<repo>; path=8ae25fade4c8/16 entries; output=All checks passed! | LINT_OK

- [x] G4: The Swift checks pass, including the camera's (Tests/run.sh).
  CHECK: cd ios/EveLink && ./Tests/run.sh && echo SWIFT_CHECKS_OK
  EXPECT: SWIFT_CHECKS_OK
  EVIDENCE: exit=0; shell=/bin/sh; cwd=<repo>; path=8ae25fade4c8/16 entries; output=CAPTIONS_OK | SWIFT_CHECKS_OK

- [x] G5: The iOS app builds for the simulator.
  CHECK: cd ios/EveLink && xcodegen generate -q && xcodebuild -project EveLink.xcodeproj -scheme EveLink -sdk iphonesimulator -configuration Debug -derivedDataPath build-sim build -quiet && echo SIM_BUILD_OK
  EXPECT: SIM_BUILD_OK
  EVIDENCE: exit=0; shell=/bin/sh; cwd=<repo>; path=8ae25fade4c8/16 entries; output=(xcodebuild destination list; device ids redacted) | SIM_BUILD_OK

- [x] G6: The live daemon, over the same HTTP door the phone uses, lists the camera, reads its status, moves it and reads the move back, streams its picture, and gives a WebRTC viewer a steady picture (gap p95 under 200 ms) while a second viewer keeps getting frames.
  CHECK: bridge/.venv/bin/python bridge/tools/obsbot_live_check.py
  EXPECT: OBSBOT_LIVE_OK
  EVIDENCE: exit=0; shell=/bin/sh; cwd=<repo>; path=8ae25fade4c8/16 entries; output=webrtc: 90 frames in 6 s, gap p95 76 ms; the stream viewer got 92 at the same time | OBSBOT_LIVE_OK

- [x] G7: The CLI drives the real camera: status, a move read back within 3°, zoom read back, center.
  CHECK: cd bridge && .venv/bin/eve-bridge obsbot move --pan 30 --pitch 10 && .venv/bin/eve-bridge obsbot zoom 40 && .venv/bin/eve-bridge obsbot zoom 0 && .venv/bin/eve-bridge obsbot center && .venv/bin/eve-bridge obsbot status && echo CLI_LIVE_OK
  EXPECT: CLI_LIVE_OK
  EVIDENCE: exit=0; shell=/bin/sh; cwd=<repo>; path=8ae25fade4c8/16 entries; output=Tiny 3 awake; pan 0°, pitch 0°; zoom 0%; presets: none saved. | CLI_LIVE_OK

- [x] G8: No personal data or token-shaped strings in the diff (the repo is public).
  CHECK: git diff HEAD | python3 -c "import re,sys; d=sys.stdin.read(); bad=[p for p in (r'guru\.lingamallu', r'@gmail\.com', r'Bearer [A-Za-z0-9_-]{20,}', r'[a-z0-9]+(-[a-z0-9]+){2,}\.trycloudflare\.com', r'sk-[A-Za-z0-9]{20,}') if re.search(p, d)]; print(bad or 'DIFF_CLEAN'); sys.exit(1 if bad else 0)"
  EXPECT: DIFF_CLEAN
  EVIDENCE: exit=0; shell=/bin/sh; cwd=<repo>; path=8ae25fade4c8/16 entries; output=DIFF_CLEAN

- [x] G9: The app is installed on the owner's iPhone (build.sh --install), with the WebRTC picture (2026-10-11 second build).
  EVIDENCE: manual, 2026-10-11, second build: `sh ios/EveLink/build.sh --install` -> devicectl "App installed: bundleID: com.github.eve-bridge.EveLink" (the iPhone available and paired). First build: manual, 2026-10-11: `sh ios/EveLink/build.sh --install` -> devicectl "App installed: bundleID: com.github.eve-bridge.EveLink" on the paired iPhone 17 Pro. Before that, the same build in the iPhone 17 Pro simulator, paired to the live daemon, opened Apps > Camera showing the live picture, "Pan 0°, tilt -3°", the arrows around Center, zoom, Sleep / Track / Mode.

- [x] G10: The owner opens Camera in the Apps tab, sees the picture, and turns the camera with the arrows.
  EVIDENCE: manual, 2026-10-11: the owner, on the second build (WebRTC picture, shared capture), after using Apps > Camera on the iPhone: "beautiful push".
