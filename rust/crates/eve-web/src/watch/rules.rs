//! watch.py's "a watch and its conditions" section: the stored watch, its edge-triggered rule, and the words the
//! owner reads.

use std::sync::LazyLock;

use chrono::{NaiveDate, NaiveDateTime};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::page::Reading;
use super::{
    default_every_secs, floor_secs, CONDITIONS, CURRENCY_SIGNS, KINDS, MAX_ERROR_STRETCH, MAX_EVERY_SECS,
    MAX_PERIOD_HOURS, NOTE_CHARS,
};

/// One watch, as stored in watches.json. The field order is Python's dataclass order (`to_json` writes it).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Watch {
    pub id: String,
    pub kind: String,
    pub target: String,
    pub label: String,
    pub condition: String,
    /// the price mark, or the percentage
    pub value: Option<f64>,
    /// the phrase, for "appears"
    pub text: String,
    /// ticketmaster: the city filter
    pub city: String,
    pub every_secs: i64,
    pub created: f64,
    pub next_at: f64,
    pub checks: i64,
    pub last_checked: f64,
    pub last_value: Option<f64>,
    pub last_available: Option<bool>,
    pub last_hit: Option<bool>,
    pub currency: String,
    pub baseline: Option<f64>,
    pub armed: bool,
    pub fired: i64,
    pub last_fired: f64,
    pub errors: i64,
    pub told_error: bool,
    pub last_error: String,
    /// an alert whose send failed, sent again at the next tick
    pub pending: String,
    /// not checked while paused (its state is kept)
    pub paused: bool,
    /// a pause that ends by itself at this time; 0: until resumed
    pub paused_until: f64,
    /// the watch is removed at this time, and the owner told; 0: no end
    pub ends_at: f64,
    pub last_note: String,
    pub last_url: String,
    /// "tls" (read as Chrome would), "browser" (rendered, then seen), "hosted" (TinyFish) or "search" when a page
    /// refuses a plain read
    pub via: String,
    /// ticketmaster: the attraction id the keyword resolved to
    #[serde(rename = "ref")]
    pub ref_: String,
    /// when it was looked up
    pub ref_at: f64,
    /// the page text the last model reading was made from (a hash)
    pub digest: String,
    /// that model reading, reused while the text is the same
    pub cached: Map<String, Value>,
}

impl Watch {
    /// `Watch(id, kind, target, label, condition)` with every other field at its default.
    pub fn new(id: &str, kind: &str, target: &str, label: &str, condition: &str) -> Self {
        Watch {
            id: id.to_string(),
            kind: kind.to_string(),
            target: target.to_string(),
            label: label.to_string(),
            condition: condition.to_string(),
            value: None,
            text: String::new(),
            city: String::new(),
            every_secs: 0,
            created: 0.0,
            next_at: 0.0,
            checks: 0,
            last_checked: 0.0,
            last_value: None,
            last_available: None,
            last_hit: None,
            currency: String::new(),
            baseline: None,
            armed: true,
            fired: 0,
            last_fired: 0.0,
            errors: 0,
            told_error: false,
            last_error: String::new(),
            pending: String::new(),
            paused: false,
            paused_until: 0.0,
            ends_at: 0.0,
            last_note: String::new(),
            last_url: String::new(),
            via: String::new(),
            ref_: String::new(),
            ref_at: 0.0,
            digest: String::new(),
            cached: Map::new(),
        }
    }

    /// `dataclasses.asdict(self)`: every field, in order (floats stay floats, ints ints).
    pub fn to_json(&self) -> Value {
        serde_json::to_value(self).expect("a Watch is plain data")
    }

    /// A stored watch, defensively: an unknown key is dropped, a field of the wrong type goes back to its default,
    /// a number that is not finite (or past 1e15) is none, an id is a lowercase wN; the label is collapsed (else
    /// the target), the counters clamped, `via` one of the known rungs, `every_secs` inside its floor and
    /// MAX_EVERY_SECS. None for anything that is not a watch.
    ///
    /// A hand-edited file with a 400-digit number or a NaN used to raise out of the loader and stop the daemon's
    /// start (re-verification, 2026-09-25).
    pub fn from_json(raw: &Value) -> Option<Watch> {
        let raw = raw.as_object()?;
        let kind = raw.get("kind").and_then(Value::as_str).filter(|k| KINDS.contains(k))?;
        let cond = raw.get("condition").and_then(Value::as_str).filter(|c| CONDITIONS.contains(c))?;
        let target = raw.get("target").and_then(Value::as_str).filter(|t| !py::strip(t).is_empty())?;
        let wid = py::strip(raw.get("id").and_then(Value::as_str)?);
        if !WATCH_ID.is_match(wid) {
            return None;
        }
        let mut w = Watch::new(&wid.to_lowercase(), kind, target, "", cond);

        // Each field keeps its default unless the stored value has the field's type.
        let get = |name: &str| raw.get(name);
        let set_bool = |slot: &mut bool, name: &str| {
            if let Some(Value::Bool(b)) = get(name) {
                *slot = *b;
            }
        };
        let set_opt_bool = |slot: &mut Option<bool>, name: &str| match get(name) {
            Some(Value::Bool(b)) => *slot = Some(*b),
            Some(Value::Null) => *slot = None,
            _ => {}
        };
        let set_float = |slot: &mut f64, name: &str| {
            if let Some(v) = get(name).and_then(stored_number) {
                *slot = v;
            }
        };
        let set_int = |slot: &mut i64, name: &str| {
            if let Some(v) = get(name).and_then(stored_number) {
                *slot = v.trunc() as i64;
            }
        };
        let set_opt_float = |slot: &mut Option<f64>, name: &str| match get(name) {
            Some(Value::Null) => *slot = None,
            Some(v) => {
                if let Some(x) = stored_number(v) {
                    *slot = Some(x);
                }
            }
            None => {}
        };
        let set_str = |slot: &mut String, name: &str| {
            if let Some(Value::String(s)) = get(name) {
                *slot = py::take(s, 4000);
            }
        };

        set_str(&mut w.label, "label");
        set_opt_float(&mut w.value, "value");
        set_str(&mut w.text, "text");
        set_str(&mut w.city, "city");
        set_int(&mut w.every_secs, "every_secs");
        set_float(&mut w.created, "created");
        set_float(&mut w.next_at, "next_at");
        set_int(&mut w.checks, "checks");
        set_float(&mut w.last_checked, "last_checked");
        set_opt_float(&mut w.last_value, "last_value");
        set_opt_bool(&mut w.last_available, "last_available");
        set_opt_bool(&mut w.last_hit, "last_hit");
        set_str(&mut w.currency, "currency");
        set_opt_float(&mut w.baseline, "baseline");
        set_bool(&mut w.armed, "armed");
        set_int(&mut w.fired, "fired");
        set_float(&mut w.last_fired, "last_fired");
        set_int(&mut w.errors, "errors");
        set_bool(&mut w.told_error, "told_error");
        set_str(&mut w.last_error, "last_error");
        set_str(&mut w.pending, "pending");
        set_bool(&mut w.paused, "paused");
        set_float(&mut w.paused_until, "paused_until");
        set_float(&mut w.ends_at, "ends_at");
        set_str(&mut w.last_note, "last_note");
        set_str(&mut w.last_url, "last_url");
        set_str(&mut w.via, "via");
        set_str(&mut w.ref_, "ref");
        set_float(&mut w.ref_at, "ref_at");
        set_str(&mut w.digest, "digest");
        if let Some(Value::Object(m)) = get("cached") {
            w.cached = m.clone();
        }

        let label = py::take(&py::split_join(&w.label), 80);
        w.label = if label.is_empty() { py::take(&w.target, 80) } else { label };
        w.errors = w.errors.clamp(0, 1_000_000);
        w.checks = w.checks.clamp(0, 1_000_000);
        w.fired = w.fired.clamp(0, 1_000_000);
        if !["", "tls", "browser", "hosted", "search"].contains(&w.via.as_str()) {
            w.via = String::new();
        }
        let floor = if w.via == "browser" || w.via == "hosted" { floor_secs(&w.via) } else { floor_secs(&w.kind) };
        let every = if w.every_secs != 0 { w.every_secs } else { default_every_secs(&w.kind) };
        w.every_secs = floor.max(MAX_EVERY_SECS.min(every));
        Some(w)
    }
}

static WATCH_ID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[wW]\d{1,9}$").expect("watch id regex"));

/// A stored number field: a JSON number (never a bool), finite and under 1e15 in size; None otherwise (the field
/// keeps its default).
fn stored_number(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64().filter(|x| x.is_finite() && x.abs() < 1e15),
        _ => None,
    }
}

/// Yahoo quotes some exchanges in the minor unit: pence, cents, agorot (yfinance's history.py lists these three).
pub const MINOR_UNITS: [(&str, &str); 4] = [("GBp", "GBP"), ("GBX", "GBP"), ("ZAc", "ZAR"), ("ILA", "ILS")];
/// `_OPTIONAL_FLOATS`.
pub const OPTIONAL_FLOATS: [&str; 3] = ["value", "last_value", "baseline"];
/// `_OPTIONAL_BOOLS`.
pub const OPTIONAL_BOOLS: [&str; 2] = ["last_available", "last_hit"];

/// `_price(raw)` where Python's only answer for anything but a number or a string is None: the page reader's
/// parser for those two, None for the rest.
pub(crate) fn price_of(raw: Option<&Value>) -> Option<f64> {
    match raw {
        Some(v @ (Value::Number(_) | Value::String(_))) => super::page::price(v, ""),
        _ => None,
    }
}

/// A chart meta block as a reading: the regular-session price (what a mark is set against), in the major unit,
/// and outside the regular session a note with the extended-hours price when Yahoo has a different one
/// (`fulldayPrice`, undocumented; it matched the last pre/post-market bar when measured, 2026-09-25).
pub fn quote_reading(meta: &Map<String, Value>, price: f64, now: f64) -> Reading {
    let raw_cur = py::str_or_empty(meta.get("currency"));
    let minor = MINOR_UNITS.iter().find(|(k, _)| *k == raw_cur).map(|(_, major)| *major);
    let cur = py::take(&minor.map(str::to_string).unwrap_or_else(|| raw_cur.to_uppercase()), 8);
    let scale = if minor.is_some() { 0.01 } else { 1.0 };
    let name = ["shortName", "longName", "symbol"]
        .iter()
        .map(|k| meta.get(*k))
        .find(|v| v.is_some_and(py::truthy))
        .flatten();
    let mut r = Reading {
        value: Some(py::round_ndigits(price * scale, 6)),
        currency: cur.clone(),
        source: "quote".to_string(),
        name: py::str_or_empty(name),
        ..Reading::default()
    };
    let full = price_of(meta.get("fulldayPrice"));
    let regular = py::obj(meta.get("currentTradingPeriod")).and_then(|p| py::obj(p.get("regular")));
    let start = regular.and_then(|m| py::number(m.get("start")));
    let end = regular.and_then(|m| py::number(m.get("end")));
    let outside = match (start, end) {
        (Some(s), Some(e)) => !(s <= now && now < e),
        _ => false,
    };
    if let Some(full) = full {
        if outside && (full - price).abs() > 1e-9 {
            let word = if now >= end.unwrap_or(0.0) { "after" } else { "before" };
            r.note = format!("{word} hours {}", money(Some(full * scale), &cur));
        }
    }
    r
}

/// A note from outside (a model, a web page, Ticketmaster's event names) as plain words: whitespace collapsed, at
/// most NOTE_CHARS, `[` and `]` become `(` and `)`: the Telegram formatter turns [words](link) into a link that
/// shows only the words.
pub fn plain_note(note: &str) -> String {
    py::take(&py::split_join(note), NOTE_CHARS).replace('[', "(").replace(']', ")")
}

/// "$1,299", "$0.00001234", "85 CHF", "?" for none: the currency's sign, thousands grouped, ".00" dropped at 1 and
/// above, six significant digits written out below 1 (never scientific notation).
pub fn money(value: Option<f64>, currency: &str) -> String {
    let value = match value {
        Some(v) if v.is_finite() => v,
        _ => return "?".to_string(),
    };
    let upper = currency.to_uppercase();
    let sign = CURRENCY_SIGNS.iter().find(|(k, _)| *k == upper).map(|(_, s)| *s).unwrap_or("");
    let mut digits =
        if value < 1000.0 || value != value.trunc() { py::grouped(value, 2) } else { py::grouped(value, 0) };
    if value >= 1.0 && digits.ends_with(".00") {
        digits.truncate(digits.len() - 3);
    } else if 0.0 < value && value < 1.0 {
        // six significant digits, written out: SHIB-USD near $0.00001 read "$1.234e-05" (review, 2026-09-25)
        let places = 2.max(6 - (value.log10().floor() as i64) - 1) as usize;
        digits = format!("{value:.places$}").trim_end_matches('0').trim_end_matches('.').to_string();
    }
    if !sign.is_empty() {
        format!("{sign}{digits}")
    } else if !currency.is_empty() {
        format!("{digits} {upper}")
    } else {
        digits
    }
}

/// The condition in words: "at or below $300", "a 10% drop", "\"phrase\" on the page", "in stock or on sale".
pub fn describe(w: &Watch) -> String {
    let (c, cur) = (w.condition.as_str(), w.currency.as_str());
    match c {
        "below" | "above" => format!("at or {c} {}", money(w.value, cur)),
        "drop_pct" | "rise_pct" => {
            // a percentage, never tiny
            let pct = w.value.filter(|v| *v != 0.0).unwrap_or(0.0);
            format!("a {}% {}", py::fmt_g(pct), if c == "drop_pct" { "drop" } else { "rise" })
        }
        "appears" => format!("\"{}\" on the page", w.text),
        "change" => "any price change".to_string(),
        "available" => "in stock or on sale".to_string(),
        other => other.to_string(),
    }
}

/// Fold one reading into the watch's state; the alert to text, or None. Edge-triggered: a condition that holds
/// fires once and re-arms only once it has stopped holding. A first reading shown in the add reply (`shown`)
/// never fires; it only sets the baseline and the arming (a price already under the mark waits for it to go back
/// over first). A first reading the owner never saw (the add's own check failed or was queued) fires when it
/// holds.
pub fn evaluate(w: &mut Watch, r: &Reading, shown: bool) -> Option<String> {
    let first = w.checks == 0;
    // the owner saw this first reading in the add reply; one they never saw can fire
    let quiet = first && shown;
    let (prev_value, prev_avail) = (w.last_value, w.last_available);
    w.checks += 1;
    let mut value = r.value;
    if !r.currency.is_empty() && !w.currency.is_empty() && r.currency != w.currency && r.value.is_some() {
        // A reading in another currency is no price (check() counts it as a failed read before it gets here)
        value = None;
    } else if !r.currency.is_empty() && r.value.is_some() {
        // only a priced reading sets it: a bare currency locked it in
        w.currency = r.currency.clone();
    }
    if !r.url.is_empty() {
        w.last_url = r.url.clone();
    }
    if !r.note.is_empty() {
        w.last_note = r.note.clone();
    }
    if value.is_some() {
        w.last_value = value;
    }
    if r.available.is_some() {
        w.last_available = r.available;
    }
    if r.hit.is_some() {
        w.last_hit = r.hit;
    }
    let cur = w.currency.clone();
    let mut alert: Option<String> = None;
    let c = w.condition.clone();

    match c.as_str() {
        "below" | "above" => {
            if let (Some(v), Some(mark)) = (value, w.value) {
                let holds = if c == "below" { v <= mark } else { v >= mark };
                if quiet {
                    w.armed = !holds;
                } else if holds && w.armed {
                    w.armed = false;
                    let word = if c == "below" { "dropped to" } else { "rose to" };
                    let was = prev_value.map(|p| format!(" (was {})", money(Some(p), &cur))).unwrap_or_default();
                    alert = Some(format!(
                        "{} {word} {}{was}, past your {} mark.",
                        w.label,
                        money(Some(v), &cur),
                        money(Some(mark), &cur)
                    ));
                } else if !holds {
                    w.armed = true;
                }
            }
        }
        "drop_pct" | "rise_pct" => {
            if let (Some(v), Some(pct)) = (value, w.value.filter(|p| *p != 0.0)) {
                match w.baseline {
                    Some(base) if !first && (base > 0.0 || base.is_nan()) => {
                        let moved =
                            if c == "drop_pct" { (base - v) / base * 100.0 } else { (v - base) / base * 100.0 };
                        // an exact move (3.00 -> 2.70 is 9.999...% in floats) counts
                        if moved >= pct - 1e-9 {
                            let word = if c == "drop_pct" { "down" } else { "up" };
                            alert = Some(format!(
                                "{} is {word} {moved:.1}% to {} (from {}).",
                                w.label,
                                money(Some(v), &cur),
                                money(Some(base), &cur)
                            ));
                            w.baseline = Some(v);
                        }
                    }
                    // a baseline of 0 is no baseline: nothing moves by a % from it
                    _ => w.baseline = Some(v),
                }
            }
        }
        "change" => {
            if !first {
                let flipped = match (r.available, prev_avail) {
                    (Some(a), Some(p)) => a != p,
                    _ => false,
                };
                let avail_words = |a: Option<bool>| if a == Some(true) { "available now" } else { "no longer available" };
                match (value, prev_value) {
                    (Some(v), Some(p)) if (v - p).abs() > 1e-9 => {
                        let word = if v < p { "down" } else { "up" };
                        let mut text =
                            format!("{} changed: {}, {word} from {}.", w.label, money(Some(v), &cur), money(Some(p), &cur));
                        if flipped {
                            text.push_str(&format!(" It is {}.", avail_words(r.available)));
                        }
                        alert = Some(text);
                    }
                    _ => {
                        if flipped {
                            alert = Some(format!("{} is {}.", w.label, avail_words(r.available)));
                        }
                    }
                }
            }
        }
        "available" => {
            if let Some(available) = r.available {
                if quiet {
                    w.armed = !available;
                } else if available && w.armed {
                    w.armed = false;
                    let price = value.map(|v| format!(" From {}.", money(Some(v), &cur))).unwrap_or_default();
                    alert = Some(format!("{} is available now.{price}", w.label));
                } else if !available {
                    w.armed = true;
                }
            }
        }
        "appears" => {
            if let Some(hit) = r.hit {
                if quiet {
                    w.armed = !hit;
                } else if hit && w.armed {
                    w.armed = false;
                    alert = Some(format!("\"{}\" is now on the page for {}.", w.text, w.label));
                } else if !hit {
                    w.armed = true;
                }
            }
        }
        _ => {}
    }
    if let Some(text) = alert.as_mut() {
        let link = if w.kind == "page" { w.target.clone() } else { w.last_url.clone() };
        if !r.note.is_empty() && (w.kind == "search" || w.kind == "ticketmaster" || w.via == "search") {
            text.push(' ');
            text.push_str(&plain_note(&r.note));
        }
        if !link.is_empty() {
            text.push('\n');
            text.push_str(&link);
        }
        w.fired += 1;
    }
    alert
}

/// The latest reading in words, for the chat and /watches.
pub fn now_line(w: &Watch) -> String {
    let mut bits: Vec<String> = Vec::new();
    if let Some(v) = w.last_value {
        let note = if w.kind == "quote" && !w.last_note.is_empty() { format!(" ({})", w.last_note) } else { String::new() };
        bits.push(money(Some(v), &w.currency) + &note);
    }
    if let Some(a) = w.last_available {
        bits.push(if a { "available" } else { "not available" }.to_string());
    }
    if w.condition == "appears" {
        if let Some(hit) = w.last_hit {
            bits.push(if hit { "phrase present" } else { "phrase not there" }.to_string());
        }
    }
    if bits.is_empty() {
        "no reading yet".to_string()
    } else {
        bits.join(", ")
    }
}

/// `_alert_about`: what the chat's history keeps of an alert: built by code from the watch, no word from the web.
pub fn alert_about(w: &Watch) -> String {
    format!("(I texted a watch alert: {}, {}, {}.)", w.id, w.label, describe(w))
}

/// `_hours(raw)`: a period in hours from a tool call: a positive number up to MAX_PERIOD_HOURS, or None for none;
/// the second part is why not ("" when fine).
pub fn hours(raw: &Value) -> (Option<f64>, String) {
    let n = match raw {
        Value::Null => return (None, String::new()),
        Value::Number(n) => n.as_f64(),
        _ => None,
    };
    match n {
        Some(h) if h.is_finite() && h > 0.0 => {
            if h > MAX_PERIOD_HOURS {
                (None, "a period of at most a year".to_string())
            } else {
                (Some(h), String::new())
            }
        }
        _ => (None, "the period must be a positive number of hours".to_string()),
    }
}

/// `_when(t)`: a local time for the owner: "Fri Sep 26, 3:00 PM" ("later" when it cannot be shown).
///
/// Python's `datetime.fromtimestamp(t).astimezone()` refuses a local time in the first or the last day of the
/// calendar (it probes a day either side for a fold: "year 10000 is out of range" for 9999-12-31, measured on
/// CPython 3.13, 2026-10-20); those are "later" here too.
pub fn when(t: f64) -> String {
    match py::fromtimestamp(t) {
        Some(local) if (*WHEN_FIRST..=*WHEN_LAST).contains(&local) => {
            local.format("%a %b %d, %I:%M %p").to_string().replace(" 0", " ")
        }
        _ => "later".to_string(),
    }
}

static WHEN_FIRST: LazyLock<NaiveDateTime> = LazyLock::new(|| {
    NaiveDate::from_ymd_opt(1, 1, 2).and_then(|d| d.and_hms_opt(0, 0, 0)).expect("0001-01-02")
});
static WHEN_LAST: LazyLock<NaiveDateTime> = LazyLock::new(|| {
    NaiveDate::from_ymd_opt(9999, 12, 30).and_then(|d| d.and_hms_micro_opt(23, 59, 59, 999_999)).expect("9999-12-30")
});

/// `_stretch(errors)`: how far a failing watch's interval is stretched: 1, 2, 4 … up to MAX_ERROR_STRETCH. The
/// exponent is capped: 2.0 ** 1024 overflowed a float at the 1025th failure in a row and stopped the tick
/// (re-verification, 2026-09-25).
pub fn stretch(errors: i64) -> f64 {
    (MAX_ERROR_STRETCH as f64).min(2.0_f64.powi(errors.saturating_sub(1).clamp(0, 16) as i32))
}

/// `_every_words(secs)`: "15 min", "6 h", "1.5 h", "7 d".
pub fn every_words(secs: i64) -> String {
    if secs < 3600 {
        return format!("{} min", (secs as f64 / 60.0).round_ties_even() as i64);
    }
    if secs < 86400 {
        let h = secs as f64 / 3600.0;
        return if h == h.trunc() { format!("{} h", py::fmt_g(h)) } else { format!("{h:.1} h") };
    }
    format!("{} d", py::fmt_g(secs as f64 / 86400.0))
}

/// Python's own semantics where watch.py leans on them: `str.split()` / `strip()` whitespace, truthiness, `str()`
/// of a JSON value, float `repr` and `format(x, "g")`, `round(x, n)`, `"{:,.Nf}"`, and `datetime.fromtimestamp`.
pub(crate) mod py {
    use chrono::{Datelike, Local, NaiveDateTime, TimeZone};
    use serde_json::{Map, Value};

    /// `str.isspace` for one character: Unicode White_Space plus the four information separators (U+001C-1F).
    pub fn isspace(c: char) -> bool {
        c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
    }

    /// `s.strip()`.
    pub fn strip(s: &str) -> &str {
        s.trim_matches(isspace)
    }

    /// `" ".join(s.split())`.
    pub fn split_join(s: &str) -> String {
        s.split(isspace).filter(|p| !p.is_empty()).collect::<Vec<_>>().join(" ")
    }

    /// `s[:n]`, in characters.
    pub fn take(s: &str, n: usize) -> String {
        s.chars().take(n).collect()
    }

    /// `bool(v)`.
    pub fn truthy(v: &Value) -> bool {
        match v {
            Value::Null => false,
            Value::Bool(b) => *b,
            Value::Number(n) => n.as_f64().is_some_and(|x| x != 0.0),
            Value::String(s) => !s.is_empty(),
            Value::Array(a) => !a.is_empty(),
            Value::Object(o) => !o.is_empty(),
        }
    }

    /// `str(v)` of a value json.loads made.
    pub fn str_of(v: &Value) -> String {
        match v {
            Value::String(s) => s.clone(),
            other => repr(other),
        }
    }

    /// `str(v or "")`.
    pub fn str_or_empty(v: Option<&Value>) -> String {
        match v {
            Some(v) if truthy(v) => str_of(v),
            _ => String::new(),
        }
    }

    /// `(v or {})` as a dict: a falsy value is the empty dict, a dict itself; anything else (where Python would
    /// raise AttributeError at the next `.get`) reads as empty too.
    pub fn obj(v: Option<&Value>) -> Option<&Map<String, Value>> {
        v.and_then(Value::as_object)
    }

    /// `(v or [])` as a list: anything but a list reads as empty.
    pub fn list(v: Option<&Value>) -> &[Value] {
        match v {
            Some(Value::Array(a)) => a,
            _ => &[],
        }
    }

    /// `v` when `isinstance(v, (int, float))` (a bool is an int there), as a float.
    pub fn number(v: Option<&Value>) -> Option<f64> {
        match v {
            Some(Value::Number(n)) => n.as_f64(),
            Some(Value::Bool(b)) => Some(if *b { 1.0 } else { 0.0 }),
            _ => None,
        }
    }

    /// `repr(v)` of a value json.loads made.
    pub fn repr(v: &Value) -> String {
        match v {
            Value::Null => "None".to_string(),
            Value::Bool(true) => "True".to_string(),
            Value::Bool(false) => "False".to_string(),
            Value::Number(n) => {
                if n.is_f64() {
                    float_repr(n.as_f64().unwrap_or(f64::NAN))
                } else {
                    n.to_string()
                }
            }
            Value::String(s) => repr_str(s),
            Value::Array(a) => format!("[{}]", a.iter().map(repr).collect::<Vec<_>>().join(", ")),
            Value::Object(o) => format!(
                "{{{}}}",
                o.iter().map(|(k, v)| format!("{}: {}", repr_str(k), repr(v))).collect::<Vec<_>>().join(", ")
            ),
        }
    }

    /// `repr(s)` of a str: single quotes unless the text has one and no double quote.
    pub fn repr_str(s: &str) -> String {
        let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
        let mut out = String::with_capacity(s.len() + 2);
        out.push(quote);
        for c in s.chars() {
            match c {
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if c == quote => {
                    out.push('\\');
                    out.push(c);
                }
                c if (c as u32) < 0x20 || (0x7f..0xa0).contains(&(c as u32)) => {
                    out.push_str(&format!("\\x{:02x}", c as u32));
                }
                c => out.push(c),
            }
        }
        out.push(quote);
        out
    }

    /// The shortest round-trip digits of a finite, non-zero `x` and its decimal exponent: `x = d.ddd × 10^exp`.
    fn shortest(x: f64) -> (String, i32) {
        let s = format!("{:e}", x.abs());
        let (mantissa, exp) = s.split_once('e').unwrap_or((&s, "0"));
        (mantissa.replace('.', ""), exp.parse().unwrap_or(0))
    }

    /// `repr(x)` of a float: "12.0", "0.0001", "1e-05", "1e+16".
    pub fn float_repr(x: f64) -> String {
        if x.is_nan() {
            return "nan".to_string();
        }
        if x.is_infinite() {
            return if x > 0.0 { "inf" } else { "-inf" }.to_string();
        }
        let sign = if x.is_sign_negative() { "-" } else { "" };
        if x == 0.0 {
            return format!("{sign}0.0");
        }
        let (digits, exp) = shortest(x);
        let body = if (-4..16).contains(&exp) {
            if exp >= 0 {
                let int_len = exp as usize + 1;
                if digits.len() <= int_len {
                    format!("{}{}.0", digits, "0".repeat(int_len - digits.len()))
                } else {
                    format!("{}.{}", &digits[..int_len], &digits[int_len..])
                }
            } else {
                format!("0.{}{}", "0".repeat((-exp - 1) as usize), digits)
            }
        } else {
            let mantissa = if digits.len() > 1 { format!("{}.{}", &digits[..1], &digits[1..]) } else { digits };
            format!("{mantissa}e{}{:02}", if exp < 0 { '-' } else { '+' }, exp.abs())
        };
        format!("{sign}{body}")
    }

    /// `format(x, "g")`: six significant digits, trailing zeros dropped, scientific below 1e-4 and from 1e6.
    pub fn fmt_g(x: f64) -> String {
        if x.is_nan() {
            return "nan".to_string();
        }
        if x.is_infinite() {
            return if x > 0.0 { "inf" } else { "-inf" }.to_string();
        }
        if x == 0.0 {
            return if x.is_sign_negative() { "-0" } else { "0" }.to_string();
        }
        let sci = format!("{x:.5e}");
        let (mantissa, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
        let exp: i32 = exp.parse().unwrap_or(0);
        if (-4..6).contains(&exp) {
            let places = (5 - exp) as usize;
            strip_zeros(&format!("{x:.places$}"))
        } else {
            format!("{}e{}{:02}", strip_zeros(mantissa), if exp < 0 { '-' } else { '+' }, exp.abs())
        }
    }

    fn strip_zeros(s: &str) -> String {
        if s.contains('.') {
            s.trim_end_matches('0').trim_end_matches('.').to_string()
        } else {
            s.to_string()
        }
    }

    /// `round(x, n)` of a float: the nearest multiple of 10^-n, ties to even on the exact binary value (Rust's
    /// fixed-precision formatting rounds the same way), read back.
    pub fn round_ndigits(x: f64, n: usize) -> f64 {
        if !x.is_finite() {
            return x;
        }
        format!("{x:.n$}").parse().unwrap_or(x)
    }

    /// `f"{x:,.{n}f}"`: fixed point, the integer part grouped by commas.
    pub fn grouped(x: f64, n: usize) -> String {
        let s = format!("{x:.n$}");
        let (sign, rest) = match s.strip_prefix('-') {
            Some(r) => ("-", r),
            None => ("", s.as_str()),
        };
        let (int, frac) = match rest.split_once('.') {
            Some((i, f)) => (i, Some(f)),
            None => (rest, None),
        };
        let mut out = String::with_capacity(int.len() + int.len() / 3 + 4);
        for (i, ch) in int.chars().enumerate() {
            if i > 0 && (int.len() - i) % 3 == 0 {
                out.push(',');
            }
            out.push(ch);
        }
        match frac {
            Some(f) => format!("{sign}{out}.{f}"),
            None => format!("{sign}{out}"),
        }
    }

    /// `datetime.fromtimestamp(t)`: the local wall time, rounded to the microsecond (half to even, as CPython
    /// does), or None where Python raises (not finite, past the platform's range, a year outside 1-9999).
    pub fn fromtimestamp(t: f64) -> Option<NaiveDateTime> {
        if !t.is_finite() {
            return None;
        }
        let mut whole = t.trunc();
        let mut us = ((t - whole) * 1e6).round_ties_even();
        if us >= 1e6 {
            whole += 1.0;
            us -= 1e6;
        } else if us < 0.0 {
            whole -= 1.0;
            us += 1e6;
        }
        if whole.abs() > 1e15 {
            return None;
        }
        let local = Local.timestamp_opt(whole as i64, (us as u32) * 1000).earliest()?.naive_local();
        (1..=9999).contains(&local.year()).then_some(local)
    }
}
