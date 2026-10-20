//! watch.py's Ticketmaster rules (pure): which attraction the owner means, which events are tribute acts, and the
//! reading from Discovery API events. The two reads that call the API (`_tm_get`, `_read_ticketmaster`) are the
//! Watcher's, in watcher.rs.

use std::collections::HashSet;
use std::sync::LazyLock;

use chrono::{Duration, FixedOffset, Local, NaiveDate, NaiveDateTime, NaiveTime, TimeZone};
use regex::Regex;
use serde_json::{Map, Value};

use super::page::Reading;
use super::rules::{price_of, py, when};
use super::NOTE_CHARS;

/// `TRIBUTE`.
pub const TRIBUTE: &str = r"(?i)\b(tribute|a tribute to|experience|salute to|legacy of|vs\.?|cover band|celebrating)\b";

static TRIBUTE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(TRIBUTE).expect("tribute regex"));
static PUNCT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[^\w\s]").expect("punctuation regex"));
static HTTP_LINK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^https?://").expect("link regex"));

/// `_iso(raw)`: an ISO time (a trailing Z is UTC) as epoch seconds, None when it is not one.
pub fn iso(raw: &Value) -> Option<f64> {
    match raw {
        Value::String(s) if !s.is_empty() => fromisoformat_timestamp(&s.replace('Z', "+00:00")),
        _ => None,
    }
}

/// `datetime.fromisoformat(text).timestamp()` (Python 3.11+): a date (`YYYY-MM-DD` or `YYYYMMDD`), optionally
/// any one separator character and a time (`HH`, `HH:MM`, `HH:MM:SS`, `HH:MM:SS.ffffff` or their basic forms,
/// a fraction of any length cut to microseconds), optionally an offset (`+HH`, `+HH:MM`, `+HHMM`,
/// `+HH:MM:SS[.ffffff]`). A time with no offset is local time, as Python reads a naive datetime.
fn fromisoformat_timestamp(text: &str) -> Option<f64> {
    let (date, rest) = parse_date(text)?;
    let (naive, offset) = if rest.is_empty() {
        (date.and_hms_opt(0, 0, 0)?, None)
    } else {
        // any one character separates the date from the time
        let mut chars = rest.chars();
        chars.next()?;
        let time_text = chars.as_str();
        let (clock, offset_text) = match time_text.find(['+', '-']) {
            Some(i) => (&time_text[..i], Some(&time_text[i..])),
            None => (time_text, None),
        };
        let time = parse_time(clock)?;
        let offset = match offset_text {
            Some(o) => Some(parse_offset(o)?),
            None => None,
        };
        (date.and_time(time), offset)
    };
    let micros = match offset {
        Some(off) => (naive - off).and_utc().timestamp_micros(),
        None => local_timestamp(naive)?.timestamp_micros(),
    };
    Some(micros as f64 / 1e6)
}

/// The instant a naive local time names, as Python's `timestamp()` reads it (fold 0): the earlier of two in a
/// fall-back hour; in a spring-forward gap, the offset in force before the gap.
fn local_timestamp(naive: NaiveDateTime) -> Option<chrono::DateTime<chrono::Utc>> {
    match Local.from_local_datetime(&naive).earliest() {
        Some(dt) => Some(dt.to_utc()),
        None => {
            let before = Local.from_local_datetime(&(naive - Duration::hours(3))).earliest()?;
            let offset = Duration::seconds(i64::from(before.offset().local_minus_utc()));
            Some((naive - offset).and_utc())
        }
    }
}

fn digits(s: &str) -> Option<u32> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// The date at the start of `text`, and what follows it.
fn parse_date(text: &str) -> Option<(NaiveDate, &str)> {
    let b = text.as_bytes();
    let (y, m, d, len) = if b.len() >= 10 && b[4] == b'-' && b[7] == b'-' {
        (digits(text.get(0..4)?)?, digits(text.get(5..7)?)?, digits(text.get(8..10)?)?, 10)
    } else if b.len() >= 8 && b[..8].iter().all(u8::is_ascii_digit) {
        (digits(&text[0..4])?, digits(&text[4..6])?, digits(&text[6..8])?, 8)
    } else {
        return None;
    };
    let date = NaiveDate::from_ymd_opt(y as i32, m, d).filter(|_| y >= 1)?;
    Some((date, &text[len..]))
}

/// `HH[:MM[:SS[.f+]]]` or `HH[MM[SS[.f+]]]`.
fn parse_time(text: &str) -> Option<NaiveTime> {
    let (whole, frac) = match text.find(['.', ',']) {
        Some(i) => (&text[..i], Some(&text[i + 1..])),
        None => (text, None),
    };
    let parts: Vec<&str> = if whole.contains(':') {
        whole.split(':').collect()
    } else {
        if whole.len() % 2 != 0 {
            return None;
        }
        (0..whole.len()).step_by(2).map(|i| &whole[i..i + 2]).collect()
    };
    if parts.is_empty() || parts.len() > 3 || parts.iter().any(|p| p.len() != 2) {
        return None;
    }
    let h = digits(parts[0])?;
    let m = parts.get(1).map(|p| digits(p)).unwrap_or(Some(0))?;
    let s = parts.get(2).map(|p| digits(p)).unwrap_or(Some(0))?;
    let micro = match frac {
        // a fraction only after the seconds
        Some(f) if parts.len() == 3 => {
            digits(f)?;
            let six: String = f.chars().chain(std::iter::repeat('0')).take(6).collect();
            six.parse().ok()?
        }
        Some(_) => return None,
        None => 0,
    };
    NaiveTime::from_hms_micro_opt(h, m, s, micro)
}

/// `+HH`, `+HH:MM`, `+HHMM`, `+HH:MM:SS[.ffffff]`: less than a day either way.
fn parse_offset(text: &str) -> Option<FixedOffset> {
    let sign = if text.starts_with('-') { -1 } else { 1 };
    let t = parse_time(&text[1..])?;
    let secs = chrono::Timelike::num_seconds_from_midnight(&t) as i32;
    if chrono::Timelike::nanosecond(&t) != 0 {
        // a sub-second offset is rare enough to refuse rather than round
        return None;
    }
    FixedOffset::east_opt(sign * secs)
}

/// `_tribute_words(text)`: the tribute words in `text`, lowercased, "a " and " to" and a trailing "." dropped.
pub fn tribute_words(text: &str) -> HashSet<String> {
    TRIBUTE_RE
        .find_iter(text)
        .map(|m| {
            let word = m.as_str().to_lowercase();
            let word = word.strip_prefix("a ").unwrap_or(&word);
            let word = word.strip_suffix(" to").unwrap_or(word);
            word.trim_end_matches('.').to_string()
        })
        .collect()
}

/// `_tribute(name, query, sports=)`: a tribute act the owner did not ask for: the name has a tribute word the
/// owner's own words lack. A game is billed "A vs. B", so "vs" marks no tribute in the Sports segment.
pub fn tribute(name: &str, query: &str, sports: bool) -> bool {
    let theirs = tribute_words(query);
    tribute_words(name).iter().any(|w| !(theirs.contains(w) || (sports && w == "vs")))
}

/// `_sports(event)`: one of its classifications is the Sports segment.
pub fn sports(event: &Map<String, Value>) -> bool {
    py::list(event.get("classifications")).iter().any(|c| {
        c.as_object()
            .and_then(|c| py::obj(c.get("segment")))
            .and_then(|seg| seg.get("name"))
            .is_some_and(|name| name == "Sports")
    })
}

/// `_norm(name)`: lowercased, punctuation to spaces, whitespace collapsed.
pub fn norm(name: &str) -> String {
    py::split_join(&PUNCT.replace_all(&name.to_lowercase(), " "))
}

/// `_tm_events(payload)`: `_embedded.events`, objects only.
pub fn tm_events(payload: &Map<String, Value>) -> Vec<Map<String, Value>> {
    let embedded = py::obj(payload.get("_embedded"));
    py::list(embedded.and_then(|e| e.get("events"))).iter().filter_map(|e| e.as_object().cloned()).collect()
}

/// The attraction the owner means: its name or an alias equals the query (case and punctuation aside), it is not
/// a tribute act, and among several the one with the most upcoming events. "" when none is.
pub fn pick_attraction(query: &str, payload: &Map<String, Value>) -> String {
    let want = norm(query);
    let mut best: (i64, String) = (-1, String::new());
    let embedded = py::obj(payload.get("_embedded"));
    for a in py::list(embedded.and_then(|e| e.get("attractions"))) {
        let Some(a) = a.as_object() else { continue };
        let Some(id) = a.get("id").filter(|v| py::truthy(v)) else { continue };
        let name = py::str_or_empty(a.get("name"));
        let mut names: HashSet<String> = HashSet::from([norm(&name)]);
        names.extend(py::list(a.get("aliases")).iter().filter_map(Value::as_str).map(norm));
        if !names.contains(&want) || tribute(&name, query, false) {
            continue;
        }
        let total = py::obj(a.get("upcomingEvents")).and_then(|u| u.get("_total"));
        // an int only (a bool is one in Python)
        let score = match total {
            Some(Value::Number(n)) if !n.is_f64() => n.as_i64().unwrap_or(i64::MAX),
            Some(Value::Bool(b)) => *b as i64,
            _ => 0,
        };
        if score > best.0 {
            best = (score, py::str_of(id));
        }
    }
    best.1
}

/// A reading from Discovery API events, by Ticketmaster's own status definitions (developer.ticketmaster.com
/// discovery-feed): onsale is "within the public onsale timeframe"; offsale is "not yet reached (or past)" it,
/// and "inventory may still be available if there is an active presale"; rescheduled "will appear as on sale if
/// inventory is available"; postponed and canceled are not on sale. So an event is available when it is onsale or
/// rescheduled, or when one of its presale windows is open now, whatever its status. An offsale event whose public
/// sale has not ended is still watched: that is the "not on sale yet" being waited for. priceRanges was removed
/// from Ticketmaster's feeds on 2025-03-11 and is sparse in the API: when it is absent the price is unknown
/// (None), never 0. The note counts the dates and says when the next sale opens.
pub fn ticketmaster_reading(events: &[Map<String, Value>], now: f64) -> Reading {
    let mut r = Reading { source: "ticketmaster".to_string(), ..Reading::default() };
    if events.is_empty() {
        r.available = Some(false);
        r.note = "No matching events are listed on Ticketmaster yet.".to_string();
        return r;
    }
    let mut live: Vec<&Map<String, Value>> = Vec::new();
    let mut open_events: Vec<&Map<String, Value>> = Vec::new();
    let mut upcoming: Vec<(f64, String, &Map<String, Value>)> = Vec::new();
    for e in events {
        let dates = py::obj(e.get("dates"));
        let status = dates.and_then(|d| py::obj(d.get("status")));
        let code = py::str_or_empty(status.and_then(|s| s.get("code"))).to_lowercase();
        if ["canceled", "cancelled", "postponed"].contains(&code.as_str()) {
            continue;
        }
        let sales = py::obj(e.get("sales"));
        let public = sales.and_then(|s| py::obj(s.get("public")));
        let start = public.and_then(|p| p.get("startDateTime")).and_then(iso);
        let end = public.and_then(|p| p.get("endDateTime")).and_then(iso);
        if code == "offsale" && end.is_some_and(|end| end <= now) {
            continue; // its sale is over, not yet to come
        }
        live.push(e);
        let windows: Vec<(Option<f64>, Option<f64>, String)> = py::list(sales.and_then(|s| s.get("presales")))
            .iter()
            .filter_map(Value::as_object)
            .map(|p| {
                let name = p.get("name").filter(|v| py::truthy(v)).map(py::str_of);
                (
                    p.get("startDateTime").and_then(iso),
                    p.get("endDateTime").and_then(iso),
                    name.unwrap_or_else(|| "presale".to_string()),
                )
            })
            .collect();
        let presale_open =
            windows.iter().any(|(s, pe, _)| s.is_some_and(|s| s <= now) && pe.is_none_or(|pe| now < pe));
        if code == "onsale" || code == "rescheduled" || presale_open {
            open_events.push(e);
        }
        let public_sale = (start, None, "the public sale".to_string());
        for (s, _, name) in windows.into_iter().chain(std::iter::once(public_sale)) {
            if let Some(s) = s.filter(|s| *s > now) {
                upcoming.push((s, name, e));
            }
        }
    }
    r.available = Some(!open_events.is_empty());
    let pool: Vec<&Map<String, Value>> = if !open_events.is_empty() {
        open_events.clone()
    } else if !live.is_empty() {
        live.clone()
    } else {
        events.iter().collect()
    };
    let mut lows: Vec<(f64, String)> = Vec::new();
    for e in &pool {
        for p in py::list(e.get("priceRanges")).iter().filter_map(Value::as_object) {
            if let Some(v) = price_of(p.get("min")).filter(|v| *v > 0.0) {
                lows.push((v, py::str_or_empty(p.get("currency"))));
            }
        }
    }
    // min() of (price, currency) pairs: the lowest price, then the first currency in order
    if let Some((v, cur)) =
        lows.into_iter().reduce(|a, b| if (b.0, &b.1).partial_cmp(&(a.0, &a.1)) == Some(std::cmp::Ordering::Less) { b } else { a })
    {
        r.value = Some(v);
        r.currency = py::take(&cur.to_uppercase(), 8);
    }
    let first = pool[0];
    let url = py::str_or_empty(first.get("url"));
    r.url = if HTTP_LINK.is_match(&url) { url } else { String::new() };
    r.name = py::take(&py::str_or_empty(first.get("name")), 120);
    let shown = &live; // an event whose sale has ended is not listed
    let mut note = format!(
        "{} date{} listed, {} on sale now.",
        shown.len(),
        if shown.len() != 1 { "s" } else { "" },
        open_events.len()
    );
    // min(upcoming, key=start): the first of the earliest
    let next = upcoming.iter().fold(None::<&(f64, String, &Map<String, Value>)>, |best, x| match best {
        Some(b) if b.0 <= x.0 => Some(b),
        _ => Some(x),
    });
    if let Some((s, name, e)) = next {
        // 9999-12-31 is past a local datetime: "later"
        let opens = when(*s);
        let start = py::obj(e.get("dates")).and_then(|d| py::obj(d.get("start")));
        let day = py::str_or_empty(start.and_then(|s| s.get("localDate")));
        let show = if day.is_empty() { String::new() } else { format!(" for the {day} show") };
        note.push_str(&format!(" Next: {name} opens {opens}{show}."));
    }
    r.note = py::take(&note, NOTE_CHARS);
    r
}
