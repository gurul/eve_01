# Gates: remembered Codex Computer Use permissions

Scope: Allow the owner to remember specific Computer Use grants, with matching and revocation that preserve Codex's permission requirements.

- [x] P1: Installed permission metadata and official documentation establish how a grant is scoped and remembered.
  EVIDENCE: Official Computer Use/managed config docs, generated McpServerElicitationRequestResponse schema, installed native policy wrapper and desktop approval card confirm response _meta.persist session/always, exact native app identity and managed restrictions. Real metadata-only Calculator probe observed both scopes and declined without altering grants.

- [x] P2: Owner-selected remembered grants apply only to the approved scope; unknown requests and one-time denials remain unapproved, and Codex owns grant persistence and Settings revocation.
  CHECK: bridge/.venv/bin/python -m pytest -q -p no:cacheprovider bridge/tests/test_codex_permissions.py
  EXPECT: passed
  EVIDENCE: exit=0; shell=/bin/sh; cwd=/Users/gurucharan/Documents/personal/eve; path=5cdd84c66b04/21 entries; output=....................                                                     [100%] | 20 passed in 0.03s

- [x] P3: Adapter and Telegram regression checks pass and changed code passes lint.
  CHECK: bridge/.venv/bin/ruff check bridge/src/eve_bridge/codex_computer.py bridge/tests/test_codex_permissions.py && bridge/.venv/bin/python -m pytest -q -p no:cacheprovider bridge/tests/test_codex_computer.py bridge/tests/test_telegram.py bridge/tests/test_voice_agent.py bridge/tests/test_daemon_task_cancel.py
  EXPECT: passed
  EVIDENCE: exit=0; shell=/bin/sh; cwd=/Users/gurucharan/Documents/personal/eve; path=5cdd84c66b04/21 entries; output=..                                                                       [100%] | 146 passed in 24.88s

- [x] P4: Documentation explains scopes and native revocation, and the running Eve instance loads the change.
  EVIDENCE: README and Telegram/integration docs cover exact commands and Settings revocation. permission-smoke.json records one native Calculator approval with always and zero requests in a separate fresh session, both with UI value 4. Eve restarted idle at 2026-09-22 19:54 PDT; status socket healthy, Codex ready and Telegram listening.
