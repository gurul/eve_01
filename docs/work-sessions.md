# Work sessions: every Claude Code session on the Mac, from the phone

Owner, 2026-10-12: "can work essentially be a new terminal session in documents
personal", "since its long horizon", "that would be optimal", "make a folder
called work under personal where this happens", and "plus access to all the
tools and things eve already has". Owner, 2026-10-26: "I want to also be able
to talk to current terminal sessions through the work area ... Unify
everything. This is important."

The Work tab in Eve Link lists every live Claude Code session on the Mac,
wherever it runs: Warp, Terminal, an IDE, tmux. **+** starts a new one: a
Terminal.app window running plain `claude --dangerously-skip-permissions` in
`~/Documents/personal/work`, with the typed task as its first prompt. A
session's sheet shows its latest turns and takes the owner's words. There is
one way in for every live session: its own messaging socket. A session that
has ended can be **resumed**: it reopens as itself, the same id and the same
transcript. eve can stop the sessions it started or reopened. Each of those
also has eve's own tools (mail, calendar, contacts, memory, notes, lights,
music, TV, camera, watches, location, research search, sharing...) through an
MCP server, and every acting tool asks the owner on the phone exactly as it
does in chat.

Gates and their checks: [work-sessions-gates.md](work-sessions-gates.md).

## The folder, and what it is not

- The folder is `~/Documents/personal/work` (`EVE_WORK_ROOT` moves it; the
  tests use a temporary one). The route's answers carry only its basename,
  `work`.
- It is **not** `claude_launch`'s *area* "work" (`~/Documents/work`, which
  opens `era-code claude`). "new claude" in chat is unchanged.
- New and resumed sessions open in **Terminal.app**, whatever
  `EVE_CLAUDE_TERMINAL` says for "new claude". Talking to a session needs no
  window at all (below), so a session in Warp or an IDE is reached as easily
  as one in Terminal.
- `EVE_WORK_SESSIONS=0` turns the routes off (503 "Work sessions are off on
  this Mac") and eve's inbox with them. They are on by default whenever the
  Mini App runs.

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

## Which sessions there are, and which is which

The list merges three sources by session id:

- **Claude Code's session registry**, `~/.claude/sessions/<pid>.json`
  (`claude_peer.live_sessions`): one file per live interactive session, with
  its id, folder, name, status (busy, idle, waiting) and, from Claude Code
  versions that have one, its messaging socket.
- **The daemon's hook state** (`state.sessions`): SessionStart,
  UserPromptSubmit, Stop, Notification, PreToolUse and PostToolUse, which
  `WorkSessions.note` also follows exactly for eve's own sessions.
- **eve's own records** (the registry file above).

A one-shot `claude -p` run (a hook's drafter, a script) registers like a
session, but nobody talks to it: when `ps` shows `-p` or `--print` before
`--`, the list leaves it out.

`ps -axww -o pid=,ppid=,tty=,args=`, one call per look, gives the rest:

- the session a claude process runs, from `--session-id <id>` (a new session)
  or `--resume <id>` (one reopened), read only from the words before `--`, so
  a task cannot name another session (`named_session`). A headless `claude -p`
  is never counted, and only the words before `--` say headless: a task like
  "mkdir -p src" leaves its session counted. That is liveness and the stop
  target;
- **where** a session runs: the first `<Name>.app` among the claude process's
  ancestors (Warp, Terminal, iTerm, VS Code, Cursor...), or tmux, screen or
  SSH when one of those comes first; `""` when nothing says;
- the claude process's tty now, which typing needs (below).

Known limit: `/clear` inside a session goes on under a new Claude Code
session id in the same process. An eve row stays (ps still shows the id it
started with) and words still reach it (the registry entry of the same pid),
but its state and detail stop following, and the new id is listed beside it.

A session's state: **waiting** (a Notification that waits on the human, an
`AskUserQuestion` call, or the registry's waiting status), **working**
(between a prompt and Stop, or "busy" in the registry), **idle**, or
**starting** (eve opened it, claude not up yet; for 30 s at most).

### Ended, or not sure

An eve session has ended once `ps` no longer finds its claude after its 30 s
grace. Any other session has ended once the registry no longer lists it, but
only when the registry is **complete**: every claude process `ps` shows is in
it, an IDE's (which runs claude with the SDK's `--output-format` and
`--input-format`) and a one-shot `claude -p` too; only Chrome's native host,
never a session, is left out. An older Claude Code never registers, so a
session it runs would otherwise look ended while it runs, and reopening a
running session is the one thing Resume must not do. When the registry is not complete, eve falls
back to whether any claude runs in the session's folder: if one does, eve
cannot tell, the session stays listed read-only, and Resume is refused. An eve
session that ended can run again (the owner resumed it at the Mac): from then
on it is judged, listed and sent to like any other session, and it is not
eve's to stop.

## The routes (link_chat.py)

Every route has the link token and Origin refusals of every `/api/link` route
(401 without the token, 403 with any Origin), 404 for an unknown path, 405
for the wrong method, and 503 while Work sessions are off. A refusal is
`{"ok": false, "reason", "code"?}`: `reason` is the sentence the phone shows,
and the app matches `code`, never the words.

| Call | Answer |
|---|---|
| `GET /api/link/sessions` | `{"ok": true, "folder": "work", "sessions": [S]}`, newest activity first, at most 30. S: `{"id", "folder", "title", "state": "starting"\|"working"\|"waiting"\|"idle", "where", "last_activity" (epoch s), "last_line" (Claude's newest text, one line, 200 characters), "from_work", "can_reply", "can_stop", "can_continue"}`. `title` is eve's title for its own sessions, else the registry's name for the session, else its folder. `can_reply`: live, and eve can reach it (it has a socket, or it is eve's own Terminal session whose tab can be found). `can_stop`: eve's own. `can_continue` is false for every live session. |
| `POST /api/link/sessions/start` | `{"task": 1..4000 chars, "client_id"?: ≤ 64}` → `{"ok": true, "session": S}` with state `starting`. 400 for a bad body, 503 when Terminal does not open. |
| `GET /api/link/sessions/<id>` | `{"ok": true, "session": S, "turns": [{"role": "owner"\|"claude", "text", "at"}], "cut": bool, "ended": bool}`. An ended session has S's state `"ended"`, `"ended": true` and `can_continue` true when it can be resumed (its folder and transcript are there and Claude is trusted in the folder). A session only its transcript still knows (the daemon forgot it at SessionEnd) answers too, as ended. 404 for an unknown id. Turns: below. |
| `POST /api/link/sessions/<id>/reply` | `{"text": 1..4000}` → `{"ok": true, "delivered": D, "reason"?}`. D is `"now"` (an idle session has it), `"queued"` (a busy one takes it at its next step, one waiting on its user once that is answered), `"held"` (the session holds messages from other programs until the owner allows them on the Mac; `reason` says so) or `"typed"` (typed into eve's own Terminal tab). 409 codes: `ended`, `refused` (the session refuses messages from other programs), `gone` (no longer listening there), `error` (a timeout, a full inbox, too many messages), `no_socket` (its Claude Code cannot take messages and it is not eve's to type into), `starting` (eve's session did not come up within 15 s), `no_window` (its tab cannot be found, or `ps` failed). 404 for an unknown id. |
| `POST /api/link/sessions/<id>/continue` | `{"text": 1..4000, "client_id"?: ≤ 64}`. A live session: the reply's answer plus `"session": S`; nothing opens and nothing forks. An ended one: `{"ok": true, "delivered": "resumed", "session": S}`, S with the same id, state `starting`. 409 codes: `unsure` (eve cannot tell whether it runs), `no_resume` (its folder or transcript is gone), `untrusted` (Claude was never trusted in that folder), and the reply's codes for a live one. 503 when Terminal does not open. 404 for an unknown id. The same `client_id` within 10 minutes answers the first call and sends nothing again. |
| `POST /api/link/sessions/<id>/stop` | `{"ok": true, "stopped": true}`, also for one already stopped or ended. 409 "eve can only stop sessions it started or reopened from Work" for any other session, including one of eve's that the owner has since resumed at the Mac. |

The transcript is SessionStart's `transcript_path`, or else Claude Code's own
place for it: `<config home>/projects/<folder, every character that is not a
letter or digit as "-">/<id>.jsonl`, or else `<id>.jsonl` in any project
folder.

Turns are the last 20 from the transcript's last 512 kB (doubled up to 8 MB
while it has fewer): the owner's typed prompts (also those typed while Claude
worked), the owner's words eve relayed (without eve's envelope and its first
line), Claude's text, and Claude's SendMessage answers to eve. Never tool
calls, tool results, thinking, subagents' lines, command wrappers or other
sessions' messages. Each turn is clipped to 2000 characters, and token-shaped
strings and `KEY=value` secrets are masked as `[hidden]`.

## Talking to a session: its own messaging socket

`reply`, and `continue` on a live session, go through `_deliver`, one way for
every session whatever it runs in:

1. **The socket.** `claude_peer.send` puts the owner's words into the
   session's messaging socket as a cross-session message from `eve-phone`.
   Its first line is always `claude_peer.OWNER_LINE` ("From the owner, typed
   on their phone in eve's Work tab:"); then the words, as typed. eve never
   claims another identity or a permission mode to get past a session's hold
   or refusal. An idle session wakes on the message; a busy one takes it at
   its next tool round.
2. **Typing, only for eve's own Terminal sessions with no usable socket** (an
   older Claude Code, or a send that finds the socket gone).
   Terminal.app's `do script <text> in <tab>` types the words, flattened to
   one line, into the **busy** tab whose tty is the claude process's tty
   **now** (from `ps`): looked for in the window eve recorded first, then in
   every window. The window id is updated when the tab turns up elsewhere.
   Recorded window ids and ttys both went stale on 2026-10-26 ("not typed
   (gone)"), which is why neither is trusted alone. A tab that is not busy is
   passed over, so a reply never becomes a shell command.

Nothing else: a live session is never forked, and no window is opened to talk
to one. A session outside Terminal with no socket answers `no_socket`.

**Several sends in a row** all go through, in the order they came: sends to
one session wait for each other (a lock per session), never refuse. A send to
an eve session still opening waits for its socket for up to 15 s.

**Held.** A session can hold messages from other programs until its user
allows them on the Mac. The phone then says the message waits for approval
on the Mac, and a later approval, refusal or expiry arrives at eve's inbox
(below) and is logged. eve declares itself `from-mode="bypass"` (owner,
2026-10-26: phone messages go straight in), the class of the sessions it
launches, so every session that runs with `--dangerously-skip-permissions`
(eve's own Work sessions and the owner's sessions started that way in Warp
or anywhere) takes them at once. A session that prompts for permission holds
them: Claude Code asks once per message ("Deliver this message to Claude", or
Deny), and an unanswered one expires. Claude Code's `crossSessionInbound`
setting overrides both; eve does not set it. The wire and the rules:
[claude-peer.md](claude-peer.md).

### eve's inbox

While Work is on, the daemon keeps one `claude_peer.Inbox`: eve's own socket,
`uds:/tmp/cc-socks/<daemon pid>.sock`, the `from` address of everything eve
sends. A session that answers the owner with SendMessage reaches it instead
of failing, and so do later words about a held message. eve never acts on
what arrives: a message is logged by its size, and the answer shows in that
session's sheet from the session's own transcript. If the inbox does not
start, one warning says so and Work goes on without it.

## Resume: an ended session reopens as itself

Owner, 2026-10-20: "i want to be able to see the content of sessions i didnt
start and just jump in and continue from mobile if i am working on multiple
projects". Until 2026-10-26 Continue forked even a live session into a new
Terminal window, and a second agent redid the first one's unfinished work in
the same repository, twice.

`continue` on an ended session opens a Terminal window in the folder it
started in (eve's record's folder, else the first `cwd` its transcript names,
since a hook's `cwd` is wherever the session's shell has moved to), running

    claude --dangerously-skip-permissions --mcp-config <file> \
           --resume <id> -- <text>

never `--fork-session`. The session keeps its id and its transcript (checked
live on 2026-10-26), and the text is its next prompt. eve keeps a record under
that same id, so the row, replies, stop and eve's tools work on it as on any
Work session. The line in the window is
`... -m eve_bridge.work_sessions resume <id>`; `launch` and `resume` are the
only verbs, and the id must be a UUID.

Resume is refused when eve cannot tell whether the session still runs
(`unsure`, above), when its folder or transcript is gone (`no_resume`), and
in a folder where Claude Code's folder-trust prompt was never accepted
(`untrusted`): an interactive claude there stops at that prompt before it
reads anything, and the phone cannot press it (found live on 2026-10-20).

## Stop

SIGTERM to the pid `ps` gives for the session's `--session-id <id>` or
`--resume <id>`, then SIGKILL after 5 s if it is still there. The record is
marked stopped, its MCP config is deleted (so its token stops working), and
the daemon forgets the session at once. Only eve's own sessions: another
claude process is never signalled. The Terminal window is left as Terminal's
profile leaves a finished process (open, or closed).

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
reply, a transcript line, a token or a tool's arguments (AGENTS.md rule 9).

## Files

| File | What |
|---|---|
| `bridge/src/eve_bridge/work_sessions.py` | the list, start / detail / reply / continue / stop, typing, `launch` and `resume` |
| `bridge/src/eve_bridge/claude_peer.py` | Claude Code's session registry, the send into a session's socket, eve's inbox |
| `bridge/src/eve_bridge/eve_mcp.py` | the stdio MCP server |
| `bridge/src/eve_bridge/link_chat.py` | the `/api/link/sessions*` routes |
| `bridge/src/eve_bridge/daemon.py` | `_work`, eve's inbox (`_work_inbox`), the hooks' `_work_note`, IPC `mcp_tools` and `mcp_call` |
| `bridge/src/eve_bridge/telegram.py` | `_tool_lists`, `session_tools`, `session_call`, `_gate(read=)` |
| `bridge/tests/test_work_sessions.py`, `test_claude_peer.py`, `test_eve_mcp.py` | the tests |
| `bridge/tools/work_sessions_check.py` | W4, live: start, list, detail, reply, stop; prints counts and states only |
