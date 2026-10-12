# Gates: the Work tab is Claude Code sessions in ~/Documents/personal

OWNS: bridge/src/eve_bridge/eve_mcp.py (new), bridge/tests/test_eve_mcp.py (new), the session routes in bridge/src/eve_bridge/link_chat.py, bridge/src/eve_bridge/work_sessions.py (new), claude_launch.py / claude_live.py / the relay only where the routes need a seam, bridge/tests/test_work_sessions.py (new), ios/EveLink/App/WorkView.swift (the Work half only), ios/EveLink/App/SessionSheet.swift (new), ios/EveLink/Shared/WorkSessions.swift (new), ios/EveLink/Tests/**, docs/work-sessions.md (new), docs/app-chat.md, README.md, docs/work-sessions-gates.md

Scope: the owner, 2026-10-12: "can work essentially be a new terminal session in documents personal", "since its long horizon", "that would be optimal". eve already opens `claude` in a folder under ~/Documents from the phone ("new claude", claude_launch.py: a Warp window, --dangerously-skip-permissions for personal), knows live sessions (claude_live.py, the hooks) and relays to them ("claude on"). So the Work tab lists those sessions; + New opens a fresh terminal session running claude in ~/Documents/personal with the typed task as its first prompt; a session's sheet shows its latest turns, takes a reply, and can stop it.

- [ ] W1: The routes and the session model pass their tests: list (live sessions with folder, state working / waiting on you / idle, last activity, last line), start (one new claude in ~/Documents/personal with the task as first prompt, nothing else on the Mac), detail (the latest turns from the transcript, clipped), reply (reaches only that session), stop; link token and Origin refusal as every /api/link route; a task is never interpolated into a shell command.
  CHECK: cd bridge && .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_work_sessions.py tests/test_link_chat.py && echo WORK_TESTS_OK
  EXPECT: WORK_TESTS_OK

- [ ] W2: The full bridge suite and lint pass.
  CHECK: bridge/.venv/bin/python -m pytest -q -p no:cacheprovider -n auto bridge/tests && (cd bridge && uv run ruff check src tests tools) && echo BRIDGE_OK
  EXPECT: BRIDGE_OK

- [ ] W3: The app's tests and the iPhone build pass (session decoding, states, the start and reply requests).
  CHECK: sh ios/EveLink/Tests/run.sh && sh ios/EveLink/build.sh && echo APP_OK
  EXPECT: APP_OK

- [ ] W4: Live on this Mac: a session started through the route opens a terminal running claude in ~/Documents/personal, shows up in the list within 15 s, and its reply appears in detail; the check stops it after. Prints counts and states only.
  CHECK: bridge/.venv/bin/python bridge/tools/work_sessions_check.py
  EXPECT: WORK_LIVE_OK

- [ ] W7: Each Work session has eve's tools (owner, 2026-10-12: "plus access to all the tools and things eve already has"): an eve MCP server the session loads offers the same tools the chat brain has (mail, calendar, contacts, memory, notes, location, science, lights, music, camera, watches, share...), each call runs through the same tool gate, and an acting tool (send, buy, delete...) asks the owner on the phone exactly as in chat; a session cannot reach a tool the brain could not.
  CHECK: cd bridge && .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_eve_mcp.py && echo EVE_MCP_OK
  EXPECT: EVE_MCP_OK

- [ ] W5: No personal data or token-shaped strings in the diff.
  CHECK: git diff main | grep -v 'CHECK: git diff main' | python3 -c "import re,sys; d=sys.stdin.read(); bad=[p for p in (r'guru\.lingamallu', r'@gmail\.com', r'guruchara[n]', r'Bearer [A-Za-z0-9_-]{20,}', r'sk-[A-Za-z0-9]{20,}', r'/Users/') if re.search(p, d)]; print(bad or 'DIFF_CLEAN'); sys.exit(1 if bad else 0)"
  EXPECT: DIFF_CLEAN

- [ ] W6: From the phone, the owner starts a session from Work, watches it work, replies, and it carries on.
  MANUAL: the owner's word.
