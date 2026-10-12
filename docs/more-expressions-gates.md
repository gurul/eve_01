# Gates: more expressions for eve's eyes

OWNS: bridge/src/eve_bridge/eye_model.py (CRITERIA), bridge/tools/jev_eyes_eval.py, bridge/tests/fixtures/emotion_blind_v2.json, bridge/tests/test_live_expressions.py, bridge/tests/test_eye_model*.py, ios/EveLink/Shared/FaceExpression.swift, ios/EveLink/App/StackChanFace.swift, ios/EveLink/Tests/**, docs/voice-mode.md, docs/more-expressions-gates.md, docs/expressions-design.md

Scope: the owner, 2026-10-12, while the phone-eyes build (feat/phone-eyes) routes the eleven eye labels into the voice-mode face: "also build more expressions". Today's set is eye_model.CRITERIA (calm, happy, curious, affection, surprised, sad, worried, skeptical, frustrated, excited, wink). Baseline, measured 2026-09-24 (eye_model.py docstring, tools/jev_eyes_eval.py on the 84 hand-written cases in tests/fixtures/emotion, folded onto their six labels): Jev 62.5% on the 48 scenario tests, 72.2% on the 36 confirmation cases, ~200 ms p50. New labels change Jev's one choice question, so they must be recognised AND must not blur the old ones. Built on top of feat/phone-eyes once it lands.

Bar, PRE-REGISTERED 2026-10-12 before the new label set or its blind set exists:

- [ ] X1: The new labels, their Jev criteria and the face's shapes pass their tests (every label has a shape, decoding, gating).
  CHECK: cd bridge && .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_live_expressions.py tests/test_phone_expressions.py && cd .. && sh ios/EveLink/Tests/run.sh && echo EXPR_TESTS_OK
  EXPECT: EXPR_TESTS_OK

- [ ] X2: The full bridge suite, lint and the iPhone build pass.
  CHECK: bridge/.venv/bin/python -m pytest -q -p no:cacheprovider -n auto bridge/tests && (cd bridge && uv run ruff check src tests tools) && sh ios/EveLink/build.sh && echo ALL_BUILD_OK
  EXPECT: ALL_BUILD_OK

- [ ] X3: New labels are recognised. On a BLIND set (tests/fixtures/emotion_blind_v2.json, written by a separate agent from label names and plain glosses, without the Jev criteria text), Jev with the production question: top-1 accuracy >= 65% over all cases; every label with >= 5 cases has recall >= 55%; p50 latency <= 400 ms; errors <= 2%.
  CHECK: cd bridge && env $(grep -E '^(EVE_JEV_[A-Z_]*|JEV_[A-Z_]*|OPENROUTER_API_KEY)=' ~/.config/eve-bridge/env | xargs) .venv/bin/python tools/jev_eyes_eval.py --blind tests/fixtures/emotion_blind_v2.json --min-acc 65 --min-recall 55 --max-p50-ms 400
  EXPECT: EYES_BAR_OK
  EVIDENCE: FAILED, exit=1, 2026-10-12, first scored run on the blind set (frozen in 36f0eb9 before any label existed in code): n=129, top-1 86.8%, errors 0, p50 283 ms, p95 697 ms, $0.0039; every label >= 55% except thinking 3/8 = 37.5% (read as calm x3, curious x2); laughing 5/8, happy 5/8, curious 5/8 | EYES_BAR_FAIL. Not retuned against this set.

- [x] X4: Old labels do not blur. On the existing 84 cases (tests/fixtures/emotion, folded as on 2026-09-24): scenario tests >= 57.5% and confirmation cases >= 67.2% (baseline minus 5 points each).
  CHECK: cd bridge && env $(grep -E '^(EVE_JEV_[A-Z_]*|JEV_[A-Z_]*|OPENROUTER_API_KEY)=' ~/.config/eve-bridge/env | xargs) .venv/bin/python tools/jev_eyes_eval.py --legacy --min-scenario 57.5 --min-confirm 67.2
  EXPECT: EYES_LEGACY_OK
  EVIDENCE: exit=0, same tree: scenarios 64.6%, confirmations 69.4%; one legacy miss lands on a new label (secret_costume-1 surprised -> proud) | EYES_LEGACY_OK

- [ ] X5: No personal data or token-shaped strings in the diff.
  CHECK: git diff main | grep -v 'CHECK: git diff main' | python3 -c "import re,sys; d=sys.stdin.read(); bad=[p for p in (r'guru\.lingamallu', r'@gmail\.com', r'guruchara[n]', r'Bearer [A-Za-z0-9_-]{20,}', r'sk-[A-Za-z0-9]{20,}', r'/Users/') if re.search(p, d)]; print(bad or 'DIFF_CLEAN'); sys.exit(1 if bad else 0)"
  EXPECT: DIFF_CLEAN

- [ ] X6: In voice mode on the owner's iPhone the new expressions show and read right.
  MANUAL: the owner's word.
