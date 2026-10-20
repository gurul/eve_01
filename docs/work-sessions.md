# Work sessions: Claude Code in ~/Documents/personal/work, from the phone

Owner, 2026-10-12: "can work essentially be a new terminal session in documents
personal", "since its long horizon", "that would be optimal", "make a folder
called work under personal where this happens", and "plus access to all the
tools and things eve already has".

The Work tab in Eve Link lists the Mac's Claude Code sessions. **+** starts a
new one: a Terminal.app window on the Mac running plain
`claude --dangerously-skip-permissions` in `~/Documents/personal/work`, with
the typed task as its first prompt. A session's sheet shows its latest turns,
takes a reply, and can stop it. Each session also has eve's own tools (mail,
calendar, contacts, memory, notes, lights, music, TV, camera, watches,
location, research search, sharing...) through an MCP server, and every
acting tool asks the owner on the phone exactly as it does in chat.

Sessions eve did not start (one in Warp, an IDE or another terminal, in any
project) show their turns too, and can be **continued** from the phone: see
[Continue: jump into any session](#continue-jump-into-any-session).

Gates and their checks: [work-sessions-gates.md](work-sessions-gates.md).

## The folder, and what it is not

- The folder is `~/Documents/personal/work` (`EVE_WORK_ROOT` moves it; the
  tests use a temporary one). The route's answers carry only its basename,
  `work`.
- It is **not** `claude_launch`'s *area* "work" (`~/Documents/work`, which
  opens `era-code claude`). "new claude" in chat is unchanged.
- Work sessions always open in **Terminal.app**, whatever
  `EVE_CLAUDE_TERMINAL` says for "new claude" (Warp stays its default). Warp
  has no AppleScript way to type into one particular tab, and replies need one.
- `EVE_WORK_SESSIONS=0` turns the routes off (503 "Work sessions are off on
  this Mac"). They are on by default whenever the Mini App runs.

## Starting a session (the task never touches a shell)

`work_sessions.WorkSessions.start(task, client_id)`:

1. The task is 1 to 4000 characters after trimming (400 otherwise).
2. A session id is minted (a UUID) and a fresh eve MCP token (32 random
   bytes). The task goes to `~/.config/eve-bridge/work/<id>.task` and eve's MCP
   config to `<id>.mcp.json`, both 0600 in a 0700 folder. The registry,
   `~/.config/eve-bridge/work-sessions.json` (0600), keeps the id, the title
   (the task's first line, 80 characters), the Terminal window id and tab tty,
   the client id and the token's SHA-256, never the token.
3. One Terminal window opens. Its only line is made of code-made words:
   `cd '<work folder>' && exec '<python>' -P -m eve_bridge.work_sessions launch <id>`.
   It reaches osascript as an argument (`on run argv`), never inside the
   script's source. `-P` keeps the work folder off `sys.path`: sessions
   write files there, and a `json.py` or `uuid.py` left at its root must not
   shadow the standard library or run in eve's interpreter.
4. `launch` reads and deletes the task file and replaces itself (`execvp`)
   with
   `claude --dangerously-skip-permissions --mcp-config <id>.mcp.json --session-id <id> -- <task>`.
   The task is one argv element after `--`, so even a task that starts with a
   dash is the first prompt, not a flag. `--mcp-config` takes several values,
   so `--session-id` follows it.

The same `client_id` within 10 minutes answers the first start's session and
opens nothing. If Terminal does not open, the answer is 503 and the files and
the record are removed.

`tests/test_work_sessions.py` starts sessions with hostile tasks (quotes,
`$(...)`, backticks, AppleScript `end tell` lines, leading dashes) and checks
that no word of them is in the shell line, that the script is the fixed
source, and that the task arrives whole as claude's last argv element.

## Telling sessions apart

Every Work session shares one folder, so the folder cannot name one. The id
can: `--session-id` makes Claude Code's own session id the one eve minted, so

- the hooks' `session_id` (SessionStart, UserPromptSubmit, Stop,
  Notification, PreToolUse, PostToolUse) names the session. The daemon passes
  each event to `WorkSessions.note`, which keeps the state exactly (no 15 s
  Stop deferral);
- `ps -axww -o pid=,args=` shows `--session-id <id>` on the claude process
  (`session_pids`, which reads only the words before `--`, so a task cannot
  name another session). That is liveness and the stop target.

Known limit: `/clear` inside a session goes on under a new Claude Code
session id in the same process. The Work row stays (ps still shows the id it
started with) and reply and stop still reach it, but its state and detail
stop following, and the new id is listed beside it as a session eve did not
start (it can be continued, below).

A session's state: **waiting** (a Notification that waits on the human, or an
`AskUserQuestion` call), **working** (between a prompt and Stop), **idle**,
or **starting** (opened, claude not up yet; for 30 s at most).

## The routes (link_chat.py)

Every route has the link token and Origin refusals of every `/api/link` route
(401 without the token, 403 with any Origin), 404 for an unknown path, 405
for the wrong method, and 503 while Work sessions are off.

| Call | Answer |
|---|---|
| `GET /api/link/sessions` | `{"ok": true, "folder": "work", "sessions": [S]}`, newest activity first, at most 30. S: `{"id", "folder", "title", "state": "starting"\|"working"\|"waiting"\|"idle", "last_activity" (epoch s), "last_line" (Claude's newest text, one line, 200 characters), "from_work", "can_reply", "can_stop", "can_continue"}`. Every live session the daemon knows is listed; only Work sessions (`from_work: true`) can be replied to or stopped. A Work session whose claude process is gone (after its 30 s grace) leaves the list and is marked ended; `starting` only within that grace. |
| `POST /api/link/sessions/start` | `{"task": 1..4000 chars, "client_id"?: ≤ 64}` → `{"ok": true, "session": S}` with state `starting`. 400 for a bad body, 503 `{"ok": false, "reason"}` when Terminal does not open. |
| `GET /api/link/sessions/<id>` | `{"ok": true, "session": S, "turns": [{"role": "owner"\|"claude", "text", "at"}], "cut": bool, "ended": bool}`; an ended or stopped Work session has S's state `"ended"` and `"ended": true`, and so does one whose claude process is gone after its 30 s grace (detail marks it ended, as the list does). The last 20 turns from the transcript's last 512 kB. Only the owner's prompts and Claude's text: no tool calls, tool results, thinking, subagents' lines or command wrappers. Each turn is clipped to 2000 characters, and token-shaped strings and `KEY=value` secrets are masked as `[hidden]`. 404 for an unknown id. |
| `POST /api/link/sessions/<id>/reply` | `{"text": 1..4000}` → `{"ok": true}`. The text is flattened to one line (a newline would submit early) and typed into that session's own tab (below). 409 "that session has ended" (with `"code": "ended"`, which the app matches, never the words), "can't reach that session's window", or "eve can only reply to or stop sessions it started from Work"; 404 for an unknown id. |
| `POST /api/link/sessions/<id>/stop` | `{"ok": true, "stopped": true}`, also for one already gone. 409 for a session eve did not start. |
| `POST /api/link/sessions/<id>/continue` | `{"text": 1..4000, "client_id"?: ≤ 64}` → `{"ok": true, "session": S}`: a new Work session that forks `<id>` (below). 409 "that session takes replies as it is" for a live Work session (or when ps cannot tell), 409 "eve can't find that session's folder or transcript" when there is nothing to fork, 404 for an unknown id. The same `client_id` within 10 minutes answers the first fork. |

The transcript is SessionStart's `transcript_path`, or else Claude Code's own
place for it: `<config home>/projects/<folder, every character that is not a
letter or digit as "-">/<id>.jsonl`, or else `<id>.jsonl` in any project
folder. A session the daemon adopted from a tool hook after a restart has no
`transcript_path`; before 2026-10-20 its sheet said "Nothing yet."

## Continue: jump into any session

Owner, 2026-10-20: "i want to be able to see the content of sessions i didnt
start and just jump in and continue from mobile if i am working on multiple
projects".

A session that started outside eve cannot be typed into: Warp has no
AppleScript tab addressing, and an IDE's terminal has none either. So
continuing **forks** it. `continue` opens a new Work session in the
original's folder (not the work folder: the first `cwd` its transcript names,
since a hook's `cwd` is wherever the session's shell has moved to; the list's
folder name comes from there too) running

    claude --dangerously-skip-permissions --mcp-config <file> \
           --resume <id> --fork-session --session-id <new> -- <text>

The fork has the whole conversation under a new id, which `ps` names, so
reply, stop and the state chips work on it as on any Work session. The
original session and its transcript are left as they were. If it is still
running at the desk, the two run side by side in one folder, so the sheet
says so before the owner sends. The original's id waits in `<new>.resume` (0600)
beside the task file; `launch` reads it once, checks it is a UUID, and deletes
it. A Work session that has ended can be continued the same way, in the work
folder.

`can_continue` is true for a session eve did not start whose folder and
transcript are both there, and for an ended Work session with a transcript.
The sheet shows a "Continue from here" box in place of the read-only note,
and follows the fork once it opens.

## Reply: that session's tab, only while it is live

Terminal.app's `do script <text> in <tab>` types into one tab. The tab is
found by the window id and tty kept at start, and the text is an osascript
argument. It is refused unless

- `ps` finds the session's claude (`--session-id <id>`); a probe that fails
  refuses too, and
- the tab is busy: a program holds it, not a shell prompt. Because the start
  line `exec`s, no shell is left in the tab once claude exits, so a reply can
  never become a shell command.

No other window is touched and nothing needs focus. The relay's "claude on"
path (keystrokes into the frontmost window) is not used, and its queue
(`_relay_prompts`, verification/Eve/CallScope.lean) is untouched.

**Not verified without a live run:** how Claude Code's prompt takes the line
`do script` types (that the trailing newline submits it). W4
(`bridge/tools/work_sessions_check.py`) replies and waits for the answer.

## Stop

SIGTERM to the pid `ps` gives for `--session-id <id>`, then SIGKILL after 5 s
if it is still there. The record is marked stopped, its MCP config is deleted
(so its token stops working), and the daemon forgets the session at once.
Another claude process is never signalled. The Terminal window is left as
Terminal's profile leaves a finished process (open, or closed).

## eve's tools in a session (eve_mcp.py)

The session's MCP config names one stdio server, `eve`:
`<python> -P -m eve_bridge.eve_mcp` (`-P`: Claude Code starts it in the work folder, which stays off `sys.path`; also `eve-bridge mcp`, which starts before
the env file is read, so the server holds none of eve's keys), with
`EVE_MCP_SESSION` and `EVE_MCP_TOKEN` in its environment. It holds no tools
of its own. It speaks JSON-RPC 2.0 over stdio (initialize, ping, tools/list,
tools/call; written by hand, as `apps_mcp.py` writes its client) and asks the
running daemon over its 0600 local socket (the hooks' transport):

- `mcp_tools {sid, token}` → `TelegramInlet.session_tools()`: the chat brain's
  own lists, built by the same code (`_tool_lists`, which `_turn_parts` also
  uses), as an unrouted turn with every up direct app opened offers them. Less
  `SESSION_HIDDEN`: the relays (`claude_relay`, `codex_relay`),
  `start_coding_session`, `set_stealth`, `pair_eve_link`, the Mac's computer
  tasks (`start_task`, `steer_task`, `stop_task`) and the / menu's hand-offs
  (`menu_tools.HANDS_OFF`), which steer eve's own chat. The hosted web search
  has no MCP form and is left out; the function form is kept. A tool that
  comes or goes for the chat comes or goes on the next tools/list.
- `mcp_call {sid, token, name, args}` → `TelegramInlet.session_call()`:
  - a name the session was not offered is refused, and nothing runs;
  - the session counts as having read outside text (`SESSION_READ`, "what a
    Claude Code session wrote"), so `tool_gate.needs_confirmation` asks for
    deletions always and for recording, joining and answering too;
  - `_gate` asks on the phone (firm Yes / No, titled "Check first (from Claude
    in Work)") and the call waits for the answer, up to the chat's
    `ask_timeout_secs` (180 s);
  - then `_tool`, where the apps' own policies ask for consequential writes
    (Gmail shows the whole email), purchases and shares, as in chat;
  - a question nobody answers ends as "the owner did not answer, so it did
    not run";
  - the chat turn's own taint (`_taint`) is never read or written.
- The daemon checks the token against the registry's hash in constant time,
  and that the session has not ended or been stopped, before anything else.
- The daemon bounds the whole call: within the ask timeout plus 90 s it
  answers. A call still running then is not cancelled (an approved purchase
  or send may be half done); it goes on, and the session is told the outcome
  is unknown and not to retry it.
- The MCP side waits longer (`call_secs`: the ask timeout plus 120 s). If the
  daemon never answers, closing the socket cancels nothing there, so it says
  the outcome is unknown and not to retry, never "it did not run". The same
  holds when the connection breaks after the request went out (a reset, a
  closed socket, half an answer). "eve isn't running on this Mac" only when
  the socket is not there or the request never went out. Arguments over 48 kB never leave.
- **Outside text stays marked.** A result whose words come from outside eve
  (`tool_gate.taints`: mail, the web, memory, notes, a file...) comes back
  with a first text block naming its source and saying it is data, not
  instructions.

Known side effect: a direct app a session's call names is opened for the
chat's next turn too (`_direct_tool`'s `ensure_open`), as the chat's own call
would.

## Logs

Ids' first 8 characters, counts, states and tool names only: never a task, a
reply, a transcript line, a token or a tool's arguments (AGENTS.md rule 10).

## Files

| File | What |
|---|---|
| `bridge/src/eve_bridge/work_sessions.py` | the registry, start / list / detail / reply / stop / continue, `launch` |
| `bridge/src/eve_bridge/eve_mcp.py` | the stdio MCP server |
| `bridge/src/eve_bridge/link_chat.py` | the `/api/link/sessions*` routes |
| `bridge/src/eve_bridge/daemon.py` | `_work`, the hooks' `_work_note`, IPC `mcp_tools` and `mcp_call` |
| `bridge/src/eve_bridge/telegram.py` | `_tool_lists`, `session_tools`, `session_call`, `_gate(read=)` |
| `bridge/tests/test_work_sessions.py`, `test_eve_mcp.py` | the tests |
| `bridge/tools/work_sessions_check.py` | W4, live: start, list, detail, reply, stop; prints counts and states only |
