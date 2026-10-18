"""Monid: a paid catalogue of data endpoints, used as the web search's last resort.

Owner, 2026-10-08: "wire monid into eve as a fallback". Monid (monid.ai) sells many data endpoints behind one
CLI. eve uses one of them: DataForSEO's live Google results (``dataforseo`` ``/serp/google-organic``, $0.002 a
call of up to 10 results, p50 5.8 s and p95 8.7 s per ``monid inspect`` on 2026-10-08). websearch.engine_search
calls ``web_search`` only when BOTH OpenRouter engines failed, and never after a 4xx other than 429.

The CLI holds the key, never eve. eve runs ``monid`` (the npm package ``@monid-ai/cli``) the way firetv.py runs
adb: an argv list (never a shell string), ``NO_COLOR=1``, ``-j`` for JSON, a hard timeout, and a PATH that also has
Homebrew's and npm's bin directories (the daemon runs under launchd with a thin PATH, and ``monid`` is a
``#!/usr/bin/env node`` script, so node must be on that PATH too). ``available`` reads ``monid keys list -j`` for
an active key and reads only the ``active`` flag: the key is never read into a log, a result or an exception.

Spend control, counted HERE and nowhere else (like firecrawl.py's daily cap): every run takes one from
``EVE_MONID_PER_DAY`` (default 20 runs a local day) and adds its reported cost to the day's dollars, capped by
``EVE_MONID_USD_PER_DAY`` (default $0.25). A run that would pass either cap is not sent: one log line, and the
caller keeps its own failure. Every run is recorded in spend.py under provider "monid" with the run's reported cost.
``EVE_MONID`` = fallback (default) | 0 (off).

Nothing here raises: every call returns ``{"ok": False, "reason": ...}`` instead. Logs carry the shape only (ok,
result count, ms, cost), never the query and never the key.
"""

from __future__ import annotations

import asyncio
import json
import logging
import os
import re
import shutil
import subprocess
import threading
import time
from datetime import datetime
from typing import Any, Callable, Mapping, Optional

log = logging.getLogger(__name__)

CLI_PATHS = ("/opt/homebrew/bin/monid", "/usr/local/bin/monid")   # launchd's thin PATH misses both
NPM_BIN_DIRS = ("~/.npm-global/bin", "~/.local/bin")                # npm's user-level global prefixes
RC_TIMEOUT = 124                      # the runner's code for "no answer in time" (as timeout(1))
RC_NO_CLI = 127                       # the runner's code for "monid would not start"
KEYS_SECS = 8.0                       # `monid keys list` (node start-up plus a local file read)
BALANCE_SECS = 10.0
KEY_OK_SECS = 600.0                   # a found key is believed for 10 minutes before `keys list` is read again
DEFAULT_MODE = "fallback"
DEFAULT_PER_DAY = 20                  # runs a local day
DEFAULT_USD_PER_DAY = 0.25            # dollars a local day
COUNT_FILE = "monid-runs.json"        # beside the spend ledger: {"day": "YYYY-MM-DD", "runs": n, "usd": x}

# The search endpoint, its input shape as `monid inspect -p dataforseo -e /serp/google-organic` gave it on
# 2026-10-08: body {keyword (required, <= 700 chars), location_code | location_name, language_code | language_name,
# depth (1-200, billed per page of 10), ...}, bodyType json, $0.002 a call (PER_CALL, per page of 10).
SEARCH_PROVIDER = "dataforseo"
SEARCH_ENDPOINT = "/serp/google-organic"
SEARCH_USD = 0.002                    # the listed price: counted against the day when a run reports no cost
SEARCH_DEPTH = 10                     # one page of 10: one $0.002 call
LOCATION_CODE = 2840                  # United States
LANGUAGE_CODE = "en"
MAX_SEARCH_SECS = 12.0                # Monid's share of the web search's budget, at most
MIN_SEARCH_SECS = 9.0                 # less than this left: not worth paying for a run that cannot finish (p95 8.7 s)
MAX_SOURCES = 8
MAX_SNIPPET_CHARS = 300
MAX_ANSWER_CHARS = 1500

# (argv, timeout, env) -> (rc, stdout, stderr). A timeout is an answer (RC_TIMEOUT), never an exception.
Runner = Callable[[list[str], float, dict[str, str]], tuple[int, str, str]]

_LOCK = threading.Lock()
_USED: dict[str, dict[str, float]] = {}   # local day -> {"runs": n, "usd": x}
_LOADED: set[str] = set()
_KEY_OK_AT: list[float] = []              # monotonic time a key was last seen active (empty: not seen)


# ---- running the CLI ---------------------------------------------------------------------------------------

def find_cli(environ: Optional[Mapping[str, str]] = None) -> str:
    """``EVE_MONID_CLI``, then the PATH, then Homebrew's two places and npm's user prefixes. "" when none."""
    env = os.environ if environ is None else environ
    override = str(env.get("EVE_MONID_CLI", "") or "").strip()
    if override:
        return override
    found = shutil.which("monid")
    if found:
        return found
    places = list(CLI_PATHS) + [os.path.join(os.path.expanduser(d), "monid") for d in NPM_BIN_DIRS]
    prefix = str(env.get("NPM_CONFIG_PREFIX", "") or "").strip()
    if prefix:
        places.append(os.path.join(os.path.expanduser(prefix), "bin", "monid"))
    return next((p for p in places if os.access(p, os.X_OK)), "")


ENV_KEEP = ("HOME", "USER", "LOGNAME", "LANG", "LC_ALL", "TMPDIR", "SHELL", "NPM_CONFIG_PREFIX")


def _env(cli: str) -> dict[str, str]:
    """The CLI's environment: only what node and the CLI need (its own key lives in its own config), never eve's
    secrets (review, 2026-10-08: the whole environment, OpenRouter and Telegram keys included, reached a third-party
    npm package). NO_COLOR=1, and a PATH where ``node`` is found (the CLI's shebang)."""
    env = {k: os.environ[k] for k in ENV_KEEP if os.environ.get(k)}
    env["NO_COLOR"] = "1"
    dirs = [os.path.dirname(cli)] if os.path.dirname(cli) else []
    dirs += ["/opt/homebrew/bin", "/usr/local/bin"]
    # The daemon's own PATH (nvm's node lives there), not env's: env was just built without one. /usr/bin and
    # /bin always, for the shebang's /usr/bin/env.
    path = [d for d in (os.environ.get("PATH") or "").split(os.pathsep) if d]
    env["PATH"] = os.pathsep.join(dict.fromkeys(dirs + path + ["/usr/bin", "/bin"]))
    return env


def subprocess_runner(argv: list[str], timeout: float, env: dict[str, str]) -> tuple[int, str, str]:
    """The real runner: one CLI process, an argv list (never a shell string), a hard timeout (the child is killed)."""
    try:
        done = subprocess.run(argv, stdin=subprocess.DEVNULL, capture_output=True, timeout=timeout, env=env,
                              check=False)
    except subprocess.TimeoutExpired:
        return RC_TIMEOUT, "", ""
    except OSError as e:
        return RC_NO_CLI, "", f"monid could not start ({e.strerror or type(e).__name__})"
    return done.returncode, (done.stdout or b"").decode("utf-8", "replace"), (done.stderr or b"").decode(
        "utf-8", "replace")


RUNNER: Runner = subprocess_runner       # tests swap this for a fake; nothing else does


def _json(text: str) -> Any:
    """The CLI's JSON, past any line it printed before it (an update notice). Raises ValueError."""
    text = (text or "").strip()
    starts = [i for i in (text.find("{"), text.find("[")) if i >= 0]
    if not starts:
        raise ValueError("no JSON in the output")
    return json.loads(text[min(starts):])


def _call(args: list[str], timeout: float, environ: Optional[Mapping[str, str]],
          runner: Optional[Runner]) -> tuple[Optional[Any], str, bool]:
    """(parsed JSON, "", True) or (None, reason, started): ``started`` is False when no process ran (nothing can
    have been spent). The reason never quotes the CLI's output (it could hold anything)."""
    cli = find_cli(environ)
    if not cli:
        return None, "the monid CLI is not installed", False
    rc, out, _err = (runner or RUNNER)([cli, *args, "-j"], timeout, _env(cli))
    if rc == RC_TIMEOUT:
        return None, f"monid did not answer in {timeout:.0f} s", True
    if rc == RC_NO_CLI:
        return None, "the monid CLI would not start", False
    try:
        data = _json(out)
    except (ValueError, RecursionError):
        return None, (f"monid exited {rc} without JSON" if rc else "monid sent unreadable JSON"), True
    if rc != 0:
        return None, f"monid exited {rc}", True
    return data, "", True


# ---- switches and the daily cap ------------------------------------------------------------------------------

def mode(environ: Optional[Mapping[str, str]] = None) -> str:
    """EVE_MONID: "fallback" (default) or "off" (0, off, false, no)."""
    env = os.environ if environ is None else environ
    raw = str(env.get("EVE_MONID", "") or "").strip().lower()
    return "off" if raw in ("0", "off", "false", "no") else DEFAULT_MODE


def per_day(environ: Optional[Mapping[str, str]] = None) -> int:
    """EVE_MONID_PER_DAY: Monid runs a local day (default 20; 0 stops them all)."""
    env = os.environ if environ is None else environ
    raw = str(env.get("EVE_MONID_PER_DAY", "") or "").strip()
    try:
        return max(0, min(10000, int(float(raw)))) if raw else DEFAULT_PER_DAY
    except (ValueError, OverflowError):
        log.warning("monid: EVE_MONID_PER_DAY is not a number; %d a day", DEFAULT_PER_DAY)
        return DEFAULT_PER_DAY


def usd_per_day(environ: Optional[Mapping[str, str]] = None) -> float:
    """EVE_MONID_USD_PER_DAY: dollars of Monid runs a local day (default 0.25; 0 stops them all)."""
    env = os.environ if environ is None else environ
    raw = str(env.get("EVE_MONID_USD_PER_DAY", "") or "").strip()
    try:
        usd = float(raw) if raw else DEFAULT_USD_PER_DAY
    except (ValueError, OverflowError):
        log.warning("monid: EVE_MONID_USD_PER_DAY is not a number; $%.2f a day", DEFAULT_USD_PER_DAY)
        return DEFAULT_USD_PER_DAY
    return max(0.0, min(100.0, usd)) if usd == usd else DEFAULT_USD_PER_DAY


def _day(now: Optional[float]) -> str:
    return datetime.fromtimestamp(time.time() if now is None else now).strftime("%Y-%m-%d")


def _count_path() -> Any:
    from . import spend

    return spend.spend_dir() / COUNT_FILE


def _load(day: str) -> None:
    """Once a process per day: the day's use saved by an earlier process (a restart must not hand out a fresh cap).
    Under _LOCK. An unreadable file counts as nothing saved."""
    if day in _LOADED:
        return
    _LOADED.add(day)
    try:
        saved = json.loads(_count_path().read_text())
        if isinstance(saved, dict) and saved.get("day") == day:
            used = _USED.setdefault(day, {"runs": 0, "usd": 0.0})
            used["runs"] = max(used["runs"], int(saved.get("runs") or 0))
            used["usd"] = max(used["usd"], float(saved.get("usd") or 0.0))
    except (OSError, ValueError, TypeError):
        pass


def _save(day: str) -> None:
    """Under _LOCK. Best effort: a failed write only loses the count across a restart."""
    try:
        path = _count_path()
        path.parent.mkdir(parents=True, exist_ok=True)
        tmp = path.with_suffix(".tmp")
        used = _USED.get(day, {"runs": 0, "usd": 0.0})
        tmp.write_text(json.dumps({"day": day, "runs": int(used["runs"]), "usd": round(used["usd"], 6)}))
        tmp.replace(path)
    except OSError as e:
        log.warning("monid: could not save today's run count (%s)", type(e).__name__)


def used_today(now: Optional[float] = None) -> dict[str, float]:
    """{"runs": n, "usd": x} spent today."""
    day = _day(now)
    with _LOCK:
        _load(day)
        return dict(_USED.get(day, {"runs": 0, "usd": 0.0}))


def _take(estimate: float, environ: Optional[Mapping[str, str]], now: Optional[float]) -> str:
    """Count one run against today's caps before it is sent: "" when it may go, else which cap is spent."""
    day = _day(now)
    with _LOCK:
        _load(day)
        used = _USED.setdefault(day, {"runs": 0, "usd": 0.0})
        if used["runs"] >= per_day(environ):
            return f"today's {per_day(environ)} Monid runs are used up (EVE_MONID_PER_DAY)"
        if used["usd"] + estimate > usd_per_day(environ) + 1e-9:
            return f"today's ${usd_per_day(environ):.2f} of Monid runs is used up (EVE_MONID_USD_PER_DAY)"
        used["runs"] += 1
        used["usd"] += estimate
        for old in sorted(_USED)[:-7]:
            _USED.pop(old, None)
        _save(day)
        return ""


def _settle(estimate: float, cost: Optional[float], now: Optional[float]) -> None:
    """Swap a run's estimate for its reported cost in today's dollars (a run with no reported cost keeps it)."""
    if cost is None:
        return
    day = _day(now)
    with _LOCK:
        _load(day)
        used = _USED.setdefault(day, {"runs": 0, "usd": 0.0})
        used["usd"] = max(0.0, used["usd"] - estimate + cost)
        _save(day)


def reset_for_tests() -> None:
    with _LOCK:
        _USED.clear()
        _LOADED.clear()
        _KEY_OK_AT.clear()


# ---- the three calls ---------------------------------------------------------------------------------------

def available(environ: Optional[Mapping[str, str]] = None, runner: Optional[Runner] = None,
              timeout: float = KEYS_SECS) -> bool:
    """The CLI is found and holds an active key (``monid keys list -j``; only the ``active`` flag is read). The
    check waits at most ``timeout`` (a search passes what is left of its budget)."""
    if _KEY_OK_AT and time.monotonic() - _KEY_OK_AT[0] < KEY_OK_SECS and find_cli(environ):
        return True
    data, reason, _ = _call(["keys", "list"], max(1.0, min(KEYS_SECS, timeout)), environ, runner)
    if data is None:
        log.info("monid: not available (%s)", reason)
        return False
    items = data.get("items") if isinstance(data, dict) else data
    ok = isinstance(items, list) and any(isinstance(k, dict) and k.get("active") is True for k in items)
    if ok:
        _KEY_OK_AT[:] = [time.monotonic()]
    else:
        log.info("monid: not available (no active key; `monid keys add` sets one)")
    return ok


UNIT_USD = {"USD": 1.0, "DOLLAR": 1.0, "CENT": 0.01, "MILLI_DOLLAR": 1e-3, "MICRO_DOLLAR": 1e-6}


def _usd(value: Any) -> Optional[float]:
    """A money object ({"value", "unit"?}) or a bare number, in dollars; None when it is not one."""
    unit = 1.0
    if isinstance(value, dict):
        # no unit is no price: the CLI reports micro-dollars, and reading a bare 2000 as dollars would record a
        # $0.002 run as $2000 (review, 2026-10-08)
        unit = UNIT_USD.get(str(value.get("unit") or value.get("currency") or "").upper(), -1.0)
        value = value.get("value")
    if unit < 0 or isinstance(value, bool) or not isinstance(value, (int, float)) or value != value or value < 0:
        return None
    return float(value) * unit


def _cost(data: Any) -> Optional[float]:
    """A run's reported cost in dollars. The 0.1.7 CLI reports ``billing.reportedCost`` as {"value": 2000,
    "unit": "MICRO_DOLLAR"} (a $0.002 run, 2026-10-08); the skill documents ``cost.value``. Neither: None."""
    if not isinstance(data, dict):
        return None
    billing = data.get("billing")
    if isinstance(billing, dict) and billing.get("reportedCost") is not None:
        return _usd(billing["reportedCost"])
    return _usd(data.get("cost"))


def run(provider: str, endpoint: str, body: dict[str, Any], wait_secs: float, *, estimate_usd: float = 0.0,
        feature: str = "", environ: Optional[Mapping[str, str]] = None, runner: Optional[Runner] = None,
        now: Optional[float] = None) -> dict[str, Any]:
    """One Monid run, waited for up to ``wait_secs`` (the process is killed a little after). Never raises.
    {"ok": True, "output", "cost_usd", "status", "ms"} or {"ok": False, "reason"} (``capped`` when a daily cap
    refused it). Takes a run from today's caps before it is sent and records it in spend.py after."""
    from . import spend

    if mode(environ) == "off":
        return {"ok": False, "reason": "Monid is switched off (EVE_MONID=0)"}
    if not find_cli(environ):
        return {"ok": False, "reason": "the monid CLI is not installed"}
    wait = max(1, int(wait_secs))
    refused = _take(estimate_usd, environ, now)
    if refused:
        log.info("monid: %s; not sent", refused)
        return {"ok": False, "reason": refused, "capped": True}
    t0 = time.monotonic()
    data, reason, started = _call(["run", "-p", provider, "-e", endpoint, "-i", json.dumps(body), "-w", str(wait)],
                         wait + 3.0, environ, runner)
    ms = round((time.monotonic() - t0) * 1000.0)
    cost = _cost(data)
    _settle(estimate_usd, 0.0 if not started else cost, now)   # nothing ran: nothing spent
    what = f"{provider}{endpoint}"
    status = str(data.get("status") or "") if isinstance(data, dict) else ""
    if started:
        spend.record("monid", what, feature or spend.SEARCH, cost, source="reported" if cost is not None else "priced",
                     note="" if cost is not None else f"no reported cost ({status or 'no reply'}); "
                                                      f"~${estimate_usd} counted against the day")
    if data is None:
        log.warning("monid: run failed in %d ms (%s)", ms, reason)
        return {"ok": False, "reason": reason, "ms": ms}
    if status != "COMPLETED":
        log.warning("monid: run ended %s in %d ms", status or "without a status", ms)
        return {"ok": False, "reason": f"the Monid run ended {status or 'without a status'}", "ms": ms,
                "status": status}
    return {"ok": True, "output": data.get("output"), "cost_usd": cost, "status": status, "ms": ms}


def balance(environ: Optional[Mapping[str, str]] = None, runner: Optional[Runner] = None) -> dict[str, Any]:
    """{"ok": True, "usd": x, "held_usd": y} from ``monid balance -j``, or {"ok": False, "reason"}."""
    data, reason, _ = _call(["balance"], BALANCE_SECS, environ, runner)
    if data is None:
        return {"ok": False, "reason": reason}
    usd = _usd(data.get("balance")) if isinstance(data, dict) else None
    held = _usd(data.get("held")) if isinstance(data, dict) else None
    if usd is None:
        return {"ok": False, "reason": "monid sent no balance"}
    return {"ok": True, "usd": usd, "held_usd": held or 0.0}


async def arun(*args: Any, **kwargs: Any) -> dict[str, Any]:
    """``run`` off the event loop."""
    return await asyncio.to_thread(run, *args, **kwargs)


async def abalance(*args: Any, **kwargs: Any) -> dict[str, Any]:
    """``balance`` off the event loop."""
    return await asyncio.to_thread(balance, *args, **kwargs)


# ---- the web search fallback ---------------------------------------------------------------------------------

def search_body(query: str) -> dict[str, Any]:
    """One query, one page of 10, United States, English: one $0.002 call."""
    return {"keyword": " ".join((query or "").split())[:700], "location_code": LOCATION_CODE,
            "language_code": LANGUAGE_CODE, "depth": SEARCH_DEPTH}


def _items(output: Any) -> list[dict[str, Any]]:
    """The SERP items, wherever the output nests them (DataForSEO's tasks[].result[].items[], or items at the top)."""
    if isinstance(output, list):
        found: list[dict[str, Any]] = []
        for part in output:
            found += _items(part)
        return found
    if not isinstance(output, dict):
        return []
    if isinstance(output.get("items"), list) and any(isinstance(i, dict) and "type" in i for i in output["items"]):
        return [i for i in output["items"] if isinstance(i, dict)]
    found = []
    for key in ("tasks", "result", "results", "data", "output"):
        if key in output:
            found += _items(output[key])
    return found


def _clean(text: Any, limit: int) -> str:
    return " ".join(str(text or "").split())[:limit]


_CITE = re.compile(r"\[\[\d+\]\]\([^)]*\)|\[\d+\]")
_MARKUP = re.compile(r"[*#_`>]+")


def _overview(markdown: Any) -> str:
    """The first two sentences of the first paragraph of Google's AI overview (a list under it is left out), its
    citation links and markdown taken out."""
    first = next((p for p in str(markdown or "").split("\n") if _clean(_CITE.sub("", p), 10)), "")
    text = _clean(_MARKUP.sub("", _CITE.sub("", first)), 2000)
    text = text.replace(" ,", ",").replace(" .", ".")
    return " ".join(re.split(r"(?<=[.!?])\s+", text)[:2])[:MAX_SNIPPET_CHARS].strip()


def parse_search(output: Any) -> dict[str, Any]:
    """{"answer", "sources"} from a Google results run: the organic results as sources, and a short answer built
    from what Google showed on top (its AI overview, else a featured snippet) and the top organic snippets, two
    lines in all. No model call."""
    items = _items(output)
    sources: list[dict[str, str]] = []
    seen: set[str] = set()
    for it in items:
        if it.get("type") != "organic":
            continue
        url = _clean(it.get("url"), 500)
        if not url or url in seen:
            continue
        seen.add(url)
        sources.append({"title": _clean(it.get("title"), 120), "url": url,
                        "snippet": _clean(it.get("description"), MAX_SNIPPET_CHARS)})
        if len(sources) >= MAX_SOURCES:
            break
    lines: list[str] = []
    for it in items:
        if it.get("type") == "ai_overview" and _overview(it.get("markdown")):
            lines.append(f"{_overview(it.get('markdown'))} (Google AI overview)")
            break
        if it.get("type") == "featured_snippet" and _clean(it.get("description"), MAX_SNIPPET_CHARS):
            lines.append(f"{_clean(it.get('description'), MAX_SNIPPET_CHARS)} ({_clean(it.get('title'), 120)})")
            break
    for s in sources:
        if len(lines) >= 2:
            break
        if s["snippet"]:
            lines.append(f"{s['snippet']} ({s['title']})" if s["title"] else s["snippet"])
    answer = ("From Google results: " + " ".join(lines))[:MAX_ANSWER_CHARS] if lines else ""
    return {"answer": answer, "sources": sources}


def web_search(query: str, budget_secs: float, *, environ: Optional[Mapping[str, str]] = None,
               runner: Optional[Runner] = None, now: Optional[float] = None) -> dict[str, Any]:
    """Never raises (review, 2026-10-08: odd output or settings escaped as OverflowError/RecursionError)."""
    try:
        return _web_search(query, budget_secs, environ=environ, runner=runner, now=now)
    except Exception as e:  # noqa: BLE001 — a last resort that fails returns the search's own failure
        log.warning("monid: search failed (%s)", type(e).__name__)
        return {"ok": False, "reason": "the Monid run failed"}


def _web_search(query: str, budget_secs: float, *, environ: Optional[Mapping[str, str]] = None,
                runner: Optional[Runner] = None, now: Optional[float] = None) -> dict[str, Any]:
    """The web search's last resort: Google results through Monid, in websearch's result shape
    ({"ok", "answer", "sources", "usage", "ms", "cost_usd", "fallback": "monid"}). Waits at most
    min(budget_secs, MAX_SEARCH_SECS). Never raises; {"ok": False, "reason"} when it could not answer."""
    from . import spend

    if mode(environ) == "off":
        return {"ok": False, "reason": "Monid is switched off (EVE_MONID=0)"}
    if not (query or "").strip():
        return {"ok": False, "reason": "empty query"}
    t0 = time.monotonic()
    budget = min(MAX_SEARCH_SECS, budget_secs)
    if budget < MIN_SEARCH_SECS:
        log.info("monid: %.1f s left of the search; not worth a run", max(0.0, budget))
        return {"ok": False, "reason": "no time left for Monid"}
    if not available(environ, runner, timeout=budget - MIN_SEARCH_SECS + 1.0):
        return {"ok": False, "reason": "Monid is not set up on this computer"}
    left = budget - (time.monotonic() - t0)
    if left < MIN_SEARCH_SECS:
        return {"ok": False, "reason": "no time left for Monid"}
    out = run(SEARCH_PROVIDER, SEARCH_ENDPOINT, search_body(query), left - 3.0 if left > 4.0 else 1.0,
              estimate_usd=SEARCH_USD, feature=spend.SEARCH, environ=environ, runner=runner, now=now)
    ms = round((time.monotonic() - t0) * 1000.0)
    if not out.get("ok"):
        return {"ok": False, "reason": out.get("reason") or "the Monid run failed"}
    parsed = parse_search(out.get("output"))
    cost = out.get("cost_usd")
    if not parsed["sources"] and not parsed["answer"]:
        log.warning("monid: search ok in %d ms but no results, $%s", ms, cost)
        return {"ok": False, "reason": "Monid found no results"}
    usage: dict[str, Any] = {"in": 0, "out": 0}
    if cost is not None:
        usage["cost"] = cost
    log.info("monid: search ok, %d results in %d ms, $%s", len(parsed["sources"]), ms,
             f"{cost:.4f}" if cost is not None else "?")
    return {"ok": True, "answer": parsed["answer"], "sources": parsed["sources"], "usage": usage, "ms": ms,
            "cost_usd": round(cost, 4) if cost is not None else None, "fallback": "monid"}
