#!/usr/bin/env python3
"""Fit and score the toolset router (toolset_router.py): which of the owner's toolsets a Telegram turn starts with.

    .venv/bin/python tools/toolset_eval.py --fit        # live Jev once over toolset_tuning.json (cached), fit GATES
    .venv/bin/python tools/toolset_eval.py --holdout    # live Jev ONCE over the blind toolset_holdout.json, the bar

The house recipe (tools/route_eval.py --search): fit on the tuning set only, then score ONCE on a holdout written
blind by another writer, against a bar written before any holdout result existed. Nothing is refitted after the
holdout; a failed bar is reported, not tuned away.

--fit: Jev's answers to the tuning set are cached in toolset_tuning_answers.json (a refit reads them, no calls);
per toolset the HIGHEST gate on toolset_router.GRID at which keywords + Jev open it for >= 98% of the tuning cases
that need it. Writes toolset_gates.json; the gates are then pasted into toolset_router.GATES by hand.

--holdout: reads the fitted toolset_gates.json (never fits), asks Jev once per holdout case (cached in
toolset_holdout_answers.json, so a second run re-scores the same answers and never re-asks), and prints the bar.
A Jev answer slower than toolset_router.TIMEOUT_S counts as no answer, as it would in a turn.

The datasets and their cached answers are history and are never edited: a label or a cached Noul for a toolset
no longer in toolset_router.TOOLSETS (2026-10-05: ordering delivery, removed with its integration) is dropped when
read (labels(), known()), so a case that needed only that toolset now needs none.

The request TEXT (synthetic, from the datasets) leaves the Mac to Jev on EVE_JEV_ROUTE.
"""

from __future__ import annotations

import argparse
import json
import statistics
import sys
import time
from pathlib import Path
from typing import Any, Callable, Optional

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "src"))

from eve_bridge import toolset_router as tr  # noqa: E402

ROUTES = Path(__file__).resolve().parents[1] / "tests" / "fixtures" / "routes"
TUNING, HOLDOUT = ROUTES / "toolset_tuning.json", ROUTES / "toolset_holdout.json"
TUNING_ANSWERS, HOLDOUT_ANSWERS = ROUTES / "toolset_tuning_answers.json", ROUTES / "toolset_holdout_answers.json"

# PRE-REGISTERED BAR (written by the orchestrator 2026-10-05, before any holdout result existed); verbatim in
# toolset_router's docstring and docs/toolset-router.md. This is the one place that computes it.
MIN_FULL_RECALL = 0.95          # (1) case-level full recall of keywords∪Jev
MAX_MEAN_EXTRAS = 0.6           # (3) mean unneeded toolsets opened per case
MAX_P95_MS = 800.0              # (4) Jev latency p95


def cases(path: Path) -> list[dict[str, Any]]:
    return json.loads(path.read_text(encoding="utf-8"))["cases"]


def labels(c: dict[str, Any]) -> set[str]:
    """A case's labels, restricted to the toolsets that exist now."""
    return set(c["toolsets"]) & set(tr.TOOLSETS)


def known(a: tr.ToolsetAnswer) -> tr.ToolsetAnswer:
    """A cached answer with only the Nouls of the toolsets that exist now (a removed toolset's Noul is history)."""
    return tr.ToolsetAnswer({k: v for k, v in a.nouls.items() if k in tr.TOOLSETS}, a.ms, a.error)


def live_predict() -> Callable[[Any, dict[str, Any]], dict[str, Any]]:
    import os

    from eve_bridge import jev
    from eve_bridge.envfile import load_env_file

    load_env_file()
    url, key, model = jev.route_config(os.environ)
    print(f"jev: {model} on {url.split('/')[2]}")
    # 8 s, not TIMEOUT_S: the eval measures Jev's real latency; the turn's budget is applied when scoring
    return jev.make_predict(url, key, model, timeout_s=8.0)


def answers_for(data: list[dict[str, Any]], cache: Path, predict: Optional[Callable[..., Any]],
                *, once: bool) -> list[tr.ToolsetAnswer]:
    """The cached answers when the cache covers every case id, else Jev asked for each case (and cached). ``once``:
    the holdout, whose cache is never overwritten once written."""
    if cache.exists():
        raw = json.loads(cache.read_text(encoding="utf-8"))
        if all(c["id"] in raw["answers"] for c in data):
            print(f"   answers from {cache.name} (asked {raw.get('asked', '?')}; no Jev calls)")
            return [known(tr.ToolsetAnswer.from_json(raw["answers"][c["id"]])) for c in data]
        if once:
            raise SystemExit(f"{cache.name} exists but does not cover the holdout: refusing to ask Jev again")
    predict = predict or live_predict()
    out = []
    for c in data:
        out.append(tr.ask(predict, c["text"], c["previous"], time.perf_counter))
    cache.write_text(json.dumps({"asked": time.strftime("%Y-%m-%d %H:%M"), "questions": tr.questions(),
                                 "answers": {c["id"]: a.to_json() for c, a in zip(data, out, strict=True)}},
                                indent=1) + "\n", encoding="utf-8")
    print(f"   asked Jev {len(out)} times; cached in {cache.name}")
    return out


def score(said: list[set[str]], needed: list[set[str]]) -> dict[str, Any]:
    n = len(needed)
    full = sum(1 for s, t in zip(said, needed, strict=True) if t <= s)
    extras = sum(len(s - t) for s, t in zip(said, needed, strict=True))
    missed = sum(len(t - s) for s, t in zip(said, needed, strict=True))
    per = {}
    for name in tr.TOOLSETS:
        opened = sum(1 for s in said if name in s)
        right = sum(1 for s, t in zip(said, needed, strict=True) if name in s and name in t)
        per[name] = {"recall": tr.recall(said, needed, name), "precision": right / opened if opened else None,
                     "opened": opened, "needed": sum(1 for t in needed if name in t)}
    return {"n": n, "full": full, "full_recall": full / n if n else 0.0, "mean_extras": extras / n if n else 0.0,
            "missed_toolsets": missed, "per": per}


def _pct(x: Optional[float]) -> str:
    return "n/a" if x is None else f"{100 * x:.1f}%"


def print_score(title: str, s: dict[str, Any]) -> None:
    print(f"== {title}: n={s['n']}  full recall {s['full']}/{s['n']} = {_pct(s['full_recall'])}  mean unneeded "
          f"{s['mean_extras']:.3f}  missed toolsets {s['missed_toolsets']}")
    for name, p in s["per"].items():
        print(f"   {name:<10} recall {_pct(p['recall']):>6} ({p['needed']} needed)  precision {_pct(p['precision']):>6} "
              f"({p['opened']} opened)")


def latency(answers: list[tr.ToolsetAnswer]) -> tuple[float, float]:
    ms = sorted(a.ms for a in answers)
    return statistics.median(ms), ms[int(0.95 * (len(ms) - 1))]


def fit_main(predict: Optional[Callable[..., Any]] = None, out: Path = tr.GATES_FILE) -> dict[str, float]:
    data = cases(TUNING)
    print(f"toolset router / fit on {TUNING.name}: {len(data)} cases")
    answers = answers_for(data, TUNING_ANSWERS, predict, once=False)
    needed = [labels(c) for c in data]
    plain = [tr._words(" ".join(c["text"].split())) for c in data]
    kw = [tr.keyword_toolsets(c["text"], c["previous"]) for c in data]
    # the follow-up policy, chosen here on tuning only: the previous message's keywords for a pointing follow-up
    print_score("keywords, message only", score(plain, needed))
    print_score("keywords, + previous for a follow-up (keyword_toolsets)", score(kw, needed))
    p50, p95 = latency(answers)
    print(f"   Jev latency ms p50 {p50:.0f}  p95 {p95:.0f}; errors {sum(1 for a in answers if a.error)}; slower than "
          f"{tr.TIMEOUT_S:g} s {sum(1 for a in answers if a.ms > tr.TIMEOUT_S * 1000)}")
    gates = tr.fit(answers, kw, needed)
    said = [k | set(tr.decide(a, gates, tr.TIMEOUT_S * 1000)) for a, k in zip(answers, kw, strict=True)]
    s = score(said, needed)
    print(f"   gates (highest on GRID with tuning recall >= {tr.MIN_TUNING_RECALL:.0%} per toolset): {gates}")
    for name in tr.TOOLSETS:
        if s["per"][name]["recall"] < tr.MIN_TUNING_RECALL:
            print(f"   NOTE {name}: no gate on GRID reaches {tr.MIN_TUNING_RECALL:.0%}; kept the lowest")
    print_score("keywords ∪ Jev on tuning (seen data: not evidence)", s)
    for i, c in enumerate(data):
        if needed[i] - said[i]:
            a = answers[i]
            print(f"   miss  {c['id']} {c['previous']!r} {c['text']!r} needed {sorted(needed[i])} opened "
                  f"{sorted(said[i])} nouls {{{', '.join(f'{k} {v:.2f}' for k, v in a.nouls.items() if v >= 0.05)}}}")
    out.write_text(json.dumps({"fitted": time.strftime("%Y-%m-%d"), "on": TUNING.name, "n": len(data),
                               "min_tuning_recall": tr.MIN_TUNING_RECALL, "grid": list(tr.GRID),
                               "gates": gates}, indent=1) + "\n", encoding="utf-8")
    print(f"   wrote {out.name}; paste into toolset_router.GATES")
    print("TOOLSET_FIT_COMPLETE")
    return gates


def bar(s: dict[str, Any], base: dict[str, Any], p95: float) -> dict[str, bool]:
    one = s["full_recall"] >= MIN_FULL_RECALL
    two = s["full_recall"] > base["full_recall"]
    three = s["mean_extras"] <= MAX_MEAN_EXTRAS
    four = p95 <= MAX_P95_MS
    return {"1": one, "2": two, "3": three, "4": four, "shipped": one and two and three and four}


def holdout_main(predict: Optional[Callable[..., Any]] = None, gates_path: Path = tr.GATES_FILE) -> bool:
    gates = tr.load_gates(gates_path)
    print(f"toolset router / holdout {HOLDOUT.name}, gates from {gates_path.name} (never refitted here): {gates}")
    data = cases(HOLDOUT)
    answers = answers_for(data, HOLDOUT_ANSWERS, predict, once=True)
    needed = [labels(c) for c in data]
    kw = [tr.keyword_toolsets(c["text"], c["previous"]) for c in data]
    budget = tr.TIMEOUT_S * 1000
    said = [k | set(tr.decide(a, gates, budget)) for a, k in zip(answers, kw, strict=True)]
    base, s = score(kw, needed), score(said, needed)
    p50, p95 = latency(answers)
    print(f"   Jev latency ms p50 {p50:.0f}  p95 {p95:.0f}; errors {sum(1 for a in answers if a.error)}; slower than "
          f"{tr.TIMEOUT_S:g} s (counted as no answer) {sum(1 for a in answers if a.ms > budget)}")
    print_score("keywords alone (baseline)", base)
    print_score("keywords ∪ Jev", s)
    b = bar(s, base, p95)
    print(f"   (1) full recall {_pct(s['full_recall'])} (need >= {MIN_FULL_RECALL:.0%}): {'pass' if b['1'] else 'FAIL'}")
    print(f"   (2) vs keywords alone {_pct(base['full_recall'])} (need strictly higher): {'pass' if b['2'] else 'FAIL'}")
    print(f"   (3) mean unneeded toolsets {s['mean_extras']:.3f} (need <= {MAX_MEAN_EXTRAS}): "
          f"{'pass' if b['3'] else 'FAIL'}")
    print(f"   (4) Jev p95 {p95:.0f} ms (need <= {MAX_P95_MS:.0f}): {'pass' if b['4'] else 'FAIL'}")
    print(f"   cases fully covered: keywords alone {base['full']}/{s['n']}, keywords ∪ Jev {s['full']}/{s['n']}; "
          f"open_app rounds (one per missed toolset): {base['missed_toolsets']} -> {s['missed_toolsets']}, saved "
          f"{base['missed_toolsets'] - s['missed_toolsets']}")
    for i, c in enumerate(data):
        if needed[i] - said[i]:
            a = answers[i]
            print(f"   MISSED {c['id']} prev {c['previous']!r} text {c['text']!r} needed {sorted(needed[i])} opened "
                  f"{sorted(said[i])} (keywords {sorted(kw[i])}; nouls "
                  f"{', '.join(f'{k} {v:.2f}' for k, v in a.nouls.items() if v >= 0.05)}"
                  f"{'; ' + a.error if a.error else ''}; {a.ms:.0f} ms)")
    print(f"SHIPPED={b['shipped']}")
    print("TOOLSET_HOLDOUT_COMPLETE")
    return b["shipped"]


def main(argv: Optional[list[str]] = None) -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    g = p.add_mutually_exclusive_group(required=True)
    g.add_argument("--fit", action="store_true", help="fit the gates on toolset_tuning.json")
    g.add_argument("--holdout", action="store_true", help="score once on the blind toolset_holdout.json")
    args = p.parse_args(argv)
    if args.fit:
        fit_main()
    else:
        holdout_main()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
