"""Firecrawl: a paid web search that reads its results, and the Alexandria data tools beside it.

eve's web search routes here (search_router.py) only when a request needs live structured data that a data
service holds: a fare, a price from a data service, an official record. Everything else is cheaper elsewhere:
TinyFish reads pages for free, Perplexity synthesises many sources. Two calls, both ``Authorization: Bearer
$FIRECRAWL_API_KEY``, both through watch.http_request so its guards hold, both raising watch.FetchError:

* Search (``POST /v2/search``) with the request as its query, which also reads the top few result pages as
  markdown in the same call (``scrapeOptions``): 1 credit per 10 results plus about 1 per page read
  (docs.firecrawl.dev, 2026-09-26). Pages up to an hour old may come from Firecrawl's cache (``maxAge``): fast,
  and fresh enough for a lookup. With ``tools``, the same search also asks Firecrawl's Alexandria catalogue for
  data tools that fit (Google Flights and Skyscanner fares, government records, ...), with their full contracts.
* A tool run (``POST /v2/scrape`` with ``alexandria``; firecrawl-cli 1.24.6): one tool, the options the model
  picked. A provider whose terms the owner has not accepted refuses; eve never accepts them (the owner's call).

The model sees the tools beside the pages and may pick ONE with its options instead of answering (web_reader.
answer_from's ``tools``); ``pick_tool`` checks that pick against what the search offered: a listed tool, only its
declared options, every required one present, plain values. Recovered 2026-09-29 from the Firecrawl web reader
(origin/main 523748d, web_reader.py), which TinyFish replaced as the page reader the same day.

Spend: every call is recorded under provider "firecrawl" with its credits. ``EVE_FIRECRAWL_USD`` is one
credit's price on the owner's plan; unset, the line is recorded unpriced rather than guessed.

Daily cap (2026-09-29): ``EVE_FIRECRAWL_PER_DAY`` Firecrawl calls a local day (default 5), counted HERE and
nowhere else, so the brain's searches and the web reader's tasks share it: every call (a search, a data tool run)
takes one in ``_post`` before it is sent, and a call past the cap raises watch.FetchError without reaching
Firecrawl. search_router also reads ``left()`` before routing to Firecrawl, and sends a spent day's request to
the many-source search with the reason logged. The web reader's own 100-a-day cap counts tasks, not paid calls.
"""

from __future__ import annotations

import json
import logging
import os
import re
import threading
import time
from datetime import datetime
from typing import Any, Mapping, Optional

log = logging.getLogger(__name__)

SEARCH_URL = "https://api.firecrawl.dev/v2/search"
TOOL_URL = "https://api.firecrawl.dev/v2/scrape"   # Alexandria tools run through scrape (firecrawl-cli 1.24.6)
HOST = "api.firecrawl.dev"
MAX_TOOLS = 4                        # Alexandria tools shown to the model
TOOL_TIMEOUT_SECS = 45.0
RESULTS = 3                          # pages read per search: ~4-5 credits (a 4th page costs a credit and 2-3 s uncached)
MAX_AGE_MS = 3_600_000               # an hour of Firecrawl's cache is fresh enough for a lookup
SEARCH_TIMEOUT_SECS = 30.0
DEFAULT_PER_DAY = 5                  # Firecrawl calls a local day, the brain's and the web reader's together

_CALLS: dict[str, int] = {}          # local day -> Firecrawl calls sent (a daemon lives for days)
_CALLS_LOCK = threading.Lock()       # both surfaces call from worker threads
_LOADED: set[str] = set()            # days whose count was read back from disk in this process
COUNT_FILE = "firecrawl-calls.json"  # beside the spend ledger: {"day": "YYYY-MM-DD", "calls": n}; a restart keeps the day's count


def api_key(environ: Optional[Mapping[str, str]] = None) -> str:
    env = os.environ if environ is None else environ
    return (env.get("FIRECRAWL_API_KEY") or "").strip()


def usd_per_credit(environ: Optional[Mapping[str, str]] = None) -> Optional[float]:
    """EVE_FIRECRAWL_USD: one Firecrawl credit's price on the owner's plan ($0 free, about $0.004 Hobby,
    $0.005 an extra credit, firecrawl.dev/pricing 2026-09-26). Unset or unreadable: None, recorded unpriced."""
    env = os.environ if environ is None else environ
    raw = (env.get("EVE_FIRECRAWL_USD") or "").strip()
    try:
        usd = float(raw) if raw else None
    except ValueError:
        log.warning("firecrawl: EVE_FIRECRAWL_USD=%r is not a number; Firecrawl calls recorded unpriced", raw)
        return None
    return usd if usd is not None and 0 <= usd <= 1 else None


def per_day(environ: Optional[Mapping[str, str]] = None) -> int:
    """EVE_FIRECRAWL_PER_DAY: Firecrawl calls a local day (default 5; 0 stops them all)."""
    env = os.environ if environ is None else environ
    raw = (env.get("EVE_FIRECRAWL_PER_DAY") or "").strip()
    try:
        return max(0, min(10000, int(float(raw)))) if raw else DEFAULT_PER_DAY
    except ValueError:
        log.warning("firecrawl: EVE_FIRECRAWL_PER_DAY=%r is not a number; %d a day", raw, DEFAULT_PER_DAY)
        return DEFAULT_PER_DAY


def _day(now: Optional[float]) -> str:
    return datetime.fromtimestamp(time.time() if now is None else now).strftime("%Y-%m-%d")


def _count_path() -> Any:
    from . import spend

    return spend.spend_dir() / COUNT_FILE


def _load(day: str) -> None:
    """Once a process per day: take the day's count saved by an earlier process (a restart must not hand out a
    fresh cap). Under _CALLS_LOCK. An unreadable file counts as nothing saved."""
    if day in _LOADED:
        return
    _LOADED.add(day)
    try:
        saved = json.loads(_count_path().read_text())
        if isinstance(saved, dict) and saved.get("day") == day and isinstance(saved.get("calls"), int):
            _CALLS[day] = max(_CALLS.get(day, 0), saved["calls"])
    except (OSError, ValueError):
        pass


def _save(day: str) -> None:
    """Under _CALLS_LOCK. Best effort: a failed write only loses the count across a restart."""
    try:
        path = _count_path()
        path.parent.mkdir(parents=True, exist_ok=True)
        tmp = path.with_suffix(".tmp")
        tmp.write_text(json.dumps({"day": day, "calls": _CALLS.get(day, 0)}))
        tmp.replace(path)
    except OSError as e:
        log.warning("firecrawl: could not save today's call count (%s)", type(e).__name__)


def left(environ: Optional[Mapping[str, str]] = None, now: Optional[float] = None) -> int:
    """Firecrawl calls left today."""
    day = _day(now)
    with _CALLS_LOCK:
        _load(day)
        return max(0, per_day(environ) - _CALLS.get(day, 0))


def take(environ: Optional[Mapping[str, str]] = None, now: Optional[float] = None) -> bool:
    """Count one Firecrawl call against today's cap: True when it may be sent, False when the day is spent."""
    day = _day(now)
    with _CALLS_LOCK:
        _load(day)
        if _CALLS.get(day, 0) >= per_day(environ):
            return False
        _CALLS[day] = _CALLS.get(day, 0) + 1
        for old in sorted(_CALLS)[:-7]:
            _CALLS.pop(old, None)
        _save(day)
        return True


def meter(what: str, credits: Any, feature: str, price: Optional[float] = None) -> None:
    """One Firecrawl call into the daily spend meter: its credits at ``price`` a credit, or unpriced."""
    from . import spend

    spend.record("firecrawl", what, feature, (credits * price) if credits and price is not None else None,
                 note=f"{credits if credits is not None else '?'} credits")


def _post(url: str, body: dict[str, Any], timeout: float) -> dict[str, Any]:
    """A Firecrawl POST through watch.http_request, so the watcher's guards hold, once today's cap allows it (every
    Firecrawl call passes here, so the cap counts them all). Raises watch.FetchError."""
    from . import watch

    key = api_key()
    if not key:
        raise watch.FetchError("Firecrawl is not set up on this computer (FIRECRAWL_API_KEY)", host=HOST)
    if not take():
        log.info("firecrawl: today's %d calls are used up (EVE_FIRECRAWL_PER_DAY); not sent", per_day())
        raise watch.FetchError(f"Firecrawl: today's {per_day()} calls are used up (EVE_FIRECRAWL_PER_DAY)",
                               host=HOST)
    try:
        _, text = watch.http_request(url, data=json.dumps(body).encode("utf-8"), timeout=timeout, headers={
            "Authorization": f"Bearer {key}", "Content-Type": "application/json", "Accept": "application/json"})
    except watch.FetchError as e:
        raise watch.FetchError(f"Firecrawl: {e.reason}", e.status, e.retry_after, host=HOST) from None
    try:
        payload = json.loads(text)
    except ValueError:
        raise watch.FetchError("Firecrawl sent an unreadable answer", host=HOST) from None
    return payload if isinstance(payload, dict) else {}


_NEEDS_LOOKUP = re.compile(r"(?:^|_)ids?$", re.I)


def _tool(item: Any) -> Optional[dict[str, Any]]:
    """An Alexandria tool as the model sees it, or None when it is not one eve can call."""
    if not isinstance(item, dict) or not item.get("provider") or not item.get("capability"):
        return None
    options = [{"name": str(o["name"]), "type": str(o.get("type") or ""), "about": str(o.get("about") or "")[:300],
                "required": o.get("required") is True}
               for o in item.get("options") or [] if isinstance(o, dict) and o.get("name")]
    # A required option that is only an id another tool must look up first (Skyscanner's search_flights wants a
    # numeric destination_entity_id) cannot be filled from the request in one step: the answer model picked it
    # anyway and it failed with invalid_option (live, 2026-09-29). Not offered. Judged by the option's NAME: Google
    # Flights' origin says "IATA code or Google city id" and takes "SFO", so an "id" in the text is no sign.
    if any(o["required"] and _NEEDS_LOOKUP.search(o["name"]) for o in options):
        return None
    return {"provider": str(item["provider"]), "capability": str(item["capability"]),
            "name": str(item.get("name") or "")[:120], "about": str(item.get("description") or "")[:600],
            "options": options}


def search(query: str, *, limit: int = RESULTS, timeout: float = SEARCH_TIMEOUT_SECS,
           tools: bool = False) -> dict[str, Any]:
    """One Firecrawl search that reads its results: {"pages": [{"url", "title", "markdown"}], "tools": [...],
    "credits": n}. ``tools``: also ask Alexandria for data tools that fit (their full contracts). Raises
    watch.FetchError."""
    body: dict[str, Any] = {
        "query": " ".join(query.split())[:500], "limit": limit,
        "sources": [{"type": "web"}] + ([{"type": "alexandria"}] if tools else []),
        "timeout": int((timeout - 5) * 1000),
        "scrapeOptions": {"formats": ["markdown"], "onlyMainContent": True, "maxAge": MAX_AGE_MS}}
    if tools:
        body["toolDetail"] = "full"
    payload = _post(SEARCH_URL, body, timeout)
    data = payload.get("data") if isinstance(payload, dict) else None
    web = data.get("web") if isinstance(data, dict) else data if isinstance(data, list) else None
    pages = []
    for item in web or []:
        if not isinstance(item, dict):
            continue
        url = str(item.get("url") or (item.get("metadata") or {}).get("sourceURL") or "")
        md = str(item.get("markdown") or item.get("description") or "")
        if url and md.strip():
            pages.append({"url": url, "title": str(item.get("title") or "")[:200], "markdown": md})
    found = [t for t in (_tool(i) for i in ((data.get("tools") if isinstance(data, dict) else None) or [])) if t]
    credits = payload.get("creditsUsed")
    return {"pages": pages, "tools": found[:MAX_TOOLS], "credits": credits if isinstance(credits, int) else None}


def run_tool(call: dict[str, Any], *, timeout: float = TOOL_TIMEOUT_SECS) -> dict[str, Any]:
    """Run one Alexandria tool: {"data": its result, "credits": n}. Raises watch.FetchError when it fails, including
    a provider whose terms the owner has not accepted (eve never accepts them)."""
    from . import watch

    payload = _post(TOOL_URL, {"alexandria": [call], "timeout": int((timeout - 5) * 1000)}, timeout)
    items = ((payload.get("data") or {}).get("alexandria") if isinstance(payload.get("data"), dict) else None) or []
    item = items[0] if items and isinstance(items[0], dict) else {}
    if not payload.get("success", True) or item.get("error") or "data" not in item:
        reason = str(payload.get("code") or item.get("error") or payload.get("error") or "no data")[:120]
        raise watch.FetchError(f"Alexandria {call.get('provider')}/{call.get('capability')}: {reason}", host=HOST)
    credits = item.get("creditsCost")
    return {"data": item["data"], "credits": credits if isinstance(credits, (int, float)) else None}


def pick_tool(payload: dict[str, Any], tools: list[dict[str, Any]]) -> Optional[dict[str, Any]]:
    """The call the model asked for, checked against what the search offered: a listed tool, only its declared
    options, every required one present, plain values. None for anything else (the pages then answer)."""
    from . import watch

    choice = watch._json_object(watch._reply_text(payload)).get("tool")
    if not isinstance(choice, dict):
        return None
    n = choice.get("id")
    if isinstance(n, str) and n.strip().isdigit():
        n = int(n)
    if not isinstance(n, int) or isinstance(n, bool) or not 1 <= n <= len(tools):
        return None
    tool = tools[n - 1]
    declared = {o["name"] for o in tool["options"]}
    raw = choice.get("options") if isinstance(choice.get("options"), dict) else {}

    def plain(v: Any) -> bool:
        return isinstance(v, (str, int, float, bool)) or (isinstance(v, list) and all(
            isinstance(x, (str, int, float, bool)) for x in v))

    options = {k: v for k, v in raw.items() if k in declared and plain(v)}
    if any(o["required"] and o["name"] not in options for o in tool["options"]):
        return None
    return {"provider": tool["provider"], "capability": tool["capability"], "options": options, "name": tool["name"]}
