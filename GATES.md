# Gates: Updates, a pane of things to act on, left of Chat in Eve Link

OWNS: bridge/src/eve_bridge/updates.py, bridge/src/eve_bridge/link_chat.py, bridge/src/eve_bridge/daemon.py, bridge/tools/updates_eval.py, bridge/tools/updates_live_check.py, bridge/tests/test_updates*.py, bridge/tests/test_link_chat.py, bridge/tests/fixtures/updates/**, ios/EveLink/App/**, ios/EveLink/Shared/**, ios/EveLink/Tests/**, bridge/src/eve_bridge/phone_location.py, bridge/tools/phone_location_check.py, bridge/tests/test_phone_location.py, the brain/voice tool registration files, docs/updates.md, docs/phone-location.md, docs/app-chat.md, README.md, GATES.md

Scope: the owner, 2026-10-12, from a screenshot of Hark's Home tab: "build this into eve in an updates section". Then: "find actionable steps from our emails notes everything we have and suggest things"; no "create a new panel", no weather, "just actionable things"; "a pane to left of chat", "when I open, chat should still be main pane". So: a bridge producer reads the owner's Gmail, calendar, Notes vault, records/dream journal and chief cards, asks a model for actionable suggestions only, caches them; `/api/link/updates` serves them; the app gets an Updates tab left of Chat, opens on Chat, and a tapped card sends its visible sentence to chat. The ledger this replaces (the camera) is at 0b8ec33.

- [x] G1: The Updates producer, store and routes pass their tests (sources degrade one at a time, only actionable cards kept, card shape clamped, dismiss persists, routes need the link token and refuse an Origin).
  CHECK: cd bridge && .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_updates.py tests/test_link_chat.py && echo UPDATES_TESTS_OK
  EXPECT: UPDATES_TESTS_OK
  EVIDENCE: exit=0; cwd=<repo>/bridge; feat/updates 314f528; output=159 passed in 5.87s | UPDATES_TESTS_OK

- [x] G2: The bridge's full test suite passes.
  CHECK: bridge/.venv/bin/python -m pytest -q -p no:cacheprovider -n auto bridge/tests && echo BRIDGE_TESTS_OK
  EXPECT: BRIDGE_TESTS_OK
  EVIDENCE: exit=0; feat/updates 314f528; output=6621 passed, 23 skipped, 2 xfailed in 211.22s | BRIDGE_TESTS_OK

- [x] G3: Lint is clean.
  CHECK: cd bridge && uv run ruff check src tests tools && echo LINT_OK
  EXPECT: LINT_OK
  EVIDENCE: exit=0; feat/updates 314f528; output=All checks passed! | LINT_OK

- [x] G4: The app's logic tests pass on the Mac, including the Updates decoding and the tap-to-chat text.
  CHECK: sh ios/EveLink/Tests/run.sh && echo APP_TESTS_OK
  EXPECT: APP_TESTS_OK
  EVIDENCE: exit=0; feat/updates 314f528; output=eve link: all checks passed (UPDATES_OK, LOCATION_OK) | APP_TESTS_OK

- [x] G5: The app builds for the iPhone.
  CHECK: sh ios/EveLink/build.sh && echo APP_BUILD_OK
  EXPECT: APP_BUILD_OK
  EVIDENCE: exit=0; feat/updates 314f528; output=built: …/Debug-iphoneos/EveLink.app (then installed with --install, 2026-10-12 08:5x) | APP_BUILD_OK

- [x] G6: The suggester, with the real model, on a BLIND set (tests/fixtures/updates/blind.json, written by a separate agent from a brief, without the code or the prompt): recall of must-suggest items >= 80%; cards about must-not-suggest items (newsletters, receipts, done things, information with nothing to do) <= 10% of them; zero cards that carry out an instruction planted in an email; zero provider errors. Bar fixed before the first run on the set.
  CHECK: cd bridge && env $(grep -E '^(OPENROUTER_API_KEY)=' ~/.config/eve-bridge/env | xargs) .venv/bin/python tools/updates_eval.py tests/fixtures/updates/blind.json --recall 80 --noise 10
  EXPECT: UPDATES_BAR_OK
  EVIDENCE: exit=0; feat/updates 314f528 (blind set frozen in 36a4e02 before any scored run; leak scoring rewritten from review findings before the first blind run); output=recall 100.0% (40/40); noise 0.0% (0/60); leaks 0; provider errors 0; cost $0.0048 | UPDATES_BAR_OK

- [x] G7: The live daemon, on this branch, builds Updates from the owner's real sources and serves them at /api/link/updates with the link token. The check prints counts and source kinds only, never card text.
  CHECK: bridge/.venv/bin/python bridge/tools/updates_live_check.py
  EXPECT: UPDATES_LIVE_OK
  EVIDENCE: exit=0; daemon restarted, log 'code: … @ 314f528 (feat/updates)'; output=refresh started true; cards: 6; by kind: calendar 3, email 1, memory 2; fresh by this check's refresh | UPDATES_LIVE_OK

- [x] G8: No personal data or token-shaped strings in the diff (the repo is public).
  CHECK: git diff main | python3 -c "import re,sys; d=sys.stdin.read(); bad=[p for p in (r'guru\.lingamallu', r'@gmail\.com', r'guruchara[n]', r'Bearer [A-Za-z0-9_-]{20,}', r'[a-z0-9]+(-[a-z0-9]+){2,}\.trycloudflare\.com', r'sk-[A-Za-z0-9]{20,}') if re.search(p, d)]; print(bad or 'DIFF_CLEAN'); sys.exit(1 if bad else 0)"
  EXPECT: DIFF_CLEAN
  EVIDENCE: exit=0; git diff main at 314f528; output=DIFF_CLEAN

- [x] G10: Phone location (owner, 2026-10-12: "also give access to live phone location through app"): the bridge stores only the latest fix (POST /api/link/location, link token, Origin refused, values validated, kept on this Mac, never logged), and the brain's phone_location tool reports place, coordinates, accuracy and age, saying when the fix is stale or missing.
  CHECK: cd bridge && .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_phone_location.py tests/test_link_chat.py && echo LOCATION_TESTS_OK
  EXPECT: LOCATION_TESTS_OK
  EVIDENCE: exit=0; feat/updates 314f528; output=170 passed in 3.88s | LOCATION_TESTS_OK

- [x] G11: The live daemon has a fresh fix from the phone (age under 30 min after the app has been opened). Prints age and accuracy only, never coordinates or place.
  CHECK: bridge/.venv/bin/python bridge/tools/phone_location_check.py
  EXPECT: LOCATION_LIVE_OK
  EVIDENCE: exit=0; after the owner opened the new app, 2026-10-12; output=age 0.6 min, accuracy 7 m | LOCATION_LIVE_OK

- [x] G12: The phone_location tool is a prompt change (AGENTS.md rule 6), so: the intent eval, real chat model and production routing, on a BLIND set (tests/fixtures/intent_location_blind.json, written by a separate agent from a brief, without the code or the tool text): phone_location >= 90% of its cases, no "never" label fires, no hard negative acts, no provider errors. Bar fixed before the set exists. Run from the merged feat/updates tree.
  CHECK: cd bridge && env $(grep -E '^(OPENROUTER_API_KEY)=' ~/.config/eve-bridge/env | xargs) .venv/bin/python tools/intent_eval.py tests/fixtures/intent_location_blind.json --require phone_location=90
  EXPECT: INTENT_BAR_OK
  EVIDENCE: exit=0; run on feat/phone-location d04f26c+1 (the set frozen in its own commit before the first run; blind-loc-29 amended mechanically after intent_eval refused it as CONFLICT, before any scored run, as at G5 of the camera ledger); output=phone_location 19/20; TOTAL 31/32 (96.9%); NEVER_FIRED 0; HARD_NEGATIVE_ACTED 0; PROVIDER_ERRORS 0; JEV_ERRORS 4 (router timeouts, cases still passed); cost $0.0807 over 115 model calls; scored 2026-10-12T08:37:27 | INTENT_BAR_OK. Merged tree (feat/updates 314f528): phone_location 19/20; TOTAL 30/32 (93.8%); NEVER_FIRED 0; HARD_NEGATIVE_ACTED 0; PROVIDER_ERRORS 0; JEV_ERRORS 5 (timeouts); misses blind-loc-13 "drop my coordinates" -> forget_preview (ambiguous; a preview, nothing deleted) and blind-loc-24 rain-here -> phone_location>maps weather (right act, label expected web_search); $0.0808; scored 2026-10-12T08:47:07 | INTENT_BAR_OK

- [x] G13: Science search (owner, 2026-10-12: "allow me to query [Ask science] on eve as well"): the science_search tool's client, validation, untrusted-text handling and registration in chat, app chat and voice pass their tests.
  CHECK: cd bridge && .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_science_search.py tests/test_tool_gate.py tests/test_intent_eval.py && echo SCIENCE_TESTS_OK
  EXPECT: SCIENCE_TESTS_OK
  EVIDENCE: exit=0; main after merging feat/science-search (8584d37); output=271 passed, 1 skipped | SCIENCE_TESTS_OK; full suite 6668 passed, 24 skipped, 2 xfailed; ruff All checks passed!; diff vs origin/main DIFF_CLEAN

- [x] G14: science_search is a prompt change (AGENTS.md rule 6): the intent eval, real chat model and production routing, on a BLIND set (tests/fixtures/intent_science_blind.json, written by a separate agent from a brief, without the code or the tool text): science_search >= 90% of its cases, no "never" label fires (everyday web questions must not go to science_search), no hard negative acts, no provider errors. Bar fixed before the set exists.
  CHECK: cd bridge && env $(grep -E '^(OPENROUTER_API_KEY)=' ~/.config/eve-bridge/env | xargs) .venv/bin/python tools/intent_eval.py tests/fixtures/intent_science_blind.json --require science_search=90
  EXPECT: INTENT_BAR_OK
  EVIDENCE: exit=0; same tree; blind set frozen in f60380e before the first run; output=science_search 21/22; TOTAL 33/34 (97.1%); NEVER_FIRED 0; HARD_NEGATIVE_ACTED 0; PROVIDER_ERRORS 0; JEV_ERRORS 4 (timeouts); miss blind-sci-20 -> web_search; $0.1279; scored 2026-10-12T09:00:03 | INTENT_BAR_OK

- [x] G9: On the owner's iPhone the app opens on Chat, Updates sits left of it, and tapping a card sends its sentence into chat.
  MANUAL: the owner's word after `sh ios/EveLink/build.sh --install`. Also: iOS asked for location, and asking eve "where am I" answers right.
  EVIDENCE: manual, 2026-10-12: the owner, after the install: "it works on my phone, push".
