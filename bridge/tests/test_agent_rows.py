"""Unit tests for the monitor build's per-agent heartbeat rows.

Covers State.agent_rows/note_tool and the `agents` key build_heartbeat adds.
The wire shape is load-bearing: the e-ink monitor firmware indexes `n`/`s`/`t`
directly, so a renamed key is a silently blank column on the panel.
"""

from __future__ import annotations

from eve_bridge.protocol import build_heartbeat
from eve_bridge.state import State


def _session(st: State, sid: str, cwd: str | None = None) -> None:
    st.session_start(sid, cwd=cwd)


def test_row_names_come_from_cwd_basename():
    st = State()
    _session(st, "s1", cwd="/Users/x/Documents/work/claude-pet")
    assert st.agent_rows() == [{"n": "claude-pet", "s": "idle"}]


def test_trailing_slash_does_not_blank_the_name():
    st = State()
    _session(st, "s1", cwd="/Users/x/work/era-hub-api/")
    assert st.agent_rows()[0]["n"] == "era-hub-api"


def test_name_falls_back_to_session_prefix_without_cwd():
    """A hook that carries no cwd must still produce a labelled row."""
    st = State()
    _session(st, "9f3fdfd2-aaaa")
    assert st.agent_rows()[0]["n"] == "9f3fdf"


def test_long_name_is_clipped_to_the_column_width():
    st = State()
    _session(st, "s1", cwd="/tmp/a-really-long-repository-name")
    assert len(st.agent_rows()[0]["n"]) == State.AGENT_NAME_CHARS


def test_states_and_current_tool():
    st = State()
    _session(st, "run", cwd="/tmp/r")
    _session(st, "idle", cwd="/tmp/i")
    st.turn_begin("run")
    st.note_tool("run", "Bash")
    rows = {r["n"]: r for r in st.agent_rows()}
    assert rows["r"]["s"] == "run"
    assert rows["r"]["t"] == "Bash"
    assert rows["i"]["s"] == "idle"
    assert "t" not in rows["i"]  # no tool yet → key omitted, not empty string


def test_ordering_is_waiting_then_running_then_idle():
    st = State()
    _session(st, "i", cwd="/tmp/zzz-idle")
    _session(st, "r", cwd="/tmp/aaa-run")
    _session(st, "w", cwd="/tmp/mmm-wait")
    st.turn_begin("r")
    st.needs_input("w")
    assert [r["n"] for r in st.agent_rows()] == ["mmm-wait", "aaa-run", "zzz-idle"]


def test_ordering_is_stable_by_name_within_a_rank():
    """Rank-then-name, never recency: a row that hops between refreshes costs
    a full e-ink redraw and reads as flicker."""
    st = State()
    for cwd in ("/tmp/charlie", "/tmp/alpha", "/tmp/bravo"):
        _session(st, cwd, cwd=cwd)
    assert [r["n"] for r in st.agent_rows()] == ["alpha", "bravo", "charlie"]


def test_rows_are_capped():
    st = State()
    for i in range(State.MAX_AGENT_ROWS + 4):
        _session(st, f"s{i}", cwd=f"/tmp/repo{i}")
    assert len(st.agent_rows()) == State.MAX_AGENT_ROWS
    assert len(st.agent_rows(limit=2)) == 2


def test_note_tool_never_conjures_a_session():
    """Unlike needs_input, a stray tool hook must not invent an agent row."""
    st = State()
    st.note_tool("ghost", "Bash")
    assert st.agent_rows() == []
    assert st.total == 0


def test_heartbeat_carries_agents_and_stays_small():
    st = State()
    for i in range(State.MAX_AGENT_ROWS):
        _session(st, f"s{i}", cwd=f"/tmp/some-repo-{i}")
        st.note_tool(f"s{i}", "ToolName")
    snap = build_heartbeat(st)
    assert len(snap["agents"]) == State.MAX_AGENT_ROWS
    assert snap["agents"][0].keys() == {"n", "s", "t"}
    # The firmware reads heartbeats into a 2KB line buffer shared with a
    # 720-byte prompt detail; a full agent table must not crowd it out.
    import json
    assert len(json.dumps(snap, separators=(",", ":"))) < 1200


def test_empty_agents_key_is_always_present():
    """Present-but-empty is how 'all sessions ended' reaches the firmware —
    an absent key leaves the previous list standing on the panel."""
    assert build_heartbeat(State())["agents"] == []
