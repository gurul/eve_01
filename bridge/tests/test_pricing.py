"""Tests for the pricing module — pure functions, no IO."""

from __future__ import annotations

from eve_bridge.pricing import estimate_cost, family_of


def test_family_of_known_models():
    assert family_of("claude-opus-4-7") == "opus"
    assert family_of("claude-opus-4-6") == "opus"
    assert family_of("claude-sonnet-4-6") == "sonnet"
    assert family_of("claude-sonnet-4") == "sonnet"
    assert family_of("claude-haiku-4-5-20251001") == "haiku"


def test_family_of_unknown_falls_back_to_sonnet():
    assert family_of("") == "sonnet"
    assert family_of("gpt-4") == "sonnet"
    assert family_of("claude-totally-new-model") == "sonnet"


def test_estimate_cost_output_only():
    # 1M output tokens on Sonnet = $15
    cost = estimate_cost("claude-sonnet-4-6", {"output_tokens": 1_000_000})
    assert abs(cost - 15.0) < 1e-9


def test_estimate_cost_opus_vs_sonnet():
    usage = {"input_tokens": 1_000_000, "output_tokens": 1_000_000}
    opus = estimate_cost("claude-opus-4-7", usage)   # 15 + 75 = $90
    sonnet = estimate_cost("claude-sonnet-4-6", usage)  # 3 + 15 = $18
    assert abs(opus - 90.0) < 1e-9
    assert abs(sonnet - 18.0) < 1e-9


def test_estimate_cost_cache_breakdown_preferred_over_flat():
    """When the per-TTL cache breakdown is present, 1h cache writes cost 2× the 5m rate."""
    usage_breakdown = {
        "output_tokens": 0,
        "cache_creation_input_tokens": 1_000_000,  # flat total — should be ignored
        "cache_creation": {
            "ephemeral_5m_input_tokens": 0,
            "ephemeral_1h_input_tokens": 1_000_000,
        },
    }
    # Sonnet 1h cache write = $6/M
    cost = estimate_cost("claude-sonnet-4-6", usage_breakdown)
    assert abs(cost - 6.0) < 1e-9


def test_estimate_cost_flat_cache_total_fallback():
    """No per-TTL breakdown → fall back to the flat total at the (cheaper) 5m rate."""
    usage_flat = {
        "output_tokens": 0,
        "cache_creation_input_tokens": 1_000_000,
        # No 'cache_creation' breakdown
    }
    # Sonnet 5m cache write = $3.75/M
    cost = estimate_cost("claude-sonnet-4-6", usage_flat)
    assert abs(cost - 3.75) < 1e-9


def test_estimate_cost_cache_read_is_cheap():
    """Cache reads should be ~10% of input rate."""
    usage = {"output_tokens": 0, "cache_read_input_tokens": 1_000_000}
    # Sonnet cache read = $0.30/M
    cost = estimate_cost("claude-sonnet-4-6", usage)
    assert abs(cost - 0.30) < 1e-9


def test_estimate_cost_empty_usage_is_zero():
    assert estimate_cost("claude-opus-4-7", {}) == 0.0


def test_estimate_cost_handles_missing_model():
    # Falls back to Sonnet rates
    cost = estimate_cost("", {"output_tokens": 1_000_000})
    assert abs(cost - 15.0) < 1e-9


def test_estimate_cost_handles_none_fields():
    """Real-world usage objects sometimes have nulls; treat them as 0."""
    usage = {
        "input_tokens": None,
        "output_tokens": 240,
        "cache_creation_input_tokens": None,
    }
    cost = estimate_cost("claude-sonnet-4-6", usage)
    expected = 240 * 15.0 / 1_000_000.0
    assert abs(cost - expected) < 1e-9


def test_estimate_cost_realistic_record():
    """Verify against a real shape lifted from a transcript file."""
    usage = {
        "input_tokens": 6,
        "cache_creation_input_tokens": 11714,
        "cache_read_input_tokens": 16928,
        "output_tokens": 240,
        "cache_creation": {
            "ephemeral_1h_input_tokens": 11714,
            "ephemeral_5m_input_tokens": 0,
        },
    }
    cost = estimate_cost("claude-opus-4-7", usage)
    # Opus rates: input $15, output $75, cache_read $1.50, cache_write_1h $30
    expected = (
        6 * 15.0
        + 240 * 75.0
        + 16928 * 1.50
        + 11714 * 30.0
    ) / 1_000_000.0
    assert abs(cost - expected) < 1e-9


# ---- OpenAI Responses and Jev (the bill per task) ----

def test_openai_responses_usage_is_priced_at_the_grounded_rates():
    from eve_bridge.pricing import estimate_openai_cost, openai_rates, responses_tokens

    usage = {"input_tokens": 1_000_000, "input_tokens_details": {"cached_tokens": 200_000}, "output_tokens": 100_000}
    # astra: 800k uncached at $10 + 200k cached at $1 + 100k out at $50 = 8 + 0.2 + 5
    assert abs(estimate_openai_cost("gpt-6-astra", usage) - 13.2) < 1e-9
    assert abs(estimate_openai_cost("gpt-5.4-nano", usage) - (0.8 * 0.20 + 0.2 * 0.02 + 0.1 * 1.25)) < 1e-9
    assert responses_tokens(usage) == {"in": 1_000_000, "cached": 200_000, "out": 100_000}
    assert responses_tokens({}) == {"in": 0, "cached": 0, "out": 0}
    assert estimate_openai_cost("gpt-6-astra", {}) == 0.0
    # a pinned or routed id still finds its row; a similar-looking other model does not
    assert openai_rates("openai/gpt-5.4-nano") is openai_rates("gpt-5.4-nano") is not None
    assert openai_rates("gpt-6-astra-2026-08-01")["input"] == 10.0
    assert openai_rates("gpt-6-luna") == {"input": 0.10, "cached": 0.01, "output": 0.50}     # the chat, 2026-09-24
    assert openai_rates("gpt-6-sol") == {"input": 2.0, "cached": 0.20, "output": 10.0}       # thinking
    assert openai_rates("gpt-6-astra-pro") is None and openai_rates("gpt-5.6-luna-pro") is None


def test_jev_input_is_priced_and_unknown_models_are_none():
    from eve_bridge.pricing import estimate_jev_cost, estimate_openai_cost

    assert abs(estimate_jev_cost(1_000_000) - 0.042) < 1e-12
    assert estimate_jev_cost(0) == 0.0 and estimate_jev_cost(-5) == 0.0
    assert estimate_openai_cost("gpt-9-unknown", {"input_tokens": 10}) is None
    assert estimate_openai_cost("", {"input_tokens": 10}) is None


def test_estimate_cost_prices_a_model_the_current_table_names_at_its_rates():
    # claude-opus-5-5 is in ANTHROPIC_RATES at $4 / $20; the family table priced it as an old Opus at $15 / $75
    usage = {"input_tokens": 1_000_000, "output_tokens": 1_000_000, "cache_read_input_tokens": 1_000_000,
             "cache_creation": {"ephemeral_5m_input_tokens": 1_000_000, "ephemeral_1h_input_tokens": 1_000_000}}
    assert abs(estimate_cost("claude-opus-5-5", usage) - (4.0 + 20.0 + 0.20 + 5.0 + 8.0)) < 1e-9
    assert abs(estimate_cost("claude-opus-5-5", {"input_tokens": 1_000_000, "cache_creation_input_tokens": 1_000_000})
               - (4.0 + 5.0)) < 1e-9
