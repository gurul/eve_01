#!/usr/bin/env python3
"""A live smoke check of the json-render card engine: the real text brain answers the ten dev cases
(tests/fixtures/openui_dev.json) as app turns with EVE_CARD_ENGINE=json-render, through tools/openui_eval.py.

    uv run python tools/card_eval_gate.py [--out replies.json]

The bar, fixed before the first run (2026-11-16), all of:
- at least 5 of the 6 cases labelled ui answer with a complete json-render block;
- every block in every reply passes the app's validator (ios/EveLink/Web/scripts/validate-spec.mjs);
- none of the 4 cases labelled plain answers with a block, closed or open;
- no reply holds the other engine's block; no provider error.
Prints LIVE_CARDS_OK only when the bar holds. Ten cases the prompt's author has read are a smoke check that the
engine works end to end, not evidence about how often it does: a default changes on a blind set, not on this.

The OpenRouter key and the brain's model and effort are read from the owner's env file by name (the same brain the
app talks to); nothing else from that file is used, and no value is printed.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path

BRIDGE = Path(__file__).resolve().parents[1]
ENV_FILE = Path.home() / ".config" / "eve-bridge" / "env"
WANTED = ("OPENROUTER_API_KEY", "EVE_TELEGRAM_MODEL", "EVE_TELEGRAM_EFFORT")


def from_env_file() -> dict[str, str]:
    found: dict[str, str] = {}
    try:
        lines = ENV_FILE.read_text(encoding="utf-8").splitlines()
    except OSError:
        return found
    for line in lines:
        name, sep, value = line.partition("=")
        if sep and name.strip() in WANTED:
            found[name.strip()] = value.strip().strip('"').strip("'")
    return found


def main() -> int:
    p = argparse.ArgumentParser()
    p.add_argument("--out", default="")
    args = p.parse_args()
    env = dict(os.environ)
    mine = from_env_file()
    env.setdefault("OPENROUTER_API_KEY", mine.get("OPENROUTER_API_KEY", ""))
    if not env.get("OPENROUTER_API_KEY"):
        print("no OPENROUTER_API_KEY in the environment or the env file")
        return 1
    out = Path(args.out) if args.out else Path(tempfile.mkdtemp(prefix="card-eval-")) / "replies.json"
    cmd = [sys.executable, str(BRIDGE / "tools" / "openui_eval.py"), "--engine", "json-render", "--arm", "json-render",
           "--out", str(out)]
    if mine.get("EVE_TELEGRAM_MODEL"):
        cmd += ["--model", mine["EVE_TELEGRAM_MODEL"]]
    if mine.get("EVE_TELEGRAM_EFFORT"):
        cmd += ["--effort", mine["EVE_TELEGRAM_EFFORT"]]
    run = subprocess.run(cmd, env=env, cwd=BRIDGE, capture_output=True, text=True, timeout=900)
    print(run.stdout[-6000:])
    if run.returncode != 0:
        print(run.stderr[-2000:])
        print(f"NOT OK: the eval exited {run.returncode}")
        return 1
    rows = json.loads(out.read_text(encoding="utf-8"))["rows"]
    ui = [r for r in rows if r["label"] == "ui"]
    plain = [r for r in rows if r["label"] == "plain"]
    blocks = sum(r["programs"] for r in rows)
    facts = {
        "cases": len(rows), "ui cases": len(ui), "plain cases": len(plain),
        "ui cases with a block": sum(bool(r["uses_ui"]) for r in ui),
        "blocks": blocks, "replies whose blocks all validate": sum(r["parse_ok"] is True for r in rows),
        "replies with a block that fails": sum(r["parse_ok"] is False for r in rows),
        "plain cases with a block": sum(bool(r["uses_ui"] or r["unclosed_fence"]) for r in plain),
        "other engine": sum(bool(r.get("other_engine")) for r in rows),
        "errors": sum(bool(r.get("error")) for r in rows),
        "model": sorted({str(r.get("model")) for r in rows}),
    }
    for k, v in facts.items():
        print(f"{k}: {v}")
    print(f"replies: {out}")
    ok = (len(ui) == 6 and len(plain) == 4 and facts["ui cases with a block"] >= 5 and blocks > 0
          and facts["replies with a block that fails"] == 0 and facts["plain cases with a block"] == 0
          and facts["other engine"] == 0 and facts["errors"] == 0)
    print("LIVE_CARDS_OK" if ok else "NOT OK: the bar did not hold")
    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
