"""The gate withdraw harness's plumbing (tools/gate_withdraw_eval.py), with its scripted brain: no API call, no env
file. It proves the harness drives the real inlet the way the bar (docs/evals/gate-withdraw-BAR.md) needs, and that
no real executor is reachable from it; it says nothing about the model."""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

import pytest

BRIDGE = Path(__file__).resolve().parents[1]
TOOL = BRIDGE / "tools" / "gate_withdraw_eval.py"
SMOKE = BRIDGE / "tests" / "fixtures" / "gate_withdraw_smoke.json"
TOOLS = BRIDGE / "tests" / "fixtures" / "gate_withdraw_tools.json"
SAFE_ROUTES = {"open-app", "fake-google", "fake-watcher", "fake-maker", "recorder"}
READ_ROUTES = {"canned-read", "chat-log"}


def _run(arm: str, tmp: Path) -> dict:
    out = tmp / f"{arm}.json"
    done = subprocess.run([sys.executable, str(TOOL), "--arm", arm, "--fake-model", "--cases", str(SMOKE),
                           "--json", str(out)], cwd=BRIDGE, capture_output=True, text=True, timeout=300)
    assert done.returncode == 0, done.stderr[-2000:]
    return json.loads(out.read_text())


@pytest.fixture(scope="module")
def runs(tmp_path_factory: pytest.TempPathFactory) -> dict[str, dict]:
    tmp = tmp_path_factory.mktemp("gate-withdraw")
    return {arm: _run(arm, tmp) for arm in ("on", "off")}


def _rows(run: dict, kind: str) -> list[dict]:
    rows = [r for r in run["rows"] if r["kind"] == kind]
    assert rows, kind
    return rows


def test_arms_differ_only_in_the_switch(runs: dict[str, dict]) -> None:
    assert runs["on"]["withdraw_env"] == "1"
    assert runs["off"]["withdraw_env"] == ""
    for run in runs.values():
        assert run["fake_model"] is True
        assert run["ask_timeout_harness_secs"] < run["ask_timeout_real_secs"]
        assert all(r["outcome"] == "ok" and not r["provider_error"] for r in run["rows"])


def test_on_arm_withdraws_and_never_runs_the_original_call(runs: dict[str, dict]) -> None:
    for r in _rows(runs["on"], "correction"):
        assert r["first_answer"] == "withdrawn"
        assert r["original_executed"] == 0
        assert not r["reply_ran_as_next_turn"]          # the same turn read the words
        assert r["executed"] and r["executed"][-1]["tool"] == r["tool"]   # the corrected call, after a yes
        assert r["latency_s"] is not None


def test_off_arm_times_out_and_runs_the_reply_as_the_next_turn(runs: dict[str, dict]) -> None:
    for r in _rows(runs["off"], "correction"):
        assert r["first_answer"] == "timeout"
        assert r["reply_ran_as_next_turn"]
        assert r["original_executed"] == 0
        assert r["latency_s"] >= runs["off"]["ask_timeout_harness_secs"] - 0.1
        assert r["latency_daemon_s"] >= runs["off"]["ask_timeout_real_secs"] - 0.1   # the daemon's real wait


def test_controls_are_identical_across_arms(runs: dict[str, dict]) -> None:
    on = {r["id"]: r for r in _rows(runs["on"], "control")}
    off = {r["id"]: r for r in _rows(runs["off"], "control")}
    assert on.keys() == off.keys()
    for cid in on:
        assert on[cid]["first_answer"] in ("yes", "no")
        assert on[cid]["first_answer"] == off[cid]["first_answer"]
        pick = lambda r: [(e["tool"], e["args"]) for e in r["executed"]]   # noqa: E731
        assert pick(on[cid]) == pick(off[cid])


def test_every_executed_act_had_a_yes_for_that_exact_call(runs: dict[str, dict]) -> None:
    for run in runs.values():
        for r in run["rows"]:
            assert r["unapproved_executed"] == 0
            assert all(e["preceded_by_yes"] for e in r["executed"])
            assert all(e["needs_yes"] for e in r["executed"])          # every scripted call is a gated one
            assert r["routine_unasked_executed"] == 0


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
            # every tool of tool_gate.POLICY that acts and is offered is a recorder or a stand-in
            acting = {n for n in r["offered"] if tool_gate.policy(n) not in (None, "read")}
            assert acting and all(r["routes"][n] in SAFE_ROUTES for n in acting)


def test_the_drivable_tools_are_gated_and_offered(runs: dict[str, dict]) -> None:
    sys.path.insert(0, str(BRIDGE / "src"))
    from eve_bridge import google_rest, tool_gate

    listed = json.loads(TOOLS.read_text())["tools"]
    assert listed and all({"name", "what", "args"} <= t.keys() for t in listed)
    writes = {f"{app}__{t['name']}" for app, ts in google_rest.TOOLS.items() for t in ts
              if not t["annotations"]["readOnlyHint"]}
    for t in listed:
        assert t["name"] in writes or tool_gate.policy(t["name"]) == "asks", t["name"]
    offered = set().union(*(set(r["offered"]) for r in runs["on"]["rows"]))
    for case in json.loads(SMOKE.read_text())["cases"]:
        assert case["tool"] in offered


def test_the_scripted_brain_has_real_args_for_every_drivable_tool() -> None:
    sys.path.insert(0, str(BRIDGE / "tools"))
    import gate_withdraw_eval as gw

    listed = {t["name"] for t in json.loads(TOOLS.read_text())["tools"]}
    assert listed <= gw.FAKE_ARGS.keys(), sorted(listed - gw.FAKE_ARGS.keys())
