# Gates: Eve browser permissions and pictures

OWNS: bridge/src/eve_bridge/codex_chat.py, bridge/src/eve_bridge/codex_computer.py, bridge/src/eve_bridge/codex_relay.py, bridge/src/eve_bridge/telegram.py, bridge/tests/test_codex_chat.py, bridge/tests/test_codex_permissions.py, bridge/tests/test_codex_computer.py, bridge/tests/test_codex_relay.py, bridge/tests/test_telegram.py, docs/codex-computer-use/README.md, docs/stackchan/telegram.md, README.md, docs/browser-delivery/**

Scope: Honor ordinary website access, send the correct browser picture, and start a fresh Codex chat in an accessible saved folder from Telegram. The owner's later request replaces existing-task selection.

- [x] G6: Fresh sessions retain context across turns, relay questions and cancellation, and reject stale or failed transports without retrying work.
  CHECK: bridge/.venv/bin/python -m pytest -q -p no:cacheprovider bridge/tests/test_codex_chat.py bridge/tests/test_telegram.py
  EXPECT: passed
  EVIDENCE: exit=0; shell=/bin/sh; cwd=/Users/gurucharan/Documents/personal/eve; path=9b77915ca749/21 entries; output=.......................................................................  [100%] | 71 passed in 0.86s

- [x] G7: Verify folder-only listing and a fresh chat with two harmless turns through the production Telegram handler against the installed Codex runtime, then restart the daemon and verify its health.
  EVIDENCE: Live production dispatch started new thread 01a0cdbb-e631-7993-8cbb-2fe4ec544ebf in the Eve cwd, returned READY and the earlier token cobalt-pine-73 on a follow-up, and closed on codex off. FRESH_CODEX_CHAT_LIVE_OK, exit=0. Harmless test thread archived. Daemon restarted; launchctl running PID 33006, exit=0, Telegram listening at 03:09:35. See verification.md.

- [x] G1: Ordinary site-access handling is covered alongside native-app, unknown-form and consequential-action controls.
  CHECK: bridge/.venv/bin/python -m pytest -q -p no:cacheprovider bridge/tests/test_codex_permissions.py
  EXPECT: passed
  EVIDENCE: exit=0; shell=/bin/sh; cwd=/Users/gurucharan/Documents/personal/eve; path=9b77915ca749/21 entries; output=............................................                             [100%] | 44 passed in 0.06s

- [x] G2: Codex browser images reach Telegram from the task result, with unrelated desktop capture excluded and invalid/missing images handled honestly.
  CHECK: bridge/.venv/bin/python -m pytest -q -p no:cacheprovider bridge/tests/test_codex_computer.py bridge/tests/test_telegram.py bridge/tests/test_telegram_images.py
  EXPECT: passed
  EVIDENCE: exit=0; shell=/bin/sh; cwd=/Users/gurucharan/Documents/personal/eve; path=9b77915ca749/21 entries; output=............                                                             [100%] | 84 passed in 0.78s

- [x] G3: Changed Python passes lint and the patch has no whitespace errors.
  CHECK: bridge/.venv/bin/ruff check bridge/src/eve_bridge/codex_chat.py bridge/src/eve_bridge/codex_computer.py bridge/src/eve_bridge/codex_relay.py bridge/src/eve_bridge/telegram.py bridge/tests/test_codex_chat.py bridge/tests/test_codex_permissions.py bridge/tests/test_codex_computer.py bridge/tests/test_codex_relay.py bridge/tests/test_telegram.py && git diff --check
  EXPECT: All checks passed
  EVIDENCE: exit=0; shell=/bin/sh; cwd=/Users/gurucharan/Documents/personal/eve; path=9b77915ca749/21 entries; output=All checks passed!

- [x] G4: Document observed Telegram behavior and verify the real browser protocol and picture source, or explicitly identify a runtime limitation.
  EVIDENCE: 2026-09-23 live Chrome task accepted access_browser_origin without an ask callback, observed Example Domain and captured tab 229895500 as a valid JPEG. Production _send_screen returned task_browser and Telegram delivered it; owner confirmed "browser tab looks good" and "Just saw in telegram". Built-in iab unavailable in standalone session, documented in verification.md. Site preference enabled; launchctl reports daemon PID 28282 running, Telegram listening at 02:53:18. Folder eve resolved against live catalog. Documentation checker TELEGRAM_DOCS_OK, exit 0.

- [x] G5: Telegram lists accessible saved folders only, starts a new chat on each folder selection, and returns to Eve after failed startup; ambiguous or unknown folders cannot start elsewhere.
  CHECK: bridge/.venv/bin/python -m pytest -q -p no:cacheprovider bridge/tests/test_codex_chat.py bridge/tests/test_telegram.py
  EXPECT: passed
  EVIDENCE: exit=0; shell=/bin/sh; cwd=/Users/gurucharan/Documents/personal/eve; path=9b77915ca749/21 entries; output=.......................................................................  [100%] | 71 passed in 0.76s
