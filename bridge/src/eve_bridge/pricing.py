"""Per-message cost estimation for Claude API usage.

Pure-function module: takes a ``model`` id + ``usage`` dict (as it appears in
~/.claude/projects/*.jsonl assistant records) and returns a USD estimate.

Rates as of 2026-05, USD per million tokens. Two cache-write tiers exist
because the 1-hour extended cache costs ~2× the 5-minute default cache.
Cache reads are ~10% of the input rate. Override rates by editing this
file or fork — there's intentionally no separate config layer to manage.

This is an *estimate*, not a billing source of truth. We don't account for
service_tier discounts, batch pricing, or per-request volume tiers. Good
enough for a heads-up "$N.NN today" in the statusline.
"""

from __future__ import annotations

import re
from typing import Any

# input / output / cache_write_5m / cache_write_1h / cache_read
_RATES: dict[str, dict[str, float]] = {
    "opus":   {"input": 15.0, "output": 75.0, "cache_write_5m": 18.75, "cache_write_1h": 30.0, "cache_read": 1.50},
    "sonnet": {"input":  3.0, "output": 15.0, "cache_write_5m":  3.75, "cache_write_1h":  6.0, "cache_read": 0.30},
    "haiku":  {"input":  1.0, "output":  5.0, "cache_write_5m":  1.25, "cache_write_1h":  2.0, "cache_read": 0.10},
}

# Unknown models bill at Sonnet rates — the middle of the road. We log the
# unknown model id once per process at daemon startup so a missing entry is
# surfaced rather than silently mispriced; see jsonl_tailer.py.
_DEFAULT_FAMILY = "sonnet"


def family_of(model_id: str) -> str:
    """Resolve a Claude model id ('claude-opus-4-7', 'claude-haiku-4-5-...') to a rate family."""
    if not model_id:
        return _DEFAULT_FAMILY
    m = model_id.lower()
    if "opus" in m:
        return "opus"
    if "haiku" in m:
        return "haiku"
    if "sonnet" in m:
        return "sonnet"
    return _DEFAULT_FAMILY


def estimate_cost(model_id: str, usage: dict) -> float:
    """USD cost for a single message's usage object.

    Prefers the per-TTL cache breakdown (``usage.cache_creation.ephemeral_*_input_tokens``)
    because 1-hour cache writes cost 2× the 5-minute rate. Falls back to the
    flat ``cache_creation_input_tokens`` total at the 5-minute rate when the
    breakdown isn't present (older transcripts).
    """
    family = family_of(model_id)
    r = _RATES.get(family, _RATES[_DEFAULT_FAMILY])

    inp = int(usage.get("input_tokens") or 0)
    out = int(usage.get("output_tokens") or 0)
    cache_read = int(usage.get("cache_read_input_tokens") or 0)

    cache_breakdown = usage.get("cache_creation") or {}
    cache_write_5m = int(cache_breakdown.get("ephemeral_5m_input_tokens") or 0)
    cache_write_1h = int(cache_breakdown.get("ephemeral_1h_input_tokens") or 0)
    if not cache_write_5m and not cache_write_1h:
        cache_write_5m = int(usage.get("cache_creation_input_tokens") or 0)

    total = (
        inp * r["input"]
        + out * r["output"]
        + cache_read * r["cache_read"]
        + cache_write_5m * r["cache_write_5m"]
        + cache_write_1h * r["cache_write_1h"]
    )
    return total / 1_000_000.0


# ---- OpenAI Responses and Jev: the bill per computer task ------------------------------------------------
#
# The Jev Engineering article (0xmovez, 2026-09-18): "Track the bill per completed task. A cheap decision
# that sends a worker down the wrong branch can cost more than the decision itself." Until 2026-09-21 the
# run log had tokens per turn and no money, and nothing at all for Jev. These rates are USD per million
# tokens, grounded the day they were written:
#   developers.openai.com/api/docs/pricing, 2026-09-21 (standard tier; long-context variants cost double)
#   gpt-6-sol and gpt-6-luna: developers.openai.com/api/docs/pricing, 2026-09-24 (standard tier)
#   docs.typesafe.ai/models, 2026-09-21: jev-1.13 $0.042 per million input tokens, output free
# An unknown model prices to None on purpose: a wrong figure in a bill is worse than a blank one.

OPENAI_RATES: dict[str, dict[str, float]] = {
    "gpt-6-astra":   {"input": 10.0, "cached": 1.0,  "output": 50.0},
    "gpt-6-sol":     {"input": 2.0,  "cached": 0.20, "output": 10.0},
    "gpt-6-luna":    {"input": 0.10, "cached": 0.01, "output": 0.50},
    "gpt-5.6-sol":   {"input": 4.0,  "cached": 0.40, "output": 20.0},
    "gpt-5.6-terra": {"input": 2.0,  "cached": 0.20, "output": 12.0},
    "gpt-5.6-luna":  {"input": 0.20, "cached": 0.02, "output": 1.20},
    "gpt-5.4-nano":  {"input": 0.20, "cached": 0.02, "output": 1.25},
    "gpt-5-mini":    {"input": 0.25, "cached": 0.025, "output": 2.00},
}
JEV_INPUT_PER_M = 0.042


def openai_rates(model_id: str) -> dict[str, float] | None:
    """The rate row for a Responses model id, or None. A pinned date suffix or an OpenRouter prefix is
    tolerated ("openai/gpt-5.4-nano", "gpt-6-astra-2026-08-01"); a different model is not guessed."""
    m = (model_id or "").lower().split("/")[-1]
    for name in sorted(OPENAI_RATES, key=len, reverse=True):
        if re.fullmatch(re.escape(name) + r"(?:-\d{4}-\d{2}-\d{2})?(?::[a-z0-9_-]+)?", m):
            return OPENAI_RATES[name]
    return None


def responses_tokens(usage: dict) -> dict[str, int]:
    """{"in", "cached", "out"} from a Responses API usage object; cached tokens are a subset of "in"."""
    details = usage.get("input_tokens_details") if isinstance(usage.get("input_tokens_details"), dict) else {}
    return {"in": int(usage.get("input_tokens") or 0), "cached": int(details.get("cached_tokens") or 0),
            "out": int(usage.get("output_tokens") or 0)}


def estimate_openai_cost(model_id: str, usage: dict) -> float | None:
    """USD for one Responses API usage object at the table's rates, or None for a model not in it."""
    r = openai_rates(model_id)
    if r is None:
        return None
    t = responses_tokens(usage)
    uncached = max(0, t["in"] - t["cached"])
    return (uncached * r["input"] + t["cached"] * r["cached"] + t["out"] * r["output"]) / 1_000_000.0


def estimate_jev_cost(input_tokens: int) -> float:
    """USD for Jev input tokens; output is free."""
    return max(0, int(input_tokens or 0)) * JEV_INPUT_PER_M / 1_000_000.0



# ---- every other model eve pays for: the rates spend.py meters with --------------------------------------
#
# Grounded 2026-09-24, the day the spend meter was written (owner: "i want to see how much i am spending everyday"):
#   developers.openai.com/api/docs/pricing, standard tier, fetched 2026-09-24:
#     gpt-live-1 "Voice sessions cost $0.05 per minute, billed per second"; its backend Responses calls "use the
#       normal pricing for the configured model and tools" (developers.openai.com/api/docs/models/gpt-live-1).
#       There is no per-token audio or text rate for it: the minute is the unit.
#     gpt-4o-mini-transcribe: input $1.25, output $5.00 per million tokens ($0.003 a minute, estimated).
#     text-embedding-3-small: $0.02 per million input tokens.
#     Web search (all models): $10.00 per 1,000 calls, plus the search content tokens at the model's rates.
#   platform.claude.com/docs/en/about-claude/pricing, fetched 2026-09-24: Claude Opus 5.5 input $4, output $20,
#     5-minute cache write $5, 1-hour cache write $8, cache hits and refreshes $0.20 per million tokens.
# The rule stays the one above: a model not in a table prices to None, and the meter records it unpriced.

LIVE_PER_MINUTE: dict[str, float] = {"gpt-live-1": 0.05}
TRANSCRIBE_RATES: dict[str, dict[str, float]] = {
    "gpt-4o-mini-transcribe": {"input": 1.25, "output": 5.00, "per_minute": 0.003},
    # developers.openai.com/api/docs/pricing, fetched 2026-09-30: input $2.50, output $10.00 per million tokens,
    # $0.006 a minute estimated. The call's ears since 2026-09-30 (phone_call.DEFAULT_STT_MODEL).
    "gpt-4o-transcribe": {"input": 2.50, "output": 10.00, "per_minute": 0.006},
    # The notes' speaker pass (notes.py, 2026-10-04): the same rates as gpt-4o-transcribe, $0.006 a minute
    # estimated (developers.openai.com/api/docs/pricing). A live reply carries token usage, priced by tokens.
    "gpt-4o-transcribe-diarize": {"input": 2.50, "output": 10.00, "per_minute": 0.006},
}
EMBEDDING_RATES: dict[str, float] = {"text-embedding-3-small": 0.02}
OPENAI_WEB_SEARCH_PER_CALL = 10.0 / 1000
ANTHROPIC_RATES: dict[str, dict[str, float]] = {
    "claude-opus-5-5": {"input": 4.0, "output": 20.0, "cache_write_5m": 5.0, "cache_write_1h": 8.0, "cache_read": 0.20},
}


def _named(table: dict[str, Any], model_id: str) -> Any:
    """The row for `model_id` in `table`, tolerating a provider prefix ("openai/…") and a pinned date suffix
    ("…-2026-08-01" or "…-20260801"). None for a model the table does not name."""
    m = (model_id or "").lower().split("/")[-1]
    for name in sorted(table, key=len, reverse=True):
        if re.fullmatch(re.escape(name) + r"(?:-\d{4}-\d{2}-\d{2}|-\d{8})?", m):
            return table[name]
    return None


def estimate_live_cost(model_id: str, seconds: float) -> float | None:
    """USD for a Live voice session of `seconds` (billed per second at the per-minute rate), or None."""
    rate = _named(LIVE_PER_MINUTE, model_id)
    return None if rate is None else max(0.0, float(seconds or 0)) / 60.0 * rate


def estimate_transcribe_cost(model_id: str, usage: dict | None = None, seconds: float = 0.0) -> float | None:
    """USD for one transcription: its token usage when the reply carries it ({"input_tokens", "output_tokens"}),
    else its audio seconds at the per-minute estimate. None for an unknown model, or with neither."""
    r = _named(TRANSCRIBE_RATES, model_id)
    if r is None:
        return None
    u = usage if isinstance(usage, dict) else {}
    if u.get("input_tokens") is not None or u.get("output_tokens") is not None:
        return (int(u.get("input_tokens") or 0) * r["input"] + int(u.get("output_tokens") or 0) * r["output"]) / 1e6
    seconds = float(u.get("seconds") or seconds or 0)
    return seconds / 60.0 * r["per_minute"] if seconds > 0 else None


def estimate_embedding_cost(model_id: str, input_tokens: int) -> float | None:
    rate = _named(EMBEDDING_RATES, model_id)
    return None if rate is None else max(0, int(input_tokens or 0)) * rate / 1e6


def estimate_anthropic_cost(model_id: str, usage: Any) -> float | None:
    """USD for one Messages API usage (a dict or the SDK's object) at ANTHROPIC_RATES, or None for a model not in
    it. ``input_tokens`` is already the uncached part; cache writes split by TTL when ``cache_creation`` says so,
    else every write is a 5-minute one."""
    r = _named(ANTHROPIC_RATES, model_id)
    if r is None:
        return None

    def get(obj: Any, name: str) -> Any:
        return obj.get(name) if isinstance(obj, dict) else getattr(obj, name, None)

    def n(obj: Any, name: str) -> int:
        v = get(obj, name)
        return v if isinstance(v, int) and not isinstance(v, bool) else 0

    split = get(usage, "cache_creation")
    hour = n(split, "ephemeral_1h_input_tokens") if split is not None else 0
    writes = n(usage, "cache_creation_input_tokens")
    if split is not None and not writes:
        writes = hour + n(split, "ephemeral_5m_input_tokens")
    return (n(usage, "input_tokens") * r["input"] + n(usage, "output_tokens") * r["output"]
            + n(usage, "cache_read_input_tokens") * r["cache_read"]
            + max(0, writes - hour) * r["cache_write_5m"] + hour * r["cache_write_1h"]) / 1e6
