# Gates: small talk answered fast by Haiku

OWNS: bridge/src/eve_bridge/small_talk.py, the chat brain's turn entry in bridge/src/eve_bridge/telegram.py, bridge/tests/test_small_talk.py, bridge/tests/fixtures/small_talk_blind.json, bridge/tests/fixtures/small_talk_dev.json, bridge/tools/small_talk_eval.py, docs/small-talk.md, docs/small-talk-gates.md, README.md

Scope: the owner, 2026-10-12: "can simple interactions be handled with haiku for really fast speed, like hi shouldn't take this long to respond to". Measured the same day from the daemon log: since the Codex subscription went on (2026-10-11 13:38) one-call chat turns take 4.2-11.8 s (before, on OpenRouter luna: 2.3-2.6 s), and every turn carries 37-58k characters of instructions. Haiku 5.5 through OpenRouter answered "hi" under a one-line prompt in 1.33-1.50 s (3 runs). So: a code gate picks only short, attachment-free messages while nothing is waiting on the owner; Haiku answers in eve's voice with the last few messages, or hands off to the full brain.

- [ ] S1: The lane's gate, hand-off, transcript and switch pass their tests (never while eve awaits a confirmation, a question, or a task's answer; never with an attachment; a hand-off reaches the full brain unchanged; EVE_SMALL_TALK=0 is today's behaviour).
  CHECK: cd bridge && .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_small_talk.py && echo SMALL_TALK_TESTS_OK
  EXPECT: SMALL_TALK_TESTS_OK

- [ ] S2: The full bridge suite and lint pass.
  CHECK: bridge/.venv/bin/python -m pytest -q -p no:cacheprovider -n auto bridge/tests && (cd bridge && uv run ruff check src tests tools) && echo BRIDGE_OK
  EXPECT: BRIDGE_OK

- [ ] S3: On a BLIND set (tests/fixtures/small_talk_blind.json, written by a separate agent from a brief, without the code or the prompt), with the real Haiku and the production gate: the lane answers ZERO of the cases that need the full brain; it answers >= 80% of the small-talk cases; ZERO lane replies claim to have done, checked, sent, scheduled or looked anything up (a separate judge model, fixed rubric); lane latency p50 <= 1.8 s and p95 <= 3.0 s; zero provider errors. Bar fixed before the set exists.
  Scored once per owner voice, the bar on each: the eval's default runs `--voice none` (memory off) and `--voice sample` (a fictional "How they like to talk" section), because production adds the profile's section when memory is on (review, 2026-10-12; stricter, not looser).
  CHECK: cd bridge && env $(grep -E '^(OPENROUTER_API_KEY)=' ~/.config/eve-bridge/env | xargs) .venv/bin/python tools/small_talk_eval.py tests/fixtures/small_talk_blind.json --coverage 80 --p50 1.8 --p95 3.0
  EXPECT: SMALL_TALK_BAR_OK
  EVIDENCE: FAILED, exit=1, 2026-10-12, first and only scored run on the blind set (frozen in its own commit before the run). voice none: needs_brain answered 0; coverage 88.6% (31/35); judge yes 1 (blind-st-30 '👍' -> 'Sounds good, Friday it is.'); p50 1.47 s, p95 1.90 s; provider errors 0. voice sample: needs_brain answered 0; coverage 77.1% (27/35, misses: 'glad that's sorted' replies caught by the claims guard, and Haiku correcting the owner's time-of-day greeting); judge yes 0; p50 1.43 s, p95 1.70 s. Hand-off cost on the brain's path p50 1.38-1.46 s. | SMALL_TALK_BAR_FAILED. Not tuned against this set; SHIPPED stays False. A v2 needs a fresh blind set.

- [ ] S3b: The simple design (owner, 2026-10-12: "route to haiku only for small talk", "occams razor"): a fixed phrase list picks small talk by code, Haiku answers with no history or clock, everything else goes to the brain untouched. On a FRESH blind set (tests/fixtures/small_talk_blind_v2.json, written from a brief without the code; the v1 set is spent): needs_brain answered by the lane = 0; judge yes = 0; small_talk coverage >= 70% (a fixed list trades coverage for safety); lane p50 <= 1.8 s, p95 <= 3.0 s; provider errors 0. Bar written before the v2 set exists.
  CHECK: cd bridge && env $(grep -E '^(OPENROUTER_API_KEY)=' ~/.config/eve-bridge/env | xargs) .venv/bin/python tools/small_talk_eval.py tests/fixtures/small_talk_blind_v2.json --coverage 70 --p50 1.8 --p95 3.0
  EXPECT: SMALL_TALK_BAR_OK

- [ ] S4: No personal data or token-shaped strings in the diff (the repo is public).
  CHECK: git diff main | python3 -c "import re,sys; d=sys.stdin.read(); bad=[p for p in (r'guru\.lingamallu', r'@gmail\.com', r'guruchara[n]', r'Bearer [A-Za-z0-9_-]{20,}', r'sk-[A-Za-z0-9]{20,}', r'/Users/') if re.search(p, d)]; print(bad or 'DIFF_CLEAN'); sys.exit(1 if bad else 0)"
  EXPECT: DIFF_CLEAN

- [ ] S5: On the live daemon, the owner says "hi" in the app and the log's turn line shows the small-talk lane answering in under 2 s.
  MANUAL: the log line after the owner's message, and the owner's word.
