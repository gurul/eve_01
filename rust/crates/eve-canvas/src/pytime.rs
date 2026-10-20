//! Python's datetime behavior that `canvas.py` relies on: `datetime.fromisoformat` (the C implementation
//! CPython runs, the same port as eve-google-rest's), `isoformat()`, and `astimezone(tz)` with the zone's name,
//! for the Mac's zone or a named one.

use chrono::{DateTime, Datelike, FixedOffset, Local, NaiveDate, NaiveTime, Offset, TimeZone, Timelike, Utc};

/// The zone times are shown in: the Mac's (Python's `astimezone(None)`), or a named IANA zone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Zone {
    #[default]
    Local,
    Named(chrono_tz::Tz),
}

/// Where the Mac's local zone comes from, read the way libc reads it (Python's `astimezone(None)` asks libc).
enum LocalZone {
    /// An IANA zone: `TZ` naming one, or the target of `/etc/localtime` when `TZ` is unset.
    Iana(chrono_tz::Tz),
    /// A POSIX `TZ` string ("XYZ-3", "<+05>-5"): chrono's `Local` reads its rules; these are its zone names.
    Posix { std: String, std_east: i32, dst: Option<String> },
    /// `TZ` set but empty or unreadable: libc falls back to UTC.
    Utc,
    /// No `TZ` and no readable `/etc/localtime` link.
    Unknown,
}

fn local_zone() -> LocalZone {
    if let Some(raw) = std::env::var_os("TZ") {
        let raw = raw.to_string_lossy().into_owned();
        let tz = raw.strip_prefix(':').unwrap_or(&raw);
        if tz.is_empty() {
            return LocalZone::Utc;
        }
        if let Ok(z) = tz.parse::<chrono_tz::Tz>() {
            return LocalZone::Iana(z);
        }
        return match posix_names(tz) {
            Some((std, std_east, dst)) => LocalZone::Posix { std, std_east, dst },
            None => LocalZone::Utc,
        };
    }
    let Some(link) = std::fs::canonicalize("/etc/localtime").ok() else {
        return LocalZone::Unknown;
    };
    let link = link.to_string_lossy();
    match link.split_once("zoneinfo/").and_then(|(_, name)| name.parse().ok()) {
        Some(z) => LocalZone::Iana(z),
        None => LocalZone::Unknown,
    }
}

/// A POSIX TZ string's standard name, its offset east of UTC, and its daylight name: `std offset [dst ...]`.
fn posix_names(tz: &str) -> Option<(String, i32, Option<String>)> {
    fn name(s: &str) -> Option<(String, &str)> {
        if let Some(rest) = s.strip_prefix('<') {
            let (n, rest) = rest.split_once('>')?;
            return (n.len() >= 3).then(|| (n.to_string(), rest));
        }
        let end = s.find(|c: char| !c.is_ascii_alphabetic()).unwrap_or(s.len());
        (end >= 3).then(|| (s[..end].to_string(), &s[end..]))
    }
    fn offset(s: &str) -> Option<(i32, &str)> {
        let (sign, body) = match s.as_bytes().first() {
            Some(b'-') => (-1, &s[1..]),
            Some(b'+') => (1, &s[1..]),
            _ => (1, s),
        };
        let end = body.find(|c: char| !(c.is_ascii_digit() || c == ':')).unwrap_or(body.len());
        let mut secs = 0;
        let mut scale = 3600;
        for (i, part) in body[..end].split(':').enumerate() {
            if i > 2 || part.is_empty() {
                return None;
            }
            secs += part.parse::<i32>().ok()? * scale;
            scale /= 60;
        }
        Some((sign * secs, &body[end..]))
    }
    let (std, rest) = name(tz)?;
    let (west, rest) = offset(rest)?;
    let dst = if rest.is_empty() || rest.starts_with(',') { None } else { name(rest).map(|(n, _)| n) };
    Some((std, -west, dst))
}

/// A fixed offset's name as Python's `timezone(offset).tzname()` gives it: "UTC", or "UTC-07:00".
fn offset_name(secs: i32) -> String {
    if secs == 0 {
        return "UTC".into();
    }
    let sign = if secs < 0 { '-' } else { '+' };
    let a = secs.unsigned_abs();
    let mut s = format!("UTC{sign}{:02}:{:02}", a / 3600, (a % 3600) / 60);
    if !a.is_multiple_of(60) {
        s.push_str(&format!(":{:02}", a % 60));
    }
    s
}

/// A wall-clock reading in a zone: the local time and the zone's abbreviation then ("PDT").
pub struct Wall {
    pub time: chrono::NaiveDateTime,
    pub tzname: String,
}

impl Zone {
    /// `dt.astimezone(tz)`: the same instant on this zone's clock, with the zone's name at that instant.
    pub fn wall(&self, dt: &DateTime<FixedOffset>) -> Wall {
        let zone = match self {
            Zone::Named(tz) => LocalZone::Iana(*tz),
            Zone::Local => local_zone(),
        };
        let named = |tz: chrono_tz::Tz| {
            let t = dt.with_timezone(&tz);
            Wall { time: t.naive_local(), tzname: t.format("%Z").to_string() }
        };
        match zone {
            LocalZone::Iana(tz) => named(tz),
            LocalZone::Utc => named(chrono_tz::UTC),
            LocalZone::Posix { std, std_east, dst } => {
                let t = dt.with_timezone(&Local);
                let east = t.offset().fix().local_minus_utc();
                let tzname = if east == std_east { std } else { dst.unwrap_or_else(|| offset_name(east)) };
                Wall { time: t.naive_local(), tzname }
            }
            LocalZone::Unknown => {
                let t = dt.with_timezone(&Local);
                Wall { time: t.naive_local(), tzname: offset_name(t.offset().fix().local_minus_utc()) }
            }
        }
    }
}

/// `datetime.isoformat()` of an aware time: microseconds only when not zero, the offset as ±HH:MM (and :SS
/// when it has seconds).
pub fn isoformat(dt: &DateTime<FixedOffset>) -> String {
    let n = dt.naive_local();
    let mut s = format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        n.year(),
        n.month(),
        n.day(),
        n.hour(),
        n.minute(),
        n.second()
    );
    let micro = n.nanosecond() / 1000;
    if micro != 0 {
        s.push_str(&format!(".{micro:06}"));
    }
    let off = dt.offset().local_minus_utc();
    let sign = if off < 0 { '-' } else { '+' };
    let a = off.unsigned_abs();
    s.push_str(&format!("{sign}{:02}:{:02}", a / 3600, (a % 3600) / 60));
    if !a.is_multiple_of(60) {
        s.push_str(&format!(":{:02}", a % 60));
    }
    s
}

/// `datetime.now(timezone.utc)`, to the microsecond as Python keeps it.
pub fn now_utc() -> DateTime<Utc> {
    let now = Utc::now();
    now.with_nanosecond(now.nanosecond() / 1000 * 1000).unwrap_or(now)
}

// ---- parsing, ported from CPython 3.13's Modules/_datetimemodule.c -------------------------------------


struct Bytes<'a>(&'a [u8]);

impl Bytes<'_> {
    /// The byte at `i`, or NUL past the end, as the C code reads its NUL-terminated buffer.
    fn at(&self, i: usize) -> u8 {
        self.0.get(i).copied().unwrap_or(0)
    }

    fn digits(&self, pos: usize, n: usize) -> Option<(i64, usize)> {
        let mut v = 0i64;
        for i in 0..n {
            let c = self.at(pos + i);
            if !c.is_ascii_digit() {
                return None;
            }
            v = v * 10 + (c - b'0') as i64;
        }
        Some((v, pos + n))
    }
}

fn find_separator(b: &Bytes, len: usize) -> Option<usize> {
    if len == 7 {
        return Some(7);
    }
    if b.at(4) == b'-' {
        if b.at(5) == b'W' {
            if len < 8 {
                return None;
            }
            if len > 8 && b.at(8) == b'-' {
                if len == 9 {
                    return None;
                }
                if len > 10 && b.at(10).is_ascii_digit() {
                    return Some(8);
                }
                return Some(10);
            }
            return Some(8);
        }
        return Some(10);
    }
    if b.at(4) == b'W' {
        let mut idx = 7;
        while idx < len && b.at(idx).is_ascii_digit() {
            idx += 1;
        }
        if idx < 9 {
            return Some(idx);
        }
        return Some(if idx % 2 == 0 { 7 } else { 8 });
    }
    Some(8)
}

/// (year, month, day) from an ISO week date, or None where CPython's `iso_to_ymd` fails.
fn iso_to_ymd(year: i64, week: i64, day: i64) -> Option<(i64, i64, i64)> {
    if !(1..=9999).contains(&year) || !(1..=7).contains(&day) {
        return None;
    }
    let d = NaiveDate::from_isoywd_opt(year as i32, week as u32, chrono::Weekday::try_from((day - 1) as u8).ok()?)?;
    Some((d.year() as i64, d.month() as i64, d.day() as i64))
}

fn parse_date(b: &Bytes, len: usize) -> Option<(i64, i64, i64)> {
    let (year, mut p) = b.digits(0, 4)?;
    let sep = b.at(p) == b'-';
    if sep {
        p += 1;
    }
    if b.at(p) == b'W' {
        p += 1;
        let (week, q) = b.digits(p, 2)?;
        p = q;
        let mut day = 1;
        if p < len {
            if sep {
                if b.at(p) != b'-' {
                    return None;
                }
                p += 1;
            }
            day = b.digits(p, 1)?.0;
        }
        return iso_to_ymd(year, week, day);
    }
    let (month, q) = b.digits(p, 2)?;
    p = q;
    if sep {
        if b.at(p) != b'-' {
            return None;
        }
        p += 1;
    }
    let (day, _) = b.digits(p, 2)?;
    Some((year, month, day))
}

/// `parse_hh_mm_ss_ff` over [start, end): Ok((h, m, s, us, more)) where `more` is the C code's "not the end
/// of the string" flag; Err for a malformed component.
fn parse_hms(b: &Bytes, start: usize, end: usize) -> Result<(i64, i64, i64, i64, bool), ()> {
    let mut vals = [0i64; 3];
    let mut p = start;
    let mut has_sep = true;
    for (i, slot) in vals.iter_mut().enumerate() {
        let (v, q) = b.digits(p, 2).ok_or(())?;
        *slot = v;
        p = q;
        let c = b.at(p);
        p += 1;
        if i == 0 {
            has_sep = c == b':';
        }
        if p >= end {
            return Ok((vals[0], vals[1], vals[2], 0, c != 0));
        } else if has_sep && c == b':' {
            if i == 2 {
                return Err(());
            }
            continue;
        } else if c == b'.' || c == b',' {
            if i < 2 {
                return Err(());
            }
            break;
        } else if !has_sep {
            p -= 1;
        } else {
            return Err(());
        }
    }
    let remains = end.saturating_sub(p);
    let to_parse = remains.min(6);
    let (mut us, mut q) = b.digits(p, to_parse).ok_or(())?;
    if (1..6).contains(&to_parse) {
        us *= [100_000, 10_000, 1_000, 100, 10][to_parse - 1];
    }
    while b.at(q).is_ascii_digit() {
        q += 1;
    }
    Ok((vals[0], vals[1], vals[2], us, b.at(q) != 0))
}

struct TimeParts {
    hms: (i64, i64, i64, i64),
    tz: Option<(i64, i64)>, // (offset seconds, offset microseconds)
}

fn parse_time_part(b: &Bytes, start: usize, end: usize) -> Result<TimeParts, ()> {
    let mut tz_pos = start;
    loop {
        let c = b.at(tz_pos);
        if c == b'Z' || c == b'+' || c == b'-' {
            break;
        }
        tz_pos += 1;
        if tz_pos >= end {
            break;
        }
    }
    let (h, m, s, us, more) = parse_hms(b, start, tz_pos)?;
    if tz_pos >= end {
        if more {
            return Err(());
        }
        return Ok(TimeParts { hms: (h, m, s, us), tz: None });
    }
    if b.at(tz_pos) == b'Z' {
        if b.at(tz_pos + 1) != 0 {
            return Err(());
        }
        return Ok(TimeParts { hms: (h, m, s, us), tz: Some((0, 0)) });
    }
    let sign = if b.at(tz_pos) == b'-' { -1 } else { 1 };
    let (th, tm, ts, tus, more) = parse_hms(b, tz_pos + 1, end)?;
    if more {
        return Err(());
    }
    Ok(TimeParts { hms: (h, m, s, us), tz: Some((sign * (th * 3600 + tm * 60 + ts), sign * tus)) })
}

fn fixed(secs: i32) -> FixedOffset {
    FixedOffset::east_opt(secs).unwrap_or_else(|| FixedOffset::east_opt(0).expect("UTC is a valid offset"))
}

fn checked_date(year: i64, month: i64, day: i64) -> Option<NaiveDate> {
    if !(1..=9999).contains(&year) || !(1..=12).contains(&month) || day < 1 {
        return None;
    }
    NaiveDate::from_ymd_opt(year as i32, month as u32, day as u32)
}

/// `datetime.fromisoformat(s)` as CPython parses it: every ISO 8601 shape it takes and any single character as
/// the separator. The time is naive (no offset) or aware (a fixed offset); None where Python raises
/// ValueError, and for an offset with a fraction of a second, which chrono cannot hold.
pub fn fromisoformat(s: &str) -> Option<(chrono::NaiveDateTime, Option<FixedOffset>)> {
    let b = Bytes(s.as_bytes());
    let len = s.len();
    if s.chars().count() < 7 {
        return None;
    }
    let sep = find_separator(&b, len)?;
    let (year, month, day) = parse_date(&b, sep)?;
    let mut parts = TimeParts { hms: (0, 0, 0, 0), tz: None };
    if len > sep {
        parts = parse_time_part(&b, sep + 1, len).ok()?;
    }
    let tz = match parts.tz {
        None => None,
        Some((secs, us)) => {
            if us != 0 || secs.abs() >= 86_400 {
                return None;
            }
            Some(fixed(secs as i32))
        }
    };
    let date = checked_date(year, month, day)?;
    let (h, m, sec, us) = parts.hms;
    if !(0..=23).contains(&h) || !(0..=59).contains(&m) || !(0..=59).contains(&sec) {
        return None;
    }
    let time = NaiveTime::from_hms_micro_opt(h as u32, m as u32, sec as u32, us as u32)?;
    Some((date.and_time(time), tz))
}

/// An aware time from a naive one read at `off`.
pub fn at_offset(naive: chrono::NaiveDateTime, off: FixedOffset) -> DateTime<FixedOffset> {
    off.from_local_datetime(&naive).single().expect("a fixed offset has one reading")
}

/// UTC as a fixed offset.
pub fn utc() -> FixedOffset {
    fixed(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fromisoformat_reads_canvas_times_and_the_other_iso_shapes() {
        let (n, off) = fromisoformat("2026-10-03T06:59:00+00:00").unwrap();
        assert_eq!(isoformat(&at_offset(n, off.unwrap())), "2026-10-03T06:59:00+00:00");
        let (n, off) = fromisoformat("2026-10-03 06:59:00.5-07:00").unwrap();
        assert_eq!(isoformat(&at_offset(n, off.unwrap())), "2026-10-03T06:59:00.500000-07:00");
        assert!(fromisoformat("2026-10-03").unwrap().1.is_none());
        assert!(fromisoformat("20261003T0659").is_some());
        assert!(fromisoformat("2026-13-03T00:00:00").is_none());
        assert!(fromisoformat("soon").is_none());
        assert!(fromisoformat("2026-10-03T06:59:00+00:00+00:00").is_none());
    }

    #[test]
    fn a_named_zone_gives_its_abbreviation_and_a_fixed_offset_its_python_name() {
        let dt = at_offset(fromisoformat("2026-10-03T06:59:00").unwrap().0, utc());
        let w = Zone::Named(chrono_tz::America::Los_Angeles).wall(&dt);
        assert_eq!(w.tzname, "PDT");
        assert_eq!(w.time.to_string(), "2026-10-02 23:59:00");
        assert_eq!(offset_name(-7 * 3600), "UTC-07:00");
        assert_eq!(offset_name(0), "UTC");
    }

    #[test]
    fn posix_tz_strings_give_their_names() {
        assert_eq!(posix_names("XYZ-3"), Some(("XYZ".into(), 3 * 3600, None)));
        assert_eq!(posix_names("AAA3BBB,M3.2.0,M11.1.0"), Some(("AAA".into(), -3 * 3600, Some("BBB".into()))));
        assert_eq!(posix_names("<+0530>-5:30"), Some(("+0530".into(), 5 * 3600 + 1800, None)));
        assert_eq!(posix_names("Bogus/Zone"), None);
        assert_eq!(posix_names("AB1"), None);
    }
}
