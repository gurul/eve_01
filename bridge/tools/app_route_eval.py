#!/usr/bin/env python3
"""Fit and score the app router (app_router.py): whether a request is for one of the owner's devices.

    .venv/bin/python tools/app_route_eval.py --fit        # live Jev once over app_tuning.json (cached), fit the gates
    .venv/bin/python tools/app_route_eval.py --holdout    # round 1: live Jev ONCE over the blind app_holdout.json
    .venv/bin/python tools/app_route_eval.py --holdout2   # round 2: live Jev ONCE over the blind app_holdout2.json
    .venv/bin/python tools/app_route_eval.py --report     # the bar again from the kept answers, no network

The house recipe (tools/toolset_eval.py): fit on the tuning set only, then score ONCE on a holdout written blind by
another writer, against a bar written before any result existed (app_router's docstring and docs/app-router.md hold
it verbatim; ``bar`` here is the one place that computes it). Nothing is refitted after the holdout; a failed bar
is reported, not tuned away.

A case is ``{"id", "door": "call|app|telegram", "previous", "text", "app": "tv|lights|music|camera|mac|none",
"can": true|false|null, "why"}`` inside ``{"about", "cases": [...]}``; an optional ``"method"`` (one of the
device's method keys in app_router.THINGS) is scored when a case carries it. Every device is asked about, with
every tool offered (app_router.ALL_TOOLS), as on a Mac that has them all.

--fit: Jev's answers are cached in app_tuning_answers.json under each case's id, a hash of its two texts, and a
hash of everything else Jev is asked (app_router.fingerprint: the about line, the capability lines, every
question). A reworded question or an edited case is asked again, never read from an old answer (the toolset eval's
cache is keyed by id alone, and a new question there silently reads 0.0). Then app_router.fit picks the gates (its
docstring is the rule) and writes app_gates.json beside the module, which app_router reads at import.

--holdout: reads the fitted app_gates.json (never fits), asks Jev once per holdout case, keeps the answers in
app_holdout_answers.json and the verdict in app_holdout_result.json, and REFUSES to run again once the result file
exists. A Jev answer slower than app_router.TIMEOUT_S claims nothing, as it would in a turn.

--holdout2 (round 2): the same, once, over app_holdout2.json (answers in app_holdout2_answers.json, the verdict in
app_holdout2_result.json), scored with the LIVE path's whole decision: app_router.mac_app_only first, then the gates
as a turn loads them (app_router.load_gates: the fitted gates, never looser than app_router.FLOOR). The installed
apps mac_app_only reads are a fixed list here (MAC_APPS), so the score does not depend on the host; the list, the
floor, the fitted gates and the gates used are all recorded in the result. Round 1 was scored before the floor and
before mac_app_only existed; its files are never rewritten.

--report: recomputes the bar from the kept answers of the LATEST scored round, with the gates (and, for round 2,
the app list) that round was scored with, and prints each bar line with its measured value, then
APP_ROUTER_BAR_OK when lines 1-4 all hold, else APP_ROUTER_BAR_FAILED and exit 1. Once round 2 is scored, round 1
is printed above it as history, marked as seen data. A gates file that changed since a round was scored fails the
report for that round (a refit after the holdout). The floor is not a change of that file: it is a code rule
applied at load, and a round scored after it records it, so a floor that moved since then fails the report too.

The request TEXT (synthetic, from the datasets) leaves the Mac to Jev on EVE_JEV_ROUTE.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import statistics
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Callable, Optional, Sequence

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "src"))

from eve_bridge import app_router as ar  # noqa: E402

ROUTES = Path(__file__).resolve().parents[1] / "tests" / "fixtures" / "routes"
TUNING, HOLDOUT = "app_tuning.json", "app_holdout.json"
TUNING_ANSWERS, HOLDOUT_ANSWERS, RESULT = "app_tuning_answers.json", "app_holdout_answers.json", "app_holdout_result.json"
HOLDOUT2, HOLDOUT2_ANSWERS, RESULT2 = "app_holdout2.json", "app_holdout2_answers.json", "app_holdout2_result.json"
# The installed apps round 2 hands to app_router.mac_app_only: common Mac app names, fixed here so the score is the
# same on every host (a turn reads the Mac's real list). Recorded in the result file.
MAC_APPS: tuple[str, ...] = (
    "Spotify", "Slack", "Safari", "Google Chrome", "Notes", "Mail", "Calendar", "Zoom", "Photo Booth", "Calculator",
    "Finder", "System Settings", "Messages", "Preview", "TV", "Music", "Home")


@dataclass(frozen=True)
class Round:
    """One blind round: its three files, and whether it is scored with the live path's whole decision (mac_app_only
    with MAC_APPS, then the floored gates) or, as round 1 was, with the fitted gates alone."""
    n: int
    holdout: str
    answers: str
    result: str
    live: bool


ROUND1 = Round(1, HOLDOUT, HOLDOUT_ANSWERS, RESULT, live=False)
ROUND2 = Round(2, HOLDOUT2, HOLDOUT2_ANSWERS, RESULT2, live=True)

TRUTHS = (*ar.KEYS, ar.MAC, "none")
# PRE-REGISTERED BAR (the orchestrator, 2026-11-11, before any tuning or holdout result existed).
MIN_DEVICE_RIGHT = 0.90         # (1) device cases: the router names that device
MAX_MAC_CLAIMED = 0.03          # (2) Mac cases: the router claims a device
MAX_NONE_CLAIMED = 0.05         # (3) neither cases: the router claims a device
MAX_P95_MS = 800.0              # (4) Jev latency p95; an error or timeout claims nothing
OK, FAILED = "APP_ROUTER_BAR_OK", "APP_ROUTER_BAR_FAILED"


def cases(path: Path) -> list[dict[str, Any]]:
    """The file's cases, checked: a bad label would be scored as a wrong answer, so it stops the run instead."""
    if not path.exists():
        raise SystemExit(f"NO_CASES: {path.name} is not there yet (the datasets are copied in by the orchestrator)")
    raw = json.loads(path.read_text(encoding="utf-8"))
    data = raw.get("cases") if isinstance(raw, dict) else None
    if not isinstance(data, list) or not data:
        raise SystemExit(f"BAD_CASES: {path.name} is not {{about, cases: [...]}}")
    seen: set[str] = set()
    for c in data:
        ident = str(c.get("id") or "") if isinstance(c, dict) else ""
        if not ident or ident in seen or not isinstance(c.get("text"), str) or not c["text"].strip():
            raise SystemExit(f"BAD_CASES: {path.name}: a case needs a unique id and a text ({ident or '?'})")
        seen.add(ident)
        if c.get("app") not in TRUTHS:
            raise SystemExit(f"BAD_CASES: {path.name}: {ident}: app must be one of {', '.join(TRUTHS)}")
        if c.get("can") not in (True, False, None):
            raise SystemExit(f"BAD_CASES: {path.name}: {ident}: can must be true, false or null")
        thing = ar.thing(str(c["app"]))
        if c.get("method") and (thing is None or c["method"] not in {m.key for m in thing.methods}):
            raise SystemExit(f"BAD_CASES: {path.name}: {ident}: method {c['method']!r} is not one of that device's")
    return data


def case_hash(c: dict[str, Any]) -> str:
    """A case's two texts, hashed: an edited case is never answered from the cache."""
    blob = json.dumps([str(c.get("previous") or ""), str(c["text"])], ensure_ascii=False)
    return hashlib.sha256(blob.encode("utf-8")).hexdigest()[:12]


def live_predict() -> Callable[[Any, dict[str, Any]], dict[str, Any]]:
    import os

    from eve_bridge import jev
    from eve_bridge.envfile import load_env_file

    load_env_file()
    url, key, model = jev.route_config(os.environ)
    print(f"jev: {model} on {url.split('/')[2]}")
    # 8 s, not TIMEOUT_S: the eval measures Jev's real latency; the turn's budget is applied when scoring
    return jev.make_predict(url, key, model, timeout_s=8.0)


def read_cache(cache: Path, questions: str) -> dict[str, dict[str, Any]]:
    """The cache's rows, or {} when it is missing or was asked with other questions (then nothing in it is read)."""
    if not cache.exists():
        return {}
    raw = json.loads(cache.read_text(encoding="utf-8"))
    return dict(raw.get("answers") or {}) if raw.get("questions") == questions else {}


def answers_for(data: list[dict[str, Any]], cache: Path, predict: Optional[Callable[..., Any]],
                *, once: bool = False) -> list[ar.AppAnswer]:
    """Each case's answer: from the cache when its id, its texts' hash and the questions' hash all match, else from
    Jev (and cached). ``once`` (the holdout): a cache that exists must cover every case, or the run is refused."""
    questions = ar.fingerprint(ar.live(ar.ALL_TOOLS))
    kept = read_cache(cache, questions)
    have = {c["id"]: kept[c["id"]] for c in data if c["id"] in kept and kept[c["id"]].get("case") == case_hash(c)}
    if len(have) == len(data):
        print(f"   answers from {cache.name} (questions {questions}; no Jev calls)")
        return [ar.AppAnswer.from_json(have[c["id"]]["answer"]) for c in data]
    if once and cache.exists():
        raise SystemExit(f"{cache.name} exists but does not cover the holdout as it is asked now: refusing to ask "
                         "Jev again")
    predict = predict or live_predict()
    asked = 0
    for c in data:
        if c["id"] not in have:
            a = ar.ask(predict, c["text"], str(c.get("previous") or ""), ar.ALL_TOOLS, time.perf_counter)
            have[c["id"]] = {"case": case_hash(c), "answer": a.to_json()}
            asked += 1
    if once and all(row["answer"].get("error") for row in have.values()):
        # an outage is not a measurement: nothing is kept, so the holdout is still unscored
        raise SystemExit("Jev did not answer any holdout case; nothing was kept and the holdout is not spent")
    cache.write_text(json.dumps({"asked": time.strftime("%Y-%m-%d %H:%M"), "questions": questions,
                                 "answers": {c["id"]: have[c["id"]] for c in data}}, indent=1) + "\n",
                     encoding="utf-8")
    print(f"   asked Jev {asked} times ({len(data) - asked} from the cache); kept in {cache.name}")
    return [ar.AppAnswer.from_json(have[c["id"]]["answer"]) for c in data]


def latency(answers: Sequence[ar.AppAnswer]) -> tuple[float, float]:
    ms = sorted(a.ms for a in answers)
    return (statistics.median(ms), ms[int(0.95 * (len(ms) - 1))]) if ms else (0.0, float("inf"))


def score(data: list[dict[str, Any]], answers: Sequence[ar.AppAnswer], gates: dict[str, ar.Gate],
          mac_apps: Optional[Sequence[str]] = None) -> dict[str, Any]:
    """Every number the bar and the report read, from the answers and the gates alone. With ``mac_apps`` (round 2)
    it is the turn's whole decision: a case app_router.mac_app_only answers is no claim, whatever Jev said (in a
    turn Jev is not asked for it at all)."""
    budget = ar.TIMEOUT_S * 1000.0
    ruled = [mac_apps is not None and ar.mac_app_only(str(c["text"]), mac_apps) for c in data]
    routes = [ar.NO_CLAIM if r else ar.decide(a, gates, budget) for a, r in zip(answers, ruled, strict=True)]
    truth = [str(c["app"]) for c in data]
    device = [i for i, t in enumerate(truth) if t in ar.KEYS]
    mac = [i for i, t in enumerate(truth) if t == ar.MAC]
    none = [i for i, t in enumerate(truth) if t == "none"]
    right = [i for i in device if routes[i].app == truth[i]]
    confusion = {t: {} for t in TRUTHS}
    for i, t in enumerate(truth):
        said = routes[i].app or "no claim"
        confusion[t][said] = confusion[t].get(said, 0) + 1
    can_cases = [i for i in right if data[i].get("can") is not None]
    can_right = [i for i in can_cases if routes[i].can == data[i]["can"]]
    # layer 2's method is read for the claimed device whatever ``can`` says (the route keeps it only when it can)
    how = [i for i in right if data[i].get("method")]
    how_right = [i for i in how if answers[i].how.get(truth[i], ("", 0.0))[0] == data[i]["method"]]
    p50, p95 = latency(answers)
    rate = lambda part, whole: len(part) / len(whole) if whole else None  # noqa: E731
    out = {
        "n": len(data), "device": len(device), "mac": len(mac), "none": len(none),
        "device_right": len(right), "device_rate": rate(right, device),
        "mac_claimed": [data[i]["id"] for i in mac if routes[i].app], "none_claimed": [
            data[i]["id"] for i in none if routes[i].app],
        "p50_ms": round(p50, 1), "p95_ms": round(p95, 1),
        "errors": sum(1 for a in answers if a.error), "slow": sum(1 for a in answers if a.ms > budget),
        "per": {k: {"cases": sum(1 for t in truth if t == k),
                    "named": sum(1 for i in device if truth[i] == k and routes[i].app == k),
                    "claimed": sum(1 for r in routes if r.app == k)} for k in ar.KEYS},
        "can_cases": len(can_cases), "can_right": len(can_right),
        "cant_cases": sum(1 for i in can_cases if data[i]["can"] is False),
        "cant_right": sum(1 for i in can_right if data[i]["can"] is False),
        "method_cases": len(how), "method_right": len(how_right), "confusion": confusion,
        "missed": [data[i]["id"] for i in device if i not in right],
    }
    out["mac_rate"], out["none_rate"] = rate(out["mac_claimed"], mac), rate(out["none_claimed"], none)
    if mac_apps is not None:
        # the cases the code rule answered before the gates, by truth: a device case here is a claim the rule cost
        out["mac_rule"] = {t: [data[i]["id"] for i, r in enumerate(ruled) if r and truth[i] == t] for t in TRUTHS}
    return out


def bar(s: dict[str, Any]) -> dict[str, bool]:
    """The pre-registered bar, lines 1-4. A line with no case to measure it on does not hold: a bar over nothing
    proves nothing."""
    one = s["device_rate"] is not None and s["device_rate"] >= MIN_DEVICE_RIGHT
    two = s["mac_rate"] is not None and s["mac_rate"] <= MAX_MAC_CLAIMED
    three = s["none_rate"] is not None and s["none_rate"] <= MAX_NONE_CLAIMED
    four = s["p95_ms"] <= MAX_P95_MS
    return {"1": one, "2": two, "3": three, "4": four, "shipped": one and two and three and four}


def _pct(x: Optional[float]) -> str:
    return "n/a" if x is None else f"{100 * x:.1f}%"


def print_bar(s: dict[str, Any]) -> dict[str, bool]:
    b = bar(s)
    mark = lambda ok: "pass" if ok else "FAIL"  # noqa: E731
    print(f"   (1) device cases named right {s['device_right']}/{s['device']} = {_pct(s['device_rate'])} "
          f"(need >= {MIN_DEVICE_RIGHT:.0%}): {mark(b['1'])}")
    print(f"   (2) Mac cases claimed for a device {len(s['mac_claimed'])}/{s['mac']} = {_pct(s['mac_rate'])} "
          f"(need <= {MAX_MAC_CLAIMED:.0%}): {mark(b['2'])}")
    print(f"   (3) neither cases claimed for a device {len(s['none_claimed'])}/{s['none']} = {_pct(s['none_rate'])} "
          f"(need <= {MAX_NONE_CLAIMED:.0%}): {mark(b['3'])}")
    print(f"   (4) Jev p95 {s['p95_ms']:.0f} ms, p50 {s['p50_ms']:.0f} ms (need p95 <= {MAX_P95_MS:.0f}); errors "
          f"{s['errors']}, slower than {ar.TIMEOUT_S:g} s {s['slow']} (each claims nothing): {mark(b['4'])}")
    return b


def print_reported(s: dict[str, Any]) -> None:
    """The numbers the bar reports and does not gate: can/can't, the method, and who was taken for whom."""
    print("   reported, not gated:")
    print(f"      can / can't, on the device cases named right: {s['can_right']}/{s['can_cases']}; of the ones that "
          f"can't: {s['cant_right']}/{s['cant_cases']}")
    print(f"      method, on the named cases that carry one: {s['method_right']}/{s['method_cases']}"
          + ("" if s["method_cases"] else " (no case carries a method label)"))
    for k, p in s["per"].items():
        print(f"      {k:<7} named {p['named']}/{p['cases']}; claimed in all {p['claimed']}")
    print("      confusion (truth -> what the router said):")
    for t, row in s["confusion"].items():
        if row:
            print(f"         {t:<7} " + ", ".join(f"{k} {v}" for k, v in sorted(row.items(), key=lambda kv: -kv[1])))
    for name, ids in (("claimed Mac cases", s["mac_claimed"]), ("claimed neither cases", s["none_claimed"]),
                      ("device cases not named", s["missed"])):
        if ids:
            print(f"      {name}: {', '.join(ids)}")
    if "mac_rule" in s:
        fired = {t: ids for t, ids in s["mac_rule"].items() if ids}
        print("      mac_app_only answered before the gates: "
              + (", ".join(f"{t} {len(ids)}" for t, ids in fired.items()) if fired else "no case"))


def gates_json(gates: dict[str, ar.Gate]) -> dict[str, dict[str, float]]:
    return {k: g.to_json() for k, g in gates.items()}


def fit_main(predict: Optional[Callable[..., Any]] = None, data_dir: Path = ROUTES,
             out: Path = ar.GATES_FILE) -> dict[str, ar.Gate]:
    data = cases(data_dir / TUNING)
    print(f"app router / fit on {TUNING}: {len(data)} cases")
    answers = answers_for(data, data_dir / TUNING_ANSWERS, predict)
    truths = [str(c["app"]) for c in data]
    gates, seen = ar.fit(answers, truths, [c.get("can") for c in data])
    for key in ar.KEYS:
        g, info = gates[key], seen[key]
        print(f"   {key:<7} noul >= {g.noul:.2f}, choice >= {g.choice:.2f}, lead over the Mac >= {g.lead:+.1f}, can >= "
              f"{g.can:.2f}; claims {info['claimed']}/{info['own']} of its own, {info['wrong']} of another truth"
              + (f"; strictest {info['strictest']}, loosest alone {info['loosest']}" if "strictest" in info else "")
              + (f"; NOTE {info['note']}" if info.get("note") else ""))
    s = score(data, answers, gates)
    print("   on the tuning set (seen data: not evidence):")
    print_bar(s)
    print_reported(s)
    wrong = len(s["mac_claimed"]) + len(s["none_claimed"])
    if wrong:
        raise SystemExit(f"FIT_BROKEN: the fitted gates claim {wrong} Mac or neither tuning cases; nothing written")
    out.write_text(json.dumps({
        "fitted": time.strftime("%Y-%m-%d"), "on": TUNING, "n": len(data),
        "questions": ar.fingerprint(ar.live(ar.ALL_TOOLS)),
        "rule": "per device: no tuning case of another truth claimed, then the most own cases, then halfway between "
                "the strictest such gates and the loosest each could go alone, rounded strict (app_router.fit)",
        "gates": gates_json(gates)}, indent=1) + "\n", encoding="utf-8")
    print(f"   wrote {out.name}")
    print("APP_ROUTER_FIT_COMPLETE")
    return gates


def _fitted_or_stop(gates_path: Path) -> None:
    fitted = json.loads(gates_path.read_text(encoding="utf-8")) if gates_path.exists() else {}
    if not fitted.get("fitted"):
        raise SystemExit("fit the gates first (--fit): app_gates.json still holds the starting values")
    if fitted.get("questions") != ar.fingerprint(ar.live(ar.ALL_TOOLS)):
        raise SystemExit("the questions changed since the gates were fitted: run --fit again before the holdout")


def _floor_json() -> dict[str, float]:
    return {"noul": ar.FLOOR.noul, "choice": ar.FLOOR.choice, "lead": ar.FLOOR.lead}


def holdout_main(predict: Optional[Callable[..., Any]] = None, data_dir: Path = ROUTES,
                 gates_path: Path = ar.GATES_FILE, rnd: Round = ROUND1) -> bool:
    """Score one round, once. Round 1: the fitted gates alone, as it was scored on 2026-11-11 (before the floor).
    Round 2: the live path's whole decision, with everything it was decided by recorded in the result."""
    result = data_dir / rnd.result
    if result.exists():
        raise SystemExit(f"round {rnd.n}'s holdout was already scored ({result.name} exists); it is scored once. "
                         "Use --report.")
    _fitted_or_stop(gates_path)
    fitted = ar.fitted_gates(gates_path)
    gates = ar.load_gates(gates_path) if rnd.live else fitted
    apps = MAC_APPS if rnd.live else None
    data = cases(data_dir / rnd.holdout)
    print(f"app router / round {rnd.n} holdout {rnd.holdout}: {len(data)} cases, gates from {gates_path.name} "
          "(never refitted here)" + (f", floored at {_floor_json()}; mac_app_only with {len(MAC_APPS)} fixed app "
                                     "names" if rnd.live else ""))
    answers = answers_for(data, data_dir / rnd.answers, predict, once=True)
    s = score(data, answers, gates, apps)
    kept: dict[str, Any] = {
        "round": rnd.n, "scored": time.strftime("%Y-%m-%d %H:%M"),
        "holdout_sha256": hashlib.sha256((data_dir / rnd.holdout).read_bytes()).hexdigest(),
        "questions": ar.fingerprint(ar.live(ar.ALL_TOOLS)), "gates": gates_json(gates)}
    if rnd.live:
        kept.update({
            "decision": "the live path: app_router.mac_app_only with mac_apps, then app_router.decide with gates "
                        "(the fitted gates, each claim gate raised to the floor; can as fitted)",
            "fitted": gates_json(fitted), "floor": _floor_json(), "mac_apps": list(MAC_APPS)})
    else:
        del kept["round"]                       # round 1's file, as it was written on 2026-11-11
    kept.update({"bar": bar(s), "score": s})
    result.write_text(json.dumps(kept, indent=1) + "\n", encoding="utf-8")
    print(f"   wrote {result.name}")
    return report_main(data_dir, gates_path)


def _recomputed(rnd: Round, data_dir: Path, gates_path: Path) -> Optional[tuple[Optional[dict[str, Any]], list[str],
                                                                                dict[str, Any]]]:
    """One scored round again from its kept answers: (the score, or None when the answers do not cover the cases;
    what no longer matches the scoring; the result file). None when the round has not been scored."""
    result, kept = data_dir / rnd.result, data_dir / rnd.answers
    if not result.exists() or not kept.exists():
        return None
    was = json.loads(result.read_text(encoding="utf-8"))
    data = cases(data_dir / rnd.holdout)
    problems = []
    if hashlib.sha256((data_dir / rnd.holdout).read_bytes()).hexdigest() != was.get("holdout_sha256"):
        problems.append(f"{rnd.holdout} is not the file that was scored")
    if rnd.live:
        # the floor is a code rule applied at load and this round recorded it: both it and the file must still match
        if gates_json(ar.fitted_gates(gates_path)) != was.get("fitted"):
            problems.append(f"{gates_path.name} is not the fitted gates round {rnd.n} was scored with (a refit "
                            "after the holdout)")
        if _floor_json() != was.get("floor"):
            problems.append(f"app_router.FLOOR is not the floor round {rnd.n} was scored with (a rule changed "
                            "after the holdout)")
        if gates_json(ar.load_gates(gates_path)) != was.get("gates"):
            problems.append(f"the gates a turn loads are not the gates round {rnd.n} was scored with")
    elif gates_json(ar.fitted_gates(gates_path)) != was.get("gates"):
        # round 1 was scored with the file's gates as fitted, before the floor: the floor is not a change of the file
        problems.append(f"{gates_path.name} is not the gates the holdout was scored with (a refit after the holdout)")
    rows = json.loads(kept.read_text(encoding="utf-8")).get("answers") or {}
    if any(c["id"] not in rows or rows[c["id"]].get("case") != case_hash(c) for c in data):
        problems.append(f"{rnd.answers} does not cover the holdout's cases")
        return None, problems, was
    gates = {k: ar.Gate(**v) for k, v in was["gates"].items()}
    answers = [ar.AppAnswer.from_json(rows[c["id"]]["answer"]) for c in data]
    return score(data, answers, gates, was.get("mac_apps") if rnd.live else None), problems, was


def _print_round(rnd: Round, s: dict[str, Any], was: dict[str, Any], head: str) -> dict[str, bool]:
    print(f"app router / {head} on {rnd.holdout}: {s['n']} cases ({s['device']} device, {s['mac']} Mac, {s['none']} "
          f"neither), scored {was.get('scored')}, no Jev calls")
    if rnd.live:
        print(f"   the live path: mac_app_only with {len(was.get('mac_apps') or ())} fixed app names, then the "
              f"fitted gates floored at {was.get('floor')}")
    b = print_bar(s)
    print_reported(s)
    return b


def report_main(data_dir: Path = ROUTES, gates_path: Path = ar.GATES_FILE) -> bool:
    """The bar from the kept answers of the latest scored round: no network. True when lines 1-4 all hold for it.
    With round 2 scored, round 1 is printed first as history (seen data): it does not decide."""
    one, two = _recomputed(ROUND1, data_dir, gates_path), _recomputed(ROUND2, data_dir, gates_path)
    if one is None and two is None:
        print(f"app router / report: the holdout has not been scored ({RESULT} or {HOLDOUT_ANSWERS} is missing)")
        print(FAILED)
        return False
    if two is not None and one is not None:
        s1, problems1, was1 = one
        if s1 is None:
            print(f"app router / history, round 1: {'; '.join(problems1)}")
        else:
            _print_round(ROUND1, s1, was1, "history, round 1 (SEEN DATA: read after it was scored, and the floor, "
                                           "the guard and the Mac rule were changed on it; it decides nothing)")
            for p in problems1:
                print(f"   NOTE: {p}")
        print()
    rnd, (s, problems, was) = (ROUND2, two) if two is not None else (ROUND1, one)  # type: ignore[misc]
    if s is None:
        print(f"app router / report, round {rnd.n}: {'; '.join(problems)}")
        print(FAILED)
        return False
    b = _print_round(rnd, s, was, f"report, round {rnd.n}")
    for p in problems:
        print(f"   PROBLEM: {p}")
    if not rnd.live and gates_json(ar.load_gates(gates_path)) != was.get("gates"):
        print(f"   NOTE: this is round 1, scored with the gates as fitted. A turn now loads them floored at "
              f"{_floor_json()} (a code rule, not a refit of {gates_path.name}), so these numbers are not the live "
              f"rule's. Round 2 (--holdout2) scores the live rule.")
    ok = b["shipped"] and not problems
    print(f"   app_router.SHIPPED is {ar.SHIPPED}; round {rnd.n}'s bar (lines 1-4) says {b['shipped']}")
    print(OK if ok else FAILED)
    return ok


def main(argv: Optional[list[str]] = None) -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    g = p.add_mutually_exclusive_group(required=True)
    g.add_argument("--fit", action="store_true", help="fit the gates on app_tuning.json")
    g.add_argument("--holdout", action="store_true", help="round 1: score once on the blind app_holdout.json")
    g.add_argument("--holdout2", action="store_true",
                   help="round 2: score once on the blind app_holdout2.json, with the live path's whole decision")
    g.add_argument("--report", action="store_true",
                   help="the bar for the latest scored round from its kept answers, no network")
    args = p.parse_args(argv)
    if args.fit:
        fit_main()
        return 0
    if args.holdout or args.holdout2:
        return 0 if holdout_main(rnd=ROUND2 if args.holdout2 else ROUND1) else 1
    return 0 if report_main() else 1


if __name__ == "__main__":
    raise SystemExit(main())
