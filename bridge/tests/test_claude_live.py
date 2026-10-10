"""claude_live.py: a Claude session whose process is gone leaves the "claude on" picker, and a doubt keeps it."""

from __future__ import annotations

import subprocess
from pathlib import Path
from types import SimpleNamespace
from typing import Any

from eve_bridge import claude_live
from eve_bridge.state import State

PS_OUT = """\
  1524 /Users/g/.local/bin/claude --chrome-native-host
  2643 claude --dangerously-skip-permissions
  3001 /bin/zsh -l
  4100 node /opt/homebrew/lib/node_modules/@anthropic-ai/claude-code/cli.js
  5000 /usr/bin/python3 claude.py
"""


def fake_run(ps: str = PS_OUT, lsof: str = "", *, ps_code: int = 0, lsof_code: int = 0,
             fail: str = "") -> tuple[Any, list[list[str]]]:
    calls: list[list[str]] = []

    def run(argv: list[str], **kw: Any) -> Any:
        calls.append(argv)
        name = Path(argv[0]).name
        if name == fail:
            raise FileNotFoundError(argv[0])
        if name == "ps":
            return SimpleNamespace(returncode=ps_code, stdout=ps)
        return SimpleNamespace(returncode=lsof_code, stdout=lsof)

    return run, calls


def test_the_claude_processes_and_their_folders_are_read() -> None:
    run, calls = fake_run(lsof="p1524\nfcwd\nn/Users/g/.claude/chrome\np2643\nfcwd\nn/Users/g/repo\n")
    assert claude_live.live_cwds(run) == {"/Users/g/.claude/chrome", "/Users/g/repo"}
    # only Claude Code: the native binary by any path and the npm install; never a shell or a claude.py
    assert calls[1][-1] == "1524,2643,4100"


def test_a_pid_gone_since_ps_still_gives_the_rest() -> None:
    run, _ = fake_run(lsof="p2643\nfcwd\nn/Users/g/repo\n", lsof_code=1)
    assert claude_live.live_cwds(run) == {"/Users/g/repo"}


def test_a_probe_that_cannot_tell_says_so() -> None:
    assert claude_live.live_cwds(fake_run(fail="ps")[0]) is None
    assert claude_live.live_cwds(fake_run(fail="lsof")[0]) is None
    assert claude_live.live_cwds(fake_run(ps_code=1)[0]) is None
    assert claude_live.live_cwds(fake_run(ps="  3001 /bin/zsh -l\n")[0]) is None      # no claude at all: blind
    assert claude_live.live_cwds(fake_run(lsof="")[0]) is None

    def slow(argv: list[str], **kw: Any) -> Any:
        raise subprocess.TimeoutExpired(argv, kw.get("timeout", 0))

    assert claude_live.live_cwds(slow) is None


def test_a_session_in_a_subfolder_of_a_live_process_is_alive(tmp_path: Path) -> None:
    repo = tmp_path / "repo"
    (repo / "src").mkdir(parents=True)
    live = {str(repo)}
    assert claude_live.alive(str(repo), live)
    assert claude_live.alive(str(repo / "src"), live)                    # the session's shell moved in
    assert not claude_live.alive(str(tmp_path), live)                   # a folder above is another session
    assert not claude_live.alive(str(tmp_path / "other"), live)


def _state(now: float) -> State:
    state = State()
    for sid, cwd, age in (("dead", "/Users/g/old", 3600), ("live", "/Users/g/repo", 600),
                          ("new", "/Users/g/fresh", 5), ("asks", "/Users/g/asks", 900), ("bare", None, 900)):
        state.session_start(sid, cwd=cwd)
        state.sessions[sid].started_at = now - age
    return state


def test_the_picker_drops_dead_sessions_and_forgets_them() -> None:
    now = 1_800_000_000.0
    state = _state(now)
    got = claude_live.picker_sessions(state, {"/Users/g/repo"}, now=now)
    # newest first; the new one (grace) is kept whatever the probe says
    assert got == ["/Users/g/fresh", "/Users/g/repo"]
    assert "dead" not in state.sessions and "asks" not in state.sessions and "bare" in state.sessions


def test_a_blind_probe_keeps_every_session() -> None:
    now = 1_800_000_000.0
    state = _state(now)
    got = claude_live.picker_sessions(state, probe=lambda: None, now=now)
    assert got == ["/Users/g/fresh", "/Users/g/repo", "/Users/g/asks", "/Users/g/old"]
    assert set(state.sessions) == {"dead", "live", "new", "asks", "bare"}


def test_the_daemon_lends_the_live_list_to_the_inlet(monkeypatch: Any) -> None:
    """The inlet the daemon builds lists live sessions only, and the dead one leaves the daemon's state."""
    import asyncio

    from eve_bridge.daemon import Daemon

    for name, value in (("EVE_TELEGRAM", "1"), ("EVE_TELEGRAM_TOKEN", "123456:AAsecretTOKENvalue"),
                        ("EVE_TELEGRAM_OWNER", "4242"), ("OPENROUTER_API_KEY", "sk-test")):
        monkeypatch.setenv(name, value)
    monkeypatch.setattr(claude_live, "live_processes", lambda run=None: {"/Users/g/repo": True})
    state = State()
    state.session_start("dead", cwd="/Users/g/old")
    state.session_start("live", cwd="/Users/g/repo")
    state.sessions["dead"].started_at -= 3600
    state.sessions["live"].started_at -= 600                             # past the grace: kept by the probe alone
    daemon = SimpleNamespace(
        state=state, _make_agent=lambda *a: None, _agent_cfg=SimpleNamespace(enabled=True), _recall_cfg=None,
        _photo_for_owner=None, _thinker=None, _scene=None, _head=None, _on_agent_state=lambda s: None,
        _request_explore=None, _set_sound=lambda on: None,
        _on_caption=lambda m: None, _room_notes_taker=lambda: None)
    inlet = Daemon._make_telegram(daemon)
    try:
        assert inlet is not None
        assert asyncio.run(inlet._claude_sessions()) == ["/Users/g/repo"]
        assert set(state.sessions) == {"live"}
    finally:
        asyncio.run(inlet.api.close())


def test_a_session_run_through_the_npm_bin_shim_is_a_claude_process() -> None:
    """Review finding: ps shows an npm install started by its shebang shim as ``node …/bin/claude``; it was
    not recognised, so a live session was judged dead and dropped from the picker."""
    assert claude_live._is_claude("node /opt/homebrew/bin/claude --resume")
    assert claude_live._is_claude("/usr/local/bin/bun /Users/g/.bun/bin/claude")
    assert not claude_live._is_claude("node /opt/homebrew/bin/claude-mem worker")       # the control
    assert not claude_live._is_claude("node server.js claude")


def test_the_probe_runs_off_the_event_loop_and_the_pruning_on_it() -> None:
    """Review finding: "claude on" ran ps and lsof synchronously on the daemon's event loop (up to 3 s)."""
    import asyncio
    import threading

    state = State()
    state.session_start("dead", cwd="/Users/g/old")
    state.session_start("live", cwd="/Users/g/repo")
    state.sessions["dead"].started_at -= 3600
    state.sessions["live"].started_at -= 600
    threads: list[str] = []

    def probe() -> set[str]:
        threads.append(threading.current_thread().name)
        return {"/Users/g/repo"}

    async def go() -> list[str]:
        return await claude_live.picker_sessions_off_loop(state, probe)

    assert asyncio.run(go()) == ["/Users/g/repo"]
    assert threads and threads[0] != threading.main_thread().name
    assert set(state.sessions) == {"live"}


def test_each_folder_says_whether_an_owner_can_type_there() -> None:
    ps = PS_OUT + "  6100 /Users/g/.local/bin/claude --output-format stream-json --verbose\n"
    run, _ = fake_run(ps=ps, lsof="p1524\nfcwd\nn/Users/g/.claude/chrome\np2643\nfcwd\nn/Users/g/repo\n"
                              "p6100\nfcwd\nn/Users/g/.claude-mem/observer\n")
    assert claude_live.live_processes(run) == {"/Users/g/.claude/chrome": False, "/Users/g/repo": True,
                                              "/Users/g/.claude-mem/observer": False}
    assert claude_live.interactive("claude --dangerously-skip-permissions --resume")
    assert not claude_live.interactive("claude -p hello")
    assert not claude_live.interactive("claude --output-format=stream-json")


def test_after_a_restart_claude_on_still_finds_the_open_session(monkeypatch: Any) -> None:
    """Live, 2026-10-10: the daemon restarted, the session in front of the owner sent no hook before "claude on",
    and the empty list opened a new session instead of joining it."""
    import asyncio

    monkeypatch.setattr(claude_live, "live_processes",
                        lambda run=None: {"/Users/g/buddy": True, "/Users/g/.claude/chrome": False})
    assert asyncio.run(claude_live.picker_sessions_off_loop(State())) == ["/Users/g/buddy"]


def test_a_known_session_is_not_offered_twice(tmp_path: Path, monkeypatch: Any) -> None:
    import asyncio

    repo, other = tmp_path / "repo", tmp_path / "other"
    (repo / "src").mkdir(parents=True)
    other.mkdir()
    state = State()
    state.session_start("known", cwd=str(repo / "src"))                 # its shell moved into a subfolder
    monkeypatch.setattr(claude_live, "live_processes", lambda run=None: {str(repo): True, str(other): True})
    assert asyncio.run(claude_live.picker_sessions_off_loop(state)) == [str(repo / "src"), str(other)]


def test_a_blind_probe_offers_only_what_the_hooks_told() -> None:
    import asyncio

    import pytest

    mp = pytest.MonkeyPatch()
    try:
        mp.setattr(claude_live, "live_processes", lambda run=None: None)
        state = State()
        state.session_start("known", cwd="/Users/g/repo")
        assert asyncio.run(claude_live.picker_sessions_off_loop(state)) == ["/Users/g/repo"]
    finally:
        mp.undo()
