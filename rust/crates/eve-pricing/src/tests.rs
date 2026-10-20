//! Port of bridge/tests/test_pricing.py, plus parity checks for the spend-meter tables.

use super::*;
use serde_json::json;

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

#[test]
fn test_family_of_known_models() {
    assert_eq!(family_of("claude-opus-4-7"), "opus");
    assert_eq!(family_of("claude-opus-4-6"), "opus");
    assert_eq!(family_of("claude-sonnet-4-6"), "sonnet");
    assert_eq!(family_of("claude-sonnet-4"), "sonnet");
    assert_eq!(family_of("claude-haiku-4-5-20251001"), "haiku");
}

#[test]
fn test_family_of_unknown_falls_back_to_sonnet() {
    assert_eq!(family_of(""), "sonnet");
    assert_eq!(family_of("gpt-4"), "sonnet");
    assert_eq!(family_of("claude-totally-new-model"), "sonnet");
}

#[test]
fn test_estimate_cost_output_only() {
    // 1M output tokens on Sonnet = $15
    let cost = estimate_cost("claude-sonnet-4-6", &json!({"output_tokens": 1_000_000}));
    assert!(close(cost, 15.0));
}

#[test]
fn test_estimate_cost_opus_vs_sonnet() {
    let usage = json!({"input_tokens": 1_000_000, "output_tokens": 1_000_000});
    let opus = estimate_cost("claude-opus-4-7", &usage); // 15 + 75 = $90
    let sonnet = estimate_cost("claude-sonnet-4-6", &usage); // 3 + 15 = $18
    assert!(close(opus, 90.0));
    assert!(close(sonnet, 18.0));
}

/// When the per-TTL cache breakdown is present, 1h cache writes cost 2x the 5m rate.
#[test]
fn test_estimate_cost_cache_breakdown_preferred_over_flat() {
    let usage_breakdown = json!({
        "output_tokens": 0,
        "cache_creation_input_tokens": 1_000_000,  // flat total: should be ignored
        "cache_creation": {
            "ephemeral_5m_input_tokens": 0,
            "ephemeral_1h_input_tokens": 1_000_000,
        },
    });
    // Sonnet 1h cache write = $6/M
    assert!(close(estimate_cost("claude-sonnet-4-6", &usage_breakdown), 6.0));
}

/// No per-TTL breakdown: fall back to the flat total at the (cheaper) 5m rate.
#[test]
fn test_estimate_cost_flat_cache_total_fallback() {
    let usage_flat = json!({"output_tokens": 0, "cache_creation_input_tokens": 1_000_000});
    // Sonnet 5m cache write = $3.75/M
    assert!(close(estimate_cost("claude-sonnet-4-6", &usage_flat), 3.75));
}

/// Cache reads should be ~10% of input rate.
#[test]
fn test_estimate_cost_cache_read_is_cheap() {
    let usage = json!({"output_tokens": 0, "cache_read_input_tokens": 1_000_000});
    // Sonnet cache read = $0.30/M
    assert!(close(estimate_cost("claude-sonnet-4-6", &usage), 0.30));
}

#[test]
fn test_estimate_cost_empty_usage_is_zero() {
    assert_eq!(estimate_cost("claude-opus-4-7", &json!({})), 0.0);
}

#[test]
fn test_estimate_cost_handles_missing_model() {
    // Falls back to Sonnet rates
    assert!(close(estimate_cost("", &json!({"output_tokens": 1_000_000})), 15.0));
}

/// Real-world usage objects sometimes have nulls; treat them as 0.
#[test]
fn test_estimate_cost_handles_none_fields() {
    let usage = json!({"input_tokens": null, "output_tokens": 240, "cache_creation_input_tokens": null});
    let expected = 240.0 * 15.0 / 1_000_000.0;
    assert!(close(estimate_cost("claude-sonnet-4-6", &usage), expected));
}

/// Verify against a real shape lifted from a transcript file.
#[test]
fn test_estimate_cost_realistic_record() {
    let usage = json!({
        "input_tokens": 6,
        "cache_creation_input_tokens": 11714,
        "cache_read_input_tokens": 16928,
        "output_tokens": 240,
        "cache_creation": {"ephemeral_1h_input_tokens": 11714, "ephemeral_5m_input_tokens": 0},
    });
    // Opus rates: input $15, output $75, cache_read $1.50, cache_write_1h $30
    let expected = (6.0 * 15.0 + 240.0 * 75.0 + 16928.0 * 1.50 + 11714.0 * 30.0) / 1_000_000.0;
    assert!(close(estimate_cost("claude-opus-4-7", &usage), expected));
}

// ---- OpenAI Responses and Jev (the bill per task) ----

#[test]
fn test_openai_responses_usage_is_priced_at_the_grounded_rates() {
    let usage = json!({"input_tokens": 1_000_000, "input_tokens_details": {"cached_tokens": 200_000}, "output_tokens": 100_000});
    // astra: 800k uncached at $10 + 200k cached at $1 + 100k out at $50 = 8 + 0.2 + 5
    assert!(close(estimate_openai_cost("gpt-6-astra", &usage).unwrap(), 13.2));
    assert!(close(
        estimate_openai_cost("gpt-5.4-nano", &usage).unwrap(),
        0.8 * 0.20 + 0.2 * 0.02 + 0.1 * 1.25
    ));
    assert_eq!(responses_tokens(&usage), ResponsesTokens { input: 1_000_000, cached: 200_000, out: 100_000 });
    assert_eq!(
        serde_json::to_value(responses_tokens(&usage)).unwrap(),
        json!({"in": 1_000_000, "cached": 200_000, "out": 100_000})
    );
    assert_eq!(responses_tokens(&json!({})), ResponsesTokens { input: 0, cached: 0, out: 0 });
    assert_eq!(estimate_openai_cost("gpt-6-astra", &json!({})), Some(0.0));
    // a pinned or routed id still finds its row; a similar-looking other model does not
    let routed = openai_rates("openai/gpt-5.4-nano").unwrap();
    assert!(std::ptr::eq(routed, openai_rates("gpt-5.4-nano").unwrap()));
    assert_eq!(openai_rates("gpt-6-astra-2026-08-01").unwrap().input, 10.0);
    assert_eq!(openai_rates("gpt-6-luna"), Some(&OpenAiRates { input: 0.10, cached: 0.01, output: 0.50 })); // the chat, 2026-09-24
    assert_eq!(openai_rates("gpt-6-sol"), Some(&OpenAiRates { input: 2.0, cached: 0.20, output: 10.0 })); // thinking
    assert!(openai_rates("gpt-6-astra-pro").is_none() && openai_rates("gpt-5.6-luna-pro").is_none());
}

#[test]
fn test_jev_input_is_priced_and_unknown_models_are_none() {
    assert!((estimate_jev_cost(1_000_000) - 0.042).abs() < 1e-12);
    assert!(estimate_jev_cost(0) == 0.0 && estimate_jev_cost(-5) == 0.0);
    assert_eq!(estimate_openai_cost("gpt-9-unknown", &json!({"input_tokens": 10})), None);
    assert_eq!(estimate_openai_cost("", &json!({"input_tokens": 10})), None);
}

#[test]
fn test_estimate_cost_prices_a_model_the_current_table_names_at_its_rates() {
    // claude-opus-5-5 is in ANTHROPIC_RATES at $4 / $20; the family table priced it as an old Opus at $15 / $75
    let usage = json!({"input_tokens": 1_000_000, "output_tokens": 1_000_000, "cache_read_input_tokens": 1_000_000,
             "cache_creation": {"ephemeral_5m_input_tokens": 1_000_000, "ephemeral_1h_input_tokens": 1_000_000}});
    assert!(close(estimate_cost("claude-opus-5-5", &usage), 4.0 + 20.0 + 0.20 + 5.0 + 8.0));
    assert!(close(
        estimate_cost("claude-opus-5-5", &json!({"input_tokens": 1_000_000, "cache_creation_input_tokens": 1_000_000})),
        4.0 + 5.0
    ));
}

// ---- parity checks beyond the Python test file (behavior read from pricing.py) ----

#[test]
fn openai_rates_accepts_an_openrouter_variant_and_not_a_compact_date() {
    assert!(openai_rates("openai/gpt-6-luna:free").is_some());
    assert!(openai_rates("GPT-6-ASTRA").is_some());
    // `openai_rates` takes only the dashed date; `_named` takes both forms.
    assert!(openai_rates("gpt-6-astra-20260801").is_none());
    assert!(anthropic_named("anthropic/claude-opus-5-5-20260801").is_some());
    assert!(anthropic_named("claude-opus-5-5-2026-08-01").is_some());
    assert!(anthropic_named("claude-opus-5-5:free").is_none());
    // longest name first: gpt-4o-transcribe-diarize is its own row, not gpt-4o-transcribe plus a suffix
    assert!(std::ptr::eq(transcribe_named("gpt-4o-transcribe-diarize").unwrap(), &TRANSCRIBE_RATES[2].1));
    assert!(named(&TRANSCRIBE_RATES, "openai/gpt-4o-mini-transcribe").is_some());
}

#[test]
fn live_cost_is_per_second_at_the_minute_rate() {
    assert!(close(estimate_live_cost("gpt-live-1", 120.0).unwrap(), 0.10));
    assert_eq!(estimate_live_cost("gpt-live-1", -3.0), Some(0.0));
    assert_eq!(estimate_live_cost("gpt-live-2", 60.0), None);
}

#[test]
fn transcribe_prefers_tokens_then_seconds() {
    let tok = json!({"input_tokens": 1_000_000, "output_tokens": null});
    assert!(close(estimate_transcribe_cost("gpt-4o-transcribe", Some(&tok), 600.0).unwrap(), 2.50));
    assert!(close(estimate_transcribe_cost("gpt-4o-transcribe", None, 60.0).unwrap(), 0.006));
    let secs = json!({"seconds": 120});
    assert!(close(estimate_transcribe_cost("gpt-4o-mini-transcribe", Some(&secs), 0.0).unwrap(), 0.006));
    assert_eq!(estimate_transcribe_cost("gpt-4o-transcribe", Some(&json!({})), 0.0), None);
    assert_eq!(estimate_transcribe_cost("gpt-4o-transcribe", Some(&json!({"seconds": 0})), 0.0), None);
    assert_eq!(estimate_transcribe_cost("whisper-1", None, 60.0), None);
}

#[test]
fn embedding_cost_and_constants() {
    assert!(close(estimate_embedding_cost("text-embedding-3-small", 1_000_000).unwrap(), 0.02));
    assert_eq!(estimate_embedding_cost("text-embedding-3-small", -1), Some(0.0));
    assert_eq!(estimate_embedding_cost("text-embedding-3-large", 10), None);
    assert!(close(OPENAI_WEB_SEARCH_PER_CALL, 0.01));
}

#[test]
fn anthropic_cost_splits_writes_by_ttl() {
    // flat total with a TTL split: the 1h part at the 1h rate, the rest at 5m
    let u = json!({"input_tokens": 1_000_000, "cache_creation_input_tokens": 3_000_000,
                   "cache_creation": {"ephemeral_1h_input_tokens": 1_000_000}});
    assert!(close(estimate_anthropic_cost("claude-opus-5-5", &u).unwrap(), 4.0 + 2.0 * 5.0 + 8.0));
    // a split with no flat total sums its parts
    let u = json!({"cache_creation": {"ephemeral_1h_input_tokens": 1_000_000, "ephemeral_5m_input_tokens": 1_000_000}});
    assert!(close(estimate_anthropic_cost("claude-opus-5-5", &u).unwrap(), 5.0 + 8.0));
    // no split: every write is a 5-minute one; floats and bools read as 0
    let u = json!({"cache_creation_input_tokens": 1_000_000, "output_tokens": 5.0, "input_tokens": true});
    assert!(close(estimate_anthropic_cost("claude-opus-5-5", &u).unwrap(), 5.0));
    assert_eq!(estimate_anthropic_cost("claude-sonnet-4-6", &json!({})), None);
}

#[test]
fn estimate_cost_reads_fields_like_python_int() {
    // int(x or 0): floats truncate, numeric strings parse, false is 0
    let u = json!({"output_tokens": 1_000_000.9, "input_tokens": "1000000", "cache_read_input_tokens": false});
    assert!(close(estimate_cost("claude-sonnet-4-6", &u), 15.0 + 3.0));
    // an empty breakdown falls back to the flat total
    let u = json!({"cache_creation_input_tokens": 1_000_000, "cache_creation": {}});
    assert!(close(estimate_cost("claude-haiku-4-5", &u), 1.25));
}
