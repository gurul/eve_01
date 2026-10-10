"""ComputerAgent planning once (EVE_PLAN_EXEC): one planner call, then the worker walks the plan.

The planner is test_computer_agent's FakeClient, which counts every request, so "one planner call and none at
the end" is a number the test reads. The worker is its FakeWorker plus `outline` and `run_plan`.
"""

from __future__ import annotations

import asyncio
import json
from pathlib import Path

from test_computer_agent import FakeClient, FakeWorker, _agent, _message, _response

from eve_bridge import plan_contract as pc
from eve_bridge.computer_agent import PLAN_EXEC_DEFAULT, AgentConfig, configured

PLAN = {"needs_eyes": False, "why": "", "final_say": "Calendar is on the year view.", "success": None, "steps": [
    {"kind": "open_app", "target": "Calendar", "label_hint": "", "text": "", "key": "", "expect": None,
     "consequential": False},
    {"kind": "click", "target": "the year view button", "label_hint": "Year", "text": "", "key": "", "expect": None,
     "consequential": False}]}
DONE = {"status": "complete", "next_index": 2, "reason": "", "confirm": "", "sentence": "Calendar is on the year view.",
        "ledger": [{"index": 1, "step": "open app Calendar", "effect": "confirmed"},
                   {"index": 2, "step": "click the year view button", "effect": "confirmed"}]}


class PlanWorker(FakeWorker):
    def __init__(self, results: list[dict], **kw) -> None:
        super().__init__(**kw)
        self.results, self.runs, self.observed = list(results), [], 0

    async def lane_first(self, goal: str) -> dict:
        return {"status": "none", "reason": "no_match", "clicked": []}

    async def outline(self) -> dict:
        return {"app": "Finder", "lines": ["Year (radio button)", "Week (radio button)"]}

    async def run_plan(self, plan: dict, request: str, start: int = 0, approved=None) -> dict:
        self.runs.append({"plan": plan, "start": start, "approved": dict(approved or {})})
        return self.results.pop(0)

    async def observe(self) -> list[dict]:
        self.observed += 1
        return await super().observe()


def _cfg(tmp: Path, **kw) -> AgentConfig:
    return AgentConfig(runs_dir=tmp / "runs", lane_first=True, plan_exec=True, reflexes=False, verify=False, **kw)


def _plan_response(plan: dict) -> dict:
    return _response("p1", _message(json.dumps(plan)))


def test_it_ships_off_and_the_owner_has_a_switch() -> None:
    assert PLAN_EXEC_DEFAULT is False and configured({}).plan_exec is False
    assert configured({"EVE_PLAN_EXEC": "1"}).plan_exec is True


def test_a_plan_that_runs_costs_one_planner_call_and_none_at_the_end(tmp_path: Path) -> None:
    client = FakeClient([_plan_response(PLAN)])               # a second request would raise "ran out of responses"
    worker = PlanWorker([DONE])
    a, events = _agent(client, worker, tmp_path, config=_cfg(tmp_path))
    said = asyncio.run(a.run("put the calendar on the year view"))
    assert said == "Calendar is on the year view." and client.calls == 1
    # the request names Calendar and Finder is in front: it is opened first, so the outline is Calendar's window
    assert worker.observed == 0 and worker.executed == ["open_app('Calendar')"] and len(worker.runs) == 1
    request = client.requests[0]
    assert request["instructions"] == pc.PLAN_INSTRUCTIONS and "tools" not in request
    assert request["text"]["format"]["schema"] == pc.PLAN_SCHEMA
    shown = request["input"][0]["content"][0]["text"]
    assert "Year (radio button)" in shown and "input_image" not in json.dumps(request)   # planned without a screenshot
    assert [e.kind for e in events][-1] == "final"


def test_a_consequential_step_asks_the_human_with_no_planner_turn(tmp_path: Path) -> None:
    ask = {"status": "needs_human", "next_index": 1, "confirm": "Send", "reason": "a consequential control", "ledger": []}
    client = FakeClient([_plan_response(PLAN)])
    worker = PlanWorker([ask, DONE])
    asked: list[str] = []

    async def human(question: str) -> str:
        asked.append(question)
        return "yes, go ahead"

    a, _ = _agent(client, worker, tmp_path, config=_cfg(tmp_path), ask_user=human)
    assert asyncio.run(a.run("send it")) == "Calendar is on the year view."
    assert client.calls == 1 and asked == ["Should I go ahead: Send?"]
    assert worker.runs[1]["start"] == 1 and worker.runs[1]["approved"] == {"1": "Send"}


def test_a_no_stops_the_plan_and_nothing_else_runs(tmp_path: Path) -> None:
    ask = {"status": "needs_human", "next_index": 1, "confirm": "Send", "reason": "", "ledger": []}
    client = FakeClient([_plan_response(PLAN)])
    worker = PlanWorker([ask])

    async def human(question: str) -> str:
        return "no"

    a, _ = _agent(client, worker, tmp_path, config=_cfg(tmp_path), ask_user=human)
    assert asyncio.run(a.run("send it")) == "Okay, I stopped before that: Send."
    assert client.calls == 1 and len(worker.runs) == 1


def test_a_plan_that_stops_part_way_hands_the_loop_what_was_done(tmp_path: Path) -> None:
    partial = {"status": "partial", "next_index": 1, "reason": "jev_none", "sentence": "",
               "ledger": [{"index": 1, "step": "open app Calendar", "effect": "confirmed"},
                          {"index": 2, "step": "click the year view button", "effect": "refused"}]}
    client = FakeClient([_plan_response(PLAN), _response("r1", _message("It shows the year."))])
    worker = PlanWorker([partial])
    a, _ = _agent(client, worker, tmp_path, config=_cfg(tmp_path))
    assert asyncio.run(a.run("put the calendar on the year view")) == "It shows the year."
    assert client.calls == 2 and worker.observed == 1
    first = client.requests[1]["input"][0]["content"][0]["text"]
    assert "[note] Before you started, a plan already did, in order: open app Calendar." in first
    assert "(jev_none)" in first and "click the year view button" not in first


def test_no_plan_or_a_bad_plan_is_todays_loop_exactly(tmp_path: Path) -> None:
    for first in (_plan_response({"needs_eyes": True, "why": "it asks what is there", "steps": [], "final_say": "",
                                  "success": None}),
                  _response("p1", _message("not json")),
                  _plan_response({"steps": [{"kind": "drag", "target": "x"}]})):
        client = FakeClient([first, _response("r1", _message("done"))])
        worker = PlanWorker([])
        a, _ = _agent(client, worker, tmp_path, config=_cfg(tmp_path))
        assert asyncio.run(a.run("tidy the desktop")) == "done"
        assert client.calls == 2 and worker.runs == []
        assert "[note]" not in client.requests[1]["input"][0]["content"][0]["text"]


def test_a_request_for_an_answer_in_words_is_never_planned(tmp_path: Path) -> None:
    client = FakeClient([_response("r1", _message("Two meetings."))])
    worker = PlanWorker([])
    a, _ = _agent(client, worker, tmp_path, config=_cfg(tmp_path))
    assert asyncio.run(a.run("tell me what is on my calendar")) == "Two meetings."
    assert client.calls == 1 and worker.runs == [] and "tools" in client.requests[0]


def test_the_named_app_is_not_reopened_when_it_is_already_in_front(tmp_path: Path) -> None:
    class Front(PlanWorker):
        async def outline(self) -> dict:
            return {"app": "Calendar", "lines": ["Year (radio button)"]}

    client = FakeClient([_plan_response(PLAN)])
    worker = Front([DONE])
    a, _ = _agent(client, worker, tmp_path, config=_cfg(tmp_path))
    assert asyncio.run(a.run("put the calendar on the year view")) == "Calendar is on the year view."
    assert worker.executed == [] and client.calls == 1
