#!/usr/bin/env python
"""The fast lane's offline eval and live gates (GATES.md G4, G5, G6, G11, G14).

    .venv/bin/python tools/fastlane_eval.py --fixtures tests/fixtures/ax [--min-select 30 ...] [--fake-predictor]
    .venv/bin/python tools/fastlane_eval.py --check-default
    .venv/bin/python tools/fastlane_eval.py --live-snapshot Calendar --min-pressable 5 --max-ms 300 --require-complete
    .venv/bin/python tools/fastlane_eval.py --live-delegate Calendar "switch to week view" --done-when Week --max-steps 3
    .venv/bin/python tools/fastlane_eval.py --shadow-report --min-runs 5
    .venv/bin/python tools/fastlane_eval.py --make-fixture raw.json --out tests/fixtures/ax/select/calendar-month.json --set select
    .venv/bin/python tools/fastlane_eval.py --redact tests/fixtures/ax/select/calendar-month.json --map 'Dentist 3pm=Meeting A'

What the offline eval measures
------------------------------
Every fixture is one redacted accessibility snapshot plus hand-authored cases
(goal → expected candidate id, or "abstain"). For each case the harness runs the
lane's decision path exactly: rank_candidates(kinds="pressable", max_out=10), each
option worded by decider.render_option ("Week (radio button)"), the reserved
"reobserve" and "abstain", the context fast_lane.lane_context gives the model, and
first the lane's keyword gate (fast_lane.keyword_pick: one offered control uniquely
sharing the most objective tokens decides without the model) — only the cases the
gate leaves open reach the decider, once per prompt style. Beside that it reports a
code-only keyword baseline (highest objective-token overlap on label + value, ties
by menu order, abstain on zero overlap). The baseline is not independent of the
system any more — the gate is its unique-maximum half — so the printout names the
gate's usage rate and the model's own accuracy on the cases the gate did not decide.

Style and thresholds are chosen on select/ only: the thresholds are the highest
(p_min, margin_min) on the grid with coverage ≥ 0.70, the style is the best
cost-weighted score at its own thresholds (ties by top-1). holdout/ is read once,
at those settings, for the ship decision (--check-default): enabled iff gated
top-1 ≥ 0.80 AND coverage ≥ 0.70 AND real ≥ keyword + 0.10 on the overlap:false (click-expected)
subset AND cost-weighted score > 0.

Cost model (PLAN §0, measured on the 2026-09 run logs): a delegate step that
replaces one astra middle turn saves 3.56 s api + 1.33 s exec − ~1.4 s lane
≈ 3.5 s; a wrong click costs the step plus a 3.15 s recovery turn ≈ 4.55 s; an
escalation (uncovered pick, or abstain/reobserve when a click was expected, or a
correct abstain — astra still has to act) costs the lane's own step ≈ 1.4 s.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import platform
import statistics
import sys
import time
from dataclasses import asdict, dataclass, field
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Callable, Optional

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "src"))

from eve_bridge import ax_candidates as ax  # noqa: E402
from eve_bridge.ax_candidates import (  # noqa: E402
    Candidate,
    Snapshot,
    is_sensitive,
    pressable,
    rank_candidates,
    snapshot_from_dict,
    snapshot_from_raw,
)
from eve_bridge.decider import RESERVED, STYLES, Choice, Decider, render_option  # noqa: E402
from eve_bridge.fast_lane import MenuItem, lane_context  # noqa: E402
from eve_bridge.fast_lane import keyword_pick as lane_keyword_pick  # noqa: E402

# ---- constants -------------------------------------------------------------------------

SETS = ("select", "holdout")
MENU_MAX = 10                                   # real options; the two reserved make 12 (Thresholds.max_options)
RESERVED_TEXT = {"reobserve": "the screen is still changing, look again",
                 "abstain": "none of these advances the objective"}
P_GRID = (0.2, 0.3, 0.4, 0.5, 0.6)
MARGIN_GRID = (0.05, 0.10, 0.15)
K_BUCKETS = (("2-5", 2, 5), ("6-10", 6, 10), ("11+", 11, 10**9))
MIN_COVERAGE = 0.70
SHIP_TOP1 = 0.80
SHIP_COVERAGE = 0.70
SHIP_KEYWORD_LEAD = 0.10
# The router (lane_router.py) acts on the human's own words with no planner behind it, so its bar
# is precision, not coverage: a request it declines costs one AX snapshot and the planner runs as
# before. Fixed here before any router number was read (2026-09-21): at +3.5 s a right click and
# −4.55 s a wrong one the break-even precision is 56.5%; the bar is 0.90, on at least 10 engaged
# holdout cases so the figure means something, with zero sensitive clicks.
ROUTER_MIN_PRECISION = 0.90
ROUTER_MIN_ENGAGED = 10
# A fixed list, so the "names another app" rule scores the same on every machine.
ROUTER_APPS = frozenset({"calendar", "system settings", "safari", "finder", "notes", "music", "reminders", "mail",
                         "photos", "messages", "maps", "contacts", "preview", "terminal", "textedit", "podcasts",
                         "books", "news", "stocks", "weather", "clock", "calculator", "shortcuts", "facetime"})
# §0: saved seconds per correct delegated step, cost per wrong click (step + recovery turn), cost of an
# escalation (the lane's own step, astra acts anyway). Replace with measured ones once live timings exist.
COST_CORRECT = 3.5
COST_WRONG = 4.55
COST_ESCALATE = 1.4
Z95 = 1.959964
DEFAULT_RUNS_DIR = "~/.config/eve-bridge/agent-runs"
SRC_DIR = Path(__file__).resolve().parents[1] / "src" / "eve_bridge"


# ---- fixtures ------------------------------------------------------------------------------

@dataclass
class Fixture:
    path: Path
    set_name: str
    name: str
    snapshot: Snapshot
    cases: list[dict[str, Any]]
    captured: dict[str, Any]
    screen: Optional[tuple[int, int]]
    raw: dict[str, Any]

    @property
    def app(self) -> str:
        return self.snapshot.app


def ax_candidates_sha() -> str:
    return hashlib.sha256((SRC_DIR / "ax_candidates.py").read_bytes()).hexdigest()


def load_fixture(path: Path, set_name: str = "") -> Fixture:
    data = json.loads(path.read_text(encoding="utf-8"))
    for key in ("captured", "raw", "snapshot", "cases"):
        if key not in data:
            raise ValueError(f"{path}: fixture has no {key!r}")
    screen = tuple(int(v) for v in data["screen"]) if data.get("screen") else None
    return Fixture(path=path, set_name=set_name or path.parent.name, name=path.stem,
                   snapshot=snapshot_from_dict(data["snapshot"]), cases=list(data["cases"]),
                   captured=dict(data["captured"]), screen=screen, raw=data["raw"])  # type: ignore[arg-type]


def load_sets(root: Path) -> dict[str, list[Fixture]]:
    out: dict[str, list[Fixture]] = {}
    for set_name in SETS:
        folder = root / set_name
        files = sorted(folder.glob("*.json")) if folder.is_dir() else []
        out[set_name] = [load_fixture(p, set_name) for p in files]
    return out


def count_errors(sets: dict[str, list[Fixture]], *, min_select: int, min_holdout: int, min_apps: int,
                 min_no_overlap: int, min_distractor: int) -> list[str]:
    """Why the fixture sets are too small, one line each; empty when they meet the minimums."""
    errors: list[str] = []
    select_cases = sum(len(f.cases) for f in sets["select"])
    hold = sets["holdout"]
    hold_cases = [c for f in hold for c in f.cases]
    apps = {f.app for f in hold}
    no_overlap = sum(1 for c in hold_cases if not c.get("overlap", True))
    distractor = sum(1 for c in hold_cases if c.get("distractor"))
    if select_cases < min_select:
        errors.append(f"select has {select_cases} cases, needs {min_select}")
    if len(hold_cases) < min_holdout:
        errors.append(f"holdout has {len(hold_cases)} cases, needs {min_holdout}")
    if len(apps) < min_apps:
        errors.append(f"holdout covers {len(apps)} apps ({', '.join(sorted(apps))}), needs {min_apps}")
    if no_overlap < min_no_overlap:
        errors.append(f"holdout has {no_overlap} overlap:false cases, needs {min_no_overlap}")
    if distractor < min_distractor:
        errors.append(f"holdout has {distractor} distractor cases, needs {min_distractor}")
    shared = {f.name for f in sets["select"]} & {f.name for f in hold}
    if shared:
        errors.append(f"fixtures in both sets: {', '.join(sorted(shared))}")
    return errors


def app_version(app: str) -> str:
    """The app's short version string via NSWorkspace, "" when unknown (tests never call this)."""
    try:
        from AppKit import NSBundle, NSWorkspace

        url = NSWorkspace.sharedWorkspace().URLForApplicationWithBundleIdentifier_(app)
        if url is None:
            return ""
        bundle = NSBundle.bundleWithURL_(url)
        info = bundle.infoDictionary() if bundle is not None else None
        return str((info or {}).get("CFBundleShortVersionString") or "")
    except Exception:  # noqa: BLE001 — provenance only
        return ""


def make_fixture(dump: dict[str, Any], *, set_name: str, redacted: bool = False,
                 now: Optional[datetime] = None, version: str = "") -> dict[str, Any]:
    """A fixture file's content from the CLI's --json dump: sha, re-derived snapshot, empty cases."""
    if set_name not in SETS:
        raise ValueError(f"set must be one of {SETS}")
    raw = dump["raw"]
    screen = tuple(int(v) for v in dump["screen"]) if dump.get("screen") else None
    snap = snapshot_from_raw(raw, seq=1, screen=screen)
    if snap.truncated:
        raise ValueError("the walk was truncated: a fixture must be a complete snapshot")
    at = (now or datetime.now(timezone.utc)).isoformat(timespec="seconds")
    return {"captured": {"ax_candidates_sha": ax_candidates_sha(), "macos": platform.mac_ver()[0],
                         "app_version": version, "at": at, "truncated": False, "redacted": bool(redacted),
                         "set": set_name},
            "screen": list(screen) if screen else None, "raw": raw, "snapshot": snap.to_dict(), "cases": []}


def _redact_node(node: dict[str, Any], mapping: dict[str, str], hits: list[str]) -> None:
    for key in ("title", "description", "value"):
        v = node.get(key)
        if isinstance(v, str) and v in mapping:
            hits.append(v)
            node[key] = mapping[v]
    for child in node.get("children") or ():
        _redact_node(child, mapping, hits)


def redact_fixture(data: dict[str, Any], mapping: dict[str, str]) -> tuple[dict[str, Any], list[str]]:
    """Replace whole labels in the raw dump, re-derive the snapshot from it, mark redacted.

    Exact whole-string matches only: "Dentist 3pm" → "Meeting A" leaves "Dentist" alone, so a
    synthetic label of the same shape replaces the personal one everywhere it occurs, and the
    snapshot cannot disagree with the raw because it is derived again.
    """
    out = json.loads(json.dumps(data))
    hits: list[str] = []
    raw = out["raw"]
    if isinstance(raw.get("title"), str) and raw["title"] in mapping:
        hits.append(raw["title"])
        raw["title"] = mapping[raw["title"]]
    if raw.get("root"):
        _redact_node(raw["root"], mapping, hits)
    screen = tuple(int(v) for v in out["screen"]) if out.get("screen") else None
    old = snapshot_from_dict(out["snapshot"])
    out["snapshot"] = snapshot_from_raw(raw, seq=old.seq, screen=screen).to_dict()
    out["captured"]["redacted"] = True
    for case in out.get("cases", ()):
        note = case.get("note")
        if isinstance(note, str) and note in mapping:
            case["note"] = mapping[note]
    return out, hits


# ---- the menu and the baseline ----------------------------------------------------------------

def build_menu(snapshot: Snapshot, goal: str, *, allow_page_links: bool = False,
               max_out: int = MENU_MAX) -> tuple[dict[str, str], dict[str, Candidate], int]:
    """(options, candidates by id, dropped_by_cap) — the lane's own menu for a click step."""
    kept, dropped = rank_candidates(snapshot, goal, max_out=max_out, kinds="pressable",
                                    allow_page_links=allow_page_links)
    options = {c.id: render_option(c) for c in kept}
    options.update(RESERVED_TEXT)
    return options, {c.id: c for c in kept}, dropped


def keyword_baseline(goal: str, options: dict[str, str], by_id: Optional[dict[str, Candidate]] = None) -> str:
    """The code-only baseline: the option sharing the most objective tokens (label + value
    when the candidates are given, else the option text), ties by menu order, "abstain"
    when no option shares any. The lane's keyword gate is this picker's unique-maximum half."""
    tokens = set(ax.objective_tokens(goal))
    best, best_score = "abstain", 0
    for key, text in options.items():
        if key in RESERVED:
            continue
        if by_id is not None and key in by_id:
            c = by_id[key]
            score = len(tokens & set(ax.objective_tokens(f"{c.label} {c.value}")))
        else:
            hay = text.casefold()
            score = sum(1 for t in tokens if t in hay)
        if score > best_score:
            best, best_score = key, score
    return best


def gate_pick(goal: str, options: dict[str, str], by_id: dict[str, Candidate]) -> str:
    """The lane's keyword gate on this menu: the candidate id it decides, or "" (the model's turn)."""
    items = [MenuItem(key, "click", text, by_id[key]) for key, text in options.items() if key in by_id]
    hit = lane_keyword_pick(items, goal)
    return hit.key if hit is not None else ""


def make_fake_predict(strength: float = 3.0) -> Callable[[Any, dict[str, Any]], dict[str, Any]]:
    """A System-One-shaped predictor for tests and --fake-predictor: probabilities from keyword
    overlap (1 + strength × shared tokens), deterministic, no model."""

    def predict(state: Any, questions: dict[str, Any]) -> dict[str, Any]:
        answers: dict[str, Any] = {}
        goal = state.get("goal", "") if isinstance(state, dict) else str(state)
        tokens = ax.objective_tokens(goal)
        used = len(json.dumps(state, ensure_ascii=False).split())
        for qid, q in questions.items():
            if q.get("type") == "noul":
                answers[qid] = {"type": "noul", "noul": 0.5, "confidence": 0.5, "action": {"act_probability": 0.5}}
                continue
            crit = q["criteria"]
            weights = {k: 1.0 + (0.0 if k in RESERVED else strength * sum(1 for t in tokens if t in v.casefold()))
                       for k, v in crit.items()}
            total = sum(weights.values())
            probs = {k: round(w / total, 4) for k, w in weights.items()}
            choice = max(probs, key=lambda k: (probs[k], -list(probs).index(k)))
            answers[qid] = {"type": "choice", "choice": choice, "probabilities": probs, "confidence": 0.3,
                            "action": {"act_probability": 0.5}}
            used += sum(len(v.split()) + 1 for v in crit.values())
        return {"answers": answers, "usage": {"input_tokens": used, "output_tokens": 0}}

    return predict


# ---- running the cases ----------------------------------------------------------------------

@dataclass
class CaseResult:
    fixture: str
    set_name: str
    app: str
    goal: str
    expected: str                # a candidate id, or "abstain"
    overlap: bool
    distractor: bool
    k: int
    pick: str                    # option key, or "" when the decider rejected the answer
    correct: bool
    wrong_click: bool            # a real option that is not the expected one
    escalated: bool              # reserved pick when a click was expected, or no pick
    p_top: float
    margin: float
    confidence: float
    ms: float
    tokens: int
    overflow: bool
    dropped_budget: int
    dropped_cap: int
    kw_pick: str
    kw_correct: bool
    via: str = "model"           # keyword | model | none: which oracle decided this case
    offered_forbidden: list[str] = field(default_factory=list)
    sensitive_offered: int = 0
    sensitive_pick: bool = False
    error: str = ""


def _is_correct(pick: str, expected: str) -> bool:
    if expected == "abstain":
        return pick in RESERVED
    return pick == expected


def run_case(decider: Decider, fixture: Fixture, case: dict[str, Any]) -> CaseResult:
    goal = str(case["goal"])
    expected = "abstain" if case.get("expected") == "abstain" else str(case.get("expected_id"))
    options, by_id, dropped_cap = build_menu(fixture.snapshot, goal)
    forbidden = [str(i) for i in case.get("must_not_offer") or () if str(i) in options]
    sensitive_offered = sum(1 for c in by_id.values() if is_sensitive(c.label))
    gate = gate_pick(goal, options, by_id) if by_id else ""
    via = "keyword" if gate else ("model" if by_id else "none")
    if gate:
        # the lane's keyword gate decided: the model is not asked, the pick is always covered
        choice = Choice(gate, p_top=1.0, margin=1.0, confidence=1.0,
                        probabilities={k: (1.0 if k == gate else 0.0) for k in options}, k=len(options))
        pick = gate
        correct = _is_correct(pick, expected)
        error = ""
    elif by_id:
        choice = decider.choose(goal, app=fixture.snapshot.app, context=lane_context(fixture.snapshot),
                                options=options)
        pick = choice.id
        correct = bool(pick) and _is_correct(pick, expected)
        error = choice.error
    else:
        # Nothing to offer (every control dropped: dialog, sensitive, unlabelled): the lane escalates
        # no_candidate without asking the model, which is right exactly when the case expects abstain.
        choice = Choice("", k=len(options), error="no candidates offered")
        pick = ""
        correct = expected == "abstain"
        error = "" if correct else choice.error
    kw = keyword_baseline(goal, options, by_id)
    wrong_click = bool(pick) and pick not in RESERVED and not correct
    escalated = not pick or (pick in RESERVED and expected != "abstain")
    return CaseResult(fixture=fixture.name, set_name=fixture.set_name, app=fixture.app, goal=goal,
                      expected=expected, overlap=bool(case.get("overlap", True)),
                      distractor=bool(case.get("distractor")), k=choice.k, pick=pick, correct=correct,
                      wrong_click=wrong_click, escalated=escalated, p_top=choice.p_top, margin=choice.margin,
                      confidence=choice.confidence, ms=choice.ms, tokens=choice.input_tokens,
                      overflow=choice.overflow, dropped_budget=choice.dropped_for_budget, dropped_cap=dropped_cap,
                      kw_pick=kw, kw_correct=_is_correct(kw, expected), via=via, offered_forbidden=forbidden,
                      sensitive_offered=sensitive_offered,
                      sensitive_pick=bool(pick in by_id and is_sensitive(by_id[pick].label)), error=error)


def run_set(decider: Decider, fixtures: list[Fixture], style: str) -> list[CaseResult]:
    decider.style = style
    return [run_case(decider, f, c) for f in fixtures for c in f.cases]


# ---- metrics ---------------------------------------------------------------------------------

def wilson(successes: int, n: int, z: float = Z95) -> tuple[float, float]:
    """The Wilson 95% interval of a proportion; (0, 1) when n is 0."""
    if n <= 0:
        return (0.0, 1.0)
    p = successes / n
    denom = 1 + z * z / n
    centre = (p + z * z / (2 * n)) / denom
    half = z * math.sqrt(p * (1 - p) / n + z * z / (4 * n * n)) / denom
    return (max(0.0, centre - half), min(1.0, centre + half))


def percentiles(values: list[float], points: tuple[float, ...] = (0.1, 0.5, 0.9)) -> list[float]:
    if not values:
        return [0.0 for _ in points]
    xs = sorted(values)
    out = []
    for p in points:
        idx = min(len(xs) - 1, max(0, int(round(p * (len(xs) - 1)))))
        out.append(round(xs[idx], 4))
    return out


def covered(r: CaseResult, p_min: float, margin_min: float) -> bool:
    return bool(r.pick) and r.p_top >= p_min and r.margin >= margin_min


def gated(results: list[CaseResult], p_min: float, margin_min: float) -> dict[str, float]:
    """gated top-1 (accuracy among covered cases) and coverage at one threshold pair."""
    cov = [r for r in results if covered(r, p_min, margin_min)]
    n = len(results)
    acc = sum(1 for r in cov if r.correct) / len(cov) if cov else 0.0
    return {"gated_top1": acc, "coverage": len(cov) / n if n else 0.0, "n_covered": len(cov)}


def coverage_table(results: list[CaseResult]) -> list[dict[str, float]]:
    rows = []
    for p_min in P_GRID:
        for m in MARGIN_GRID:
            rows.append({"p_min": p_min, "margin_min": m, **gated(results, p_min, m)})
    return rows


def pick_thresholds(results: list[CaseResult], min_coverage: float = MIN_COVERAGE) -> tuple[float, float, bool]:
    """The highest (p_min, margin_min) on the grid whose coverage ≥ min_coverage; the lowest
    grid point, flagged False, when none reaches it."""
    for p_min in sorted(P_GRID, reverse=True):
        for m in sorted(MARGIN_GRID, reverse=True):
            if gated(results, p_min, m)["coverage"] >= min_coverage:
                return (p_min, m, True)
    return (min(P_GRID), min(MARGIN_GRID), False)


def cost_weighted(results: list[CaseResult], p_min: float, margin_min: float) -> float:
    """Seconds saved per case at these thresholds under the §0 cost model (see the docstring)."""
    if not results:
        return 0.0
    total = 0.0
    for r in results:
        if covered(r, p_min, margin_min) and r.correct and r.expected != "abstain":
            total += COST_CORRECT
        elif covered(r, p_min, margin_min) and r.wrong_click:
            total -= COST_WRONG
        else:
            total -= COST_ESCALATE
    return total / len(results)


def subset_accuracy(results: list[CaseResult], pred: Callable[[CaseResult], bool],
                    key: str = "correct") -> tuple[float, int]:
    sub = [r for r in results if pred(r)]
    if not sub:
        return (0.0, 0)
    return (sum(1 for r in sub if getattr(r, key)) / len(sub), len(sub))


def k_buckets(results: list[CaseResult]) -> list[dict[str, Any]]:
    rows = []
    for name, lo, hi in K_BUCKETS:
        sub = [r for r in results if lo <= r.k <= hi]
        rows.append({"bucket": name, "n": len(sub),
                     "top1": sum(1 for r in sub if r.correct) / len(sub) if sub else 0.0,
                     "keyword": sum(1 for r in sub if r.kw_correct) / len(sub) if sub else 0.0})
    return rows


def summarize(results: list[CaseResult], p_min: float, margin_min: float) -> dict[str, Any]:
    n = len(results)
    correct = sum(1 for r in results if r.correct)
    kw = sum(1 for r in results if r.kw_correct)
    # overlap:false AND a click expected: the keyword picker has nothing to go on there (it abstains
    # by construction), so this is the model's real added value; abstain-expected cases are reported apart.
    no_overlap_real, n_no = subset_accuracy(results, lambda r: not r.overlap and r.expected != "abstain")
    no_overlap_kw, _ = subset_accuracy(results, lambda r: not r.overlap and r.expected != "abstain", "kw_correct")
    abstain_real, n_ab = subset_accuracy(results, lambda r: r.expected == "abstain")
    abstain_kw, _ = subset_accuracy(results, lambda r: r.expected == "abstain", "kw_correct")
    distractor_real, n_dis = subset_accuracy(results, lambda r: r.distractor)
    distractor_kw, _ = subset_accuracy(results, lambda r: r.distractor, "kw_correct")
    g = gated(results, p_min, margin_min)
    by_gate = [r for r in results if r.via == "keyword"]
    by_model = [r for r in results if r.via == "model"]
    return {
        "n": n, "top1": correct / n if n else 0.0, "wilson": wilson(correct, n), "keyword_top1": kw / n if n else 0.0,
        "gate_used": len(by_gate) / n if n else 0.0, "gate_top1": (sum(1 for r in by_gate if r.correct) / len(by_gate)
                                                                    if by_gate else 0.0),
        "model_n": len(by_model), "model_top1": (sum(1 for r in by_model if r.correct) / len(by_model)
                                                 if by_model else 0.0),
        "no_overlap": {"n": n_no, "real": no_overlap_real, "keyword": no_overlap_kw},
        "abstain": {"n": n_ab, "real": abstain_real, "keyword": abstain_kw},
        "distractor": {"n": n_dis, "real": distractor_real, "keyword": distractor_kw},
        "p_top_correct": percentiles([r.p_top for r in results if r.correct and r.pick]),
        "p_top_incorrect": percentiles([r.p_top for r in results if not r.correct and r.pick]),
        "margin_correct": percentiles([r.margin for r in results if r.correct and r.pick]),
        "margin_incorrect": percentiles([r.margin for r in results if not r.correct and r.pick]),
        "sensitive_offered": sum(r.sensitive_offered for r in results),
        "sensitive_pick_rate": sum(1 for r in results if r.sensitive_pick) / n if n else 0.0,
        "forbidden_offered": sum(len(r.offered_forbidden) for r in results),
        "options_truncated": sum(1 for r in results if r.dropped_budget > 0),
        "state_truncated": sum(1 for r in results if r.overflow),
        "rejected": sum(1 for r in results if not r.pick),
        "ms_p50": percentiles([r.ms for r in results], (0.5,))[0], "ms_p95": percentiles([r.ms for r in results], (0.95,))[0],
        "tokens_mean": statistics.fmean([r.tokens for r in results]) if results else 0.0,
        "thresholds": (p_min, margin_min), "gated_top1": g["gated_top1"], "coverage": g["coverage"],
        "n_covered": g["n_covered"], "cost_weighted": cost_weighted(results, p_min, margin_min),
        "k_buckets": k_buckets(results), "coverage_table": coverage_table(results),
    }


def choose_style(select_by_style: dict[str, list[CaseResult]]) -> tuple[str, tuple[float, float], bool]:
    """The winner on select/: best cost-weighted score at its own thresholds, ties by top-1."""
    best: Optional[tuple[float, float, str, tuple[float, float], bool]] = None
    for style, results in select_by_style.items():
        p_min, m, reached = pick_thresholds(results)
        score = cost_weighted(results, p_min, m)
        top1 = sum(1 for r in results if r.correct) / len(results) if results else 0.0
        key = (score, top1, style, (p_min, m), reached)
        if best is None or (key[0], key[1]) > (best[0], best[1]):
            best = key
    assert best is not None
    return best[2], best[3], best[4]


def ship_decision(holdout: list[CaseResult], p_min: float, margin_min: float) -> tuple[bool, dict[str, Any]]:
    """The four conditions of PLAN §3.5 on holdout at the shipped thresholds."""
    g = gated(holdout, p_min, margin_min)
    real, _ = subset_accuracy(holdout, lambda r: not r.overlap and r.expected != "abstain")
    kw, _ = subset_accuracy(holdout, lambda r: not r.overlap and r.expected != "abstain", "kw_correct")
    score = cost_weighted(holdout, p_min, margin_min)
    numbers = {"gated_top1": g["gated_top1"], "coverage": g["coverage"], "no_overlap_real": real,
               "no_overlap_keyword": kw, "cost_weighted": score, "n": len(holdout)}
    ok = (len(holdout) > 0 and g["gated_top1"] >= SHIP_TOP1 and g["coverage"] >= SHIP_COVERAGE
          and real >= kw + SHIP_KEYWORD_LEAD and score > 0)
    return ok, numbers


# ---- printing ---------------------------------------------------------------------------------

def _pct(x: float) -> str:
    return f"{100 * x:5.1f}%"


def print_summary(title: str, s: dict[str, Any], out: Callable[[str], None] = print) -> None:
    lo, hi = s["wilson"]
    out(f"== {title}: n={s['n']}")
    out(f"   top-1 {_pct(s['top1'])} (Wilson 95% {_pct(lo)}–{_pct(hi)})   keyword baseline {_pct(s['keyword_top1'])}"
        f"   rejected {s['rejected']}")
    out(f"   keyword gate decided {_pct(s['gate_used'])} of cases at {_pct(s['gate_top1'])} top-1; the model alone on "
        f"the other n={s['model_n']}: {_pct(s['model_top1'])} (the baseline shares the gate's picker: not independent)")
    no, di = s["no_overlap"], s["distractor"]
    ab = s["abstain"]
    out(f"   abstain-expected n={ab['n']}: real {_pct(ab['real'])} keyword {_pct(ab['keyword'])}")
    out(f"   overlap:false (click expected) n={no['n']}: real {_pct(no['real'])} keyword {_pct(no['keyword'])}"
        f"   distractor n={di['n']}: real {_pct(di['real'])} keyword {_pct(di['keyword'])}")
    out(f"   p_top p10/p50/p90 correct {s['p_top_correct']} incorrect {s['p_top_incorrect']}")
    out(f"   margin p10/p50/p90 correct {s['margin_correct']} incorrect {s['margin_incorrect']}")
    out("   accuracy-vs-coverage (gated top-1 / coverage):")
    out("     p_min  " + "  ".join(f"m≥{m:.2f}        " for m in MARGIN_GRID))
    for p_min in P_GRID:
        cells = []
        for m in MARGIN_GRID:
            row = next(r for r in s["coverage_table"] if r["p_min"] == p_min and r["margin_min"] == m)
            cells.append(f"{_pct(row['gated_top1'])}/{_pct(row['coverage'])}")
        out(f"     {p_min:.1f}    " + "  ".join(cells))
    out("   by k: " + "  ".join(f"{b['bucket']}: n={b['n']} top-1 {_pct(b['top1'])} kw {_pct(b['keyword'])}"
                                for b in s["k_buckets"]))
    out(f"   sensitive labels offered {s['sensitive_offered']}, sensitive-pick rate {_pct(s['sensitive_pick_rate'])},"
        f" must_not_offer violations {s['forbidden_offered']}")
    out(f"   options_truncated {s['options_truncated']}  state_truncated {s['state_truncated']}"
        f"  ms p50 {s['ms_p50']:.1f} p95 {s['ms_p95']:.1f}  tokens mean {s['tokens_mean']:.0f}")
    p_min, m = s["thresholds"]
    out(f"   at thresholds p_min={p_min:.2f} margin_min={m:.2f}: gated top-1 {_pct(s['gated_top1'])}"
        f" coverage {_pct(s['coverage'])} (n={s['n_covered']})  cost-weighted {s['cost_weighted']:+.2f} s/case")


# ---- the real decider ---------------------------------------------------------------------------

def load_decider(fake: bool, style: str = "compact") -> Decider:
    """The decider `model` mode loads (hosted Jev, jev.load: a network call per case, TYPESAFE_API_KEY or
    EVE_JEV_ROUTE=openrouter + OPENROUTER_API_KEY), or the keyword fake for tests."""
    if fake:
        return Decider(make_fake_predict(), style=style)
    from eve_bridge import jev
    from eve_bridge.envfile import load_env_file

    load_env_file()
    d = jev.load(style=style)
    print(f"model loaded in {d.load_ms:.0f} ms, warm-up {d.warm_ms:.0f} ms")
    return d


def shipped_settings() -> tuple[float, float, str, bool]:
    """(p_min, margin_min, style, FAST_LANE_DEFAULT) from fast_lane.py, imported lazily."""
    from eve_bridge import fast_lane

    t = fast_lane.Thresholds()
    return (float(t.p_min), float(t.margin_min), str(getattr(fast_lane, "DEFAULT_STYLE", "compact")),
            bool(fast_lane.FAST_LANE_DEFAULT))


# ---- the router (lane_router.py) on the fixtures ------------------------------------------------

class _FixtureSenses:
    """One fixture snapshot, forever: the router decides on it, and the click "changes the screen"."""

    def __init__(self, snapshot: Snapshot) -> None:
        self._snapshot = snapshot

    def snapshot(self) -> Snapshot:
        return self._snapshot

    def text_visible(self, _s: str) -> bool:
        return False

    def focused(self) -> Optional[Candidate]:
        return None

    def frontmost_pid(self) -> int:
        return int(self._snapshot.pid or 0)

    def screen_changed(self) -> Optional[bool]:
        return True


class _RecordingEffectors:
    def __init__(self) -> None:
        self.clicked: list[Candidate] = []

    def click_candidate(self, c: Candidate) -> str:
        self.clicked.append(c)
        return f"clicked {c.role} {c.label!r}"

    def focus_and_type(self, c: Candidate, text: str) -> str:
        return "refused: the router never types"

    def press(self, key: str) -> str:
        return "refused: the router never presses a key"

    def settle(self, secs: float) -> float:
        return 0.0


@dataclass
class RouterCase:
    fixture: str
    set_name: str
    app: str
    goal: str
    expected: str                # a candidate id, or "abstain"
    overlap: bool
    distractor: bool
    status: str                  # the RouteResult status
    reason: str
    clicked: list[str]           # candidate ids, in click order
    engaged: bool
    correct: bool                # engaged and the clicks are exactly [expected]
    sensitive: bool              # a clicked label is in the sensitive table — must never happen


def route_case(fixture: Fixture, case: dict[str, Any]) -> RouterCase:
    """The REAL router on one fixture case: every gate, the cover rule, the clause split, the
    System Settings doors — with effectors that record the click instead of making it."""
    from eve_bridge.lane_router import route

    goal = str(case["goal"])
    expected = "abstain" if case.get("expected") == "abstain" else str(case.get("expected_id"))
    effectors = _RecordingEffectors()
    result = route(goal, senses=_FixtureSenses(fixture.snapshot), effectors=effectors,
                   frontmost_app=fixture.snapshot.app, apps=ROUTER_APPS)
    ids = [c.id for c in effectors.clicked]
    engaged = bool(ids)
    return RouterCase(fixture=fixture.name, set_name=fixture.set_name, app=fixture.app, goal=goal,
                      expected=expected, overlap=bool(case.get("overlap", True)),
                      distractor=bool(case.get("distractor")), status=result.status, reason=result.reason,
                      clicked=ids, engaged=engaged, correct=engaged and ids == [expected],
                      sensitive=any(is_sensitive(c.label) for c in effectors.clicked))


def router_summary(results: list[RouterCase]) -> dict[str, Any]:
    engaged = [r for r in results if r.engaged]
    right = sum(1 for r in engaged if r.correct)
    clicks_expected = [r for r in results if r.expected != "abstain"]
    lo, hi = wilson(right, len(engaged)) if engaged else (0.0, 0.0)
    reasons: dict[str, int] = {}
    for r in results:
        if not r.engaged:
            # complete with no click: the control was already selected, and the router says so
            key = r.reason.split(":", 1)[0] or ("already_selected" if r.status == "complete" else r.status)
            reasons[key] = reasons.get(key, 0) + 1
    return {"n": len(results), "engaged": len(engaged), "right": right,
            "precision": right / len(engaged) if engaged else 0.0, "wilson": [lo, hi],
            "wrong": [f"{r.fixture}: {r.goal!r} clicked {r.clicked} expected {r.expected}" for r in engaged
                      if not r.correct],
            "engaged_on_abstain": sum(1 for r in engaged if r.expected == "abstain"),
            "coverage": (sum(1 for r in engaged if r.expected != "abstain") / len(clicks_expected)
                         if clicks_expected else 0.0),
            "sensitive": sum(1 for r in results if r.sensitive), "declined": dict(sorted(reasons.items())),
            "cost_per_case": ((right * COST_CORRECT - (len(engaged) - right) * COST_WRONG) / len(results)
                              if results else 0.0)}


def router_ship_decision(holdout: list[RouterCase]) -> tuple[bool, dict[str, Any]]:
    s = router_summary(holdout)
    ok = (s["engaged"] >= ROUTER_MIN_ENGAGED and s["precision"] >= ROUTER_MIN_PRECISION and s["sensitive"] == 0)
    return ok, s


def print_router(title: str, s: dict[str, Any], out: Callable[[str], None] = print) -> None:
    out(f"== router / {title}: n={s['n']}")
    out(f"   engaged {s['engaged']} ({_pct(s['coverage'])} of the click-expected cases), right {s['right']}: "
        f"precision {_pct(s['precision'])} (Wilson 95% {_pct(s['wilson'][0])}–{_pct(s['wilson'][1])})")
    out(f"   engaged on an abstain-expected case {s['engaged_on_abstain']}, sensitive clicks {s['sensitive']}, "
        f"cost-weighted {s['cost_per_case']:+.2f} s/case")
    out(f"   declined: {s['declined']}")
    for line in s["wrong"]:
        out(f"   WRONG {line}")


def cmd_router(args: argparse.Namespace) -> int:
    sets = load_sets(Path(args.fixtures))
    if not sets["holdout"]:
        print(f"ROUTER_EVAL_FAILED: no holdout fixtures under {args.fixtures}")
        return 1
    by_set = {name: [route_case(f, c) for f in fixtures for c in f.cases] for name, fixtures in sets.items()}
    for name in ("select", "holdout"):
        print_router(name, router_summary(by_set[name]))
    ok, s = router_ship_decision(by_set["holdout"])
    print(f"holdout: precision {_pct(s['precision'])} (need ≥ {_pct(ROUTER_MIN_PRECISION)}) on {s['engaged']} engaged "
          f"(need ≥ {ROUTER_MIN_ENGAGED}), sensitive clicks {s['sensitive']} (need 0)")
    print(f"ROUTER SHIP DECISION: {'enabled' if ok else 'disabled'} (EVE_LANE_FIRST default {'1' if ok else '0'})")
    if args.results_out:
        Path(args.results_out).write_text(json.dumps({
            "router": {name: [asdict(r) for r in rows] for name, rows in by_set.items()}, "ship": ok,
            "numbers": s}, ensure_ascii=False, indent=1), encoding="utf-8")
    print("ROUTER_EVAL_COMPLETE")
    return 0


def shipped_router() -> bool:
    """fast_lane.LANE_FIRST_DEFAULT, imported lazily (and replaceable in tests, like shipped_settings)."""
    from eve_bridge import fast_lane

    return bool(fast_lane.LANE_FIRST_DEFAULT)


# ---- subcommands --------------------------------------------------------------------------------

def cmd_fixtures(args: argparse.Namespace) -> int:
    root = Path(args.fixtures)
    sets = load_sets(root)
    errors = count_errors(sets, min_select=args.min_select, min_holdout=args.min_holdout, min_apps=args.min_apps,
                          min_no_overlap=args.min_no_overlap, min_distractor=args.min_distractor)
    n_sel, n_hold = sum(len(f.cases) for f in sets["select"]), sum(len(f.cases) for f in sets["holdout"])
    print(f"fixtures: select {len(sets['select'])} files / {n_sel} cases, holdout {len(sets['holdout'])} files / "
          f"{n_hold} cases, apps {sorted({f.app for f in sets['select'] + sets['holdout']})}")
    if errors:
        for e in errors:
            print(f"FIXTURES_INSUFFICIENT: {e}")
        return 1
    styles = [s.strip() for s in args.styles.split(",") if s.strip()]
    bad = [s for s in styles if s not in STYLES]
    if bad:
        print(f"unknown style(s) {bad}; choose from {STYLES}")
        return 2
    decider = load_decider(args.fake_predictor)
    select_by_style: dict[str, list[CaseResult]] = {}
    holdout_by_style: dict[str, list[CaseResult]] = {}
    for style in styles:
        select_by_style[style] = run_set(decider, sets["select"], style)
        holdout_by_style[style] = run_set(decider, sets["holdout"], style)
        p_min, m, reached = pick_thresholds(select_by_style[style])
        print_summary(f"style {style} / select", summarize(select_by_style[style], p_min, m))
        if not reached:
            print(f"   (no grid point reaches coverage ≥ {MIN_COVERAGE:.2f} on select; lowest point shown)")
        print_summary(f"style {style} / holdout", summarize(holdout_by_style[style], p_min, m))
    winner, (p_min, m), reached = choose_style(select_by_style)
    print(f"WINNER: style={winner} thresholds p_min={p_min:.2f} margin_min={m:.2f}"
          + ("" if reached else f" (coverage ≥ {MIN_COVERAGE:.2f} not reached on select; lowest grid point)"))
    ok, numbers = ship_decision(holdout_by_style[winner], p_min, m)
    print(f"holdout at the winner's thresholds: gated top-1 {_pct(numbers['gated_top1'])}, coverage "
          f"{_pct(numbers['coverage'])}, overlap:false real {_pct(numbers['no_overlap_real'])} vs keyword "
          f"{_pct(numbers['no_overlap_keyword'])}, cost-weighted {numbers['cost_weighted']:+.2f} s/case")
    print(f"SHIP DECISION: {'enabled' if ok else 'disabled'} (EVE_FAST_LANE default {'1' if ok else '0'})")
    if args.results_out:
        Path(args.results_out).write_text(json.dumps({
            "styles": {s: {"select": [asdict(r) for r in select_by_style[s]],
                           "holdout": [asdict(r) for r in holdout_by_style[s]]} for s in styles},
            "winner": winner, "thresholds": [p_min, m], "ship": ok, "numbers": numbers},
            ensure_ascii=False, indent=1), encoding="utf-8")
    print("EVAL_COMPLETE")
    return 0


def cmd_check_default(args: argparse.Namespace) -> int:
    root = Path(args.fixtures)
    sets = load_sets(root)
    if not sets["holdout"]:
        print(f"DEFAULT_MISMATCH: no holdout fixtures under {root}")
        return 1
    p_min, m, style, shipped = shipped_settings()
    decider = load_decider(args.fake_predictor, style)
    results = run_set(decider, sets["holdout"], style)
    ok, numbers = ship_decision(results, p_min, m)
    print(f"holdout n={numbers['n']} style={style} thresholds p_min={p_min:.2f} margin_min={m:.2f}")
    print(f"  gated top-1 {_pct(numbers['gated_top1'])} (need ≥ {_pct(SHIP_TOP1)})")
    print(f"  coverage    {_pct(numbers['coverage'])} (need ≥ {_pct(SHIP_COVERAGE)})")
    print(f"  overlap:false real {_pct(numbers['no_overlap_real'])} vs keyword {_pct(numbers['no_overlap_keyword'])}"
          f" (need real ≥ keyword + {_pct(SHIP_KEYWORD_LEAD)})")
    print(f"  cost-weighted {numbers['cost_weighted']:+.2f} s/case (need > 0)")
    print(f"  decision: {'enabled' if ok else 'disabled'}; shipped FAST_LANE_DEFAULT={shipped}")
    if ok != shipped:
        print(f"DEFAULT_MISMATCH: eval says {'enabled' if ok else 'disabled'}, FAST_LANE_DEFAULT is {shipped}")
        return 1
    router_ok, r = router_ship_decision([route_case(f, c) for f in sets["holdout"] for c in f.cases])
    router_shipped = shipped_router()
    print(f"router on holdout: precision {_pct(r['precision'])} on {r['engaged']} engaged (need ≥ "
          f"{_pct(ROUTER_MIN_PRECISION)} on ≥ {ROUTER_MIN_ENGAGED}), sensitive clicks {r['sensitive']} (need 0)")
    print(f"  decision: {'enabled' if router_ok else 'disabled'}; shipped LANE_FIRST_DEFAULT={router_shipped}")
    if router_ok != router_shipped:
        print(f"DEFAULT_MISMATCH: the router eval says {'enabled' if router_ok else 'disabled'}, "
              f"LANE_FIRST_DEFAULT is {router_shipped}")
        return 1
    print("DEFAULT_CONSISTENT")
    return 0


def cmd_live_snapshot(args: argparse.Namespace) -> int:
    pid = ax.pid_for_app(args.live_snapshot)
    if not pid:
        print(f"LIVE_SNAPSHOT_FAILED: no running app called {args.live_snapshot!r}")
        return 1
    budget = args.max_ms / 1000.0 if args.max_ms else ax.DEFAULT_BUDGET_SECS
    max_nodes = args.max_nodes or ax.DEFAULT_MAX_NODES
    t0 = time.perf_counter()
    snap = ax.ax_snapshot(pid, max_nodes=max_nodes, budget_secs=max(budget, 0.05), screen=ax.main_screen_points())
    wall_ms = (time.perf_counter() - t0) * 1000.0
    n_press = len(pressable(snap))
    print(f"{snap.app} pid {pid} — {snap.title!r}: node_count {snap.node_count}, elements {len(snap.elements)}, "
          f"pressable {n_press}, truncated {snap.truncated}, secs {snap.secs:.3f} (wall {wall_ms:.0f} ms)")
    if not snap.elements and not snap.node_count:
        print("LIVE_SNAPSHOT_FAILED: no window (the app has no focused window)")
        return 1
    if args.expect_truncated:
        if not snap.truncated:
            print("LIVE_SNAPSHOT_FAILED: expected truncated=True")
            return 1
        print("LIVE_SNAPSHOT_OK")
        return 0
    if args.require_complete and snap.truncated:
        print("LIVE_SNAPSHOT_FAILED: the walk was truncated")
        return 1
    if args.min_pressable and n_press < args.min_pressable:
        print(f"LIVE_SNAPSHOT_FAILED: {n_press} pressable candidates, need {args.min_pressable}")
        return 1
    if args.max_ms and snap.secs * 1000.0 > args.max_ms:
        print(f"LIVE_SNAPSHOT_FAILED: {snap.secs * 1000:.0f} ms, budget {args.max_ms} ms")
        return 1
    print("LIVE_SNAPSHOT_OK")
    return 0


def _norm(text: str) -> str:
    return " ".join(str(text or "").split()).casefold()


class LiveSenses:
    """The lane's senses on the real desktop, built from the public backends (no Helpers)."""

    def __init__(self, pid: int) -> None:
        from eve_bridge import desktop_helpers as dh

        self._dh = dh
        self.pid = pid
        self.screen = ax.main_screen_points()
        self._thumbs: list[Any] = []
        self.last: Optional[Snapshot] = None

    def snapshot(self) -> Snapshot:
        self.last = ax.ax_snapshot(self.pid, screen=self.screen)
        return self.last

    def text_visible(self, text: str) -> bool:
        needle = _norm(text)
        if not needle:
            return False
        if self.last is not None and any(needle in _norm(c.label) for c in self.last.elements):
            return True
        frame = self._dh.cg_capture()
        return any(needle in _norm(box["text"]) for box in self._dh.vision_ocr(frame, "fast"))

    def focused(self) -> Optional[Candidate]:
        return ax.ax_snapshot(self.pid, screen=self.screen).focused

    def frontmost_pid(self) -> int:
        return int(self._dh._focused_pid())

    def screen_changed(self) -> Optional[bool]:
        thumb = self._dh.cg_capture().thumb()
        self._thumbs.append(thumb)
        if len(self._thumbs) < 2:
            return None
        return self._dh.change_box(self._thumbs[-2], self._thumbs[-1], 0) is not None


class LiveEffectors:
    """Real clicks and keys; every click re-hit-tests the candidate's centre first."""

    def __init__(self, pid: int, dry_run: bool = False) -> None:
        import pyautogui

        from eve_bridge import desktop_helpers as dh

        pyautogui.FAILSAFE = True
        self._gui = pyautogui
        self._dh = dh
        self.pid = pid
        self.dry_run = dry_run
        self.clicks: list[tuple[int, int, str]] = []
        self.presses: list[str] = []
        self.typed: list[str] = []

    def click_candidate(self, c: Candidate) -> str:
        x, y, w, h = c.frame
        cx, cy = int(x + w / 2), int(y + h / 2)
        el = self._dh.ax_element_at(cx, cy)
        if el is None:
            return f"refused: nothing under ({cx},{cy})"
        title = el.get("title") or ""
        if _norm(title) != _norm(c.label):
            return f"refused: under ({cx},{cy}) is {el.get('role')} {title!r}, not {c.label!r}"
        if el.get("app") and c.app and _norm(el["app"]) != _norm(c.app):
            return f"refused: ({cx},{cy}) belongs to {el['app']}, not {c.app}"
        if is_sensitive(title):
            return f"refused: {title!r} is a sensitive control"
        if self.dry_run:
            return f"dry run: would click {c.role} {c.label!r} at ({cx},{cy})"
        self.clicks.append((cx, cy, c.label))
        self._gui.click(cx, cy)
        return f"clicked {c.role} {c.label!r} at ({cx},{cy})"

    def focus_and_type(self, c: Candidate, text: str) -> str:
        line = self.click_candidate(c)
        if line.startswith("refused") or self.dry_run:
            return line
        snap = ax.ax_snapshot(self.pid, screen=ax.main_screen_points())
        f = snap.focused
        if f is None or not f.editable or any(abs(a - b) > 2 for a, b in zip(f.frame, c.frame, strict=True)):
            return "refused: the focused element is not the field that was clicked"
        self.typed.append(text)
        self._gui.write(text)
        return f"typed {len(text)} characters into {c.role} {c.label!r}"

    def press(self, key: str) -> str:
        if self.dry_run:
            return f"dry run: would press {key}"
        self.presses.append(key)
        self._gui.press(key)
        return f"pressed {key}"

    def settle(self, secs: float) -> float:
        t0 = time.perf_counter()
        prev = self._dh.cg_capture().thumb()
        while time.perf_counter() - t0 < secs:
            time.sleep(0.1)
            cur = self._dh.cg_capture().thumb()
            if self._dh.change_box(prev, cur, 0) is None:
                break
            prev = cur
        return time.perf_counter() - t0


def _find_label(snap: Snapshot, label: str) -> Optional[Candidate]:
    return next((c for c in pressable(snap) if _norm(c.label) == _norm(label)), None)


def _click_steps(result: Any) -> list[Any]:
    return [s for s in result.steps if getattr(s, "kind", "") == "click" and getattr(s, "verdict", "") == "act"]


def _print_steps(result: Any) -> None:
    for s in result.steps:
        print(f"   step {s.n}: k={s.k} pick={s.chosen!r} {s.description!r} p_top={s.p_top:.2f} margin={s.margin:.2f}"
              f" verdict={s.verdict} changed={s.changed}  snapshot {s.snapshot_ms:.0f} ms ({s.node_count} nodes,"
              f" truncated={s.truncated}) decide {s.decide_ms:.0f} ms act {s.act_ms:.0f} ms settle {s.settle_ms:.0f} ms")
    print(f"   result: {result.line}")


def cmd_live_delegate(args: argparse.Namespace) -> int:
    from eve_bridge.fast_lane import run_delegate

    app, objective = args.live_delegate
    pid = ax.pid_for_app(app)
    if not pid:
        print(f"LIVE_DELEGATE_FAILED: no running app called {app!r}")
        return 1
    senses = LiveSenses(pid)
    first = senses.snapshot()
    target = _find_label(first, args.done_when)
    if target is None:
        print(f"PRECONDITION_NOT_MET: no pressable control labelled {args.done_when!r} in {app}")
        return 1
    if target.value == "selected":
        print(f"PRECONDITION_NOT_MET: {args.done_when!r} is already selected in {app}")
        return 1
    decider = load_decider(args.fake_predictor)
    common = dict(senses=senses, decider=decider, done_when=args.done_when, max_steps=args.max_steps)
    print(f"-- dry run: {objective!r} in {app}")
    dry = LiveEffectors(pid, dry_run=True)
    r = run_delegate(objective, effectors=dry, dry_run=True, **common)
    _print_steps(r)
    if dry.clicks or dry.presses or dry.typed:
        print("LIVE_DELEGATE_FAILED: the dry run produced input")
        return 1
    print(f"-- real run: {objective!r} in {app}")
    eff = LiveEffectors(pid)
    r = run_delegate(objective, effectors=eff, **common)
    _print_steps(r)
    clicks = _click_steps(r)
    after = senses.snapshot()
    reached = _find_label(after, args.done_when)
    if r.status != "done":
        print(f"LIVE_DELEGATE_FAILED: status {r.status}")
        return 1
    if len(eff.clicks) != 1 or len(clicks) != 1 or _norm(eff.clicks[0][2]) != _norm(args.done_when):
        print(f"LIVE_DELEGATE_FAILED: expected exactly one click on {args.done_when!r}, got {eff.clicks}")
        return 1
    if args.require_transition and clicks[0].changed is not True:
        print("LIVE_DELEGATE_FAILED: the click step did not report changed=true")
        return 1
    if reached is None or reached.value != "selected":
        print(f"LIVE_DELEGATE_FAILED: the post-run snapshot does not show {args.done_when!r} selected")
        return 1
    print(f"-- negative control: {objective!r} again from the reached state")
    eff2 = LiveEffectors(pid)
    r2 = run_delegate(objective, effectors=eff2, **common)
    _print_steps(r2)
    if r2.status != "done" or eff2.clicks:
        print(f"LIVE_DELEGATE_FAILED: negative control status {r2.status}, clicks {eff2.clicks}")
        return 1
    print("LIVE_DELEGATE_DONE")
    return 0


def _front_app() -> str:
    from eve_bridge.desktop_helpers import ax_frontmost

    try:
        return str(ax_frontmost().get("app") or "")
    except Exception:  # noqa: BLE001 — unknown is "not in front yet"
        return ""


def _selected(app: str, label: str) -> Optional[bool]:
    """Is the control labelled `label` selected in `app` right now? None when there is no such control."""
    pid = ax.pid_for_app(app)
    if not pid:
        return None
    found = _find_label(ax.ax_snapshot(pid, screen=ax.main_screen_points()), label)
    return None if found is None else found.value == "selected"


def _route_once(goal: str) -> tuple[str, float, int, Optional[Path]]:
    """One task through the real ComputerAgent and a real desktop worker, with the router on and a
    planner that records every call and refuses: (what it said, wall seconds, planner calls, run log)."""
    import asyncio
    import tempfile

    from eve_bridge.computer_agent import AgentConfig, ComputerAgent

    calls: list[dict[str, Any]] = []

    async def planner(request: dict[str, Any]) -> dict[str, Any]:
        calls.append(request)
        raise RuntimeError("the planner was called")

    cfg = AgentConfig(runs_dir=Path(tempfile.mkdtemp(prefix="lane-route-")), lane_first=True, verify=False)
    agent = ComputerAgent(planner, config=cfg)
    t0 = time.perf_counter()
    said = asyncio.run(agent.run(goal))
    return said, time.perf_counter() - t0, len(calls), agent.run_log


def cmd_live_route(args: argparse.Namespace) -> int:
    """A real lane-first task, end to end: worker start, the router, the click, the sentence."""
    import subprocess

    app, goal = args.live_route
    expect, restore = args.expect_selected, args.restore
    if not expect:
        print("LIVE_ROUTE_FAILED: --live-route needs --expect-selected LABEL")
        return 1
    os.environ["EVE_LANE_FIRST"] = "1"            # the worker child reads it
    subprocess.run(["open", "-a", app], check=False)
    deadline = time.time() + 8.0
    while time.time() < deadline and _norm(_front_app()) != _norm(app):
        time.sleep(0.2)
    if _norm(_front_app()) != _norm(app):
        print(f"PRECONDITION_NOT_MET: {app} did not come to the front")
        return 1
    state = _selected(app, expect)
    if state is None:
        print(f"PRECONDITION_NOT_MET: no control labelled {expect!r} in {app}")
        return 1
    if state and restore:
        said, secs, calls, _log = _route_once(restore)
        print(f"-- precondition: {expect!r} was already selected; routed {restore!r} first: {said!r} ({secs:.2f} s)")
        state = _selected(app, expect)
    if state:
        print(f"PRECONDITION_NOT_MET: {expect!r} is already selected in {app}")
        return 1
    said, secs, calls, run_log = _route_once(goal)
    print(f"-- {goal!r} in {app}: said {said!r}; wall {secs:.2f} s (worker start included); planner calls {calls}")
    if run_log is not None and run_log.exists():
        for line in run_log.read_text(encoding="utf-8").splitlines():
            if '"lane_first"' in line:
                print(f"   run log: {line}")
    ok = calls == 0 and said.startswith("Done.") and _selected(app, expect) is True
    if restore:
        back, back_secs, back_calls, _log = _route_once(restore)
        print(f"-- restored with {restore!r}: said {back!r}; wall {back_secs:.2f} s; planner calls {back_calls}")
    if not ok:
        print(f"LIVE_ROUTE_FAILED: planner calls {calls}, said {said!r}, {expect!r} selected: {_selected(app, expect)}")
        return 1
    print(f"LIVE_ROUTE_DONE wall={secs:.2f}s planner_calls=0")
    return 0


def shadow_entries(lines: list[str]) -> list[dict[str, Any]]:
    """Verify entries with both the model's verdict and the local p_true, from run-log lines."""
    out = []
    for line in lines:
        try:
            entry = json.loads(line)
        except ValueError:
            continue
        v = entry.get("verify") if isinstance(entry, dict) else None
        if not isinstance(v, dict) or not isinstance(v.get("valid"), bool):
            continue
        local = v.get("local")
        if not isinstance(local, dict) or not isinstance(local.get("p_true"), (int, float)):
            continue
        out.append({"valid": v["valid"], "p_true": float(local["p_true"])})
    return out


def shadow_confusion(entries: list[dict[str, Any]], cut: float = 0.5) -> dict[str, int]:
    c = {"tp": 0, "fp": 0, "fn": 0, "tn": 0}
    for e in entries:
        local_true = e["p_true"] >= cut
        if local_true and e["valid"]:
            c["tp"] += 1
        elif local_true:
            c["fp"] += 1
        elif e["valid"]:
            c["fn"] += 1
        else:
            c["tn"] += 1
    return c


def histogram(values: list[float], bins: int = 10) -> list[str]:
    """"[0.0:3]" style counts over [0, 1) tenths; 1.0 lands in the last bin."""
    counts = [0] * bins
    for v in values:
        counts[min(bins - 1, max(0, int(v * bins)))] += 1
    return [f"[{i / bins:.1f}:{c}]" for i, c in enumerate(counts)]


def cmd_shadow_report(args: argparse.Namespace) -> int:
    runs = Path(os.path.expanduser(args.runs_dir))
    files = sorted(runs.glob("*.jsonl")) if runs.is_dir() else []
    entries: list[dict[str, Any]] = []
    for f in files:
        entries.extend(shadow_entries(f.read_text(encoding="utf-8").splitlines()))
    n = len(entries)
    print(f"run logs: {len(files)} files under {runs}; n_shadow={n}")
    if n:
        c = shadow_confusion(entries)
        agree = (c["tp"] + c["tn"]) / n
        print(f"confusion (local p_true ≥ 0.5 vs model valid): tp {c['tp']} fp {c['fp']} fn {c['fn']} tn {c['tn']}"
              f"  agreement {_pct(agree)}")
        ps = [e["p_true"] for e in entries]
        print(f"p_true p10/p50/p90 {percentiles(ps)}; histogram " + " ".join(histogram(ps)))
    if n < args.min_runs:
        print(f"SHADOW_REPORT_INCOMPLETE: n={n}, need {args.min_runs}")
        return 1
    print("SHADOW_REPORT_OK")
    return 0


def cmd_make_fixture(args: argparse.Namespace) -> int:
    dump = json.loads(Path(args.make_fixture).read_text(encoding="utf-8"))
    bundle = str(dump.get("raw", {}).get("bundle") or "")
    data = make_fixture(dump, set_name=args.set, redacted=args.redacted, version=app_version(bundle) if bundle else "")
    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(data, ensure_ascii=False, indent=1) + "\n", encoding="utf-8")
    snap = snapshot_from_dict(data["snapshot"])
    print(f"wrote {out}: {snap.app} {snap.title!r}, {len(snap.elements)} elements, {len(pressable(snap))} pressable,"
          f" redacted={data['captured']['redacted']}, cases: 0 (fill them in)")
    return 0


def cmd_redact(args: argparse.Namespace) -> int:
    path = Path(args.redact)
    data = json.loads(path.read_text(encoding="utf-8"))
    mapping: dict[str, str] = {}
    for item in args.map or ():
        if "=" not in item:
            print(f"--map needs 'Personal Label=Synthetic Label', got {item!r}")
            return 2
        old, new = item.split("=", 1)
        mapping[old] = new
    if args.map_file:
        mapping.update(json.loads(Path(args.map_file).read_text(encoding="utf-8")))
    out, hits = redact_fixture(data, mapping)
    path.write_text(json.dumps(out, ensure_ascii=False, indent=1) + "\n", encoding="utf-8")
    missing = sorted(set(mapping) - set(hits))
    print(f"redacted {path}: {len(hits)} replacements" + (f"; not found: {missing}" if missing else ""))
    return 0


# ---- CLI ---------------------------------------------------------------------------------------

def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(prog="fastlane_eval", description=__doc__.split("\n\n")[0])
    p.add_argument("--fixtures", metavar="DIR", help="the offline eval over DIR/select and DIR/holdout "
                   "(with --check-default: the ship decision over DIR/holdout)")
    p.add_argument("--check-default", action="store_true", help="the ship decision vs FAST_LANE_DEFAULT")
    p.add_argument("--router", action="store_true", help="with --fixtures: measure the lane-first router instead")
    p.add_argument("--live-route", nargs=2, metavar=("APP", "GOAL"),
                   help="one real lane-first task through ComputerAgent; the planner must never be called")
    p.add_argument("--expect-selected", default=None, metavar="LABEL", help="--live-route: the control that ends selected")
    p.add_argument("--restore", default=None, metavar="GOAL", help="--live-route: a goal that puts the app back")
    p.add_argument("--live-snapshot", metavar="APP", help="one real accessibility snapshot of a running app")
    p.add_argument("--live-delegate", nargs=2, metavar=("APP", "OBJECTIVE"), help="one real delegate run")
    p.add_argument("--shadow-report", action="store_true", help="local-vs-model verdict agreement from run logs")
    p.add_argument("--make-fixture", metavar="RAW_JSON", help="a fixture file from the ax_candidates --json dump")
    p.add_argument("--redact", metavar="FIXTURE", help="replace personal labels in a fixture, re-derive it")
    p.add_argument("--min-select", type=int, default=0)
    p.add_argument("--min-holdout", type=int, default=0)
    p.add_argument("--min-apps", type=int, default=0)
    p.add_argument("--min-no-overlap", type=int, default=0)
    p.add_argument("--min-distractor", type=int, default=0)
    p.add_argument("--styles", default=",".join(STYLES))
    p.add_argument("--fake-predictor", action="store_true", help="a keyword fake instead of the model (tests)")
    p.add_argument("--results-out", help="write every case result as JSON here")
    p.add_argument("--min-pressable", type=int, default=0)
    p.add_argument("--max-ms", type=float, default=0.0)
    p.add_argument("--max-nodes", type=int, default=0)
    p.add_argument("--require-complete", action="store_true")
    p.add_argument("--expect-truncated", action="store_true")
    p.add_argument("--done-when", default=None)
    p.add_argument("--max-steps", type=int, default=3)
    p.add_argument("--require-transition", action="store_true")
    p.add_argument("--runs-dir", default=DEFAULT_RUNS_DIR)
    p.add_argument("--min-runs", type=int, default=5)
    p.add_argument("--out", help="for --make-fixture: the fixture path to write")
    p.add_argument("--set", choices=SETS, default="select")
    p.add_argument("--redacted", action="store_true", help="for --make-fixture: no personal labels in this capture")
    p.add_argument("--map", action="append", help="for --redact: 'Personal Label=Synthetic Label' (repeatable)")
    p.add_argument("--map-file", help="for --redact: a JSON object of the same mapping")
    return p


def main(argv: Optional[list[str]] = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    modes = [m for m in ("live_snapshot", "live_delegate", "live_route", "shadow_report", "make_fixture", "redact")
             if getattr(args, m)]
    if len(modes) > 1 or (modes and (args.check_default or args.fixtures)):
        parser.error("one mode at a time")
    if args.live_snapshot:
        return cmd_live_snapshot(args)
    if args.live_delegate:
        if not args.done_when:
            parser.error("--live-delegate needs --done-when")
        return cmd_live_delegate(args)
    if args.live_route:
        return cmd_live_route(args)
    if args.shadow_report:
        return cmd_shadow_report(args)
    if args.make_fixture:
        if not args.out:
            parser.error("--make-fixture needs --out")
        return cmd_make_fixture(args)
    if args.redact:
        return cmd_redact(args)
    if args.check_default:
        args.fixtures = args.fixtures or "tests/fixtures/ax"
        return cmd_check_default(args)
    if args.fixtures:
        return cmd_router(args) if args.router else cmd_fixtures(args)
    parser.error("choose a mode: --fixtures, --check-default, --live-snapshot, --live-delegate, --shadow-report, "
                 "--make-fixture or --redact")
    return 2


if __name__ == "__main__":
    sys.exit(main())
