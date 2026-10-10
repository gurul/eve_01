"""desk_lane.py: keyword-decided clicks on the frontmost window, tried before Codex (2026-10-06)."""

from __future__ import annotations

import asyncio
from types import SimpleNamespace
from typing import Any, Optional

from test_app_reflex import APPS, Inner

from eve_bridge import desk_lane
from eve_bridge.app_reflex import ReflexFirstAgent
from eve_bridge.daemon import Daemon
from eve_bridge.desk_lane import DeskLane, codex_note


def rig(lane_result: Optional[dict[str, Any]] = None, *, raises: bool = False, cancel_during: bool = False):
    events: list[tuple[str, str]] = []
    asked: list[str] = []
    inner = Inner()
    made: list[int] = []
    holder: dict[str, Any] = {}

    async def desk(goal: str):
        asked.append(goal)
        if cancel_during:
            holder["agent"].cancel("owner said stop")
        if raises:
            raise RuntimeError("ax down")
        return lane_result

    def make():
        made.append(1)
        return inner

    async def opener(app: str):
        return True, ""

    agent = ReflexFirstAgent(make, lambda ev: events.append((ev.kind, ev.text)), apps=lambda: APPS,
                             opener=opener, desk=desk)
    holder["agent"] = agent
    return agent, inner, asked, events, made


COMPLETE = {"status": "complete", "reason": "", "clicked": ["Week"], "already": [], "sentence": "Done. I clicked Week."}


def test_a_complete_lane_answers_and_codex_never_starts() -> None:
    agent, inner, asked, events, made = rig(COMPLETE)
    assert asyncio.run(agent.run("switch to week view")) == "Done. I clicked Week."
    assert asked == ["switch to week view"] and made == [] and inner.goals == []
    assert events == [("started", "switch to week view"), ("final", "Done. I clicked Week.")]
    assert agent.body == "desk" and agent.provider == "desk lane"


def test_a_partial_lane_hands_codex_the_task_and_what_was_clicked() -> None:
    lane = {"status": "partial", "reason": "unverified", "clicked": ["Year"], "already": [], "sentence": ""}
    agent, inner, asked, events, made = rig(lane)
    assert asyncio.run(agent.run("year view and then next year", note="\n\n(file hint)")) == "codex did it"
    (goal,) = inner.goals
    assert goal.startswith("year view and then next year\n\nBefore you started, eve's fast lane already clicked 'Year'")
    assert "do not click those again" in goal and goal.endswith("(file hint)")


def test_a_lane_that_did_nothing_leaves_the_task_to_codex_unchanged() -> None:
    for lane in (None, {"status": "none", "reason": "no_match", "clicked": []},
                 {"status": "refused", "reason": "question"}, {"status": "unavailable", "reason": "off"}):
        agent, inner, asked, events, made = rig(lane)
        assert asyncio.run(agent.run("find the email from Sam")) == "codex did it"
        assert inner.goals == ["find the email from Sam"]


def test_a_lane_that_raises_never_costs_the_task() -> None:
    agent, inner, asked, events, made = rig(raises=True)
    assert asyncio.run(agent.run("switch to week view")) == "codex did it" and inner.goals == ["switch to week view"]


def test_a_stop_while_the_lane_runs_stops_before_codex() -> None:
    lane = {"status": "none", "reason": "no_match", "clicked": []}
    agent, inner, asked, events, made = rig(lane, cancel_during=True)
    assert asyncio.run(agent.run("switch to week view")) == "Stopped." and made == []


def test_a_bare_launch_is_still_the_launch_reflex_and_never_the_lane() -> None:
    agent, inner, asked, events, made = rig(COMPLETE)
    assert asyncio.run(agent.run("open Spotify")) == "Opened Spotify." and asked == []


def test_a_complete_lane_without_a_sentence_is_not_an_answer() -> None:
    agent, inner, asked, events, made = rig({**COMPLETE, "sentence": ""})
    assert asyncio.run(agent.run("switch to week view")) == "codex did it"


def test_codex_note_names_the_clicks_only_when_there_were_some() -> None:
    assert codex_note({"clicked": []}) == ""
    note = codex_note({"clicked": ["Year", "Next"]})
    assert "'Year', then 'Next'" in note and "Look at the screen first" in note


# ---- DeskLane itself ----------------------------------------------------------------------------------------

class FakeHelpers:
    def __init__(self, result: dict[str, Any]) -> None:
        self.result, self.goals = result, []

    def bind(self, log, display):
        self._log = log

    def begin(self):
        pass

    def lane_first(self, goal: str, dry_run: bool = False):
        self.goals.append(goal)
        self._log("delegate route: …")
        return dict(self.result)


def test_the_lane_runs_lane_first_on_one_helpers_for_the_daemons_life() -> None:
    made: list[FakeHelpers] = []

    def make():
        made.append(FakeHelpers(COMPLETE))
        return made[-1]
    lane = DeskLane(make_helpers=make, trusted=lambda: True)
    for _ in range(2):
        r = asyncio.run(lane.attempt("switch to week view"))
        assert r is not None and r["status"] == "complete" and r["log"] == ["delegate route: …"]
    assert len(made) == 1 and made[0].goals == ["switch to week view"] * 2


def test_without_accessibility_the_lane_is_off_and_never_builds_helpers() -> None:
    made: list[int] = []
    lane = DeskLane(make_helpers=lambda: made.append(1), trusted=lambda: False)
    assert asyncio.run(lane.attempt("switch to week view")) is None
    assert asyncio.run(lane.attempt("switch to week view")) is None and made == []


def test_a_lane_that_cannot_start_is_off_for_good() -> None:
    calls: list[int] = []

    def make():
        calls.append(1)
        raise ImportError("no pyautogui")
    lane = DeskLane(make_helpers=make, trusted=lambda: True)
    assert asyncio.run(lane.attempt("x")) is None and asyncio.run(lane.attempt("x")) is None and calls == [1]


def test_a_goal_is_capped_before_it_reaches_the_router() -> None:
    h = FakeHelpers(COMPLETE)
    asyncio.run(DeskLane(make_helpers=lambda: h, trusted=lambda: True).attempt("x" * 5000))
    assert len(h.goals[0]) == desk_lane.MAX_GOAL_CHARS


def test_the_daemon_makes_one_lane_and_the_switch_turns_it_off(monkeypatch) -> None:
    host = SimpleNamespace()
    monkeypatch.setenv("EVE_DESK_LANE", "0")
    assert Daemon._desk(host) is None
    monkeypatch.setenv("EVE_DESK_LANE", "1")
    first, second = Daemon._desk(host), Daemon._desk(host)
    assert first is not None and first.__self__ is second.__self__
    monkeypatch.delenv("EVE_DESK_LANE")
    assert desk_lane.enabled({}) is desk_lane.DEFAULT


def test_the_chiefs_codex_floor_never_gets_the_lane(monkeypatch) -> None:
    monkeypatch.setenv("EVE_DESK_LANE", "1")
    host = SimpleNamespace(_codex_warm=None, _active_agent=None)
    agent = Daemon._make_agent(host, lambda e: None, lambda q: None, floor="codex")
    assert agent._desk is None
