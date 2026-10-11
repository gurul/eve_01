"""What eve spends, every day, on every model it calls.

Owner, 2026-09-24: "build in a dashboard that tracks costs (ik some of them are other keys but can u track pricing,
i want to see how much i am spending everyday)". Before this, only the app builder kept a ledger
(miniapp.SpendLedger), the computer agent wrote a bill into its run log, and everything else — the chat, the voice,
thinking, search, the camera, memory — spent without a trace.

This module is the one meter. Every place eve spends money calls ``record`` (or one of the ``record_*`` helpers
that price a provider's usage object), and one line goes into a per-day JSONL file under
``~/.config/eve-bridge/spend/`` (``EVE_SPEND_DIR`` moves it), named by the local date:

    {"t": 1790000000, "p": "openrouter", "m": "gpt-6-luna", "f": "chat", "usd": 0.00042, "src": "reported",
     "tok": {"in": 3100, "cached": 2900, "out": 80}}

* ``p`` is ``openrouter`` for every model call eve makes since 2026-10-08 (owner: one key, docs/openrouter.md),
  the Anthropic and embedding ones included; ``openai`` is left only for the desk voice's Live sessions
  (``record_live``), which still run on OpenAI until their rebuild. Older rows keep the provider they were written
  with. ``m`` is the call site's model name ("gpt-6-luna"), so pricing.py reads it whatever the prefix on the wire.

* ``src`` is ``reported`` when the provider's reply carried its own cost (OpenRouter's ``usage.cost``), ``priced``
  when eve priced the tokens at pricing.py's grounded rates.
* A model with no rate is recorded with ``usd: null`` and ``unpriced: true`` (logged once per model): a wrong
  figure is worse than a blank one, so nothing is guessed. The dashboard counts those calls separately.
* Tiny records, never a prompt, never an answer: provider, model, feature, dollars, token counts, an optional
  short note, and the chief's card id when the call was made for one. Nothing else leaves the call site.
* ``job("c7")`` tags every row written inside it with ``"job": "c7"`` (chief design 3.2, 2026-09-29: a context
  variable, so no call site has to pass a card id; ``asyncio.to_thread`` and ``contextvars.copy_context`` carry
  it into worker threads). ``job_total`` sums one card's rows: what the chief's budget reads.
* ``record`` never raises and is thread-safe: a failed write is one log line, never a failed turn.

The reader (``day_rows``, ``totals``, ``summary``) aggregates by day, provider, feature and model for the Mini App's
Spending view and the ``/spend`` command. The providers' own figures (OpenRouter, and OpenAI and Anthropic when an
admin key is set) are kept by spend_sync.py beside this ledger, never mixed into it.
"""

from __future__ import annotations

import contextlib
import contextvars
import json
import logging
import os
import re
import threading
import time
from datetime import date, datetime, timedelta
from pathlib import Path
from typing import Any, Iterable, Iterator, Optional

from . import pricing

log = logging.getLogger(__name__)

DEFAULT_DIR = "~/.config/eve-bridge/spend"
KEEP_DAYS = 400                  # older day files are left alone; the reader never looks further back than this

# The features, in plain words: what the owner sees in the split. One name per kind of work.
CHAT = "chat"
THINKING = "thinking"
VOICE = "voice"
TASKS = "browser & Mac tasks"
SEARCH = "search"
JEV = "jev"
APPS = "app builder"
MEMORY = "memory"
CAMERA = "camera"
EXPLORING = "exploring"
TRANSCRIPTION = "transcription"
ROOM_NOTES = "room notes"
LESSONS = "lessons"
CODEX = "codex"
WATCH = "watching"
MEET = "meet notes"
CALLS = "phone calls"
CHIEF = "chief of staff"

# The chief card a call is made for ("c7"), or "". Set only through ``job``.
_JOB: contextvars.ContextVar[str] = contextvars.ContextVar("spend_job", default="")
_JOB_ID = re.compile(r"^c[1-9]\d{0,8}$")

_lock = threading.Lock()
_dir_override: Optional[Path] = None
_warned: set[str] = set()


def set_dir(path: Optional[Path]) -> None:
    """Point the ledger somewhere else (tests), or back to the default with None."""
    global _dir_override
    _dir_override = Path(path) if path is not None else None


def spend_dir() -> Path:
    if _dir_override is not None:
        return _dir_override
    raw = (os.environ.get("EVE_SPEND_DIR") or "").strip()
    return Path(raw or DEFAULT_DIR).expanduser()


@contextlib.contextmanager
def job(card_id: str) -> Iterator[None]:
    """Every spend row written inside this block, on this task or a thread it hands off to with the context
    (``asyncio.to_thread``), carries ``"job": card_id``. A card id that is not ``c<N>`` tags nothing."""
    token = _JOB.set(card_id if isinstance(card_id, str) and _JOB_ID.match(card_id) else "")
    try:
        yield
    finally:
        _JOB.reset(token)


def current_job() -> str:
    return _JOB.get()


def _once(key: str, msg: str, *args: Any) -> None:
    if key not in _warned:
        _warned.add(key)
        log.warning(msg, *args)


def _local_day(ts: float) -> str:
    return datetime.fromtimestamp(ts).date().isoformat()


def _num(value: Any) -> int:
    return value if isinstance(value, int) and not isinstance(value, bool) and value > 0 else 0


def record(provider: str, model: str, feature: str, usd: Optional[float], *,
           tokens: Optional[dict[str, Any]] = None, source: str = "priced", note: str = "",
           now: Optional[float] = None) -> None:
    """One spend line in today's ledger. ``usd`` None means "not priced" (flagged, never guessed). Never raises."""
    try:
        ts = time.time() if now is None else float(now)
        row: dict[str, Any] = {"t": int(ts), "p": str(provider or "?")[:32], "m": str(model or "?")[:64],
                               "f": str(feature or "other")[:32]}
        if usd is None or not isinstance(usd, (int, float)) or usd != usd or usd < 0:
            row["usd"] = None
            row["unpriced"] = True
            if not note:
                _once(f"unpriced:{row['p']}:{row['m']}", "spend: no price for %s model %s (%s); recorded unpriced",
                      row["p"], row["m"], row["f"])
        else:
            row["usd"] = round(float(usd), 8)
        row["src"] = "reported" if source == "reported" else "priced"
        tok = {k: _num(v) for k, v in (tokens or {}).items() if _num(v)}
        if tok:
            row["tok"] = tok
        if note:
            row["note"] = " ".join(str(note).split())[:80]
        tag = _JOB.get()
        if tag:
            row["job"] = tag
        line = json.dumps(row, separators=(",", ":")) + "\n"
        folder = spend_dir()
        with _lock:
            folder.mkdir(parents=True, exist_ok=True)
            with open(folder / f"{_local_day(ts)}.jsonl", "a", encoding="utf-8") as f:
                f.write(line)
    except Exception as e:  # noqa: BLE001 — the meter never breaks the work it measures
        _once("write", "spend: a spend line was not written (%s)", type(e).__name__)


# ---- the helpers: a provider's usage object in, one priced line out ----------------------------------------

def _as_dict(obj: Any) -> dict[str, Any]:
    """A reply as a dict: a dict as is, an SDK object through model_dump, else {}."""
    if isinstance(obj, dict):
        return obj
    dump = getattr(obj, "model_dump", None)
    if callable(dump):
        try:
            out = dump(exclude_none=True)
            return out if isinstance(out, dict) else {}
        except Exception:  # noqa: BLE001
            return {}
    return {}


def _usage_of(reply: Any) -> dict[str, Any]:
    """A reply's ``usage`` as a dict, without dumping the whole reply (an embeddings reply is mostly floats)."""
    usage = reply.get("usage") if isinstance(reply, dict) else getattr(reply, "usage", None)
    return usage if isinstance(usage, dict) else _as_dict(usage)


def _web_search_calls(output: Any) -> int:
    """OpenAI's hosted web_search calls in a Responses output: $10 per 1,000, on top of the tokens."""
    return sum(1 for item in (output if isinstance(output, list) else [])
               if isinstance(item, dict) and item.get("type") == "web_search_call")


def record_response(feature: str, response: Any, *, model: str = "", provider: str = "openrouter",
                    searches: Optional[int] = None) -> None:
    """One Responses API reply (a dict or the SDK object). Priced at pricing.OPENAI_RATES, plus any hosted web
    searches it made; a reply without usage is recorded unpriced. Never raises."""
    try:
        body = _as_dict(response)
        usage = body.get("usage") if isinstance(body.get("usage"), dict) else None
        name = str(model or body.get("model") or "")
        calls = _web_search_calls(body.get("output")) if searches is None else max(0, int(searches))
        if usage is None:
            record(provider, name, feature, None, note="no usage in the reply")
            return
        if isinstance(usage.get("cost"), (int, float)):
            t = pricing.responses_tokens(usage)
            record(provider, name, feature, float(usage["cost"]), tokens=t, source="reported")
            return
        t = pricing.responses_tokens(usage)
        usd = pricing.estimate_openai_cost(name, usage)
        if usd is not None and calls:
            usd += calls * pricing.OPENAI_WEB_SEARCH_PER_CALL
        record(provider, name, feature, usd, tokens={**t, **({"searches": calls} if calls else {})})
    except Exception as e:  # noqa: BLE001
        _once("helper", "spend: could not read a reply's usage (%s)", type(e).__name__)


def record_chat_completion(feature: str, payload: Any, *, model: str = "", provider: str = "openrouter") -> None:
    """One Chat Completions reply (OpenRouter's or OpenAI's): the provider's ``usage.cost`` when it is there,
    else the tokens at pricing.OPENAI_RATES (an OpenAI model behind a prefix), else unpriced."""
    try:
        body = _as_dict(payload)
        usage = body.get("usage") if isinstance(body.get("usage"), dict) else {}
        name = str(model or body.get("model") or "")
        details = usage.get("prompt_tokens_details") if isinstance(usage.get("prompt_tokens_details"), dict) else {}
        tok = {"in": _num(usage.get("prompt_tokens")), "cached": _num(details.get("cached_tokens")),
               "out": _num(usage.get("completion_tokens"))}
        if isinstance(usage.get("cost"), (int, float)):
            record(provider, name, feature, float(usage["cost"]), tokens=tok, source="reported")
            return
        usd = None
        if usage:
            usd = pricing.estimate_openai_cost(name, {"input_tokens": tok["in"], "output_tokens": tok["out"],
                                                      "input_tokens_details": {"cached_tokens": tok["cached"]}})
        record(provider, name, feature, usd, tokens=tok)
    except Exception as e:  # noqa: BLE001
        _once("helper", "spend: could not read a reply's usage (%s)", type(e).__name__)


def _reported_cost(usage: Any) -> Optional[float]:
    """OpenRouter's own figure for a call (``usage.cost``, dollars), when the reply carries one."""
    cost = usage.get("cost") if isinstance(usage, dict) else getattr(usage, "cost", None)
    return float(cost) if isinstance(cost, (int, float)) and not isinstance(cost, bool) and cost >= 0 else None


def record_anthropic(feature: str, model: str, usage: Any, *, provider: str = "openrouter") -> Optional[float]:
    """One Messages API usage: OpenRouter's reported cost when it is there, else pricing.ANTHROPIC_RATES. Returns
    the dollars recorded (None: unpriced)."""
    try:
        def get(name: str) -> Any:
            return usage.get(name) if isinstance(usage, dict) else getattr(usage, name, None)

        reported = _reported_cost(usage)
        usd = reported if reported is not None else pricing.estimate_anthropic_cost(model, usage)
        record(provider, model, feature, usd,
               tokens={"in": get("input_tokens"), "out": get("output_tokens"),
                       "cache_read": get("cache_read_input_tokens"),
                       "cache_write": get("cache_creation_input_tokens")},
               source="reported" if reported is not None else "priced")
        return usd
    except Exception as e:  # noqa: BLE001
        _once("helper", "spend: could not read a reply's usage (%s)", type(e).__name__)
        return None


def record_embedding(feature: str, model: str, response: Any, *, provider: str = "openrouter") -> None:
    """One embeddings call: OpenRouter's reported cost when it is there, else the tokens at the listed rate."""
    usage = _usage_of(response)
    tokens = _num(usage.get("prompt_tokens")) or _num(usage.get("total_tokens"))
    reported = _reported_cost(usage)
    usd = reported if reported is not None else pricing.estimate_embedding_cost(model, tokens) if usage else None
    record(provider, model, feature, usd, tokens={"in": tokens}, source="reported" if reported is not None else "priced")


def record_transcription(feature: str, model: str, response: Any, seconds: float = 0.0, *,
                         provider: str = "openrouter") -> None:
    """One transcription: OpenRouter's reported cost when it is there, else its token usage, else its audio seconds
    at the per-minute rate."""
    usage = _usage_of(response)
    reported = _reported_cost(usage)
    usd = reported if reported is not None else pricing.estimate_transcribe_cost(model, usage, seconds)
    record(provider, model, feature, usd,
           tokens={"in": usage.get("input_tokens"), "out": usage.get("output_tokens")},
           source="reported" if reported is not None else "priced")


def record_live(feature: str, model: str, seconds: float, *, note: str = "") -> None:
    """A Live voice session: its billed seconds at the per-minute rate. Still OpenAI's (the desk voice is not on
    OpenRouter yet, docs/openrouter.md)."""
    usd = pricing.estimate_live_cost(model, seconds)
    record("openai", model, feature, usd, tokens={"secs": int(round(seconds or 0))}, note=note)


# ---- the reader ---------------------------------------------------------------------------------------------

def day_rows(day: str, folder: Optional[Path] = None) -> list[dict[str, Any]]:
    """Every line of one local day. A torn or foreign line is skipped."""
    path = (folder or spend_dir()) / f"{day}.jsonl"
    rows: list[dict[str, Any]] = []
    try:
        with open(path, encoding="utf-8") as f:
            for line in f:
                try:
                    row = json.loads(line)
                except ValueError:
                    continue
                if isinstance(row, dict):
                    rows.append(row)
    except OSError:
        return []
    return rows


def _usd(row: dict[str, Any]) -> float:
    v = row.get("usd")
    return float(v) if isinstance(v, (int, float)) and not isinstance(v, bool) else 0.0


def totals(rows: Iterable[dict[str, Any]]) -> dict[str, Any]:
    """{"usd", "calls", "unpriced", "by_provider", "by_feature", "by_model"} for some lines. The splits are sorted
    by dollars, largest first; unpriced calls count in ``unpriced`` and in no dollar figure."""
    total, calls, unpriced = 0.0, 0, 0
    splits: dict[str, dict[str, float]] = {"p": {}, "f": {}, "m": {}}
    unpriced_models: set[str] = set()
    for row in rows:
        calls += 1
        if row.get("usd") is None:
            unpriced += 1
            unpriced_models.add(f"{row.get('p', '?')}/{row.get('m', '?')}")
        usd = _usd(row)
        total += usd
        for k, split in splits.items():
            name = str(row.get(k) or "?")
            split[name] = split.get(name, 0.0) + usd

    def ordered(split: dict[str, float]) -> dict[str, float]:
        return {k: round(v, 6) for k, v in sorted(split.items(), key=lambda kv: (-kv[1], kv[0]))}

    return {"usd": round(total, 6), "calls": calls, "unpriced": unpriced,
            "unpriced_models": sorted(unpriced_models),
            "by_provider": ordered(splits["p"]), "by_feature": ordered(splits["f"]), "by_model": ordered(splits["m"])}


def job_total(card_id: str, since: date, today: Optional[date] = None, folder: Optional[Path] = None) -> float:
    """The dollars of every row tagged with this card, from ``since`` to ``today`` (at most KEEP_DAYS days).
    Unpriced rows count nothing, as in ``totals``."""
    today = today or date.today()
    d, total = max(since, today - timedelta(days=KEEP_DAYS)), 0.0
    while d <= today:
        total += sum(_usd(r) for r in day_rows(d.isoformat(), folder) if r.get("job") == card_id)
        d += timedelta(days=1)
    return round(total, 8)


def day_total(day: str, folder: Optional[Path] = None) -> float:
    return totals(day_rows(day, folder))["usd"]


def today_total(today: Optional[date] = None) -> float:
    return day_total((today or date.today()).isoformat())


def summary(today: Optional[date] = None, days: int = 30, folder: Optional[Path] = None) -> dict[str, Any]:
    """What the Spending view and /spend show: today and yesterday in full, the last `days` days as totals (oldest
    first, zeros included), and this month's total. Eve's own meter only; spend_sync adds the providers'."""
    today = today or date.today()
    base = folder or spend_dir()
    t = totals(day_rows(today.isoformat(), base))
    y_day = today - timedelta(days=1)
    y = totals(day_rows(y_day.isoformat(), base))
    series = []
    for i in range(max(1, min(days, KEEP_DAYS)) - 1, -1, -1):
        d = today - timedelta(days=i)
        usd = t["usd"] if i == 0 else y["usd"] if i == 1 else day_total(d.isoformat(), base)
        series.append({"date": d.isoformat(), "usd": round(usd, 6)})
    month = 0.0
    d = today.replace(day=1)
    while d <= today:
        month += t["usd"] if d == today else y["usd"] if d == y_day else day_total(d.isoformat(), base)
        d += timedelta(days=1)
    return {"today": {"date": today.isoformat(), **t}, "yesterday": {"date": y_day.isoformat(), **y},
            "days": series, "month": {"label": today.strftime("%Y-%m"), "usd": round(month, 6)}}


def _money(usd: Optional[float]) -> str:
    if usd is None:
        return "?"
    return f"${usd:.2f}" if usd >= 0.1 or usd == 0 else f"${usd:.3f}"


def brief(s: dict[str, Any], providers: Optional[dict[str, Any]] = None) -> str:
    """The /spend answer, written by code (no model call): today, yesterday, this month, the top three features
    today, and what OpenRouter itself says about today."""
    today, yesterday, month = s["today"], s["yesterday"], s["month"]
    lines = [f"Today: {_money(today['usd'])}", f"Yesterday: {_money(yesterday['usd'])}",
             f"This month: {_money(month['usd'])}"]
    top = [(name, usd) for name, usd in today["by_feature"].items() if usd > 0][:3]
    if top:
        lines.append("Top today: " + ", ".join(f"{name} {_money(usd)}" for name, usd in top))
    if today.get("unpriced"):
        n = today["unpriced"]
        lines.append(f"{n} call{'' if n == 1 else 's'} today had no price (Codex on the ChatGPT plan, or a model "
                     "without a rate); not in the totals.")
    orow = (providers or {}).get("openrouter") or {}
    if orow.get("status") == "ok":
        part = f"OpenRouter says today (UTC): {_money(orow.get('key_today'))} on eve's key"
        if orow.get("account_today") is not None:
            part += f", {_money(orow['account_today'])} on the whole account" + (" so far" if orow.get(
                "account_partial") else "")
        lines.append(part + ".")
    elif orow.get("status") == "error":
        lines.append(f"OpenRouter's own figure could not be read ({orow.get('error')}).")
    return "Spending (eve's own meter)\n" + "\n".join(lines)
