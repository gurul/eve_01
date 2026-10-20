//! Per-message cost estimation for Claude API usage.
//!
//! Port of `eve_bridge/pricing.py`. Pure functions: take a model id and a
//! `usage` object (as it appears in `~/.claude/projects/*.jsonl` assistant
//! records, or in an OpenAI Responses / Anthropic Messages reply) and return a
//! USD estimate.
//!
//! Rates as of 2026-05, USD per million tokens. Two cache-write tiers exist
//! because the 1-hour extended cache costs ~2x the 5-minute default cache.
//! Cache reads are ~10% of the input rate. Override rates by editing this
//! file or fork; there's intentionally no separate config layer to manage.
//!
//! This is an *estimate*, not a billing source of truth. We don't account for
//! service_tier discounts, batch pricing, or per-request volume tiers. Good
//! enough for a heads-up "$N.NN today" in the statusline.
//!
//! Usage objects are free-form JSON (`serde_json::Value`). Fields are read the
//! way Python's `int(x or 0)` reads them: null, absent, `false`, `0` and `""`
//! count as zero, floats truncate toward zero, numeric strings parse.

use std::sync::LazyLock;

use regex::Regex;
use serde::Serialize;
use serde_json::Value;

/// One Claude rate row, USD per million tokens (`input / output / cache_write_5m / cache_write_1h / cache_read`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct ClaudeRates {
    pub input: f64,
    pub output: f64,
    pub cache_write_5m: f64,
    pub cache_write_1h: f64,
    pub cache_read: f64,
}

/// The family table (`_RATES` in Python), in Python's dict order.
pub static RATES: [(&str, ClaudeRates); 3] = [
    ("opus", ClaudeRates { input: 15.0, output: 75.0, cache_write_5m: 18.75, cache_write_1h: 30.0, cache_read: 1.50 }),
    ("sonnet", ClaudeRates { input: 3.0, output: 15.0, cache_write_5m: 3.75, cache_write_1h: 6.0, cache_read: 0.30 }),
    ("haiku", ClaudeRates { input: 1.0, output: 5.0, cache_write_5m: 1.25, cache_write_1h: 2.0, cache_read: 0.10 }),
];

/// Unknown models bill at Sonnet rates, the middle of the road. The unknown
/// model id is logged once per process at daemon startup so a missing entry is
/// surfaced rather than silently mispriced; see jsonl_tailer.
pub const DEFAULT_FAMILY: &str = "sonnet";

/// The family table's row for `family`, or `None` for a family it does not name.
pub fn family_rates(family: &str) -> Option<&'static ClaudeRates> {
    RATES.iter().find(|(name, _)| *name == family).map(|(_, r)| r)
}

fn default_rates() -> &'static ClaudeRates {
    family_rates(DEFAULT_FAMILY).expect("the default family is in RATES")
}

/// Resolve a Claude model id ('claude-opus-4-7', 'claude-haiku-4-5-...') to a rate family.
pub fn family_of(model_id: &str) -> &'static str {
    if model_id.is_empty() {
        return DEFAULT_FAMILY;
    }
    let m = model_id.to_lowercase();
    if m.contains("opus") {
        return "opus";
    }
    if m.contains("haiku") {
        return "haiku";
    }
    if m.contains("sonnet") {
        return "sonnet";
    }
    DEFAULT_FAMILY
}

// ---- Python value semantics over JSON ------------------------------------------------------------------

/// Python truthiness of a JSON value.
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

fn number_to_i64(n: &serde_json::Number) -> i64 {
    if let Some(i) = n.as_i64() {
        i
    } else if n.as_u64().is_some() {
        i64::MAX
    } else {
        let f = n.as_f64().unwrap_or(0.0);
        if f.is_finite() {
            f.trunc() as i64
        } else {
            0
        }
    }
}

/// `int(v or 0)`. Where Python would raise (a non-numeric string, a non-empty list), this returns 0.
fn py_int(v: Option<&Value>) -> i64 {
    match v {
        None => 0,
        Some(v) if !truthy(v) => 0,
        Some(Value::Bool(b)) => i64::from(*b),
        Some(Value::Number(n)) => number_to_i64(n),
        Some(Value::String(s)) => s.trim().replace('_', "").parse::<i64>().unwrap_or(0),
        Some(_) => 0,
    }
}

/// `float(v)` for a truthy value. Where Python would raise, this returns 0.0.
fn py_float(v: &Value) -> f64 {
    match v {
        Value::Bool(b) => f64::from(u8::from(*b)),
        Value::Number(n) => n.as_f64().unwrap_or(0.0),
        Value::String(s) => s.trim().parse::<f64>().unwrap_or(0.0),
        _ => 0.0,
    }
}

/// `obj.get(name)` on a dict; anything that is not an object reads as empty.
fn field<'a>(obj: &'a Value, name: &str) -> Option<&'a Value> {
    obj.as_object().and_then(|o| o.get(name))
}

// ---- table lookup by model id ---------------------------------------------------------------------------

/// Names of a table sorted longest first (stable, as Python's `sorted(..., reverse=True)` on length), each with
/// its full-match pattern: the escaped name plus `suffix`.
fn build_matchers(names: &[&str], suffix: &str) -> Vec<(Regex, usize)> {
    let mut order: Vec<usize> = (0..names.len()).collect();
    order.sort_by(|a, b| names[*b].len().cmp(&names[*a].len()));
    order
        .into_iter()
        .map(|i| {
            let pattern = format!(r"\A(?:{}{})\z", regex::escape(names[i]), suffix);
            (Regex::new(&pattern).expect("pricing table pattern compiles"), i)
        })
        .collect()
}

/// The bare model id: lowercased, with any provider prefix ("openai/...") dropped.
fn bare(model_id: &str) -> String {
    let m = model_id.to_lowercase();
    m.rsplit('/').next().unwrap_or("").to_string()
}

fn lookup(matchers: &[(Regex, usize)], model_id: &str) -> Option<usize> {
    let m = bare(model_id);
    matchers.iter().find(|(re, _)| re.is_match(&m)).map(|(_, i)| *i)
}

/// `_named`'s suffix: a pinned date ("-2026-08-01" or "-20260801").
const NAMED_SUFFIX: &str = r"(?:-\d{4}-\d{2}-\d{2}|-\d{8})?";
/// `openai_rates`'s suffix: a pinned date, then an OpenRouter variant (":free").
const OPENAI_SUFFIX: &str = r"(?:-\d{4}-\d{2}-\d{2})?(?::[a-z0-9_-]+)?";

fn names_of<T>(table: &[(&'static str, T)]) -> Vec<&'static str> {
    table.iter().map(|(n, _)| *n).collect()
}

/// The row for `model_id` in `table`, tolerating a provider prefix ("openai/...") and a pinned date suffix
/// ("...-2026-08-01" or "...-20260801"). `None` for a model the table does not name. (`_named` in Python.)
pub fn named<'t, T>(table: &'t [(&str, T)], model_id: &str) -> Option<&'t T> {
    let names: Vec<&str> = table.iter().map(|(n, _)| *n).collect();
    let matchers = build_matchers(&names, NAMED_SUFFIX);
    lookup(&matchers, model_id).map(|i| &table[i].1)
}

// ---- Claude: one transcript message ---------------------------------------------------------------------

/// USD cost for a single message's usage object.
///
/// Prefers the per-TTL cache breakdown (`usage.cache_creation.ephemeral_*_input_tokens`) because 1-hour cache
/// writes cost 2x the 5-minute rate. Falls back to the flat `cache_creation_input_tokens` total at the 5-minute
/// rate when the breakdown isn't present (older transcripts).
pub fn estimate_cost(model_id: &str, usage: &Value) -> f64 {
    // A model ANTHROPIC_RATES names prices at that row; the family table is for the rest.
    // Without this, claude-opus-5-5 at $4 / $20 was priced as an old Opus at $15 / $75.
    let r = anthropic_named(model_id)
        .or_else(|| family_rates(family_of(model_id)))
        .unwrap_or_else(default_rates);

    let inp = py_int(field(usage, "input_tokens"));
    let out = py_int(field(usage, "output_tokens"));
    let cache_read = py_int(field(usage, "cache_read_input_tokens"));

    let empty = Value::Null;
    let cache_breakdown = match field(usage, "cache_creation") {
        Some(v) if truthy(v) => v,
        _ => &empty,
    };
    let mut cache_write_5m = py_int(field(cache_breakdown, "ephemeral_5m_input_tokens"));
    let cache_write_1h = py_int(field(cache_breakdown, "ephemeral_1h_input_tokens"));
    if cache_write_5m == 0 && cache_write_1h == 0 {
        cache_write_5m = py_int(field(usage, "cache_creation_input_tokens"));
    }

    let total = inp as f64 * r.input
        + out as f64 * r.output
        + cache_read as f64 * r.cache_read
        + cache_write_5m as f64 * r.cache_write_5m
        + cache_write_1h as f64 * r.cache_write_1h;
    total / 1_000_000.0
}

// ---- OpenAI Responses and Jev: the bill per computer task ----------------------------------------------
//
// The Jev Engineering article (0xmovez, 2026-09-18): "Track the bill per completed task. A cheap decision
// that sends a worker down the wrong branch can cost more than the decision itself." Until 2026-09-21 the
// run log had tokens per turn and no money, and nothing at all for Jev. These rates are USD per million
// tokens, grounded the day they were written:
//   developers.openai.com/api/docs/pricing, 2026-09-21 (standard tier; long-context variants cost double)
//   gpt-6-sol and gpt-6-luna: developers.openai.com/api/docs/pricing, 2026-09-24 (standard tier)
//   docs.typesafe.ai/models, 2026-09-21: jev-1.13 $0.042 per million input tokens, output free
// An unknown model prices to None on purpose: a wrong figure in a bill is worse than a blank one.

/// One OpenAI Responses rate row, USD per million tokens.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct OpenAiRates {
    pub input: f64,
    pub cached: f64,
    pub output: f64,
}

pub static OPENAI_RATES: [(&str, OpenAiRates); 8] = [
    ("gpt-6-astra", OpenAiRates { input: 10.0, cached: 1.0, output: 50.0 }),
    ("gpt-6-sol", OpenAiRates { input: 2.0, cached: 0.20, output: 10.0 }),
    ("gpt-6-luna", OpenAiRates { input: 0.10, cached: 0.01, output: 0.50 }),
    ("gpt-5.6-sol", OpenAiRates { input: 4.0, cached: 0.40, output: 20.0 }),
    ("gpt-5.6-terra", OpenAiRates { input: 2.0, cached: 0.20, output: 12.0 }),
    ("gpt-5.6-luna", OpenAiRates { input: 0.20, cached: 0.02, output: 1.20 }),
    ("gpt-5.4-nano", OpenAiRates { input: 0.20, cached: 0.02, output: 1.25 }),
    ("gpt-5-mini", OpenAiRates { input: 0.25, cached: 0.025, output: 2.00 }),
];
pub const JEV_INPUT_PER_M: f64 = 0.042;

static OPENAI_MATCHERS: LazyLock<Vec<(Regex, usize)>> =
    LazyLock::new(|| build_matchers(&names_of(&OPENAI_RATES), OPENAI_SUFFIX));

/// The rate row for a Responses model id, or `None`. A pinned date suffix or an OpenRouter prefix is tolerated
/// ("openai/gpt-5.4-nano", "gpt-6-astra-2026-08-01"); a different model is not guessed.
pub fn openai_rates(model_id: &str) -> Option<&'static OpenAiRates> {
    lookup(&OPENAI_MATCHERS, model_id).map(|i| &OPENAI_RATES[i].1)
}

/// `{"in", "cached", "out"}` from a Responses API usage object; cached tokens are a subset of "in".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct ResponsesTokens {
    #[serde(rename = "in")]
    pub input: i64,
    pub cached: i64,
    pub out: i64,
}

/// `{"in", "cached", "out"}` from a Responses API usage object; cached tokens are a subset of "in".
pub fn responses_tokens(usage: &Value) -> ResponsesTokens {
    let empty = Value::Null;
    let details = match field(usage, "input_tokens_details") {
        Some(v @ Value::Object(_)) => v,
        _ => &empty,
    };
    ResponsesTokens {
        input: py_int(field(usage, "input_tokens")),
        cached: py_int(field(details, "cached_tokens")),
        out: py_int(field(usage, "output_tokens")),
    }
}

/// USD for one Responses API usage object at the table's rates, or `None` for a model not in it.
pub fn estimate_openai_cost(model_id: &str, usage: &Value) -> Option<f64> {
    let r = openai_rates(model_id)?;
    let t = responses_tokens(usage);
    let uncached = 0.max(t.input.saturating_sub(t.cached));
    Some((uncached as f64 * r.input + t.cached as f64 * r.cached + t.out as f64 * r.output) / 1_000_000.0)
}

/// USD for Jev input tokens; output is free.
pub fn estimate_jev_cost(input_tokens: i64) -> f64 {
    0.max(input_tokens) as f64 * JEV_INPUT_PER_M / 1_000_000.0
}

// ---- every other model eve pays for: the rates spend meters with -----------------------------------------
//
// Grounded 2026-09-24, the day the spend meter was written (owner: "i want to see how much i am spending everyday"):
//   developers.openai.com/api/docs/pricing, standard tier, fetched 2026-09-24:
//     gpt-live-1 "Voice sessions cost $0.05 per minute, billed per second"; its backend Responses calls "use the
//       normal pricing for the configured model and tools" (developers.openai.com/api/docs/models/gpt-live-1).
//       There is no per-token audio or text rate for it: the minute is the unit.
//     gpt-4o-mini-transcribe: input $1.25, output $5.00 per million tokens ($0.003 a minute, estimated).
//     text-embedding-3-small: $0.02 per million input tokens.
//     Web search (all models): $10.00 per 1,000 calls, plus the search content tokens at the model's rates.
//   platform.claude.com/docs/en/about-claude/pricing, fetched 2026-09-24: Claude Opus 5.5 input $4, output $20,
//     5-minute cache write $5, 1-hour cache write $8, cache hits and refreshes $0.20 per million tokens.
// The rule stays the one above: a model not in a table prices to None, and the meter records it unpriced.

pub static LIVE_PER_MINUTE: [(&str, f64); 1] = [("gpt-live-1", 0.05)];

/// One transcription rate row: USD per million tokens, plus the per-minute estimate.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct TranscribeRates {
    pub input: f64,
    pub output: f64,
    pub per_minute: f64,
}

pub static TRANSCRIBE_RATES: [(&str, TranscribeRates); 3] = [
    ("gpt-4o-mini-transcribe", TranscribeRates { input: 1.25, output: 5.00, per_minute: 0.003 }),
    // developers.openai.com/api/docs/pricing, fetched 2026-09-30: input $2.50, output $10.00 per million tokens,
    // $0.006 a minute estimated. The call's ears since 2026-09-30 (phone_call.DEFAULT_STT_MODEL).
    ("gpt-4o-transcribe", TranscribeRates { input: 2.50, output: 10.00, per_minute: 0.006 }),
    // The notes' speaker pass (notes, 2026-10-04): the same rates as gpt-4o-transcribe, $0.006 a minute
    // estimated (developers.openai.com/api/docs/pricing). A live reply carries token usage, priced by tokens.
    ("gpt-4o-transcribe-diarize", TranscribeRates { input: 2.50, output: 10.00, per_minute: 0.006 }),
];
pub static EMBEDDING_RATES: [(&str, f64); 1] = [("text-embedding-3-small", 0.02)];
pub const OPENAI_WEB_SEARCH_PER_CALL: f64 = 10.0 / 1000.0;
pub static ANTHROPIC_RATES: [(&str, ClaudeRates); 1] = [(
    "claude-opus-5-5",
    ClaudeRates { input: 4.0, output: 20.0, cache_write_5m: 5.0, cache_write_1h: 8.0, cache_read: 0.20 },
)];

static LIVE_MATCHERS: LazyLock<Vec<(Regex, usize)>> =
    LazyLock::new(|| build_matchers(&names_of(&LIVE_PER_MINUTE), NAMED_SUFFIX));
static TRANSCRIBE_MATCHERS: LazyLock<Vec<(Regex, usize)>> =
    LazyLock::new(|| build_matchers(&names_of(&TRANSCRIBE_RATES), NAMED_SUFFIX));
static EMBEDDING_MATCHERS: LazyLock<Vec<(Regex, usize)>> =
    LazyLock::new(|| build_matchers(&names_of(&EMBEDDING_RATES), NAMED_SUFFIX));
static ANTHROPIC_MATCHERS: LazyLock<Vec<(Regex, usize)>> =
    LazyLock::new(|| build_matchers(&names_of(&ANTHROPIC_RATES), NAMED_SUFFIX));

/// The ANTHROPIC_RATES row for `model_id` (`_named(ANTHROPIC_RATES, model_id)`), or `None`.
pub fn anthropic_named(model_id: &str) -> Option<&'static ClaudeRates> {
    lookup(&ANTHROPIC_MATCHERS, model_id).map(|i| &ANTHROPIC_RATES[i].1)
}

/// The TRANSCRIBE_RATES row for `model_id`, or `None`.
pub fn transcribe_named(model_id: &str) -> Option<&'static TranscribeRates> {
    lookup(&TRANSCRIBE_MATCHERS, model_id).map(|i| &TRANSCRIBE_RATES[i].1)
}

/// The LIVE_PER_MINUTE rate for `model_id`, or `None`.
pub fn live_named(model_id: &str) -> Option<f64> {
    lookup(&LIVE_MATCHERS, model_id).map(|i| LIVE_PER_MINUTE[i].1)
}

/// The EMBEDDING_RATES rate for `model_id`, or `None`.
pub fn embedding_named(model_id: &str) -> Option<f64> {
    lookup(&EMBEDDING_MATCHERS, model_id).map(|i| EMBEDDING_RATES[i].1)
}

/// USD for a Live voice session of `seconds` (billed per second at the per-minute rate), or `None`.
pub fn estimate_live_cost(model_id: &str, seconds: f64) -> Option<f64> {
    let rate = live_named(model_id)?;
    // `max(0.0, float(seconds or 0))`: Python's max keeps 0.0 for NaN, as f64::max does.
    Some(0.0_f64.max(seconds) / 60.0 * rate)
}

/// USD for one transcription: its token usage when the reply carries it (`{"input_tokens", "output_tokens"}`),
/// else its audio seconds at the per-minute estimate. `None` for an unknown model, or with neither.
pub fn estimate_transcribe_cost(model_id: &str, usage: Option<&Value>, seconds: f64) -> Option<f64> {
    let r = transcribe_named(model_id)?;
    let empty = Value::Null;
    let u = match usage {
        Some(v @ Value::Object(_)) => v,
        _ => &empty,
    };
    let present = |name: &str| field(u, name).is_some_and(|v| !v.is_null());
    if present("input_tokens") || present("output_tokens") {
        return Some(
            (py_int(field(u, "input_tokens")) as f64 * r.input + py_int(field(u, "output_tokens")) as f64 * r.output)
                / 1e6,
        );
    }
    // `float(u.get("seconds") or seconds or 0)`
    let seconds = match field(u, "seconds") {
        Some(v) if truthy(v) => py_float(v),
        _ if seconds != 0.0 => seconds,
        _ => 0.0,
    };
    if seconds > 0.0 {
        Some(seconds / 60.0 * r.per_minute)
    } else {
        None
    }
}

pub fn estimate_embedding_cost(model_id: &str, input_tokens: i64) -> Option<f64> {
    let rate = embedding_named(model_id)?;
    Some(0.max(input_tokens) as f64 * rate / 1e6)
}

/// USD for one Messages API usage (a JSON object; SDK objects are serialized to one by the caller) at
/// ANTHROPIC_RATES, or `None` for a model not in it. `input_tokens` is already the uncached part; cache writes
/// split by TTL when `cache_creation` says so, else every write is a 5-minute one.
pub fn estimate_anthropic_cost(model_id: &str, usage: &Value) -> Option<f64> {
    let r = anthropic_named(model_id)?;

    // Only a real integer counts (a bool or a float reads as 0), unlike `int(x or 0)` elsewhere.
    fn n(obj: Option<&Value>, name: &str) -> i64 {
        match obj.and_then(|o| field(o, name)) {
            Some(Value::Number(num)) if num.is_i64() || num.is_u64() => number_to_i64(num),
            _ => 0,
        }
    }

    let split = field(usage, "cache_creation").filter(|v| !v.is_null());
    let hour = if split.is_some() { n(split, "ephemeral_1h_input_tokens") } else { 0 };
    let mut writes = n(Some(usage), "cache_creation_input_tokens");
    if split.is_some() && writes == 0 {
        writes = hour.saturating_add(n(split, "ephemeral_5m_input_tokens"));
    }
    Some(
        (n(Some(usage), "input_tokens") as f64 * r.input
            + n(Some(usage), "output_tokens") as f64 * r.output
            + n(Some(usage), "cache_read_input_tokens") as f64 * r.cache_read
            + 0.max(writes.saturating_sub(hour)) as f64 * r.cache_write_5m
            + hour as f64 * r.cache_write_1h)
            / 1e6,
    )
}

#[cfg(test)]
mod tests;
