"""The providers' own figures, beside eve's meter (spend.py).

Eve's meter prices what eve itself calls. The providers know more: another key on the same account (another
tool's OpenRouter key, the owner's own scripts) and anything eve cannot price. So the daemon asks them, at start
and then hourly, off the event loop, and keeps what they said per day in ``<spend dir>/providers.json``. Nothing
here ever touches the meter's ledger: the dashboard shows the two side by side.

* **OpenRouter** (always, with ``OPENROUTER_API_KEY``): ``GET /api/v1/key`` gives eve's key's own
  ``usage_daily`` / ``usage_monthly`` (OpenRouter's docs: the current UTC day, the current UTC month), and
  ``GET /api/v1/credits`` gives the whole account's ``total_usage`` (every key). The account's spend on a day is
  the change of ``total_usage`` since the day before's last snapshot; a day with no snapshot the day before is
  counted from its own first snapshot and marked partial.
* **OpenAI**, only with ``EVE_OPENAI_ADMIN_KEY`` (an Admin key: eve's own key is refused, it lacks the
  ``api.usage.read`` scope): ``GET https://api.openai.com/v1/organization/costs?start_time=<unix>&bucket_width=1d``
  → ``data[].start_time`` (UTC day buckets) and ``data[].results[].amount.value`` in dollars, paged by
  ``next_page``.
* **Anthropic**, only with ``EVE_ANTHROPIC_ADMIN_KEY`` (an Admin API key, ``sk-ant-admin…``): ``GET
  https://api.anthropic.com/v1/organizations/cost_report?starting_at=<RFC 3339>&bucket_width=1d`` with
  ``x-api-key`` and ``anthropic-version: 2023-06-01`` → ``data[].starting_at`` and ``data[].results[].amount``,
  a decimal string in cents ("123.45" is $1.23), paged by ``next_page``.

A failure is one log line per provider and reason, and the provider's row says it could not be read; it never
breaks anything else. Keys are sent in headers only and never logged or stored.
"""

from __future__ import annotations

import asyncio
import json
import logging
import os
import time
import urllib.error
import urllib.request
from datetime import date, datetime, timedelta, timezone
from typing import Any, Callable, Mapping, Optional
from urllib.parse import urlencode

from . import spend

log = logging.getLogger(__name__)

STATE_FILE = "providers.json"
SYNC_SECS = 3600.0
TIMEOUT_SECS = 20.0
KEEP_DAYS = 400
LOOKBACK_DAYS = 31              # how far back the OpenAI and Anthropic cost reports are read each sync
MAX_PAGES = 5
OPENROUTER_KEY_URL = "https://openrouter.ai/api/v1/key"
OPENROUTER_CREDITS_URL = "https://openrouter.ai/api/v1/credits"
OPENAI_COSTS_URL = "https://api.openai.com/v1/organization/costs"
ANTHROPIC_COST_URL = "https://api.anthropic.com/v1/organizations/cost_report"
ANTHROPIC_VERSION = "2023-06-01"

Opener = Callable[..., Any]
_warned: set[str] = set()


class SyncError(RuntimeError):
    """One short reason, safe to log and to show: never a key, never a body."""


def _once(key: str, msg: str, *args: Any) -> None:
    if key not in _warned:
        _warned.add(key)
        log.warning(msg, *args)


def get_json(url: str, headers: dict[str, str], opener: Optional[Opener] = None,
             timeout: float = TIMEOUT_SECS) -> dict[str, Any]:
    send = opener or urllib.request.urlopen
    req = urllib.request.Request(url, headers={**headers, "Accept": "application/json",
                                               "User-Agent": "eve-spend"})
    try:
        with send(req, timeout=timeout) as response:
            body = json.loads(response.read().decode("utf-8"))
    except urllib.error.HTTPError as e:
        raise SyncError(f"HTTP {e.code}") from None
    except urllib.error.URLError as e:
        raise SyncError(f"unreachable ({type(e.reason).__name__})") from None
    except (TimeoutError, OSError):
        raise SyncError("timed out") from None
    except (ValueError, TypeError):
        raise SyncError("unreadable reply") from None
    if not isinstance(body, dict):
        raise SyncError("unreadable reply")
    return body


def _num(value: Any) -> Optional[float]:
    return float(value) if isinstance(value, (int, float)) and not isinstance(value, bool) else None


# ---- the three providers ---------------------------------------------------------------------------------

def sync_openrouter(state: dict[str, Any], key: str, now: float, opener: Optional[Opener] = None) -> None:
    auth = {"Authorization": f"Bearer {key}"}
    data = get_json(OPENROUTER_KEY_URL, auth, opener).get("data")
    credits = get_json(OPENROUTER_CREDITS_URL, auth, opener).get("data")
    if not isinstance(data, dict) or not isinstance(credits, dict):
        raise SyncError("unreadable reply")
    row = state.setdefault("openrouter", {})
    utc = datetime.fromtimestamp(now, timezone.utc).date().isoformat()
    daily, monthly = _num(data.get("usage_daily")), _num(data.get("usage_monthly"))
    if daily is not None:
        row.setdefault("key_days", {})[utc] = round(daily, 6)
    if monthly is not None:
        row["key_month"] = {"month": utc[:7], "usd": round(monthly, 6)}
    total = _num(credits.get("total_usage"))
    if total is not None:
        local = datetime.fromtimestamp(now).date().isoformat()
        day = row.setdefault("account_days", {}).setdefault(local, {})
        day.setdefault("first", round(total, 6))
        day["last"] = round(total, 6)
    row["at"] = int(now)
    row.pop("error", None)


def sync_openai(state: dict[str, Any], admin_key: str, now: float, opener: Optional[Opener] = None) -> None:
    start = datetime.fromtimestamp(now, timezone.utc).replace(hour=0, minute=0, second=0, microsecond=0)
    start -= timedelta(days=LOOKBACK_DAYS - 1)
    params: dict[str, Any] = {"start_time": int(start.timestamp()), "bucket_width": "1d", "limit": LOOKBACK_DAYS}
    days: dict[str, float] = {}
    for _ in range(MAX_PAGES):
        body = get_json(f"{OPENAI_COSTS_URL}?{urlencode(params)}", {"Authorization": f"Bearer {admin_key}"}, opener)
        for bucket in body.get("data") or []:
            if not isinstance(bucket, dict) or _num(bucket.get("start_time")) is None:
                continue
            day = datetime.fromtimestamp(bucket["start_time"], timezone.utc).date().isoformat()
            usd = 0.0
            for result in bucket.get("results") or []:
                amount = result.get("amount") if isinstance(result, dict) else None
                if isinstance(amount, dict) and str(amount.get("currency") or "usd").lower() == "usd":
                    usd += _num(amount.get("value")) or 0.0
            days[day] = round(days.get(day, 0.0) + usd, 6)
        if not body.get("has_more") or not body.get("next_page"):
            break
        params["page"] = body["next_page"]
    row = state.setdefault("openai", {})
    row.setdefault("days", {}).update(days)
    row["at"] = int(now)
    row.pop("error", None)


def sync_anthropic(state: dict[str, Any], admin_key: str, now: float, opener: Optional[Opener] = None) -> None:
    start = datetime.fromtimestamp(now, timezone.utc).date() - timedelta(days=LOOKBACK_DAYS - 1)
    params: dict[str, Any] = {"starting_at": f"{start.isoformat()}T00:00:00Z", "bucket_width": "1d",
                              "limit": LOOKBACK_DAYS}
    headers = {"x-api-key": admin_key, "anthropic-version": ANTHROPIC_VERSION}
    days: dict[str, float] = {}
    for _ in range(MAX_PAGES):
        body = get_json(f"{ANTHROPIC_COST_URL}?{urlencode(params)}", headers, opener)
        for bucket in body.get("data") or []:
            if not isinstance(bucket, dict) or not isinstance(bucket.get("starting_at"), str):
                continue
            day = bucket["starting_at"][:10]
            cents = 0.0
            for result in bucket.get("results") or []:
                if not isinstance(result, dict) or str(result.get("currency") or "USD").upper() != "USD":
                    continue
                try:
                    cents += float(result.get("amount") or 0)       # lowest currency unit: cents
                except (TypeError, ValueError):
                    continue
            days[day] = round(days.get(day, 0.0) + cents / 100.0, 6)
        if not body.get("has_more") or not body.get("next_page"):
            break
        params["page"] = body["next_page"]
    row = state.setdefault("anthropic", {})
    row.setdefault("days", {}).update(days)
    row["at"] = int(now)
    row.pop("error", None)


# ---- the state file ----------------------------------------------------------------------------------------

def state_path() -> Any:
    return spend.spend_dir() / STATE_FILE


def load_state() -> dict[str, Any]:
    try:
        data = json.loads(state_path().read_text())
    except (OSError, ValueError):
        return {}
    return data if isinstance(data, dict) else {}


def _prune(state: dict[str, Any]) -> None:
    for row in state.values():
        if not isinstance(row, dict):
            continue
        for name in ("key_days", "account_days", "days"):
            days = row.get(name)
            if isinstance(days, dict) and len(days) > KEEP_DAYS:
                row[name] = dict(sorted(days.items())[-KEEP_DAYS:])


def save_state(state: dict[str, Any]) -> None:
    path = state_path()
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(".tmp")
    tmp.write_text(json.dumps(state, sort_keys=True))
    tmp.replace(path)


def sync_once(env: Optional[Mapping[str, str]] = None, *, now: Optional[float] = None,
              opener: Optional[Opener] = None) -> dict[str, Any]:
    """Ask every provider eve has a key for, keep what they said, and return the state. Never raises."""
    source = os.environ if env is None else env
    now = time.time() if now is None else now
    state = load_state()
    jobs = [("openrouter", "OPENROUTER_API_KEY", sync_openrouter),
            ("openai", "EVE_OPENAI_ADMIN_KEY", sync_openai),
            ("anthropic", "EVE_ANTHROPIC_ADMIN_KEY", sync_anthropic)]
    for name, key_name, job in jobs:
        key = (source.get(key_name) or "").strip()
        if not key:
            continue
        try:
            job(state, key, now, opener)
        except SyncError as e:
            _once(f"{name}:{e}", "spend: %s's own figures could not be read (%s); tried again next hour", name, e)
            state.setdefault(name, {})["error"] = str(e)
        except Exception as e:  # noqa: BLE001 — one provider's surprise never stops the others
            _once(f"{name}:{type(e).__name__}", "spend: %s's own figures could not be read (%s)", name,
                  type(e).__name__)
            state.setdefault(name, {})["error"] = type(e).__name__
    _prune(state)
    try:
        save_state(state)
    except OSError as e:
        _once("save", "spend: the providers' figures could not be saved (%s)", type(e).__name__)
    return state


async def loop(shutdown: asyncio.Event, interval_secs: float = SYNC_SECS,
               env: Optional[Mapping[str, str]] = None) -> None:
    """At start, then every ``interval_secs`` until shutdown: one sync on a worker thread. Never raises."""
    while not shutdown.is_set():
        try:
            await asyncio.to_thread(sync_once, env)
        except Exception as e:  # noqa: BLE001
            _once(f"loop:{type(e).__name__}", "spend: a provider sync failed (%s)", type(e).__name__)
        try:
            await asyncio.wait_for(shutdown.wait(), timeout=interval_secs)
        except asyncio.TimeoutError:
            pass


# ---- what the dashboard shows ------------------------------------------------------------------------------

def reported(state: Optional[dict[str, Any]] = None, *, now: Optional[float] = None,
             env: Optional[Mapping[str, str]] = None) -> dict[str, Any]:
    """Per provider: what it says today and this month, or why there is nothing (no admin key, an error). Keyed
    by provider; each row has ``"status"``: ``ok``, ``error`` or ``no_key``."""
    state = load_state() if state is None else state
    source = os.environ if env is None else env
    now = time.time() if now is None else now
    utc = datetime.fromtimestamp(now, timezone.utc).date()
    local = datetime.fromtimestamp(now).date()
    out: dict[str, Any] = {}

    row = state.get("openrouter") if isinstance(state.get("openrouter"), dict) else {}
    if not (source.get("OPENROUTER_API_KEY") or "").strip() and not row:
        out["openrouter"] = {"status": "no_key"}
    else:
        key_days = row.get("key_days") if isinstance(row.get("key_days"), dict) else {}
        month = row.get("key_month") if isinstance(row.get("key_month"), dict) else {}
        acct = _account_day(row.get("account_days"), local)
        out["openrouter"] = {
            "status": "error" if row.get("error") else "ok" if row.get("at") else "pending",
            "error": row.get("error"), "at": row.get("at"),
            "key_today": key_days.get(utc.isoformat()), "key_day": utc.isoformat(),
            "key_month": month.get("usd") if month.get("month") == utc.isoformat()[:7] else None,
            "account_today": acct[0], "account_partial": acct[1],
        }
    for name, key_name in (("openai", "EVE_OPENAI_ADMIN_KEY"), ("anthropic", "EVE_ANTHROPIC_ADMIN_KEY")):
        row = state.get(name) if isinstance(state.get(name), dict) else {}
        if not (source.get(key_name) or "").strip():
            out[name] = {"status": "no_key", "key_name": key_name}
            continue
        days = row.get("days") if isinstance(row.get("days"), dict) else {}
        month = sum(float(v) for d, v in days.items() if str(d)[:7] == utc.isoformat()[:7]
                    and isinstance(v, (int, float)))
        out[name] = {"status": "error" if row.get("error") else "ok" if row.get("at") else "pending",
                     "error": row.get("error"), "at": row.get("at"), "day": utc.isoformat(),
                     "today": days.get(utc.isoformat()), "month": round(month, 6) if days else None}
    return out


def _account_day(days: Any, day: date) -> tuple[Optional[float], bool]:
    """(the whole OpenRouter account's spend on `day`, partial?) from the total_usage snapshots."""
    if not isinstance(days, dict) or not isinstance(days.get(day.isoformat()), dict):
        return None, False
    snap = days[day.isoformat()]
    last = _num(snap.get("last"))
    if last is None:
        return None, False
    before = days.get((day - timedelta(days=1)).isoformat())
    if isinstance(before, dict) and _num(before.get("last")) is not None:
        return round(max(0.0, last - float(before["last"])), 6), False
    first = _num(snap.get("first"))
    return (round(max(0.0, last - first), 6) if first is not None else None), True
