//! Python's datetime behavior that `google_rest.py` relies on: `datetime.fromisoformat` and
//! `date.fromisoformat` (the C implementation CPython runs), `isoformat()`, and `astimezone()` into the Mac's
//! zone, which resolves a wall-clock time in a gap or a fold the way CPython's C code does.

use chrono::{
    DateTime, Datelike, Duration, FixedOffset, Local, NaiveDate, NaiveDateTime, NaiveTime, Offset, TimeZone,
    Timelike, Utc,
};

use crate::py::repr_str;
use crate::Error;

/// The zone a bare local time is read in: the Mac's (`Local`, what Python's `astimezone()` uses), or a named
/// IANA zone, which tests use in place of setting `TZ` for the whole process.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Zone {
    #[default]
    Local,
    Named(chrono_tz::Tz),
}

impl Zone {
    /// The UTC offset in seconds at a UTC instant (`time.localtime(u).tm_gmtoff`).
    pub fn offset_at(&self, utc_secs: i64) -> i32 {
        match self {
            Zone::Local => Local
                .timestamp_opt(utc_secs, 0)
                .single()
                .map(|d| d.offset().local_minus_utc())
                .unwrap_or(0),
            Zone::Named(tz) => tz
                .timestamp_opt(utc_secs, 0)
                .single()
                .map(|d| d.offset().fix().local_minus_utc())
                .unwrap_or(0),
        }
    }

    /// CPython's `local_to_seconds`: the UTC second whose local wall clock is `t` (seconds of a naive time
    /// counted as if it were UTC), with `fold` choosing between the two answers in a fold or a gap.
    fn mktime(&self, t: i64, fold: bool) -> i64 {
        const MAX_FOLD: i64 = 24 * 3600;
        let local = |u: i64| u + self.offset_at(u) as i64;
        let a = local(t) - t;
        let u1 = t - a;
        let t1 = local(u1);
        let b = if t1 == t {
            let u2 = u1 + if fold { MAX_FOLD } else { -MAX_FOLD };
            let b = local(u2) - u2;
            if a == b {
                return u1;
            }
            b
        } else {
            t1 - u1
        };
        let u2 = t - b;
        let t2 = local(u2);
        if t2 == t {
            return u2;
        }
        if t1 == t {
            return u1;
        }
        if fold {
            u1.min(u2)
        } else {
            u1.max(u2)
        }
    }

    /// `naive.astimezone()`: the naive time read in this zone (fold 0), shown with the zone's offset at that
    /// instant. In a fold it is the first occurrence; in a gap, CPython reads it with the offset before the
    /// change, so 02:30 on the spring-forward night comes back as 03:30 in daylight time.
    pub fn localize(&self, naive: NaiveDateTime) -> DateTime<FixedOffset> {
        let t = naive.and_utc().timestamp();
        let mut ts = self.mktime(t, false);
        let ts2 = self.mktime(t, true);
        if ts2 != ts && !(ts2 > ts) {
            ts = ts2;
        }
        let utc = naive - Duration::seconds(self.offset_at(ts) as i64);
        let off = self.offset_at(utc.and_utc().timestamp());
        fixed(off).from_utc_datetime(&utc)
    }

    /// `aware.astimezone()`: the same instant in this zone.
    pub fn convert(&self, dt: &DateTime<FixedOffset>) -> DateTime<FixedOffset> {
        dt.with_timezone(&fixed(self.offset_at(dt.timestamp())))
    }

    /// The zone's IANA name: the Mac's from `/etc/localtime` (UTC when unknown), or the named zone's.
    pub fn iana_name(&self) -> String {
        match self {
            Zone::Local => crate::local_zone(),
            Zone::Named(tz) => tz.name().to_string(),
        }
    }
}

pub(crate) fn fixed(secs: i32) -> FixedOffset {
    FixedOffset::east_opt(secs).unwrap_or_else(|| FixedOffset::east_opt(0).expect("UTC is a valid offset"))
}

/// A Python datetime: naive, or aware with a fixed offset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PyDateTime {
    Naive(NaiveDateTime),
    Aware(DateTime<FixedOffset>),
}

impl PyDateTime {
    /// `dt.astimezone()`.
    pub fn astimezone(&self, zone: Zone) -> DateTime<FixedOffset> {
        match self {
            PyDateTime::Naive(n) => zone.localize(*n),
            PyDateTime::Aware(a) => zone.convert(a),
        }
    }

    /// `dt.astimezone(tz)` for a fixed-offset `tz`; a naive time is first read in `zone`.
    pub fn astimezone_to(&self, zone: Zone, tz: FixedOffset) -> DateTime<FixedOffset> {
        match self {
            PyDateTime::Naive(n) => zone.localize(*n).with_timezone(&tz),
            PyDateTime::Aware(a) => a.with_timezone(&tz),
        }
    }

    /// `dt if dt.tzinfo else dt.astimezone()`.
    pub fn aware_or_local(&self, zone: Zone) -> DateTime<FixedOffset> {
        match self {
            PyDateTime::Naive(n) => zone.localize(*n),
            PyDateTime::Aware(a) => *a,
        }
    }

    pub fn isoformat(&self) -> String {
        match self {
            PyDateTime::Naive(n) => naive_isoformat(n),
            PyDateTime::Aware(a) => isoformat(a),
        }
    }
}

fn naive_isoformat(n: &NaiveDateTime) -> String {
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
    s
}

/// `datetime.isoformat()` of an aware time: microseconds only when not zero, the offset as ±HH:MM (and :SS
/// when it has seconds).
pub fn isoformat(dt: &DateTime<FixedOffset>) -> String {
    let mut s = naive_isoformat(&dt.naive_local());
    let off = dt.offset().local_minus_utc();
    let sign = if off < 0 { '-' } else { '+' };
    let a = off.unsigned_abs();
    s.push_str(&format!("{sign}{:02}:{:02}", a / 3600, (a % 3600) / 60));
    if a % 60 != 0 {
        s.push_str(&format!(":{:02}", a % 60));
    }
    s
}

/// `datetime.now(timezone.utc)`, to the microsecond as Python keeps it.
pub fn now_utc() -> DateTime<FixedOffset> {
    let now = Utc::now();
    let now = now.with_nanosecond(now.nanosecond() / 1000 * 1000).unwrap_or(now);
    now.with_timezone(&fixed(0))
}

/// The latest time Python can hold: `datetime.max` in UTC, which `_start_key` sorts unreadable events to.
pub fn datetime_max_utc() -> DateTime<FixedOffset> {
    let n = NaiveDate::from_ymd_opt(9999, 12, 31)
        .and_then(|d| d.and_hms_micro_opt(23, 59, 59, 999_999))
        .expect("9999-12-31 is a date");
    fixed(0).from_utc_datetime(&n)
}

/// `dt + timedelta(...)`, an OverflowError past Python's year 9999 or before year 1.
pub fn add(dt: &DateTime<FixedOffset>, delta: Duration) -> Result<DateTime<FixedOffset>, Error> {
    let out = dt.checked_add_signed(delta).ok_or_else(|| Error::Overflow("date value out of range".into()))?;
    if !(1..=9999).contains(&out.naive_local().year()) {
        return Err(Error::Overflow("date value out of range".into()));
    }
    Ok(out)
}

/// `datetime.combine(d, time(hour))`, with `time()`'s range check.
pub fn combine(d: NaiveDate, hour: i64) -> Result<NaiveDateTime, Error> {
    if !(0..=23).contains(&hour) {
        return Err(Error::value("hour must be in 0..23"));
    }
    Ok(d.and_time(NaiveTime::from_hms_opt(hour as u32, 0, 0).expect("hour in range")))
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

fn parse_time(b: &Bytes, start: usize, end: usize) -> Result<TimeParts, ()> {
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

fn invalid(s: &str) -> Error {
    Error::value(format!("Invalid isoformat string: {}", repr_str(s)))
}

fn timedelta_repr(secs: i64) -> String {
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    match (days, rem) {
        (0, 0) => "datetime.timedelta(0)".into(),
        (0, r) => format!("datetime.timedelta(seconds={r})"),
        (d, 0) => format!("datetime.timedelta(days={d})"),
        (d, r) => format!("datetime.timedelta(days={d}, seconds={r})"),
    }
}

fn checked_date(year: i64, month: i64, day: i64) -> Result<NaiveDate, Error> {
    if !(1..=9999).contains(&year) {
        return Err(Error::value(format!("year {year} is out of range")));
    }
    if !(1..=12).contains(&month) {
        return Err(Error::value("month must be in 1..12"));
    }
    NaiveDate::from_ymd_opt(year as i32, month as u32, day as u32)
        .filter(|_| day >= 1)
        .ok_or_else(|| Error::value("day is out of range for month"))
}

/// `datetime.fromisoformat(s)` as CPython 3.13 parses it (the C implementation): every ISO 8601 shape it
/// takes, any single character as the separator, and the same error messages.
pub fn fromisoformat(s: &str) -> Result<PyDateTime, Error> {
    let b = Bytes(s.as_bytes());
    let len = s.len();
    if s.chars().count() < 7 {
        return Err(invalid(s));
    }
    let sep = find_separator(&b, len).ok_or_else(|| invalid(s))?;
    let (year, month, day) = parse_date(&b, sep).ok_or_else(|| invalid(s))?;
    let mut parts = TimeParts { hms: (0, 0, 0, 0), tz: None };
    if len > sep {
        parts = parse_time(&b, sep + 1, len).map_err(|_| invalid(s))?;
    }
    let tz = match parts.tz {
        None => None,
        Some((secs, us)) => {
            if us != 0 {
                // Python keeps a sub-second offset; chrono's offsets are whole seconds.
                return Err(Error::value(format!(
                    "Invalid isoformat string: {} (a UTC offset with a fraction of a second)",
                    repr_str(s)
                )));
            }
            if secs.abs() >= 86_400 {
                return Err(Error::value(format!(
                    "offset must be a timedelta strictly between -timedelta(hours=24) and timedelta(hours=24), not {}.",
                    timedelta_repr(secs)
                )));
            }
            Some(fixed(secs as i32))
        }
    };
    let date = checked_date(year, month, day)?;
    let (h, m, sec, us) = parts.hms;
    if !(0..=23).contains(&h) {
        return Err(Error::value("hour must be in 0..23"));
    }
    if !(0..=59).contains(&m) {
        return Err(Error::value("minute must be in 0..59"));
    }
    if !(0..=59).contains(&sec) {
        return Err(Error::value("second must be in 0..59"));
    }
    let naive = date.and_time(
        NaiveTime::from_hms_micro_opt(h as u32, m as u32, sec as u32, us as u32).expect("components in range"),
    );
    Ok(match tz {
        None => PyDateTime::Naive(naive),
        Some(off) => PyDateTime::Aware(
            off.from_local_datetime(&naive).single().expect("a fixed offset has one reading"),
        ),
    })
}

/// `date.fromisoformat(s)`, CPython 3.13's C implementation.
pub fn date_fromisoformat(s: &str) -> Result<NaiveDate, Error> {
    let b = Bytes(s.as_bytes());
    let len = s.len();
    if !matches!(len, 7 | 8 | 10) {
        return Err(invalid(s));
    }
    let (y, m, d) = parse_date(&b, len).ok_or_else(|| invalid(s))?;
    checked_date(y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn la() -> Zone {
        Zone::Named(chrono_tz::America::Los_Angeles)
    }

    fn naive(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S").unwrap()
    }

    #[test]
    fn a_gap_and_a_fold_resolve_as_cpython_does() {
        assert_eq!(isoformat(&la().localize(naive("2026-03-08T02:30:00"))), "2026-03-08T03:30:00-07:00");
        assert_eq!(isoformat(&la().localize(naive("2026-11-01T01:30:00"))), "2026-11-01T01:30:00-07:00");
        assert_eq!(isoformat(&la().localize(naive("2026-01-15T08:00:00"))), "2026-01-15T08:00:00-08:00");
    }

    #[test]
    fn isoformat_shows_microseconds_and_offset_seconds_only_when_set() {
        let dt = fixed(-7 * 3600).with_ymd_and_hms(2026, 10, 5, 9, 0, 0).unwrap();
        assert_eq!(isoformat(&dt), "2026-10-05T09:00:00-07:00");
        let dt = fixed(5 * 3600 + 30 * 60 + 15).with_ymd_and_hms(2026, 10, 5, 9, 0, 0).unwrap();
        assert_eq!(isoformat(&dt), "2026-10-05T09:00:00+05:30:15");
        assert_eq!(timedelta_repr(86_400), "datetime.timedelta(days=1)");
        assert_eq!(timedelta_repr(-60), "datetime.timedelta(days=-1, seconds=86340)");
    }
}
