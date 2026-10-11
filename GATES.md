# Gates: eve moves the camera when asked (chat, app chat, voice)

OWNS: bridge/src/eve_bridge/obsbot.py, bridge/src/eve_bridge/obsbot_usb.py, bridge/src/eve_bridge/native/obsbot_uvc.c, bridge/pyproject.toml, bridge/src/eve_bridge/obsbot_panel.py, bridge/src/eve_bridge/daemon.py, bridge/src/eve_bridge/telegram.py, bridge/src/eve_bridge/voice_agent.py, bridge/src/eve_bridge/tool_gate.py, bridge/tools/intent_eval.py, bridge/tools/obsbot_chat_check.py, bridge/tests/** (test_obsbot_brain.py and the device-tool tests it touches), bridge/tests/fixtures/intent_dev.json, docs/obsbot.md, docs/stackchan/telegram.md, docs/toolset-router.md, README.md, GATES.md

Scope: the owner, 2026-10-11, after the camera CLI and app shipped (6f681c5): "go for it build that", meaning eve itself answers "point the camera at the door", "zoom in", "camera left" in the chat, the app chat and out loud. Midway the owner asked "can't u just interface with [the camera] directly" and chose "Direct + app for AI": moves, turns, zoom, center and wake go over USB (UVC through IOKit, native/obsbot_uvc.c), and tracking, AI modes, presets and sleep use OBSBOT Center only while it runs; eve never starts the app. Two tools for the models (camera_status, camera_control), a no-model code path for plain commands that name the camera, and the safety/intent wiring. The camera's picture stays take_photo's (it already uses this camera). The ledger this replaces (the camera CLI and app) is at 6f681c5.

- [x] G1: The camera's model surface and its wiring pass their tests: strict tools, handle, the code path's matcher (and what it leaves to the model), Telegram's fast path and tool offer and dispatch, the voice's offer and dispatch, the tool gate, the intent eval's label and code route, the menu-tool intent lines.
  CHECK: cd bridge && .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_obsbot.py tests/test_obsbot_usb.py tests/test_obsbot_panel.py tests/test_obsbot_brain.py tests/test_telegram.py tests/test_voice_agent.py tests/test_tool_gate.py tests/test_intent_eval.py tests/test_menu_tools.py && echo BRAIN_TESTS_OK
  EXPECT: BRAIN_TESTS_OK
  EVIDENCE: exit=0; shell=/bin/sh; cwd=<repo>; path=8ae25fade4c8/16 entries; output=719 passed in 26.86s | BRAIN_TESTS_OK

- [x] G2: The bridge's full test suite passes.
  CHECK: bridge/.venv/bin/python -m pytest -q -p no:cacheprovider -n auto bridge/tests && echo BRIDGE_TESTS_OK
  EXPECT: BRIDGE_TESTS_OK
  EVIDENCE: exit=0; shell=/bin/sh; cwd=<repo>; path=8ae25fade4c8/16 entries; output=6464 passed, 23 skipped, 2 xfailed in 209.98s (0:03:29) | BRIDGE_TESTS_OK

- [x] G3: Lint is clean.
  CHECK: cd bridge && uv run ruff check src tests tools && echo LINT_OK
  EXPECT: LINT_OK
  EVIDENCE: exit=0; shell=/bin/sh; cwd=<repo>; path=8ae25fade4c8/16 entries; output=All checks passed! | LINT_OK

- [x] G4: The live daemon's chat brain moves the real camera: a plain "camera left" is done by code (no model turn) and the camera's pan changes; a request in other words goes to the model, which calls camera_control, and the camera moves the way asked. Read back from the camera, not from the reply.
  CHECK: bridge/.venv/bin/python bridge/tools/obsbot_chat_check.py
  EXPECT: OBSBOT_CHAT_OK
  EVIDENCE: exit=0; shell=/bin/sh; cwd=<repo>; path=8ae25fade4c8/16 entries; output=the model: "could you swing the webcam roughly 30 degrees to the right of where it's aimed now" -> pan 20° -> -9°; eve said: Swung it 30° right. It’s now pointing 9° right of center, level. | OBSBOT_CHAT_OK

- [x] G5: The intent eval, with the real chat model and production routing, on a BLIND set (tests/fixtures/intent_camera_blind.json: 30 cases written by a separate agent from a brief, without the code or the tool text): camera_aim >= 90% (18 of 20), camera_photo >= 75% (3 of 4), no "never" label fires, no hard negative acts, no provider errors. Bar fixed before the first run on the set. AMENDED after the first four runs (owner, 2026-10-11: "fix the G5 eval set conflict"): the set and intent_eval's rule conflicted, since my brief to the blind writer offered computer_task as a hard negative's answer while a hard negative fails on any acting step. The fix is a rule, not a result: intent_eval now refuses a hard_negative case that accepts an acting-only label (CONFLICT), and a case may name "never" labels that must not fire at any step; the set was amended mechanically (every hard_negative case gets never=[camera_aim], the brief's own intent; blind-cam-28 and -30, which accept computer_task, are no longer hard_negative). No text or expected label changed. Before the amendment, all four runs: camera_aim 20/20, camera_photo 4/4, HARD_NEGATIVE_ACTED 2 (blind-cam-28, -30 -> start_task), so the bar was missed.
  CHECK: cd bridge && env $(grep -E '^(OPENROUTER_API_KEY)=' ~/.config/eve-bridge/env | xargs) .venv/bin/python tools/intent_eval.py tests/fixtures/intent_camera_blind.json --require camera_aim=90 --require camera_photo=75
  EXPECT: INTENT_BAR_OK
  EVIDENCE: exit=0; shell=/bin/sh; cwd=<repo>; path=8ae25fade4c8/16 entries; output=cost: $0.0634 over 79 model calls (0 unpriced); scored 2026-10-11T12:02:19 | INTENT_BAR_OK

- [x] G6: No personal data or token-shaped strings in the diff (the repo is public).
  CHECK: git diff HEAD | python3 -c "import re,sys; d=sys.stdin.read(); bad=[p for p in (r'guru\.lingamallu', r'@gmail\.com', r'guruchara[n]', r'Bearer [A-Za-z0-9_-]{20,}', r'[a-z0-9]+(-[a-z0-9]+){2,}\.trycloudflare\.com', r'sk-[A-Za-z0-9]{20,}') if re.search(p, d)]; print(bad or 'DIFF_CLEAN'); sys.exit(1 if bad else 0)"
  EXPECT: DIFF_CLEAN
  EVIDENCE: exit=0; shell=/bin/sh; cwd=<repo>; path=8ae25fade4c8/16 entries; output=DIFF_CLEAN

- [x] G7: The owner asks eve, in the app chat or out loud, to move the camera, and it moves.
  EVIDENCE: manual, 2026-10-11: the owner, after asking eve to move the camera from the iPhone: "it works on my phone, push".
