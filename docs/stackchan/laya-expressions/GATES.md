# Gates: live Laya expressions on Eve

Scope: Run local Laya on live conversation/diary events and render temporary eye cues. The owner revised the request to eyes only after trying the initial chirps: original sound behavior must be restored. Enable on this Mac, flash Eve, verify the actual model-to-device path, and push to main. Model accuracy limits remain documented.

- [x] G1: Host service uses real model results asynchronously, bounds pending work, drops stale results, preserves original caption chirps and mute, and consumes live voice/diary events; regression tests pass.
  CHECK: bridge/.venv/bin/python -m pytest -q bridge/tests/test_live_expressions.py bridge/tests/test_voice_agent.py bridge/tests/test_emotion_policy.py bridge/tests/test_sound.py && echo LIVE_HOST_OK
  EXPECT: LIVE_HOST_OK
  EVIDENCE: exit=0; shell=/bin/sh; cwd=/Users/gurucharan/Documents/personal/eve; path=07a05fc4f0d2/21 entries; output=116 passed in 24.05s | LIVE_HOST_OK

- [x] G2: Firmware expression arbitration validates labels, rejects duplicate events, expires cues, restores base states, and protects listening/attention.
  CHECK: c++ -std=c++17 -Wall -Wextra -Werror firmware/claude_pet_stackchan/host/expression_test.cpp -o /tmp/eve-expression-test && /tmp/eve-expression-test
  EXPECT: EXPRESSION_FIRMWARE_OK
  EVIDENCE: exit=0; shell=/bin/sh; cwd=/Users/gurucharan/Documents/personal/eve; path=07a05fc4f0d2/21 entries; output=EXPRESSION_FIRMWARE_OK

- [x] G3: Changed Python files pass lint, firmware builds and uploads, and the bridge reconnects to the flashed revision.
  EVIDENCE: Python lint passed; firmware 900435c built (792823 bytes flash, 142648 bytes static RAM), USB upload verified every hash, and the restarted bridge received 900435c eye/wink telemetry. device-results.json records six actual-model cases, including closed-lid rendering followed by reopening. Rounded-square geometry, curve corner zones, timing and symmetry passed the C++ checks.

- [x] G4: With the actual local model enabled, a fresh event reaches the board; telemetry confirms eye-overlay application, expiry, suppression while listening, no Laya chirps, and an unchanged mute setting. The wink reports one closed curved-line hold followed by reopening.
  CHECK: bridge/.venv/bin/python bridge/tools/expression_live.py --check
  EXPECT: LIVE_LAYA_DEVICE_OK
  EVIDENCE: exit=0; shell=/bin/sh; cwd=/Users/gurucharan/Documents/personal/eve; path=07a05fc4f0d2/21 entries; output=] | LIVE_LAYA_DEVICE_OK

- [x] G5: README and docs describe live behavior, controls, model location, known accuracy limits and measured verification. Authorized changes are pushed to main.
  EVIDENCE: README and docs cover eleven expressions, binary Laya selection, chronological speaker context, the reference-inspired 650 ms curved wink, unchanged original chirps, all failed prompt trials, and the 34/39 reused-development result. Implementation through 900435c was pushed to origin/main; verification evidence is committed with this ledger.

- [x] G6: Expanded eleven-label eye vocabulary has distinct visual profiles and uses speaker-labelled conversational context. A real local model evaluation measures label correspondence on authored user/reply scenarios; at least 28 of 36 explicit benchmark labels match, and every added expression is exercised, including three wink cases. This is a development check reused for prompt selection, not a held-out accuracy estimate. Results remain synthetic, not human validation.
  CHECK: bridge/.venv/bin/python bridge/tools/eye_context_eval.py && echo EYE_CONTEXT_OK
  EXPECT: EYE_CONTEXT_OK
  EVIDENCE: exit=0; shell=/bin/sh; cwd=/Users/gurucharan/Documents/personal/eve; path=07a05fc4f0d2/21 entries; output=skeptical -> excited That sounds too good to be true. | EYE_CONTEXT_OK
