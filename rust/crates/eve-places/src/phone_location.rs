//! The phone's live location: the Eve Link app shares where the iPhone is, and eve can read it.
//!
//! Owner, 2026-10-12: "give access to live phone location through app". The app posts a fix to
//! `POST /api/link/location` (link_chat, the link token, any Origin refused) and this module keeps it; eve's brain
//! reads it with one read-only tool, `phone_location`, in the chat, the app chat and out loud.
//!
//! **What is kept.** Only the latest fix, in one small file (`DEFAULT_FILE`, beside the chief's folder and the link
//! token; `EVE_PHONE_LOCATION_FILE` moves it). A new fix replaces it with a temp file in the same folder and a
//! rename (atomic), owner-only (0600). This file has no history: a fix older than (or as old as) the one stored is
//! ignored, so a late retry never moves the owner back. The history of fixes and visits is `places`' (owner,
//! 2026-10-18), in its own file. Coordinates and the place are never logged and never sent anywhere by this module:
//! a log line says "location updated" with the fix's age and accuracy only. The tool's answer goes to the model that
//! asked for it, like any tool result, and is not written to the transcript. quick_answers sends a fresh fix's
//! coordinates, rounded to two decimals (about 1 km), to Open-Meteo for "what's the weather"; events sends them,
//! rounded the same, to Ticketmaster for "concerts near me".
//!
//! **The fix.** `{"lat", "lon", "accuracy_m", "at", "place", "speed_mps"}`: degrees, the horizontal accuracy in
//! meters, when the phone took it (ISO 8601 with a zone), the phone's reverse-geocoded name for it (a town or a
//! street, or null) and the speed in meters per second (null when unknown; iOS's -1, "invalid", is kept as null).
//! `parse` refuses anything else with a reason (the route's 400).
//!
//! **The tool.** `phone_location` (no arguments) answers `{"place", "lat", "lon", "accuracy_m", "age_min",
//! "stale"}` (`stale` past `STALE_MIN` minutes) or `{"error": NO_FIX}`. It is offered while the switch is on and a
//! fix has been stored (`offered`): a Mac whose phone never shared its location sends every prompt byte for byte as
//! before, the way the camera's tools ride only while there is a camera.
//!
//! **The switch.** `EVE_PHONE_LOCATION` (on unless 0/false/no/off): off, the route answers 403 and the tool is
//! neither offered nor answered.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde_json::{json, Map, Value};

use crate::{env_get, expanduser, places, py, Env};

pub const SWITCH: &str = "EVE_PHONE_LOCATION";
pub const FILE_ENV: &str = "EVE_PHONE_LOCATION_FILE";
pub const DEFAULT_FILE: &str = "~/.config/eve-bridge/phone-location.json";
/// A fix older than this many minutes is stale.
pub const STALE_MIN: i64 = 60;
/// A fix stamped further ahead than this (clock skew) is refused.
pub const FUTURE_SECS: f64 = 300.0;
/// 100 km: past this a "fix" says nothing about where the phone is.
pub const MAX_ACCURACY_M: f64 = 100_000.0;
/// Faster than an airliner is garbage.
pub const MAX_SPEED_MPS: f64 = 350.0;
pub const MAX_PLACE_CHARS: usize = 120;
/// About a meter: what the tool gives the model.
pub const COORD_DECIMALS: usize = 5;
pub const NO_FIX: &str = "no location from the phone yet";
pub const OFF_REASON: &str = "Phone location is off on this Mac (EVE_PHONE_LOCATION=0).";
pub const TOOL_NAME: &str = "phone_location";

pub static TOOL: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "function", "name": TOOL_NAME, "strict": true,
        "description": concat!(
            "Where the owner is right now, from their iPhone's GPS, shared by the Eve Link app: the ",
            "place's name (a town or a street, or null), latitude and longitude, the accuracy in meters, ",
            "how many minutes ago the phone took it (age_min) and whether that is over an hour old ",
            "(stale). Use it when the owner asks where they are, what is near them, how far or how long ",
            "it is from here to somewhere, or the time or weather where they are, and they name no other ",
            "place. It is their live position, newer than any home location you were given. When stale ",
            "is true, say how old it is. An error means the phone has not shared a location yet."),
        "parameters": {"type": "object", "additionalProperties": false, "required": [], "properties": {}},
    })
});
pub static TOOLS: LazyLock<Vec<Value>> = LazyLock::new(|| vec![TOOL.clone()]);
/// places' three tools ride with this one (the history it keeps is of these fixes), so every brain that offers and
/// answers phone_location offers and answers them too; the tests check the names match `places::TOOL_NAMES`.
pub const PLACES_TOOL_NAMES: [&str; 3] = ["places_recall", "place_name", "thing_place"];
pub const TOOL_NAMES: [&str; 4] = [TOOL_NAME, "places_recall", "place_name", "thing_place"];

/// One writer at a time in this process: the compare and the replace.
static LOCK: Mutex<()> = Mutex::new(());

/// EVE_PHONE_LOCATION: on unless 0, false, no or off.
pub fn enabled(environ: Option<&Env>) -> bool {
    let raw = env_get(environ, SWITCH).unwrap_or_default();
    !matches!(raw.trim().to_lowercase().as_str(), "0" | "false" | "no" | "off")
}

/// The one file the latest fix lives in: EVE_PHONE_LOCATION_FILE (from `environ`, then the process's), else
/// DEFAULT_FILE.
pub fn state_path(environ: Option<&Env>) -> PathBuf {
    let raw = env_get(environ, FILE_ENV)
        .filter(|v| !v.is_empty())
        .or_else(|| std::env::var(FILE_ENV).ok().filter(|v| !v.is_empty()))
        .unwrap_or_else(|| DEFAULT_FILE.to_string());
    let raw = raw.trim();
    expanduser(if raw.is_empty() { "." } else { raw }) // Python: Path("") is "."
}

/// One fix from the phone. `at` is UTC, to the microsecond (as Python's datetime holds it).
#[derive(Debug, Clone, PartialEq)]
pub struct Fix {
    pub lat: f64,
    pub lon: f64,
    pub accuracy_m: f64,
    pub at: DateTime<Utc>,
    pub place: Option<String>,
    pub speed_mps: Option<f64>,
}

impl Fix {
    /// A fix, its time cut to microseconds so that what is stored reads back equal.
    pub fn new(lat: f64, lon: f64, accuracy_m: f64, at: DateTime<Utc>, place: Option<&str>) -> Self {
        Fix { lat, lon, accuracy_m, at: py::trunc_micros(at), place: place.map(str::to_string), speed_mps: None }
    }

    pub fn as_json(&self) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("lat".into(), json!(self.lat));
        m.insert("lon".into(), json!(self.lon));
        m.insert("accuracy_m".into(), json!(self.accuracy_m));
        m.insert("at".into(), json!(py::isoformat_utc(&self.at)));
        m.insert("place".into(), json!(self.place));
        m.insert("speed_mps".into(), json!(self.speed_mps));
        m
    }

    pub fn age_secs(&self, now: Option<DateTime<Utc>>) -> f64 {
        let now = now.unwrap_or_else(Utc::now);
        ((now - self.at).num_microseconds().unwrap_or(i64::MAX) as f64 / 1e6).max(0.0)
    }
}

/// A body that is not a fix; its message is the route's 400 reason (no value from the body in it).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct BadFix(pub String);

pub(crate) fn number(body: &Map<String, Value>, key: &str, low: f64, high: f64, optional: bool) -> Result<Option<f64>, BadFix> {
    let value = body.get(key).unwrap_or(&Value::Null);
    if value.is_null() && optional {
        return Ok(None);
    }
    let Some(v) = value.as_f64().filter(|_| value.is_number()) else {
        return Err(BadFix(format!("{key} must be a number")));
    };
    if !(v.is_finite() && low <= v && v <= high) {
        return Err(BadFix(format!("{key} must be from {} to {}", py::fmt_g(low), py::fmt_g(high))));
    }
    Ok(Some(v))
}

fn place_of(raw: Option<&Value>) -> Result<Option<String>, BadFix> {
    match raw {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => {
            let text = py::one_line(s);
            if py::chars_len(&text) > MAX_PLACE_CHARS {
                return Err(BadFix(format!("place is at most {MAX_PLACE_CHARS} characters")));
            }
            Ok((!text.is_empty()).then_some(text))
        }
        Some(_) => Err(BadFix("place must be a string or null".into())),
    }
}

/// An ISO 8601 time with a zone, as UTC.
pub(crate) fn when(raw: Option<&Value>) -> Result<DateTime<Utc>, BadFix> {
    let bad = || BadFix("at must be an ISO 8601 time with a zone".into());
    let Some(Value::String(raw)) = raw else { return Err(bad()) };
    let n = py::chars_len(raw);
    if !(10..=40).contains(&n) {
        return Err(bad());
    }
    let mut text = raw.trim().to_string();
    if text.ends_with('Z') || text.ends_with('z') {
        text.pop();
        text.push_str("+00:00");
    }
    let at = py::fromisoformat(&text).map_err(|_| bad())?;
    if at.offset_us.is_none() {
        return Err(bad());
    }
    at.to_utc().ok_or_else(bad)
}

fn parse_inner(body: &Value, now: Option<DateTime<Utc>>) -> Result<Fix, BadFix> {
    let Value::Object(body) = body else { return Err(BadFix("the body must be a JSON object".into())) };
    let lat = number(body, "lat", -90.0, 90.0, false)?.unwrap_or_default();
    let lon = number(body, "lon", -180.0, 180.0, false)?.unwrap_or_default();
    let accuracy = number(body, "accuracy_m", 0.0, MAX_ACCURACY_M, false)?.unwrap_or_default();
    let speed_raw = body.get("speed_mps");
    let speed = match speed_raw {
        Some(v @ Value::Number(_)) if v.as_f64().is_some_and(|s| s < 0.0) => None, // iOS's "invalid" (-1): no speed
        _ => number(body, "speed_mps", 0.0, MAX_SPEED_MPS, true)?,
    };
    let at = when(body.get("at"))?;
    if let Some(now) = now {
        let ahead = (at - now).num_microseconds().unwrap_or(i64::MAX) as f64 / 1e6;
        if ahead > FUTURE_SECS {
            return Err(BadFix("at is in the future".into()));
        }
    }
    Ok(Fix { lat, lon, accuracy_m: accuracy, at, place: place_of(body.get("place"))?, speed_mps: speed })
}

/// The app's body as a Fix, or BadFix with the reason. Keys other than the six are ignored.
pub fn parse(body: &Value, now: Option<DateTime<Utc>>) -> Result<Fix, BadFix> {
    parse_inner(body, Some(now.unwrap_or_else(Utc::now)))
}

/// The stored fix, or None (no file, or one that is not a fix: never fails).
pub fn load(path: Option<&Path>) -> Option<Fix> {
    let path = path.map(Path::to_path_buf).unwrap_or_else(|| state_path(None));
    let text = std::fs::read_to_string(&path).ok()?;
    let raw: Value = serde_json::from_str(&text).ok()?;
    parse_inner(&raw, None).ok() // what was stored is never "future"
}

/// Keep `fix` as the only one, atomically and owner-only. Ok(false) (nothing written) when the stored fix is as new
/// or newer.
pub fn store(fix: &Fix, path: Option<&Path>) -> std::io::Result<bool> {
    store_with(fix, path, |from, to| std::fs::rename(from, to))
}

/// `store` with the rename lent (the tests make it fail).
pub(crate) fn store_with(
    fix: &Fix,
    path: Option<&Path>,
    replace: impl FnOnce(&Path, &Path) -> std::io::Result<()>,
) -> std::io::Result<bool> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

    let path = path.map(Path::to_path_buf).unwrap_or_else(|| state_path(None));
    let _held = LOCK.lock();
    if let Some(old) = load(Some(&path)) {
        if fix.at <= old.at {
            return Ok(false);
        }
    }
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    if !parent.exists() {
        if let Some(grand) = parent.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(grand)?;
        }
        match std::fs::DirBuilder::new().mode(0o700).create(parent) {
            Err(e) if e.kind() != std::io::ErrorKind::AlreadyExists => return Err(e),
            _ => {}
        }
    }
    let mut tmp = tempfile::Builder::new().prefix(".phone-location-").suffix(".tmp").tempfile_in(parent)?;
    tmp.as_file().set_permissions(std::fs::Permissions::from_mode(0o600))?;
    tmp.write_all(py::dumps(&Value::Object(fix.as_json())).as_bytes())?;
    tmp.flush()?;
    tmp.as_file().sync_all()?;
    let tmp_path = tmp.into_temp_path();
    replace(&tmp_path, &path)?; // on failure the temp file is removed as tmp_path drops
    // renamed away: nothing left for the guard to remove
    let _ = tmp_path.keep();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    Ok(true)
}

/// The stored fix while the switch is on and it is at most STALE_MIN old; None otherwise.
pub fn fresh(environ: Option<&Env>) -> Option<Fix> {
    fresh_within(environ, STALE_MIN as f64, None)
}

/// `fresh` with Python's keyword arguments: the age limit in minutes and the clock.
pub fn fresh_within(environ: Option<&Env>, max_age_min: f64, now: Option<DateTime<Utc>>) -> Option<Fix> {
    if !enabled(environ) {
        return None;
    }
    let fix = load(Some(&state_path(environ)))?;
    if fix.age_secs(now) > max_age_min * 60.0 {
        return None;
    }
    Some(fix)
}

/// Whether a turn offers the tool: the switch is on and the phone has shared a fix (any age).
pub fn offered(environ: Option<&Env>) -> bool {
    enabled(environ) && state_path(environ).is_file()
}

/// The tool for this turn, or [] (`offered`), and places' while places are on.
pub fn tools(environ: Option<&Env>) -> Vec<Value> {
    if !offered(environ) {
        return Vec::new();
    }
    let mut out = TOOLS.clone();
    out.extend(places::tools(environ));
    out
}

/// The tool's answer.
pub fn reading(environ: Option<&Env>, now: Option<DateTime<Utc>>) -> Map<String, Value> {
    let mut out = Map::new();
    let Some(fix) = load(Some(&state_path(environ))) else {
        out.insert("error".into(), json!(NO_FIX));
        return out;
    };
    let age = fix.age_secs(now);
    out.insert("place".into(), json!(fix.place));
    out.insert("lat".into(), json!(py::round_to(fix.lat, COORD_DECIMALS)));
    out.insert("lon".into(), json!(py::round_to(fix.lon, COORD_DECIMALS)));
    out.insert("accuracy_m".into(), json!(py::round_int(fix.accuracy_m)));
    out.insert("age_min".into(), json!((age / 60.0).floor() as i64));
    out.insert("stale".into(), json!(age > (STALE_MIN * 60) as f64));
    // the place in the owner's words when the history knows it (places): "Home, since 18:40", not a coordinate
    if places::enabled(environ) && places::state_path(environ).exists() {
        let known = match places::Places::new(None, environ.cloned(), None).here() {
            Ok(known) => known,
            Err(e) => {
                tracing::info!("phone location: the place history could not be read ({})", e.kind_name());
                None
            }
        };
        if let Some(known) = known.filter(|k| !k.is_empty()) {
            let mut at = Map::new();
            for k in ["place", "kind", "is", "since"] {
                if let Some(v) = known.get(k) {
                    at.insert(k.into(), v.clone());
                }
            }
            out.insert("at".into(), Value::Object(at));
        }
    }
    out
}

/// One call of the tool (it takes no arguments). Off: the reason, never the stored fix.
pub async fn handle(name: &str, args: &Map<String, Value>, environ: Option<&Env>) -> Map<String, Value> {
    let env = environ.cloned();
    if PLACES_TOOL_NAMES.contains(&name) {
        let (name, args) = (name.to_string(), args.clone());
        return tokio::task::spawn_blocking(move || places::call(&name, &args, None, env.as_ref()))
            .await
            .unwrap_or_else(|e| std::panic::resume_unwind(e.into_panic()));
    }
    if name != TOOL_NAME {
        let mut out = Map::new();
        out.insert("ok".into(), json!(false));
        out.insert("reason".into(), json!(format!("{name} is not the phone's location tool")));
        return out;
    }
    if !enabled(environ) {
        let mut out = Map::new();
        out.insert("ok".into(), json!(false));
        out.insert("reason".into(), json!(OFF_REASON));
        return out;
    }
    tokio::task::spawn_blocking(move || reading(env.as_ref(), None))
        .await
        .unwrap_or_else(|e| std::panic::resume_unwind(e.into_panic()))
}

/// A log line's words for a fix: its age and accuracy, never where it is.
pub fn describe(fix: &Fix, now: Option<DateTime<Utc>>) -> String {
    format!("age {} s, accuracy {} m", py::round_int(fix.age_secs(now)), py::round_int(fix.accuracy_m))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    #[test]
    fn test_a_failed_write_leaves_the_old_fix_and_no_temp_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("phone-location.json");
        let old = Fix::new(0.0, 0.0, 10.0, Utc::now() - Duration::minutes(10), Some("Null Island"));
        assert!(store(&old, Some(&path)).unwrap());
        let newer = Fix::new(0.0, 0.0, 10.0, Utc::now() - Duration::minutes(1), Some("Null Island"));
        let err = store_with(&newer, Some(&path), |_, _| Err(std::io::Error::other("disk full"))).unwrap_err();
        assert_eq!(err.to_string(), "disk full");
        assert_eq!(load(Some(&path)), Some(old));
        let names: Vec<String> =
            std::fs::read_dir(tmp.path()).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        assert_eq!(names, ["phone-location.json"]);
    }
}
