#!/usr/bin/env python3
"""Build apps with the REAL maker, end to end, and write down how each build went.

    .venv/bin/python tools/app_eval.py OUT "a habit tracker with daily check-ins and streaks" ["a tip splitter" ...]
    .venv/bin/python tools/app_eval.py OUT --edit habit-tracker "add a monthly calendar view"
    .venv/bin/python tools/app_eval.py OUT --check DIR     # only the check (script + journeys) of DIR/index.html
    options: --effort high  --max-repairs 2  --model claude-opus-5-5  --jobs 1

Each request goes through apps_maker.AppMaker exactly as a build from the phone does: Claude
(``claude-opus-5-5`` through OpenRouter, the owner's ``OPENROUTER_API_KEY`` from ``~/.config/eve-bridge/env``,
never printed),
the static checks, the headless phone check (app_check.py) and up to ``--max-repairs`` repair rounds. The apps go
to ``OUT/apps`` (never the owner's real apps folder, so ``--edit`` changes an app an earlier run of this tool
built there) and each build's screenshot to ``OUT/screenshots/<slug>.jpg`` (an ``--edit`` to ``<slug>-2.jpg``,
``-3``..., so the picture before the change is kept).

``OUT/report.json`` gets one entry per build, appended to what earlier runs wrote: the request, whether it was
built, seconds, dollars, repair rounds, the check's summary and remaining issues, the screenshot's path, and the
token usage of every Claude call (so prompt caching can be seen working: repair rounds should read the prefix
from cache), and the journeys Jev walked (jev_verify.py: each result, Jev's calls, milliseconds and dollars; Jev
runs on the route in EVE_JEV_ROUTE with its key from the env file, never printed). ``--check DIR`` runs only
the check on ``DIR/index.html`` with ``DIR/journeys.json`` (a page broken by hand: the negative control) and
appends it under "checks". This tool measures; whether an app is good is for a person to judge from the screenshot and by
opening ``OUT/apps/<slug>/index.html``.
"""

from __future__ import annotations

import argparse
import asyncio
import contextvars
import datetime as dt
import json
import os
import sys
import time
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "src"))

ENV_FILE = Path.home() / ".config" / "eve-bridge" / "env"
PASSED_ENV = ("OPENROUTER_API_KEY", "EVE_TIMEZONE", "EVE_LOCATION")
# What jev_verify.from_env reads, put into this process's environment (the check reads os.environ); never printed.
JEV_ENV = ("EVE_JEV_ROUTE", "EVE_JEV_MODEL", "EVE_JEV_TIMEOUT", "OPENROUTER_API_KEY", "TYPESAFE_API_KEY",
           "EVE_APP_JOURNEYS")
# The Claude calls of the build running in this task, so concurrent builds (--jobs) keep their own.
CALLS: contextvars.ContextVar[list[dict[str, Any]]] = contextvars.ContextVar("calls")


def owner_env() -> dict[str, str]:
    """The key and the owner's clock/location settings: the shell's first, then the env file's. Never printed."""
    from eve_bridge.envfile import parse_env_file

    try:
        file = parse_env_file(ENV_FILE.read_text())
    except OSError:
        file = {}
    for k in JEV_ENV:
        if not os.environ.get(k) and file.get(k):
            os.environ[k] = file[k]
    return {k: os.environ.get(k) or file.get(k, "") for k in PASSED_ENV}


def usage_row(usage: Any) -> dict[str, int]:
    def n(name: str) -> int:
        v = usage.get(name) if isinstance(usage, dict) else getattr(usage, name, None)
        return int(v or 0)

    return {k: n(k) for k in ("input_tokens", "output_tokens", "cache_read_input_tokens",
                              "cache_creation_input_tokens")}


async def build_one(maker: Any, out: Path, request: str, edit: str) -> dict[str, Any]:
    from eve_bridge import apps_maker

    stages: list[str] = []
    t0 = time.perf_counter()
    label = f"--edit {edit}" if edit else request[:60]

    def progress(stage: str, n: int) -> None:
        if not stages or stages[-1] != stage:
            stages.append(stage)
            extra = f" ({n} problem{'s' if n != 1 else ''})" if stage == "fixing" else ""
            print(f"  [{time.perf_counter() - t0:5.0f} s] {label}: {stage}{extra}", file=sys.stderr, flush=True)

    from eve_bridge import jev
    from eve_bridge.pricing import estimate_jev_cost

    calls: list[dict[str, Any]] = []
    CALLS.set(calls)
    jev0 = jev.METER.snapshot()           # every round's Jev requests (with --jobs 1, only this build's)
    try:
        made = await (maker.edit(edit, request, progress) if edit else maker.make(request, progress))
    except Exception as e:  # noqa: BLE001 — one failed build is a row in the report, not the end of the run
        return {"request": request, "edit": edit, "built": False, "reason": f"{type(e).__name__}: {str(e)[:300]}",
                "seconds": round(time.perf_counter() - t0, 1), "calls": calls}
    row: dict[str, Any] = {
        "request": request, "edit": edit, "built": made.ok, "reason": made.reason,
        "seconds": round(made.secs, 1), "usd": round(made.usd, 4), "rounds": made.rounds,
        "check": made.check.summary() if made.check else "not checked",
        "check_seconds": round(made.check.secs, 1) if made.check else None,
        "issues": made.issues, "stages": stages, "calls": calls, "screenshot": None,
        "journeys": made.check.journeys.as_dict() if made.check and made.check.journeys else None,
        "rounds_found": made.history,
    }
    spent_jev = jev.Meter.delta(jev0, jev.METER.snapshot())
    row["jev_all_rounds"] = {**spent_jev, "usd": round(estimate_jev_cost(spent_jev["input_tokens"]), 6)}
    if made.app is not None:
        row.update({"slug": made.app.slug, "title": made.app.title, "icon": made.app.icon,
                    "description": made.app.description, "versions": made.app.versions,
                    "html": str(out / "apps" / made.app.slug / "index.html"),
                    "bytes": len((maker.store.html(made.app.slug) or "").encode()),
                    "static_issues": apps_maker.static_issues(maker.store.html(made.app.slug) or "")})
        if made.check is not None and made.check.screenshot:
            shot = screenshot_path(out, made.app.slug)
            shot.parent.mkdir(parents=True, exist_ok=True)
            shot.write_bytes(made.check.screenshot)
            row["screenshot"], row["photo_from"] = str(shot), made.check.photo_from
    return row


def screenshot_path(out: Path, slug: str) -> Path:
    """Where a build's screenshot goes: ``<slug>.jpg`` the first time, then ``<slug>-2.jpg``, ``-3``... An
    ``--edit`` never overwrites the picture of the build before it, which is what it is compared with."""
    folder = out / "screenshots"
    n = 1
    while (folder / (f"{slug}.jpg" if n == 1 else f"{slug}-{n}.jpg")).exists():
        n += 1
    return folder / (f"{slug}.jpg" if n == 1 else f"{slug}-{n}.jpg")


async def check_only(out: Path, app: Path) -> int:
    """The check alone (script, then journeys) on a page on disk, appended to OUT/report.json under "checks"."""
    from eve_bridge import app_check, jev_verify

    html = (app / "index.html").read_text()
    try:
        journeys, _ = jev_verify.parse_journeys(json.loads((app / "journeys.json").read_text()))
    except (OSError, ValueError):
        journeys = []
    r = await app_check.check_app(html, journeys=journeys or None)
    row = {"at": dt.datetime.now().isoformat(timespec="seconds"), "dir": str(app), "check": r.summary(),
           "check_seconds": round(r.secs, 1), "issues": r.issues,
           "journeys": r.journeys.as_dict() if r.journeys else None}
    out.mkdir(parents=True, exist_ok=True)
    path = out / "report.json"
    try:
        report = json.loads(path.read_text())
    except (OSError, ValueError):
        report = {"apps": []}
    report["checks"] = list(report.get("checks", [])) + [row]
    path.write_text(json.dumps(report, indent=2, ensure_ascii=False))
    print(f"check: {r.summary()} in {r.secs:.1f} s")
    for issue in r.issues:
        print(f"    - {issue}")
    return 0


async def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("out", type=Path, help="output folder: apps/, screenshots/ and report.json go here")
    ap.add_argument("requests", nargs="*", help="one request per app to build")
    ap.add_argument("--edit", nargs=2, metavar=("SLUG", "CHANGE"), help="change an app an earlier run built in OUT")
    ap.add_argument("--check", type=Path, metavar="DIR", help="only check DIR/index.html with DIR/journeys.json")
    ap.add_argument("--model", default="claude-opus-5-5")
    ap.add_argument("--effort", default="high", choices=("low", "medium", "high", "xhigh", "max"))
    ap.add_argument("--max-repairs", type=int, default=2)
    ap.add_argument("--jobs", type=int, default=1, help="builds at once")
    args = ap.parse_args()
    if not args.requests and not args.edit and not args.check:
        ap.error("give at least one request, or --edit SLUG CHANGE, or --check DIR")
    if args.check:
        owner_env()
        return await check_only(args.out.resolve(), args.check.resolve())

    from eve_bridge import apps_maker, openrouter
    from eve_bridge.miniapp import cost_usd

    out = args.out.resolve()
    if out / "apps" == apps_maker.DEFAULT_ROOT.resolve() or out == apps_maker.DEFAULT_ROOT.resolve():
        ap.error("OUT would be the owner's real apps folder; pick another")
    env = owner_env()
    if not env["OPENROUTER_API_KEY"]:
        print("No OPENROUTER_API_KEY in the environment or in ~/.config/eve-bridge/env.", file=sys.stderr)
        return 2
    (out / "apps").mkdir(parents=True, exist_ok=True)

    real = apps_maker.claude_generate(args.model, args.effort,
                                      client=openrouter.anthropic_client(env["OPENROUTER_API_KEY"]))

    async def generate(messages: list[dict[str, Any]], progress: Any) -> Any:
        gen = await real(messages, progress)
        CALLS.get([]).append({**usage_row(gen.usage), "stop": gen.stop_reason})
        return gen

    spent: list[float] = []
    maker = apps_maker.AppMaker(apps_maker.AppStore(out / "apps"), generate,
                                cost=lambda u: cost_usd(args.model, u), spend=spent.append,
                                context=lambda: apps_maker.owner_context(env), max_repairs=args.max_repairs)
    jobs = [(r, "") for r in args.requests] + ([(args.edit[1], args.edit[0])] if args.edit else [])
    gate = asyncio.Semaphore(max(1, args.jobs))

    report_path = out / "report.json"

    def keep(row: dict[str, Any]) -> None:
        """Each build's row goes to the report as soon as it is done: a run cut short keeps what it built."""
        try:
            report = json.loads(report_path.read_text())
        except (OSError, ValueError):
            report = {"apps": []}
        row.update({"at": dt.datetime.now().isoformat(timespec="seconds"), "model": args.model,
                    "effort": args.effort, "max_repairs": args.max_repairs})
        report["apps"] = list(report.get("apps", [])) + [row]
        report["total_usd"] = round(sum(a.get("usd", 0.0) or 0.0 for a in report["apps"]), 4)
        report_path.write_text(json.dumps(report, indent=2, ensure_ascii=False))

    async def run(request: str, edit: str) -> dict[str, Any]:
        async with gate:
            row = await build_one(maker, out, request, edit)
        keep(row)
        return row

    t0 = time.perf_counter()
    rows = await asyncio.gather(*(run(r, e) for r, e in jobs))

    for row in rows:
        mark = "built" if row["built"] else "FAILED"
        print(f"{mark}: {row.get('icon', '')} {row.get('title', row['request'][:50])} in {row['seconds']} s, "
              f"${row.get('usd', 0):.3f}, {row.get('rounds', 0)} repair round(s), check {row.get('check', '-')}")
        if row.get("journeys"):
            j = row["journeys"]["jev"]
            print(f"    Jev: {j.get('calls', 0)} calls, p50 {j.get('p50_ms', 0)} ms, ${j.get('usd', 0):.5f}")
        for issue in row.get("issues", []):
            print(f"    - {issue}")
        if row.get("screenshot"):
            print(f"    screenshot: {row['screenshot']}")
        if not row["built"]:
            print(f"    reason: {row.get('reason')}")
    print(f"this run: ${sum(spent):.3f} in {time.perf_counter() - t0:.0f} s; report: {report_path}")
    return 0 if all(r["built"] for r in rows) else 1


if __name__ == "__main__":
    sys.exit(asyncio.run(main()))
