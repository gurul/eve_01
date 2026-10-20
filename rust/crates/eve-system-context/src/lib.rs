//! Owner-configured location and an aware clock snapshot for Eve's prompts: a port of `system_context.py`.
//!
//! `context()` is appended to every brain's instructions (voice, think, Telegram's trailing developer item), so its
//! text is product behavior and is kept verbatim.

use chrono::{DateTime, Offset, TimeZone, Utc};

pub use eve_places::Env;

/// Where the owner's zone came from: a named zone (EVE_TIMEZONE, key kept as written), else this Mac's local time.
enum OwnerZone {
    /// `ZoneInfo(key)`: `key` is what the owner wrote (stripped), as `ZoneInfo.key` keeps it.
    Named { key: String, tz: chrono_tz::Tz },
    Local,
}

/// `os.environ` or the lent mapping.
fn env_get(environ: Option<&Env>, key: &str) -> Option<String> {
    match environ {
        Some(map) => map.get(key).cloned(),
        None => std::env::var(key).ok(),
    }
}

/// `ZoneInfo(name)`, or None where Python catches ZoneInfoNotFoundError / ValueError.
///
/// zoneinfo opens a file under the zone directory, so on this Mac's case-insensitive file system a key in the wrong
/// case still loads (and keeps its spelling); a key that is absolute, climbs out with `..`, or names a directory or a
/// non-zone file is refused, as chrono-tz refuses it.
fn zone_info(name: &str) -> Option<OwnerZone> {
    if name.is_empty() {
        return None;
    }
    let tz = name
        .parse::<chrono_tz::Tz>()
        .ok()
        .or_else(|| chrono_tz::Tz::from_str_insensitive(name).ok())
        // the zone directory's `posixrules` is a copy of New York's rules; chrono-tz has no entry for it
        .or_else(|| name.eq_ignore_ascii_case("posixrules").then_some(chrono_tz::America::New_York))?;
    Some(OwnerZone::Named { key: name.to_string(), tz })
}

/// Python's `str.split()` whitespace: Unicode White_Space plus the four separators `\x1c`..`\x1f`.
fn is_py_space(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

/// `" ".join(raw.split())[:300]`.
fn one_line(raw: &str, limit: usize) -> String {
    raw.split(is_py_space).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ").chars().take(limit).collect()
}

/// A UTC offset as `isoformat` writes it: `+HH:MM`, with `:SS` only when the seconds are not zero.
fn iso_offset(secs: i32) -> String {
    let sign = if secs < 0 { '-' } else { '+' };
    let a = secs.unsigned_abs();
    let (h, m, s) = (a / 3600, a % 3600 / 60, a % 60);
    if s == 0 {
        format!("{sign}{h:02}:{m:02}")
    } else {
        format!("{sign}{h:02}:{m:02}:{s:02}")
    }
}

#[cfg(unix)]
extern "C" {
    // POSIX; the libc crate does not bind it on every target
    fn tzset();
}

#[cfg(unix)]
/// `instant.astimezone(None)`: this Mac's offset and zone abbreviation at that instant, from the C library's
/// `localtime` (the TZ variable, else /etc/localtime), as CPython's `_local_timezone` reads them.
fn local_offset_and_name(instant: &DateTime<Utc>) -> (i32, String) {
    // CPython floors to whole seconds before localtime()
    let ts = instant.timestamp() as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: tzset reads TZ and the zone file; localtime_r writes only into `tm`, which outlives the call.
    let ok = unsafe {
        tzset();
        !libc::localtime_r(&ts, &mut tm).is_null()
    };
    if ok {
        let name = if tm.tm_zone.is_null() {
            String::new()
        } else {
            // SAFETY: tm_zone points at a NUL-terminated name owned by the C library's zone tables.
            unsafe { std::ffi::CStr::from_ptr(tm.tm_zone) }.to_string_lossy().into_owned()
        };
        return (tm.tm_gmtoff as i32, name);
    }
    local_offset_fallback(instant)
}

#[cfg(not(unix))]
fn local_offset_and_name(instant: &DateTime<Utc>) -> (i32, String) {
    local_offset_fallback(instant)
}

/// Without a C zone name: chrono's local offset, named as `datetime.timezone` names an unnamed offset ("UTC+hh:mm").
fn local_offset_fallback(instant: &DateTime<Utc>) -> (i32, String) {
    let secs = chrono::Local.offset_from_utc_datetime(&instant.naive_utc()).fix().local_minus_utc();
    let name = if secs == 0 { "UTC".to_string() } else { format!("UTC{}", iso_offset(secs)) };
    (secs, name)
}

/// The system context block: the local clock snapshot, the zone, the owner-configured location, where the phone is
/// by name (from the places history), and the warning that the time is a snapshot.
///
/// `environ` is Python's `environ` (None reads the process environment); `now` is the instant to stamp (None is now).
pub fn context(environ: Option<&Env>, now: Option<DateTime<Utc>>) -> String {
    let zone_name = env_get(environ, "EVE_TIMEZONE").unwrap_or_default();
    let zone = zone_info(zone_name.trim()).unwrap_or(OwnerZone::Local);
    let instant = now.unwrap_or_else(Utc::now);
    let (wall, offset, tzname) = match &zone {
        OwnerZone::Named { key, tz } => {
            let local = instant.with_timezone(tz);
            (local.naive_local(), local.offset().fix().local_minus_utc(), key.clone())
        }
        OwnerZone::Local => {
            let (secs, name) = local_offset_and_name(&instant);
            (instant.naive_utc() + chrono::Duration::seconds(i64::from(secs)), secs, name)
        }
    };
    let stamp = format!("{}{}", wall.format("%Y-%m-%dT%H:%M:%S"), iso_offset(offset));
    let location = one_line(&env_get(environ, "EVE_LOCATION").unwrap_or_default(), 300);
    let mut lines = vec![
        "\n\nSystem context:".to_string(),
        format!("Local clock when this context was generated: {stamp}."),
        format!("Timezone: {tzname}."),
    ];
    if !location.is_empty() {
        lines.push(format!("Owner-configured location: {location}."));
        lines.push(
            "Use this location for local questions unless the owner names another place; \
             it is a configured default, not live GPS."
                .to_string(),
        );
    }
    // where the phone is, by name, from the places history (places.rs): the brain reads "Home, since 18:40", never a
    // coordinate; nothing while places are off or the phone has not shared a fix in the last hour
    let here = eve_places::places::here_line(environ);
    if !here.is_empty() {
        lines.push(here);
    }
    lines.push(
        "The timestamp is a snapshot, not a ticking clock. For the current time later in a \
         conversation, use a fresh time lookup. Do not treat an old snapshot as the current time."
            .to_string(),
    );
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_one_line_splits_like_python() {
        assert_eq!(one_line("  Example\x1c Apartments,\n\u{a0}Seattle\t ", 300), "Example Apartments, Seattle");
        assert_eq!(one_line("\u{200b}x", 300), "\u{200b}x"); // zero-width space is not whitespace to Python
        assert_eq!(one_line(&"é".repeat(400), 300).chars().count(), 300);
        assert_eq!(one_line("   ", 300), "");
    }

    #[test]
    fn test_iso_offset_matches_isoformat() {
        assert_eq!(iso_offset(0), "+00:00");
        assert_eq!(iso_offset(-8 * 3600), "-08:00");
        assert_eq!(iso_offset(19800), "+05:30");
        assert_eq!(iso_offset(21208), "+05:53:28");
        assert_eq!(iso_offset(-(3600 + 30)), "-01:00:30");
    }

    #[test]
    fn test_zone_info_keeps_the_key_and_refuses_what_zoneinfo_refuses() {
        let key = |n: &str| match zone_info(n) {
            Some(OwnerZone::Named { key, .. }) => Some(key),
            _ => None,
        };
        assert_eq!(key("america/los_angeles").as_deref(), Some("america/los_angeles"));
        assert_eq!(key("US/Pacific").as_deref(), Some("US/Pacific"));
        assert_eq!(key("Etc/GMT+5").as_deref(), Some("Etc/GMT+5"));
        assert_eq!(key("posixrules").as_deref(), Some("posixrules"));
        for bad in ["", "America", "Missing/Zone", "../etc", "/etc/x", "zone.tab", "localtime"] {
            assert!(key(bad).is_none(), "{bad}");
        }
    }
}
