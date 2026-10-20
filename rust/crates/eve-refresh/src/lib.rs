//! The weekly refresh reminder (docs/refresh.md). Port of `refresh.py`.
//!
//! Owner, 2026-10-16, after the widget went blank: "make a weekly reminder to refresh eve, both app and auths". Two
//! things lapse every 7 days and fail quietly when they do:
//!
//! * **Eve Link** is signed with a free personal team, so its profile expires 7 days after it was made. The app then
//!   will not open. [`app_expiry`] reads the profile of the last build ([`APP`]), which is what `build.sh --install`
//!   put on the phone.
//! * **Google** sign-in: the OAuth app is in "Testing", so Google drops the refresh token 7 days after sign-in
//!   (docs/apps.md). Calendar, Gmail, Drive and Contacts stop, and the home-screen widget comes back empty.
//!   `google_login` stamps `signed_in_at` in the token file; the file's mtime changes at every hourly refresh, so it
//!   cannot stand in.
//!
//! [`remind`] posts one notification (ntfy, notify.py) and one on the Mac, with both dates and the steps.
//! [`schedule`] writes a LaunchAgent that runs it every Friday at 09:00. It runs the daemon's own program, as the
//! daemon's agent does: launchd may not run a `/bin/sh` script out of ~/Documents ("Operation not permitted").
//!
//! Where Python raises (a program that cannot be started, a token file whose top level is not an object, a
//! timestamp out of range), these functions return `Err`, so the CLI fails the way the Python one does.

use std::io::{self, Cursor};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::LazyLock;
use std::time::SystemTime;

use anyhow::{anyhow, bail};
use chrono::{DateTime, Datelike, Local, NaiveDate, TimeDelta, TimeZone, Utc};
use serde_json::Value as Json;

use eve_brain::notify::{self, NotifyConfig, PostFn};
use eve_service::plistxml::{self, Value as Plist};
use eve_service::{service_launchd, Launch, Launchd};

/// `EVE`: the checkout this crate was built from (Python: `Path(__file__).resolve().parents[3]`, the checkout the
/// module runs from).
pub static EVE: LazyLock<PathBuf> = LazyLock::new(|| {
    let raw = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.."));
    raw.canonicalize().unwrap_or(raw)
});

/// `APP`: the last device build of Eve Link.
pub static APP: LazyLock<PathBuf> =
    LazyLock::new(|| EVE.join("ios/EveLink/build/Build/Products/Debug-iphoneos/EveLink.app"));

/// `GOOGLE_LIFE`: 7 days.
pub const GOOGLE_LIFE_DAYS: i64 = 7;

/// `LABEL`
pub const LABEL: &str = "com.github.eve-bridge.refresh-reminder";

/// `WEEKDAY, HOUR, MINUTE`: Friday 09:00: the app and Google were last refreshed on a Friday.
pub const WEEKDAY: i64 = 5;
pub const HOUR: i64 = 9;
pub const MINUTE: i64 = 0;

/// The launchd default PATH the reminder's agent runs with.
pub const AGENT_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

/// `LOG`: `~/Library/Logs/eve-refresh-reminder.log` under `home`.
pub fn log_path(home: &Path) -> PathBuf {
    home.join("Library").join("Logs").join("eve-refresh-reminder.log")
}

/// `GOOGLE_LIFE` as a duration.
pub fn google_life() -> TimeDelta {
    TimeDelta::days(GOOGLE_LIFE_DAYS)
}

// ---- the process boundary ------------------------------------------------------------------------------------------

/// `subprocess.run(argv, capture_output=True)`: the exit code and the raw stdout.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Ran {
    pub returncode: i32,
    pub stdout: Vec<u8>,
}

/// What [`app_expiry`] and [`remind`] run. Err where `subprocess.run` raises (the program cannot be started).
pub type Run<'a> = &'a dyn Fn(&[String]) -> io::Result<Ran>;

/// The real `subprocess.run(argv, capture_output=True)`: stdin inherited, stdout and stderr captured.
pub fn system_run(argv: &[String]) -> io::Result<Ran> {
    let (program, args) =
        argv.split_first().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "empty command"))?;
    let out = Command::new(program).args(args).stdin(Stdio::inherit()).output()?;
    let returncode = match out.status.code() {
        Some(code) => code,
        None => {
            #[cfg(unix)]
            {
                use std::os::unix::process::ExitStatusExt;
                out.status.signal().map(|s| -s).unwrap_or(-1)
            }
            #[cfg(not(unix))]
            {
                -1
            }
        }
    };
    Ok(Ran { returncode, stdout: out.stdout })
}

// ---- the dates -----------------------------------------------------------------------------------------------------

/// `app_expiry`: when the last build's signing profile expires (UTC), or None when there is no build or it cannot
/// be read.
pub fn app_expiry(app: &Path, run: Run<'_>) -> io::Result<Option<DateTime<Utc>>> {
    let profile = app.join("embedded.mobileprovision");
    if !profile.exists() {
        return Ok(None);
    }
    let argv: Vec<String> =
        ["security", "cms", "-D", "-i"].iter().map(|s| s.to_string()).chain([profile.to_string_lossy().into()]).collect();
    let r = run(&argv)?;
    if r.returncode != 0 {
        return Ok(None);
    }
    // an unreadable profile is "unknown", not a crash
    let Ok(value) = plist::Value::from_reader(Cursor::new(&r.stdout)) else {
        return Ok(None);
    };
    let Some(dict) = value.as_dictionary() else {
        return Ok(None);
    };
    Ok(dict.get("ExpirationDate").and_then(plist::Value::as_date).map(plist_date_utc))
}

/// A plist date as `plistlib` reads it (naive UTC, to the microsecond), made aware in UTC.
fn plist_date_utc(date: plist::Date) -> DateTime<Utc> {
    let st: SystemTime = date.into();
    let dt: DateTime<Utc> = st.into();
    let micros = dt.timestamp_micros() + i64::from(dt.timestamp_subsec_nanos() % 1000 >= 500);
    DateTime::from_timestamp_micros(micros).unwrap_or(dt)
}

/// `google_expiry`: when the Google sign-in lapses: `signed_in_at` plus 7 days, or None (signed in before it was
/// stamped).
pub fn google_expiry(token_path: &Path) -> anyhow::Result<Option<DateTime<Utc>>> {
    let raw = match std::fs::read(token_path) {
        Ok(raw) => raw,
        Err(_) => return Ok(None), // OSError
    };
    // read_text's UnicodeDecodeError and json.loads' JSONDecodeError are both ValueError.
    let Ok(text) = String::from_utf8(raw) else {
        return Ok(None);
    };
    let Ok(doc) = serde_json::from_str::<Json>(&text) else {
        return Ok(None);
    };
    let Json::Object(map) = doc else {
        bail!("AttributeError: '{}' object has no attribute 'get'", py_type_name(&doc));
    };
    let stamp = map.get("signed_in_at").cloned().unwrap_or(Json::Null);
    let signed = match py_float_of_or_zero(&stamp) {
        Float::Value(f) => f,
        Float::ValueError => return Ok(None),
        Float::TypeError(name) => {
            bail!("TypeError: float() argument must be a string or a real number, not '{name}'")
        }
    };
    if signed == 0.0 {
        return Ok(None);
    }
    let when = fromtimestamp_utc(signed)?;
    let lapse = when
        .checked_add_signed(google_life())
        .filter(|d| d.year() <= 9999)
        .ok_or_else(|| anyhow!("OverflowError: date value out of range"))?;
    Ok(Some(lapse))
}

/// What `float(x or 0)` gives.
enum Float {
    Value(f64),
    ValueError,
    TypeError(&'static str),
}

fn py_type_name(v: &Json) -> &'static str {
    match v {
        Json::Null => "NoneType",
        Json::Bool(_) => "bool",
        Json::Number(n) if n.is_f64() => "float",
        Json::Number(_) => "int",
        Json::String(_) => "str",
        Json::Array(_) => "list",
        Json::Object(_) => "dict",
    }
}

/// `float(x or 0)` for a JSON value.
fn py_float_of_or_zero(v: &Json) -> Float {
    match v {
        Json::Null | Json::Bool(false) => Float::Value(0.0),
        Json::Bool(true) => Float::Value(1.0),
        Json::Number(n) => Float::Value(n.as_f64().unwrap_or(f64::INFINITY)),
        Json::String(s) if s.is_empty() => Float::Value(0.0),
        Json::String(s) => py_float_str(s).map_or(Float::ValueError, Float::Value),
        Json::Array(a) if a.is_empty() => Float::Value(0.0),
        Json::Object(o) if o.is_empty() => Float::Value(0.0),
        other => Float::TypeError(py_type_name(other)),
    }
}

/// Python's `float(str)`: surrounding whitespace stripped, `inf`/`infinity`/`nan` in any case, single underscores
/// between digits. None where Python raises ValueError.
fn py_float_str(s: &str) -> Option<f64> {
    let t = s.trim_matches(|c: char| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c));
    if t.is_empty() {
        return None;
    }
    let body = t.strip_prefix(['+', '-']).unwrap_or(t);
    if !body.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '.') {
        return None;
    }
    let word = body.to_ascii_lowercase();
    if matches!(word.as_str(), "inf" | "infinity" | "nan") {
        let neg = t.starts_with('-');
        let x = if word == "nan" { f64::NAN } else { f64::INFINITY };
        return Some(if neg { -x } else { x });
    }
    // only ASCII digits, '.', 'e'/'E', signs after an exponent, and underscores between two digits
    let chars: Vec<char> = body.chars().collect();
    let mut cleaned = String::with_capacity(t.len());
    if t.starts_with('-') {
        cleaned.push('-');
    }
    for (i, &c) in chars.iter().enumerate() {
        match c {
            '_' => {
                let before = i > 0 && chars[i - 1].is_ascii_digit();
                let after = chars.get(i + 1).is_some_and(|c| c.is_ascii_digit());
                if !(before && after) {
                    return None;
                }
            }
            '0'..='9' | '.' | 'e' | 'E' | '+' | '-' => cleaned.push(c),
            _ => return None,
        }
    }
    if !cleaned.trim_start_matches('-').starts_with(|c: char| c.is_ascii_digit() || c == '.') {
        return None;
    }
    cleaned.parse::<f64>().ok()
}

/// `datetime.fromtimestamp(t, timezone.utc)`: the fraction rounded half-even to the microsecond.
fn fromtimestamp_utc(t: f64) -> anyhow::Result<DateTime<Utc>> {
    if t.is_nan() {
        bail!("ValueError: Invalid value NaN (not a number)");
    }
    if !t.is_finite() || t.abs() > 1e18 {
        bail!("OverflowError: timestamp out of range for platform time_t");
    }
    let mut secs = t.floor();
    let mut micros = ((t - secs) * 1e6).round_ties_even();
    if micros >= 1e6 {
        secs += 1.0;
        micros -= 1e6;
    }
    let min = NaiveDate::from_ymd_opt(1, 1, 1).and_then(|d| d.and_hms_opt(0, 0, 0)).map(|d| d.and_utc().timestamp());
    let max = NaiveDate::from_ymd_opt(9999, 12, 31)
        .and_then(|d| d.and_hms_opt(23, 59, 59))
        .map(|d| d.and_utc().timestamp());
    let secs = secs as i64;
    if min.is_some_and(|m| secs < m) || max.is_some_and(|m| secs > m) {
        bail!("ValueError: year is out of range");
    }
    DateTime::from_timestamp(secs, micros as u32 * 1000).ok_or_else(|| anyhow!("ValueError: year is out of range"))
}

// ---- the message ---------------------------------------------------------------------------------------------------

/// `_when`, in this Mac's time zone (`expiry.astimezone()`).
pub fn when(expiry: Option<DateTime<Utc>>, now: DateTime<Utc>) -> String {
    when_in(expiry, now, &Local)
}

/// `_when` with the zone the dates are shown in.
pub fn when_in<Tz: TimeZone>(expiry: Option<DateTime<Utc>>, now: DateTime<Utc>, tz: &Tz) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let Some(expiry) = expiry else {
        return "date unknown".to_string();
    };
    let local = expiry.with_timezone(tz);
    let delta = expiry - now;
    let seconds = delta.num_seconds() as f64 + f64::from(delta.subsec_nanos()) / 1e9;
    let days = seconds / 86400.0;
    if days < 0.0 {
        return format!("expired {}", local.format("%a %b %-d"));
    }
    let left = if days < 1.0 {
        "today".to_string()
    } else if days < 2.0 {
        "in 1 day".to_string()
    } else {
        format!("in {} days", days as i64)
    };
    format!("expires {} ({left})", local.format("%a %b %-d, %-I:%M %p"))
}

/// `message`
pub fn message(now: DateTime<Utc>, app: Option<DateTime<Utc>>, google: Option<DateTime<Utc>>) -> String {
    message_in(now, app, google, &Local)
}

/// `message` with the zone the dates are shown in.
pub fn message_in<Tz: TimeZone>(
    now: DateTime<Utc>,
    app: Option<DateTime<Utc>>,
    google: Option<DateTime<Utc>>,
    tz: &Tz,
) -> String
where
    Tz::Offset: std::fmt::Display,
{
    [
        "Weekly refresh, on the Mac:".to_string(),
        format!("1. Eve Link {}: sh ios/EveLink/build.sh --install (phone unlocked)", when_in(app, now, tz)),
        format!(
            "2. Google {}: eve-bridge apps login google, then restart the daemon",
            when_in(google, now, tz)
        ),
    ]
    .join("\n")
}

/// `current`: the message for now (or `now`), from the last build and the Google token file.
pub fn current(token_path: &Path, now: Option<DateTime<Utc>>) -> anyhow::Result<String> {
    let now = now.unwrap_or_else(Utc::now);
    let app = app_expiry(&APP, &system_run)?;
    Ok(message(now, app, google_expiry(token_path)?))
}

// ---- the reminder --------------------------------------------------------------------------------------------------

/// `json.dumps(text)` (ensure_ascii on): a JSON string literal, non-ASCII as `\uXXXX`.
fn json_dumps_ascii(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            ' '..='~' => out.push(c),
            c => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("\\u{unit:04x}"));
                }
            }
        }
    }
    out.push('"');
    out
}

/// The `osascript` line that shows `text` on the Mac.
pub fn mac_notification_argv(text: &str) -> Vec<String> {
    vec![
        "osascript".into(),
        "-e".into(),
        format!("display notification {} with title \"eve: refresh\"", json_dumps_ascii(text)),
    ]
}

/// `remind`: post `text` on the phone (ntfy) and on the Mac. True when the phone post went out.
pub fn remind(text: &str) -> io::Result<bool> {
    remind_with(text, &system_run, &notify::configured(None), &|| notify::read_token(None), &notify::default_post())
}

/// `remind` with its seams: the process runner, the notify config, the link token and the ntfy post.
pub fn remind_with(
    text: &str,
    run: Run<'_>,
    cfg: &NotifyConfig,
    token: &dyn Fn() -> String,
    post: &PostFn,
) -> io::Result<bool> {
    run(&mac_notification_argv(text))?;
    Ok(cfg.enabled && notify::post_once(&token(), text, &cfg.base, post))
}

// ---- the LaunchAgent -----------------------------------------------------------------------------------------------

/// `plist_path`: `~/Library/LaunchAgents/com.github.eve-bridge.refresh-reminder.plist`.
pub fn plist_path(launchd: &Launchd) -> anyhow::Result<PathBuf> {
    Ok(launchd.agent_path(LABEL)?)
}

/// `build_plist`: runs `<program> refresh remind` every Friday at 09:00, logging to `LOG`.
pub fn build_plist(launchd: &Launchd) -> anyhow::Result<Vec<u8>> {
    let home = launchd.home()?;
    let log = log_path(&home);
    let log = log.to_str().ok_or_else(|| anyhow!("path is not valid UTF-8: {}", log.display()))?.to_string();
    let home = home.to_str().ok_or_else(|| anyhow!("path is not valid UTF-8: {}", home.display()))?.to_string();
    let plist = Plist::Dict(vec![
        ("Label".into(), Plist::s(LABEL)),
        ("ProgramArguments".into(), Plist::Array(program_arguments(&launchd.launch).into_iter().map(Plist::String).collect())),
        (
            "StartCalendarInterval".into(),
            Plist::Dict(vec![
                ("Weekday".into(), Plist::Integer(WEEKDAY)),
                ("Hour".into(), Plist::Integer(HOUR)),
                ("Minute".into(), Plist::Integer(MINUTE)),
            ]),
        ),
        ("StandardOutPath".into(), Plist::s(log.clone())),
        ("StandardErrorPath".into(), Plist::s(log)),
        (
            "EnvironmentVariables".into(),
            Plist::Dict(vec![("HOME".into(), Plist::s(home)), ("PATH".into(), Plist::s(AGENT_PATH))]),
        ),
    ]);
    Ok(plistxml::dumps(&plist)?)
}

/// `[sys.executable, "-m", "eve_bridge.cli", "refresh", "remind"]`, or `[<eve-bridge>, "refresh", "remind"]` for
/// the Rust build.
pub fn program_arguments(launch: &Launch) -> Vec<String> {
    let mut argv = launch.argv("refresh");
    argv.push("remind".into());
    argv
}

/// `schedule`: write the LaunchAgent and load it. 2 without `launchctl`, or when the load fails.
pub fn schedule(launchd: &Launchd) -> anyhow::Result<i32> {
    if launchd.host.which("launchctl").is_none() {
        launchd.host.eprint("eve-bridge: `launchctl` not found on PATH");
        return Ok(2);
    }
    let path = plist_path(launchd)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    service_launchd::write_plist(&path, &build_plist(launchd)?)?;
    let rc = launchd.load(&path)?;
    if rc == 0 {
        launchd.host.print(&format!("Scheduled: every Friday at 09:00 ({}).", path.display()));
    }
    Ok(rc)
}

/// `unschedule`: unload and remove the LaunchAgent.
pub fn unschedule(launchd: &Launchd) -> anyhow::Result<i32> {
    let path = plist_path(launchd)?;
    if !path.exists() {
        launchd.host.print("Not scheduled; nothing to do.");
        return Ok(0);
    }
    launchd.host.run(&["launchctl".into(), "unload".into(), path.to_string_lossy().into_owned()])?;
    std::fs::remove_file(&path)?;
    launchd.host.print(&format!("Removed: {}", path.display()));
    Ok(0)
}

#[cfg(test)]
mod tests;
