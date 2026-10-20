//! `datetime.fromisoformat(...)` and `.astimezone()` as CPython 3.13 runs them, for `_record_is_today` and
//! `_today_key`.
//!
//! The tailer only needs a local calendar date, but which strings parse and which instants raise decide whether
//! a record counts toward today, and an exception out of `astimezone()` (not caught by `_record_is_today`)
//! ends that file's pass. So this ports `_datetimemodule.c`'s ISO parser byte for byte (it reads past the end
//! of the string up to its NUL terminator, and that is modeled by reading 0 there) and its `astimezone()`
//! conversion through the C library's `localtime`.

use chrono::{Datelike, NaiveDate};

/// The local clock as `localtime()` reports it: the UTC offset in seconds (`tm_gmtoff`) at a Unix timestamp.
/// `None` where `localtime()` fails.
pub trait LocalZone: Send + Sync {
    fn utc_offset(&self, timestamp: i64) -> Option<i64>;
}

/// The process time zone through the C library's `localtime_r`, as CPython reads it.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemZone;

#[cfg(unix)]
impl LocalZone for SystemZone {
    fn utc_offset(&self, timestamp: i64) -> Option<i64> {
        static TZSET: std::sync::Once = std::sync::Once::new();
        // CPython calls tzset() when the time module loads; localtime_r is not required to.
        extern "C" {
            fn tzset();
        }
        // SAFETY: tzset takes no arguments and only reads TZ and the zoneinfo files.
        TZSET.call_once(|| unsafe { tzset() });
        #[allow(clippy::useless_conversion)] // time_t is i64 on the 64-bit targets, narrower elsewhere
        let t: libc::time_t = timestamp.try_into().ok()?;
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        // SAFETY: both pointers are valid for the call; localtime_r writes only into `tm`.
        let r = unsafe { libc::localtime_r(&t, &mut tm) };
        if r.is_null() {
            return None;
        }
        Some(tm.tm_gmtoff as i64)
    }
}

#[cfg(not(unix))]
impl LocalZone for SystemZone {
    fn utc_offset(&self, timestamp: i64) -> Option<i64> {
        use chrono::{Local, Offset, TimeZone};
        let dt = chrono::DateTime::from_timestamp(timestamp, 0)?;
        Some(Local.offset_from_utc_datetime(&dt.naive_utc()).fix().local_minus_utc() as i64)
    }
}

/// An exception `astimezone()` raises (`ValueError`, `OverflowError` or `OSError` in Python). `_record_is_today`
/// does not catch it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TimeError {
    #[error("date value out of range")]
    Overflow,
    #[error("year {0} is out of range")]
    YearOutOfRange(i64),
    #[error("timestamp out of range for platform time_t")]
    Localtime,
}

/// A parsed `datetime`: wall-clock fields plus a fixed UTC offset in microseconds for an aware value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IsoDateTime {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    pub microsecond: u32,
    /// `None` for a naive datetime.
    pub offset_us: Option<i64>,
}

const MAXORDINAL: i64 = 3_652_059; // date(9999, 12, 31).toordinal()
const EPOCH: i64 = 719_163 * 86_400; // date(1970, 1, 1).toordinal() in seconds
const US: i64 = 1_000_000;
const DAY_US: i64 = 86_400 * US;
const MAX_FOLD_SECONDS: i64 = 24 * 3600;

fn ymd_to_ord(y: i32, m: u32, d: u32) -> Option<i64> {
    NaiveDate::from_ymd_opt(y, m, d).map(|n| n.num_days_from_ce() as i64)
}

fn ord_to_ymd(ord: i64) -> Option<(i32, u32, u32)> {
    let n = NaiveDate::from_num_days_from_ce_opt(i32::try_from(ord).ok()?)?;
    Some((n.year(), n.month(), n.day()))
}

fn is_leap(y: i32) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

// ---- datetime.fromisoformat ----------------------------------------------------------------------------------

/// The byte at `i`, or 0 past the end (the C string's terminator, and what reading on past it finds first).
fn at(s: &[u8], i: usize) -> u8 {
    s.get(i).copied().unwrap_or(0)
}

fn is_digit(c: u8) -> bool {
    c.is_ascii_digit()
}

/// `parse_digits`: `n` ASCII digits from `p`, or `None`.
fn parse_digits(s: &[u8], p: usize, n: usize, var: &mut i64) -> Option<usize> {
    for k in 0..n {
        let c = at(s, p + k);
        if !is_digit(c) {
            return None;
        }
        *var = *var * 10 + (c - b'0') as i64;
    }
    Some(p + n)
}

/// `_find_isoformat_datetime_separator`.
fn find_separator(s: &[u8]) -> Option<usize> {
    let len = s.len();
    if len == 7 {
        return Some(7);
    }
    if at(s, 4) == b'-' {
        if at(s, 5) == b'W' {
            if len < 8 {
                return None;
            }
            if len > 8 && at(s, 8) == b'-' {
                if len == 9 {
                    return None;
                }
                if len > 10 && is_digit(at(s, 10)) {
                    return Some(8);
                }
                return Some(10);
            }
            return Some(8);
        }
        return Some(10);
    }
    if at(s, 4) == b'W' {
        let mut idx = 7;
        while idx < len {
            if !is_digit(s[idx]) {
                break;
            }
            idx += 1;
        }
        if idx < 9 {
            return Some(idx);
        }
        return Some(if idx % 2 == 0 { 7 } else { 8 });
    }
    Some(8)
}

/// `iso_to_ymd`.
fn iso_to_ymd(iso_year: i64, iso_week: i64, iso_day: i64) -> Option<(i32, u32, u32)> {
    if !(1..=9999).contains(&iso_year) {
        return None;
    }
    let y = iso_year as i32;
    if iso_week <= 0 || iso_week >= 53 {
        let mut out_of_range = true;
        if iso_week == 53 {
            // ISO years have 53 weeks on years starting with a Thursday and on leap years starting on Wednesday.
            let first_weekday = (ymd_to_ord(y, 1, 1)? + 6) % 7;
            if first_weekday == 3 || (first_weekday == 2 && is_leap(y)) {
                out_of_range = false;
            }
        }
        if out_of_range {
            return None;
        }
    }
    if iso_day <= 0 || iso_day >= 8 {
        return None;
    }
    let first_day = ymd_to_ord(y, 1, 1)?;
    let first_weekday = (first_day + 6) % 7;
    let mut week1_monday = first_day - first_weekday;
    if first_weekday > 3 {
        week1_monday += 7;
    }
    let day_offset = (iso_week - 1) * 7 + iso_day - 1;
    ord_to_ymd(week1_monday + day_offset)
}

/// `parse_isoformat_date` over the first `len` bytes: (year, month, day), not yet range-checked.
fn parse_date(s: &[u8], len: usize) -> Option<(i64, i64, i64)> {
    let mut year = 0;
    let mut p = parse_digits(s, 0, 4, &mut year)?;
    let uses_separator = at(s, p) == b'-';
    if uses_separator {
        p += 1;
    }
    if at(s, p) == b'W' {
        p += 1;
        let mut iso_week = 0;
        let mut iso_day = 0;
        p = parse_digits(s, p, 2, &mut iso_week)?;
        if p < len {
            if uses_separator {
                let c = at(s, p);
                p += 1;
                if c != b'-' {
                    return None;
                }
            }
            parse_digits(s, p, 1, &mut iso_day)?;
        } else {
            iso_day = 1;
        }
        let (y, m, d) = iso_to_ymd(year, iso_week, iso_day)?;
        return Some((y as i64, m as i64, d as i64));
    }
    let mut month = 0;
    p = parse_digits(s, p, 2, &mut month)?;
    if uses_separator {
        let c = at(s, p);
        p += 1;
        if c != b'-' {
            return None;
        }
    }
    let mut day = 0;
    parse_digits(s, p, 2, &mut day)?;
    Some((year, month, day))
}

/// `parse_hh_mm_ss_ff` over `s[p..p_end]`: ([h, m, s], microsecond, more-follows), or `None` on a malformed time.
fn parse_hh_mm_ss_ff(s: &[u8], mut p: usize, p_end: usize) -> Option<([i64; 3], i64, bool)> {
    let mut vals = [0i64; 3];
    let mut microsecond = 0i64;
    let mut has_separator = true;
    let mut i = 0;
    while i < 3 {
        p = parse_digits(s, p, 2, &mut vals[i])?;
        let c = at(s, p);
        p += 1;
        if i == 0 {
            has_separator = c == b':';
        }
        if p >= p_end {
            return Some((vals, 0, c != 0));
        } else if has_separator && c == b':' {
            i += 1;
            continue;
        } else if c == b'.' || c == b',' {
            break;
        } else if !has_separator {
            p -= 1;
        } else {
            return None;
        }
        i += 1;
    }
    let len_remains = p_end - p;
    let to_parse = len_remains.min(6);
    p = parse_digits(s, p, to_parse, &mut microsecond)?;
    const CORRECTION: [i64; 5] = [100_000, 10_000, 1_000, 100, 10];
    if to_parse < 6 {
        microsecond *= CORRECTION[to_parse - 1];
    }
    while is_digit(at(s, p)) {
        p += 1;
    }
    Some((vals, microsecond, at(s, p) != 0))
}

/// `parse_isoformat_time`: time fields and, for an aware value, (offset seconds, offset microseconds).
#[allow(clippy::type_complexity)]
fn parse_time(s: &[u8]) -> Option<([i64; 3], i64, Option<(i64, i64)>)> {
    let len = s.len();
    let mut tz = 0;
    loop {
        let c = at(s, tz);
        if c == b'Z' || c == b'+' || c == b'-' {
            break;
        }
        tz += 1;
        if tz >= len {
            break;
        }
    }
    let (hms, us, rest) = parse_hh_mm_ss_ff(s, 0, tz)?;
    if tz == len {
        return if rest { None } else { Some((hms, us, None)) };
    }
    if at(s, tz) == b'Z' {
        return if at(s, tz + 1) != 0 { None } else { Some((hms, us, Some((0, 0)))) };
    }
    let sign = if at(s, tz) == b'-' { -1 } else { 1 };
    let (tzhms, tzus, tzrest) = parse_hh_mm_ss_ff(s, tz + 1, len)?;
    if tzrest {
        return None;
    }
    let offset = sign * (tzhms[0] * 3600 + tzhms[1] * 60 + tzhms[2]);
    Some((hms, us, Some((offset, tzus * sign))))
}

/// `datetime.fromisoformat(text)`, or `None` where it raises `ValueError`.
pub fn fromisoformat(text: &str) -> Option<IsoDateTime> {
    if text.chars().count() < 7 {
        return None;
    }
    let s = text.as_bytes();
    let sep = find_separator(s)?;
    let (year, month, day) = parse_date(s, sep)?;
    let mut time = ([0i64; 3], 0i64, None);
    if s.len() > sep {
        // In UTF-8 the length of a multi-byte character is encoded in its lead byte.
        let lead = s[sep];
        let skip = if lead & 0x80 == 0 {
            1
        } else {
            match lead & 0xf0 {
                0xe0 => 3,
                0xf0 => 4,
                _ => 2,
            }
        };
        let start = sep + skip;
        if start > s.len() {
            return None;
        }
        time = parse_time(&s[start..])?;
    }
    let ([hour, minute, second], microsecond, tz) = time;
    let offset_us = match tz {
        None => None,
        Some((0, _)) => Some(0),
        Some((secs, us)) => {
            let total = secs * US + us;
            // timezone() takes an offset strictly between -24h and +24h.
            if total <= -DAY_US || total >= DAY_US {
                return None;
            }
            Some(total)
        }
    };
    if !(1..=9999).contains(&year)
        || !(1..=12).contains(&month)
        || !(0..=23).contains(&hour)
        || !(0..=59).contains(&minute)
        || !(0..=59).contains(&second)
    {
        return None;
    }
    NaiveDate::from_ymd_opt(year as i32, month as u32, day.clamp(0, 99) as u32)?;
    Some(IsoDateTime {
        year: year as i32,
        month: month as u32,
        day: day as u32,
        hour: hour as u32,
        minute: minute as u32,
        second: second as u32,
        microsecond: microsecond as u32,
        offset_us,
    })
}

// ---- datetime.astimezone() -----------------------------------------------------------------------------------

/// `local(u)`: seconds (ordinal-based, as `utc_to_seconds` counts them) of the local wall time at UTC `u`.
fn local(zone: &dyn LocalZone, u: i64) -> Result<i64, TimeError> {
    let t = u - EPOCH;
    let off = zone.utc_offset(t).ok_or(TimeError::Localtime)?;
    let lt = u + off;
    let ord = lt.div_euclid(86_400);
    if !(1..=MAXORDINAL).contains(&ord) {
        let year = ord_to_ymd(ord).map(|(y, _, _)| y as i64).unwrap_or(0);
        return Err(TimeError::YearOutOfRange(year));
    }
    Ok(lt)
}

/// `local_to_seconds`: solve `t = local(u)` for `u`, picking the earlier (fold 0) or later (fold 1) solution.
fn local_to_seconds(zone: &dyn LocalZone, t: i64, fold: bool) -> Result<i64, TimeError> {
    let lt = local(zone, t)?;
    let a = lt - t;
    let u1 = t - a;
    let t1 = local(zone, u1)?;
    let b;
    if t1 == t {
        let u2 = if fold { u1 + MAX_FOLD_SECONDS } else { u1 - MAX_FOLD_SECONDS };
        let lt = local(zone, u2)?;
        b = lt - u2;
        if a == b {
            return Ok(u1);
        }
    } else {
        b = t1 - u1;
    }
    let u2 = t - b;
    let t2 = local(zone, u2)?;
    if t2 == t {
        return Ok(u2);
    }
    if t1 == t {
        return Ok(u1);
    }
    // Both offsets found, neither solves it: t is in the gap.
    Ok(if fold { u1.min(u2) } else { u1.max(u2) })
}

fn check_range(us: i64) -> Result<i64, TimeError> {
    let ord = us.div_euclid(DAY_US);
    if (1..=MAXORDINAL).contains(&ord) {
        Ok(us)
    } else {
        Err(TimeError::Overflow)
    }
}

/// The local calendar date of `dt.astimezone()`.
pub fn astimezone_date(zone: &dyn LocalZone, dt: &IsoDateTime) -> Result<(i32, u32, u32), TimeError> {
    let ord = ymd_to_ord(dt.year, dt.month, dt.day).ok_or(TimeError::Overflow)?;
    let wall_secs = ord * 86_400 + dt.hour as i64 * 3600 + dt.minute as i64 * 60 + dt.second as i64;
    let wall_us = wall_secs * US + dt.microsecond as i64;
    let offset_us = match dt.offset_us {
        Some(o) => o,
        None => {
            // local_timezone_from_local, with fold 0.
            let mut seconds = local_to_seconds(zone, wall_secs, false)?;
            let seconds2 = local_to_seconds(zone, wall_secs, true)?;
            // Gap: `seconds2 != seconds and (seconds2 > seconds) == fold`, with fold 0.
            if seconds2 < seconds {
                seconds = seconds2;
            }
            zone.utc_offset(seconds - EPOCH).ok_or(TimeError::Localtime)? * US
        }
    };
    let utc = check_range(wall_us - offset_us)?;
    // local_timezone(utc): the offset at the whole second (floor division) of the UTC instant.
    let ts = (utc - EPOCH * US).div_euclid(US);
    let off2 = zone.utc_offset(ts).ok_or(TimeError::Localtime)?;
    let local_us = check_range(utc + off2 * US)?;
    ord_to_ymd(local_us.div_euclid(DAY_US)).ok_or(TimeError::Overflow)
}

/// `date.strftime("%Y-%m-%d")` (CPython pads the year to four digits).
pub fn format_day((y, m, d): (i32, u32, u32)) -> String {
    format!("{y:04}-{m:02}-{d:02}")
}

/// The local date of a Unix timestamp, as `datetime.fromtimestamp(ts).astimezone()` gives it.
pub fn local_day_of_timestamp(zone: &dyn LocalZone, timestamp: i64) -> Result<(i32, u32, u32), TimeError> {
    let off = zone.utc_offset(timestamp).ok_or(TimeError::Localtime)?;
    let local = timestamp + off + EPOCH;
    let ord = local.div_euclid(86_400);
    if !(1..=MAXORDINAL).contains(&ord) {
        return Err(TimeError::Overflow);
    }
    ord_to_ymd(ord).ok_or(TimeError::Overflow)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A zone backed by chrono-tz, standing in for `localtime()` under `TZ=<name>`.
    struct TzZone(chrono_tz::Tz);

    impl LocalZone for TzZone {
        fn utc_offset(&self, timestamp: i64) -> Option<i64> {
            use chrono::{Offset, TimeZone};
            let dt = chrono::DateTime::from_timestamp(timestamp, 0)?;
            Some(self.0.offset_from_utc_datetime(&dt.naive_utc()).fix().local_minus_utc() as i64)
        }
    }

    fn iso(s: &str) -> Option<String> {
        fromisoformat(s).map(|d| {
            let mut out = format!(
                "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
                d.year, d.month, d.day, d.hour, d.minute, d.second
            );
            if d.microsecond != 0 {
                out.push_str(&format!(".{:06}", d.microsecond));
            }
            if let Some(o) = d.offset_us {
                let sign = if o < 0 { '-' } else { '+' };
                let a = o.abs();
                let (secs, us) = (a / US, a % US);
                out.push_str(&format!("{sign}{:02}:{:02}", secs / 3600, secs / 60 % 60));
                if secs % 60 != 0 || us != 0 {
                    out.push_str(&format!(":{:02}", secs % 60));
                }
                if us != 0 {
                    out.push_str(&format!(".{us:06}"));
                }
            }
            out
        })
    }

    /// Expected values from `datetime.fromisoformat(s).isoformat()` under CPython 3.13 (None where it raised).
    #[test]
    fn fromisoformat_matches_cpython() {
        let cases: &[(&str, Option<&str>)] = &[
            ("2026-04-22", Some("2026-04-22T00:00:00")),
            ("20260422", Some("2026-04-22T00:00:00")),
            ("2026-W17-3", Some("2026-04-22T00:00:00")),
            ("2026-W17", Some("2026-04-20T00:00:00")),
            ("2026-112", None),
            ("2026-04", None),
            ("2026-04-22T10", Some("2026-04-22T10:00:00")),
            ("2026-04-22T10:30", Some("2026-04-22T10:30:00")),
            ("2026-04-22T1030", Some("2026-04-22T10:30:00")),
            ("2026-04-22T10:30:15.1", Some("2026-04-22T10:30:15.100000")),
            ("2026-04-22T10:30:15.1234567", Some("2026-04-22T10:30:15.123456")),
            ("2026-04-22T10:30:15,123", Some("2026-04-22T10:30:15.123000")),
            ("2026-04-22 10:30:15+05", Some("2026-04-22T10:30:15+05:00")),
            ("2026-04-22T10:30:15+0530", Some("2026-04-22T10:30:15+05:30")),
            ("2026-04-22T10:30:15+05:30:10", Some("2026-04-22T10:30:15+05:30:10")),
            ("2026-04-22T10:30:15+05:30:10.5", Some("2026-04-22T10:30:15+05:30:10.500000")),
            ("2026-04-22T10:30:15z", None),
            ("2026-04-22T10:30:15+00:00", Some("2026-04-22T10:30:15+00:00")),
            ("2026-04-22T24:00:00", None),
            ("2026-04-22T10:30:60", None),
            ("2026-04-22X10:30", Some("2026-04-22T10:30:00")),
            ("2026-04-22T10:30:15.", None),
            ("2026-04-22T10:30:15.123+05:30", Some("2026-04-22T10:30:15.123000+05:30")),
            ("20260422T103015", Some("2026-04-22T10:30:15")),
            ("2026-04-22T103015.5+00:00", Some("2026-04-22T10:30:15.500000+00:00")),
            ("2026-04-22T10:30:15 +05:00", Some("2026-04-22T10:30:15+05:00")),
            ("2026-04-22T10:30:15-00:00", Some("2026-04-22T10:30:15+00:00")),
            ("+2026-04-22", None),
            ("2026-0422", None),
            ("202604-22", None),
            ("2026-04-22T10:30:15.123456789+01:00", Some("2026-04-22T10:30:15.123456+01:00")),
            ("2026-04-22T10:30:15+24:00", None),
            ("2026-04-22T10:30:15+23:59", Some("2026-04-22T10:30:15+23:59")),
            ("2026-04-22T10:30:15.000+05", Some("2026-04-22T10:30:15+05:00")),
            ("2026-04-22T10:30:15+05:3", None),
            ("2026-04-22T10:3", None),
            ("2026-04-22T10:30:1", None),
            ("2026-04-22T10:30:15UTC", None),
            ("٢٠٢٦-04-22", None),
            ("2026-04-22T", None),
            ("2026-04-22T10:30:15-0", None),
            ("2026-02-29", None),
            ("2024-02-29", Some("2024-02-29T00:00:00")),
            ("2026-W53-1", Some("2026-12-28T00:00:00")),
            ("2020-W53-7", Some("2021-01-03T00:00:00")),
            ("2026-W00-1", None),
            ("2026-W17-8", None),
            ("2026-04-22T10:30:15.12345678912", Some("2026-04-22T10:30:15.123456")),
            ("2026-04-22T10.5", Some("2026-04-22T10:00:00.500000")),
            ("2026-04-22T10:30.5", Some("2026-04-22T10:30:00.500000")),
            ("2026-04-22T10:30:15+05:30.5", Some("2026-04-22T10:30:15+05:30:00.500000")),
            ("2026-04-22T10:30:15+053010", Some("2026-04-22T10:30:15+05:30:10")),
            ("2026-04-22T10:30:15+053010.123456", Some("2026-04-22T10:30:15+05:30:10.123456")),
            ("2026-04-22T10:30:15+05:30:10.1234567", Some("2026-04-22T10:30:15+05:30:10.123456")),
            ("0000-01-01", None),
            ("2026-13-01", None),
            ("2026-04-22é10:30", Some("2026-04-22T10:30:00")),
            ("2026-04-22T10:30:15:5", Some("2026-04-22T10:30:15.500000")),
            ("2026-04-22T10:30:15\u{0}junk", None),
            ("2026-04-22T10:30:15+05:99", Some("2026-04-22T10:30:15+06:39")),
            ("2026-04-22T10:30:15+00:00:00.5", Some("2026-04-22T10:30:15+00:00")),
            ("2026W173", Some("2026-04-22T00:00:00")),
            ("2026W17", Some("2026-04-20T00:00:00")),
            ("2026W17T10", Some("2026-04-20T10:00:00")),
            ("2026W173T10", Some("2026-04-22T10:00:00")),
            ("2026-W17-3T10:30", Some("2026-04-22T10:30:00")),
            ("2026-W1703", None),
            ("9999-W52-6", None),
            ("2026-04-22T10:30:15-05:00", Some("2026-04-22T10:30:15-05:00")),
            ("2026-04-22T10:30:15.5-05:00", Some("2026-04-22T10:30:15.500000-05:00")),
            ("2026-04-22T10:30:15-23:59:59.999999", Some("2026-04-22T10:30:15-23:59:59.999999")),
            ("2026-04-22T10Z", Some("2026-04-22T10:00:00+00:00")),
            ("2026-04-22T10+00:00", Some("2026-04-22T10:00:00+00:00")),
            ("2026-04-22T+00:00", None),
            ("2026-04-22T10:30:15+", None),
            ("2026-04-22T10:30:15+5", None),
            ("2026-04-22T1:30", None),
        ];
        for (input, want) in cases {
            assert_eq!(iso(input).as_deref(), *want, "{input:?}");
        }
    }

    /// Expected values from `datetime.fromisoformat(s.replace("Z", "+00:00")).astimezone().date()` under
    /// CPython 3.13 with `TZ=America/New_York` (Err where astimezone raised).
    #[test]
    fn astimezone_matches_cpython_new_york() {
        let zone = TzZone(chrono_tz::America::New_York);
        let cases: &[(&str, Result<&str, ()>)] = &[
            ("2026-04-22T03:00:00Z", Ok("2026-04-21")),
            ("2026-04-22T04:00:00Z", Ok("2026-04-22")),
            ("2026-04-22T03:59:59.999999+00:00", Ok("2026-04-21")),
            ("2026-01-15T04:59:59Z", Ok("2026-01-14")),
            ("2026-01-15T05:00:00Z", Ok("2026-01-15")),
            ("2026-04-22", Ok("2026-04-22")),
            ("2026-03-08T02:30:00", Ok("2026-03-08")),
            ("2026-11-01T01:30:00", Ok("2026-11-01")),
            ("2026-04-22T23:30:00-05:00", Ok("2026-04-23")),
            ("2026-04-22T00:30:00+05:30", Ok("2026-04-21")),
            ("0001-01-01T00:00:00+05:00", Err(())),
            ("0001-01-01T12:00:00+05:00", Ok("0001-01-01")),
            ("0001-01-01T00:00:00", Err(())),
            ("0001-01-01T12:00:00", Err(())),
            ("0001-01-02T00:00:00", Ok("0001-01-02")),
            ("0001-01-01T12:00:00Z", Ok("0001-01-01")),
            ("9999-12-31T23:59:59-05:00", Err(())),
            ("9999-12-31T23:59:59", Err(())),
            ("9999-12-31T12:00:00", Err(())),
            ("9999-12-31T23:59:59Z", Ok("9999-12-31")),
            ("9999-12-31T18:00:00", Err(())),
            ("9999-12-31T19:30:00", Err(())),
        ];
        for (input, want) in cases {
            let dt = fromisoformat(&input.replace('Z', "+00:00")).expect(input);
            let got = astimezone_date(&zone, &dt).map(format_day).map_err(|_| ());
            assert_eq!(got.as_deref().map_err(|_| ()), want.map(|s| s), "{input:?}");
        }
    }

    /// The same, under `TZ=Asia/Kolkata` (a positive offset moves the risk to the other end of the range).
    #[test]
    fn astimezone_matches_cpython_kolkata() {
        let zone = TzZone(chrono_tz::Asia::Kolkata);
        let cases: &[(&str, Result<&str, ()>)] = &[
            ("2026-04-22T18:29:59Z", Ok("2026-04-22")),
            ("2026-04-22T18:30:00Z", Ok("2026-04-23")),
            ("9999-12-31T23:59:59Z", Err(())),
            ("9999-12-31T12:00:00Z", Ok("9999-12-31")),
            ("9999-12-31T12:00:00", Err(())),
            ("9999-12-31T23:59:59", Err(())),
            ("0001-01-01T00:00:00", Err(())),
            ("0001-01-01T00:00:00Z", Ok("0001-01-01")),
            ("0001-01-01T00:00:00+05:00", Err(())),
            ("0001-01-02T00:00:00", Ok("0001-01-02")),
        ];
        for (input, want) in cases {
            let dt = fromisoformat(&input.replace('Z', "+00:00")).expect(input);
            let got = astimezone_date(&zone, &dt).map(format_day).map_err(|_| ());
            assert_eq!(got.as_deref().map_err(|_| ()), want.map(|s| s), "{input:?}");
        }
    }

    #[test]
    fn system_zone_reports_an_offset() {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
        let off = SystemZone.utc_offset(now).expect("localtime works for now");
        assert!(off.abs() < 86_400);
        assert!(local_day_of_timestamp(&SystemZone, now).is_ok());
    }
}
