//! Events near the owner: concerts, games and shows by place and date, from Ticketmaster's Discovery API.
//!
//! A port of `bridge/src/eve_bridge/events.py`.
//!
//! Why (owner, 2026-10-08: "this should also be using ticketmaster"): "any concerts in Seattle this weekend" went to
//! web_search, which timed out on a slow night. Ticketmaster answers the same question in about 300 ms (measured live
//! 2026-10-08: `events.json?city=Seattle&classificationName=music` gave HTTP 200 in 287 ms, 566 events). Before this,
//! eve used Ticketmaster only as one watch kind (watch.py `ticketmaster`: is one artist's show on sale yet); nothing
//! searched events by place and date.
//!
//! One tool, `events_search`: a city (or "city, ST"), a date range in the owner's own days, a category and words.
//! It returns at most MAX_EVENTS events, each trimmed to what a reply needs (the raw answer is about 9 KB an event):
//! name, local date and time, venue, city, on-sale status, genre, a price range when Ticketmaster lists one (it often
//! does not; a missing range is unknown, never free), the ticket link and its widest 16:9 picture (`image`, for a card).
//!
//! What it shares with watch.py: the API root (`watch::TM_API`), the guarded HTTP GET (`watch::http_request`: the
//! address checks, no proxy, a hard deadline) and, when a Watcher runs, its rate limiter, so a search counts against
//! the same 5-a-second, 5000-a-day key as the watches. The key is read from `TICKETMASTER_API_KEY` at each call and
//! is never logged, returned or kept; a failure is reported by its HTTP status only, never with the URL.
//!
//! "Near me" (owner, 2026-10-18: "ticketmaster not working on eve"): "Concerts near me" sent no city, so Ticketmaster
//! searched everywhere (8 of 10000, none local), and the brain's retry three seconds later was refused by the
//! watcher's 20-second gap between two requests to one host. Now a search with no city that is near the owner (`near`,
//! or no city and no keyword: a what's-on search anywhere is never what was meant) goes by `latlong` within
//! NEAR_RADIUS_MI of where they are: the phone's fresh fix (phone_location::fresh), else `EVE_WEATHER_LAT`/`LON`,
//! rounded to NEAR_DECIMALS (about 1 km) as the weather's are. With neither, it searches everywhere and says so. A
//! search keeps a back-off Ticketmaster asked for and is counted, but it does not wait out the watches' pacing: the
//! key allows 5 a second, and the owner is waiting.
//!
//! `search` never fails: no key, a timeout, a refusal and an unreadable answer are each an `ok: false` result
//! with a plain reason. The whole call is bounded by TIMEOUT_S.

use std::sync::{Arc, LazyLock};
use std::time::Duration as StdDuration;

use chrono::{Datelike, DateTime, Duration, FixedOffset, Local, LocalResult, NaiveDate, NaiveDateTime, NaiveTime, Offset, TimeZone, Utc};
use chrono_tz::Tz;
use eve_places::py;
use eve_places::Env;
use eve_web::watch::{self, FetchError, HttpOptions, RateLimiter};
use indexmap::IndexMap;
use regex::Regex;
use serde_json::{json, Map, Value};
use tracing::info;

pub const TOOL_NAME: &str = "events_search";
pub const TOOL_NAMES: [&str; 1] = [TOOL_NAME];
pub const KEY_ENV: &str = "TICKETMASTER_API_KEY";
pub const TIMEOUT_S: f64 = 6.0;
/// "near me": Ticketmaster's radius around the owner, in miles
pub const NEAR_RADIUS_MI: i64 = 50;
/// the owner's position as sent: about 1 km, as the weather's (quick_answers.py)
pub const NEAR_DECIMALS: usize = 2;
pub const MAX_EVENTS: usize = 8;
/// Ticketmaster's segment names, as classificationName takes them; "" searches every kind.
pub const CATEGORIES: [&str; 6] = ["music", "sports", "arts & theatre", "film", "family", ""];
pub const OFF_REASON: &str = "Ticketmaster is not set up on this computer (TICKETMASTER_API_KEY)";
pub const NO_HERE: &str = "eve does not know where the owner is, so these are from anywhere; ask which city";
/// The reason for a date that is not YYYY-MM-DD or a last day before the first.
pub const BAD_DATES: &str = "dates are YYYY-MM-DD, and the last day is not before the first";
pub const UNREADABLE: &str = "Ticketmaster sent an unreadable answer";

// The text was stripped first, so Python's `$` (which also matches before a final newline) is `$` here. Python's
// `\d` takes any Unicode digit, but date.fromisoformat then refuses one: the same ValueError either way.
static DAY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[0-9]{4}-[0-9]{2}-[0-9]{2}$").expect("day pattern"));
static STATE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z]{2}$").expect("state pattern"));

/// The chat tool, key for key as Python's dict (so `json.dumps` of both match).
pub static TOOL: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "function", "name": TOOL_NAME, "strict": true,
        "description": "Find concerts, sports games, theatre, comedy and other ticketed events on Ticketmaster by \
                        place, dates, kind or name. Returns up to 8 upcoming events with venue, local date and time, \
                        price range when listed, and the ticket link.",
        "parameters": {
            "type": "object", "additionalProperties": false,
            "required": ["city", "near", "keyword", "category", "start_date", "end_date"],
            "properties": {
                "city": {"type": "string",
                         "description": "The city, optionally with its state code (\"Seattle\", \"Portland, OR\"); \
                                         empty for near the owner or anywhere."},
                "near": {"type": "boolean",
                         "description": "True when the owner means near where they are now (near me, nearby, around \
                                         here, in town) and names no city. A search with no city and no keyword is \
                                         always near them."},
                "keyword": {"type": "string",
                            "description": "An artist, team, show, venue or genre the owner named; empty for any."},
                "category": {"type": "string", "enum": CATEGORIES,
                             "description": "The kind of event, or empty for every kind."},
                "start_date": {"type": "string",
                               "description": "First day, YYYY-MM-DD in the owner's time zone; empty for from now."},
                "end_date": {"type": "string",
                             "description": "Last day, YYYY-MM-DD in the owner's time zone; empty for no end."}}},
    })
});

pub const INSTRUCTIONS: &str = "Concerts, games, shows and other ticketed events are events_search: what is on in a \
place or on dates (\"anything on in Seattle this weekend\"), when a team plays next, whether an artist is touring or \
coming to a city, and tickets for any of them. A team's schedule (its next game, its next home game) and an artist's \
tour dates are here too: the team or artist goes in keyword, and the city stays empty unless the owner names one (for \
a home game, the team's own city). It reads Ticketmaster directly and answers in about a second. Work out the dates \
from today's date in your note.";
pub const WATCH_HINT: &str = " To be told when tickets for one go on sale, that is watch_add with kind ticketmaster.";

/// `Fetch`: watch.http_request's shape, `(url, headers) -> (status, body)`. An error that is a [`FetchError`]
/// carries its status; a `std::io::Error` of kind `TimedOut` is a timeout; anything else could not reach.
pub type Fetch = Arc<dyn Fn(&str, &[(String, String)]) -> anyhow::Result<(u16, String)> + Send + Sync>;
/// The Watcher's limiter when one runs (`Ok(None)`: none). An error is no limiter, as in Python.
pub type LimiterFn = Arc<dyn Fn() -> anyhow::Result<Option<Arc<dyn Limiter>>> + Send + Sync>;
/// The owner's clock.
pub type NowFn = Arc<dyn Fn() -> OwnerNow + Send + Sync>;
/// Where the owner is (rounded), for "near me"; an error is not knowing.
pub type WhereFn = Arc<dyn Fn() -> anyhow::Result<Option<(f64, f64)>> + Send + Sync>;

/// What `search` asks of the watcher's RateLimiter (watch.py's duck type). Each call may fail: a limiter that
/// cannot answer holds nothing back and a limiter that cannot count costs nothing.
pub trait Limiter: Send + Sync {
    fn backed_off(&self, host: &str) -> anyhow::Result<f64>;
    fn take(&self, host: &str) -> anyhow::Result<()>;
    fn penalize(&self, host: &str, retry_after: Option<f64>) -> anyhow::Result<f64>;
    fn clear(&self, host: &str) -> anyhow::Result<()>;
}

/// The Watcher's own limiter (`WatcherState::limiter`, `Arc<parking_lot::Mutex<RateLimiter>>`).
impl Limiter for parking_lot::Mutex<RateLimiter> {
    fn backed_off(&self, host: &str) -> anyhow::Result<f64> {
        Ok(self.lock().backed_off(host))
    }

    fn take(&self, host: &str) -> anyhow::Result<()> {
        self.lock().take(host);
        Ok(())
    }

    fn penalize(&self, host: &str, retry_after: Option<f64>) -> anyhow::Result<f64> {
        Ok(self.lock().penalize(host, retry_after))
    }

    fn clear(&self, host: &str) -> anyhow::Result<()> {
        self.lock().clear(host);
        Ok(())
    }
}

/// A `limiter` callable that always returns this limiter (the Watcher's, shared with its checks).
pub fn limiter_of(lim: Arc<parking_lot::Mutex<RateLimiter>>) -> LimiterFn {
    Arc::new(move || Ok(Some(lim.clone() as Arc<dyn Limiter>)))
}

/// The tz of an aware Python datetime: a fixed offset (`datetime.timezone`, what `datetime.now().astimezone()`
/// carries) or a real zone (a ZoneInfo).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Zone {
    /// A fixed offset: a day is read in this Mac's local zone (`astimezone` of a naive time).
    Fixed(FixedOffset),
    /// A real zone keeps its own rules.
    Named(Tz),
}

/// Python's aware `now`: the instant and its zone.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OwnerNow {
    pub at: DateTime<Utc>,
    pub zone: Zone,
}

impl OwnerNow {
    /// `datetime.now().astimezone()`: now, with today's fixed local offset.
    pub fn local() -> Self {
        let now = Local::now();
        OwnerNow { at: now.with_timezone(&Utc), zone: Zone::Fixed(now.offset().fix()) }
    }

    /// `datetime(..., tzinfo=ZoneInfo(...))`.
    pub fn named(at: DateTime<Tz>) -> Self {
        OwnerNow { at: at.with_timezone(&Utc), zone: Zone::Named(at.timezone()) }
    }

    /// `datetime(...).astimezone()` or any time with a `datetime.timezone`.
    pub fn fixed(at: DateTime<FixedOffset>) -> Self {
        OwnerNow { at: at.with_timezone(&Utc), zone: Zone::Fixed(*at.offset()) }
    }
}

/// `available(environ)`: whether the key is set: the tool is lent only then. `None` is the process environment.
pub fn available(environ: Option<&Env>) -> bool {
    !py::strip(&env_get(environ, KEY_ENV).unwrap_or_default()).is_empty()
}

/// The brain's line for events_search; the watch hint only when watch_add offers the ticketmaster kind (no name
/// in a prompt that the turn does not offer).
pub fn instructions(watch_ticketmaster: bool) -> String {
    let mut out = INSTRUCTIONS.to_string();
    if watch_ticketmaster {
        out.push_str(WATCH_HINT);
    }
    out
}

fn env_get(environ: Option<&Env>, key: &str) -> Option<String> {
    match environ {
        Some(map) => map.get(key).cloned(),
        None => std::env::var(key).ok(),
    }
}

// ---- the query --------------------------------------------------------------------------------------------------

/// `_utc`: Ticketmaster's startDateTime/endDateTime form: UTC, seconds, a Z, no fraction.
pub fn utc(moment: &DateTime<Utc>) -> String {
    moment.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// A date that is not YYYY-MM-DD, an end before the start, or a day past the calendar (Python's ValueError).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BadQuery(pub String);

/// `_day(raw)`: None for empty, else the date, or BadQuery.
pub fn day(raw: Option<&Value>) -> Result<Option<NaiveDate>, BadQuery> {
    let text = py::truthy_str(raw).unwrap_or_default();
    let text = py::strip(&text);
    if text.is_empty() {
        return Ok(None);
    }
    if !DAY.is_match(text) {
        return Err(BadQuery(text.to_string()));
    }
    // Python's date runs from year 1 to 9999
    NaiveDate::parse_from_str(text, "%Y-%m-%d")
        .ok()
        .filter(|d| (1..=9999).contains(&d.year()))
        .map(Some)
        .ok_or_else(|| BadQuery(text.to_string()))
}

/// A naive local time in `tz`, as Python resolves one with fold=0: an ambiguous time is the first (the earlier
/// instant); a time in a gap takes the offset in force before the gap (ZoneInfo's rule, and `_mktime`'s
/// `max(u1, u2)` for a naive `astimezone()`).
fn resolve<T: TimeZone>(tz: &T, naive: NaiveDateTime) -> DateTime<Utc> {
    match tz.from_local_datetime(&naive) {
        LocalResult::Single(d) => d.with_timezone(&Utc),
        LocalResult::Ambiguous(a, b) => {
            let (a, b) = (a.with_timezone(&Utc), b.with_timezone(&Utc));
            a.min(b)
        }
        LocalResult::None => {
            let before = tz.offset_from_utc_datetime(&(naive - Duration::days(1))).fix();
            Utc.from_utc_datetime(&(naive - Duration::seconds(i64::from(before.local_minus_utc()))))
        }
    }
}

/// The start of `day` in the owner's zone, with that day's own UTC offset. `datetime.now().astimezone()`
/// carries a fixed offset (today's), so a day across a daylight-saving change was an hour off with it (review,
/// 2026-10-08): a fixed offset is read as this Mac's local zone (astimezone of a naive time applies the offset in
/// force on that day); a real zone (a ZoneInfo, in tests and anywhere one is passed) keeps its own rules.
pub fn midnight(day: NaiveDate, zone: Option<Zone>) -> DateTime<Utc> {
    midnight_in(day, zone, &Local)
}

/// `midnight` with the Mac's local zone lent (`Local` in the daemon).
pub fn midnight_in<L: TimeZone>(day: NaiveDate, zone: Option<Zone>, local: &L) -> DateTime<Utc> {
    let naive = day.and_time(NaiveTime::MIN);
    match zone {
        None | Some(Zone::Fixed(_)) => resolve(local, naive),
        Some(Zone::Named(tz)) => resolve(&tz, naive),
    }
}

/// Python's `float(text)` for a setting: whitespace stripped, underscores between digits, inf and nan by name.
fn py_float(text: &str) -> Option<f64> {
    let t = py::strip(text);
    if t.contains('_') {
        let chars: Vec<char> = t.chars().collect();
        let ok = chars.iter().enumerate().all(|(i, &c)| {
            c != '_'
                || (i > 0 && chars[i - 1].is_ascii_digit() && chars.get(i + 1).is_some_and(|n| n.is_ascii_digit()))
        });
        if !ok {
            return None;
        }
        return t.replace('_', "").parse().ok();
    }
    t.parse().ok()
}

/// Where the owner is, for "near me": the phone's fresh fix, else EVE_WEATHER_LAT/LON; rounded to
/// NEAR_DECIMALS. None when neither is known.
pub fn here(environ: Option<&Env>) -> Option<(f64, f64)> {
    // no fix is the configured place, as the weather does
    if let Some(fix) = eve_places::phone_location::fresh(environ) {
        return Some((py::round_to(fix.lat, NEAR_DECIMALS), py::round_to(fix.lon, NEAR_DECIMALS)));
    }
    let lat = py_float(&env_get(environ, "EVE_WEATHER_LAT").unwrap_or_default())?;
    let lon = py_float(&env_get(environ, "EVE_WEATHER_LON").unwrap_or_default())?;
    if !((-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon)) {
        return None;
    }
    Some((py::round_to(lat, NEAR_DECIMALS), py::round_to(lon, NEAR_DECIMALS)))
}

/// `" ".join(str(args.get(key) or "").split())`.
fn words(args: &Map<String, Value>, key: &str) -> String {
    py::split(&py::truthy_str(args.get(key)).unwrap_or_default()).join(" ")
}

/// Python's `text[:n]`: the first n code points.
fn head(text: &str, n: usize) -> String {
    text.chars().take(n).collect()
}

/// Whether a search is near the owner: no city, and `near` or no keyword either.
pub fn wants_near(args: &Map<String, Value>) -> bool {
    if !words(args, "city").is_empty() {
        return false;
    }
    args.get("near") == Some(&Value::Bool(true)) || words(args, "keyword").is_empty()
}

/// `f"{x:.{n}f}"`, Python's spelling of the non-finite.
fn fixed(x: f64, n: usize) -> String {
    if x.is_nan() {
        "nan".into()
    } else if x.is_infinite() {
        if x > 0.0 { "inf".into() } else { "-inf".into() }
    } else {
        format!("{x:.n$}")
    }
}

/// The Discovery API parameters for one search (without the key), in Python's insertion order. `now` is aware, in
/// the owner's zone: a day is that zone's midnight to midnight (each with its own offset, `midnight`), sent in UTC.
/// `near` is the owner's rounded position, used when `wants_near`. BadQuery for a date that is not YYYY-MM-DD or
/// an end before the start.
pub fn query(args: &Map<String, Value>, now: &OwnerNow, near: Option<(f64, f64)>) -> Result<IndexMap<String, String>, BadQuery> {
    query_in(args, now, near, &Local)
}

/// `query` with the Mac's local zone lent (what a fixed-offset `now` reads its days in).
pub fn query_in<L: TimeZone>(
    args: &Map<String, Value>,
    now: &OwnerNow,
    near: Option<(f64, f64)>,
    local: &L,
) -> Result<IndexMap<String, String>, BadQuery> {
    let zone = Some(now.zone);
    let (first, last) = (day(args.get("start_date"))?, day(args.get("end_date"))?);
    let mut start = now.at;
    if let Some(first) = first {
        start = midnight_in(first, zone, local).max(now.at); // "today" starts now: what began this morning is not news
    }
    let mut q: IndexMap<String, String> = IndexMap::new();
    q.insert("size".into(), MAX_EVENTS.to_string());
    q.insert("sort".into(), "date,asc".into());
    q.insert("startDateTime".into(), utc(&start));
    q.insert("includeTBA".into(), "no".into());
    q.insert("includeTBD".into(), "no".into());
    if let Some(last) = last {
        // Python's date + timedelta raises OverflowError past 9999-12-31; that is a bad date here
        let next = last
            .succ_opt()
            .filter(|d| d.year() <= 9999)
            .ok_or_else(|| BadQuery("end past the calendar".into()))?;
        let end = midnight_in(next, zone, local) - Duration::seconds(1);
        if end < start {
            return Err(BadQuery("end before start".into()));
        }
        q.insert("endDateTime".into(), utc(&end));
    }
    let mut city = words(args, "city");
    if let Some((name, rest)) = city.split_once(',') {
        let rest = py::strip(rest);
        if STATE.is_match(rest) {
            q.insert("stateCode".into(), rest.to_uppercase());
        }
        city = py::strip(name).to_string();
    }
    if !city.is_empty() {
        q.insert("city".into(), head(&city, 80));
    } else if let Some((lat, lon)) = near.filter(|_| wants_near(args)) {
        q.insert("latlong".into(), format!("{},{}", fixed(lat, NEAR_DECIMALS), fixed(lon, NEAR_DECIMALS)));
        q.insert("radius".into(), NEAR_RADIUS_MI.to_string());
        q.insert("unit".into(), "miles".into());
    }
    let keyword = words(args, "keyword");
    if !keyword.is_empty() {
        q.insert("keyword".into(), head(&keyword, 120));
    }
    let category = py::strip(&py::truthy_str(args.get("category")).unwrap_or_default()).to_lowercase();
    if !category.is_empty() && CATEGORIES.contains(&category.as_str()) {
        q.insert("classificationName".into(), category);
    }
    Ok(q)
}

/// `urllib.parse.urlencode(pairs)`: `quote_plus` on each key and value (letters, digits and `_.-~` kept, a space
/// is `+`, every other byte `%XX`).
pub fn urlencode<'a>(pairs: impl IntoIterator<Item = (&'a str, &'a str)>) -> String {
    fn quote_plus(s: &str, out: &mut String) {
        for b in s.bytes() {
            match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-' | b'~' => out.push(b as char),
                b' ' => out.push('+'),
                _ => out.push_str(&format!("%{b:02X}")),
            }
        }
    }
    let mut out = String::new();
    for (i, (k, v)) in pairs.into_iter().enumerate() {
        if i > 0 {
            out.push('&');
        }
        quote_plus(k, &mut out);
        out.push('=');
        quote_plus(v, &mut out);
    }
    out
}

// ---- the answer -------------------------------------------------------------------------------------------------

/// Python truthiness of a JSON value.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) | Some(Value::Bool(false)) => false,
        Some(Value::Bool(true)) => true,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

static EMPTY: LazyLock<Map<String, Value>> = LazyLock::new(Map::new);

/// `(x or {})` used as a mapping: an object, else empty. (Python raised AttributeError on a truthy non-mapping; a
/// shape Ticketmaster never sends is read as missing here.)
fn obj(v: Option<&Value>) -> &Map<String, Value> {
    match v {
        Some(Value::Object(m)) => m,
        _ => &EMPTY,
    }
}

/// `str(x or "")`.
fn text(v: Option<&Value>) -> String {
    py::truthy_str(v).unwrap_or_default()
}

/// `_first(items)`: the first item of a list when it is an object, else {}.
fn first(items: Option<&Value>) -> &Map<String, Value> {
    match items {
        Some(Value::Array(a)) => match a.first() {
            Some(Value::Object(m)) => m,
            _ => &EMPTY,
        },
        _ => &EMPTY,
    }
}

/// `_name(raw)`.
fn name(raw: Option<&Value>) -> String {
    match raw {
        Some(Value::Object(m)) => text(m.get("name")),
        _ => String::new(),
    }
}

/// Python's `float(x)` of a JSON value; None where it raises (None, a list, an object, a string that is no number).
fn float_of(x: Option<&Value>) -> Option<f64> {
    match x? {
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::Number(n) => n.as_f64(),
        Value::String(s) => py_float(s),
        _ => None,
    }
}

/// `_money(x)`: whole amounts with no decimals, others with two; "" for what is no number. (A non-finite amount
/// made Python's `int(v)` raise; it is no amount here.)
pub fn money(x: Option<&Value>) -> String {
    let Some(v) = float_of(x).filter(|v| v.is_finite()) else {
        return String::new();
    };
    if v == v.trunc() {
        format!("{v:.0}")
    } else {
        format!("{v:.2}")
    }
}

/// Python's `int(x or 0)` of a JSON value; None where it raises.
fn int_of(x: Option<&Value>) -> Option<i64> {
    if !truthy(x) {
        return Some(0);
    }
    match x? {
        Value::Bool(b) => Some(i64::from(*b)),
        Value::Number(n) => match n.as_i64() {
            Some(i) => Some(i),
            None => n.as_f64().filter(|f| f.is_finite()).map(|f| f.trunc() as i64),
        },
        Value::String(s) => {
            let t = py::strip(s);
            let digits = t.strip_prefix(['+', '-']).unwrap_or(t);
            let chars: Vec<char> = digits.chars().collect();
            let well_formed = !chars.is_empty()
                && chars.iter().enumerate().all(|(i, &c)| {
                    c.is_ascii_digit()
                        || (c == '_' && i > 0 && chars[i - 1].is_ascii_digit()
                            && chars.get(i + 1).is_some_and(|n| n.is_ascii_digit()))
                });
            if !well_formed {
                return None;
            }
            t.replace('_', "").parse().ok()
        }
        _ => None,
    }
}

/// Ticketmaster's widest 16:9 picture (its listings carry several sizes), https only; "" when none is wide enough.
pub fn best_image(images: Option<&Value>) -> String {
    let empty = Vec::new();
    let list = match images {
        Some(Value::Array(a)) => a,
        _ => &empty,
    };
    let mut best: Option<(i64, String)> = None;
    for i in list {
        let Value::Object(i) = i else { continue };
        let url = text(i.get("url"));
        if !url.starts_with("https://") || i.get("ratio") != Some(&Value::String("16_9".into())) {
            continue;
        }
        // a width int() cannot read made Python raise; such a picture is not taken
        let Some(width) = int_of(i.get("width")) else { continue };
        if width < 640 || truthy(i.get("fallback")) {
            continue;
        }
        // max() keeps the first of equals
        if best.as_ref().is_none_or(|(w, _)| width > *w) {
            best = Some((width, text(i.get("url"))));
        }
    }
    best.map(|(_, u)| u).unwrap_or_default()
}

/// One event as the brain needs it. Fields Ticketmaster left out are left out (or empty), never guessed.
pub fn trim(event: &Map<String, Value>) -> Map<String, Value> {
    let venue = first(obj(event.get("_embedded")).get("venues"));
    let dates = obj(event.get("dates"));
    let start = obj(dates.get("start"));
    let status = text(obj(dates.get("status")).get("code"));
    let cls = first(event.get("classifications"));
    let mut genre = name(cls.get("genre"));
    if genre.is_empty() {
        genre = name(cls.get("segment"));
    }
    let city: Vec<String> = [name(venue.get("city")), text(obj(venue.get("state")).get("stateCode"))]
        .into_iter()
        .filter(|x| !x.is_empty())
        .collect();
    let mut out = Map::new();
    out.insert("name".into(), json!(text(event.get("name"))));
    out.insert("date".into(), json!(text(start.get("localDate"))));
    out.insert("time".into(), json!(head(&text(start.get("localTime")), 5)));
    out.insert("venue".into(), json!(text(venue.get("name")))); // _name of the venue, always a mapping
    out.insert("city".into(), json!(city.join(", ")));
    out.insert("status".into(), json!(status));
    out.insert("genre".into(), json!(if genre.to_lowercase() == "undefined" { String::new() } else { genre }));
    out.insert("url".into(), json!(text(event.get("url"))));
    let image = best_image(event.get("images"));
    if !image.is_empty() {
        out.insert("image".into(), json!(image)); // for a card's picture (OpenUI ImageBlock / ListBlock image)
    }
    let price = first(event.get("priceRanges"));
    let (lo, hi) = (money(price.get("min")), money(price.get("max")));
    if !lo.is_empty() || !hi.is_empty() {
        let cur = text(price.get("currency")).to_uppercase();
        let range = if lo == hi || hi.is_empty() {
            lo
        } else if !lo.is_empty() {
            format!("{lo}-{hi}")
        } else {
            hi
        };
        out.insert("price".into(), json!(format!("{}{range}", if cur.is_empty() { String::new() } else { format!("{cur} ") })));
    }
    out
}

/// The tool's result for a Discovery answer: the trimmed events and how many there are in all.
pub fn parse(payload: &Value) -> Value {
    let Value::Object(payload) = payload else {
        return json!({"ok": false, "reason": UNREADABLE});
    };
    let events: Vec<Value> =
        watch::ticketmaster::tm_events(payload).iter().map(|e| Value::Object(trim(e))).take(MAX_EVENTS).collect();
    let total = obj(payload.get("page")).get("totalElements");
    let mut out = Map::new();
    out.insert("ok".into(), json!(true));
    let none = events.is_empty();
    out.insert("events".into(), Value::Array(events));
    // an int, not a bool (serde keeps the two apart)
    if let Some(Value::Number(n)) = total {
        if n.is_i64() || n.is_u64() {
            out.insert("total".into(), Value::Number(n.clone()));
        }
    }
    if none {
        out.insert("note".into(), json!("Ticketmaster lists no events for that search"));
    }
    Value::Object(out)
}

/// What went wrong with a request, as `_reason` and the back-off read it.
enum Failure {
    Timeout,
    Fetch(FetchError),
    Other,
}

impl Failure {
    fn of(e: &anyhow::Error) -> Self {
        if let Some(f) = e.downcast_ref::<FetchError>() {
            return Failure::Fetch(f.clone());
        }
        if e.downcast_ref::<tokio::time::error::Elapsed>().is_some() {
            return Failure::Timeout;
        }
        if e.downcast_ref::<std::io::Error>().is_some_and(|io| io.kind() == std::io::ErrorKind::TimedOut) {
            return Failure::Timeout;
        }
        Failure::Other
    }

    fn status(&self) -> u16 {
        match self {
            Failure::Fetch(f) => f.status,
            _ => 0,
        }
    }

    fn retry_after(&self) -> Option<f64> {
        match self {
            Failure::Fetch(f) => f.retry_after,
            _ => None,
        }
    }

    /// `_reason(e)`: a failure in words, by status only: never the URL (it carries the key).
    fn reason(&self) -> String {
        let status = self.status();
        if matches!(self, Failure::Timeout) {
            return "Ticketmaster did not answer in time".into();
        }
        if status == 401 || status == 403 {
            return "Ticketmaster refused the key (TICKETMASTER_API_KEY)".into();
        }
        if status == 429 {
            return "Ticketmaster asked to slow down; try again in a minute".into();
        }
        if status != 0 {
            return format!("Ticketmaster answered HTTP {status}");
        }
        if let Failure::Fetch(f) = self {
            if f.reason.contains("too long") {
                return "Ticketmaster did not answer in time".into();
            }
        }
        "Ticketmaster could not be reached".into()
    }
}

/// `_reason(e)` for one error.
pub fn reason(e: &anyhow::Error) -> String {
    Failure::of(e).reason()
}

/// `Events(...)`'s keyword arguments. `Default` is Python's: watch.http_request with `timeout_s`, no limiter,
/// `datetime.now().astimezone()`, the process environment, TIMEOUT_S, offloaded, `here(environ)`.
#[derive(Clone)]
pub struct EventsOptions {
    pub fetch: Option<Fetch>,
    pub limiter: Option<LimiterFn>,
    pub now: Option<NowFn>,
    pub environ: Option<Env>,
    pub timeout_s: f64,
    pub offload: bool,
    pub r#where: Option<WhereFn>,
}

impl Default for EventsOptions {
    fn default() -> Self {
        EventsOptions { fetch: None, limiter: None, now: None, environ: None, timeout_s: TIMEOUT_S, offload: true, r#where: None }
    }
}

/// `events_search` for the chat brain. `fetch` is watch.http_request's shape (a fake in tests); `limiter` returns
/// the Watcher's RateLimiter when one runs, so a search is counted with the watches.
#[derive(Clone)]
pub struct Events {
    fetch: Fetch,
    limiter: LimiterFn,
    now: NowFn,
    env: Option<Env>,
    timeout: f64,
    offload: bool,
    r#where: WhereFn,
}

impl Default for Events {
    fn default() -> Self {
        Events::new(EventsOptions::default())
    }
}

impl Events {
    pub fn new(opts: EventsOptions) -> Self {
        let timeout_s = opts.timeout_s;
        let fetch = opts.fetch.unwrap_or_else(|| {
            Arc::new(move |url: &str, headers: &[(String, String)]| {
                let o = HttpOptions { headers: headers.to_vec(), timeout: timeout_s, ..HttpOptions::default() };
                watch::http_request(url, &o).map_err(anyhow::Error::from)
            })
        });
        let env = opts.environ;
        let r#where = opts.r#where.unwrap_or_else(|| {
            let env = env.clone();
            Arc::new(move || Ok(here(env.as_ref())))
        });
        Events {
            fetch,
            limiter: opts.limiter.unwrap_or_else(|| Arc::new(|| Ok(None))),
            now: opts.now.unwrap_or_else(|| Arc::new(OwnerNow::local)),
            env,
            timeout: timeout_s,
            offload: opts.offload,
            r#where,
        }
    }

    pub fn enabled(&self) -> bool {
        available(self.env.as_ref())
    }

    /// `_limiter_now`: no limiter is no limit here.
    fn limiter_now(&self) -> Option<Arc<dyn Limiter>> {
        (self.limiter)().ok().flatten()
    }

    pub fn tools(&self) -> Vec<Value> {
        if self.enabled() {
            vec![TOOL.clone()]
        } else {
            Vec::new()
        }
    }

    pub async fn handle(&self, name: &str, args: &Map<String, Value>) -> Value {
        if name != TOOL_NAME {
            return json!({"ok": false, "reason": "not an events tool"});
        }
        self.search(args).await
    }

    /// One search. Never fails.
    pub async fn search(&self, args: &Map<String, Value>) -> Value {
        let key = py::strip(&env_get(self.env.as_ref(), KEY_ENV).unwrap_or_default()).to_string();
        if key.is_empty() {
            return json!({"ok": false, "reason": OFF_REASON});
        }
        let mut near = None;
        if wants_near(args) {
            // not knowing where is searching everywhere, and saying so
            near = (self.r#where)().ok().flatten();
        }
        let q = match query(args, &(self.now)(), near) {
            Ok(q) => q,
            Err(_) => return json!({"ok": false, "reason": BAD_DATES}),
        };
        let pairs = std::iter::once(("apikey", key.as_str())).chain(q.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        let url = format!("{}/events.json?{}", watch::TM_API, urlencode(pairs));
        let limiter = self.limiter_now();
        if let Some(lim) = &limiter {
            // The watcher's limiter (review, 2026-10-08): a back-off Ticketmaster asked for is kept (a request now
            // would only be refused again and lengthen it) and each search is counted with the watches' requests.
            // The watches' own pacing (20 s between two requests to one host) is not waited: it refused the
            // owner's retry three seconds after a first search (2026-10-18), and the key allows 5 a second.
            let held = seconds(lim.as_ref());
            if held > 0.0 {
                return json!({"ok": false, "reason": format!("Ticketmaster asked to slow down; try again in {} s", round_up(held))});
            }
            // a limiter that cannot count costs nothing here
            let _ = lim.take(watch::TM_HOST);
        }
        let headers = vec![("Accept".to_string(), "application/json".to_string())];
        let wait = deadline(self.timeout + 0.5);
        let got: anyhow::Result<(u16, String)> = if self.offload {
            let fetch = self.fetch.clone();
            let call = tokio::task::spawn_blocking(move || fetch(&url, &headers));
            match tokio::time::timeout(wait, call).await {
                Ok(Ok(r)) => r,
                Ok(Err(join)) => Err(anyhow::anyhow!("the fetch failed: {join}")),
                Err(elapsed) => Err(elapsed.into()),
            }
        } else {
            // run on the loop (tests' fakes): the deadline is checked after it returns, as wait_for does
            let fetch = self.fetch.clone();
            match tokio::time::timeout(wait, async move { fetch(&url, &headers) }).await {
                Ok(r) => r,
                Err(elapsed) => Err(elapsed.into()),
            }
        };
        let payload = match got {
            Ok((_status, body)) => match serde_json::from_str::<Value>(&body) {
                Ok(v) => v,
                Err(_) => return json!({"ok": false, "reason": UNREADABLE}),
            },
            Err(e) => {
                // by status only, never the URL
                let failure = Failure::of(&e);
                let reason = failure.reason();
                let status = failure.status();
                let retry_after = failure.retry_after();
                // retry_after is truthy: present and not 0
                let asked = retry_after.is_some_and(|r| r != 0.0);
                if let Some(lim) = &limiter {
                    if status == 429 || status == 503 || (status >= 500 && asked) {
                        // Ticketmaster said slow down: the shared key backs off, for the watches too (as watch.py does)
                        let _ = lim.penalize(watch::TM_HOST, retry_after);
                    }
                }
                info!("events: search failed ({reason})");
                return json!({"ok": false, "reason": reason});
            }
        };
        if let Some(lim) = &limiter {
            let _ = lim.clear(watch::TM_HOST); // an answer ends a back-off, as a watch's success does
        }
        let mut out = parse(&payload);
        let latlong = q.contains_key("latlong");
        if let Value::Object(o) = &mut out {
            if o.get("ok") == Some(&Value::Bool(true)) && wants_near(args) {
                if latlong {
                    o.insert("near".into(), json!(format!("within {NEAR_RADIUS_MI} miles of where the owner is")));
                } else {
                    o.insert("note".into(), json!(NO_HERE));
                }
            }
        }
        let n = out.get("events").and_then(Value::as_array).map(Vec::len).unwrap_or(0);
        let total = out.get("total").map(|t| t.to_string()).unwrap_or_else(|| "?".into());
        info!("events: {n} of {total} events{}", if latlong { " near the owner" } else { "" });
        out
    }
}

/// `asyncio.wait_for`'s timeout as a Duration: nothing left (0 or less, or NaN) is now; an infinite one never ends.
fn deadline(secs: f64) -> StdDuration {
    if secs.is_nan() || secs <= 0.0 {
        return StdDuration::ZERO;
    }
    StdDuration::try_from_secs_f64(secs).unwrap_or(StdDuration::MAX)
}

/// `_seconds(limiter, "backed_off")`: the limiter's back-off for Ticketmaster's host, in seconds; 0 when the
/// limiter cannot say.
fn seconds(limiter: &dyn Limiter) -> f64 {
    match limiter.backed_off(watch::TM_HOST) {
        Ok(v) if v > 0.0 => v,
        _ => 0.0,
    }
}

/// `_round(secs)`: whole seconds, rounded up, at least 1.
fn round_up(secs: f64) -> i64 {
    // `as` saturates where Python's int() of an infinity raised
    ((secs + 0.999) as i64).max(1)
}

#[cfg(test)]
mod unit {
    use super::*;

    #[test]
    fn money_is_whole_or_two_decimals_as_python_formats_it() {
        assert_eq!(money(Some(&json!(35))), "35");
        assert_eq!(money(Some(&json!(89.5))), "89.50");
        assert_eq!(money(Some(&json!(0.125))), "0.12"); // the exact binary value, ties to even
        assert_eq!(money(Some(&json!(2.675))), "2.67");
        assert_eq!(money(Some(&json!(" 40 "))), "40");
        assert_eq!(money(Some(&json!("1_000.5"))), "1000.50");
        assert_eq!(money(Some(&json!(true))), "1");
        assert_eq!(money(Some(&json!("x"))), "");
        assert_eq!(money(Some(&Value::Null)), "");
        assert_eq!(money(None), "");
        assert_eq!(money(Some(&json!(1e20))), "100000000000000000000");
    }

    #[test]
    fn urlencode_is_quote_plus() {
        assert_eq!(urlencode([("a b", "date,asc"), ("t", "2026-10-10T07:00:00Z"), ("x", "~_.-*&é")]),
                   "a+b=date%2Casc&t=2026-10-10T07%3A00%3A00Z&x=~_.-%2A%26%C3%A9");
    }

    #[test]
    fn a_midnight_in_a_gap_takes_the_offset_before_it() {
        // Santiago springs forward at 2026-09-06 00:00 (to 01:00, -04 to -03): Python's fold=0 is -04
        let tz: Tz = "America/Santiago".parse().unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 9, 6).unwrap();
        assert_eq!(utc(&midnight(d, Some(Zone::Named(tz)))), "2026-09-06T04:00:00Z");
        // and an ambiguous one is the first: Havana falls back at 2026-11-01 01:00 to 00:00 (-04 to -05)
        let tz: Tz = "America/Havana".parse().unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 11, 1).unwrap();
        assert_eq!(utc(&midnight(d, Some(Zone::Named(tz)))), "2026-11-01T04:00:00Z");
    }

    #[test]
    fn int_and_float_read_as_python_does() {
        assert_eq!(int_of(Some(&json!("640"))), Some(640));
        assert_eq!(int_of(Some(&json!(" 1_024 "))), Some(1024));
        assert_eq!(int_of(Some(&json!(640.9))), Some(640));
        assert_eq!(int_of(Some(&json!(null))), Some(0));
        assert_eq!(int_of(Some(&json!("wide"))), None);
        assert_eq!(py_float(" 40.5 "), Some(40.5));
        assert_eq!(py_float("_1"), None);
        assert!(py_float("nan").is_some_and(f64::is_nan));
        assert_eq!(py_float(""), None);
    }

    #[test]
    fn the_last_day_of_the_calendar_is_a_bad_date_not_a_crash() {
        let now = OwnerNow::named(chrono_tz::America::Los_Angeles.with_ymd_and_hms(2026, 10, 8, 18, 30, 0).unwrap());
        let args = json!({"end_date": "9999-12-31"});
        assert!(query(args.as_object().unwrap(), &now, None).is_err());
        let args = json!({"start_date": "2026-02-30"});
        assert!(query(args.as_object().unwrap(), &now, None).is_err());
    }
}
