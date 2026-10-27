# Gates: the Work tab is Claude Code sessions in ~/Documents/personal/work

OWNS: bridge/src/eve_bridge/eve_mcp.py (new), bridge/tests/test_eve_mcp.py (new), the session routes in bridge/src/eve_bridge/link_chat.py, bridge/src/eve_bridge/work_sessions.py (new), claude_launch.py / claude_live.py / the relay only where the routes need a seam, bridge/tests/test_work_sessions.py (new), ios/EveLink/App/WorkView.swift (the Work half only), ios/EveLink/App/SessionSheet.swift (new), ios/EveLink/Shared/WorkSessions.swift (new), ios/EveLink/Tests/**, docs/work-sessions.md (new), docs/app-chat.md, README.md, docs/work-sessions-gates.md

Scope: the owner, 2026-10-12: "can work essentially be a new terminal session in documents personal", "since its long horizon", "that would be optimal", "make a folder called work under personal where this happens" (created 2026-10-12). Careful: claude_launch's *area* "work" is ~/Documents/work with era-code; these sessions are plain `claude` in ~/Documents/personal/work, never the work area. eve already opens `claude` in a folder under ~/Documents from the phone ("new claude", claude_launch.py: a Warp window, --dangerously-skip-permissions for personal), knows live sessions (claude_live.py, the hooks) and relays to them ("claude on"). So the Work tab lists those sessions; + New opens a fresh terminal session running claude in ~/Documents/personal/work with the typed task as its first prompt; a session's sheet shows its latest turns, takes a reply, and can stop it.

- [ ] W1: The routes and the session model pass their tests: list (live sessions with folder, state working / waiting on you / idle, last activity, last line), start (one new claude in ~/Documents/personal/work with the task as first prompt, nothing else on the Mac), detail (the latest turns from the transcript, clipped), reply (reaches only that session), stop; link token and Origin refusal as every /api/link route; a task is never interpolated into a shell command.
  CHECK: cd bridge && .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_work_sessions.py tests/test_link_chat.py && echo WORK_TESTS_OK
  EXPECT: WORK_TESTS_OK

- [ ] W2: The full bridge suite and lint pass.
  CHECK: bridge/.venv/bin/python -m pytest -q -p no:cacheprovider -n auto bridge/tests && (cd bridge && uv run ruff check src tests tools) && echo BRIDGE_OK
  EXPECT: BRIDGE_OK

- [ ] W3: The app's tests and the iPhone build pass (session decoding, states, the start and reply requests).
  CHECK: sh ios/EveLink/Tests/run.sh && sh ios/EveLink/build.sh && echo APP_OK
  EXPECT: APP_OK

- [x] W4: Live on this Mac: a session started through the route opens a terminal running claude in ~/Documents/personal/work, shows up in the list within 15 s, and its reply appears in detail; the check stops it after. Prints counts and states only.
  CHECK: bridge/.venv/bin/python bridge/tools/work_sessions_check.py
  EXPECT: WORK_LIVE_OK
  EVIDENCE: exit=0, 2026-10-12, main a32872d (daemon restarted on it): start 200 in 2.1 s; listed working after 3.4 s in personal/work; claude process found by session id in personal/work; detail 1+1 turns after 5.7 s; reply 200, detail 2+2 turns; stop 200, gone from the list | WORK_LIVE_OK

- [ ] W7: Each Work session has eve's tools (owner, 2026-10-12: "plus access to all the tools and things eve already has"): an eve MCP server the session loads offers the same tools the chat brain has (mail, calendar, contacts, memory, notes, location, science, lights, music, camera, watches, share...), each call runs through the same tool gate, and an acting tool (send, buy, delete...) asks the owner on the phone exactly as in chat; a session cannot reach a tool the brain could not.
  CHECK: cd bridge && .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_eve_mcp.py && echo EVE_MCP_OK
  EXPECT: EVE_MCP_OK

- [ ] W5: No personal data or token-shaped strings in the diff.
  CHECK: git diff main | grep -v 'CHECK: git diff main' | python3 -c "import re,sys; d=sys.stdin.read(); bad=[p for p in (r'guru\.lingamallu', r'@gmail\.com', r'guruchara[n]', r'Bearer [A-Za-z0-9_-]{20,}', r'sk-[A-Za-z0-9]{20,}', r'/Users/') if re.search(p, d)]; print(bad or 'DIFF_CLEAN'); sys.exit(1 if bad else 0)"
  EXPECT: DIFF_CLEAN

- [ ] W6: From the phone, the owner starts a session from Work, watches it work, replies, and it carries on.
  MANUAL: the owner's word.

## Unified Work (2026-10-26)

Scope: the owner, 2026-10-26: "I want to also be able to talk to current terminal sessions through the work area ... Unify everything. This is important." Continue forked live sessions into new Terminal windows (two agents on one repository, twice), and replies to eve's own sessions failed as "not typed (gone)" because the recorded window id was wrong and ttys are reused. The design: one way to talk to any live session (its Claude Code messaging socket, `claude_peer.send`); typing by the claude process's live tty only for eve's own Terminal sessions with no usable socket; an ended session resumes as itself (`claude --resume <id>`, never `--fork-session`); several sends in a row all go through, in order. Details: [work-sessions.md](work-sessions.md).

- [ ] U1: The unified model passes its tests (fakes for ps, osascript, kill, the registry and the send): a Warp session gets the words through its socket (now, queued while busy); held, refused, gone, unsupported and error are what the phone is told; an eve session with no socket is typed into by its live tty even with a stale window id; three quick sends go out in order; a send waits for a session still starting; continue to a live session is a reply and opens nothing; an ended session reopens with `--resume <id>` and no fork, under the same id; continue is refused when eve cannot tell (an unregistered claude, a failed ps or registry); an eve session the owner resumed at the Mac is sent to, never reopened or stopped; a look never ends a session reopened while it ran; relayed words and SendMessage answers to eve are turns, other sessions' messages are not; `named_session` reads only the words before `--`; `where` reads the app from the ancestors; the daemon runs one inbox, adds its address, closes it, and logs no text.
  CHECK: cd bridge && .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_work_sessions.py tests/test_link_chat.py && echo UNIFY_TESTS_OK
  EXPECT: UNIFY_TESTS_OK

- [x] U2: Live on this Mac, with throwaway sessions started for the check and stopped after (never the owner's): words reach a live session through its socket (delivered now); two sends while it worked were both taken (queued); a session in `--dangerously-skip-permissions` held the message ("held"); its SendMessage answer reached eve's inbox and showed in the sheet's turns; Claude Code filed a message taken mid-turn as one meta user line (no duplicate turn) and a held one as a system line only.
  MANUAL: run 2026-10-26 by the leaf that built it, before the pass-3 fixes to ending and stopping (the send path did not change after).
  EVIDENCE: kept only in that build session's transcript (counts and states); not re-run since.

- [x] U3: Live: `claude --resume <id>` (no `--fork-session`) keeps the session's id and its transcript; `session_pids` names the resumed process; `where` says Terminal for it.
  MANUAL: run 2026-10-26 on a throwaway session, stopped after. Re-run the same day by the review, through this code: a throwaway started and stopped through the live daemon, then `continue_session` on it (the real Terminal window and `resume` launch): 200 "resumed" under the same id; ps showed `--resume <id>` with no `--fork-session` or `--session-id`; the registry listed it; `where` said Terminal; the same transcript grew by 17 lines with the prompt and an answer, every new line under the same id, and no new transcript held the prompt; a reply to it answered "held"; `stop` ended it, and its files in eve's work folder were gone.

- [x] U4: Live: the typing script, given a window id that is wrong, finds the busy tab by the claude process's tty in another window and types into it.
  MANUAL: run 2026-10-26 on a throwaway Terminal window, closed after.

- [ ] U5: W4's live check still passes. It replies to an eve Work session and waits for the answer; such a session runs with `--dangerously-skip-permissions`, which holds eve's messages until the owner approves on the Mac (U2), so it is expected to wait until someone approves there.
  CHECK: bridge/.venv/bin/python bridge/tools/work_sessions_check.py
  EXPECT: WORK_LIVE_OK

- [ ] U6: From the phone, the owner talks to a session running in Warp, sends three messages in a row to one session, and resumes one that has ended; each lands once, in order, and nothing forks.
  MANUAL: the owner's word.
