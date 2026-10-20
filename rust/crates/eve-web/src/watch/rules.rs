//! watch.py's "a watch and its conditions" section: the stored watch, its edge-triggered rule, and the words the
//! owner reads.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::page::Reading;

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
        todo!("watch.Watch.to_json")
    }

    /// A stored watch, defensively: an unknown key is dropped, a field of the wrong type goes back to its default,
    /// a number that is not finite (or past 1e15) is none, an id is a lowercase wN; the label is collapsed (else
    /// the target), the counters clamped, `via` one of the known rungs, `every_secs` inside its floor and
    /// MAX_EVERY_SECS. None for anything that is not a watch.
    pub fn from_json(raw: &Value) -> Option<Watch> {
        let _ = raw;
        todo!("watch.Watch.from_json")
    }
}

/// Yahoo quotes some exchanges in the minor unit: pence, cents, agorot (yfinance's history.py lists these three).
pub const MINOR_UNITS: [(&str, &str); 4] = [("GBp", "GBP"), ("GBX", "GBP"), ("ZAc", "ZAR"), ("ILA", "ILS")];
/// `_OPTIONAL_FLOATS`.
pub const OPTIONAL_FLOATS: [&str; 3] = ["value", "last_value", "baseline"];
/// `_OPTIONAL_BOOLS`.
pub const OPTIONAL_BOOLS: [&str; 2] = ["last_available", "last_hit"];

/// A chart meta block as a reading: the regular-session price (what a mark is set against), in the major unit,
/// and outside the regular session a note with the extended-hours price when Yahoo has a different one
/// (`fulldayPrice`, undocumented).
pub fn quote_reading(meta: &Map<String, Value>, price: f64, now: f64) -> Reading {
    let _ = (meta, price, now);
    todo!("watch.quote_reading")
}

/// A note from outside (a model, a web page, Ticketmaster's event names) as plain words: whitespace collapsed, at
/// most NOTE_CHARS, `[` and `]` become `(` and `)`.
pub fn plain_note(note: &str) -> String {
    let _ = note;
    todo!("watch.plain_note")
}

/// "$1,299", "$0.00001234", "85 CHF", "?" for none: the currency's sign, thousands grouped, ".00" dropped at 1 and
/// above, six significant digits written out below 1 (never scientific notation).
pub fn money(value: Option<f64>, currency: &str) -> String {
    let _ = (value, currency);
    todo!("watch.money")
}

/// The condition in words: "at or below $300", "a 10% drop", "\"phrase\" on the page", "in stock or on sale".
pub fn describe(w: &Watch) -> String {
    let _ = w;
    todo!("watch.describe")
}

/// Fold one reading into the watch's state; the alert to text, or None. Edge-triggered: a condition that holds
/// fires once and re-arms only once it has stopped holding. A first reading shown in the add reply (`shown`)
/// never fires; it only sets the baseline and the arming. A first reading the owner never saw fires when it
/// holds.
pub fn evaluate(w: &mut Watch, r: &Reading, shown: bool) -> Option<String> {
    let _ = (w, r, shown);
    todo!("watch.evaluate")
}

/// The latest reading in words, for the chat and /watches.
pub fn now_line(w: &Watch) -> String {
    let _ = w;
    todo!("watch.now_line")
}

/// `_alert_about`: what the chat's history keeps of an alert: built by code from the watch, no word from the web.
pub fn alert_about(w: &Watch) -> String {
    let _ = w;
    todo!("watch._alert_about")
}

/// `_hours(raw)`: a period in hours from a tool call: a positive number up to MAX_PERIOD_HOURS, or None for none;
/// the second part is why not ("" when fine).
pub fn hours(raw: &Value) -> (Option<f64>, String) {
    let _ = raw;
    todo!("watch._hours")
}

/// `_when(t)`: a local time for the owner: "Fri Sep 26, 3:00 PM" ("later" when it cannot be shown).
pub fn when(t: f64) -> String {
    let _ = t;
    todo!("watch._when")
}

/// `_stretch(errors)`: how far a failing watch's interval is stretched: 1, 2, 4 … up to MAX_ERROR_STRETCH.
pub fn stretch(errors: i64) -> f64 {
    let _ = errors;
    todo!("watch._stretch")
}

/// `_every_words(secs)`: "15 min", "6 h", "1.5 h", "7 d".
pub fn every_words(secs: i64) -> String {
    let _ = secs;
    todo!("watch._every_words")
}
