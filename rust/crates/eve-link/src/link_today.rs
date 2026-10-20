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

use std::sync::Arc;

use chrono::NaiveDate;
use futures::future::{BoxFuture, Shared};
use serde_json::Value;

use eve_transcripts::Dt;
use eve_web::watch::Watch;

use crate::seams::{Chief, DirectApps, Inlet, MiniApp, Obj};
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

/// `str(text or "")` on one line, at most `limit` characters with "…".
pub fn clip(text: &Value, limit: usize) -> String {
    let _ = (text, limit);
    todo!()
}

// ---- the sources ------------------------------------------------------------------------------------------

/// The chief's open cards: `{"needs_you", "needs" (waiting on the owner), "going" (the rest)}`, as link_chat lists
/// them; each row `{"id", "kind": "job", "title", "line"}`.
pub fn jobs_part(chief: &dyn Chief) -> Obj {
    let _ = chief;
    todo!()
}

/// The watches that are on, each as one row `{"id", "kind": "watch", "title", "line"}`: its label and the latest
/// reading.
pub fn watches_part(watches: &[Watch]) -> Vec<Obj> {
    let _ = watches;
    todo!()
}

/// An event's start or end (Google's `{"dateTime"}` or `{"date"}`) as an aware local time, and whether it is a whole
/// day. Python's `_moment`.
pub fn moment(when: Option<&Value>) -> Option<(Dt, bool)> {
    let _ = when;
    todo!()
}

/// The events in a `calendar__list_events` answer that have not ended and start before tomorrow, whole days first
/// then soonest, as `{"title", "start", "end", "all_day", "place"?}` (unix seconds). Declined and cancelled ones are
/// left out.
pub fn events_part(raw: &Value, now: Dt) -> Vec<Obj> {
    let _ = (raw, now);
    todo!()
}

/// Spotify's status answer → one row while a track plays; none while paused or idle.
pub fn music_part(state: Option<&Value>) -> Vec<Obj> {
    let _ = state;
    todo!()
}

/// The TV remote's status answer → one row while the TV is on; none in standby or unreachable.
pub fn tv_part(state: Option<&Value>) -> Vec<Obj> {
    let _ = state;
    todo!()
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
    reading: parking_lot::Mutex<Option<CalendarRead>>,
}

impl Today {
    pub fn new(opts: TodayOptions) -> Self {
        let _ = opts;
        todo!()
    }

    /// The calendar's answer for today: the kept one while fresh, else one read (shared with any request already
    /// waiting on it). Python's `_calendar_raw` and `_read_calendar`.
    pub async fn calendar_raw(&self, apps: Arc<dyn DirectApps>, now: Dt) -> Result<Value, String> {
        let _ = (apps, now, &self.now, &self.mono, &self.calendar, &self.reading);
        todo!()
    }

    /// Every source at once, each within its limit; what failed is left out and named in `sources`:
    /// `{"at", "needs_you", "needs", "going", "today", "now", "sources"}`.
    pub async fn gather(&self, inlet: Option<Arc<dyn Inlet>>, server: Option<Arc<dyn MiniApp>>) -> Obj {
        let _ = (inlet, server);
        todo!()
    }
}
