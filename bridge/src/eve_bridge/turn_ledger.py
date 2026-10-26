"""How long every turn takes, and which tool, model or door costs the seconds.

Owner's rule for the speed work: rank it by wall time saved. Before this, the only per-turn timing was one prose log
line ("turn answered in X s"), model calls carried no latency, and nothing joined a turn's spend rows to its tool
calls. This module is that join: one small JSON line per event in a per-day file under
``~/.config/eve-bridge/turns/`` (``EVE_TURNS_DIR`` moves it), named by the local date:

    {"t": 1790000000.123, "ev": "turn.started", "trace": "4bf92f3577b34da6", "door": "app"}
    {"t": 1790000000.412, "ev": "model", "trace": "4bf9…", "door": "app", "model": "gpt-6-luna", "ms": 289,
     "in": 3100, "cached": 2900, "out": 80, "usd": 0.00042}
    {"t": 1790000000.700, "ev": "call", "trace": "4bf9…", "door": "app", "tool": "web_search", "ok": true,
     "ms": 280, "gated": false}
    {"t": 1790000001.020, "ev": "turn.settled", "trace": "4bf9…", "door": "app", "outcome": "answered",
     "rounds": 2, "total_ms": 897}

* ``turn(door)`` opens a turn: a fresh trace id in a ContextVar, so every row written inside it (on this task, on a
  task it spawns, on a thread it hands off to with the context) carries it, and spend.record stamps it on the spend
  row too. Two turns at once get two ids. Use it as ``with`` or ``async with``.
* ``call``, ``gate``, ``model`` and ``link`` write one row each. ``link`` joins another system's id to the turn: a
  Mac task's run log (computer_agent.py) or Codex's own turn id (codex_computer.py).
* **Names and numbers only** (AGENTS.md rule 9). Every event has an allowlist of keys; anything else is dropped
  before the line is built, and every string is checked against a name pattern (a tool, a model, a door), so no
  message text, tool argument, tool result, recipient or token can reach the file, whatever a caller passes.
* **Never raises.** A failed write is one log line (the type only), never a failed turn.
* ``EVE_TURN_LEDGER=0`` turns it off: no file, no trace id, and spend rows exactly as they were.

The reader (``rows``, ``stats``, ``trace_rows``) is what ``eve-bridge turns`` and ``eve-bridge trace <id>`` print. See
docs/turn-ledger.md.
"""

from __future__ import annotations

import asyncio
import contextvars
import json
import logging
import math
import os
import re
import secrets
import threading
import time
from dataclasses import dataclass, field
from datetime import date, datetime, timedelta
from pathlib import Path
from typing import Any, Iterable, Optional

log = logging.getLogger(__name__)

ENV = "EVE_TURN_LEDGER"
DIR_ENV = "EVE_TURNS_DIR"
DEFAULT_DIR = "~/.config/eve-bridge/turns"

# The doors a turn arrives through; anything else is written as "other".
DOORS = frozenset({"telegram", "app", "call", "voice", "work", "other"})
OUTCOMES = frozenset({"answered", "silent", "small_talk", "failed", "cancelled", "error", "open"})
LINKS = frozenset({"agent_run", "codex"})

# A name: a tool, a model id, a trace id, a linked id. Short, no spaces: a sentence can never pass.
_NAME = re.compile(r"^[A-Za-z0-9_.:/@+-]{1,80}$")
_TRACE_ID = re.compile(r"^[0-9a-f]{16}$")

# Per event: the keys a row may carry, and the kind of each. Every other key is dropped.
_STR, _INT, _NUM, _BOOL, _DOOR, _OUTCOME, _LINK, _TRACE = range(8)
_COMMON = {"trace": _TRACE, "door": _DOOR}
ALLOWED: dict[str, dict[str, int]] = {
    "turn.started": {**_COMMON, "parent": _TRACE, "queued_ms": _INT},
    "turn.settled": {**_COMMON, "outcome": _OUTCOME, "rounds": _INT, "total_ms": _INT},
    "call": {**_COMMON, "tool": _STR, "ok": _BOOL, "ms": _INT, "gated": _BOOL},
    "gate": {**_COMMON, "tool": _STR, "allowed": _BOOL, "ms": _INT},
    "model": {**_COMMON, "model": _STR, "ms": _INT, "in": _INT, "cached": _INT, "out": _INT, "usd": _NUM},
    "link": {**_COMMON, "kind": _LINK, "ref": _STR},
}


@dataclass
class _State:
    trace: str
    door: str


_TRACE: contextvars.ContextVar[Optional[_State]] = contextvars.ContextVar("turn_ledger_trace", default=None)
# Set around a call the taint gate asked about, so the call's row says so (``gated``).
_GATED: contextvars.ContextVar[bool] = contextvars.ContextVar("turn_ledger_gated", default=False)

_lock = threading.Lock()
_dir_override: Optional[Path] = None
_warned: set[str] = set()


def enabled() -> bool:
    return (os.environ.get(ENV) or "").strip() != "0"


def set_dir(path: Optional[Path]) -> None:
    """Point the ledger somewhere else (tests), or back to the default with None."""
    global _dir_override
    _dir_override = Path(path) if path is not None else None


def turns_dir() -> Path:
    if _dir_override is not None:
        return _dir_override
    raw = (os.environ.get(DIR_ENV) or "").strip()
    return Path(raw or DEFAULT_DIR).expanduser()


def current_trace() -> str:
    """The trace id of the turn running now, or "" (no turn, or the ledger off)."""
    state = _TRACE.get()
    return state.trace if state is not None else ""


def _once(key: str, msg: str, *args: Any) -> None:
    if key not in _warned:
        _warned.add(key)
        log.warning(msg, *args)


def _clean(kind: int, value: Any) -> Any:
    """The value as the ledger may write it, or None (dropped)."""
    if value is None:
        return None
    if kind == _BOOL:
        return value if isinstance(value, bool) else None
    if kind in (_INT, _NUM):
        if isinstance(value, bool) or not isinstance(value, (int, float)) or value != value or value < 0:
            return None
        if math.isinf(value):
            return None
        return int(round(value)) if kind == _INT else round(float(value), 8)
    if not isinstance(value, str):
        return None
    if kind == _TRACE:
        return value if _TRACE_ID.match(value) else None
    if kind == _DOOR:
        return value if value in DOORS else "other"
    if kind == _OUTCOME:
        return value if value in OUTCOMES else None
    if kind == _LINK:
        return value if value in LINKS else None
    return value if _NAME.match(value) else "?"


def _row(event: str, fields: dict[str, Any], now: Optional[float] = None) -> Optional[dict[str, Any]]:
    """One event's row: its allowlisted keys, each cleaned; None for an event the ledger does not know."""
    allowed = ALLOWED.get(event)
    if allowed is None:
        return None
    row: dict[str, Any] = {"t": round(time.time() if now is None else float(now), 3), "ev": event}
    for key, kind in allowed.items():
        value = _clean(kind, fields.get(key))
        if value is not None:
            row[key] = value
    return row


def _append(row: dict[str, Any]) -> None:
    """One line into the row's local day. The only writer; it trusts its caller, so it is only fed by ``_emit``."""
    line = json.dumps(row, separators=(",", ":")) + "\n"
    folder = turns_dir()
    day = datetime.fromtimestamp(row["t"]).date().isoformat()
    with _lock:
        folder.mkdir(parents=True, exist_ok=True, mode=0o700)
        with open(folder / f"{day}.jsonl", "a", encoding="utf-8") as f:
            f.write(line)


def _emit(event: str, **fields: Any) -> None:
    """Write one event. Off, or a write that fails: nothing, and the caller never knows. Never raises."""
    try:
        if not enabled():
            return
        state = _TRACE.get()
        if state is not None:
            fields.setdefault("trace", state.trace)
            fields.setdefault("door", state.door)
        else:
            fields.setdefault("door", "other")
        row = _row(event, fields)
        if row is not None:
            _append(row)
    except Exception as e:  # noqa: BLE001 — the ledger never breaks the turn it measures
        _once("write", "turn ledger: a line was not written (%s)", type(e).__name__)


# ---- the writer's API --------------------------------------------------------------------------------------

class Turn:
    """One turn, opened by ``turn(door)``. Set ``outcome`` and ``rounds`` inside; the settled row is written on
    the way out. An exception leaving the block is the turn's ``error`` (``cancelled`` for a cancel), and is never
    swallowed."""

    def __init__(self, door: str, queued_ms: Optional[float] = None) -> None:
        self.door = door if door in DOORS else "other"
        self.queued_ms = queued_ms
        self.outcome = "answered"
        self.rounds = 0
        self.trace = ""
        self._t0 = 0.0
        self._token: Optional[contextvars.Token[Optional[_State]]] = None

    def __enter__(self) -> "Turn":
        try:
            self._t0 = time.perf_counter()
            if not enabled():
                return self
            parent = current_trace()
            self.trace = secrets.token_hex(8)
            self._token = _TRACE.set(_State(self.trace, self.door))
            _emit("turn.started", parent=parent or None, queued_ms=self.queued_ms)
        except Exception as e:  # noqa: BLE001
            _once("enter", "turn ledger: a turn was not opened (%s)", type(e).__name__)
        return self

    def __exit__(self, exc_type: Any, exc: Any, tb: Any) -> bool:
        try:
            if self._token is not None:
                outcome = self.outcome
                if exc_type is not None:
                    outcome = "cancelled" if issubclass(exc_type, asyncio.CancelledError) else "error"
                _emit("turn.settled", outcome=outcome, rounds=self.rounds,
                      total_ms=(time.perf_counter() - self._t0) * 1000)
                try:
                    _TRACE.reset(self._token)
                except ValueError:                 # left in another context: drop this turn's id all the same
                    _TRACE.set(None)
                self._token = None
        except Exception as e:  # noqa: BLE001
            _once("exit", "turn ledger: a turn was not settled (%s)", type(e).__name__)
        return False                               # never swallow the turn's own exception

    async def __aenter__(self) -> "Turn":
        return self.__enter__()

    async def __aexit__(self, exc_type: Any, exc: Any, tb: Any) -> bool:
        return self.__exit__(exc_type, exc, tb)


def turn(door: str, queued_ms: Optional[float] = None) -> Turn:
    """``with turn("app") as t: ... t.rounds = 2``: one turn's started and settled rows, and its trace id for every
    row written inside. Also ``async with``, so it can share a line with an ``async with`` lock."""
    return Turn(door, queued_ms)


class _Gated:
    def __init__(self) -> None:
        self._token: Optional[contextvars.Token[bool]] = None

    def __enter__(self) -> "_Gated":
        try:
            self._token = _GATED.set(True)
        except Exception:  # noqa: BLE001
            self._token = None
        return self

    def __exit__(self, *exc: Any) -> bool:
        try:
            if self._token is not None:
                _GATED.reset(self._token)
        except Exception:  # noqa: BLE001
            pass
        return False


def gated() -> _Gated:
    """Around a call the taint gate asked about: its ``call`` row says ``gated: true``."""
    return _Gated()


def call(tool: str, ok: bool, ms: float, gated: Optional[bool] = None) -> None:
    """One tool call: its name, whether it worked, its milliseconds. Never its arguments or its result."""
    _emit("call", tool=tool, ok=bool(ok), ms=ms, gated=_GATED.get() if gated is None else bool(gated))


def gate(tool: str, allowed: bool, ms: float) -> None:
    """One taint-gate question: the tool it asked about, the owner's answer, how long the asking took."""
    _emit("gate", tool=tool, allowed=bool(allowed), ms=ms)


def model(name: str, ms: float, tokens_in: int = 0, cached: int = 0, out: int = 0,
          usd: Optional[float] = None) -> None:
    """One model call: the model, its milliseconds, its token counts and dollars (None: not priced)."""
    _emit("model", model=name, ms=ms, **{"in": tokens_in}, cached=cached, out=out, usd=usd)


def link(kind: str, ref: str) -> None:
    """Another system's id for this turn's work: ``agent_run`` (a run log's name) or ``codex`` (Codex's turn id).
    Outside a turn there is nothing to join it to, so nothing is written."""
    if current_trace():
        _emit("link", kind=kind, ref=ref)


def ok_of(result: Any) -> bool:
    """A tool result's success, as the round loop reads it: a dict that does not say ``ok: false``."""
    return not (isinstance(result, dict) and result.get("ok", True) is False)


# ---- the reader --------------------------------------------------------------------------------------------

def day_rows(day: str, folder: Optional[Path] = None) -> list[dict[str, Any]]:
    """Every line of one local day. A torn or foreign line is skipped."""
    path = (folder or turns_dir()) / f"{day}.jsonl"
    out: list[dict[str, Any]] = []
    try:
        with open(path, encoding="utf-8") as f:
            for line in f:
                try:
                    row = json.loads(line)
                except ValueError:
                    continue
                if isinstance(row, dict) and isinstance(row.get("ev"), str):
                    out.append(row)
    except OSError:
        return []
    return out


def parse_day(text: str, today: Optional[date] = None) -> str:
    """"today", "yesterday" or YYYY-MM-DD, as an ISO day. ValueError for anything else."""
    today = today or date.today()
    word = (text or "today").strip().lower()
    if word == "today":
        return today.isoformat()
    if word == "yesterday":
        return (today - timedelta(days=1)).isoformat()
    return date.fromisoformat(word).isoformat()


def percentile(values: list[float], p: float) -> float:
    """Nearest rank: the smallest value with at least p% of the values at or below it. 0 for none."""
    if not values:
        return 0.0
    ordered = sorted(values)
    rank = max(1, math.ceil(p / 100 * len(ordered)))
    return float(ordered[rank - 1])


@dataclass
class Line:
    name: str
    count: int = 0
    failed: int = 0
    usd: float = 0.0
    ms: list[float] = field(default_factory=list)

    @property
    def p50(self) -> float:
        return percentile(self.ms, 50)

    @property
    def p95(self) -> float:
        return percentile(self.ms, 95)


def _num(v: Any) -> Optional[float]:
    return float(v) if isinstance(v, (int, float)) and not isinstance(v, bool) else None


def stats(rows: Iterable[dict[str, Any]], tool: str = "") -> dict[str, list[Line]]:
    """{"tool", "model", "door"}: each a list of lines (count, p50 and p95 milliseconds), slowest p95 first. Doors
    are timed by their settled turns. ``tool``: only that tool's calls, and only the turns that made one (their
    models and doors)."""
    rows = list(rows)
    if tool:
        keep = {r.get("trace") for r in rows if r.get("ev") == "call" and r.get("tool") == tool and r.get("trace")}
        rows = [r for r in rows if (r.get("ev") == "call" and r.get("tool") == tool)
                or (r.get("ev") != "call" and r.get("trace") in keep)]
    groups: dict[str, dict[str, Line]] = {"tool": {}, "model": {}, "door": {}}
    for r in rows:
        ev = r.get("ev")
        if ev == "call":
            kind, name, ms = "tool", str(r.get("tool") or "?"), _num(r.get("ms"))
        elif ev == "model":
            kind, name, ms = "model", str(r.get("model") or "?"), _num(r.get("ms"))
        elif ev == "turn.settled":
            kind, name, ms = "door", str(r.get("door") or "?"), _num(r.get("total_ms"))
        else:
            continue
        line = groups[kind].setdefault(name, Line(name))
        line.count += 1
        if ms is not None:
            line.ms.append(ms)
        if (ev == "call" and r.get("ok") is False) or (ev == "turn.settled" and r.get("outcome") in
                                                         ("failed", "error")):
            line.failed += 1
        usd = _num(r.get("usd"))
        if ev == "model" and usd is not None:
            line.usd += usd
    return {k: sorted(v.values(), key=lambda line: (-line.p95, line.name)) for k, v in groups.items()}


def render(table: dict[str, list[Line]], day: str, tool: str = "") -> str:
    """``eve-bridge turns``: three plain tables."""
    head = f"Turn ledger, {day}" + (f", tool {tool}" if tool else "")
    out = [head]
    titles = {"door": "Doors (whole turns)", "tool": "Tools", "model": "Models"}
    for kind in ("door", "tool", "model"):
        lines = table.get(kind) or []
        out.append("")
        out.append(titles[kind])
        if not lines:
            out.append("  (none)")
            continue
        width = max(12, *(len(line.name) for line in lines))
        extra = "      usd" if kind == "model" else "  failed" if kind in ("tool", "door") else ""
        out.append(f"  {'name':<{width}}  {'count':>5}  {'p50 ms':>8}  {'p95 ms':>8}{extra}")
        for line in lines:
            tail = (f"  {line.usd:>7.4f}" if kind == "model"
                    else f"  {line.failed:>6}")
            out.append(f"  {line.name:<{width}}  {line.count:>5}  {line.p50:>8.0f}  {line.p95:>8.0f}{tail}")
    return "\n".join(out)


def trace_rows(trace: str, days: int = 14, folder: Optional[Path] = None,
               today: Optional[date] = None) -> tuple[str, list[dict[str, Any]], list[str]]:
    """(the full trace id, its rows in time order, the days they are in), from the last ``days`` days. ``trace``
    may be a prefix of 6 characters or more when it names one trace only; ("", [], []) when none or several match."""
    trace = (trace or "").strip().lower()
    if len(trace) < 6:
        return "", [], []
    today = today or date.today()
    found: dict[str, list[dict[str, Any]]] = {}
    seen_days: dict[str, set[str]] = {}
    for i in range(max(1, days)):
        day = (today - timedelta(days=i)).isoformat()
        for r in day_rows(day, folder):
            tid = r.get("trace")
            if isinstance(tid, str) and tid.startswith(trace):
                found.setdefault(tid, []).append(r)
                seen_days.setdefault(tid, set()).add(day)
    if trace in found:
        tid = trace
    elif len(found) == 1:
        tid = next(iter(found))
    else:
        return "", [], []
    rows = sorted(found[tid], key=lambda r: _num(r.get("t")) or 0.0)
    return tid, rows, sorted(seen_days[tid])


def render_trace(tid: str, rows: list[dict[str, Any]], spend_rows: list[dict[str, Any]]) -> str:
    """``eve-bridge trace <id>``: the turn's rows in order, each at its offset from the first, then its spend rows."""
    t0 = (_num(rows[0].get("t")) or 0.0) if rows else 0.0
    out = [f"Trace {tid}"]
    for r in rows:
        at = ((_num(r.get("t")) or t0) - t0) * 1000
        ev = str(r.get("ev"))
        if ev == "turn.started":
            what = f"door={r.get('door')}" + (f" parent={r['parent']}" if r.get("parent") else "") + (
                f" queued={r['queued_ms']} ms" if r.get("queued_ms") is not None else "")
        elif ev == "turn.settled":
            what = f"outcome={r.get('outcome')} rounds={r.get('rounds')} total={r.get('total_ms')} ms"
        elif ev == "call":
            what = f"{r.get('tool')} ok={r.get('ok')} {r.get('ms')} ms" + (" gated" if r.get("gated") else "")
        elif ev == "gate":
            what = f"{r.get('tool')} allowed={r.get('allowed')} {r.get('ms')} ms"
        elif ev == "model":
            usd = r.get("usd")
            what = (f"{r.get('model')} {r.get('ms')} ms in={r.get('in', 0)} cached={r.get('cached', 0)} "
                    f"out={r.get('out', 0)} usd={'?' if usd is None else usd}")
        elif ev == "link":
            what = f"{r.get('kind')} {r.get('ref')}"
        else:
            what = ""
        out.append(f"  +{at:>8.0f} ms  {ev:<13} {what}")
    out.append("")
    if spend_rows:
        total = sum(_num(r.get("usd")) or 0.0 for r in spend_rows)
        out.append(f"Spend rows ({len(spend_rows)}, ${total:.5f})")
        for r in spend_rows:
            usd = r.get("usd")
            out.append(f"  {r.get('p')}/{r.get('m')} {r.get('f')} usd={'?' if usd is None else usd}")
    else:
        out.append("Spend rows: none")
    return "\n".join(out)


def cli(argv: list[str]) -> int:
    """``eve-bridge turns`` and ``eve-bridge trace``: read the JSONL offline (the daemon need not run)."""
    import argparse

    parser = argparse.ArgumentParser(prog="eve-bridge", description="The turn ledger (docs/turn-ledger.md).")
    sub = parser.add_subparsers(dest="cmd")
    p_turns = sub.add_parser("turns", help="p50, p95 and count per tool, model and door for one day",
                             description="Read ~/.config/eve-bridge/turns/<day>.jsonl and print, per tool, model "
                                         "and door, how many there were and their p50 and p95 milliseconds.")
    p_turns.add_argument("--day", default="today", help="today (default), yesterday, or YYYY-MM-DD")
    p_turns.add_argument("--tool", default="", help="only this tool's calls, and the turns that made one")
    p_turns.add_argument("--dir", default=None, help="read another ledger folder (default: EVE_TURNS_DIR or "
                                                     "~/.config/eve-bridge/turns)")
    p_trace = sub.add_parser("trace", help="one turn's rows and its spend rows, by trace id",
                             description="Print every ledger row of one turn, in order, and the spend rows that "
                                         "carry the same trace id.")
    p_trace.add_argument("id", help="the trace id (16 hex characters; a unique prefix of 6 or more will do)")
    p_trace.add_argument("--days", type=int, default=14, help="how many days back to look (default 14)")
    p_trace.add_argument("--dir", default=None, help="read another ledger folder")
    args = parser.parse_args(argv)
    folder = Path(args.dir).expanduser() if getattr(args, "dir", None) else None
    if args.cmd == "turns":
        try:
            day = parse_day(args.day)
        except ValueError:
            print(f"eve-bridge turns: not a day: {args.day!r} (today, yesterday or YYYY-MM-DD)")
            return 2
        rows = day_rows(day, folder)
        if not rows:
            print(f"No turn ledger rows for {day} in {folder or turns_dir()}.")
            return 0
        print(render(stats(rows, args.tool), day, args.tool))
        return 0
    if args.cmd == "trace":
        tid, rows, days = trace_rows(args.id, args.days, folder)
        if not tid:
            print(f"No single trace matches {args.id!r} in the last {args.days} days.")
            return 1
        from . import spend

        spend_rows = [r for d in days for r in spend.day_rows(d) if r.get("trace") == tid]
        print(render_trace(tid, rows, spend_rows))
        return 0
    parser.print_help()
    return 1
