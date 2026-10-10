"""ComputerAgent with the router in front of the planner (GATES.md G6).

The planner is a FakeClient that counts every request, so "zero planner calls" is a number the
test reads, not a claim. The worker is test_computer_agent's FakeWorker plus a `lane_first`.
"""

from __future__ import annotations

import asyncio
import json
from pathlib import Path

from test_computer_agent import FakeClient, FakeWorker, _agent, _call, _message, _response

from eve_bridge.computer_agent import LANE_FIRST_TIMEOUT_SECS, AgentConfig, WorkerClient, configured
from eve_bridge.fast_lane import LANE_FIRST_DEFAULT


class LaneWorker(FakeWorker):
    def __init__(self, route, **kw) -> None:
        super().__init__(**kw)
        self.route = route
        self.routed: list[str] = []
        self.observed = 0

    async def lane_first(self, goal: str) -> dict:
        self.routed.append(goal)
        if isinstance(self.route, BaseException):
            raise self.route
        return self.route

    async def observe(self) -> list[dict]:
        self.observed += 1
        return await super().observe()


def _cfg(tmp: Path, **kw) -> AgentConfig:
    return AgentConfig(runs_dir=tmp / "runs", lane_first=True, **kw)


def _log(agent) -> list[dict]:
    return [json.loads(line) for line in agent.run_log.read_text().splitlines()]


COMPLETE = {"status": "complete", "reason": "", "clicked": ["Week"], "already": [], "sentence": "Done. I clicked Week.",
            "line": "script: complete; applied=[Week (radio button)]; step 1/1 stopped: reason=one_step",
            "log": ["delegate step 1: Week (radio button) (keyword) act", "delegate script: complete"], "ms": 412.0}


def test_a_lane_decided_task_makes_zero_planner_calls(tmp_path: Path) -> None:
    client = FakeClient([])                                   # any request would raise "ran out of responses"
    worker = LaneWorker(COMPLETE)
    a, events = _agent(client, worker, tmp_path, config=_cfg(tmp_path))
    said = asyncio.run(a.run("switch to week view"))
    assert said == "Done. I clicked Week."
    assert client.calls == 0 and client.requests == []        # no planner turn, and no final-answer check
    assert worker.routed == ["switch to week view"] and worker.observed == 0 and worker.executed == []
    assert worker.closed and a.final == said
    kinds = [e.kind for e in events]
    assert kinds[0] == "started" and kinds[-1] == "final" and "progress" in kinds
    entry = next(e for e in _log(a) if "lane_first" in e)
    assert entry["lane_first"]["status"] == "complete" and entry["lane_first"]["clicked"] == ["Week"]
    assert "log" not in entry["lane_first"] and "secs" in entry


def test_a_partial_route_hands_the_planner_what_was_clicked(tmp_path: Path) -> None:
    partial = {"status": "partial", "reason": "no_match", "clicked": ["Year"], "already": [], "sentence": "",
               "line": "script: partial; applied=[Year (radio button)]; step 2/2 escalate: …", "log": []}
    client = FakeClient([_response("r1", _message("It shows the year."))])
    worker = LaneWorker(partial)
    a, _ = _agent(client, worker, tmp_path, config=_cfg(tmp_path, verify=False))
    said = asyncio.run(a.run("year view and then the fortnight"))
    assert said == "It shows the year." and client.calls == 1 and worker.observed == 1
    first = client.requests[0]["input"][0]["content"][0]["text"]
    assert first.startswith("Goal: year view and then the fortnight")
    assert '[note] Before you started, the fast lane already clicked, in order: "Year"' in first
    assert "(no_match)" in first and "do not repeat those clicks" in first
    assert a._acted is True                                   # the lane's click makes the final answer checkable


def test_a_route_that_did_nothing_leaves_the_planner_exactly_as_before(tmp_path: Path) -> None:
    for route in ({"status": "none", "reason": "uncovered", "clicked": [], "log": []},
                  {"status": "refused", "reason": "question", "clicked": []},
                  {"status": "unavailable", "reason": "the router is off (EVE_LANE_FIRST)"},
                  {"status": "complete", "clicked": ["Week"], "sentence": ""},       # complete without a sentence
                  "not a dict", RuntimeError("pipe closed")):
        client = FakeClient([_response("r1", _call("exec_py", "c1", code="log(1)")),
                             _response("r2", _message("done"))])
        worker = LaneWorker(route)
        a, _ = _agent(client, worker, tmp_path, config=_cfg(tmp_path, verify=False))
        assert asyncio.run(a.run("what is on this week")) == "done", route
        first = client.requests[0]["input"][0]["content"][0]["text"]
        if not (isinstance(route, dict) and route.get("clicked")):
            assert "[note]" not in first, route
        assert client.calls == 2 and worker.executed == ["log(1)"]


def test_lane_first_off_never_calls_the_router(tmp_path: Path) -> None:
    client = FakeClient([_response("r1", _message("done"))])
    worker = LaneWorker(COMPLETE)
    cfg = AgentConfig(runs_dir=tmp_path / "runs", lane_first=False)
    a, _ = _agent(client, worker, tmp_path, config=cfg)
    assert asyncio.run(a.run("switch to week view")) == "done"
    assert worker.routed == [] and client.calls == 1
    # a worker with no router at all (an older worker) is the planner's task, not an error
    plain = FakeWorker()
    client2 = FakeClient([_response("r1", _message("done"))])
    a2, _ = _agent(client2, plain, tmp_path, config=_cfg(tmp_path))
    assert asyncio.run(a2.run("switch to week view")) == "done" and client2.calls == 1


def test_config_follows_the_eval_decision_and_the_env(tmp_path: Path) -> None:
    assert AgentConfig().lane_first is LANE_FIRST_DEFAULT and configured({}).lane_first is LANE_FIRST_DEFAULT
    assert configured({"EVE_LANE_FIRST": "1"}).lane_first is True
    assert configured({"EVE_LANE_FIRST": "off"}).lane_first is False
    assert configured({}).lane_decide == "keyword"
    assert configured({"EVE_FAST_LANE_DECIDE": "model"}).lane_decide == "model"
    assert configured({"EVE_FAST_LANE_DECIDE": "??"}).lane_decide == "keyword"
    # the client never restarts the worker over a route, and says so when there is no worker
    w = WorkerClient()
    assert asyncio.run(w.lane_first("x")) == {"status": "unavailable", "reason": "desktop worker is not running"}
    assert LANE_FIRST_TIMEOUT_SECS >= 5.0
