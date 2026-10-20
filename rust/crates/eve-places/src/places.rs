//! Places: where the owner goes, how long they stay, what they do by habit, and where they left things.
//!
//! Owner, 2026-10-18: "make a location service that tracks location ... digests it tracks patterns habits where
//! things are ... cuz rn u get pure coordinates". Until then eve kept only the phone's latest fix (phone_location)
//! and the brain saw a latitude and a longitude. This module keeps the history on this Mac and turns it into words:
//! "Home since 18:40", "Equinox, usually Mon, Wed and Fri around 07:10, about 1 h", "yesterday: Home, Work
//! 9:10-17:45, Equinox".
//!
//! **What comes in.** Two kinds of evidence, both from the Eve Link app over the link (link_chat):
//!
//! - every fix the phone sends to `POST /api/link/location` (`record_fix`): foreground updates, and in the background
//!   iOS's significant-change moves (about 500 m);
//! - iOS's visits (`POST /api/link/visit`, `record_visit`): CLVisit's arrival and departure at a place, which iOS
//!   works out itself at almost no battery cost, with Always permission. The phone adds the nearest point of interest
//!   (Apple's MapKit, within 60 m) so a place has a name like "Equinox" and a kind like "fitness center".
//!
//! A visit from iOS is the better evidence. Without one (While Using permission, or iOS not reporting) a stay is
//! read from the fixes: fixes within STAY_RADIUS_M of each other for at least MIN_STAY_S are a visit, and when a fix
//! lands far away the stay ends at that fix minus the travel time at TRAVEL_MPS (a significant-change fix means the
//! phone did not move far before it). An iOS visit that covers the same time replaces the stays read from fixes.
//!
//! **Places.** A visit joins the nearest known place within PLACE_RADIUS_M (wider for a vague fix, up to
//! MAX_MATCH_RADIUS_M) or the same point of interest within POI_MATCH_M, else it starts a new one. Home and work are
//! inferred (`infer_labels`): home is where the nights are spent, work is where weekday days are spent, each over
//! the last LABEL_DAYS days. The owner's own name for a place ("Mom's", `name_place`) wins over both.
//!
//! **Habits.** `patterns`: for each place over the last PATTERN_WEEKS weeks, the weekdays it is usually visited on,
//! the usual arrival time and the usual stay; and the home routine (when the owner leaves home and gets back on
//! weekdays). Plain medians and counts, no model: a habit is something that happened, not a guess.
//!
//! **Digests.** `digest(day)`: the day's places in order with times, time at home, at work and out, about how far
//! the phone moved, the places that were new, and the usual places that were missed. `week(day)`: the week's top
//! places, new places and the routines. Both are computed when asked, so a visit the phone sends late is never left
//! out.
//!
//! **Where things are.** `remember_thing`: "my car is here", "I left my jacket at Sam's" keeps the thing at where
//! the owner is now (or a named place); `find_thing` says where and how far from here.
//!
//! **Privacy.** Everything is one SQLite file on this Mac (`DEFAULT_FILE`, `EVE_PLACES_FILE` moves it), owner-only
//! (0600). Nothing here sends anything anywhere; the brain's tools answer with place names and times. Raw fixes are
//! kept FIX_DAYS days, visits and places `EVE_PLACES_DAYS` (KEEP_DAYS by default), things until forgotten. `forget`
//! wipes all of it, with the latest fix (the app's Settings, `eve-bridge places forget`). Logs carry counts, never a
//! place or a coordinate. `EVE_PLACES=0` turns all of it off: nothing is kept and no tool is offered.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, LazyLock};
use std::time::Duration as StdDuration;

use chrono::offset::LocalResult;
use chrono::{DateTime, Datelike, Duration, FixedOffset, NaiveDate, NaiveDateTime, NaiveTime, Offset, TimeZone, Timelike, Utc};
use indexmap::IndexMap;
use parking_lot::Mutex;
use regex::Regex;
use rusqlite::{params, Connection, OptionalExtension, Row, Transaction};
use serde_json::{json, Map, Value};

use crate::phone_location::{self, Fix};
use crate::{env_get, expanduser, py, Env};

pub const SWITCH: &str = "EVE_PLACES";
pub const FILE_ENV: &str = "EVE_PLACES_FILE";
pub const DAYS_ENV: &str = "EVE_PLACES_DAYS";
pub const DEFAULT_FILE: &str = "~/.config/eve-bridge/places.db";
/// Visits and places.
pub const KEEP_DAYS: i64 = 365;
/// Raw fixes: the stays are read from them as they come.
pub const FIX_DAYS: i64 = 30;
/// Fixes this close together are one stay.
pub const STAY_RADIUS_M: f64 = 150.0;
/// Shorter is passing through.
pub const MIN_STAY_S: f64 = 10.0 * 60.0;
/// 50 km/h: a stay read from fixes ends this long before the far fix.
pub const TRAVEL_MPS: f64 = 13.9;
/// A visit this close to a place is at that place.
pub const PLACE_RADIUS_M: f64 = 120.0;
/// A vague fix widens the match this far, no further.
pub const MAX_MATCH_RADIUS_M: f64 = 250.0;
/// The same point of interest this close is the same place.
pub const POI_MATCH_M: f64 = 300.0;
/// A fix vaguer than this says nothing about a stay.
pub const VAGUE_FIX_M: f64 = 1000.0;
/// iOS's departure carries the arrival it ends; this close is the same visit.
pub const VISIT_SAME_S: f64 = 120.0;
pub const LABEL_DAYS: i64 = 21;
/// Nights (home) or weekdays (work) with at least LABEL_MIN_HOURS there.
pub const LABEL_MIN_DAYS: usize = 3;
pub const LABEL_MIN_HOURS: f64 = 2.0;
/// In LABEL_DAYS.
pub const WORK_MIN_HOURS: f64 = 10.0;
pub const PATTERN_WEEKS: i64 = 8;
pub const MAX_TIMELINE: usize = 60;
pub const MAX_THINGS: i64 = 200;
pub const MAX_NAME_CHARS: usize = 60;
pub const MAX_NOTE_CHARS: usize = 200;
pub const LABEL_TITLES: [(&str, &str); 2] = [("home", "Home"), ("work", "Work")];
pub const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

pub const OFF_REASON: &str = "Places are off on this Mac (EVE_PLACES=0).";
pub const NO_HERE: &str = "the phone has not shared where it is in the last hour";

const WEEK_S: f64 = 7.0 * 86400.0;

static DAY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d{4}-\d{2}-\d{2}$").expect("day pattern"));
static NOT_WORD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[^\w\s]").expect("word pattern"));
static LETTERS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z]+$").expect("letters pattern"));
static PY_INT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[+-]?[0-9]+(?:_[0-9]+)*$").expect("int pattern"));
/// One connection at a time in this process.
static LOCK: Mutex<()> = Mutex::new(());

// ---- errors --------------------------------------------------------------------------------------------------------

/// What a store call can fail with: Python's ValueError (its message is the tool's reason), sqlite3.Error, OSError.
#[derive(Debug, thiserror::Error)]
pub enum PlacesError {
    #[error("{0}")]
    Value(String),
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Other(String),
}

impl PlacesError {
    /// The exception class name Python's log lines carry (`type(e).__name__`).
    pub fn kind_name(&self) -> &'static str {
        match self {
            PlacesError::Value(_) => "ValueError",
            PlacesError::Db(rusqlite::Error::SqliteFailure(e, _)) => match e.code {
                rusqlite::ErrorCode::ConstraintViolation => "IntegrityError",
                rusqlite::ErrorCode::NotADatabase | rusqlite::ErrorCode::DatabaseCorrupt => "DatabaseError",
                _ => "OperationalError",
            },
            PlacesError::Db(_) => "OperationalError",
            PlacesError::Io(_) => "OSError",
            PlacesError::Other(_) => "KeyError",
        }
    }
}

type Res<T> = Result<T, PlacesError>;

// ---- switches and paths --------------------------------------------------------------------------------------------

/// On unless EVE_PLACES is 0/false/no/off, and only while the phone's location is on.
pub fn enabled(environ: Option<&Env>) -> bool {
    let raw = env_get(environ, SWITCH).unwrap_or_default();
    !matches!(raw.trim().to_lowercase().as_str(), "0" | "false" | "no" | "off") && phone_location::enabled(environ)
}

pub fn state_path(environ: Option<&Env>) -> PathBuf {
    let raw = env_get(environ, FILE_ENV)
        .filter(|v| !v.is_empty())
        .or_else(|| std::env::var(FILE_ENV).ok().filter(|v| !v.is_empty()))
        .unwrap_or_else(|| DEFAULT_FILE.to_string());
    let raw = raw.trim();
    expanduser(if raw.is_empty() { "." } else { raw })
}

pub fn keep_days(environ: Option<&Env>) -> i64 {
    let raw = env_get(environ, DAYS_ENV).unwrap_or_default();
    let raw = raw.trim();
    let days = if raw.is_empty() {
        KEEP_DAYS
    } else if PY_INT.is_match(raw) {
        let digits = raw.replace('_', "");
        // Python's int has no ceiling: a huge value clamps like any other
        digits.parse::<i64>().unwrap_or(if digits.starts_with('-') { i64::MIN } else { i64::MAX })
    } else {
        return KEEP_DAYS;
    };
    days.clamp(FIX_DAYS, 3650)
}

/// The owner's zone: EVE_TIMEZONE, else this Mac's (its offset now, as Python's `datetime.now().astimezone()`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Zone {
    Named(chrono_tz::Tz),
    Fixed(FixedOffset),
}

impl Zone {
    /// `datetime.fromtimestamp(ts, zone)`, as a wall time.
    pub fn local(&self, ts: f64) -> NaiveDateTime {
        let utc = py::from_timestamp(ts);
        match self {
            Zone::Named(tz) => utc.with_timezone(tz).naive_local(),
            Zone::Fixed(off) => utc.with_timezone(off).naive_local(),
        }
    }

    /// `datetime.combine(day, t, tzinfo=zone).timestamp()`, with Python's fold=0: an ambiguous wall time is its
    /// first occurrence and one in a gap takes the offset from before the gap.
    pub fn timestamp(&self, naive: NaiveDateTime) -> f64 {
        let off_secs = match self {
            Zone::Fixed(off) => off.local_minus_utc(),
            Zone::Named(tz) => match tz.from_local_datetime(&naive) {
                LocalResult::Single(dt) => dt.offset().fix().local_minus_utc(),
                LocalResult::Ambiguous(first, _) => first.offset().fix().local_minus_utc(),
                LocalResult::None => tz.offset_from_utc_datetime(&(naive - Duration::days(1))).fix().local_minus_utc(),
            },
        };
        let utc = naive - Duration::seconds(i64::from(off_secs));
        py::timestamp(&utc.and_utc())
    }
}

pub fn zone(environ: Option<&Env>) -> Zone {
    let name = env_get(environ, "EVE_TIMEZONE").unwrap_or_default();
    let name = name.trim();
    if !name.is_empty() {
        if let Ok(tz) = name.parse::<chrono_tz::Tz>() {
            return Zone::Named(tz);
        }
        // zoneinfo reads the zone file, and this Mac's file system does not mind the case
        if let Ok(tz) = chrono_tz::Tz::from_str_insensitive(name) {
            return Zone::Named(tz);
        }
    }
    Zone::Fixed(chrono::Local::now().offset().fix())
}

// ---- geometry and words --------------------------------------------------------------------------------------------

/// Great-circle distance in meters (haversine).
pub fn distance_m(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let r = 6_371_008.8;
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let (dp, dl) = ((lat2 - lat1).to_radians(), (lon2 - lon1).to_radians());
    let h = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    2.0 * r * h.sqrt().min(1.0).asin()
}

/// One clean line of at most `limit` characters, None when empty; Err with Python's ValueError message.
pub fn clean(raw: Option<&str>, limit: usize) -> Result<Option<String>, String> {
    let Some(raw) = raw else { return Ok(None) };
    let text = py::one_line(raw);
    if py::chars_len(&text) > limit {
        return Err(format!("is at most {limit} characters"));
    }
    Ok((!text.is_empty()).then_some(text))
}

/// `clean` for a JSON value: anything but a string or null is refused.
fn clean_value(raw: Option<&Value>, limit: usize) -> Result<Option<String>, String> {
    match raw {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => clean(Some(s), limit),
        Some(_) => Err("must be a string or null".into()),
    }
}

/// '45 min', '2 h 10 min', '3 h'.
pub fn duration(secs: f64) -> String {
    let minutes = py::round_int(secs / 60.0).max(1);
    if minutes < 60 {
        return format!("{minutes} min");
    }
    let (h, m) = (minutes / 60, minutes % 60);
    if m == 0 {
        format!("{h} h")
    } else {
        format!("{h} h {m} min")
    }
}

pub fn clock(minute_of_day: f64) -> String {
    let m = py::round_int(minute_of_day).rem_euclid(24 * 60);
    format!("{:02}:{:02}", m / 60, m % 60)
}

pub fn distance_words(m: f64) -> String {
    if m < 1000.0 {
        return format!("about {} m", (py::round_int(m / 10.0) * 10).max(10));
    }
    if m < 10_000.0 {
        format!("about {:.1} km", m / 1000.0)
    } else {
        format!("about {} km", py::round_int(m / 1000.0))
    }
}

/// 'weekdays', 'weekends', 'every day', 'Mon, Wed and Fri'.
pub fn days_words(days: impl IntoIterator<Item = u32>) -> String {
    let mut ds: Vec<u32> = days.into_iter().collect::<HashSet<_>>().into_iter().collect();
    ds.sort_unstable();
    if ds == (0..7).collect::<Vec<_>>() {
        return "every day".into();
    }
    if ds == [0, 1, 2, 3, 4] {
        return "weekdays".into();
    }
    if ds == [5, 6] {
        return "weekends".into();
    }
    let names: Vec<&str> = ds.iter().map(|&d| WEEKDAYS[d as usize]).collect();
    match names.split_last() {
        None => String::new(),
        Some((last, [])) => last.to_string(),
        Some((last, rest)) => format!("{} and {}", rest.join(", "), last),
    }
}

/// MapKit's 'fitnessCenter' as 'fitness center'.
pub fn category_words(raw: Option<&str>) -> Option<String> {
    let raw = raw.filter(|r| !r.is_empty())?;
    let mut out = String::new();
    let mut prev: Option<char> = None;
    for c in raw.chars() {
        if c.is_ascii_uppercase() && prev.is_some_and(|p| p.is_ascii_lowercase()) {
            out.push(' ');
        }
        out.push(c);
        prev = Some(c);
    }
    Some(out.to_lowercase())
}

// ---- the evidence --------------------------------------------------------------------------------------------------

/// iOS's CLVisit, as the app posts it: an arrival (departed None), a departure, or both.
#[derive(Debug, Clone, PartialEq)]
pub struct Visit {
    pub lat: f64,
    pub lon: f64,
    pub accuracy_m: f64,
    pub arrived: Option<DateTime<Utc>>,
    pub departed: Option<DateTime<Utc>>,
    /// The reverse-geocoded line.
    pub place: Option<String>,
    /// The nearest point of interest's name.
    pub poi: Option<String>,
    /// Its MapKit category, e.g. fitnessCenter.
    pub category: Option<String>,
}

/// A body that is not a visit; its message is the route's 400 reason (no value from the body in it).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct BadVisit(pub String);

fn when(raw: Option<&Value>, key: &str) -> Result<Option<DateTime<Utc>>, BadVisit> {
    match raw {
        None | Some(Value::Null) => Ok(None),
        Some(v) => phone_location::when(Some(v))
            .map(Some)
            .map_err(|_| BadVisit(format!("{key} must be an ISO 8601 time with a zone, or null"))),
    }
}

/// The app's visit body, or BadVisit with the reason.
pub fn parse_visit(body: &Value, now: Option<DateTime<Utc>>) -> Result<Visit, BadVisit> {
    let Value::Object(body) = body else { return Err(BadVisit("the body must be a JSON object".into())) };
    let num = |key: &str, low: f64, high: f64| {
        phone_location::number(body, key, low, high, false).map(Option::unwrap_or_default).map_err(|e| BadVisit(e.0))
    };
    let lat = num("lat", -90.0, 90.0)?;
    let lon = num("lon", -180.0, 180.0)?;
    let acc = num("accuracy_m", 0.0, phone_location::MAX_ACCURACY_M)?;
    let arrived = when(body.get("arrived"), "arrived")?;
    let departed = when(body.get("departed"), "departed")?;
    if arrived.is_none() && departed.is_none() {
        return Err(BadVisit("a visit needs arrived or departed".into()));
    }
    let now = now.unwrap_or_else(Utc::now);
    for t in [arrived, departed].into_iter().flatten() {
        let ahead = (t - now).num_microseconds().unwrap_or(i64::MAX) as f64 / 1e6;
        if ahead > phone_location::FUTURE_SECS {
            return Err(BadVisit("a visit's time is in the future".into()));
        }
    }
    if let (Some(a), Some(d)) = (arrived, departed) {
        if d < a {
            return Err(BadVisit("departed is before arrived".into()));
        }
    }
    let texts = (|| -> Result<_, String> {
        Ok((
            clean_value(body.get("place"), phone_location::MAX_PLACE_CHARS)?,
            clean_value(body.get("poi"), phone_location::MAX_PLACE_CHARS)?,
            clean_value(body.get("category"), 60)?,
        ))
    })();
    let (place, poi, category) = texts.map_err(|e| BadVisit(format!("place, poi and category: {e}")))?;
    let category = category.filter(|c| LETTERS.is_match(c));
    Ok(Visit { lat, lon, accuracy_m: acc, arrived, departed, place, poi, category })
}

// ---- the store -----------------------------------------------------------------------------------------------------

pub const SCHEMA: &str = "
create table if not exists fixes (at real primary key, lat real, lon real, acc real, speed real, place text);
create table if not exists places (
    id integer primary key, lat real not null, lon real not null, n integer not null default 1,
    name text, label text, poi text, category text, address text, first_seen real, last_seen real);
create table if not exists visits (
    id integer primary key, place_id integer not null, arrived real not null, departed real,
    lat real not null, lon real not null, acc real not null, source text not null);
create index if not exists visits_arrived on visits (arrived);
create table if not exists things (
    id integer primary key, thing text not null, key text not null unique, note text,
    lat real, lon real, acc real, place_id integer, at real not null);
create table if not exists meta (key text primary key, value text);
";

/// A row of `places`.
#[derive(Debug, Clone, PartialEq)]
pub struct PlaceRow {
    pub id: i64,
    pub lat: f64,
    pub lon: f64,
    pub n: i64,
    pub name: Option<String>,
    pub label: Option<String>,
    pub poi: Option<String>,
    pub category: Option<String>,
    pub address: Option<String>,
    pub first_seen: Option<f64>,
    pub last_seen: Option<f64>,
}

impl PlaceRow {
    fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(PlaceRow {
            id: r.get("id")?,
            lat: r.get("lat")?,
            lon: r.get("lon")?,
            n: r.get("n")?,
            name: r.get("name")?,
            label: r.get("label")?,
            poi: r.get("poi")?,
            category: r.get("category")?,
            address: r.get("address")?,
            first_seen: r.get("first_seen")?,
            last_seen: r.get("last_seen")?,
        })
    }
}

/// A visit's times and place id.
#[derive(Debug, Clone, Copy, PartialEq)]
struct VisitRow {
    id: i64,
    place_id: i64,
    arrived: f64,
    departed: Option<f64>,
    lat: f64,
    lon: f64,
}

impl VisitRow {
    fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(VisitRow {
            id: r.get("id")?,
            place_id: r.get("place_id")?,
            arrived: r.get("arrived")?,
            departed: r.get("departed")?,
            lat: r.get("lat")?,
            lon: r.get("lon")?,
        })
    }
}

/// `select v.arrived, v.departed, ..., p.*`: a visit with its place.
#[derive(Debug, Clone)]
struct Joined {
    arrived: f64,
    departed: Option<f64>,
    source: Option<String>,
    place: PlaceRow,
}

impl Joined {
    fn from_row(r: &Row) -> rusqlite::Result<Self> {
        let source = match r.as_ref().column_index("source") {
            Ok(_) => r.get("source")?,
            Err(_) => None,
        };
        let departed = match r.as_ref().column_index("departed") {
            Ok(_) => r.get("departed")?,
            Err(_) => None,
        };
        Ok(Joined { arrived: r.get("arrived")?, departed, source, place: PlaceRow::from_row(r)? })
    }
}

/// A row of `things`.
#[derive(Debug, Clone)]
struct ThingRow {
    thing: String,
    key: String,
    note: Option<String>,
    lat: Option<f64>,
    lon: Option<f64>,
    acc: Option<f64>,
    place_id: Option<i64>,
    at: f64,
}

impl ThingRow {
    fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(ThingRow {
            thing: r.get("thing")?,
            key: r.get("key")?,
            note: r.get("note")?,
            lat: r.get("lat")?,
            lon: r.get("lon")?,
            acc: r.get("acc")?,
            place_id: r.get("place_id")?,
            at: r.get("at")?,
        })
    }
}

/// Home and work as `infer_labels` found them (place ids).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Labels {
    pub home: Option<i64>,
    pub work: Option<i64>,
}

pub type Clock = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;

fn all<T>(
    con: &Connection,
    sql: &str,
    params: impl rusqlite::Params,
    f: impl FnMut(&Row) -> rusqlite::Result<T>,
) -> rusqlite::Result<Vec<T>> {
    let mut stmt = con.prepare(sql)?;
    let rows = stmt.query_map(params, f)?;
    rows.collect()
}

fn obj(pairs: impl IntoIterator<Item = (&'static str, Value)>) -> Map<String, Value> {
    pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

fn get_f64(stay: &Map<String, Value>, key: &str) -> Res<f64> {
    stay.get(key).and_then(Value::as_f64).ok_or_else(|| PlacesError::Other(format!("'{key}'")))
}

/// The history in one SQLite file. Every method opens, works and closes under the module lock: calls come from
/// worker threads, a few a minute at most. `now` is lent for tests.
pub struct Places {
    pub env: Option<Env>,
    pub path: PathBuf,
    pub zone: Zone,
    now: Clock,
}

impl Places {
    /// `Places(path, environ=..., now=...)`: each None is Python's default (the state path, the process
    /// environment, the wall clock).
    pub fn new(path: Option<PathBuf>, environ: Option<Env>, now: Option<Clock>) -> Self {
        let path = path.unwrap_or_else(|| state_path(environ.as_ref()));
        let zone = zone(environ.as_ref());
        Places { env: environ, path, zone, now: now.unwrap_or_else(|| Arc::new(Utc::now)) }
    }

    fn env(&self) -> Option<&Env> {
        self.env.as_ref()
    }

    // ---- plumbing ------------------------------------------------------------------------------------------------

    fn db<T>(&self, f: impl FnOnce(&Transaction) -> Res<T>) -> Res<T> {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

        let _held = LOCK.lock();
        let new = !self.path.exists();
        if new {
            if let Some(parent) = self.path.parent().filter(|p| !p.as_os_str().is_empty()) {
                if !parent.exists() {
                    if let Some(grand) = parent.parent().filter(|p| !p.as_os_str().is_empty()) {
                        std::fs::create_dir_all(grand)?;
                    }
                    match std::fs::DirBuilder::new().mode(0o700).create(parent) {
                        Err(e) if e.kind() != std::io::ErrorKind::AlreadyExists => return Err(e.into()),
                        _ => {}
                    }
                }
            }
        }
        let mut con = Connection::open(&self.path)?;
        con.busy_timeout(StdDuration::from_secs(5))?;
        if new {
            std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600))?;
        }
        con.execute_batch(SCHEMA)?;
        let tx = con.transaction()?;
        let out = f(&tx)?; // an error drops the transaction: rolled back
        tx.commit()?;
        Ok(out)
    }

    fn ts(&self) -> f64 {
        py::timestamp(&(self.now)())
    }

    fn local(&self, ts: f64) -> NaiveDateTime {
        self.zone.local(ts)
    }

    fn day_bounds(&self, day: NaiveDate) -> (f64, f64) {
        let start = self.zone.timestamp(day.and_time(NaiveTime::MIN));
        let end = self.zone.timestamp((day + Duration::days(1)).and_time(NaiveTime::MIN));
        (start, end)
    }

    fn meta(con: &Connection, key: &str) -> Res<Option<Value>> {
        let raw: Option<String> =
            con.query_row("select value from meta where key = ?", [key], |r| r.get(0)).optional()?;
        match raw {
            None => Ok(None),
            Some(text) => serde_json::from_str(&text).map(Some).map_err(|e| PlacesError::Value(e.to_string())),
        }
    }

    fn set_meta(con: &Connection, key: &str, value: Option<&Value>) -> Res<()> {
        match value {
            None => con.execute("delete from meta where key = ?", [key])?,
            Some(v) => con.execute("insert or replace into meta (key, value) values (?, ?)", params![key, py::dumps(v)])?,
        };
        Ok(())
    }

    // ---- places --------------------------------------------------------------------------------------------------

    fn match_place(con: &Connection, lat: f64, lon: f64, acc: f64, poi: Option<&str>) -> Res<Option<PlaceRow>> {
        let reach = PLACE_RADIUS_M.max(acc.min(MAX_MATCH_RADIUS_M));
        let mut best: Option<(f64, PlaceRow)> = None;
        for row in all(con, "select * from places", [], PlaceRow::from_row)? {
            let d = distance_m(lat, lon, row.lat, row.lon);
            if let (Some(poi), Some(rpoi)) = (poi.filter(|p| !p.is_empty()), row.poi.as_deref().filter(|p| !p.is_empty())) {
                if rpoi.to_lowercase() == poi.to_lowercase() && d <= POI_MATCH_M {
                    return Ok(Some(row));
                }
            }
            if d <= reach && best.as_ref().is_none_or(|(bd, _)| d < *bd) {
                best = Some((d, row));
            }
        }
        Ok(best.map(|(_, r)| r))
    }

    #[allow(clippy::too_many_arguments)]
    fn place_for(
        con: &Connection,
        lat: f64,
        lon: f64,
        acc: f64,
        at: f64,
        place: Option<&str>,
        poi: Option<&str>,
        category: Option<&str>,
    ) -> Res<i64> {
        let Some(row) = Self::match_place(con, lat, lon, acc, poi)? else {
            con.execute(
                "insert into places (lat, lon, n, poi, category, address, first_seen, last_seen) \
                 values (?, ?, 1, ?, ?, ?, ?, ?)",
                params![lat, lon, poi, category, place, at, at],
            )?;
            return Ok(con.last_insert_rowid());
        };
        let n = row.n.min(20) as f64; // the center follows the visits, but a long history does not freeze it
        con.execute(
            "update places set lat = ?, lon = ?, n = n + 1, poi = coalesce(poi, ?), \
             category = coalesce(category, ?), address = coalesce(?, address), \
             first_seen = min(coalesce(first_seen, ?), ?), last_seen = max(coalesce(last_seen, ?), ?) \
             where id = ?",
            params![
                (row.lat * n + lat) / (n + 1.0),
                (row.lon * n + lon) / (n + 1.0),
                poi,
                category,
                place,
                at,
                at,
                at,
                at,
                row.id
            ],
        )?;
        Ok(row.id)
    }

    /// A place in words: the owner's name, else Home / Work, else its point of interest, else its address.
    pub fn title(row: &PlaceRow) -> String {
        if let Some(name) = row.name.as_deref().filter(|s| !s.is_empty()) {
            return name.to_string();
        }
        if let Some(label) = row.label.as_deref() {
            if let Some((_, t)) = LABEL_TITLES.iter().find(|(k, _)| *k == label) {
                return t.to_string();
            }
        }
        if let Some(poi) = row.poi.as_deref().filter(|s| !s.is_empty()) {
            return poi.to_string();
        }
        if let Some(address) = row.address.as_deref().filter(|s| !s.is_empty()) {
            return address.to_string();
        }
        "an unnamed place".into()
    }

    fn describe(row: &PlaceRow) -> Map<String, Value> {
        let place = Self::title(row);
        let mut out = obj([("id", json!(row.id)), ("place", json!(place))]);
        if let Some(kind) = category_words(row.category.as_deref()) {
            if !kind.is_empty() {
                out.insert("kind".into(), json!(kind));
            }
        }
        if let Some(label) = row.label.as_deref().filter(|s| !s.is_empty()) {
            out.insert("is".into(), json!(label));
        }
        if let Some(address) = row.address.as_deref().filter(|s| !s.is_empty()) {
            if address != place {
                out.insert("address".into(), json!(address));
            }
        }
        out
    }

    // ---- evidence in ---------------------------------------------------------------------------------------------

    /// Keep one fix and move the stay read from fixes along. Returns the visit it closed, if any.
    pub fn record_fix(&self, fix: &Fix) -> Res<Option<Map<String, Value>>> {
        let at = py::timestamp(&fix.at);
        self.db(|con| {
            con.execute(
                "insert or ignore into fixes (at, lat, lon, acc, speed, place) values (?, ?, ?, ?, ?, ?)",
                params![at, fix.lat, fix.lon, fix.accuracy_m, fix.speed_mps, fix.place],
            )?;
            if fix.accuracy_m > VAGUE_FIX_M {
                return Ok(None);
            }
            let stay = match Self::meta(con, "stay")? {
                None | Some(Value::Null) => None,
                Some(Value::Object(m)) => Some(m),
                Some(_) => return Err(PlacesError::Other("stay".into())),
            };
            let Some(mut stay) = stay else {
                Self::set_meta(con, "stay", Some(&Value::Object(Self::new_stay(fix, at))))?;
                return Ok(None);
            };
            if at < get_f64(&stay, "last")? {
                return Ok(None); // a late fix: kept, but the stay has moved on
            }
            let (slat, slon, sacc) = (get_f64(&stay, "lat")?, get_f64(&stay, "lon")?, get_f64(&stay, "acc")?);
            let d = distance_m(fix.lat, fix.lon, slat, slon);
            if d <= STAY_RADIUS_M.max(fix.accuracy_m.max(sacc).min(MAX_MATCH_RADIUS_M)) {
                let n = stay.get("n").and_then(Value::as_i64).ok_or_else(|| PlacesError::Other("'n'".into()))?;
                let nf = n as f64;
                let place = match &fix.place {
                    Some(p) if !p.is_empty() => json!(p),
                    _ => stay.get("place").cloned().unwrap_or(Value::Null),
                };
                stay.insert("lat".into(), json!((slat * nf + fix.lat) / (nf + 1.0)));
                stay.insert("lon".into(), json!((slon * nf + fix.lon) / (nf + 1.0)));
                stay.insert("n".into(), json!((n + 1).min(20)));
                stay.insert("last".into(), json!(at));
                stay.insert("acc".into(), json!(sacc.min(fix.accuracy_m)));
                stay.insert("place".into(), place);
                Self::set_meta(con, "stay", Some(&Value::Object(stay)))?;
                return Ok(None);
            }
            // moved: the stay ended before this fix, by the time it takes to get here
            let (arrived, last) = (get_f64(&stay, "arrived")?, get_f64(&stay, "last")?);
            let end = at.min(last.max(at - d / TRAVEL_MPS));
            let mut closed = None;
            if end - arrived >= MIN_STAY_S {
                let place = stay.get("place").and_then(Value::as_str);
                closed = self.insert_visit(con, slat, slon, sacc, arrived, Some(end), "fixes", place, None, None)?;
            }
            Self::set_meta(con, "stay", Some(&Value::Object(Self::new_stay(fix, at))))?;
            Ok(closed)
        })
    }

    fn new_stay(fix: &Fix, at: f64) -> Map<String, Value> {
        obj([
            ("lat", json!(fix.lat)),
            ("lon", json!(fix.lon)),
            ("acc", json!(fix.accuracy_m)),
            ("arrived", json!(at)),
            ("last", json!(at)),
            ("n", json!(1)),
            ("place", json!(fix.place)),
        ])
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_visit(
        &self,
        con: &Connection,
        lat: f64,
        lon: f64,
        acc: f64,
        arrived: f64,
        departed: Option<f64>,
        source: &str,
        place: Option<&str>,
        poi: Option<&str>,
        category: Option<&str>,
    ) -> Res<Option<Map<String, Value>>> {
        if source == "fixes" {
            // iOS already reported this stretch here: its visit is the better evidence
            let until = departed.filter(|d| *d != 0.0).unwrap_or(arrived);
            let ios = all(
                con,
                "select * from visits where source = 'visit' and arrived < ? and coalesce(departed, ?) > ?",
                params![until, until, arrived],
                VisitRow::from_row,
            )?;
            if ios.iter().any(|v| distance_m(lat, lon, v.lat, v.lon) <= POI_MATCH_M) {
                return Ok(None);
            }
        }
        let pid = Self::place_for(con, lat, lon, acc, arrived, place, poi, category)?;
        con.execute(
            "insert into visits (place_id, arrived, departed, lat, lon, acc, source) values (?, ?, ?, ?, ?, ?, ?)",
            params![pid, arrived, departed, lat, lon, acc, source],
        )?;
        let id = con.last_insert_rowid();
        if source == "visit" {
            Self::drop_fix_stays(con, lat, lon, arrived, departed)?;
        }
        Ok(Some(obj([("id", json!(id)), ("place_id", json!(pid))])))
    }

    fn drop_fix_stays(con: &Connection, lat: f64, lon: f64, arrived: f64, departed: Option<f64>) -> Res<()> {
        let end = departed.unwrap_or(f64::INFINITY);
        let rows = all(
            con,
            "select id, place_id, lat, lon, arrived, departed from visits where source = 'fixes' \
             and arrived < ? and coalesce(departed, arrived) > ?",
            params![end, arrived],
            VisitRow::from_row,
        )?;
        for v in rows {
            if distance_m(lat, lon, v.lat, v.lon) <= POI_MATCH_M {
                con.execute("delete from visits where id = ?", [v.id])?;
            }
        }
        Ok(())
    }

    /// Keep one iOS visit: an arrival opens a visit, a departure closes the one it ends (or adds it whole).
    /// Returns what was done, for the log.
    pub fn record_visit(&self, visit: &Visit) -> Res<&'static str> {
        let arrived = visit.arrived.as_ref().map(py::timestamp);
        let departed = visit.departed.as_ref().map(py::timestamp);
        let (place, poi, category) = (visit.place.as_deref(), visit.poi.as_deref(), visit.category.as_deref());
        self.db(|con| {
            if let Some(arrived) = arrived {
                let same: Vec<VisitRow> = all(
                    con,
                    "select * from visits where source = 'visit' and abs(arrived - ?) <= ?",
                    params![arrived, VISIT_SAME_S],
                    VisitRow::from_row,
                )?
                .into_iter()
                .filter(|v| distance_m(visit.lat, visit.lon, v.lat, v.lon) <= POI_MATCH_M)
                .collect();
                if let Some(first) = same.first() {
                    if let Some(departed) = departed {
                        con.execute("update visits set departed = ? where id = ?", params![departed, first.id])?;
                        con.execute(
                            "update places set last_seen = max(coalesce(last_seen, ?), ?), \
                             poi = coalesce(poi, ?), category = coalesce(category, ?) where id = ?",
                            params![departed, departed, poi, category, first.place_id],
                        )?;
                        Self::drop_fix_stays(con, visit.lat, visit.lon, arrived, Some(departed))?;
                        return Ok("departure");
                    }
                    return Ok("repeat");
                }
                // an arrival while another visit is still open: that one ended by now
                con.execute(
                    "update visits set departed = ? where source = 'visit' and departed is null and arrived < ?",
                    params![arrived, arrived],
                )?;
                self.insert_visit(con, visit.lat, visit.lon, visit.accuracy_m, arrived, departed, "visit", place, poi, category)?;
                return Ok(if departed.is_some() { "visit" } else { "arrival" });
            }
            // a departure whose arrival iOS did not see: close the open visit here, if there is one
            let Some(departed) = departed else { return Ok("ignored") };
            let open = con
                .query_row(
                    "select * from visits where departed is null and arrived <= ? order by arrived desc limit 1",
                    [departed],
                    VisitRow::from_row,
                )
                .optional()?;
            if let Some(open) = open {
                if distance_m(visit.lat, visit.lon, open.lat, open.lon) <= POI_MATCH_M {
                    con.execute("update visits set departed = ? where id = ?", params![departed, open.id])?;
                    return Ok("departure");
                }
            }
            Ok("ignored")
        })
    }

    // ---- labels --------------------------------------------------------------------------------------------------

    fn visits_between(&self, con: &Connection, start: f64, end: f64) -> Res<Vec<VisitRow>> {
        Ok(all(
            con,
            "select * from visits where arrived < ? and coalesce(departed, ?) > ? order by arrived",
            params![end, self.ts(), start],
            VisitRow::from_row,
        )?)
    }

    /// Per place: hours inside `window` on each day, and the days with at least LABEL_MIN_HOURS.
    fn overlap_hours(
        &self,
        visits: &[VisitRow],
        window: impl Fn(NaiveDate) -> Option<(NaiveTime, NaiveTime)>,
        start_day: NaiveDate,
        end_day: NaiveDate,
        now: f64,
    ) -> IndexMap<i64, (f64, HashSet<NaiveDate>)> {
        let mut out: IndexMap<i64, (f64, HashSet<NaiveDate>)> = IndexMap::new();
        let mut per_day: IndexMap<(i64, NaiveDate), f64> = IndexMap::new();
        let mut day = start_day;
        while day <= end_day {
            if let Some((t0, t1)) = window(day) {
                let w0 = self.zone.timestamp(day.and_time(t0));
                let w1 = self.zone.timestamp(day.and_time(t1));
                for v in visits {
                    let (a, b) = (v.arrived, v.departed.unwrap_or(now));
                    let h = (b.min(w1) - a.max(w0)).max(0.0) / 3600.0;
                    if h > 0.0 {
                        *per_day.entry((v.place_id, day)).or_insert(0.0) += h;
                    }
                }
            }
            day += Duration::days(1);
        }
        for ((pid, d), h) in per_day {
            let entry = out.entry(pid).or_insert((0.0, HashSet::new()));
            if h >= LABEL_MIN_HOURS {
                entry.1.insert(d);
            }
            entry.0 += h;
        }
        out
    }

    /// Home: the most night hours (00:00-06:00) over LABEL_DAYS, on at least LABEL_MIN_DAYS nights. Work: the most
    /// weekday hours (09:00-17:00) at another place, on at least LABEL_MIN_DAYS days and WORK_MIN_HOURS in all.
    pub fn infer_labels(&self) -> Res<Labels> {
        let now = self.ts();
        let today = self.local(now).date();
        let first = today - Duration::days(LABEL_DAYS);
        let (start, _) = self.day_bounds(first);
        let hm = |h: u32| NaiveTime::from_hms_opt(h, 0, 0).unwrap_or(NaiveTime::MIN);
        self.db(|con| {
            let visits = self.visits_between(con, start, now)?;
            let nights = self.overlap_hours(&visits, |_| Some((hm(0), hm(6))), first, today, now);
            let days = self.overlap_hours(
                &visits,
                |d| (d.weekday().num_days_from_monday() < 5).then(|| (hm(9), hm(17))),
                first,
                today,
                now,
            );
            // Python's max: the first of equals wins
            let mut home: Option<(i64, f64)> = None;
            for (&p, (h, ds)) in &nights {
                if ds.len() >= LABEL_MIN_DAYS && home.is_none_or(|(_, bh)| *h > bh) {
                    home = Some((p, *h));
                }
            }
            let home = home.map(|(p, _)| p);
            let mut work: Option<(i64, f64)> = None;
            for (&p, (h, ds)) in &days {
                if Some(p) != home
                    && ds.len() >= LABEL_MIN_DAYS
                    && *h >= WORK_MIN_HOURS
                    && work.is_none_or(|(_, bh)| *h > bh)
                {
                    work = Some((p, *h));
                }
            }
            let work = work.map(|(p, _)| p);
            con.execute("update places set label = null where label in ('home', 'work')", [])?;
            if let Some(home) = home {
                con.execute("update places set label = 'home' where id = ?", [home])?;
            }
            if let Some(work) = work {
                con.execute("update places set label = 'work' where id = ?", [work])?;
            }
            Ok(Labels { home, work })
        })
    }

    // ---- now -----------------------------------------------------------------------------------------------------

    /// Where the owner is now in words: the place and since when; None without a fix in the last hour.
    /// `{"place", "since"?, "kind"?, "is"?}`; an unknown spot is the phone's own name for it.
    pub fn here(&self) -> Res<Option<Map<String, Value>>> {
        let Some(fix) = phone_location::fresh(self.env()) else { return Ok(None) };
        let (row, stay) = {
            let found = self.db(|con| {
                let open = con
                    .query_row(
                        "select v.arrived, p.* from visits v join places p on p.id = v.place_id \
                         where v.departed is null and v.source = 'visit' order by v.arrived desc limit 1",
                        [],
                        Joined::from_row,
                    )
                    .optional()?;
                if let Some(open) = open {
                    if distance_m(fix.lat, fix.lon, open.place.lat, open.place.lon) <= POI_MATCH_M {
                        return Ok(Err(open));
                    }
                }
                let row = Self::match_place(con, fix.lat, fix.lon, fix.accuracy_m, None)?;
                let stay = Self::meta(con, "stay")?;
                Ok(Ok((row, stay)))
            })?;
            match found {
                Err(open) => {
                    let mut out = Self::describe(&open.place);
                    out.insert("since".into(), json!(self.local(open.arrived).format("%H:%M").to_string()));
                    return Ok(Some(out));
                }
                Ok(pair) => pair,
            }
        };
        let mut out = match &row {
            Some(row) => Self::describe(row),
            None => obj([("place", json!(fix.place.clone().filter(|p| !p.is_empty()).unwrap_or_else(|| "somewhere new".into())))]),
        };
        if let Some(Value::Object(stay)) = stay {
            let (slat, slon) = (get_f64(&stay, "lat")?, get_f64(&stay, "lon")?);
            if distance_m(fix.lat, fix.lon, slat, slon) <= STAY_RADIUS_M
                && get_f64(&stay, "last")? - get_f64(&stay, "arrived")? >= MIN_STAY_S
            {
                out.insert("since".into(), json!(self.local(get_f64(&stay, "arrived")?).format("%H:%M").to_string()));
            }
        }
        Ok(Some(out))
    }

    // ---- the timeline --------------------------------------------------------------------------------------------

    pub fn timeline(&self, first: NaiveDate, last: NaiveDate) -> Res<Vec<Map<String, Value>>> {
        let (start, _) = self.day_bounds(first);
        let (_, end) = self.day_bounds(last);
        let now = self.ts();
        let rows = self.db(|con| {
            Ok(all(
                con,
                "select v.arrived, v.departed, v.source, p.* from visits v join places p \
                 on p.id = v.place_id where v.arrived < ? and coalesce(v.departed, ?) > ? \
                 order by v.arrived",
                params![end, now, start],
                Joined::from_row,
            )?)
        })?;
        let skip = rows.len().saturating_sub(MAX_TIMELINE);
        let mut out = Vec::new();
        for r in &rows[skip..] {
            let a = self.local(r.arrived);
            let mut item = Self::describe(&r.place);
            item.insert("arrived".into(), json!(a.format("%a %Y-%m-%d %H:%M").to_string()));
            match r.departed {
                Some(d) => {
                    item.insert("left".into(), json!(self.local(d).format("%a %Y-%m-%d %H:%M").to_string()));
                    item.insert("stayed".into(), json!(duration(d - r.arrived)));
                }
                None => {
                    item.insert("left".into(), json!("still there"));
                    item.insert("stayed".into(), json!(duration(now - r.arrived)));
                }
            }
            if r.source.as_deref() == Some("fixes") {
                item.insert("approximate".into(), json!(true));
            }
            out.push(item);
        }
        Ok(out)
    }

    // ---- habits --------------------------------------------------------------------------------------------------

    fn pattern_rows(con: &Connection, now: f64) -> Res<Vec<VisitRow>> {
        let since = now - PATTERN_WEEKS as f64 * WEEK_S;
        Ok(all(
            con,
            "select v.id, v.place_id, v.arrived, v.departed, v.lat, v.lon from visits v where v.arrived >= ? \
             order by v.arrived",
            [since],
            VisitRow::from_row,
        )?)
    }

    fn weekday(&self, ts: f64) -> u32 {
        self.local(ts).weekday().num_days_from_monday()
    }

    fn habit(&self, place: &PlaceRow, visits: &[VisitRow], now: f64) -> Option<Map<String, Value>> {
        if visits.len() < 2 {
            return None;
        }
        let first_seen = place.first_seen.filter(|f| *f != 0.0).unwrap_or(now);
        let since = first_seen.max(now - PATTERN_WEEKS as f64 * WEEK_S);
        let weeks = py::ceil((now - since) / WEEK_S).clamp(1, PATTERN_WEEKS);
        let mut dates_by_wd: HashMap<u32, HashSet<NaiveDate>> = HashMap::new();
        for v in visits {
            let d = self.local(v.arrived).date();
            dates_by_wd.entry(d.weekday().num_days_from_monday()).or_default().insert(d);
        }
        let need = py::ceil(weeks as f64 * 0.5).max(2) as usize;
        let mut usual: Vec<u32> = dates_by_wd.iter().filter(|(_, ds)| ds.len() >= need).map(|(wd, _)| *wd).collect();
        usual.sort_unstable();
        let pick: Vec<&VisitRow> = if usual.is_empty() {
            visits.iter().collect()
        } else {
            visits.iter().filter(|v| usual.contains(&self.weekday(v.arrived))).collect()
        };
        let arrivals: Vec<f64> = pick
            .iter()
            .map(|v| {
                let l = self.local(v.arrived);
                f64::from(l.hour() * 60 + l.minute())
            })
            .collect();
        let stays: Vec<f64> = pick.iter().filter_map(|v| v.departed.map(|d| d - v.arrived)).collect();
        let mut out = obj([
            ("visits", json!(visits.len())),
            ("weeks", json!(weeks)),
            ("per_week", json!(py::round_to(visits.len() as f64 / weeks as f64, 1))),
        ]);
        if !usual.is_empty() {
            out.insert("usual_days".into(), json!(days_words(usual.iter().copied())));
            out.insert("usual_arrival".into(), json!(clock(py::median(&arrivals))));
        }
        if !stays.is_empty() {
            out.insert("usual_stay".into(), json!(duration(py::median(&stays))));
        }
        Some(out)
    }

    /// The home routine on weekdays: when the owner leaves home in the morning and is back in the evening.
    fn routine(&self, con: &Connection, home: Option<i64>, now: f64) -> Res<Vec<String>> {
        let Some(home) = home else { return Ok(Vec::new()) };
        let since = now - PATTERN_WEEKS as f64 * WEEK_S;
        let rows: Vec<(f64, Option<f64>)> = all(
            con,
            "select arrived, departed from visits where place_id = ? and arrived >= ? - 86400",
            params![home, since],
            |r| Ok((r.get("arrived")?, r.get("departed")?)),
        )?;
        let (mut leaves, mut backs) = (Vec::new(), Vec::new());
        for (arrived, departed) in rows {
            if let Some(departed) = departed {
                let d = self.local(departed);
                if d.weekday().num_days_from_monday() < 5 && (5..12).contains(&d.hour()) {
                    leaves.push(f64::from(d.hour() * 60 + d.minute()));
                }
            }
            let a = self.local(arrived);
            if a.weekday().num_days_from_monday() < 5 && (15..24).contains(&a.hour()) && arrived >= since {
                backs.push(f64::from(a.hour() * 60 + a.minute()));
            }
        }
        let mut out = Vec::new();
        if leaves.len() >= 3 {
            out.push(format!("On weekdays you usually leave home around {}.", clock(py::median(&leaves))));
        }
        if backs.len() >= 3 {
            out.push(format!("On weekdays you are usually home by {}.", clock(py::median(&backs))));
        }
        Ok(out)
    }

    /// Each place's habit over the last PATTERN_WEEKS weeks, most visited first, and the home routine.
    pub fn patterns(&self) -> Res<Map<String, Value>> {
        self.maybe_infer()?;
        let now = self.ts();
        let (rows, places, routine) = self.db(|con| {
            let rows = Self::pattern_rows(con, now)?;
            let places = all(con, "select * from places", [], PlaceRow::from_row)?;
            let home = places.iter().find(|r| r.label.as_deref() == Some("home")).map(|r| r.id);
            let routine = self.routine(con, home, now)?;
            Ok((rows, places, routine))
        })?;
        let places: HashMap<i64, PlaceRow> = places.into_iter().map(|p| (p.id, p)).collect();
        let mut by_place: IndexMap<i64, Vec<VisitRow>> = IndexMap::new();
        for r in rows {
            by_place.entry(r.place_id).or_default().push(r);
        }
        let mut groups: Vec<(i64, Vec<VisitRow>)> = by_place.into_iter().collect();
        groups.sort_by_key(|(_, vs)| std::cmp::Reverse(vs.len()));
        let mut habits = Vec::new();
        for (pid, vs) in groups {
            let Some(place) = places.get(&pid) else { continue };
            let Some(habit) = self.habit(place, &vs, now) else { continue };
            let mut item = Self::describe(place);
            item.extend(habit);
            habits.push(Value::Object(item));
        }
        habits.truncate(20);
        Ok(obj([("weeks", json!(PATTERN_WEEKS)), ("places", Value::Array(habits)), ("routine", json!(routine))]))
    }

    fn maybe_infer(&self) -> Res<()> {
        let last = self.db(|con| Self::meta(con, "labelled"))?;
        let last = match last {
            None | Some(Value::Null) | Some(Value::Bool(false)) => 0.0,
            Some(v) => v.as_f64().ok_or_else(|| PlacesError::Value(format!("could not convert to float: {v}")))?,
        };
        if self.ts() - last >= 3600.0 {
            self.infer_labels()?;
            self.db(|con| Self::set_meta(con, "labelled", Some(&json!(self.ts()))))?;
        }
        Ok(())
    }

    // ---- digests -------------------------------------------------------------------------------------------------

    /// One day in words: the places in order, time at home / work / out, how far, what was new, what was missed.
    pub fn digest(&self, day: NaiveDate) -> Res<Map<String, Value>> {
        self.maybe_infer()?;
        let (start, end) = self.day_bounds(day);
        let now = self.ts();
        let today = self.local(now).date();
        let (rows, fixes, new, habit_rows, places) = self.db(|con| {
            let rows = all(
                con,
                "select v.arrived, v.departed, v.place_id, v.source, p.* from visits v join places p \
                 on p.id = v.place_id where v.arrived < ? and coalesce(v.departed, ?) > ? \
                 order by v.arrived",
                params![end, now, start],
                Joined::from_row,
            )?;
            let fixes: Vec<(f64, f64, f64)> = all(
                con,
                "select lat, lon, acc from fixes where at >= ? and at < ? order by at",
                params![start, end],
                |r| Ok((r.get("lat")?, r.get("lon")?, r.get("acc")?)),
            )?;
            let new = all(
                con,
                "select * from places where first_seen >= ? and first_seen < ?",
                params![start, end],
                PlaceRow::from_row,
            )?;
            let habit_rows = Self::pattern_rows(con, now.min(start))?;
            let places = all(con, "select * from places", [], PlaceRow::from_row)?;
            Ok((rows, fixes, new, habit_rows, places))
        })?;
        let places: HashMap<i64, PlaceRow> = places.into_iter().map(|p| (p.id, p)).collect();
        let mut stops = Vec::new();
        let mut at: IndexMap<&str, f64> = IndexMap::from([("home", 0.0), ("work", 0.0), ("out", 0.0)]);
        let mut seen: HashSet<i64> = HashSet::new();
        for r in &rows {
            let a = r.arrived.max(start);
            let b = r.departed.unwrap_or(now).min(end);
            if b <= a {
                continue;
            }
            seen.insert(r.place.id);
            let key = r.place.label.as_deref().filter(|l| matches!(*l, "home" | "work" | "out")).unwrap_or("out");
            *at.entry(key).or_insert(0.0) += b - a;
            let to = if r.departed.is_none() && day == today {
                "now".to_string()
            } else {
                self.local(b).format("%H:%M").to_string()
            };
            let mut stop = obj([
                ("place", json!(Self::title(&r.place))),
                ("from", json!(self.local(a).format("%H:%M").to_string())),
                ("to", json!(to)),
                ("stayed", json!(duration(b - a))),
            ]);
            if let Some(kind) = category_words(r.place.category.as_deref()).filter(|k| !k.is_empty()) {
                stop.insert("kind".into(), json!(kind));
            }
            stops.push(Value::Object(stop));
        }
        let mut moved = 0.0;
        let mut prev: Option<(f64, f64, f64)> = None;
        for f in fixes {
            if f.2 > 200.0 {
                continue;
            }
            if let Some(p) = prev {
                let hop = distance_m(p.0, p.1, f.0, f.1);
                if hop > p.2.max(f.2).max(50.0) {
                    moved += hop;
                }
            }
            prev = Some(f);
        }
        let mut out = obj([("day", json!(day.format("%A %Y-%m-%d").to_string())), ("stops", Value::Array(stops))]);
        for (k, v) in &at {
            if *v >= 60.0 {
                out.insert(format!("at_{k}"), json!(duration(*v)));
            }
        }
        if moved >= 200.0 {
            out.insert("moved".into(), json!(distance_words(moved)));
        }
        if !new.is_empty() {
            out.insert("new_places".into(), json!(new.iter().map(Self::title).collect::<Vec<_>>()));
        }
        // usual places this weekday that were not visited (only once the day is over)
        if day < today {
            let mut by_place: IndexMap<i64, Vec<VisitRow>> = IndexMap::new();
            for r in habit_rows {
                by_place.entry(r.place_id).or_default().push(r);
            }
            let mut missed = Vec::new();
            let wd = day.weekday().num_days_from_monday();
            for (pid, vs) in &by_place {
                let Some(place) = places.get(pid) else { continue };
                if seen.contains(pid) || place.label.as_deref() == Some("home") {
                    continue;
                }
                if let Some(habit) = self.habit(place, vs, start) {
                    if let Some(days) = habit.get("usual_days").and_then(Value::as_str) {
                        if self.days_of(vs).contains(&wd) {
                            missed.push(format!("{} (usually {})", Self::title(place), days));
                        }
                    }
                }
            }
            if !missed.is_empty() {
                out.insert("missed".into(), json!(missed));
            }
        }
        let text = Self::digest_text(&out);
        out.insert("text".into(), json!(text));
        Ok(out)
    }

    fn days_of(&self, visits: &[VisitRow]) -> HashSet<u32> {
        let (Some(first), Some(last)) = (visits.first(), visits.last()) else { return HashSet::new() };
        let weeks = py::ceil((last.arrived - first.arrived) / WEEK_S).clamp(1, PATTERN_WEEKS);
        let mut dates: HashMap<u32, HashSet<NaiveDate>> = HashMap::new();
        for v in visits {
            let d = self.local(v.arrived).date();
            dates.entry(d.weekday().num_days_from_monday()).or_default().insert(d);
        }
        let need = py::ceil(weeks as f64 * 0.5).max(2) as usize;
        dates.into_iter().filter(|(_, ds)| ds.len() >= need).map(|(wd, _)| wd).collect()
    }

    fn digest_text(d: &Map<String, Value>) -> String {
        let day = d.get("day").and_then(Value::as_str).unwrap_or_default();
        let stops = d.get("stops").and_then(Value::as_array).cloned().unwrap_or_default();
        if stops.is_empty() {
            return format!("{day}: no places recorded.");
        }
        let s = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
        let mut lines = vec![format!(
            "{day}: {}.",
            stops
                .iter()
                .map(|x| format!("{} {}-{} ({})", s(x, "place"), s(x, "from"), s(x, "to"), s(x, "stayed")))
                .collect::<Vec<_>>()
                .join("; ")
        )];
        let times: Vec<String> = d
            .iter()
            .filter(|(k, _)| k.starts_with("at_"))
            .map(|(k, v)| format!("{} {}", v.as_str().unwrap_or_default(), &k[3..]))
            .collect();
        if !times.is_empty() {
            lines.push(format!("Time: {}.", times.join(", ")));
        }
        if let Some(moved) = d.get("moved").and_then(Value::as_str).filter(|m| !m.is_empty()) {
            lines.push(format!("Moved {moved}."));
        }
        let list = |k: &str| -> Vec<String> {
            d.get(k)
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
                .unwrap_or_default()
        };
        let new = list("new_places");
        if !new.is_empty() {
            lines.push(format!("New: {}.", new.join(", ")));
        }
        let missed = list("missed");
        if !missed.is_empty() {
            lines.push(format!("Missed: {}.", missed.join(", ")));
        }
        lines.join(" ")
    }

    /// The week (Monday to Sunday) holding `day`: the top places by time, new places and the routine.
    pub fn week(&self, day: NaiveDate) -> Res<Map<String, Value>> {
        self.maybe_infer()?;
        let monday = day - Duration::days(i64::from(day.weekday().num_days_from_monday()));
        let sunday = monday + Duration::days(6);
        let (start, _) = self.day_bounds(monday);
        let (_, end) = self.day_bounds(sunday);
        let now = self.ts();
        let (rows, new) = self.db(|con| {
            let rows = all(
                con,
                "select v.arrived, v.departed, p.* from visits v join places p on p.id = v.place_id \
                 where v.arrived < ? and coalesce(v.departed, ?) > ?",
                params![end, now, start],
                Joined::from_row,
            )?;
            let new = all(
                con,
                "select * from places where first_seen >= ? and first_seen < ?",
                params![start, end],
                PlaceRow::from_row,
            )?;
            Ok((rows, new))
        })?;
        let mut total: IndexMap<i64, (f64, HashSet<NaiveDate>, PlaceRow)> = IndexMap::new();
        for r in rows {
            let a = r.arrived.max(start);
            let b = r.departed.unwrap_or(now).min(end);
            if b <= a {
                continue;
            }
            let mut days = HashSet::new();
            let mut d = self.local(a).date();
            let last = self.local(b - 1.0).date();
            while d <= last {
                // every day the stay touches, not only its first
                days.insert(d);
                d += Duration::days(1);
            }
            let id = r.place.id;
            let entry = total.entry(id).or_insert_with(|| (0.0, HashSet::new(), r.place.clone()));
            entry.0 += b - a;
            entry.1.extend(days);
            entry.2 = r.place;
        }
        let places_visited = total.len();
        let mut top: Vec<(f64, HashSet<NaiveDate>, PlaceRow)> = total.into_values().collect();
        top.sort_by(|x, y| y.0.partial_cmp(&x.0).unwrap_or(std::cmp::Ordering::Equal));
        top.truncate(8);
        let listed: Vec<Map<String, Value>> = top
            .iter()
            .map(|(t, ds, r)| obj([("place", json!(Self::title(r))), ("time", json!(duration(*t))), ("days", json!(ds.len()))]))
            .collect();
        let mut out = obj([
            ("week", json!(format!("{} to {}", monday.format("%Y-%m-%d"), sunday.format("%Y-%m-%d")))),
            ("places", json!(listed)),
            ("places_visited", json!(places_visited)),
        ]);
        let new_titles: Vec<String> = new.iter().map(Self::title).collect();
        if !new.is_empty() {
            out.insert("new_places".into(), json!(new_titles));
        }
        let routine = self.patterns()?.get("routine").cloned().unwrap_or_else(|| json!([]));
        out.insert("routine".into(), routine);
        let text = if top.is_empty() {
            format!("Week of {}: no places recorded.", monday.format("%Y-%m-%d"))
        } else {
            let parts: Vec<String> = listed
                .iter()
                .map(|p| {
                    let days = p["days"].as_u64().unwrap_or_default();
                    format!(
                        "{} {} on {} day{}",
                        p["place"].as_str().unwrap_or_default(),
                        p["time"].as_str().unwrap_or_default(),
                        days,
                        if days != 1 { "s" } else { "" }
                    )
                })
                .collect();
            format!(
                "Week of {}: {}.{}",
                monday.format("%Y-%m-%d"),
                parts.join("; "),
                if new.is_empty() { String::new() } else { format!(" New: {}.", new_titles.join(", ")) }
            )
        };
        out.insert("text".into(), json!(text));
        Ok(out)
    }

    // ---- naming --------------------------------------------------------------------------------------------------

    /// '' is where the owner is now; else a place by its name, label, point of interest or address.
    fn find_place(&self, con: &Connection, which: &str) -> Res<Option<PlaceRow>> {
        if which.is_empty() {
            let Some(fix) = phone_location::fresh(self.env()) else { return Ok(None) };
            if let Some(row) = Self::match_place(con, fix.lat, fix.lon, fix.accuracy_m, None)? {
                return Ok(Some(row));
            }
            let pid = Self::place_for(
                con,
                fix.lat,
                fix.lon,
                fix.accuracy_m,
                py::timestamp(&fix.at),
                fix.place.as_deref(),
                None,
                None,
            )?;
            return Ok(con.query_row("select * from places where id = ?", [pid], PlaceRow::from_row).optional()?);
        }
        let key = which.trim().to_lowercase();
        let rows = all(con, "select * from places", [], PlaceRow::from_row)?;
        for field in ["name", "label", "poi", "address"] {
            for r in &rows {
                let value = match field {
                    "name" => &r.name,
                    "label" => &r.label,
                    "poi" => &r.poi,
                    _ => &r.address,
                };
                if value.as_deref().is_some_and(|v| !v.is_empty() && v.to_lowercase() == key) {
                    return Ok(Some(r.clone()));
                }
            }
        }
        Ok(rows.into_iter().find(|r| Self::title(r).to_lowercase().contains(&key)))
    }

    /// The owner's own name for a place ('' clears it): where they are now, or one named by `which`.
    pub fn name_place(&self, name: &str, which: &str) -> Res<Map<String, Value>> {
        let clean_name = match clean(Some(name), MAX_NAME_CHARS) {
            Ok(n) => n,
            Err(e) => return Ok(obj([("ok", json!(false)), ("reason", json!(format!("name {e}")))])),
        };
        let normalized = which.split_whitespace().collect::<Vec<_>>().join(" ");
        let row = self.db(|con| {
            let Some(row) = self.find_place(con, &normalized)? else { return Ok(None) };
            con.execute("update places set name = ? where id = ?", params![clean_name, row.id])?;
            Ok(con.query_row("select * from places where id = ?", [row.id], PlaceRow::from_row).optional()?)
        })?;
        let Some(row) = row else {
            let reason = if which.is_empty() { NO_HERE } else { "no known place goes by that name" };
            return Ok(obj([("ok", json!(false)), ("reason", json!(reason))]));
        };
        let mut out = obj([("ok", json!(true))]);
        out.extend(Self::describe(&row));
        Ok(out)
    }

    // ---- things --------------------------------------------------------------------------------------------------

    fn key(thing: &str) -> String {
        let lowered = thing.to_lowercase();
        let spaced = NOT_WORD.replace_all(&lowered, " ");
        spaced
            .split_whitespace()
            .filter(|w| !matches!(*w, "my" | "the" | "a" | "an" | "our"))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Keep where a thing is: where the owner is now (the fix itself, for a car on a street), or a known place.
    pub fn remember_thing(&self, thing: &str, note: &str, which: &str) -> Res<Map<String, Value>> {
        let cleaned = clean(Some(thing), MAX_NAME_CHARS).and_then(|n| Ok((n, clean(Some(note), MAX_NOTE_CHARS)?)));
        let (name, note_text) = match cleaned {
            Ok(pair) => pair,
            Err(e) => return Ok(obj([("ok", json!(false)), ("reason", json!(format!("thing and note: {e}")))])),
        };
        let Some(name) = name.filter(|n| !Self::key(n).is_empty()) else {
            return Ok(obj([("ok", json!(false)), ("reason", json!("say which thing"))]));
        };
        let refused = |reason: String| Ok(Err(obj([("ok", json!(false)), ("reason", json!(reason))])));
        let kept = self.db(|con| {
            let count: i64 = con.query_row("select count(*) from things", [], |r| r.get(0))?;
            if count >= MAX_THINGS {
                return refused(format!("eve keeps at most {MAX_THINGS} things; forget one first"));
            }
            let (lat, lon, acc, pid, row);
            if !which.is_empty() {
                let normalized = which.split_whitespace().collect::<Vec<_>>().join(" ");
                let Some(found) = self.find_place(con, &normalized)? else {
                    return refused("no known place goes by that name".into());
                };
                (lat, lon, acc, pid) = (found.lat, found.lon, PLACE_RADIUS_M, Some(found.id));
                row = Some(found);
            } else {
                let Some(fix) = phone_location::fresh(self.env()) else { return refused(NO_HERE.into()) };
                row = Self::match_place(con, fix.lat, fix.lon, fix.accuracy_m, None)?;
                (lat, lon, acc) = (fix.lat, fix.lon, fix.accuracy_m);
                pid = row.as_ref().map(|r| r.id);
            }
            con.execute(
                "insert or replace into things (thing, key, note, lat, lon, acc, place_id, at) \
                 values (?, ?, ?, ?, ?, ?, ?, ?)",
                params![name, Self::key(&name), note_text, lat, lon, acc, pid, self.ts()],
            )?;
            Ok(Ok(row))
        })?;
        match kept {
            Err(refusal) => Ok(refusal),
            Ok(row) => {
                let mut out = obj([("ok", json!(true)), ("thing", json!(name))]);
                if let Some(row) = row {
                    out.insert("place".into(), json!(Self::title(&row)));
                }
                Ok(out)
            }
        }
    }

    fn thing_out(&self, con: &Connection, r: &ThingRow) -> Res<Map<String, Value>> {
        let mut out = obj([
            ("thing", json!(r.thing)),
            ("since", json!(self.local(r.at).format("%a %Y-%m-%d %H:%M").to_string())),
        ]);
        if let Some(note) = r.note.as_deref().filter(|n| !n.is_empty()) {
            out.insert("note".into(), json!(note));
        }
        let place = match r.place_id {
            Some(pid) => con.query_row("select * from places where id = ?", [pid], PlaceRow::from_row).optional()?,
            None => None,
        };
        if let Some(place) = place {
            out.insert("place".into(), json!(Self::title(&place)));
        }
        if let (Some(fix), Some(lat), Some(lon)) = (phone_location::fresh(self.env()), r.lat, r.lon) {
            let d = distance_m(fix.lat, fix.lon, lat, lon);
            let near = 60.0f64.max(r.acc.unwrap_or(0.0));
            out.insert("from_here".into(), json!(if d <= near { "right here".to_string() } else { distance_words(d) }));
        }
        if let (Some(lat), Some(lon)) = (r.lat, r.lon) {
            out.insert("map".into(), json!(format!("https://maps.apple.com/?ll={lat:.5},{lon:.5}")));
        }
        Ok(out)
    }

    pub fn find_thing(&self, thing: &str) -> Res<Map<String, Value>> {
        let key = Self::key(thing);
        self.db(|con| {
            let rows = all(con, "select * from things order by at desc", [], ThingRow::from_row)?;
            let hit = rows.iter().find(|r| r.key == key).or_else(|| {
                rows.iter().find(|r| !key.is_empty() && (r.key.contains(&key) || key.contains(&r.key)))
            });
            let Some(hit) = hit else {
                let things: Vec<&str> = rows.iter().take(20).map(|r| r.thing.as_str()).collect();
                return Ok(obj([
                    ("ok", json!(false)),
                    ("reason", json!("eve was not told where that is")),
                    ("things", json!(things)),
                ]));
            };
            let mut out = obj([("ok", json!(true))]);
            out.extend(self.thing_out(con, hit)?);
            Ok(out)
        })
    }

    pub fn list_things(&self) -> Res<Map<String, Value>> {
        self.db(|con| {
            let rows = all(con, "select * from things order by at desc limit 50", [], ThingRow::from_row)?;
            let things = rows.iter().map(|r| self.thing_out(con, r).map(Value::Object)).collect::<Res<Vec<_>>>()?;
            Ok(obj([("ok", json!(true)), ("things", Value::Array(things))]))
        })
    }

    pub fn forget_thing(&self, thing: &str) -> Res<Map<String, Value>> {
        let key = Self::key(thing);
        let n = self.db(|con| Ok(con.execute("delete from things where key = ?", [&key])?))?;
        let mut out = obj([("ok", json!(n > 0))]);
        if n == 0 {
            out.insert("reason".into(), json!("eve was not keeping that"));
        }
        Ok(out)
    }

    // ---- upkeep --------------------------------------------------------------------------------------------------

    /// Raw fixes past FIX_DAYS, visits past keep_days, and places no visit or thing points at any more.
    pub fn prune(&self) -> Res<IndexMap<String, usize>> {
        let now = self.ts();
        let keep = keep_days(self.env());
        self.db(|con| {
            let fixes = con.execute("delete from fixes where at < ?", [now - (FIX_DAYS * 86400) as f64])?;
            let visits = con
                .execute("delete from visits where coalesce(departed, arrived) < ?", [now - (keep * 86400) as f64])?;
            let places = con.execute(
                "delete from places where name is null and id not in (select place_id from visits) \
                 and id not in (select place_id from things where place_id is not null)",
                [],
            )?;
            Ok(IndexMap::from([("fixes".to_string(), fixes), ("visits".to_string(), visits), ("places".to_string(), places)]))
        })
    }

    pub fn summary(&self) -> Res<Map<String, Value>> {
        let (counts, first) = self.db(|con| {
            let mut counts = Vec::new();
            for t in ["places", "visits", "things"] {
                let n: i64 = con.query_row(&format!("select count(*) from {t}"), [], |r| r.get(0))?;
                counts.push((t, n));
            }
            let first: Option<f64> = con.query_row("select min(arrived) from visits", [], |r| r.get(0))?;
            Ok((counts, first))
        })?;
        let mut out: Map<String, Value> = counts.into_iter().map(|(k, v)| (k.to_string(), json!(v))).collect();
        let since = first.filter(|f| *f != 0.0).map(|f| self.local(f).date().format("%Y-%m-%d").to_string());
        out.insert("since".into(), json!(since));
        Ok(out)
    }

    /// Everything: the history, the places, the things, and the phone's latest fix.
    pub fn forget(&self) -> Res<IndexMap<String, usize>> {
        let counts = self.db(|con| {
            let mut counts = IndexMap::new();
            for t in ["places", "visits", "fixes", "things"] {
                counts.insert(t.to_string(), con.execute(&format!("delete from {t}"), [])?);
            }
            con.execute("delete from meta", [])?;
            Ok(counts)
        })?;
        let _ = std::fs::remove_file(phone_location::state_path(self.env()));
        tracing::info!(
            "places: forgot {}",
            counts.iter().map(|(k, v)| format!("{v} {k}")).collect::<Vec<_>>().join(", ")
        );
        Ok(counts)
    }
}

// ---- the brain's tools ---------------------------------------------------------------------------------------------

pub const RECALL: &str = "places_recall";
pub const NAME: &str = "place_name";
pub const THING: &str = "thing_place";
pub const TOOL_NAMES: [&str; 3] = [RECALL, NAME, THING];

pub static TOOLS: LazyLock<Vec<Value>> = LazyLock::new(|| {
    vec![
        json!({"type": "function", "name": RECALL, "strict": true,
         "description": concat!(
             "What the owner's phone says about where they have been, from the places history on their Mac: ",
             "a day's digest (the places in order with times, time at home, at work and out, how far they ",
             "moved, new places, usual places they missed), a week's digest, the timeline of visits over ",
             "some days, the places they go with their habits (usual days, arrival time, how long), or the ",
             "known places. Answers are place names and local times. Use it for where was I, when did I ",
             "get home, how was my day or week, how often do I go to, what is my routine, and when did I ",
             "last go somewhere."),
         "parameters": {"type": "object", "additionalProperties": false,
                        "required": ["what", "start_date", "end_date"],
                        "properties": {
                            "what": {"type": "string", "enum": ["day", "week", "timeline", "habits", "places"],
                                     "description": concat!(
                                         "day: one day's digest; week: the week holding start_date; ",
                                         "timeline: every visit from start_date to end_date; habits: ",
                                         "patterns over the last 8 weeks; places: the known places.")},
                            "start_date": {"type": "string",
                                           "description": "YYYY-MM-DD in the owner's time zone; empty for today."},
                            "end_date": {"type": "string",
                                         "description": "YYYY-MM-DD, the last day of a timeline; empty for start_date."}}}}),
        json!({"type": "function", "name": NAME, "strict": true,
         "description": concat!(
             "Give a place the name the owner uses for it, in their words (\"this is my mom's place\", ",
             "\"call this gym by its name\"). place is empty for where the owner is now, or a known place's ",
             "current name. An empty name clears it."),
         "parameters": {"type": "object", "additionalProperties": false, "required": ["name", "place"],
                        "properties": {"name": {"type": "string"}, "place": {"type": "string"}}}}),
        json!({"type": "function", "name": THING, "strict": true,
         "description": concat!(
             "Where things are: remember where the owner left or parked something (\"my car is here\", ",
             "\"I left my jacket at a friend's\"), find it again with how far it is from here and a map ",
             "link, list what is kept, or forget one. remember keeps it at where the owner is now unless ",
             "place names a known place."),
         "parameters": {"type": "object", "additionalProperties": false,
                        "required": ["action", "thing", "note", "place"],
                        "properties": {
                            "action": {"type": "string", "enum": ["remember", "find", "list", "forget"]},
                            "thing": {"type": "string", "description": "The thing, e.g. car, bike, jacket; empty for list."},
                            "note": {"type": "string", "description": concat!(
                                "A detail the owner gave to keep with it, in ",
                                "their words; empty for none.")},
                            "place": {"type": "string", "description": "A known place's name; empty for here."}}}}),
    ]
});

/// The tools ride while places are on and the phone has shared a location (phone_location::offered).
pub fn offered(environ: Option<&Env>) -> bool {
    enabled(environ) && phone_location::offered(environ)
}

pub fn tools(environ: Option<&Env>) -> Vec<Value> {
    if offered(environ) {
        TOOLS.clone()
    } else {
        Vec::new()
    }
}

fn day_arg(raw: Option<&Value>, default: NaiveDate) -> Res<NaiveDate> {
    let text = py::truthy_str(raw).unwrap_or_default();
    let text = text.trim();
    if text.is_empty() {
        return Ok(default);
    }
    if !DAY.is_match(text) {
        return Err(PlacesError::Value("dates are YYYY-MM-DD".into()));
    }
    // date.fromisoformat's own refusals
    let invalid = || PlacesError::Value(format!("Invalid isoformat string: '{text}'"));
    if !text.is_ascii() {
        return Err(invalid());
    }
    let year: i32 = text[0..4].parse().map_err(|_| invalid())?;
    let month: u32 = text[5..7].parse().map_err(|_| invalid())?;
    let day: u32 = text[8..10].parse().map_err(|_| invalid())?;
    if !(1..=9999).contains(&year) {
        return Err(PlacesError::Value(format!("year {year} is out of range")));
    }
    if !(1..=12).contains(&month) {
        return Err(PlacesError::Value("month must be in 1..12".into()));
    }
    NaiveDate::from_ymd_opt(year, month, day).ok_or_else(|| PlacesError::Value("day is out of range for month".into()))
}

fn refusal(reason: impl Into<String>) -> Map<String, Value> {
    obj([("ok", json!(false)), ("reason", json!(reason.into()))])
}

fn with_ok(body: Map<String, Value>) -> Map<String, Value> {
    let mut out = obj([("ok", json!(true))]);
    out.extend(body);
    out
}

fn call_inner(s: &Places, name: &str, args: &Map<String, Value>) -> Res<Map<String, Value>> {
    let arg = |k: &str| py::truthy_str(args.get(k)).unwrap_or_default();
    let today = s.local(s.ts()).date();
    if name == RECALL {
        let what = py::truthy_str(args.get("what")).unwrap_or_else(|| "day".into());
        let first = day_arg(args.get("start_date"), today)?;
        return Ok(match what.as_str() {
            "day" => with_ok(s.digest(first)?),
            "week" => with_ok(s.week(first)?),
            "timeline" => {
                let last = day_arg(args.get("end_date"), first)?;
                if last < first || (last - first).num_days() > 31 {
                    return Ok(refusal("a timeline is at most 31 days, end not before start"));
                }
                let visits = s.timeline(first, last)?;
                obj([("ok", json!(true)), ("visits", json!(visits))])
            }
            "habits" => with_ok(s.patterns()?),
            "places" => {
                s.maybe_infer()?;
                let rows = s.db(|con| {
                    Ok(all(
                        con,
                        "select p.*, (select count(*) from visits v where v.place_id = p.id) as c \
                         from places p order by c desc limit 40",
                        [],
                        |r| Ok((PlaceRow::from_row(r)?, r.get::<_, i64>("c")?)),
                    )?)
                })?;
                let places: Vec<Value> = rows
                    .iter()
                    .map(|(r, c)| {
                        let mut item = Places::describe(r);
                        item.insert("visits".into(), json!(c));
                        let last = r.last_seen.filter(|l| *l != 0.0).map(|l| s.local(l).format("%Y-%m-%d").to_string());
                        item.insert("last".into(), json!(last));
                        Value::Object(item)
                    })
                    .collect();
                obj([("ok", json!(true)), ("places", Value::Array(places))])
            }
            _ => refusal("what is day, week, timeline, habits or places"),
        });
    }
    if name == NAME {
        return s.name_place(&arg("name"), &arg("place"));
    }
    if name == THING {
        let action = arg("action");
        let thing = arg("thing");
        return match action.as_str() {
            "remember" => s.remember_thing(&thing, &arg("note"), &arg("place")),
            "find" => s.find_thing(&thing),
            "list" => s.list_things(),
            "forget" => s.forget_thing(&thing),
            _ => Ok(refusal("action is remember, find, list or forget")),
        };
    }
    Ok(refusal(format!("{name} is not a places tool")))
}

/// One tool call, synchronously (run it in a worker thread). Never fails.
pub fn call(name: &str, args: &Map<String, Value>, store: Option<&Places>, environ: Option<&Env>) -> Map<String, Value> {
    if !enabled(environ) {
        return refusal(OFF_REASON);
    }
    let owned;
    let s = match store {
        Some(s) => s,
        None => {
            owned = Places::new(None, environ.cloned(), None);
            &owned
        }
    };
    match call_inner(s, name, args) {
        Ok(out) => out,
        Err(PlacesError::Value(reason)) => refusal(reason),
        Err(e) => {
            tracing::warn!("places: {} failed ({})", name, e.kind_name());
            failed()
        }
    }
}

/// The tool's answer when the store could not be read.
pub fn failed() -> Map<String, Value> {
    refusal("the places history could not be read")
}

/// The system context's line: where the owner is now, by name; '' when not known. Never fails.
pub fn here_line(environ: Option<&Env>) -> String {
    if !enabled(environ) {
        return String::new();
    }
    if !state_path(environ).exists() {
        return String::new();
    }
    let now = match Places::new(None, environ.cloned(), None).here() {
        Ok(now) => now,
        Err(e) => {
            tracing::info!("places: where-now unavailable ({})", e.kind_name());
            return String::new();
        }
    };
    let Some(now) = now.filter(|n| !n.is_empty()) else { return String::new() };
    let text = |k: &str| now.get(k).and_then(Value::as_str).filter(|s| !s.is_empty());
    let mut words = text("place").unwrap_or_default().to_string();
    if let Some(kind) = text("kind") {
        words.push_str(&format!(" ({kind})"));
    }
    let since = text("since").map(|s| format!(", since {s}")).unwrap_or_default();
    format!("Where the owner is now, from their phone: {words}{since}.")
}

// ---- the nightly upkeep --------------------------------------------------------------------------------------------

/// Hourly: prune what is past its days and re-infer home and work. Never fails. `shutdown` is the daemon's stop
/// signal (true once set), Python's asyncio.Event.
pub async fn run_loop(mut shutdown: tokio::sync::watch::Receiver<bool>, interval: StdDuration) {
    while !*shutdown.borrow() {
        if enabled(None) && state_path(None).exists() {
            let job = tokio::task::spawn_blocking(|| -> Res<(IndexMap<String, usize>, Labels)> {
                let store = Places::new(None, None, None);
                let pruned = store.prune()?;
                let labels = store.infer_labels()?;
                Ok((pruned, labels))
            })
            .await;
            match job {
                Ok(Ok((pruned, labels))) => {
                    if pruned.values().any(|v| *v > 0) {
                        tracing::info!(
                            "places: pruned {}",
                            pruned.iter().map(|(k, v)| format!("{v} {k}")).collect::<Vec<_>>().join(", ")
                        );
                    }
                    tracing::debug!("places: home {}, work {}", labels.home.is_some(), labels.work.is_some());
                }
                Ok(Err(e)) => tracing::warn!("places: upkeep failed ({})", e.kind_name()),
                Err(_) => tracing::warn!("places: upkeep failed (RuntimeError)"),
            }
        }
        // upkeep tries again next hour, or stops at once on shutdown
        if let Ok(Err(_)) = tokio::time::timeout(interval, shutdown.wait_for(|stop| *stop)).await {
            // the sender is gone, so nothing can stop the loop early any more: wait out the hour
            tokio::time::sleep(interval).await;
        }
    }
}
