# Gates: eve's expressions in the phone's eyes

OWNS: bridge/src/eve_bridge/live_expressions.py, the phone voice path that carries turns to the app (phone_call.py / link_chat.py / voice_agent.py as found), daemon.py wiring, bridge/tests/test_live_expressions*.py and the phone-path tests, ios/EveLink/App/StackChanFace.swift, ios/EveLink/App/VoiceModeView.swift, ios/EveLink/App/VoiceCall.swift, ios/EveLink/Shared/** (a new FaceExpression model), ios/EveLink/Tests/**, docs/voice-mode.md, docs/phone-eyes-gates.md

Scope: the owner, 2026-10-12: "can u route the expressions into the eyes". live_expressions.py already picks one of eye_model.LABELS (calm, happy, curious, affection, surprised, sad, worried, skeptical, frustrated, excited, wink) from the conversation with Jev, and sent it to the StackChan board over BLE; the board is retired (2026-10-03), nothing offers text to it now, and the phone's voice-mode face (StackChanFace.swift, the same RoboEyes eyes) shows only idle/listening/thinking/speaking/muted/connecting. So: phone voice turns feed the picker, each pick goes to the app, and the face draws the firmware's expression shapes (firmware/claude_pet_stackchan/src/expression.h Style table) for the pick's time to live, never while the owner is talking.

- [ ] E1: Bridge: voice-mode turns from the phone reach the picker, picks reach the app's channel with id and ttl, nothing is sent while the switch is off or no phone call is live, and a failed pick sends nothing.
  CHECK: cd bridge && .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_live_expressions.py tests/test_phone_expressions.py && echo EYES_BRIDGE_OK
  EXPECT: EYES_BRIDGE_OK

- [ ] E2: The full bridge suite and lint pass.
  CHECK: bridge/.venv/bin/python -m pytest -q -p no:cacheprovider -n auto bridge/tests && (cd bridge && uv run ruff check src tests tools) && echo BRIDGE_OK
  EXPECT: BRIDGE_OK

- [ ] E3: App: the expression message decodes, the shape table matches the firmware's for every label, stale or older ids are refused, ttl is clamped to 0.5-6 s, and no expression shows while the owner is talking or the face is muted/connecting.
  CHECK: sh ios/EveLink/Tests/run.sh && echo APP_TESTS_OK
  EXPECT: APP_TESTS_OK

- [ ] E4: The app builds for the iPhone.
  CHECK: sh ios/EveLink/build.sh && echo APP_BUILD_OK
  EXPECT: APP_BUILD_OK

- [ ] E5: No personal data or token-shaped strings in the diff.
  CHECK: git diff main | python3 -c "import re,sys; d=sys.stdin.read(); bad=[p for p in (r'guru\.lingamallu', r'@gmail\.com', r'guruchara[n]', r'Bearer [A-Za-z0-9_-]{20,}', r'sk-[A-Za-z0-9]{20,}', r'/Users/') if re.search(p, d)]; print(bad or 'DIFF_CLEAN'); sys.exit(1 if bad else 0)"
  EXPECT: DIFF_CLEAN

- [ ] E6: In voice mode on the owner's iPhone, eve's eyes change with what is said (a joke winks or smiles, bad news droops) and go back to normal.
  MANUAL: the owner's word.
