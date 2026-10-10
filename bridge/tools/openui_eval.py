#!/usr/bin/env python3
"""OpenUI eval for Eve Link app turns: does the real text brain answer with an openui-lang block when the answer
has a shape (a comparison, options, steps, a schedule, a chart, tap choices), plain text otherwise, and does the
block parse with the app's own parser?

    uv run python tools/openui_eval.py --cases CASES.json --arm after --out replies.json
    uv run python tools/openui_eval.py --rescore replies.json        # score saved replies again, offline

The replies are written to --out (or a temp file the run names) before anything is scored, so a validator failure
never loses them; --rescore scores that file.

Cases: a JSON list, or {"cases": [...]}. Each case has an ``id``, a ``label`` and the owner's ``text`` (``message``,
``prompt``, ``ask`` and ``input`` are read too). A label of ``ui`` (or ``openui``, ``yes``, true) means the answer
should be a block; ``plain`` (or ``text``, ``no``, false) means it should not; any other label is counted, never
scored. Optional ``history``: earlier lines, ``[{"role": "owner" | "eve", "text": ...}]``, noted as a turn notes
them. tests/fixtures/openui_dev.json holds ten dev cases.

Each case is a real app turn: a fresh ``TelegramInlet`` answers it with ``_FROM_APP`` set, exactly as
``hear(app=...)`` sets it for a turn from link_chat, so the request is built by the inlet itself (the app block,
link_chat.app_block, included) and the reply goes to ``app_out``, which records it. Nothing reaches Telegram (the Bot
API is a recorder) and every tool call is refused as its result, so no task, app or message leaves the eval. The
environment is built here, not read from the owner's env file: Telegram on with a made-up token and owner, memory,
records and web search off, spend written to a temp folder, and only ``OPENROUTER_API_KEY`` (every model call is on
OpenRouter) and ``PATH`` kept. ``--model`` / ``--effort`` set the brain (default: the code's DEFAULT_MODEL).

Scored by code. ``uses_ui``: the reply holds a complete ```openui-lang fence (opened and closed) or is a bare
``root = Card(...)`` program, as the app's replyParts.ts reads replies. ``parse_ok``: every program in it passes
ios/EveLink/Web/scripts/validate.mjs (the app's parser and component library: no parse error, no unknown
component, nothing undefined, not cut off); None when there is no program. A ``ui`` case passes with uses_ui and
parse_ok, a ``plain`` case passes without a block (an unclosed fence fails it).

The code under test is whatever ``src`` this file sits beside, and this file imports only what existed at 25d2728,
so a copy dropped into an older checkout's bridge/tools scores that checkout. The validator is that checkout's
validate.mjs when it has one, else ``--validator`` or $OPENUI_VALIDATOR (a validate.mjs from any checkout with
``pnpm install`` done: it resolves the parser from its own folder).
"""

from __future__ import annotations

import argparse
import asyncio
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Any, Optional

BRIDGE = Path(__file__).resolve().parents[1]
REPO = BRIDGE.parent
sys.path.insert(0, str(BRIDGE / "src"))

DEFAULT_VALIDATOR = REPO / "ios" / "EveLink" / "Web" / "scripts" / "validate.mjs"
DEV_CASES = BRIDGE / "tests" / "fixtures" / "openui_dev.json"
UI_LABELS = {"ui", "openui", "yes", "true"}
PLAIN_LABELS = {"plain", "text", "no", "false"}
TEXT_KEYS = ("text", "message", "prompt", "ask", "input")
ATTEMPTS = 3
KEEP_ENV = ("PATH", "OPENROUTER_API_KEY")

FENCE = "```openui-lang"
# `root = Card(` at the start of a line: a program, not prose that says "root = 5" (replyParts.ts ROOT_RE)
ROOT_RE = re.compile(r"(^|\n)[ \t]*root[ \t]*=[ \t]*[A-Z][A-Za-z0-9]*\(")


# ---- reading a reply the way the app does (ios/EveLink/Web/src/replyParts.ts splitReply) ---------------------

def programs(reply: str) -> tuple[list[str], bool]:
    """The complete OpenUI Lang programs in ``reply`` and whether a fence was left open. A reply with no fence that
    holds a ``root =`` program (with no ``` before it) is that program, from ``root =`` to the end."""
    text = reply or ""
    if FENCE not in text:
        m = ROOT_RE.search(text)
        if m and "```" not in text[:m.start() + 1]:
            return [text[m.start():].lstrip("\n")], False
        return [], False
    found: list[str] = []
    rest = text
    while True:
        at = rest.find(FENCE)
        if at == -1:
            return found, False
        body = rest.find("\n", at)
        if body == -1:
            return found, True
        close = rest.find("\n```", body)
        if close == -1:
            return found, True
        found.append(rest[body + 1:close])
        rest = rest[close + 4:]


def uses_ui(reply: str) -> bool:
    """A complete fence or a bare ``root =`` program."""
    return bool(programs(reply)[0])


def expected(label: Any) -> Optional[bool]:
    """True: the case wants a block; False: plain text; None: not scored."""
    key = str(label).strip().lower()
    return True if key in UI_LABELS else False if key in PLAIN_LABELS else None


def passed(label: Any, row: dict[str, Any]) -> Optional[bool]:
    want = expected(label)
    if want is None or row.get("error"):
        return None if want is None else False
    if want:
        return bool(row["uses_ui"] and row["parse_ok"])
    return not row["uses_ui"] and not row["unclosed_fence"]


# ---- the validator ---------------------------------------------------------------------------------------------

def find_validator(arg: str = "") -> Optional[Path]:
    for candidate in (arg, os.environ.get("OPENUI_VALIDATOR", ""), str(DEFAULT_VALIDATOR)):
        if candidate and Path(candidate).is_file():
            return Path(candidate).resolve()
    return None


def validate(codes: list[str], validator: Path, node: str = "") -> list[dict[str, Any]]:
    """Each program checked by validate.mjs --batch: [{ok, root, errors, orphaned}]."""
    if not codes:
        return []
    node = node or shutil.which("node") or "node"
    p = subprocess.run([node, str(validator), "--batch"], input=json.dumps(codes), capture_output=True, text=True,
                       timeout=120)
    if p.returncode not in (0, 1) or not p.stdout.strip():
        raise SystemExit(f"validator failed (exit {p.returncode}): {p.stderr.strip()[:500]}")
    results = json.loads(p.stdout)
    assert len(results) == len(codes), "the validator answered a different number of programs"
    return results


def score_rows(rows: list[dict[str, Any]], validator: Optional[Path]) -> None:
    """Fill uses_ui, unclosed_fence, parse_ok, parse_errors and pass on each row, in place."""
    codes: list[str] = []
    spans: list[tuple[int, int]] = []
    for r in rows:
        found, unclosed = programs(r.get("reply") or "")
        r["uses_ui"], r["unclosed_fence"], r["programs"] = bool(found), unclosed, len(found)
        spans.append((len(codes), len(codes) + len(found)))
        codes.extend(found)
    results = validate(codes, validator) if (codes and validator is not None) else []
    for r, (a, b) in zip(rows, spans, strict=True):
        if a == b:
            r["parse_ok"], r["parse_errors"] = None, []
        elif not results:
            r["parse_ok"], r["parse_errors"] = None, ["not validated"]
        else:
            mine = results[a:b]
            r["parse_ok"] = all(x["ok"] for x in mine)
            r["parse_errors"] = [e for x in mine for e in x["errors"]]
        r["pass"] = passed(r.get("label"), r)


# ---- running a case as a real app turn ---------------------------------------------------------------------------

class Recorder:
    """The Bot API as a recorder: every message the inlet sends is kept; nothing reaches Telegram."""

    def __init__(self) -> None:
        self.sent: list[str] = []
        self._id = 100

    async def send_message(self, chat_id: int, text: str, *a: Any, **k: Any) -> None:
        self.sent.append(text)

    async def send_inline(self, chat_id: int, text: str, *a: Any, **k: Any) -> int:
        self.sent.append(text)
        self._id += 1
        return self._id

    def __getattr__(self, name: str) -> Any:
        async def nothing(*a: Any, **k: Any) -> Any:
            self._id += 1
            return self._id
        return nothing


def environment(model: str, effort: str, scratch: Path) -> None:
    """os.environ as a made-up install: only PATH and the OpenRouter key survive (see the module text)."""
    keep = {k: os.environ[k] for k in KEEP_ENV if k in os.environ}
    if not keep.get("OPENROUTER_API_KEY"):
        raise SystemExit("OPENROUTER_API_KEY is not set: every model call goes through OpenRouter")
    os.environ.clear()
    os.environ.update(keep)
    home = scratch / "home"
    home.mkdir(parents=True, exist_ok=True)
    os.environ.update({
        "HOME": str(home),
        "EVE_ENV_FILE": str(scratch / "no-env-file"),
        "EVE_TELEGRAM": "1",
        "EVE_TELEGRAM_TOKEN": "0:openui-eval",
        "EVE_TELEGRAM_OWNER": "1",
        "EVE_MEMORY": "0",
        "EVE_RECORDS": "0",
        "EVE_WEB_SEARCH": "off",
        "EVE_SPEND_DIR": str(scratch / "spend"),
    })
    if model:
        os.environ["EVE_TELEGRAM_MODEL"] = model
    if effort:
        os.environ["EVE_TELEGRAM_EFFORT"] = effort


def case_text(case: dict[str, Any]) -> str:
    for key in TEXT_KEYS:
        if isinstance(case.get(key), str) and case[key].strip():
            return case[key]
    raise SystemExit(f"case {case.get('id')!r} has no text (one of {', '.join(TEXT_KEYS)})")


async def run_case(case: dict[str, Any], create: Any) -> dict[str, Any]:
    from eve_bridge import telegram as tg

    cfg = tg.configured()
    if not cfg.enabled:
        raise SystemExit("the made-up environment did not turn the chat on")
    sizes: list[int] = []
    out: list[tuple[str, str]] = []
    calls: list[str] = []

    async def counted(payload: dict[str, Any]) -> dict[str, Any]:
        sizes.append(len(str(payload.get("instructions") or "")))
        last: Optional[BaseException] = None
        for attempt in range(ATTEMPTS):
            try:
                return await create(payload)
            except Exception as e:  # noqa: BLE001 — retried, then the case is an error
                last = e
                await asyncio.sleep(2.0 * (attempt + 1))
        assert last is not None
        raise last

    inlet = tg.TelegramInlet(cfg, Recorder(), counted, toolset_route=None, grader=None)
    inlet._chat_id = 1

    async def refuse(name: str, args: dict[str, Any], chat_id: int) -> dict[str, Any]:
        calls.append(name)
        return {"ok": False, "reason": "that tool is not available right now"}

    inlet._tool = refuse

    async def no_owner_to_ask(name: str, args: Any, chat_id: int) -> None:
        return None                   # the taint gate passes the call on to refuse, which runs nothing

    inlet._gate = no_owner_to_ask

    def app_out(turn: str, text: str, kind: str) -> bool:
        out.append((kind, text))
        return True

    inlet.app_out = app_out
    for line in case.get("history") or []:
        role = "user" if str(line.get("role", "owner")) in ("owner", "user") else "eve"
        inlet._note(role, str(line.get("text") or ""))
    text = case_text(case)
    mark = tg._FROM_APP.set(f"openui-eval-{case['id']}")
    t0 = time.monotonic()
    error = ""
    try:
        await inlet._turn(tg.Inbound(1, 1, text, message_id=1000))
    except Exception as e:  # noqa: BLE001 — recorded as the case's error
        error = type(e).__name__
    finally:
        tg._FROM_APP.reset(mark)
    said = [t for k, t in out if k == "say"]
    reply = said[-1] if said else ""
    if not reply and not error:
        error = "no reply" if not inlet.api.sent else "reply went to Telegram"
    return {"id": case["id"], "label": case.get("label"), "text": text, "reply": reply, "lines": len(out),
            "tools": calls, "error": error, "secs": round(time.monotonic() - t0, 2),
            "instr_chars": sizes[0] if sizes else 0, "model": cfg.model}


async def run_all(cases: list[dict[str, Any]], jobs: int) -> list[dict[str, Any]]:
    from eve_bridge.computer_agent import make_response_creator

    create = make_response_creator()
    gate = asyncio.Semaphore(max(1, jobs))

    async def one(case: dict[str, Any]) -> dict[str, Any]:
        async with gate:
            return await run_case(case, create)

    return list(await asyncio.gather(*(one(c) for c in cases)))


# ---- reporting --------------------------------------------------------------------------------------------------

def load_cases(path: Path) -> list[dict[str, Any]]:
    data = json.loads(path.read_text(encoding="utf-8"))
    cases = data["cases"] if isinstance(data, dict) else data
    out = []
    for i, c in enumerate(cases):
        c = dict(c)
        c.setdefault("id", f"case-{i + 1}")
        c["id"] = str(c["id"])
        if "label" not in c:
            for key in ("expect_ui", "expect", "ui"):
                if key in c:
                    c["label"] = c[key]
                    break
        out.append(c)
    return out


def report(rows: list[dict[str, Any]], arm: str) -> dict[str, Any]:
    for r in rows:
        mark = {True: "PASS", False: "FAIL", None: "----"}[r.get("pass")]
        parse = {True: "ok", False: "BAD", None: "-"}[r.get("parse_ok")]
        extra = f"  ERROR {r['error']}" if r.get("error") else ""
        extra += "  unclosed fence" if r.get("unclosed_fence") else ""
        extra += f"  {'; '.join(r['parse_errors'])[:160]}" if r.get("parse_ok") is False else ""
        print(f"{mark}  [{arm}] {r['id']:<28} label={str(r.get('label')):<6} ui={'yes' if r.get('uses_ui') else 'no ':<3} "
              f"parse={parse:<3} {r.get('secs', 0):>5}s{extra}", flush=True)
    totals: dict[str, dict[str, int]] = {}
    for r in rows:
        t = totals.setdefault(str(r.get("label")), {"n": 0, "uses_ui": 0, "parse_ok": 0, "parse_bad": 0,
                                                     "pass": 0, "errors": 0})
        t["n"] += 1
        t["uses_ui"] += bool(r.get("uses_ui"))
        t["parse_ok"] += r.get("parse_ok") is True
        t["parse_bad"] += r.get("parse_ok") is False
        t["pass"] += r.get("pass") is True
        t["errors"] += bool(r.get("error"))
    for label, t in sorted(totals.items()):
        scored = expected(label) is not None
        print(f"== [{arm}] label={label}: n={t['n']} uses_ui={t['uses_ui']}/{t['n']} parse_ok={t['parse_ok']}/"
              f"{t['uses_ui']} parse_bad={t['parse_bad']} errors={t['errors']}"
              + (f" pass={t['pass']}/{t['n']}" if scored else " (not scored)"))
    scored_rows = [r for r in rows if r.get("pass") is not None]
    overall = sum(r["pass"] for r in scored_rows)
    print(f"== [{arm}] all: pass={overall}/{len(scored_rows)} uses_ui={sum(bool(r.get('uses_ui')) for r in rows)}/"
          f"{len(rows)} parse_ok={sum(r.get('parse_ok') is True for r in rows)}/"
          f"{sum(bool(r.get('uses_ui')) for r in rows)}")
    return {"arm": arm, "totals": totals, "pass": overall, "scored": len(scored_rows)}


def main(argv: Optional[list[str]] = None) -> int:
    p = argparse.ArgumentParser(prog="openui_eval", description=__doc__.split("\n\n")[0])
    p.add_argument("--cases", default=str(DEV_CASES), help="the cases JSON (default: the ten dev cases)")
    p.add_argument("--arm", default="this", help="a label for this run, printed and saved with every row")
    p.add_argument("--out", default="", help="write the rows (replies included) here as JSON, before scoring")
    p.add_argument("--model", default="", help="the brain's model (default: the code's DEFAULT_MODEL)")
    p.add_argument("--effort", default="", help="its reasoning effort (default: the code's DEFAULT_EFFORT)")
    p.add_argument("--jobs", type=int, default=4, help="cases answered at once")
    p.add_argument("--only", default="", help="run one case by id")
    p.add_argument("--validator", default="", help="validate.mjs to use (default: this checkout's, or $OPENUI_VALIDATOR)")
    p.add_argument("--rescore", nargs="*", default=None, help="score saved --out files again, offline")
    args = p.parse_args(argv)
    validator = find_validator(args.validator)
    if validator is None:
        raise SystemExit("no validate.mjs: pass --validator or set OPENUI_VALIDATOR (and run pnpm install beside it)")
    if args.rescore is not None:
        for path in args.rescore:
            data = json.loads(Path(path).read_text(encoding="utf-8"))
            rows = data["rows"] if isinstance(data, dict) else data
            score_rows(rows, validator)
            report(rows, str(rows[0].get("arm", Path(path).stem)) if rows else Path(path).stem)
        return 0
    cases = load_cases(Path(args.cases))
    if args.only:
        cases = [c for c in cases if c["id"] == args.only]
    with tempfile.TemporaryDirectory(prefix="openui-eval-") as tmp:
        environment(args.model, args.effort, Path(tmp))
        rows = asyncio.run(run_all(cases, args.jobs))
    for r in rows:
        r["arm"] = args.arm
    return finish(rows, args.arm, validator, Path(args.out) if args.out else None)


def save(path: Path, rows: list[dict[str, Any]], summary: Optional[dict[str, Any]]) -> None:
    """The rows as JSON, written whole to a temp file and then moved into place, so a crash never leaves half a file."""
    tmp = path.with_name(path.name + ".tmp")
    tmp.write_text(json.dumps({"summary": summary, "rows": rows}, indent=1, ensure_ascii=False) + "\n",
                   encoding="utf-8")
    tmp.replace(path)


def finish(rows: list[dict[str, Any]], arm: str, validator: Optional[Path], out: Optional[Path]) -> int:
    """Save the raw replies, then score them. The replies cost model calls and scoring is free to redo, so they are on
    disk before the validator runs (with no --out, in a temp file named here) and a scoring failure keeps them:
    ``--rescore`` scores that file again. The file is written a second time with the scores once they exist."""
    if out is None:
        fd, name = tempfile.mkstemp(prefix=f"openui-eval-{arm}-", suffix=".json")
        os.close(fd)
        out = Path(name)
    save(out, rows, None)
    print(f"replies saved: {out}", flush=True)
    try:
        score_rows(rows, validator)
    except (Exception, SystemExit) as e:  # noqa: BLE001 — the replies are already saved; say how to score them again
        print(f"scoring failed ({type(e).__name__}: {str(e)[:300]}); the replies are in {out}; "
              f"score them with: --rescore {out}", file=sys.stderr, flush=True)
        return 1
    summary = report(rows, arm)
    save(out, rows, summary)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
