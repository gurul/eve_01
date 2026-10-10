"""Paired grounding eval: committed AX development fixtures, unchanged provider gates.

This measures target selection, not input delivery or task completion. Decisions receives
AX evidence only in this slice; its image behaviour is measured separately and labelled.
"""
from __future__ import annotations

import argparse
import json
import math
import os
import statistics
import sys
import time
from dataclasses import asdict, replace
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "src"))
sys.path.insert(0, str(Path(__file__).resolve().parent))

import browser_model_eval as be  # noqa: E402
import fastlane_eval as fe  # noqa: E402
import jev_step_eval as je  # noqa: E402

from eve_bridge import decision_lane, decisions, lane_router, typed_ask  # noqa: E402
from eve_bridge.decider import RESERVED  # noqa: E402
from eve_bridge.decision_lane import Gates, evidence  # noqa: E402
from eve_bridge.envfile import load_env_file  # noqa: E402
from eve_bridge.fast_lane import jev_context  # noqa: E402

ARMS = ("Keyword", "Jev", "Decisions (AX only)", "Keyword → Jev", "Keyword → Decisions")
ADMISSION_ARMS = ("Guarded keyword", "Guarded keyword → Decisions (AX only)")
ADMISSION_LIMITATION = ("Real admission code with frozen fixture observations and recording effectors. "
                        "Decisions receives AX only; no screen image is transmitted. Verification is forced "
                        "false without a model call. This measures admitted input, never live task success.")


def raw_answers(fixture, goal, real, predict, client):
    """An empty menu is a local abstention, never a paid one-option request."""
    if not real:
        return (typed_ask.StepAnswer("none", 0, 0, 0, 0, 1, requests=0), decisions.Answer())
    j = typed_ask.ask_jev_step(predict, goal, app=fixture.app, context=jev_context(fixture.snapshot),
                               options=real, clock=time.perf_counter)
    a = client.evaluate(evidence(goal, fixture.snapshot), [], decisions.selection_questions(real))
    return j, a


class AdmissionSenses(fe._FixtureSenses):
    """Frozen AX replay: image checks are assumptions, not captured image evidence."""

    def observe(self):
        return self._snapshot, None

    def image_matches(self, before, after):
        return True


class AdmissionClient:
    def __init__(self, client=None, calls=None):
        self.client = client
        self.calls = [] if calls is None else calls
        self.replayed = 0

    def evaluate(self, state, images, questions):
        if len(questions) == 1:
            return decisions.Answer({"verified": 0})
        if self.client is not None:
            answer = self.client.evaluate(state, [], questions)
            self.calls.append({"state": state, "questions": questions, "answer": asdict(answer)})
            return answer
        assert self.replayed < len(self.calls), "unrecorded target call"
        call = self.calls[self.replayed]
        self.replayed += 1
        assert call["state"] == state and call["questions"] == questions, "admission menu/evidence differs"
        return decisions.Answer(**call["answer"])


def admission(fixture, goal, client=None, calls=None):
    """Measure shipped keyword admission then the opt-in Decisions fallback, without OS input."""
    # Fixture process IDs are historical/possibly absent; use an explicitly simulated live PID
    # for both routers, rather than claiming a historical process is currently running.
    snapshot = replace(fixture.snapshot, pid=42)
    senses, effectors = AdmissionSenses(snapshot), fe._RecordingEffectors()
    t0 = time.perf_counter()
    keyword = lane_router.route(goal, senses=senses, effectors=effectors,
                                frontmost_app=fixture.app, apps=fe.ROUTER_APPS)
    keyword_ms = (time.perf_counter() - t0) * 1000
    keyword_ids = [c.id for c in effectors.clicked]
    common = {"status": keyword.status, "reason": keyword.reason,
              "admitted_ids": keyword_ids, "already": keyword.already,
              "ms": keyword_ms, "model_ms": 0.0, "local_ms": keyword_ms, "calls": []}
    model = AdmissionClient(client, calls)
    if keyword.status == "none" and keyword.reason in lane_router.RETRY_AS_CLAUSES:
        result = decision_lane.route(goal, senses=senses, effectors=effectors,
                                     client=model, apps=fe.ROUTER_APPS)
    else:
        result = keyword
    ms = (time.perf_counter() - t0) * 1000
    model_ms = sum(c["answer"]["ms"] for c in model.calls)
    combined = {"status": result.status, "reason": result.reason,
                "admitted_ids": [c.id for c in effectors.clicked], "already": result.already,
                "ms": ms, "model_ms": model_ms, "local_ms": ms-model_ms, "calls": model.calls}
    if client is None:
        assert model.replayed == len(model.calls), "unused recorded target call"
    return common, combined


def admission_options(calls):
    if not calls:
        return {}
    return {c["value"]: c["description"] for c in calls[0]["questions"][0]["choices"]
            if c["value"] != "none"}


def validate_answer(answer, provider):
    assert math.isfinite(answer["ms"]) and answer["ms"] >= 0
    assert isinstance(answer["error"], str)
    if provider == "Decisions":
        values = answer["values"]
        numbers = [v for key, v in values.items() if key != "target"]
        if "target" in values:
            assert isinstance(values["target"]["id"], str)
            numbers += [values["target"]["p"], values["target"]["margin"]]
    else:
        numbers = [answer[k] for k in ("p_target", "margin", "present", "already_done", "risky")]
    assert all(not isinstance(v, bool) and isinstance(v, (float, int))
               and math.isfinite(v) and 0 <= v <= 1 for v in numbers), "invalid recorded probability"


def pressed_ids(row):
    return row.get("admitted_ids", [] if row["picked"] == "none" else [row["picked"]])


def is_right(row):
    return pressed_ids(row) == [row["expected"]]


def metrics(rows):
    result = {}
    for arm in (*ARMS, *[a for a in ADMISSION_ARMS if any(r["arm"] == a for r in rows)]):
        subset = [r for r in rows if r["arm"] == arm]
        clicks = [r for r in subset if r["expected"] != "none"]
        pressed = [r for r in subset if pressed_ids(r)]
        right = [r for r in pressed if is_right(r)]
        times = sorted(r["ms"] for r in subset)
        result[arm] = {"n": len(subset), "click_cases": len(clicks), "pressed": len(pressed),
                       "correct_clicks": len(right), "wrong_clicks": len(pressed)-len(right),
                       "coverage": len(right)/len(clicks) if clicks else 0,
                       "precision": len(right)/len(pressed) if pressed else None,
                       "accuracy": sum(is_right(r) if pressed_ids(r) else r["expected"] == "none" for r in subset)/len(subset) if subset else 0,
                       "abstentions": sum(not pressed_ids(r) for r in subset),
                       "errors": sum(bool(r["error"]) for r in subset),
                       "p50_ms": statistics.median(times) if times else None,
                       "p90_ms": times[min(len(times)-1, int(len(times)*.9))] if times else None}
        if arm in ADMISSION_ARMS:
            result[arm]["p50_model_ms"] = statistics.median(r["model_ms"] for r in subset)
            result[arm]["p50_local_ms"] = statistics.median(r["local_ms"] for r in subset)
    return result


def check(path, *, fixtures=None):
    data = json.loads(path.read_text())
    fixtures = fixtures or fe.load_sets(Path("bridge/tests/fixtures/ax"))
    keys = {(f.set_name, f.name, c["goal"], repeat)
            for group in fixtures.values() for f in group for c in f.cases for repeat in (1, 2)}
    oracles = {(f.set_name, f.name, c["goal"]): (f,c)
               for group in fixtures.values() for f in group for c in f.cases}
    indexed = {(r["set"],r["fixture"],r["goal"],r["repeat"],r["arm"]):r for r in data["rows"]}
    assert data["repeats"] == 2 and not data["deployment_holdout"]
    arms = ARMS if data.get("schema", 1) == 1 else (*ARMS, *ADMISSION_ARMS)
    assert data.get("schema", 1) in (1, 2)
    assert len(indexed) == len(data["rows"]) == len(keys)*len(arms), "duplicate, extra or missing rows"
    assert {r["arm"] for r in data["rows"]} == set(arms)
    if data.get("schema") == 2:
        assert data["admission_limitation"] == ADMISSION_LIMITATION
    for arm in arms:
        subset = [r for r in data["rows"] if r["arm"] == arm]
        assert len(subset) == len(keys)
        assert {(r["set"], r["fixture"], r["goal"], r["repeat"]) for r in subset} == keys
        for r in subset:
            fixture,case = oracles[(r["set"],r["fixture"],r["goal"])]
            expected = "none" if case.get("expected") == "abstain" else str(case["expected_id"])
            options, by_id, _ = fe.build_menu(fixture.snapshot,r["goal"],max_out=25)
            real = {k:v for k,v in options.items() if k not in RESERVED}
            assert r["expected"] == expected and r["app"] == fixture.app
            assert isinstance(r["ms"], (float, int)) and math.isfinite(r["ms"]) and r["ms"] >= 0
            if arm in ADMISSION_ARMS:
                key = (r["set"], r["fixture"], r["goal"], r["repeat"])
                recorded = indexed[(*key, ADMISSION_ARMS[1])]
                replay = admission(fixture, r["goal"], calls=recorded["calls"])
                oracle = replay[ADMISSION_ARMS.index(arm)]
                for field in ("status", "reason", "admitted_ids", "already", "calls"):
                    assert r[field] == oracle[field], f"admission differs: {field}"
                assert r["picked"] == (r["admitted_ids"][0] if r["admitted_ids"] else "none")
                assert r["model_ms"] == sum(c["answer"]["ms"] for c in r["calls"])
                assert math.isfinite(r["local_ms"]) and r["local_ms"] >= 0
                assert abs(r["ms"] - r["model_ms"] - r["local_ms"]) < 1e-6
                offered = admission_options(r["calls"])
                assert r["options"] == offered and r["offered_ids"] == list(offered)
                assert all(fixture.snapshot.get(cid) is not None for cid in r["admitted_ids"])
                for call in r["calls"]:
                    validate_answer(call["answer"], "Decisions")
                assert not r["answer"]
                assert r["error"] == next((c["answer"]["error"] for c in r["calls"] if c["answer"]["error"]), "")
                continue
            assert "admitted_ids" not in r
            assert r["options"] == real and r["offered_ids"] == list(real)
            assert r["picked"] in ["none", *r["offered_ids"]]
            if r["answer"]:
                validate_answer(r["answer"], "Jev" if arm == "Jev" else "Decisions")
                assert r["error"] == r["answer"]["error"] and r["ms"] == r["answer"]["ms"]
                if data.get("schema") == 2 and not real:
                    assert not r["error"] and r["ms"] == 0
                    if arm == "Jev":
                        assert r["answer"]["requests"] == 0
                    else:
                        assert not r["answer"]["values"]
            if arm == "Keyword":
                assert r["picked"] == (fe.gate_pick(r["goal"],options,by_id) or "none")
            if "Decisions" in arm and "→" not in arm:
                assert r["picked"] == (Gates().pick(decisions.Answer(**r["answer"])) or "none")
            if arm == "Jev":
                pick = typed_ask.JEV_STEP_GATES.decide(typed_ask.StepAnswer(**r["answer"]))
                assert r["picked"] == (pick if pick not in ("confirm", "done", "") else "none")
            if "→" in arm and arm in ARMS:
                key=(r["set"],r["fixture"],r["goal"],r["repeat"])
                gate=indexed[(*key,"Keyword")]
                model=indexed[(*key,"Jev" if arm.endswith("Jev") else "Decisions (AX only)")]
                chosen=gate if gate["picked"]!="none" else model
                assert r["picked"] == chosen["picked"] and r["error"] == chosen["error"]
                assert r["ms"] == gate["ms"]+(model["ms"] if gate["picked"]=="none" else 0)
    assert data["summary"] == metrics(data["rows"])
    print("AX_EVAL_CHECKED", len(keys), "paired requests per arm")


def check_browser(folder):
    rows = be.load_rows(folder)
    tasks = {t.id: t for t in be.TASKS if t.where == "local"}
    # Inspect the Task API explicitly; local cases must be exactly the nine independent oracles.
    expected = {(model, task, rep) for model in ("gpt-6-astra", "gpt-6-sol", "gpt-6-luna")
                for task in tasks for rep in (1, 2)}
    assert len(rows) == 54 and {(r["model"], r["task"], r["repeat"]) for r in rows} == expected
    for r in rows:
        assert r["success"] == bool(tasks[r["task"]].check(r.get("answer") or "", r["state"]))
        assert math.isfinite(r["secs"]) and r["secs"] >= 0
    print("BROWSER_EVAL_CHECKED", len(rows), "independently graded runs")


def run(out):
    load_env_file()
    predict = je.make_predict()
    client = decisions.Client(os.environ["OPENAI_API_KEY"])
    fixtures = fe.load_sets(Path("bridge/tests/fixtures/ax"))
    report = {"schema": 2, "admission_limitation": ADMISSION_LIMITATION, "repeats": 2, "deployment_holdout": False, "slice": "AX-only grounding plus simulated guarded admission; no OS input execution",
              "models": {"decisions": client.model}, "rows": []}
    out.parent.mkdir(parents=True, exist_ok=True)
    try:
        for repeat in (1, 2):
            for group in fixtures.values():
                for f in group:
                    for c in f.cases:
                        goal = c["goal"]
                        options, by_id, _ = fe.build_menu(f.snapshot, goal, max_out=25)
                        real = {k: v for k, v in options.items() if k not in RESERVED}
                        start = time.perf_counter()
                        gate = fe.gate_pick(goal, options, by_id) or "none"
                        gate_ms = (time.perf_counter()-start)*1000
                        j, a = raw_answers(f, goal, real, predict, client)
                        jp = typed_ask.JEV_STEP_GATES.decide(j)
                        jp = jp if jp not in ("", "confirm", "done") else "none"
                        dp = Gates().pick(a) or "none"
                        base = {"set": f.set_name, "fixture": f.name, "app": f.app, "goal": goal, "repeat": repeat,
                                "expected": "none" if c.get("expected") == "abstain" else str(c["expected_id"]),
                                "offered_ids": list(real), "options": real}
                        arms = [(ARMS[0], gate, gate_ms, "", {}), (ARMS[1], jp, j.ms, j.error, asdict(j)),
                                (ARMS[2], dp, a.ms, a.error, asdict(a)),
                                (ARMS[3], gate if gate != "none" else jp,
                                 gate_ms+(j.ms if gate == "none" else 0), j.error if gate == "none" else "", {}),
                                (ARMS[4], gate if gate != "none" else dp,
                                 gate_ms+(a.ms if gate == "none" else 0), a.error if gate == "none" else "", {})]
                        for arm, picked, ms, error, answer in arms:
                            report["rows"].append({**base, "arm": arm, "picked": picked, "ms": ms,
                                                   "error": error, "answer": answer})
                        for arm, admitted in zip(ADMISSION_ARMS, admission(f, goal, client), strict=True):
                            ids = admitted["admitted_ids"]
                            offered = admission_options(admitted["calls"])
                            report["rows"].append({**base, **admitted, "arm": arm, "options": offered,
                                                   "offered_ids": list(offered),
                                                   "picked": ids[0] if ids else "none", "answer": {},
                                                   "error": next((c["answer"]["error"] for c in admitted["calls"] if c["answer"]["error"]), "")})
                    report["summary"] = metrics(report["rows"])
                    out.write_text(json.dumps(report, indent=2)+"\n")
                    print(f"repeat {repeat} {f.set_name}/{f.name}: {len(report['rows'])//(len(ARMS)+len(ADMISSION_ARMS))} paired requests", flush=True)
    finally:
        client.close()


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--out", type=Path)
    p.add_argument("--check", type=Path)
    p.add_argument("--check-browser", type=Path)
    args = p.parse_args()
    if args.check:
        check(args.check)
    elif args.check_browser:
        check_browser(args.check_browser)
    elif args.out:
        run(args.out)
    else:
        p.error("choose --out, --check or --check-browser")


if __name__ == "__main__":
    main()
