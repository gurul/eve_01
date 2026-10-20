//! Eve Link's home-screen widget feed: `GET /api/link/today` (docs/widget.md). A port of `link_today.py`.
//!
//! Owner, 2026-10-08: "make an eve widget for my phone that just tracks everything i have to do/have going, like a
//! command center type dashboard". One read-only call gathers what the daemon already knows, so the widget makes one
//! request per refresh:
//!
//! * `needs` and `going`: the chief's open cards (link_chat::list_jobs), the ones waiting on the owner first, then
//!   the ones running; and the watcher's watches (watch.Watcher) that are not paused, as more of `going`.
//! * `today`: the owner's calendar from now to the end of the local day, through the direct Google apps
//!   (`calendar__list_events`), only when the calendar is already signed in. The answer is kept for
//!   `CALENDAR_KEEP_SECS`, so the widget's refreshes do not each cost a Google call per calendar.
//! * `now`: what Spotify plays and whether the TV is on, from the Mini App server's own Music and TV remote cards.
//!
//! Every source has its own short limit and is gathered at the same time as the others. A source that is off, slow or
//! broken is left out and named in `sources` ("ok", "off" or "failed"); it never fails the whole answer. The link
//! token guards it like every other `/api/link` path (link_chat::LinkChat::serve). Logs carry counts, never titles.

use std::collections::{BTreeSet, HashSet};
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;

use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use futures::future::{BoxFuture, FutureExt, Shared};
use serde_json::{json, Map, Value};

use eve_transcripts::py as tpy;
use eve_transcripts::Dt;
use eve_web::watch::Watch;

use crate::seams::{Chief, DirectApps, Hub, Inlet, MiniApp, Obj};
use crate::{Mono, Now};

/// each quick source's own limit (the chief, the watches, Spotify, the TV)
pub const SOURCE_SECS: f64 = 4.0;
/// the calendar reads every calendar the owner sees, one request each
pub const CALENDAR_SECS: f64 = 8.0;
/// a calendar answer is reused this long
pub const CALENDAR_KEEP_SECS: f64 = 300.0;
/// cards waiting on the owner, listed (`needs_you` counts them all)
pub const NEEDS_SHOWN: usize = 6;
/// running cards and watches, listed
pub const GOING_SHOWN: usize = 10;
/// events left today, listed
pub const TODAY_SHOWN: usize = 8;
/// one line on a phone
pub const TITLE_CHARS: usize = 80;
pub const LINE_CHARS: usize = 60;

/// `str(value)` of a decoded JSON value, as Python prints it (`True`, `5`, `0.5`, the text itself).
fn py_str(value: &Value) -> String {
    match value {
        Value::Null => "None".to_string(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        Value::Number(n) => {
            if n.is_f64() {
                tpy::float_repr(n.as_f64().unwrap_or(0.0))
            } else {
                n.to_string()
            }
        }
        Value::String(s) => s.clone(),
        other => tpy::dumps(other),
    }
}

/// `str(text or "")` on one line, at most `limit` characters with "…".
pub fn clip(text: &Value, limit: usize) -> String {
    let s = if tpy::truthy(text) { tpy::flat(&py_str(text)) } else { String::new() };
    clip_str(&s, limit)
}

/// `clip` of a string already in hand.
fn clip_str(text: &str, limit: usize) -> String {
    let s = tpy::flat(text);
    if s.chars().count() <= limit {
        return s;
    }
    let head: String = s.chars().take(limit.saturating_sub(1)).collect();
    format!("{}…", tpy::rstrip(&head))
}

/// `datetime.timestamp()` of an aware time: whole microseconds since the epoch, divided once (as Python does).
fn timestamp(at: &Dt) -> f64 {
    let micros = at.timestamp() * 1_000_000 + i64::from(at.timestamp_subsec_micros());
    micros as f64 / 1e6
}

/// `datetime.isoformat()` of an aware time: `2026-10-08T14:00:00-07:00`, microseconds only when there are some.
fn isoformat(at: &Dt) -> String {
    let mut out = at.format("%Y-%m-%dT%H:%M:%S").to_string();
    let micros = at.timestamp_subsec_micros();
    if micros != 0 {
        out.push_str(&format!(".{micros:06}"));
    }
    let offset = at.offset().local_minus_utc();
    let sign = if offset < 0 { '-' } else { '+' };
    let abs = offset.unsigned_abs();
    out.push_str(&format!("{sign}{:02}:{:02}", abs / 3600, (abs % 3600) / 60));
    if !abs.is_multiple_of(60) {
        out.push_str(&format!(":{:02}", abs % 60));
    }
    out
}

/// `datetime.combine(now.date() + timedelta(days=1), datetime.min.time()).astimezone()`: the local midnight that
/// ends `now`'s day.
fn tomorrow_of(now: &Dt) -> Dt {
    let day = now.date_naive().succ_opt().unwrap_or(now.date_naive());
    tpy::local_aware(NaiveDateTime::new(day, NaiveTime::MIN))
}

// ---- the sources ------------------------------------------------------------------------------------------

/// The chief's open cards: `{"needs_you", "needs" (waiting on the owner), "going" (the rest)}`, as link_chat lists
/// them; each row `{"id", "kind": "job", "title", "line"}`.
pub fn jobs_part(chief: &dyn Chief) -> Obj {
    let listed = crate::link_chat::list_jobs(chief, &BTreeSet::new());
    let mut needs: Vec<Value> = Vec::new();
    let mut going: Vec<Value> = Vec::new();
    if let Some(Value::Array(jobs)) = listed.get("jobs") {
        for j in jobs {
            let status = j.get("status").and_then(Value::as_str);
            if matches!(status, Some("done") | Some("dropped")) {
                continue;
            }
            let id = j.get("id").filter(|v| tpy::truthy(v)).map(py_str).unwrap_or_default();
            let row = json!({
                "id": id,
                "kind": "job",
                "title": clip(j.get("title").unwrap_or(&Value::Null), TITLE_CHARS),
                "line": clip(j.get("line").unwrap_or(&Value::Null), LINE_CHARS),
            });
            if j.get("needs_you").is_some_and(tpy::truthy) {
                needs.push(row);
            } else {
                going.push(row);
            }
        }
    }
    let needs_you = listed.get("needs_you").and_then(Value::as_i64).unwrap_or(0);
    let mut out = Map::new();
    out.insert("needs_you".into(), json!(needs_you));
    out.insert("needs".into(), Value::Array(needs));
    out.insert("going".into(), Value::Array(going));
    out
}

/// The watches that are on, each as one row `{"id", "kind": "watch", "title", "line"}`: its label and the latest
/// reading.
pub fn watches_part(watches: &[Watch]) -> Vec<Obj> {
    let mut rows = Vec::new();
    for w in watches {
        if w.paused {
            continue;
        }
        let mut line = if w.checks != 0 || w.last_value.is_some() {
            eve_web::watch::rules::now_line(w)
        } else {
            "no reading yet".to_string()
        };
        if w.errors >= eve_web::watch::TELL_AFTER_ERRORS {
            line = "can't read it".to_string();
        }
        let mut row = Map::new();
        row.insert("id".into(), json!(w.id));
        row.insert("kind".into(), json!("watch"));
        row.insert("title".into(), json!(clip_str(&w.label, TITLE_CHARS)));
        row.insert("line".into(), json!(clip_str(&line, LINE_CHARS)));
        rows.push(row);
    }
    rows
}

/// `date.fromisoformat`: `YYYY-MM-DD` or `YYYYMMDD`.
fn date_fromisoformat(text: &str) -> Option<NaiveDate> {
    let ok = |s: &str| s.chars().all(|c| c.is_ascii_digit() || c == '-');
    if !ok(text) {
        return None;
    }
    match text.len() {
        10 => NaiveDate::parse_from_str(text, "%Y-%m-%d").ok(),
        8 if !text.contains('-') => NaiveDate::parse_from_str(text, "%Y%m%d").ok(),
        _ => None,
    }
}

/// An event's start or end (Google's `{"dateTime"}` or `{"date"}`) as an aware local time, and whether it is a whole
/// day. Python's `_moment`.
pub fn moment(when: Option<&Value>) -> Option<(Dt, bool)> {
    let Some(Value::Object(when)) = when else { return None };
    if let Some(raw) = when.get("dateTime").filter(|v| tpy::truthy(v)) {
        let text = py_str(raw).replace('Z', "+00:00");
        let (naive, offset) = tpy::fromisoformat(&text)?;
        let at = match offset {
            Some(off) => naive.and_local_timezone(off).single()?,
            None => tpy::local_aware(naive),
        };
        return Some((at, false));
    }
    if let Some(raw) = when.get("date").filter(|v| tpy::truthy(v)) {
        let day = date_fromisoformat(&py_str(raw))?;
        return Some((tpy::local_aware(NaiveDateTime::new(day, NaiveTime::MIN)), true));
    }
    None
}

/// The events in a `calendar__list_events` answer that have not ended and start before tomorrow, whole days first
/// then soonest, as `{"title", "start", "end", "all_day", "place"?}` (unix seconds). Declined and cancelled ones are
/// left out.
pub fn events_part(raw: &Value, now: Dt) -> Vec<Obj> {
    let tomorrow = tomorrow_of(&now);
    let mut out: Vec<Obj> = Vec::new();
    let mut seen: HashSet<(String, i64)> = HashSet::new();
    for ev in eve_meet::events_in(raw, 0) {
        if ev.get("status").and_then(Value::as_str) == Some("cancelled") {
            continue;
        }
        let declined = match ev.get("attendees") {
            Some(Value::Array(list)) => list.iter().any(|g| {
                g.as_object().is_some_and(|g| {
                    g.get("self").is_some_and(tpy::truthy)
                        && g.get("responseStatus").and_then(Value::as_str) == Some("declined")
                })
            }),
            _ => false,
        };
        if declined {
            continue;
        }
        let Some((begins, all_day)) = moment(ev.get("start")) else { continue };
        let ends = match moment(ev.get("end")) {
            Some((end, _)) => end,
            None => begins + if all_day { chrono::Duration::days(1) } else { chrono::Duration::hours(1) },
        };
        if ends <= now || begins >= tomorrow {
            continue;
        }
        let id = ev.get("id").map(|v| serde_json::to_string(v).unwrap_or_default()).unwrap_or_else(|| "null".into());
        let key = (id, begins.timestamp() * 1_000_000 + i64::from(begins.timestamp_subsec_micros()));
        if !seen.insert(key) {
            continue;
        }
        let summary = ev.get("summary").filter(|v| tpy::truthy(v)).cloned().unwrap_or_else(|| json!("Busy"));
        let mut row = Map::new();
        row.insert("title".into(), json!(clip(&summary, TITLE_CHARS)));
        row.insert("start".into(), json!(timestamp(&begins)));
        row.insert("end".into(), json!(timestamp(&ends)));
        row.insert("all_day".into(), json!(all_day));
        if let Some(place) = ev.get("location").filter(|v| tpy::truthy(v)) {
            row.insert("place".into(), json!(clip(place, LINE_CHARS)));
        }
        out.push(row);
    }
    let key = |r: &Obj| {
        let all_day = r.get("all_day").and_then(Value::as_bool).unwrap_or(false);
        let start = r.get("start").and_then(Value::as_f64).unwrap_or(0.0);
        (all_day, start)
    };
    out.sort_by(|a, b| {
        let (a_day, a_start) = key(a);
        let (b_day, b_start) = key(b);
        (!a_day).cmp(&!b_day).then(a_start.total_cmp(&b_start))
    });
    out
}

/// Spotify's status answer → one row while a track plays; none while paused or idle.
pub fn music_part(state: Option<&Value>) -> Vec<Obj> {
    let Some(Value::Object(state)) = state else { return Vec::new() };
    let ok = state.get("ok").map(tpy::truthy).unwrap_or(true);
    let playing = state.get("playing").is_some_and(tpy::truthy);
    let track = state.get("track").filter(|v| tpy::truthy(v));
    let Some(track) = track.filter(|_| ok && playing) else { return Vec::new() };
    let by = match state.get("artists") {
        Some(Value::Array(artists)) => artists.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", "),
        _ => String::new(),
    };
    let mut row = Map::new();
    row.insert("kind".into(), json!("music"));
    row.insert("title".into(), json!(clip(track, TITLE_CHARS)));
    row.insert("line".into(), json!(clip_str(&by, LINE_CHARS)));
    vec![row]
}

/// The TV remote's status answer → one row while the TV is on; none in standby or unreachable.
pub fn tv_part(state: Option<&Value>) -> Vec<Obj> {
    let Some(Value::Object(state)) = state else { return Vec::new() };
    let power = state.get("power").and_then(Value::as_str);
    if !state.get("ok").is_some_and(tpy::truthy) || !matches!(power, Some("on") | Some("screensaver")) {
        return Vec::new();
    }
    let what = match state.get("app").filter(|v| tpy::truthy(v)) {
        Some(app) => app.clone(),
        None => json!(if power == Some("screensaver") { "Screensaver" } else { "On" }),
    };
    let mut row = Map::new();
    row.insert("kind".into(), json!("tv"));
    row.insert("title".into(), json!("TV"));
    row.insert("line".into(), json!(clip(&what, LINE_CHARS)));
    vec![row]
}

// ---- the gathering ----------------------------------------------------------------------------------------

/// A calendar read in flight, shared by every request that waits on it (Python's shielded task): the answer, or
/// why it failed.
pub type CalendarRead = Shared<BoxFuture<'static, Result<Value, String>>>;

/// What `Today::new` takes; `Default` is production.
#[derive(Clone)]
pub struct TodayOptions {
    /// the local time (a fake in tests)
    pub now: Option<Now>,
    pub mono: Option<Mono>,
    pub source_secs: f64,
    pub calendar_secs: f64,
    pub calendar_keep_secs: f64,
}

impl Default for TodayOptions {
    fn default() -> Self {
        TodayOptions {
            now: None,
            mono: None,
            source_secs: SOURCE_SECS,
            calendar_secs: CALENDAR_SECS,
            calendar_keep_secs: CALENDAR_KEEP_SECS,
        }
    }
}

/// `GET /api/link/today`.
pub struct Today {
    now: Now,
    mono: Mono,
    pub source_secs: f64,
    pub calendar_secs: f64,
    pub calendar_keep_secs: f64,
    /// (when read, which day, the answer)
    calendar: Arc<parking_lot::Mutex<Option<(f64, NaiveDate, Value)>>>,
    /// one calendar read at a time; one that outlasts its request still finishes and is kept for the next refresh
    reading: parking_lot::Mutex<Option<(CalendarRead, tokio::task::AbortHandle)>>,
}

/// A source's limit as a Duration (a negative or broken limit is no time at all).
fn limit(secs: f64) -> Duration {
    if secs.is_finite() && secs > 0.0 {
        Duration::try_from_secs_f64(secs).unwrap_or(Duration::MAX)
    } else if secs.is_infinite() && secs > 0.0 {
        Duration::MAX
    } else {
        Duration::ZERO
    }
}

/// One source's outcome: (its state, its value).
type Got = (&'static str, Option<Value>);

impl Today {
    pub fn new(opts: TodayOptions) -> Self {
        Today {
            now: opts.now.unwrap_or_else(|| Arc::new(crate::local_now)),
            mono: opts.mono.unwrap_or_else(|| Arc::new(crate::monotonic)),
            source_secs: opts.source_secs,
            calendar_secs: opts.calendar_secs,
            calendar_keep_secs: opts.calendar_keep_secs,
            calendar: Arc::new(parking_lot::Mutex::new(None)),
            reading: parking_lot::Mutex::new(None),
        }
    }

    /// The calendar's answer for today: the kept one while fresh, else one read (shared with any request already
    /// waiting on it). Python's `_calendar_raw` and `_read_calendar`.
    pub async fn calendar_raw(&self, apps: Arc<dyn DirectApps>, now: Dt) -> Result<Value, String> {
        if let Some((at, day, raw)) = self.calendar.lock().clone() {
            if day == now.date_naive() && (self.mono)() - at < self.calendar_keep_secs {
                return Ok(raw);
            }
        }
        let read = {
            let mut reading = self.reading.lock();
            // Python's `self._reading is None or self._reading.done()`: the task's own state, not whether a request
            // happened to poll the shared answer to its end
            let fresh = match reading.as_ref() {
                None => true,
                Some((_, task)) => task.is_finished(),
            };
            if fresh {
                *reading = Some(self.read_calendar(apps, now));
            }
            reading.as_ref().map(|(r, _)| r.clone()).expect("a calendar read was just set")
        };
        // shielded: a read that outlasts this request still finishes (it runs as its own task) and is kept for the
        // next refresh
        read.await
    }

    /// One calendar read as its own task, kept when it succeeds.
    fn read_calendar(&self, apps: Arc<dyn DirectApps>, now: Dt) -> (CalendarRead, tokio::task::AbortHandle) {
        let kept = Arc::clone(&self.calendar);
        let mono = Arc::clone(&self.mono);
        let task = tokio::spawn(async move {
            let tomorrow = tomorrow_of(&now);
            let mut args = Map::new();
            args.insert("startTime".into(), json!(isoformat(&now)));
            args.insert("endTime".into(), json!(isoformat(&tomorrow)));
            args.insert("orderBy".into(), json!("startTime"));
            args.insert("pageSize".into(), json!(25));
            let raw = apps.execute("calendar__list_events", &args).await;
            if raw.get("ok") == Some(&Value::Bool(false)) {
                return Err("the calendar call failed".to_string());
            }
            let raw = Value::Object(raw);
            *kept.lock() = Some((mono(), now.date_naive(), raw.clone()));
            Ok(raw)
        });
        let handle = task.abort_handle();
        let read = async move {
            match task.await {
                Ok(got) => got,
                Err(e) => Err(if e.is_panic() { "the calendar read panicked" } else { "the calendar read was cancelled" }
                    .to_string()),
            }
        }
        .boxed()
        .shared();
        (read, handle)
    }

    /// Every source at once, each within its limit; what failed is left out and named in `sources`:
    /// `{"at", "needs_you", "needs", "going", "today", "now", "sources"}`.
    pub async fn gather(&self, inlet: Option<Arc<dyn Inlet>>, server: Option<Arc<dyn MiniApp>>) -> Obj {
        let now = (self.now)();
        let chief: Option<Arc<dyn Chief>> = inlet.as_ref().and_then(|i| i.chief());
        let apps: Option<Arc<dyn DirectApps>> = inlet.as_ref().and_then(|i| i.direct_apps());
        let calendar_on = apps.as_ref().is_some_and(|a| a.apps().iter().any(|n| n == "calendar"));
        let spotify: Option<Arc<dyn Hub>> = server.as_ref().and_then(|s| s.hub("spotify"));
        let tv: Option<Arc<dyn Hub>> = server.as_ref().and_then(|s| s.hub("firetv"));

        // the quick ones are read in place, as Python's were: a source that raises is "failed"
        let jobs: Got = match chief {
            None => ("off", None),
            Some(chief) => match std::panic::catch_unwind(AssertUnwindSafe(|| jobs_part(chief.as_ref()))) {
                Ok(part) => ("ok", Some(Value::Object(part))),
                Err(_) => {
                    tracing::warn!("link today: jobs failed (panic); left out");
                    ("failed", None)
                }
            },
        };
        let watches: Got = match inlet.as_ref() {
            None => ("off", None),
            Some(inlet) => match std::panic::catch_unwind(AssertUnwindSafe(|| inlet.watches())) {
                Ok(None) => ("off", None),
                Ok(Some(list)) => {
                    let rows = watches_part(&list).into_iter().map(Value::Object).collect();
                    ("ok", Some(Value::Array(rows)))
                }
                Err(_) => {
                    tracing::warn!("link today: watches failed (panic); left out");
                    ("failed", None)
                }
            },
        };

        let source_secs = self.source_secs;
        let calendar_secs = self.calendar_secs;
        let calendar = async {
            match apps.filter(|_| calendar_on) {
                None => ("off", None),
                Some(apps) => {
                    let read = AssertUnwindSafe(self.calendar_raw(apps, now)).catch_unwind();
                    match tokio::time::timeout(limit(calendar_secs), read).await {
                        Err(_) => {
                            tracing::info!("link today: calendar took longer than {:.0} s; left out", calendar_secs);
                            ("failed", None)
                        }
                        Ok(Ok(Ok(raw))) => ("ok", Some(raw)),
                        Ok(Ok(Err(_))) => {
                            tracing::warn!("link today: calendar failed (RuntimeError); left out");
                            ("failed", None)
                        }
                        Ok(Err(_)) => {
                            tracing::warn!("link today: calendar failed (panic); left out");
                            ("failed", None)
                        }
                    }
                }
            }
        };
        let music = status_of("music", spotify, source_secs);
        let tv_read = status_of("tv", tv, source_secs);
        let (calendar, music, tv_got): (Got, Got, Got) = tokio::join!(calendar, music, tv_read);

        let mut sources = Map::new();
        for (name, (state, _)) in [("jobs", &jobs), ("watches", &watches), ("calendar", &calendar), ("music", &music), ("tv", &tv_got)] {
            sources.insert(name.into(), json!(state));
        }
        let mut out = Map::new();
        out.insert("at".into(), json!(timestamp(&now)));
        let mut needs_you = json!(0);
        let mut needs: Vec<Value> = Vec::new();
        let mut going: Vec<Value> = Vec::new();
        if let (_, Some(Value::Object(part))) = &jobs {
            needs_you = part.get("needs_you").cloned().unwrap_or(json!(0));
            if let Some(Value::Array(list)) = part.get("needs") {
                needs = list.iter().take(NEEDS_SHOWN).cloned().collect();
            }
            if let Some(Value::Array(list)) = part.get("going") {
                going = list.clone();
            }
        }
        if let (_, Some(Value::Array(rows))) = &watches {
            going.extend(rows.iter().cloned());
        }
        going.truncate(GOING_SHOWN);
        let today: Vec<Value> = match &calendar {
            (_, Some(raw)) => events_part(raw, now).into_iter().take(TODAY_SHOWN).map(Value::Object).collect(),
            _ => Vec::new(),
        };
        let playing: Vec<Value> = music_part(music.1.as_ref())
            .into_iter()
            .chain(tv_part(tv_got.1.as_ref()))
            .map(Value::Object)
            .collect();
        let not_ok: Vec<String> = sources
            .iter()
            .filter(|(_, v)| v.as_str() != Some("ok"))
            .map(|(k, v)| format!("{k} {}", v.as_str().unwrap_or("")))
            .collect();
        tracing::info!(
            "link today: {} need you, {} going, {} today, {} now ({})",
            needs_you,
            going.len(),
            today.len(),
            playing.len(),
            if not_ok.is_empty() { "all ok".to_string() } else { not_ok.join(", ") }
        );
        out.insert("needs_you".into(), needs_you);
        out.insert("needs".into(), Value::Array(needs));
        out.insert("going".into(), Value::Array(going));
        out.insert("today".into(), Value::Array(today));
        out.insert("now".into(), Value::Array(playing));
        out.insert("sources".into(), Value::Object(sources));
        out
    }
}

/// A Music or TV card's `panel({"action": "status"})` within `secs`: ("ok", its answer), ("off") when the card is not
/// on this Mac, ("failed") when it errs or is slow.
async fn status_of(name: &'static str, hub: Option<Arc<dyn Hub>>, secs: f64) -> Got {
    let Some(hub) = hub else { return ("off", None) };
    let body = json!({"action": "status"});
    let read = AssertUnwindSafe(hub.panel(&body)).catch_unwind();
    match tokio::time::timeout(limit(secs), read).await {
        Err(_) => {
            tracing::info!("link today: {name} took longer than {secs:.0} s; left out");
            ("failed", None)
        }
        Ok(Ok(Ok(state))) => ("ok", Some(state)),
        Ok(Ok(Err(_))) => {
            tracing::warn!("link today: {name} failed (error); left out");
            ("failed", None)
        }
        Ok(Err(_)) => {
            tracing::warn!("link today: {name} failed (panic); left out");
            ("failed", None)
        }
    }
}
