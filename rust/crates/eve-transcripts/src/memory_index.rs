//! Search memory by meaning and by words: a local index over the transcripts, and the three search switches.
//!
//! The memory plan (2026-10-08, items 1, 4 and 5) names three changes to `memory_search`. Each ships behind its own
//! switch, off in code until the memory eval (`tools/memory_eval.py`) clears the pre-registered bar for it:
//!
//! - `EVE_MEMORY_HYBRID=1`: the transcript part of a search is ranked, not filtered. Before, every word of the query
//!   had to be in a line ("keyword AND"), so "landlord name" missed "Carla, my landlord, says ...". Now each round of
//!   a conversation (one owner line and eve's reply to it) is one unit in a local SQLite file, with an FTS5 (BM25)
//!   row and a vector from the same embeddings model mem0 uses. A search scores every unit in its days as
//!   `0.3 * BM25 (divided by the best BM25 of the search) + 0.7 * cosine` and returns the best 8, each with the line
//!   before it and the reply. When the index or the embeddings call is not there, the search is the keyword AND it
//!   always was.
//! - `EVE_MEMORY_TIMEARGS=1`: `memory_search` takes `from` and `to`. Each is a day (year-month-day) or the owner's
//!   own words for a time ("yesterday", "last week", "in May", "two weeks ago"); `resolve_when` turns the words into
//!   days in code, on the owner's day (it starts at 04:00). A range that finds nothing is widened once.
//! - `EVE_MEMORY_RETRY=1`: a search that finds nothing tries again by itself over a whole year (and, without the
//!   index, with fewer of the words), and says to search once more with other words before answering "I don't have
//!   that". The Telegram brain allows at most three memory calls in one turn (telegram).
//!
//! ### The index file
//!
//! `<memory>/index/memory.sqlite3` (0600, in a 0700 folder, under the same guards as the transcripts). Tables:
//! `units` (one row per round: day, conversation, time, channel, the opening line, the reply, the line before, the
//! hash of its text), `fts` (the text, BM25), `vectors` (hash → float32 vector) and `days` (each day file's size,
//! mtime and inode when it was indexed). A day whose file changed is indexed again before it is searched, so a line
//! appended a moment ago, or a line the forget CLI redacted in another process, is never stale. Vectors are kept by
//! the hash of the text: indexing a day again embeds only the rounds that changed.
//!
//! New lines reach the index off the event loop: `Transcripts::append` calls `notify` (a set and an Event), and a
//! worker thread indexes the day 1.5 s after the last line. It embeds only after EMBED_IDLE_SECS with no new line,
//! so a round that is still growing is not sent to the embeddings endpoint once per line; until then the new round
//! is found by its words. Every refresh that indexed a day again drops the vectors no round uses. Only the daemon
//! runs the worker (`Memory(background=True)`); the forget, dream and reindex commands open the file without it. A
//! missing file is built again in the background when the daemon opens memory; until it is ready, a search is the
//! keyword AND.
//!
//! ### Forget reaches it
//!
//! `forget` deletes every unit whose full stored text (the opening line, every reply line, the line before) matches,
//! its FTS row and every vector no unit uses any more. Then every indexed day whose file changed is indexed again
//! from the redacted transcripts (so words a match could not see, like a round redacted by its own line, go too),
//! and the FTS segments are rewritten and the file VACUUMed, every time, with `secure_delete` on and FTS5's own
//! secure-delete on, so the words are not left in free pages or old segments. A step that fails after retries
//! raises `IndexForgetError`: the forget reports the index as failed, never as done. The forget opens the file
//! whenever it exists, with the hybrid switch on or off (`Memory._ranked_for_forget`).
//!
//! ### What leaves the machine
//!
//! The text of each round and each query goes to OpenAI's embeddings endpoint (`text-embedding-3-small`, 512
//! dimensions), as mem0's already does. The endpoint has no `store` flag. Logs carry counts and milliseconds only.

use std::borrow::Borrow;
use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, LazyLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use chrono::{Datelike, NaiveDate};
use parking_lot::{Condvar, Mutex, RwLock};
use regex::Regex;
use rusqlite::{params, params_from_iter, Connection, ErrorCode, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tracing::{info, warn};

use crate::py;
use crate::transcripts::{Dt, Line};

pub const HYBRID_ENV: &str = "EVE_MEMORY_HYBRID";
pub const TIMEARGS_ENV: &str = "EVE_MEMORY_TIMEARGS";
pub const RETRY_ENV: &str = "EVE_MEMORY_RETRY";
/// The three switches, by key, in this order.
pub const SWITCHES: [(&str, &str); 3] = [
    ("hybrid", HYBRID_ENV),
    ("timeargs", TIMEARGS_ENV),
    ("retry", RETRY_ENV),
];
/// Each switch's value when its variable is not set: the memory eval's holdout verdict
/// (tests/fixtures/memory_eval/VERDICT.md, 2026-10-08). The verdict test keeps the two equal.
pub const DEFAULTS: [(&str, bool); 3] = [("hybrid", false), ("timeargs", false), ("retry", false)];

pub const INDEX_SUBDIR: &str = "index";
pub const DB_FILE: &str = "memory.sqlite3";
pub const INDEX_VERSION: i64 = 1; // bump when a unit's text or the tables change: the file is built again
pub const EMBED_MODEL: &str = "text-embedding-3-small"; // mem0_memory.DEFAULT_EMBED_MODEL
pub const EMBED_DIMS: usize = 512;
pub const EMBED_BATCH: i64 = 256;
pub const EMBED_TIMEOUT_SECS: f64 = 20.0; // indexing, off the loop
pub const QUERY_TIMEOUT_SECS: f64 = 2.0; // one query's vector; past this the search is the keyword AND
pub const BM25_WEIGHT: f64 = 0.3;
pub const COS_WEIGHT: f64 = 0.7;
pub const TOP_K: usize = 8;
pub const MIN_COS: f64 = 0.30; // a unit with no word in common must be at least this close to count as found
pub const FTS_CANDIDATES: i64 = 300;
pub const UNIT_CHARS: usize = 2000; // one unit's text, for FTS and the embedding
pub const HIT_CHARS: usize = 300;
pub const NEIGHBOUR_CHARS: usize = 200;
pub const REPLY_CHARS: usize = 300;
pub const DEBOUNCE_SECS: f64 = 1.5;
pub const EMBED_IDLE_SECS: f64 = 30.0; // the worker embeds new rounds after this long with no new line
pub const COMPACT_WAITS: [f64; 4] = [0.0, 0.5, 1.0, 2.0]; // forget's optimize and VACUUM: tried again after these
pub const DAY_START_HOUR: i64 = 4;
pub const FORGOTTEN: &str = "(forgotten)";

const STOPWORDS_TEXT: &str = "
a about after again all also am an and any are as at be been before being but by can could did do does doing done
for from had has have having he her him his how i if in into is it its just me my myself no not of on or our ours
out over she so some than that the their them then there these they this those to too up us was we were what when
where which while who whom why will with would you your yours yourself did say said tell told ever much many
";

pub static STOPWORDS: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| STOPWORDS_TEXT.split_whitespace().collect());

// Python's `\w` on str: letters, numbers (any `N`) and `_`; never a combining mark, which Rust's `\w` takes.
static WORD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\p{L}\p{N}_]+").unwrap());

/// One of the three search switches (`SWITCHES`: a key or the variable's name). On when set to 1/true/yes/on, off
/// when set to anything else, and `DEFAULTS` when not set (or empty). `None` reads the process environment.
pub fn switch(name: &str, environ: Option<&HashMap<String, String>>) -> bool {
    let var = SWITCHES
        .iter()
        .find(|(k, _)| *k == name)
        .map(|(_, v)| *v)
        .unwrap_or(name);
    let key = SWITCHES
        .iter()
        .find(|(_, v)| *v == var)
        .map(|(k, _)| *k)
        .unwrap_or(name);
    let raw = match environ {
        Some(env) => env.get(var).cloned().unwrap_or_default(),
        None => std::env::var(var).unwrap_or_default(),
    };
    let raw = py::strip(&raw).to_lowercase();
    if raw.is_empty() {
        return DEFAULTS
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, d)| *d)
            .unwrap_or(false);
    }
    matches!(raw.as_str(), "1" | "true" | "yes" | "on")
}

// ---- time: the owner's words to a range of days ---------------------------------------------------------------------

const MONTH_NAMES: [&str; 12] = [
    "january",
    "february",
    "march",
    "april",
    "may",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
];
const MONTH_ABBRS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];
const WEEKDAYS: [&str; 7] = [
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
    "saturday",
    "sunday",
];
const NUM: &str = r"(\d+|a couple of|a couple|a few|an|a|one|two|three|four|five|six|seven|eight|nine|ten|eleven|twelve)";

/// `_MONTHS`: every month name and abbreviation (and "sept"), lowercase, in dict order.
static MONTHS: LazyLock<Vec<(&'static str, u32)>> = LazyLock::new(|| {
    let mut out: Vec<(&'static str, u32)> = Vec::new();
    for (i, m) in MONTH_NAMES.iter().chain(MONTH_ABBRS.iter()).enumerate() {
        if !out.iter().any(|(k, _)| k == m) {
            out.push((m, (i % 12) as u32 + 1));
        }
    }
    out.push(("sept", 9));
    out
});

fn month_of(name: &str) -> Option<u32> {
    MONTHS.iter().find(|(k, _)| *k == name).map(|(_, v)| *v)
}

/// `_MON`: the month alternation, longest first.
static MON: LazyLock<String> = LazyLock::new(|| {
    let mut names: Vec<&str> = MONTHS.iter().map(|(k, _)| *k).collect();
    names.sort_by_key(|n| std::cmp::Reverse(n.len()));
    names.join("|")
});

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("a valid time pattern")
}

static LEAD: LazyLock<Regex> = LazyLock::new(|| re(r"^(on|in|during|from|at|around|over|for)\s+"));
static THE: LazyLock<Regex> = LazyLock::new(|| re(r"^the\s+"));
static SINCE: LazyLock<Regex> = LazyLock::new(|| re(r"^(since|after)\s+(.*)$"));
static ISO_DAY: LazyLock<Regex> = LazyLock::new(|| re(r"^(\d{4})-(\d{1,2})-(\d{1,2})$"));
static DAYS_AGO: LazyLock<Regex> = LazyLock::new(|| re(&format!(r"^{NUM}\s+days?\s+ago$")));
static WEEKS_AGO: LazyLock<Regex> = LazyLock::new(|| re(&format!(r"^{NUM}\s+weeks?\s+ago$")));
static PAST_N: LazyLock<Regex> =
    LazyLock::new(|| re(&format!(r"^(?:past|last)\s+{NUM}\s+(days?|weeks?)$")));
static WEEKDAY: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(
        r"^(last|this|past)?\s*({})(?:\s+(?:morning|afternoon|evening|night))?$",
        WEEKDAYS.join("|")
    ))
});
static MONTHS_AGO: LazyLock<Regex> = LazyLock::new(|| re(&format!(r"^{NUM}\s+months?\s+ago$")));
static MONTH_DAY: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(
        r"^({})\.?\s+(\d{{1,2}})(?:st|nd|rd|th)?(?:\s+(\d{{4}}))?$",
        *MON
    ))
});
static DAY_MONTH: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(
        r"^(\d{{1,2}})(?:st|nd|rd|th)?\s+(?:of\s+)?({})\.?(?:\s+(\d{{4}}))?$",
        *MON
    ))
});
static MONTH_PART: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(
        r"^(?:(early|mid|late|end of|the end of|beginning of|the beginning of|start of)\s+)?({})\.?(?:\s+(\d{{4}}))?$",
        *MON
    ))
});

/// The owner's day at `now`: it starts at 04:00, so 01:00 belongs to the evening before.
pub fn owner_day(now: Dt) -> NaiveDate {
    (now - chrono::Duration::hours(DAY_START_HOUR)).date_naive()
}

fn num(word: &str) -> Option<i64> {
    let w = py::strip(word).to_lowercase();
    if !w.is_empty() && w.chars().all(|c| py::int(&c.to_string()).is_some()) {
        return py::int(&w);
    }
    Some(match w.as_str() {
        "a" | "an" | "one" => 1,
        "two" | "couple" | "a couple" | "a couple of" => 2,
        "three" | "few" | "a few" => 3,
        "four" => 4,
        "five" => 5,
        "six" => 6,
        "seven" => 7,
        "eight" => 8,
        "nine" => 9,
        "ten" => 10,
        "eleven" => 11,
        "twelve" => 12,
        _ => 1,
    })
}

fn date(year: i64, month: u32, day: u32) -> Option<NaiveDate> {
    if !(1..=9999).contains(&year) {
        return None;
    }
    NaiveDate::from_ymd_opt(year as i32, month, day)
}

fn days_back(d: NaiveDate, n: i64) -> Option<NaiveDate> {
    let out = d.checked_sub_signed(chrono::Duration::try_days(n)?)?;
    (out.year() >= 1).then_some(out)
}

fn days_on(d: NaiveDate, n: i64) -> Option<NaiveDate> {
    let out = d.checked_add_signed(chrono::Duration::try_days(n)?)?;
    (out.year() <= 9999).then_some(out)
}

fn month_range(year: i64, month: u32) -> Option<(NaiveDate, NaiveDate)> {
    let first = date(year, month, 1)?;
    let (ny, nm) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    let last = NaiveDate::from_ymd_opt(ny as i32, nm, 1)?.pred_opt()?;
    Some((first, last))
}

fn shift_month(year: i64, month: u32, back: i64) -> (i64, u32) {
    let n = year * 12 + (month as i64 - 1) - back;
    (n.div_euclid(12), (n.rem_euclid(12) + 1) as u32)
}

/// month/day in `year`, or with no year the latest one on or before today.
fn past_date(today: NaiveDate, month: u32, day: u32, year: Option<i64>) -> Option<NaiveDate> {
    if let Some(y) = year {
        return date(y, month, day);
    }
    let d = date(today.year() as i64, month, day)?;
    if d <= today {
        Some(d)
    } else {
        date(today.year() as i64 - 1, month, day)
    }
}

/// The first and last day the owner's words for a time mean, at `now` on the owner's clock (a day starts at 04:00),
/// or None when the words are not a time this knows. Never a day after today.
///
/// The set it knows: a day (2026-09-03, "September 3", "3 Sept 2026"); today, tonight, this morning; yesterday,
/// last night, the day before yesterday; N days ago; N weeks ago (that Monday-to-Sunday week); this week, last week,
/// the past week; this weekend, last weekend; a weekday ("on Tuesday", "last Tuesday"); a month ("in May", "early
/// May", "late May", "May 2026"); this month, last month, N months ago; this year, last year; and "since" any of
/// these (from its first day to today).
pub fn resolve_when(text: &str, now: Dt) -> Option<(NaiveDate, NaiveDate)> {
    let raw = py::flat(&text.to_lowercase().replace(',', " "));
    if raw.is_empty() {
        return None;
    }
    let today = owner_day(now);
    let s = LEAD.replacen(&raw, 1, "");
    let s = py::strip(&s).to_string();
    let mut s = THE.replacen(&s, 1, "").into_owned();
    let mut since = false;
    if let Some(m) = SINCE.captures(&s) {
        let rest = THE.replacen(&m[2], 1, "").into_owned();
        since = true;
        s = rest;
    }
    let (mut lo, mut hi) = resolve_core(&s, today)?;
    if since {
        hi = today;
    }
    hi = hi.min(today);
    if lo > hi {
        lo = hi;
    }
    Some((lo, hi))
}

fn resolve_core(s: &str, today: NaiveDate) -> Option<(NaiveDate, NaiveDate)> {
    let wd = today.weekday().num_days_from_monday() as i64;
    if let Some(m) = ISO_DAY.captures(s) {
        let d = date(
            py::int(&m[1])?,
            u32::try_from(py::int(&m[2])?).ok()?,
            u32::try_from(py::int(&m[3])?).ok()?,
        )?;
        return Some((d, d));
    }
    if [
        "today",
        "this morning",
        "this afternoon",
        "this evening",
        "tonight",
        "earlier today",
        "now",
    ]
    .contains(&s)
    {
        return Some((today, today));
    }
    if [
        "yesterday",
        "last night",
        "yesterday morning",
        "yesterday afternoon",
        "yesterday evening",
    ]
    .contains(&s)
    {
        let d = days_back(today, 1)?;
        return Some((d, d));
    }
    if s == "day before yesterday" {
        let d = days_back(today, 2)?;
        return Some((d, d));
    }
    if let Some(m) = DAYS_AGO.captures(s) {
        let d = days_back(today, num(&m[1])?)?;
        return Some((d, d));
    }
    if let Some(m) = WEEKS_AGO.captures(s) {
        let d = days_back(today, 7i64.checked_mul(num(&m[1])?)?)?;
        let mon = days_back(d, d.weekday().num_days_from_monday() as i64)?;
        return Some((mon, days_on(mon, 6)?));
    }
    if s == "this week" || s == "earlier this week" {
        return Some((days_back(today, wd)?, today));
    }
    if s == "last week" {
        let mon = days_back(today, wd + 7)?;
        return Some((mon, days_on(mon, 6)?));
    }
    if let Some(m) = PAST_N.captures(s) {
        let n = num(&m[1])?.checked_mul(if m[2].starts_with("week") { 7 } else { 1 })?;
        return Some((days_back(today, n)?, today));
    }
    let recent = [
        "past week",
        "past 7 days",
        "last 7 days",
        "past few days",
        "last few days",
        "last couple of days",
        "past couple of days",
        "recently",
        "lately",
    ];
    if recent.contains(&s) {
        let n = if s.contains("week") || s.contains('7') || s == "recently" || s == "lately" {
            7
        } else {
            3
        };
        return Some((days_back(today, n)?, today));
    }
    if ["weekend", "this weekend", "last weekend", "past weekend"].contains(&s) {
        let mut sat = days_back(today, (wd - 5).rem_euclid(7))?;
        if s == "last weekend" && (wd == 5 || wd == 6) {
            sat = days_back(sat, 7)?;
        }
        return Some((sat, days_on(sat, 1)?));
    }
    if let Some(m) = WEEKDAY.captures(s) {
        let target = WEEKDAYS.iter().position(|d| *d == &m[2])? as i64;
        let mut back = (wd - target).rem_euclid(7);
        if m.get(1).map(|g| g.as_str()) == Some("last") && back == 0 {
            back = 7;
        }
        let d = days_back(today, back)?;
        return Some((d, d));
    }
    if s == "this month" || s == "earlier this month" {
        return Some((date(today.year() as i64, today.month(), 1)?, today));
    }
    if s == "last month" {
        let (y, m) = shift_month(today.year() as i64, today.month(), 1);
        return month_range(y, m);
    }
    if let Some(m) = MONTHS_AGO.captures(s) {
        let (y, mo) = shift_month(today.year() as i64, today.month(), num(&m[1])?);
        return month_range(y, mo);
    }
    if s == "this year" {
        return Some((date(today.year() as i64, 1, 1)?, today));
    }
    if s == "last year" {
        let y = today.year() as i64 - 1;
        return Some((date(y, 1, 1)?, date(y, 12, 31)?));
    }
    if let Some(m) = MONTH_DAY.captures(s) {
        let year = m
            .get(3)
            .map(|y| py::int(y.as_str()))
            .map(|y| y.ok_or(()))
            .transpose()
            .ok()?;
        let d = past_date(today, month_of(&m[1])?, u32::try_from(py::int(&m[2])?).ok()?, year)?;
        return Some((d, d));
    }
    if let Some(m) = DAY_MONTH.captures(s) {
        let year = m
            .get(3)
            .map(|y| py::int(y.as_str()))
            .map(|y| y.ok_or(()))
            .transpose()
            .ok()?;
        let d = past_date(today, month_of(&m[2])?, u32::try_from(py::int(&m[1])?).ok()?, year)?;
        return Some((d, d));
    }
    if let Some(m) = MONTH_PART.captures(s) {
        let month = month_of(&m[2])?;
        let year = match m.get(3) {
            Some(y) => py::int(y.as_str())?,
            None if month <= today.month() => today.year() as i64,
            None => today.year() as i64 - 1,
        };
        let (mut lo, mut hi) = month_range(year, month)?;
        let part = m
            .get(1)
            .map(|p| p.as_str().replace("the ", ""))
            .unwrap_or_default();
        match part.as_str() {
            "early" | "beginning of" | "start of" => hi = days_on(lo, 9)?,
            "mid" => {
                let first = lo;
                lo = days_on(first, 10)?;
                hi = days_on(first, 19)?;
            }
            "late" | "end of" => lo = days_on(lo, 20)?,
            _ => {}
        }
        return Some((lo, hi));
    }
    None
}

/// (the range a search's `from` and `to` mean, whether a value that was given could not be read). `from` alone is
/// its own range ("last week" is Monday to Sunday); `to` alone ends a year-long range.
pub fn resolve_args(
    date_from: Option<&str>,
    date_to: Option<&str>,
    now: Dt,
) -> (Option<(NaiveDate, NaiveDate)>, bool) {
    let given = |v: Option<&str>| v.is_some_and(|s| !py::strip(s).is_empty());
    if !given(date_from) && !given(date_to) {
        return (None, false);
    }
    let a = if given(date_from) {
        resolve_when(date_from.unwrap_or(""), now)
    } else {
        None
    };
    let b = if given(date_to) {
        resolve_when(date_to.unwrap_or(""), now)
    } else {
        None
    };
    let unread = (given(date_from) && a.is_none()) || (given(date_to) && b.is_none());
    let rng = match (a, b) {
        (Some(a), Some(b)) => (a.0.min(b.0), a.1.max(b.1)),
        (Some(a), None) => a,
        (None, Some(b)) => match days_back(b.1, 365) {
            Some(lo) => (lo, b.1),
            None => return (None, unread),
        },
        (None, None) => return (None, unread),
    };
    (Some(rng), unread)
}

/// A range made wider once: by its own length (at least a week) on each side, never past today.
pub fn widen(rng: (NaiveDate, NaiveDate), today: NaiveDate) -> (NaiveDate, NaiveDate) {
    let (lo, hi) = rng;
    let pad = 7.max((hi - lo).num_days() + 1);
    let new_lo = days_back(lo, pad).unwrap_or(lo);
    let new_hi = days_on(hi, pad).unwrap_or(hi);
    (new_lo, today.min(new_hi))
}

// ---- words ------------------------------------------------------------------------------------------------------------

/// The query's telling words: lowercased, no stop words, no one-letter words, each once, in order.
pub fn terms(query: &str) -> Vec<String> {
    let lower = query.to_lowercase();
    let mut out: Vec<String> = Vec::new();
    for w in WORD.find_iter(&lower).map(|m| m.as_str()) {
        if py::len(w) > 1 && !STOPWORDS.contains(w) && !out.iter().any(|o| o == w) {
            out.push(w.to_string());
        }
    }
    out
}

/// An FTS5 MATCH string: any of the telling words (each quoted, so no word is read as an operator).
pub fn fts_query(query: &str) -> String {
    terms(query)
        .iter()
        .map(|w| format!("\"{}\"", w.replace('"', "")))
        .collect::<Vec<_>>()
        .join(" OR ")
}

// ---- units: one round of a conversation -----------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unit {
    pub day: String,
    pub conv: String,
    pub ts: String,
    pub ch: String,
    pub who: String,
    /// the line that opened the round, with its speaker
    pub head: String,
    pub reply: Vec<String>,
    pub before: String,
}

impl Unit {
    pub fn body(&self) -> String {
        let mut parts = vec![self.head.clone()];
        parts.extend(self.reply.iter().cloned());
        py::clip(&parts.join("\n"), UNIT_CHARS)
    }

    pub fn hash(&self) -> String {
        hex::encode(Sha256::digest(
            format!("{EMBED_MODEL}|{EMBED_DIMS}|{}", self.body()).as_bytes(),
        ))
    }
}

/// A day's lines as rounds: each line that is not eve's (the owner's, a memory-bus line) opens a round, and eve's
/// lines after it in the same conversation are its reply. Eve lines with nothing before them open their own round.
/// Close markers end a conversation's round; forgotten and empty lines are left out.
pub fn units_of<L: Borrow<Line>>(
    day: &str,
    lines: &[L],
    speaker: &dyn Fn(&Line) -> String,
) -> Vec<Unit> {
    let mut units: Vec<Unit> = Vec::new();
    let mut open: HashMap<String, usize> = HashMap::new();
    let mut last: HashMap<String, String> = HashMap::new();
    let get = |ln: &Line, k: &str| {
        ln.get(k)
            .map(eve_memory_sources::py_str_or_empty)
            .unwrap_or_default()
    };
    for ln in lines {
        let ln = ln.borrow();
        let conv = get(ln, "conv");
        let kind = ln.get("kind").and_then(Value::as_str).unwrap_or("");
        if kind == "close" {
            open.remove(&conv);
            continue;
        }
        let text = py::flat(&get(ln, "text"));
        if !["say", "tool", "image"].contains(&kind) || text.is_empty() || text == FORGOTTEN {
            continue;
        }
        let said = format!("{}: {}", speaker(ln), text);
        let is_eve = ln.get("who").and_then(Value::as_str) == Some("eve");
        match open.get(&conv) {
            Some(&i) if is_eve => units[i].reply.push(said.clone()),
            _ => {
                units.push(Unit {
                    day: day.to_string(),
                    conv: conv.clone(),
                    ts: get(ln, "ts"),
                    ch: get(ln, "ch"),
                    who: get(ln, "who"),
                    head: said.clone(),
                    reply: Vec::new(),
                    before: last.get(&conv).cloned().unwrap_or_default(),
                });
                open.insert(conv.clone(), units.len() - 1);
            }
        }
        last.insert(conv, said);
    }
    units
}

// ---- embeddings ---------------------------------------------------------------------------------------------------------

/// Texts and a timeout in seconds → one vector per text, in order.
pub type Embedder = Arc<dyn Fn(&[String], f64) -> anyhow::Result<Vec<Vec<f64>>> + Send + Sync>;

static CLIENTS: LazyLock<Mutex<HashMap<u64, eve_openrouter::BlockingClient>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// OpenAI's text-embedding-3-small at EMBED_DIMS dimensions, through OpenRouter on the owner's key (openrouter,
/// 2026-10-08; it was OpenAI's own endpoint before). The same model and the same dimensions, so the vectors already
/// in the index stay comparable and nothing is re-embedded. Each call is on the spend meter. None without a key.
///
/// Each call runs on its own short-lived thread (the blocking client must never run on an async runtime's thread);
/// the clients, one per timeout, live for the process so their connections are reused.
pub fn openai_embedder() -> Option<Embedder> {
    if !eve_openrouter::available(None) {
        return None;
    }
    Some(Arc::new(|texts: &[String], timeout: f64| {
        let texts = texts.to_vec();
        std::thread::spawn(move || embed_blocking(&texts, timeout))
            .join()
            .map_err(|_| anyhow::anyhow!("the embeddings call panicked"))?
    }))
}

fn embed_blocking(texts: &[String], timeout: f64) -> anyhow::Result<Vec<Vec<f64>>> {
    let client = {
        let mut clients = CLIENTS.lock();
        match clients.get(&timeout.to_bits()) {
            Some(c) => c.clone(),
            None => {
                let opts = eve_openrouter::ClientOptions::default()
                    .timeout(Duration::from_secs_f64(timeout.max(0.0)))
                    .max_retries(1);
                let c = eve_openrouter::client(None, opts)?;
                clients.insert(timeout.to_bits(), c.clone());
                c
            }
        }
    };
    let resp = client.embeddings_create(
        json!({"model": EMBED_MODEL, "input": texts, "dimensions": EMBED_DIMS}),
    )?;
    // the meter never costs a search
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        eve_spend::record_embedding(eve_spend::MEMORY, EMBED_MODEL, &resp, "openrouter")
    }));
    let data = resp
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("no data in the reply"))?;
    let mut items: Vec<(i64, Vec<f64>)> = Vec::with_capacity(data.len());
    for d in data {
        let index = d.get("index").and_then(Value::as_i64).unwrap_or(0);
        let emb = d
            .get("embedding")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow::anyhow!("an item has no embedding"))?
            .iter()
            .map(|x| x.as_f64().unwrap_or(0.0))
            .collect();
        items.push((index, emb));
    }
    items.sort_by_key(|(i, _)| *i);
    Ok(items.into_iter().map(|(_, v)| v).collect())
}

/// `_unit_vec`: the vector as float32, scaled to length 1 (as it was when its length is 0).
pub fn unit_vec(values: &[f64]) -> Vec<f32> {
    let v: Vec<f32> = values.iter().map(|x| *x as f32).collect();
    let n = v
        .iter()
        .map(|x| (*x as f64) * (*x as f64))
        .sum::<f64>()
        .sqrt() as f32;
    if n > 0.0 {
        v.iter().map(|x| x / n).collect()
    } else {
        v
    }
}

fn to_bytes(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

// ---- the index ------------------------------------------------------------------------------------------------------------

pub const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS meta(k TEXT PRIMARY KEY, v TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS days(day TEXT PRIMARY KEY, sig TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS units(id INTEGER PRIMARY KEY, day TEXT NOT NULL, conv TEXT NOT NULL, ts TEXT NOT NULL,
    ch TEXT NOT NULL, who TEXT NOT NULL, head TEXT NOT NULL, reply TEXT NOT NULL, before TEXT NOT NULL,
    hash TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS units_day ON units(day);
CREATE INDEX IF NOT EXISTS units_hash ON units(hash);
CREATE VIRTUAL TABLE IF NOT EXISTS fts USING fts5(body, tokenize='porter unicode61');
CREATE TABLE IF NOT EXISTS vectors(hash TEXT PRIMARY KEY, v BLOB NOT NULL);
";

/// `META`: what the file must say it holds, or it is built again.
pub fn meta() -> HashMap<String, String> {
    HashMap::from([
        ("version".to_string(), INDEX_VERSION.to_string()),
        ("model".to_string(), EMBED_MODEL.to_string()),
        ("dims".to_string(), EMBED_DIMS.to_string()),
    ])
}

pub fn index_path(store: &Path) -> PathBuf {
    py::expand_user(store).join(INDEX_SUBDIR).join(DB_FILE)
}

/// A forget could not finish cleaning the index (the file stayed busy or failed): the words may remain.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{0}")]
pub struct IndexForgetError(pub String);

/// What a source of transcript days gives the index (the Transcripts store).
pub trait DaySource: Send + Sync {
    fn enabled(&self) -> bool;
    fn days(&self) -> Vec<String>;
    fn path_of(&self, day: &str) -> PathBuf;
    fn lines(&self, day: &str) -> Vec<Arc<Line>>;
}

/// The module constants a test (or the eval) may set on one index.
#[derive(Debug, Clone)]
pub struct Tuning {
    pub debounce_secs: f64,
    pub embed_idle_secs: f64,
    pub fts_candidates: i64,
    pub min_cos: f64,
    pub compact_waits: Vec<f64>,
}

impl Default for Tuning {
    fn default() -> Self {
        Tuning {
            debounce_secs: DEBOUNCE_SECS,
            embed_idle_secs: EMBED_IDLE_SECS,
            fts_candidates: FTS_CANDIDATES,
            min_cos: MIN_COS,
            compact_waits: COMPACT_WAITS.to_vec(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IndexStats {
    pub units: i64,
    pub vectors: i64,
    pub days: i64,
}

#[derive(Default)]
struct Event {
    flag: Mutex<bool>,
    cv: Condvar,
}

impl Event {
    fn set(&self) {
        *self.flag.lock() = true;
        self.cv.notify_all();
    }

    fn clear(&self) {
        *self.flag.lock() = false;
    }

    /// `threading.Event.wait`: true when set, false at the timeout.
    fn wait(&self, timeout: Option<Duration>) -> bool {
        let mut flag = self.flag.lock();
        match timeout {
            None => {
                while !*flag {
                    self.cv.wait(&mut flag);
                }
            }
            Some(t) => {
                let deadline = Instant::now() + t;
                while !*flag {
                    if self.cv.wait_until(&mut flag, deadline).timed_out() {
                        break;
                    }
                }
            }
        }
        *flag
    }
}

struct Worker {
    handle: JoinHandle<()>,
    done: mpsc::Receiver<()>,
}

/// The class name Python's sqlite3 gives an error (what a log line names).
pub(crate) fn sql_error_name(e: &rusqlite::Error) -> &'static str {
    match e {
        rusqlite::Error::SqliteFailure(f, _) => match f.code {
            ErrorCode::ConstraintViolation => "IntegrityError",
            ErrorCode::NotADatabase | ErrorCode::DatabaseCorrupt => "DatabaseError",
            ErrorCode::TooBig => "DataError",
            ErrorCode::InternalMalfunction => "InternalError",
            _ => "OperationalError",
        },
        rusqlite::Error::InvalidParameterCount(..) | rusqlite::Error::InvalidParameterName(_) => {
            "ProgrammingError"
        }
        _ => "OperationalError",
    }
}

/// The ranked search over one memory folder's transcripts. Every method is synchronous, thread-safe and never
/// panics; `search` answers None when it cannot rank (the caller then runs the keyword AND).
pub struct MemoryIndex {
    pub path: PathBuf,
    transcripts: Arc<dyn DaySource>,
    embedder: Option<Embedder>,
    db: Mutex<Option<Connection>>,
    dirty: Mutex<HashSet<String>>,
    wake: Event,
    stop: AtomicBool,
    worker: Mutex<Option<Worker>>,
    built: AtomicBool,
    ok: AtomicBool,
    pub tuning: RwLock<Tuning>,
    #[cfg(test)]
    pub(crate) compact_fault: std::sync::atomic::AtomicUsize,
}

impl std::fmt::Debug for MemoryIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryIndex")
            .field("path", &self.path)
            .field("ok", &self.ok())
            .finish()
    }
}

impl MemoryIndex {
    pub fn new(
        path: impl Into<PathBuf>,
        transcripts: Arc<dyn DaySource>,
        embedder: Option<Embedder>,
    ) -> Self {
        let path = path.into();
        let idx = MemoryIndex {
            path: path.clone(),
            transcripts,
            embedder,
            db: Mutex::new(None),
            dirty: Mutex::new(HashSet::new()),
            wake: Event::default(),
            stop: AtomicBool::new(false),
            worker: Mutex::new(None),
            built: AtomicBool::new(false),
            ok: AtomicBool::new(false),
            tuning: RwLock::new(Tuning::default()),
            #[cfg(test)]
            compact_fault: std::sync::atomic::AtomicUsize::new(0),
        };
        match Self::open(&path) {
            Ok(db) => {
                *idx.db.lock() = Some(db);
                idx.ok.store(true, Ordering::SeqCst);
            }
            Err(name) => warn!("memory index: off — it could not be opened ({})", name),
        }
        idx
    }

    /// Whether the file is open.
    pub fn ok(&self) -> bool {
        self.ok.load(Ordering::SeqCst)
    }

    /// Whether the whole index has been built once (`ensure_built`).
    pub fn built(&self) -> bool {
        self.built.load(Ordering::SeqCst)
    }

    /// Mark the index as built or not (as for a missing index file).
    pub fn set_built(&self, built: bool) {
        self.built.store(built, Ordering::SeqCst);
    }

    /// Whether the background worker was started.
    pub fn has_worker(&self) -> bool {
        self.worker.lock().is_some()
    }

    // -- the file --

    fn open(path: &Path) -> Result<Connection, &'static str> {
        let folder = path.parent().unwrap_or(Path::new("."));
        let prepared = (|| -> std::io::Result<()> {
            fs::create_dir_all(folder)?;
            if fs::symlink_metadata(folder)?.file_type().is_symlink() {
                return Err(std::io::Error::other("the index folder is a symlink"));
            }
            fs::set_permissions(folder, fs::Permissions::from_mode(0o700))?;
            if fs::metadata(path).is_err() {
                OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(false)
                    .mode(0o600)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                    .open(path)?;
            }
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
            Ok(())
        })();
        if let Err(e) = prepared {
            return Err(py::os_error_name(&e));
        }
        let opened = (|| -> rusqlite::Result<Connection> {
            let db = Connection::open(path)?;
            db.busy_timeout(Duration::from_secs(10))?;
            db.execute_batch("PRAGMA secure_delete=ON")?;
            db.execute_batch("PRAGMA journal_mode=DELETE")?;
            db.execute_batch(SCHEMA)?;
            // a delete leaves no term behind in an older FTS segment (an SQLite before 3.44: optimize and VACUUM
            // still run)
            let _ = db.execute("INSERT INTO fts(fts, rank) VALUES('secure-delete', 1)", []);
            let mut have: HashMap<String, String> = {
                let mut st = db.prepare("SELECT k, v FROM meta")?;
                let rows =
                    st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
                rows.collect::<rusqlite::Result<_>>()?
            };
            if !have.is_empty() && have != meta() {
                // another version or model: everything is built again
                db.execute_batch(
                    "DELETE FROM units; DELETE FROM fts; DELETE FROM vectors; DELETE FROM days;",
                )?;
                have.clear();
            }
            if have.is_empty() {
                for (k, v) in [
                    ("version", INDEX_VERSION.to_string()),
                    ("model", EMBED_MODEL.to_string()),
                    ("dims", EMBED_DIMS.to_string()),
                ] {
                    db.execute(
                        "INSERT OR REPLACE INTO meta(k, v) VALUES(?, ?)",
                        params![k, v],
                    )?;
                }
            }
            Ok(db)
        })();
        opened.map_err(|e| sql_error_name(&e))
    }

    pub fn close(&self) {
        self.stop.store(true, Ordering::SeqCst);
        self.wake.set();
        let worker = self.worker.lock().take();
        if let Some(w) = worker {
            if w.handle.thread().id() != std::thread::current().id() {
                if w.done.recv_timeout(Duration::from_secs(5)).is_ok() {
                    let _ = w.handle.join();
                }
            } else {
                *self.worker.lock() = Some(w);
            }
        }
        let mut db = self.db.lock();
        if self.ok() {
            if let Some(conn) = db.take() {
                let _ = conn.close();
            }
            self.ok.store(false, Ordering::SeqCst);
        }
    }

    // -- keeping it current --

    fn sig(&self, day: &str) -> Option<String> {
        use std::os::unix::fs::MetadataExt;
        let st = fs::metadata(self.transcripts.path_of(day)).ok()?;
        let ns = st.mtime() as i128 * 1_000_000_000 + st.mtime_nsec() as i128;
        Some(format!("{}:{}:{}", ns, st.size(), st.ino()))
    }

    /// Index again every day (or each of `days`) whose file changed; with `embed`, embed the rounds that have no
    /// vector yet. A full refresh (no `days`) also drops days that are gone and vectors no round uses. → rounds
    /// indexed.
    pub fn refresh(&self, days: Option<&[String]>, embed: bool) -> usize {
        if !self.ok() {
            return 0;
        }
        let changed = match self.refresh_inner(days) {
            Ok(n) => n,
            Err(e) => {
                warn!("memory index: refresh failed ({})", sql_error_name(&e));
                return 0;
            }
        };
        if embed {
            self.embed_missing(60.0);
        }
        changed
    }

    /// refresh's work, in one transaction; an error after a rollback. Every refresh that indexed or dropped a day
    /// also drops the vectors no round uses, so a round that grew line by line leaves none behind.
    fn refresh_inner(&self, days: Option<&[String]>) -> rusqlite::Result<usize> {
        let full = days.is_none();
        let guard = self.db.lock();
        let Some(db) = guard.as_ref() else {
            return Ok(0);
        };
        let on_disk: Vec<String> = match days {
            None => self.transcripts.days(),
            Some(d) => {
                let mut v: Vec<String> = d
                    .iter()
                    .cloned()
                    .collect::<HashSet<_>>()
                    .into_iter()
                    .collect();
                v.sort();
                v
            }
        };
        let work = || -> rusqlite::Result<usize> {
            let have: HashMap<String, String> = {
                let mut st = db.prepare("SELECT day, sig FROM days")?;
                let rows =
                    st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
                rows.collect::<rusqlite::Result<_>>()?
            };
            let mut changed = 0;
            let mut touched = false;
            db.execute_batch("BEGIN")?;
            for day in &on_disk {
                let Some(sig) = self.sig(day) else {
                    if have.contains_key(day) {
                        drop_day(db, day)?;
                        touched = true;
                    }
                    continue;
                };
                if have.get(day) == Some(&sig) {
                    continue;
                }
                changed += self.index_day(db, day, &sig)?;
                touched = true;
            }
            if full {
                let disk: HashSet<&String> = on_disk.iter().collect();
                for day in have.keys().filter(|d| !disk.contains(d)) {
                    drop_day(db, day)?;
                }
            }
            if full || touched {
                gc(db)?;
            }
            db.execute_batch("COMMIT")?;
            Ok(changed)
        };
        let out = work();
        if out.is_err() {
            let _ = db.execute_batch("ROLLBACK");
        }
        out
    }

    fn index_day(&self, db: &Connection, day: &str, sig: &str) -> rusqlite::Result<usize> {
        let units = units_of(
            day,
            &self.transcripts.lines(day),
            &crate::transcripts::speaker,
        );
        db.execute(
            "DELETE FROM fts WHERE rowid IN (SELECT id FROM units WHERE day = ?)",
            [day],
        )?;
        db.execute("DELETE FROM units WHERE day = ?", [day])?;
        for u in &units {
            let reply = py::dumps(&Value::Array(u.reply.iter().map(|r| json!(r)).collect()));
            db.execute(
                "INSERT INTO units(day, conv, ts, ch, who, head, reply, before, hash) VALUES(?,?,?,?,?,?,?,?,?)",
                params![u.day, u.conv, u.ts, u.ch, u.who, u.head, reply, u.before, u.hash()],
            )?;
            let id = db.last_insert_rowid();
            db.execute(
                "INSERT INTO fts(rowid, body) VALUES(?, ?)",
                params![id, u.body()],
            )?;
        }
        db.execute(
            "INSERT OR REPLACE INTO days(day, sig) VALUES(?, ?)",
            params![day, sig],
        )?;
        Ok(units.len())
    }

    /// Embed the rounds that have no vector, in batches, outside the lock. A vector is kept only while a round still
    /// uses it (a forget in between leaves none behind). → vectors added.
    pub fn embed_missing(&self, budget_secs: f64) -> usize {
        let Some(embedder) = self.embedder.clone() else {
            return 0;
        };
        if !self.ok() {
            return 0;
        }
        let mut added = 0;
        let started = Instant::now();
        while started.elapsed().as_secs_f64() < budget_secs {
            let rows: Vec<(String, String)> = {
                let guard = self.db.lock();
                let Some(db) = guard.as_ref() else {
                    return added;
                };
                let got = (|| -> rusqlite::Result<Vec<(String, String)>> {
                    let mut st = db.prepare(
                        "SELECT u.hash, f.body FROM units u JOIN fts f ON f.rowid = u.id \
                         LEFT JOIN vectors v ON v.hash = u.hash WHERE v.hash IS NULL GROUP BY u.hash LIMIT ?",
                    )?;
                    let rows = st.query_map([EMBED_BATCH], |r| Ok((r.get(0)?, r.get(1)?)))?;
                    rows.collect()
                })();
                match got {
                    Ok(r) => r,
                    Err(_) => return added,
                }
            };
            if rows.is_empty() {
                break;
            }
            let bodies: Vec<String> = rows.iter().map(|(_, b)| b.clone()).collect();
            let vecs = match embedder(&bodies, EMBED_TIMEOUT_SECS) {
                Ok(v) => v,
                Err(e) => {
                    // no vectors this time; the words still rank
                    warn!(
                        "memory index: embedding failed ({}); {} rounds wait",
                        embed_error_name(&e),
                        rows.len()
                    );
                    break;
                }
            };
            let guard = self.db.lock();
            let Some(db) = guard.as_ref() else {
                return added;
            };
            let mut n = 0;
            let wrote = (|| -> rusqlite::Result<()> {
                db.execute_batch("BEGIN")?;
                for ((h, _body), v) in rows.iter().zip(vecs.iter()) {
                    db.execute(
                        "INSERT OR IGNORE INTO vectors(hash, v) SELECT ?, ? WHERE EXISTS (SELECT 1 FROM units WHERE hash = ?)",
                        params![h, to_bytes(&unit_vec(v)), h],
                    )?;
                    n += 1;
                }
                db.execute_batch("COMMIT")
            })();
            added += n;
            if wrote.is_err() {
                let _ = db.execute_batch("ROLLBACK");
                break;
            }
        }
        added
    }

    /// The whole index current and embedded, now (the eval calls this before a question). → ok.
    pub fn ensure_built(&self) -> bool {
        if !self.ok() {
            return false;
        }
        self.refresh(None, true);
        self.built.store(true, Ordering::SeqCst);
        true
    }

    // -- the worker --

    /// The background worker: builds a missing index once, then indexes each day `notify` names.
    pub fn start(self: &Arc<Self>) {
        let mut worker = self.worker.lock();
        if !self.ok() || worker.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let me = Arc::clone(self);
        let spawned = std::thread::Builder::new()
            .name("memory-index".into())
            .spawn(move || {
                me.run();
                let _ = tx.send(());
            });
        match spawned {
            Ok(handle) => *worker = Some(Worker { handle, done: rx }),
            Err(e) => warn!("memory index: no worker ({})", py::os_error_name(&e)),
        }
    }

    /// A line was appended to `day` (Transcripts::append calls this: a set and an Event).
    pub fn notify(&self, day: &str) {
        self.dirty.lock().insert(day.to_string());
        self.wake.set();
    }

    fn run(&self) {
        self.ensure_built();
        let mut embed_at: Option<Instant> = None; // when the new rounds are embedded: EMBED_IDLE_SECS of quiet
        while !self.stop.load(Ordering::SeqCst) {
            let wait = embed_at.map(|t| t.saturating_duration_since(Instant::now()));
            let woke = self.wake.wait(wait);
            if self.stop.load(Ordering::SeqCst) {
                break;
            }
            if woke {
                let (debounce, idle) = {
                    let t = self.tuning.read();
                    (t.debounce_secs, t.embed_idle_secs)
                };
                std::thread::sleep(Duration::from_secs_f64(debounce.max(0.0)));
                self.wake.clear();
                let days: Vec<String> = std::mem::take(&mut *self.dirty.lock())
                    .into_iter()
                    .collect();
                if !days.is_empty() {
                    // the words now; the vectors once the talk goes quiet
                    self.refresh(Some(&days), false);
                }
                embed_at = Some(Instant::now() + Duration::from_secs_f64(idle.max(0.0)));
                continue;
            }
            embed_at = None;
            self.embed_missing(60.0);
        }
    }

    // -- search --

    /// The best `limit` rounds between two days (inclusive), as transcript-search hits, best first: {"hits",
    /// "found"}. A round counts as found when it shares a telling word with the query or its cosine is at least
    /// MIN_COS. None when the index or the query's vector is not there.
    pub fn search(
        &self,
        query: &str,
        first_day: &str,
        last_day: &str,
        limit: usize,
        channels: Option<&[String]>,
    ) -> Option<Value> {
        // Not built yet: the keyword AND answers. A search never starts the build-and-embed worker itself: only the
        // daemon runs it (Memory(background=True) starts it when memory opens), and a CLI search that did would
        // begin embedding a year of transcripts in a process about to exit.
        let embedder = self.embedder.clone()?;
        if !self.ok() || !self.built() {
            return None;
        }
        let t0 = Instant::now();
        let days: Vec<String> = self
            .transcripts
            .days()
            .into_iter()
            .filter(|d| first_day <= d.as_str() && d.as_str() <= last_day)
            .collect();
        self.refresh(Some(&days), false); // a line said a moment ago, or redacted elsewhere
        let qv = match embedder(&[query.to_string()], QUERY_TIMEOUT_SECS) {
            Ok(v) if !v.is_empty() => unit_vec(&v[0]),
            Ok(_) => {
                warn!(
                    "memory index: the query's vector failed (IndexError); keyword search instead"
                );
                return None;
            }
            Err(e) => {
                warn!(
                    "memory index: the query's vector failed ({}); keyword search instead",
                    embed_error_name(&e)
                );
                return None;
            }
        };
        let (fts_candidates, min_cos) = {
            let t = self.tuning.read();
            (t.fts_candidates, t.min_cos)
        };
        type Row = (i64, String, Option<Vec<u8>>);
        let (mut rows, bm) = {
            let guard = self.db.lock();
            let db = guard.as_ref()?;
            let got = (|| -> rusqlite::Result<(Vec<Row>, HashMap<i64, f64>)> {
                // Only the id, the channel and the vector of each round in range: the text is read for the best few.
                let mut st = db.prepare(
                    "SELECT u.id, u.ch, v.v FROM units u LEFT JOIN vectors v ON v.hash = u.hash \
                     WHERE u.day BETWEEN ? AND ?",
                )?;
                let rows = st
                    .query_map(params![first_day, last_day], |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                    })?
                    .collect::<rusqlite::Result<Vec<Row>>>()?;
                let mut bm: HashMap<i64, f64> = HashMap::new();
                let fq = fts_query(query);
                if !fq.is_empty() {
                    // the BM25 candidates come from the range too. A join, not "rowid IN (subquery)": FTS5 runs the
                    // IN form row by row (0.7 s over a year).
                    let mut st = db.prepare(
                        "SELECT f.rowid, bm25(fts) AS s FROM fts f JOIN units u ON u.id = f.rowid \
                         WHERE fts MATCH ? AND u.day BETWEEN ? AND ? ORDER BY s LIMIT ?",
                    )?;
                    let found = st
                        .query_map(params![fq, first_day, last_day, fts_candidates], |r| {
                            Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(1)?))
                        })?;
                    for f in found {
                        let (rid, score) = f?;
                        bm.insert(rid, -score); // FTS5's bm25 is lower-is-better
                    }
                }
                Ok((rows, bm))
            })();
            match got {
                Ok(g) => g,
                Err(e) => {
                    warn!("memory index: search failed ({})", sql_error_name(&e));
                    return None;
                }
            }
        };
        if let Some(chs) = channels.filter(|c| !c.is_empty()) {
            rows.retain(|r| chs.contains(&r.1));
        }
        let width = qv.len() * 4;
        // one product per round with a vector of the query's width
        let cos: Vec<f32> = rows
            .iter()
            .map(|r| match &r.2 {
                Some(blob) if blob.len() == width => blob
                    .chunks_exact(4)
                    .zip(qv.iter())
                    .map(|(b, q)| f32::from_le_bytes([b[0], b[1], b[2], b[3]]) * q)
                    .sum::<f32>(),
                _ => 0.0,
            })
            .collect();
        let best_bm = rows
            .iter()
            .map(|r| bm.get(&r.0).copied().unwrap_or(0.0))
            .fold(0.0f64, f64::max);
        let mut scored: Vec<(f64, i64)> = Vec::new();
        for (k, r) in rows.iter().enumerate() {
            let c = cos[k] as f64;
            let b = bm.get(&r.0).copied().unwrap_or(0.0);
            if b > 0.0 || c >= min_cos {
                // found: a word in common, or close enough in meaning
                let norm = if best_bm > 0.0 && b > 0.0 {
                    b / best_bm
                } else {
                    0.0
                };
                scored.push((BM25_WEIGHT * norm + COS_WEIGHT * c, r.0));
            }
        }
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        let best: Vec<i64> = scored.iter().take(limit).map(|(_, rid)| *rid).collect();
        let mut hits: Vec<Value> = Vec::new();
        if !best.is_empty() {
            let guard = self.db.lock();
            let db = guard.as_ref()?;
            let sql = format!(
                "SELECT id, day, ts, ch, who, head, reply, before FROM units WHERE id IN ({})",
                vec!["?"; best.len()].join(",")
            );
            let got = (|| -> rusqlite::Result<HashMap<i64, Value>> {
                let mut st = db.prepare(&sql)?;
                let rows = st.query_map(params_from_iter(best.iter()), |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        hit(
                            &r.get::<_, String>(1)?,
                            &r.get::<_, String>(2)?,
                            &r.get::<_, String>(3)?,
                            &r.get::<_, String>(4)?,
                            &r.get::<_, String>(5)?,
                            &r.get::<_, String>(6)?,
                            &r.get::<_, String>(7)?,
                        ),
                    ))
                })?;
                rows.collect()
            })();
            let full = match got {
                Ok(f) => f,
                Err(e) => {
                    warn!("memory index: search failed ({})", sql_error_name(&e));
                    return None;
                }
            };
            hits = best
                .iter()
                .filter_map(|rid| full.get(rid).cloned())
                .collect();
        }
        info!(
            "memory index: {} rounds over {} days → {} hits in {} ms",
            rows.len(),
            days.len(),
            hits.len(),
            t0.elapsed().as_millis()
        );
        let found = !hits.is_empty();
        Some(json!({"hits": hits, "found": found}))
    }

    // -- forget --

    /// Every round whose full stored text matches: the opening line and every reply line (not the FTS body, which is
    /// clipped at UNIT_CHARS), or the line before it.
    fn matching(db: &Connection, pred: &dyn Fn(&str) -> bool) -> rusqlite::Result<Vec<i64>> {
        let mut st = db.prepare("SELECT id, head, reply, before FROM units")?;
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (rid, head, reply, before) = row?;
            let replies: Vec<String> = match py::loads(&reply) {
                None => vec![reply.clone()],
                Some(v) if !py::truthy(&v) => Vec::new(),
                Some(Value::Array(items)) => items.iter().map(eve_memory_sources::py_str).collect(),
                Some(other) => vec![eve_memory_sources::py_str(&other)],
            };
            let mut parts = vec![head];
            parts.extend(replies);
            if pred(&parts.join("\n")) || pred(&before) {
                out.push(rid);
            }
        }
        Ok(out)
    }

    /// How many rounds hold words `pred` matches (forget's preview). 0 when off.
    pub fn count(&self, pred: &dyn Fn(&str) -> bool) -> usize {
        if !self.ok() {
            return 0;
        }
        let guard = self.db.lock();
        match guard.as_ref() {
            Some(db) => Self::matching(db, pred).map(|v| v.len()).unwrap_or(0),
            None => 0,
        }
    }

    /// Delete every round `pred` matches (its full text or the line before it) and its FTS row; index again every
    /// indexed day whose file changed (the transcripts are already redacted), which also drops every vector no round
    /// uses; then rewrite the FTS segments and VACUUM, every time, so the words are not left in old segments or free
    /// pages. → rounds deleted. IndexForgetError when a step still fails after COMPACT_WAITS: the caller reports the
    /// index as not done.
    pub fn forget(&self, pred: &dyn Fn(&str) -> bool) -> Result<usize, IndexForgetError> {
        if !self.ok() {
            return Ok(0);
        }
        let (ids, indexed) = {
            let guard = self.db.lock();
            let Some(db) = guard.as_ref() else {
                return Ok(0);
            };
            let work = || -> rusqlite::Result<(Vec<i64>, Vec<String>)> {
                let ids = Self::matching(db, pred)?;
                let indexed: Vec<String> = {
                    let mut st = db.prepare("SELECT day FROM days")?;
                    let rows = st.query_map([], |r| r.get(0))?;
                    rows.collect::<rusqlite::Result<_>>()?
                };
                if !ids.is_empty() {
                    let mut days: Vec<String> = {
                        let sql = format!(
                            "SELECT DISTINCT day FROM units WHERE id IN ({})",
                            vec!["?"; ids.len()].join(",")
                        );
                        let mut st = db.prepare(&sql)?;
                        let rows = st.query_map(params_from_iter(ids.iter()), |r| r.get(0))?;
                        rows.collect::<rusqlite::Result<_>>()?
                    };
                    days.sort();
                    days.dedup();
                    db.execute_batch("BEGIN")?;
                    for i in &ids {
                        db.execute("DELETE FROM fts WHERE rowid = ?", [i])?;
                    }
                    for i in &ids {
                        db.execute("DELETE FROM units WHERE id = ?", [i])?;
                    }
                    for d in &days {
                        db.execute("DELETE FROM days WHERE day = ?", [d])?;
                    }
                    gc(db)?;
                    db.execute_batch("COMMIT")?;
                }
                Ok((ids, indexed))
            };
            match work() {
                Ok(got) => got,
                Err(e) => {
                    let _ = db.execute_batch("ROLLBACK");
                    let name = sql_error_name(&e);
                    warn!("memory index: forget failed ({})", name);
                    return Err(IndexForgetError(name.to_string()));
                }
            }
        };
        // Only the days already in the index: a forget cleans the file, it never adds days to one left switched off.
        self.retry(&|| self.refresh_inner(Some(&indexed)).map(|_| ()))?;
        self.retry(&|| self.compact())?;
        if self.has_worker() {
            // the daemon embeds the re-indexed rounds when it is quiet
            self.wake.set();
        }
        info!("memory index: forgot {} rounds", ids.len());
        Ok(ids.len())
    }

    fn compact(&self) -> rusqlite::Result<()> {
        #[cfg(test)]
        {
            if self.compact_fault.load(Ordering::SeqCst) > 0 {
                self.compact_fault.fetch_add(1, Ordering::SeqCst);
                return Err(rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
                    Some("database is locked".into()),
                ));
            }
        }
        let guard = self.db.lock();
        let Some(db) = guard.as_ref() else {
            return Ok(());
        };
        db.execute_batch("INSERT INTO fts(fts) VALUES('optimize')")?;
        db.execute_batch("VACUUM")
    }

    /// Run one forget step; on an SQLite error (a busy file: the daemon and a CLI share it) wait and try again,
    /// COMPACT_WAITS in all. Still failing: IndexForgetError.
    fn retry(&self, step: &dyn Fn() -> rusqlite::Result<()>) -> Result<(), IndexForgetError> {
        let waits = self.tuning.read().compact_waits.clone();
        let mut err: Option<rusqlite::Error> = None;
        for wait in waits {
            if wait > 0.0 {
                std::thread::sleep(Duration::from_secs_f64(wait));
            }
            match step() {
                Ok(()) => return Ok(()),
                Err(e) => err = Some(e),
            }
        }
        let name = err.as_ref().map(sql_error_name).unwrap_or("NoneType");
        warn!("memory index: forget could not finish ({})", name);
        Err(IndexForgetError(name.to_string()))
    }

    pub fn stats(&self) -> IndexStats {
        if !self.ok() {
            return IndexStats::default();
        }
        let guard = self.db.lock();
        let Some(db) = guard.as_ref() else {
            return IndexStats::default();
        };
        let count = |t: &str| -> i64 {
            db.query_row(&format!("SELECT COUNT(*) FROM {t}"), [], |r| r.get(0))
                .optional()
                .ok()
                .flatten()
                .unwrap_or(0)
        };
        IndexStats {
            units: count("units"),
            vectors: count("vectors"),
            days: count("days"),
        }
    }

    /// Run `f` on the open file under the index's lock (None when closed): the eval's seeding and tests' checks.
    pub fn with_db<R>(&self, f: impl FnOnce(&Connection) -> R) -> Option<R> {
        let guard = self.db.lock();
        guard.as_ref().map(f)
    }
}

impl crate::transcripts::Ranker for MemoryIndex {
    fn search(
        &self,
        query: &str,
        first_day: &str,
        last_day: &str,
        limit: usize,
        channels: Option<&[String]>,
    ) -> anyhow::Result<Option<Value>> {
        Ok(MemoryIndex::search(
            self, query, first_day, last_day, limit, channels,
        ))
    }
}

fn drop_day(db: &Connection, day: &str) -> rusqlite::Result<()> {
    db.execute(
        "DELETE FROM fts WHERE rowid IN (SELECT id FROM units WHERE day = ?)",
        [day],
    )?;
    db.execute("DELETE FROM units WHERE day = ?", [day])?;
    db.execute("DELETE FROM days WHERE day = ?", [day])?;
    Ok(())
}

fn gc(db: &Connection) -> rusqlite::Result<usize> {
    db.execute(
        "DELETE FROM vectors WHERE hash NOT IN (SELECT hash FROM units)",
        [],
    )
}

fn embed_error_name(e: &anyhow::Error) -> String {
    if let Some(oe) = e.downcast_ref::<eve_openrouter::Error>() {
        return match oe {
            eve_openrouter::Error::NoKey(_) => "NoKey".into(),
            eve_openrouter::Error::Status { status, .. } => match status {
                400 => "BadRequestError",
                401 => "AuthenticationError",
                403 => "PermissionDeniedError",
                404 => "NotFoundError",
                409 => "ConflictError",
                422 => "UnprocessableEntityError",
                429 => "RateLimitError",
                s if *s >= 500 => "InternalServerError",
                _ => "APIStatusError",
            }
            .into(),
            eve_openrouter::Error::Http(h) if h.is_timeout() => "APITimeoutError".into(),
            eve_openrouter::Error::Http(_) => "APIConnectionError".into(),
            _ => "APIError".into(),
        };
    }
    "Exception".into()
}

/// `_hit`: one stored round as a transcript-search hit.
fn hit(day: &str, ts: &str, ch: &str, who: &str, head: &str, reply: &str, before: &str) -> Value {
    let hhmm = py::fromisoformat(ts)
        .map(|(n, _)| n.format("%H:%M").to_string())
        .unwrap_or_default();
    let text = head.split_once(": ").map(|(_, t)| t).unwrap_or(head);
    let replies: Vec<String> = match py::loads(reply) {
        Some(Value::Array(items)) => items.iter().map(eve_memory_sources::py_str).collect(),
        Some(Value::String(s)) => s.chars().map(String::from).collect(),
        _ => Vec::new(),
    };
    json!({
        "day": day, "time": hhmm, "ch": ch, "who": who, "text": py::clip(text, HIT_CHARS),
        "before": py::clip(before, NEIGHBOUR_CHARS), "after": py::clip(&replies.join(" "), REPLY_CHARS),
    })
}

/// The index for a memory folder, or None when the transcripts are off or the folder may not hold it (the
/// transcripts' own guards: no git work tree, no synced folder, no symlink). `embed=false`: no embedder at all (a
/// forget of a file left from when the switch was on sends nothing anywhere); `embedder` None with `embed` takes
/// [`openai_embedder`].
pub fn open_index(
    store: &Path,
    transcripts: Arc<dyn DaySource>,
    embedder: Option<Embedder>,
    embed: bool,
) -> Option<Arc<MemoryIndex>> {
    if !transcripts.enabled() {
        return None;
    }
    let path = index_path(store);
    let why = crate::transcripts::refusal(path.parent().unwrap_or(Path::new(".")));
    if !why.is_empty() {
        warn!("memory index: off — {}", why);
        return None;
    }
    let embedder = if !embed {
        None
    } else {
        embedder.or_else(openai_embedder)
    };
    let idx = MemoryIndex::new(path, transcripts, embedder);
    if idx.ok() {
        Some(Arc::new(idx))
    } else {
        None
    }
}
