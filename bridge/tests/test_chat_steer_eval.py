"""The chat steering harness's plumbing (tools/chat_steer_eval.py), with its scripted brain: no API call, no env file.
It proves the harness drives the real inlet the way the bar (docs/evals/chat-steer-BAR.md) needs (a "during" second
message folded into one turn on the on arm, two turns on the off arm, "after" controls the same in both) and that no
real executor is reachable from it; it says nothing about the model."""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

import pytest

BRIDGE = Path(__file__).resolve().parents[1]
TOOL = BRIDGE / "tools" / "chat_steer_eval.py"
SMOKE = BRIDGE / "tests" / "fixtures" / "chat_steer_smoke.json"
TOOLS = BRIDGE / "tests" / "fixtures" / "chat_steer_tools.json"
SAFE_ROUTES = {"open-app", "fake-google", "fake-watcher", "fake-maker", "recorder"}
READ_ROUTES = {"canned-read", "chat-log", "canned-web"}


def _run(arm: str, tmp: Path) -> dict:
    out = tmp / f"{arm}.json"
    done = subprocess.run([sys.executable, str(TOOL), "--arm", arm, "--fake-model", "--cases", str(SMOKE),
                           "--json", str(out)], cwd=BRIDGE, capture_output=True, text=True, timeout=300)
    assert done.returncode == 0, done.stderr[-2000:]
    return json.loads(out.read_text())


@pytest.fixture(scope="module")
def runs(tmp_path_factory: pytest.TempPathFactory) -> dict[str, dict]:
    tmp = tmp_path_factory.mktemp("chat-steer")
    return {arm: _run(arm, tmp) for arm in ("on", "off")}


def _rows(run: dict, delay: str) -> list[dict]:
    rows = [r for r in run["rows"] if r["delay"] == delay]
    assert rows, delay
    return rows


def _calls(r: dict) -> list[tuple[str, str]]:
    return [(e["tool"], json.dumps(e["args"], sort_keys=True)) for e in r["executed"]]


def test_arms_differ_only_in_the_switch(runs: dict[str, dict]) -> None:
    assert runs["on"]["steer_env"] == "1"
    assert runs["off"]["steer_env"] == ""
    for run in runs.values():
        assert run["fake_model"] is True
        assert run["withdraw_env"] == ""
        assert all(r["outcome"] == "ok" and not r["provider_error"] for r in run["rows"])
        assert len(run["rows"]) == 3


def test_on_arm_folds_a_during_message_into_one_turn(runs: dict[str, dict]) -> None:
    cases = {c["id"]: c for c in json.loads(SMOKE.read_text())["cases"]}
    for r in _rows(runs["on"], "during"):
        assert r["second_sent"] == "during"
        assert r["turns"] == 1 and r["folded"] == 1
        assert r["folds"][0]["round"] >= 2                     # never into the call already in flight
        assert not r["second_own_turn"]
        assert r["executed"][-1]["tool"] == cases[r["id"]]["fake"]["second"]["tool"]
        assert r["latency_s"] is not None


def test_off_arm_runs_a_during_message_as_its_own_turn(runs: dict[str, dict]) -> None:
    for r in _rows(runs["off"], "during"):
        assert r["second_sent"] == "during"
        assert r["turns"] == 2 and r["folded"] == 0
        assert r["second_own_turn"]
        assert r["latency_s"] is not None


def test_both_arms_run_the_same_calls_on_double_texts_with_the_scripted_brain(runs: dict[str, dict]) -> None:
    on = {r["id"]: r for r in _rows(runs["on"], "during")}
    off = {r["id"]: r for r in _rows(runs["off"], "during")}
    assert on.keys() == off.keys()
    for cid in on:
        assert _calls(on[cid]) == _calls(off[cid])


def test_after_controls_are_identical_across_arms(runs: dict[str, dict]) -> None:
    on = {r["id"]: r for r in _rows(runs["on"], "after")}
    off = {r["id"]: r for r in _rows(runs["off"], "after")}
    assert on.keys() == off.keys()
    for cid in on:
        for r in (on[cid], off[cid]):
            assert r["second_sent"] == "after" and r["turns"] == 2 and r["folded"] == 0 and r["second_own_turn"]
        assert _calls(on[cid]) == _calls(off[cid])
        assert [q["answer"] for q in on[cid]["questions"]] == [q["answer"] for q in off[cid]["questions"]]


def test_every_executed_act_had_a_yes_for_that_exact_call(runs: dict[str, dict]) -> None:
    for run in runs.values():
        for r in run["rows"]:
            assert r["executed"]
            assert r["unapproved_executed"] == 0 and r["duplicate_acts"] == 0
            assert all(e["preceded_by_yes"] and e["needs_yes"] for e in r["executed"])
            assert r["gate_questions"] == len(r["executed"])
            assert all(q["answer"] == "yes" for q in r["questions"])


def test_duplicate_acts_counts_extra_runs_of_one_acting_call() -> None:
    sys.path.insert(0, str(BRIDGE / "tools"))
    import chat_steer_eval as cs

    one = {"tool": "watch_remove", "args": {"id": "w1"}, "needs_yes": True}
    other = {"tool": "watch_remove", "args": {"id": "w2"}, "needs_yes": True}
    routine = {"tool": "calendar__create_event", "args": {"summary": "Gym"}, "needs_yes": False}
    assert cs.duplicate_acts([one, other]) == 0
    assert cs.duplicate_acts([one, dict(one), dict(one), other]) == 2
    assert cs.duplicate_acts([routine, dict(routine)]) == 0


def test_no_real_executor_is_reachable(runs: dict[str, dict]) -> None:
    sys.path.insert(0, str(BRIDGE / "src"))
    from eve_bridge import tool_gate

    for run in runs.values():
        for r in run["rows"]:
            assert r["offered"], "the brain was offered tools"
            for name, route in r["routes"].items():
                assert route in SAFE_ROUTES | READ_ROUTES, (name, route)
                if "__" in name:
                    assert route == "fake-google", name
                kind = tool_gate.policy(name)
                if kind != "read" or tool_gate.acting(name, {}):
                    assert route in SAFE_ROUTES, (name, route)
            acting = {n for n in r["offered"] if tool_gate.policy(n) not in (None, "read")}
            assert acting and all(r["routes"][n] in SAFE_ROUTES for n in acting)


def test_the_web_stub_is_a_read_and_answers_canned_words() -> None:
    sys.path.insert(0, str(BRIDGE / "tools"))
    sys.path.insert(0, str(BRIDGE / "src"))
    import chat_steer_eval as cs

    from eve_bridge import tool_gate

    for name in cs.WEB_TOOLS:
        assert tool_gate.policy(name) == "read" and not tool_gate.acting(name, {})
    assert "sunny" in cs.web_answer({"query": "weather in town tomorrow"})["answer"]
    assert cs.web_answer({"query": "something else"})["ok"] is True


def test_the_drivable_tools_are_listed_gated_and_offered(runs: dict[str, dict]) -> None:
    sys.path.insert(0, str(BRIDGE / "src"))
    from eve_bridge import google_rest, tool_gate

    listed = json.loads(TOOLS.read_text())
    withdraw = json.loads((BRIDGE / "tests" / "fixtures" / "gate_withdraw_tools.json").read_text())["tools"]
    assert listed["tools"] == withdraw
    writes = {f"{app}__{t['name']}" for app, ts in google_rest.TOOLS.items() for t in ts
              if not t["annotations"]["readOnlyHint"]}
    reads = {f"{app}__{t['name']}" for app, ts in google_rest.TOOLS.items() for t in ts
             if t["annotations"]["readOnlyHint"]}
    for t in listed["tools"]:
        assert t["name"] in writes or tool_gate.policy(t["name"]) == "asks", t["name"]
    for t in listed["reads"]:
        assert {"name", "what", "args"} <= t.keys()
        assert t["name"] in reads or tool_gate.policy(t["name"]) == "read", t["name"]
    offered = set().union(*(set(r["offered"]) for r in runs["on"]["rows"]))
    for case in json.loads(SMOKE.read_text())["cases"]:
        for step in case["fake"].values():
            assert step["tool"] in offered
