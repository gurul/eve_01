"""computer_agent.py against a scripted Responses client and a fake worker:
the exec_py contract, the context-bearing first turn, output chaining, step
and wall-clock caps, cancel (between turns and mid-request), steer, ask_user,
fail-safe, effort pacing, repeat notes, retries, continuation, failure text,
progress events and the action log."""

from __future__ import annotations

import asyncio
import json
from pathlib import Path

import pytest

from eve_bridge import computer_agent as ca
from eve_bridge.computer_agent import (
    INSTRUCTIONS,
    TOOLS,
    AgentConfig,
    AgentEvent,
    ComputerAgent,
    FailSafe,
    WorkerDead,
    classify_response,
    configured,
    describe_failure,
)
from eve_bridge.desktop_helpers import HELPER_NAMES

# ---- fakes -------------------------------------------------------------------------

IMAGE = {"type": "input_image", "detail": "original", "image_url": "data:image/png;base64,AAAA"}
CONTEXT = {"type": "input_text", "text": "frontmost: Warp — 'zsh'; screen 100x50; 14:02"}


def _call(name: str, call_id: str, **args) -> dict:
    return {"type": "function_call", "name": name, "call_id": call_id, "arguments": json.dumps(args)}


def _message(text: str) -> dict:
    return {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": text}]}


def _response(rid: str, *items: dict, status: str = "completed", **extra) -> dict:
    return {"id": rid, "status": status, "output": list(items), **extra}


class FakeClient:
    """Serves scripted responses in order; records every request."""

    def __init__(self, responses: list[dict], raise_first: list[BaseException] | None = None) -> None:
        self.responses = list(responses)
        self.raise_first = list(raise_first or [])
        self.requests: list[dict] = []
        self.served: list[dict] = []
        self.gate: asyncio.Event | None = None
        self.calls = 0

    async def __call__(self, request: dict) -> dict:
        self.requests.append(request)
        self.calls += 1
        if self.gate is not None:
            await self.gate.wait()
        if self.raise_first:
            raise self.raise_first.pop(0)
        if not self.responses:
            raise AssertionError("client ran out of scripted responses")
        self.served.append(self.responses[0])
        return self.responses.pop(0)


def new_items(client: FakeClient, i: int, prev: int | None = None) -> list:
    """What request ``i`` adds to the loop's history. The chain is stateless (OpenRouter refuses
    previous_response_id, 2026-10-08): request ``i`` resends request ``prev``'s input and the response it got,
    then the new items. ``prev`` is the loop's previous request (default ``i - 1``)."""
    prev = i - 1 if prev is None else prev
    before = client.requests[prev]["input"] + list(client.served[prev].get("output") or [])
    req = client.requests[i]
    assert "previous_response_id" not in req and req["store"] is False
    assert req["include"] == ["reasoning.encrypted_content"]
    assert req["input"][:len(before)] == before                    # the whole history is resent
    return req["input"][len(before):]


class FakeWorker:
    def __init__(self, results: dict[str, list[dict]] | None = None, failsafe_on: str | None = None) -> None:
        self.results = results or {}
        self.failsafe_on = failsafe_on
        self.executed: list[str] = []
        self.started = False
        self.closed = False
        self.block: asyncio.Event | None = None

    async def start(self) -> dict:
        self.started = True
        return {"ready": True, "width": 100, "height": 50}

    async def observe(self) -> list[dict]:
        return [dict(IMAGE), dict(CONTEXT)]

    async def execute(self, code: str) -> list[dict]:
        self.executed.append(code)
        if self.block is not None:
            await self.block.wait()
        if self.failsafe_on is not None and self.failsafe_on in code:
            raise FailSafe("corner")
        return self.results.get(code, [{"type": "input_text", "text": f"ran {code}"}])

    async def close(self) -> None:
        self.closed = True


def _agent(client: FakeClient, worker: FakeWorker, tmp: Path, **kw) -> tuple[ComputerAgent, list[AgentEvent]]:
    events: list[AgentEvent] = []
    # These tests drive the PLANNER loop. The tiers in front of it (task_router reflexes, the lane
    # router) answer some of these goals before the planner is ever called, and have their own files:
    # tests/test_task_router.py and tests/test_agent_lane_first.py.
    cfg = kw.pop("config", AgentConfig(runs_dir=tmp / "runs", max_turns=kw.pop("max_turns", 25), reflexes=False,
                                       lane_first=False))
    clock = kw.pop("clock", lambda: 1.0)
    a = ComputerAgent(client, worker_factory=lambda: worker, config=cfg, on_event=events.append,
                      clock=clock, **kw)
    return a, events


async def _no_sleep(_s: float) -> None:
    pass


# ---- config / classification -------------------------------------------------------

def test_configured_defaults_and_env() -> None:
    c = configured({})
    assert c.enabled and c.model == "gpt-6-astra" and c.max_turns == 25
    c = configured({"EVE_COMPUTER_CONTROL": "off", "EVE_AGENT_MODEL": "gpt-5.6-sol",
                    "EVE_AGENT_MAX_TURNS": "3"})
    assert not c.enabled and c.model == "gpt-5.6-sol" and c.max_turns == 3
    assert configured({"EVE_AGENT_MAX_TURNS": "lots"}).max_turns == 25


def test_configured_reads_new_knobs_and_wires_exec_timeout(monkeypatch, caplog, tmp_path: Path) -> None:
    c = configured({})
    assert (c.reasoning_effort, c.plan_reasoning_effort) == ("medium", "high")
    assert c.verify is True and c.verify_reasoning_effort == "low"
    assert configured({"EVE_AGENT_VERIFY": "off"}).verify is False
    assert configured({"EVE_AGENT_PLAN_REASONING": "xhigh"}).plan_reasoning_effort == "xhigh"
    assert configured({"EVE_AGENT_PLAN_REASONING": "max"}).plan_reasoning_effort == "max"
    assert (c.exec_timeout_secs, c.max_secs, c.api_timeout_secs) == (60.0, 180.0, 90.0)
    c = configured({"EVE_AGENT_REASONING": "medium", "EVE_AGENT_PLAN_REASONING": "high",
                    "EVE_AGENT_EXEC_TIMEOUT": "30", "EVE_AGENT_MAX_SECS": "60"})
    assert (c.reasoning_effort, c.plan_reasoning_effort, c.exec_timeout_secs, c.max_secs) == ("medium", "high", 30.0, 60.0)
    with caplog.at_level("WARNING"):
        assert configured({"EVE_AGENT_REASONING": "turbo"}).reasoning_effort == "medium"
    assert "turbo" in caplog.text
    assert configured({"EVE_AGENT_EXEC_TIMEOUT": "3"}).exec_timeout_secs == 10.0
    assert configured({"EVE_AGENT_MAX_SECS": "5"}).max_secs == 30.0
    # the default worker factory hands exec_timeout_secs to the WorkerClient
    made: list[float] = []

    class Spy:
        def __init__(self, timeout_secs: float) -> None:
            made.append(timeout_secs)

        async def start(self) -> dict:
            raise RuntimeError("spy: no desktop here")

        async def close(self) -> None:
            pass
    monkeypatch.setattr(ca, "WorkerClient", Spy)
    a = ComputerAgent(FakeClient([]), config=AgentConfig(runs_dir=tmp_path / "runs", exec_timeout_secs=30.0))
    assert asyncio.run(a.run("x")) == "Sorry, that failed unexpectedly."
    assert made == [30.0]


def test_tool_contract_is_the_documented_exec_py_shape() -> None:
    exec_tool = next(t for t in TOOLS if t["name"] == "exec_py")
    assert exec_tool["type"] == "function" and exec_tool["strict"] is True
    assert exec_tool["parameters"]["required"] == ["code"]
    assert exec_tool["parameters"]["additionalProperties"] is False
    assert "ask_user" in INSTRUCTIONS and "FAILSAFE" in INSTRUCTIONS


def test_instructions_tool_text_and_helper_names_agree() -> None:
    for name in HELPER_NAMES:
        assert name in INSTRUCTIONS, name
    for phrase in ("never Spotlight", "log()", "ask_user", "FAILSAFE"):
        assert phrase in INSTRUCTIONS, phrase
    assert "start with `display" not in INSTRUCTIONS and "command+space" not in INSTRUCTIONS
    code_desc = next(t for t in TOOLS if t["name"] == "exec_py")["parameters"]["properties"]["code"]["description"]
    for name in HELPER_NAMES:
        assert name in code_desc, name


def test_classify_calls_final_commentary() -> None:
    c = classify_response(_response("r1", {"type": "reasoning"}, _call("exec_py", "c1", code="x")))
    assert c.kind == "calls" and c.calls[0]["args"] == {"code": "x"}
    assert classify_response(_response("r2", _message("done"))).kind == "final"
    assert classify_response(_response("r3")).kind == "commentary"
    c = classify_response(_response("r4", _message("looking"), _call("exec_py", "c2", code="y")))
    assert c.kind == "calls" and c.text == "looking"


def test_classify_rejects_bad_replies() -> None:
    with pytest.raises(RuntimeError):
        classify_response(_response("r", status="incomplete"))
    with pytest.raises(RuntimeError):
        classify_response(_response("r", _call("rm_rf", "c1")))
    with pytest.raises(RuntimeError):
        classify_response(_response("r", {"type": "function_call", "name": "exec_py", "call_id": "c", "arguments": "{"}))


# ---- the loop -----------------------------------------------------------------------

def test_happy_path_chains_outputs_and_ends_on_final(tmp_path: Path) -> None:
    client = FakeClient([
        _response("r1", _call("exec_py", "c1", code="display(pyautogui.screenshot())")),
        _response("r2", _message("Looking at it now."), _call("exec_py", "c2", code="pyautogui.click(1, 2)")),
        _response("r3", _message("Done, your mail is open.")),
    ])
    worker = FakeWorker({"display(pyautogui.screenshot())": [dict(IMAGE)]})
    a, events = _agent(client, worker, tmp_path)
    final = asyncio.run(a.run("open my mail"))
    assert final == "Done, your mail is open."
    assert worker.started and worker.closed
    assert worker.executed == ["display(pyautogui.screenshot())", "pyautogui.click(1, 2)"]
    # request shapes: turn 1 carries the goal, the context line and the observe screenshot
    r1, r2, r3 = client.requests
    assert r1["model"] == "gpt-6-astra" and "previous_response_id" not in r1
    [first] = r1["input"]
    assert first["type"] == "message" and first["role"] == "user"
    assert first["content"][0]["type"] == "input_text"
    assert "Goal: open my mail" in first["content"][0]["text"] and "frontmost: Warp" in first["content"][0]["text"]
    assert first["content"][1] == IMAGE
    assert r1["tools"] == TOOLS and r1["parallel_tool_calls"] is False
    assert new_items(client, 1) == [{"type": "function_call_output", "call_id": "c1", "output": [IMAGE]}]
    assert new_items(client, 2)[0]["output"] == [{"type": "input_text", "text": "ran pyautogui.click(1, 2)"}]
    kinds = [e.kind for e in events]
    assert kinds == ["started", "turn", "exec", "turn", "commentary", "exec", "progress", "turn", "final"]
    assert a.status()["running"] is False and a.status()["final"] == final


def test_first_turn_carries_context_and_plan_effort(tmp_path: Path) -> None:
    client = FakeClient([_response("r1", _call("exec_py", "c1", code="a")), _response("r2", _message("ok"))])
    a, _ = _agent(client, FakeWorker(), tmp_path)
    asyncio.run(a.run("x"))
    assert client.requests[0]["reasoning"] == {"effort": "high"}
    assert client.requests[1]["reasoning"] == {"effort": "medium"}
    assert all(r["timeout"] == 90.0 for r in client.requests)


def test_step_cap_ends_the_run(tmp_path: Path) -> None:
    client = FakeClient([_response(f"r{i}", _call("exec_py", f"c{i}", code=f"step{i}")) for i in range(10)])
    a, events = _agent(client, FakeWorker(), tmp_path, max_turns=3)
    final = asyncio.run(a.run("loop forever"))
    assert "ran out of steps (3)" in final
    assert len(client.requests) == 3 and events[-1].kind == "final"


def test_wall_clock_budget_ends_the_run(tmp_path: Path) -> None:
    client = FakeClient([_response(f"r{i}", _call("exec_py", f"c{i}", code=f"step{i}")) for i in range(10)])
    cfg = AgentConfig(runs_dir=tmp_path / "runs", max_secs=150)
    a, events = _agent(client, FakeWorker(), tmp_path, config=cfg, clock=lambda: 100.0 * client.calls)
    final = asyncio.run(a.run("slow"))
    assert final.startswith("I ran out of time (150 s)")
    assert len(client.requests) == 2 and events[-1].kind == "final"


def test_commentary_only_turn_continues(tmp_path: Path) -> None:
    client = FakeClient([_response("r1"), _response("r2", _message("ok"))])
    a, _ = _agent(client, FakeWorker(), tmp_path)
    assert asyncio.run(a.run("x")) == "ok"
    assert new_items(client, 1) == []


def test_steer_is_delivered_with_the_next_turn(tmp_path: Path) -> None:
    client = FakeClient([
        _response("r1", _call("exec_py", "c1", code="a")),
        _response("r2", _call("exec_py", "c2", code="b")),
        _response("r3", _message("done")),
    ])
    a, _ = _agent(client, FakeWorker(), tmp_path)

    async def go() -> str:
        assert a.steer("too early") is False          # not running yet
        client.gate = asyncio.Event()                  # hold every request until released
        task = asyncio.create_task(a.run("x"))
        await asyncio.sleep(0)                         # the run is now waiting on request 1
        assert a.steer("actually use safari") is True
        client.gate.set()
        return await task
    assert asyncio.run(go()) == "done"
    # the steer rides along with c1's output, tagged, as a user message
    inp = new_items(client, 1)
    assert inp[0]["type"] == "function_call_output"
    assert inp[1] == {"type": "message", "role": "user",
                      "content": [{"type": "input_text", "text": "[steer] actually use safari"}]}
    assert client.requests[1]["reasoning"] == {"effort": "high"}          # a steer is a planning moment
    assert all(i["type"] != "message" for i in new_items(client, 2))   # delivered once
    assert client.requests[2]["reasoning"] == {"effort": "medium"}


def test_cancel_stops_between_turns_and_kills_the_worker(tmp_path: Path) -> None:
    client = FakeClient([_response("r1", _call("exec_py", "c1", code="a")), _response("r2", _message("never"))])
    worker = FakeWorker()
    a, events = _agent(client, worker, tmp_path)

    async def go() -> str:
        client.gate = asyncio.Event()
        task = asyncio.create_task(a.run("x"))
        await asyncio.sleep(0)
        a.cancel()
        client.gate.set()
        return await task
    assert asyncio.run(go()) == "Stopped."
    assert worker.closed and worker.executed == []
    assert events[-1].kind == "cancelled"


def test_cancel_interrupts_an_inflight_request(tmp_path: Path) -> None:
    client = FakeClient([_response("r1", _message("never"))])
    worker = FakeWorker()
    a, events = _agent(client, worker, tmp_path)

    async def go() -> str:
        client.gate = asyncio.Event()                  # never set: the request hangs forever
        task = asyncio.create_task(a.run("x"))
        for _ in range(5):
            await asyncio.sleep(0)
        assert len(client.requests) == 1
        a.cancel(reason="hushed by a touch")
        return await asyncio.wait_for(task, timeout=0.1)
    assert asyncio.run(go()) == "Stopped: hushed by a touch."
    assert worker.closed and events[-1].kind == "cancelled"
    lines = [json.loads(ln) for ln in a.run_log.read_text().splitlines()]
    assert {"cancelled": "hushed by a touch"}.items() <= lines[-3].items()      # then final, then the bill


def test_cancel_interrupts_a_running_exec(tmp_path: Path) -> None:
    client = FakeClient([_response("r1", _call("exec_py", "c1", code="wait_for('never')"))])
    worker = FakeWorker()
    a, _ = _agent(client, worker, tmp_path)

    async def go() -> str:
        worker.block = asyncio.Event()                 # the exec never returns on its own
        task = asyncio.create_task(a.run("x"))
        for _ in range(100):
            await asyncio.sleep(0)
            if worker.executed:
                break
        assert worker.executed == ["wait_for('never')"]
        a.cancel()
        return await asyncio.wait_for(task, timeout=0.1)
    assert asyncio.run(go()) == "Stopped."
    assert worker.closed


def test_ask_user_routes_to_the_voice_session(tmp_path: Path) -> None:
    client = FakeClient([
        _response("r1", _call("ask_user", "c1", question="Send the email?")),
        _response("r2", _message("sent")),
    ])
    asked: list[str] = []

    async def ask(q: str) -> str:
        asked.append(q)
        return "yes go ahead"
    a, events = _agent(client, FakeWorker(), tmp_path, ask_user=ask)
    assert asyncio.run(a.run("x")) == "sent"
    assert asked == ["Send the email?"]
    assert new_items(client, 1)[0]["output"] == [{"type": "input_text", "text": "yes go ahead"}]
    assert any(e.kind == "ask" and e.text == "Send the email?" for e in events)


def test_ask_user_without_a_listener_answers_no(tmp_path: Path) -> None:
    client = FakeClient([_response("r1", _call("ask_user", "c1", question="Delete it?")), _response("r2", _message("ok"))])
    a, _ = _agent(client, FakeWorker(), tmp_path)
    asyncio.run(a.run("x"))
    assert new_items(client, 1)[0]["output"][0]["text"].startswith("no (")
    assert client.requests[1]["reasoning"] == {"effort": "high"}          # a "no" means re-plan


def test_failsafe_ends_the_run(tmp_path: Path) -> None:
    client = FakeClient([_response("r1", _call("exec_py", "c1", code="corner"))])
    worker = FakeWorker(failsafe_on="corner")
    a, events = _agent(client, worker, tmp_path)
    assert asyncio.run(a.run("x")).startswith("Stopped: the mouse")
    assert worker.closed and events[-1].kind == "cancelled"


def test_client_error_is_reported_not_raised(tmp_path: Path) -> None:
    client = FakeClient([_response("r1", status="failed")])
    a, events = _agent(client, FakeWorker(), tmp_path)
    assert asyncio.run(a.run("x")).startswith("Sorry, that failed")
    assert events[-1].kind == "error"


# ---- pacing: effort, repeats, retries, continuation ---------------------------------

NO_EFFECT = [{"type": "input_text", "text": "[after your input] frontmost: Spotify — 'Spotify'; screen: unchanged"}]
ACTED = [{"type": "input_text",
          "text": "[after your input] frontmost: Spotify — 'Spotify'; screen: changed around (10,10,20,20)"}]
LOOKED = [{"type": "input_text", "text": "[after] frontmost: Spotify — 'Spotify'; screen: unchanged"}]


def _verdict(rid: str, valid: bool, guidance: str = "") -> dict:
    return _response(rid, _message(json.dumps({"valid": valid, "guidance": guidance})))


def test_repeat_exec_gets_a_note_when_the_input_changed_nothing(tmp_path: Path) -> None:
    client = FakeClient([
        _response("r1", _call("exec_py", "c1", code="pyautogui.click(1, 2)")),
        _response("r2", _call("exec_py", "c2", code="pyautogui.click(1, 2)")),
        _response("r3", _message("stuck")),
        _verdict("v1", True),
    ])
    a, _ = _agent(client, FakeWorker({"pyautogui.click(1, 2)": NO_EFFECT}), tmp_path)
    asyncio.run(a.run("x"))
    assert client.requests[1]["reasoning"] == {"effort": "medium"}
    inp = new_items(client, 2)
    notes = [i for i in inp if i["type"] == "message"]
    assert len(notes) == 1 and notes[0]["content"][0]["text"].startswith("[note] That is the same code")
    assert client.requests[2]["reasoning"] == {"effort": "high"}
    lines = [json.loads(ln) for ln in a.run_log.read_text().splitlines()]
    assert any(ln.get("repeat") is True for ln in lines)


def test_repeat_exec_that_changed_the_screen_gets_no_repeat_note(tmp_path: Path) -> None:
    """Scrolling twice is a repeat that works; the note used to accuse it of changing nothing."""
    client = FakeClient([
        _response("r1", _call("exec_py", "c1", code="pyautogui.scroll(-10)")),
        _response("r2", _call("exec_py", "c2", code="pyautogui.scroll(-10)")),
        _response("r3", _message("scrolled")),
        _verdict("v1", True),
    ])
    a, _ = _agent(client, FakeWorker({"pyautogui.scroll(-10)": ACTED}), tmp_path)
    asyncio.run(a.run("x"))
    assert not [i for i in new_items(client, 2) if i["type"] == "message"]
    assert client.requests[2]["reasoning"] == {"effort": "medium"}
    assert not any(json.loads(ln).get("repeat") for ln in a.run_log.read_text().splitlines())


def test_error_result_escalates_effort_once(tmp_path: Path) -> None:
    client = FakeClient([
        _response("r1", _call("exec_py", "c1", code="bad")),
        _response("r2", _call("exec_py", "c2", code="good")),
        _response("r3", _call("exec_py", "c3", code="better")),
        _response("r4", _message("ok")),
    ])
    worker = FakeWorker({"bad": [{"type": "input_text", "text": "Traceback (most recent call last):\n  boom"}]})
    a, _ = _agent(client, worker, tmp_path)
    asyncio.run(a.run("x"))
    efforts = [r["reasoning"]["effort"] for r in client.requests]
    assert efforts == ["high", "high", "medium", "medium"]


def test_transient_api_error_retries_once(tmp_path: Path) -> None:
    class APIConnectionError(Exception):
        pass

    client = FakeClient([_response("r1", _message("ok"))], raise_first=[APIConnectionError("down")])
    a, _ = _agent(client, FakeWorker(), tmp_path, sleep=_no_sleep)
    assert asyncio.run(a.run("x")) == "ok"
    assert len(client.requests) == 2 and client.requests[0] == client.requests[1]
    lines = [json.loads(ln) for ln in a.run_log.read_text().splitlines()]
    assert any(ln.get("retry") == "APIConnectionError" for ln in lines)
    client = FakeClient([_response("r1", _message("ok"))],
                        raise_first=[APIConnectionError("down"), APIConnectionError("still down")])
    a, events = _agent(client, FakeWorker(), tmp_path, sleep=_no_sleep)
    assert asyncio.run(a.run("x")) == "Sorry, I lost my connection to the model service."
    assert events[-1].kind == "error" and events[-1].text.startswith("APIConnectionError")


def test_incomplete_response_is_continued_once(tmp_path: Path) -> None:
    cut = {"status": "incomplete", "incomplete_details": {"reason": "max_output_tokens"}}
    client = FakeClient([_response("r1", **cut), _response("r2", _message("finished"))])
    a, _ = _agent(client, FakeWorker(), tmp_path)
    assert asyncio.run(a.run("x")) == "finished"
    assert new_items(client, 1) == [{"type": "message", "role": "user", "content": [
        {"type": "input_text", "text": "[note] Your reply was cut off; continue."}]}]
    client = FakeClient([_response("r1", **cut), _response("r2", **cut)])
    a, _ = _agent(client, FakeWorker(), tmp_path)
    assert asyncio.run(a.run("x")) == "Sorry, that failed unexpectedly."


def test_failure_text_is_human() -> None:
    class APITimeoutError(Exception):
        pass

    assert "crashed twice" in describe_failure(WorkerDead("deadline, twice"))
    assert "lost my connection" in describe_failure(APITimeoutError("slow"))
    assert describe_failure(ValueError("x")) == "Sorry, that failed unexpectedly."
    assert "grant Screen Recording" in describe_failure(RuntimeError("Screen Recording is not granted"))


def test_progress_events_from_short_text_lines(tmp_path: Path) -> None:
    result = [{"type": "input_text", "text": t} for t in (
        "opened Spotify", '{"app": "Spotify", "title": "Spotify"}',
        "[after] frontmost: Spotify — 'Spotify'; screen: changed", "Traceback (most recent call last):\n  x")]
    client = FakeClient([_response("r1", _call("exec_py", "c1", code="open_app('Spotify')")),
                         _response("r2", _message("ok"))])
    a, events = _agent(client, FakeWorker({"open_app('Spotify')": result}), tmp_path)
    seen: list[str] = []
    a.on_event = lambda ev: (events.append(ev), seen.append(a.status()["last"]) if ev.kind == "progress" else None)
    asyncio.run(a.run("x"))
    progress = [e for e in events if e.kind == "progress"]
    assert progress == [AgentEvent("progress", "opened Spotify", 1)]
    assert seen == ["opened Spotify"]                  # status()["last"] while the task runs
    assert ca.progress_lines([{"type": "input_text", "text": "hi"}, {"type": "input_image"},
                              {"type": "input_text", "text": "typed 22 characters"},
                              {"type": "input_text", "text": "typed 22 characters"}]) == ["typed 22 characters"]


def test_action_log_records_goal_exec_results_and_final(tmp_path: Path) -> None:
    client = FakeClient([
        _response("r1", _call("exec_py", "c1", code="display(pyautogui.screenshot())")),
        _response("r2", _message("all done")),
    ])
    worker = FakeWorker({"display(pyautogui.screenshot())": [
        {"type": "input_text", "text": "hi"}, {"type": "input_image", "detail": "original", "image_url": "data:x"}]})
    a, _ = _agent(client, worker, tmp_path)
    asyncio.run(a.run("look"))
    assert a.run_log is not None and a.run_log.parent == tmp_path / "runs"
    lines = [json.loads(ln) for ln in a.run_log.read_text().splitlines()]
    assert lines[0]["goal"] == "look" and lines[0]["model"] == "gpt-6-astra"
    assert lines[1] == {"t": 1.0, "worker": {"width": 100, "height": 50}}          # the ready line, minus "ready"
    assert lines[2] == {"t": 1.0, "turn": 0, "context": "frontmost: Warp — 'zsh'; screen 100x50; 14:02"}
    assert lines[3] == {"t": 1.0, "turn": 1, "effort": "high", "api_secs": 0.0}
    assert lines[4]["exec"] == "display(pyautogui.screenshot())"
    assert lines[5] == {"t": 1.0, "turn": 1, "result": ["hi"], "images": 1}     # no image bytes, no timing (fake worker)
    assert lines[-2]["final"] == "all done" and "bill" in lines[-1]              # the bill closes every log
    assert "data:x" not in a.run_log.read_text() and "AAAA" not in a.run_log.read_text()



# ---- phase, usage, and the final-answer check ------------------------------------------

def _phased(text: str, phase: str) -> dict:
    return {**_message(text), "phase": phase}


def test_classify_reads_phase() -> None:
    c = classify_response(_response("r1", _phased("Spotify is playing, let me check", "commentary")))
    assert c.kind == "commentary" and c.text == "Spotify is playing, let me check" and c.phases == ["commentary"]
    c = classify_response(_response("r2", _phased("looking", "commentary"), _phased("It is playing.", "final_answer")))
    assert c.kind == "final" and c.text == "It is playing."
    assert classify_response(_response("r3", _message("no phase is an answer"))).kind == "final"


def test_phase_commentary_does_not_end_the_run(tmp_path: Path) -> None:
    client = FakeClient([_response("r1", _phased("Spotify is playing, let me check", "commentary")),
                         _response("r2", _phased("Spotify is playing.", "final_answer"))])
    a, events = _agent(client, FakeWorker(), tmp_path)
    assert asyncio.run(a.run("play")) == "Spotify is playing."
    assert new_items(client, 1) == []
    assert [e.kind for e in events].count("commentary") == 1
    logged = [json.loads(ln) for ln in a.run_log.read_text().splitlines()]
    assert [ln["phase"] for ln in logged if "phase" in ln] == [["commentary"], ["final_answer"]]


def test_usage_tokens_are_logged_per_turn(tmp_path: Path) -> None:
    usage = {"input_tokens": 1200, "output_tokens": 90, "output_tokens_details": {"reasoning_tokens": 64}}
    client = FakeClient([_response("r1", _message("ok"), usage=usage)])
    a, _ = _agent(client, FakeWorker(), tmp_path)
    asyncio.run(a.run("x"))
    [line] = [json.loads(ln) for ln in a.run_log.read_text().splitlines() if "api_secs" in ln]
    assert line["tokens"] == {"in": 1200, "out": 90, "reasoning": 64}


def test_verifier_passes_a_confirmed_claim(tmp_path: Path) -> None:
    client = FakeClient([_response("r1", _call("exec_py", "c1", code="pyautogui.press('space')")),
                         _response("r2", _message("Spotify is playing.")),
                         _verdict("v1", True)])
    a, _ = _agent(client, FakeWorker({"pyautogui.press('space')": ACTED}), tmp_path)
    assert asyncio.run(a.run("play spotify")) == "Spotify is playing."
    assert len(client.requests) == 3
    logged = [json.loads(ln) for ln in a.run_log.read_text().splitlines()]
    assert [ln["verify"]["valid"] for ln in logged if "verify" in ln] == [True]


def test_verifier_sends_back_an_unconfirmed_claim_once(tmp_path: Path) -> None:
    client = FakeClient([_response("r1", _call("exec_py", "c1", code="pyautogui.press('space')")),
                         _response("r2", _message("Spotify is playing.")),
                         _verdict("v1", False, "Finder is in front and Spotify shows a play button."),
                         _response("r3", _call("exec_py", "c3", code="open_app('Spotify')")),
                         _response("r4", _message("Now Spotify is playing.")),
                         _verdict("v2", True)])
    worker = FakeWorker({"pyautogui.press('space')": ACTED, "open_app('Spotify')": ACTED})
    a, _ = _agent(client, worker, tmp_path)
    assert asyncio.run(a.run("play spotify")) == "Now Spotify is playing."
    retry = client.requests[3]
    assert retry["reasoning"] == {"effort": "high"}
    [note] = new_items(client, 3, 1)                 # the loop's request before it is r2's (index 1)
    text = note["content"][0]["text"]
    assert text.startswith("[note] A fresh screenshot does not confirm your answer:")
    assert "Finder is in front" in text


def test_verifier_second_failure_says_it_could_not_confirm(tmp_path: Path) -> None:
    client = FakeClient([_response("r1", _call("exec_py", "c1", code="pyautogui.press('space')")),
                         _response("r2", _message("Spotify is playing.")),
                         _verdict("v1", False, "A play button is showing."),
                         _response("r3", _message("It is playing now.")),
                         _verdict("v2", False, "A play button is still showing.")])
    a, events = _agent(client, FakeWorker({"pyautogui.press('space')": ACTED}), tmp_path)
    final = asyncio.run(a.run("play spotify"))
    assert final == "I couldn't confirm that on screen. It is playing now."
    assert events[-1] == AgentEvent("final", final, 3)             # turn 3: the retried answer


def test_verifier_skipped_for_read_only_runs(tmp_path: Path) -> None:
    client = FakeClient([_response("r1", _call("exec_py", "c1", code="log(screen_text())")),
                         _response("r2", _message("Google shows 60°F."))])
    a, _ = _agent(client, FakeWorker({"log(screen_text())": LOOKED}), tmp_path)
    assert asyncio.run(a.run("weather")) == "Google shows 60°F."
    assert len(client.requests) == 2                  # no check: a run that only looked cannot have misfired


def test_verifier_failure_lets_the_answer_through(tmp_path: Path) -> None:
    client = FakeClient([_response("r1", _call("exec_py", "c1", code="pyautogui.press('space')")),
                         _response("r2", _message("Spotify is playing.")),
                         _response("v1", status="failed")])
    a, _ = _agent(client, FakeWorker({"pyautogui.press('space')": ACTED}), tmp_path)
    assert asyncio.run(a.run("play spotify")) == "Spotify is playing."
    logged = [json.loads(ln) for ln in a.run_log.read_text().splitlines()]
    [entry] = [ln["verify"] for ln in logged if "verify" in ln]
    assert entry == {"error": "RuntimeError"}


def test_verifier_request_shape(tmp_path: Path) -> None:
    client = FakeClient([_response("r1", _call("exec_py", "c1", code="pyautogui.press('space')")),
                         _response("r2", _message("Spotify is playing.")),
                         _verdict("v1", True)])
    a, _ = _agent(client, FakeWorker({"pyautogui.press('space')": ACTED}), tmp_path)
    asyncio.run(a.run("play spotify"))
    req = client.requests[2]
    assert "previous_response_id" not in req and "tools" not in req     # judges the screen, not the story
    assert req["instructions"] == ca.VERIFY_INSTRUCTIONS and req["store"] is False
    assert req["text"]["format"] == {"type": "json_schema", "name": "verdict", "schema": ca.VERIFY_SCHEMA,
                                     "strict": True}
    assert req["reasoning"] == {"effort": "low"}
    [msg] = req["input"]
    text, image = msg["content"]
    assert "Goal: play spotify" in text["text"] and "Spotify is playing." in text["text"]
    assert "[after your input] frontmost: Spotify" in text["text"]
    assert image == IMAGE                                                  # a FRESH observe screenshot


# ---- the fast lane's log fields: worker timing -------------------------------------------

class TimedWorker(FakeWorker):
    """A FakeWorker that reports a timing dict."""

    def __init__(self, results=None) -> None:
        super().__init__(results)
        self.last_timing: dict | None = None

    async def execute(self, code: str) -> list[dict]:
        out = await super().execute(code)
        self.last_timing = {"capture": 37.0, "settle": 330.0, "exec": 402.5}
        return out


def test_loop_logs_worker_timing(tmp_path: Path) -> None:
    client = FakeClient([_response("r1", _call("exec_py", "c1", code="log(screen_text())")),
                         _response("r2", _message("done"))])
    a, _ = _agent(client, TimedWorker({"log(screen_text())": LOOKED}), tmp_path)
    asyncio.run(a.run("look"))
    logged = [json.loads(ln) for ln in a.run_log.read_text().splitlines()]
    [entry] = [ln for ln in logged if "result" in ln]
    assert entry["timing"] == {"capture": 37.0, "settle": 330.0, "exec": 402.5}


def test_worker_ready_line_is_logged(tmp_path: Path, caplog) -> None:
    client = FakeClient([_response("r1", _message("done"))])

    class ReadyWorker(FakeWorker):
        async def start(self) -> dict:
            self.started = True
            return {"ready": True, "width": 100, "height": 50, "fast_lane": "loading"}
    a, _ = _agent(client, ReadyWorker(), tmp_path)
    with caplog.at_level("INFO"):
        asyncio.run(a.run("x"))
    assert "agent: worker ready — fast lane loading" in caplog.text
    logged = [json.loads(ln) for ln in a.run_log.read_text().splitlines()]
    assert logged[1] == {"t": 1.0, "worker": {"width": 100, "height": 50, "fast_lane": "loading"}}


# ---- the fast lane in the prompt and the tool text ------------------------------------------

def test_instructions_variants_track_fast_lane_default() -> None:
    """instructions(False) never names the helper; instructions(True) documents it; the shipped
    default variant is the one the eval decided (fast_lane.FAST_LANE_DEFAULT)."""
    from eve_bridge.fast_lane import FAST_LANE_DEFAULT

    off, on = ca.instructions(False), ca.instructions(True, "model")
    assert "delegate" not in off and "delegate" not in json.dumps(ca.tools(False))
    assert "11. delegate(objective" in on and "approve=" in on and "confirm" in on and "escalate" in on
    keyword = ca.instructions(True)                      # the default decide mode: exact labels, in order
    assert "11. delegate(steps=[" in keyword and "approve=" in keyword and "script: complete" in keyword
    assert "done_when" not in keyword and "{fast_lane" not in keyword
    assert ", delegate" in ca.tools(True)[0]["parameters"]["properties"]["code"]["description"]
    assert "{fast_lane" not in on and "{fast_lane" not in off
    assert ca.AgentConfig().fast_lane is FAST_LANE_DEFAULT and configured({}).fast_lane is FAST_LANE_DEFAULT
    assert configured({"EVE_FAST_LANE": "1"}).fast_lane is True
    assert configured({"EVE_FAST_LANE": "off"}).fast_lane is False
    assert INSTRUCTIONS == off and TOOLS == ca.tools(False)


def test_instructions_tool_text_and_helper_names_agree_in_both_variants() -> None:
    from eve_bridge.desktop_helpers import FAST_LANE_HELPERS

    for fast_lane in (False, True):
        text, code_desc = ca.instructions(fast_lane), ca.tools(fast_lane)[0]["parameters"]["properties"]["code"]["description"]
        for name in HELPER_NAMES:
            assert name in text and name in code_desc, name
        for name in FAST_LANE_HELPERS:
            assert (name in text) is fast_lane and (name in code_desc) is fast_lane, (name, fast_lane)


def test_loop_sends_the_variant_the_config_selects_and_escalates_on_delegate_lines(tmp_path: Path) -> None:
    line = [{"type": "input_text", "text": 'delegate confirm: "Send" needs the human\'s yes; app="Mail"; action=click; steps=0; last=none; text=none; key=none'}]
    client = FakeClient([_response("r1", _call("exec_py", "c1", code="delegate('send it')")),
                         _response("r2", _call("exec_py", "c2", code="log(1)")),
                         _response("r3", _message("done"))])
    cfg = AgentConfig(runs_dir=tmp_path / "runs", fast_lane=True)
    a, _ = _agent(client, FakeWorker({"delegate('send it')": line}), tmp_path, config=cfg)
    asyncio.run(a.run("send the mail"))
    assert client.requests[0]["instructions"] == ca.instructions(True) and client.requests[0]["tools"] == ca.tools(True)
    assert client.requests[1]["reasoning"] == {"effort": "high"}         # the confirm is a recovery turn
    assert client.requests[2]["reasoning"] == {"effort": "medium"}


# ---- the bill per task (the Jev Engineering article: "track the bill per completed task") ----

def test_the_run_log_ends_with_the_bill(tmp_path: Path) -> None:
    from eve_bridge import jev

    usage = {"input_tokens": 1_000_000, "input_tokens_details": {"cached_tokens": 200_000}, "output_tokens": 100_000}
    client = FakeClient([_response("r1", _message("ok"), usage=usage)])
    a, _ = _agent(client, FakeWorker(), tmp_path, clock=iter([1.0] * 3 + [7.5] * 40).__next__)
    jev.METER.add({"usage": {"input_tokens": 500}}, 230.0)          # a Jev call made before the run: not billed
    run_task = a.run("x")
    asyncio.run(run_task)
    lines = [json.loads(ln) for ln in a.run_log.read_text().splitlines()]
    assert "bill" in lines[-1] and "final" in lines[-2]
    bill = lines[-1]["bill"]
    assert bill["model"] == "gpt-6-astra" and bill["calls"] == 1
    assert bill["tokens"] == {"in": 1_000_000, "cached": 200_000, "out": 100_000}
    assert abs(bill["usd"] - 13.2) < 1e-6 and bill["jev"] == {"calls": 0, "input_tokens": 0, "ms": 0, "errors": 0, "usd": 0.0}
    assert abs(bill["total_usd"] - 13.2) < 1e-6 and bill["secs"] == 6.5
    # a model the table does not know: tokens yes, dollars null, never a guess
    cfg = AgentConfig(runs_dir=tmp_path / "runs2", model="gpt-9-mystery", reflexes=False, lane_first=False)
    b, _ = _agent(FakeClient([_response("r1", _message("ok"), usage=usage)]), FakeWorker(), tmp_path, config=cfg)
    asyncio.run(b.run("y"))
    bill = json.loads(b.run_log.read_text().splitlines()[-1])["bill"]
    assert bill["usd"] is None and bill["total_usd"] is None and bill["tokens"]["in"] == 1_000_000


def test_jev_calls_during_a_run_are_billed(tmp_path: Path) -> None:
    from eve_bridge import jev

    client = FakeClient([_response("r1", _message("ok"), usage={"input_tokens": 10, "output_tokens": 1})])
    a, _ = _agent(client, FakeWorker(), tmp_path)

    class Worker(FakeWorker):
        async def start(self) -> dict:
            jev.METER.add({"usage": {"input_tokens": 3000}}, 240.0)      # a step asked mid-run
            jev.METER.add({}, 3000.0, error=True)                        # and one that failed
            return await super().start()

    a.worker_factory = lambda: Worker()
    asyncio.run(a.run("x"))
    bill = json.loads(a.run_log.read_text().splitlines()[-1])["bill"]
    assert bill["jev"]["calls"] == 2 and bill["jev"]["input_tokens"] == 3000 and bill["jev"]["errors"] == 1
    assert abs(bill["jev"]["usd"] - 3000 * 0.042 / 1_000_000) < 1e-12 and bill["jev"]["ms"] == 3240


def test_bill_report_sums_the_runs(tmp_path: Path) -> None:
    import bill_report  # tools/ is on the pytest path (pyproject.toml)

    runs = tmp_path / "runs"
    runs.mkdir()
    (runs / "2026-09-21-100000.jsonl").write_text(
        json.dumps({"goal": "open mail"}) + "\n" + json.dumps({"turn": 1}) + "\n"
        + json.dumps({"bill": {"model": "gpt-6-astra", "calls": 2, "tokens": {"in": 100, "cached": 20, "out": 10},
                               "usd": 0.0013, "jev": {"calls": 3, "input_tokens": 900, "ms": 700, "errors": 0, "usd": 0.0000378},
                               "total_usd": 0.0013378, "secs": 4.2}}) + "\n")
    (runs / "2026-09-21-110000.jsonl").write_text(
        json.dumps({"goal": "old run"}) + "\n" + json.dumps({"final": "done"}) + "\n")          # no bill: older log
    (runs / "2026-09-21-120000.jsonl").write_text(
        json.dumps({"goal": "mystery"}) + "\n" + json.dumps({"bill": {"model": "gpt-9", "calls": 1, "tokens": {"in": 5, "cached": 0, "out": 1},
                                                                       "usd": None, "jev": {"calls": 0, "input_tokens": 0, "ms": 0, "errors": 0, "usd": 0.0},
                                                                       "total_usd": None, "secs": 1.0}}) + "\n"
        + "not json\n")
    rows, without = bill_report.read_bills(runs)
    assert [r["goal"] for r in rows] == ["open mail", "mystery"] and without == 1
    total = bill_report.summarize(rows)
    assert total["runs"] == 2 and total["unpriced"] == 1 and abs(total["usd"] - 0.0013) < 1e-9
    assert total["model_in"] == 105 and total["jev_calls"] == 3 and total["jev_tokens"] == 900 and total["secs"] == 5.2
    assert bill_report.main(["--runs", str(runs)]) == 0
    empty = tmp_path / "empty"
    empty.mkdir()
    assert bill_report.main(["--runs", str(empty)]) == 0                # says "no bill lines", never $0
    assert bill_report.main(["--runs", str(tmp_path / "missing")]) == 1
