# Gates: routine computer access

Scope: Reduce routine access prompts in Eve chat and call tasks while preserving consequential confirmation and runtime restrictions.

- [x] G1: Routine app, site and download grants avoid questions; consequential, strict, unknown and stale requests do not inherit them; chat uses the same policy.
  CHECK: bridge/.venv/bin/python -m pytest -q -p no:cacheprovider bridge/tests/test_codex_permissions.py bridge/tests/test_codex_computer.py bridge/tests/test_codex_chat.py bridge/tests/test_codex_warm.py bridge/tests/test_phone_call.py
  EXPECT: /[0-9]+ passed/
  EVIDENCE: exit=0; shell=/bin/sh; cwd=~/Documents/personal/eve; path=91a292f95075/16 entries; output=.............................................................            [100%] | 133 passed in 1.54s

- [x] G2: Changed Python files pass lint.
  CHECK: bridge/.venv/bin/ruff check bridge/src/eve_bridge/codex_computer.py bridge/src/eve_bridge/codex_chat.py bridge/tests/test_codex_permissions.py
  EXPECT: All checks passed!
  EVIDENCE: exit=0; shell=/bin/sh; cwd=~/Documents/personal/eve; path=91a292f95075/16 entries; output=All checks passed!

- [x] G3: Documentation matches implemented permissions and activation requirements.
  EVIDENCE: Reviewed README.md and docs/codex-computer-use/README.md against routine_access, browser_origin and both thread instruction strings. Both document automatic app/site/download grants, exact Telegram upload origin, opt-out and restart requirements. Consequential click classification is explicitly an agent instruction; unknown/strict permissions remain prompts. Initial sandbox run failed 13 local-socket call tests; the same checks with socket access passed all 133. No live UI completion is claimed.
