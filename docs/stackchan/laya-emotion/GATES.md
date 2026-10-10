# Gates: Laya specialization for Eve expression

Scope: Research the actual checkpoint and training code, build a reproducible local specialization experiment, measure held-out expression decisions and runtime constraints, and document an evidence-based integration decision. This is expression selection for Eve, not diagnosis of the person's internal emotion. No push or hardware activation is included.

- [x] G1: Research links and design claims agree with primary model sources, actual installed code, and Eve's firmware.
  EVIDENCE: Reviewed Laya upstream commit 573e5b62696ba441230cd6be71d593331b5d23af and training notebook (RL plus CE), installed laya_mlx model/agent/common code and checkpoint config, main.cpp mood-expression admission, eyes.cpp speaking priority, primary eye/prosody/calibration/SetFit papers. Corrected the unsupported eyes-plus-head-plus-color description in personality.md. AlphaXiv used for discovery; authors' abstracts/papers used for claims.

- [x] G2: Dataset splits are disjoint by scenario family, labels map to supported expressions, and the evaluation metrics and temporal controller pass behavioral tests including failure controls.
  CHECK: bridge/.venv/bin/python -m pytest -q bridge/tests/test_emotion_policy.py && echo EMOTION_TESTS_OK
  EXPECT: EMOTION_TESTS_OK
  EVIDENCE: exit=0; shell=/bin/sh; cwd=/Users/gurucharan/Documents/personal/eve; path=07a05fc4f0d2/21 entries; output=16 passed in 0.05s | EMOTION_TESTS_OK

- [x] G3: A real local model run produces baseline, tuning, calibration, and held-out results with checkpoint and dataset provenance; artifact verification rejects tampered evidence.
  CHECK: bridge/.venv/bin/python bridge/tools/emotion_eval.py verify --report docs/stackchan/laya-emotion/results.json && bridge/.venv/bin/python bridge/tools/emotion_head_tune.py verify && bridge/.venv/bin/python bridge/tools/emotion_confirm.py verify && echo EMOTION_EVIDENCE_OK
  EXPECT: EMOTION_EVIDENCE_OK
  EVIDENCE: exit=0; shell=/bin/sh; cwd=/Users/gurucharan/Documents/personal/eve; path=07a05fc4f0d2/21 entries; output=CONFIRMATION_REPLAY_OK | EMOTION_EVIDENCE_OK

- [x] G4: Changed Python files pass lint and the existing affect and decider regression tests pass.
  CHECK: bridge/.venv/bin/ruff check bridge/src/eve_bridge/emotion_policy.py bridge/tools/emotion_eval.py bridge/tools/emotion_head_tune.py bridge/tools/emotion_confirm.py bridge/tests/test_emotion_policy.py && bridge/.venv/bin/python -m pytest -q bridge/tests/test_decider.py && echo EMOTION_REGRESSION_OK
  EXPECT: EMOTION_REGRESSION_OK
  EVIDENCE: exit=0; shell=/bin/sh; cwd=/Users/gurucharan/Documents/personal/eve; path=07a05fc4f0d2/21 entries; output=42 passed in 0.04s | EMOTION_REGRESSION_OK

- [x] G5: Documentation records measured limits, distinguishes synthetic-label results from human validation, and describes the concrete eye/sound integration including conversation-phase priority.
  EVIDENCE: README reports original and fresh-set counts/F1, all-abstain calibration failure, model-only latency scope, same-assistant synthetic labels, exploratory reuse of the first test, and fresh confirmation without retuning. Existing emote deltas are distinguished from an unimplemented exact-expression overlay; mute/listening/caption arbitration and hardware validation remain explicit future work. Root README and personality.md link the study. Negative tuning results are retained; no production-quality or live-hardware completion claim is made.
