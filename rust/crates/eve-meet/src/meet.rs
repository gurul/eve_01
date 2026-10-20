//! Meet: eve joins a Google Meet call, listens, and texts the owner the notes.
//!
//! The owner says `/meet <link>` on Telegram, "join my 3pm" in the chat, or "join my meeting" out loud. Eve
//! opens a tab of its own in the owner's Chrome (attach mode, so the owner's Google account and its meetings),
//! turns the microphone and the camera off, presses Join, reads Meet's live captions with the speakers' names,
//! and when the call ends — or the owner says leave — writes the notes under `transcripts/meetings/<date>/`
//! and texts a summary.
//!
//! ### Listen only, for good
//!
//! The owner chose a notetaker that never speaks (2026-09-25). So there is no audio path out at all, and the page
//! script (`meet_page.js`) can click only a short list of safe controls: microphone off, camera off, continue
//! without a microphone, Join now / Ask to join / Join here too, captions on, and Leave. It presses Join only once
//! it has SEEN both the microphone and the camera off, and in the call it turns either off again if it comes on.
//! The tab is kept quiet too: the call is not played out loud on the Mac, where the robot's own microphone is.
//!
//! ### The owner's account, twice
//!
//! The owner may be in the same call from another computer with the same Google account. Meet then offers
//! "Join here too" and "Switch here". Eve presses only Join here too: Switch here moves the call to eve's tab
//! and drops the owner's own device.
//!
//! ### Captions, not audio
//!
//! Meet's captions carry who said what, cost nothing, and need no audio device. The page only records what the
//! caption region shows (a snapshot per change); [`CaptionMerger`] here turns those snapshots into lines. That is
//! the hard part and it is plain code, so it is tested without a browser: Meet shows a sliding window of a few
//! lines per speaker, revises the last words as it hears more, and re-renders the block now and then.
//!
//! The transcript is appended to the notes file as lines settle, so a crash mid-call keeps what was heard.
//!
//! The page script is a port of OpenClaw's google-meet plugin (MIT, Copyright (c) 2026 OpenClaw Foundation).

use std::collections::{HashMap, HashSet};
use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::{Duration as StdDuration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use chrono::{DateTime, Duration, FixedOffset, Local, TimeZone};
use eve_browser_lane::{BrowserLane, BrowserLaneConfig, LaneOptions};
use eve_desktop_lanes::LaneError;
use eve_notes::{NotesError, NotesSdk, OpenRouterSdk};
use eve_vault::pystr;
use futures::future::BoxFuture;
use indexmap::IndexMap;
use parking_lot::Mutex;
use regex::Regex;
use serde_json::{json, Map, Value};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::py;

/// on wherever Telegram and the owner's Chrome (attach mode) are
pub const MEET_DEFAULT: bool = true;
pub const MEET_HOST: &str = "meet.google.com";
/// `CODE`: a meeting code, `abc-defg-hij`.
pub static CODE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-z]{3}-[a-z]{4}-[a-z]{3}$").expect("CODE"));
/// The script eve runs inside its Meet tab, the same bytes the Python daemon evaluates.
pub const PAGE_SCRIPT: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../bridge/src/eve_bridge/meet_page.js"));
/// only asked for when Chrome is signed out; the owner's account has a name
pub const DEFAULT_GUEST_NAME: &str = "Eve";
/// the room notes' summary model (notes.py)
pub const DEFAULT_SUMMARY_MODEL: &str = "gpt-6-luna";
/// from the page opening to being in the call or in the lobby
pub const JOIN_SECS: f64 = 90.0;
/// no microphone/camera toggle shown this long: no device, join without
pub const NO_CONTROLS_SECS: f64 = 12.0;
pub const DEFAULT_LOBBY_MINUTES: f64 = 15.0;
pub const DEFAULT_MAX_HOURS: f64 = 4.0;
pub const JOIN_POLL_SECS: f64 = 1.0;
pub const LISTEN_POLL_SECS: f64 = 1.5;
/// "not in the call" this many polls running: it is over, not a redraw
pub const OUT_OF_CALL_POLLS: u32 = 3;
/// the page failing this many polls running: the tab or Chrome is gone
pub const PAGE_ERRORS: u32 = 5;
/// in the call this long with no captions: tell the owner, once
pub const NO_CAPTIONS_SECS: f64 = 60.0;
/// a caption block gone this long is finished (OpenClaw's settle window)
pub const SETTLE_SECS: f64 = 1.0;
/// a block unchanged this long is written down (the speaker paused)
pub const IDLE_COMMIT_SECS: f64 = 4.0;
/// a monologue is written down in paragraphs of about this many words
pub const PARAGRAPH_WORDS: usize = 60;
/// Meet revises at most this many of the last words it showed
pub const REVISED_TAIL_WORDS: usize = 3;
pub const CALENDAR_LOOKBACK: Duration = Duration::minutes(10);
pub const CALENDAR_AHEAD: Duration = Duration::minutes(60);
/// "my 3pm" matches an event starting within this of 3pm
pub const TIME_SLACK: Duration = Duration::minutes(20);
/// sent to the summary model, at most (the file keeps everything)
pub const MAX_TRANSCRIPT_CHARS: usize = 120_000;
pub const OFF_REASON: &str =
    "joining meetings is off on this computer (EVE_MEET, and the owner's Chrome: EVE_BROWSER_ATTACH)";

/// `TOOLS`: the three tools the chat and the voice get.
pub static TOOLS: LazyLock<Value> = LazyLock::new(|| {
    json!([
        {"type": "function", "name": "meet_join", "strict": true,
         "description": "Join a Google Meet call as a silent notetaker: microphone and camera off, captions read, \
notes texted to the owner when it ends. Returns at once; the joining carries on.",
         "parameters": {"type": "object", "additionalProperties": false, "required": ["meeting"],
                        "properties": {"meeting": {
                            "type": "string",
                            "description": "A meet.google.com link or meeting code when you have one; otherwise the \
owner's words for which meeting on their calendar: \"now\", \"next\", a time like \"3pm\", or words from its title."}}}},
        {"type": "function", "name": "meet_status", "strict": true,
         "description": "Whether eve is in a meeting now: which, since when, how many lines it has heard.",
         "parameters": {"type": "object", "additionalProperties": false, "required": [], "properties": {}}},
        {"type": "function", "name": "meet_leave", "strict": true,
         "description": "Leave the meeting eve is in; the notes are written and texted.",
         "parameters": {"type": "object", "additionalProperties": false, "required": [], "properties": {}}},
    ])
});
pub const TOOL_NAMES: [&str; 3] = ["meet_join", "meet_status", "meet_leave"];

pub const INSTRUCTIONS_BLOCK: &str = "You can sit in on the owner's Google Meet calls as a silent notetaker with meet_join:
eve joins from the owner's Chrome with the microphone and camera off, never speaks, reads the captions, and
texts the notes when the call ends. Pass the meet.google.com link when you have it. Otherwise pass the owner's
own words for which meeting (\"now\", \"next\", \"3pm\", \"the standup\"): eve finds its link on their calendar
itself. meet_status says whether eve is in a call; meet_leave leaves and sends the notes. Joining can take a
minute and may wait in the lobby until someone lets eve in: tell the owner it is on its way, not that it is
in.";

// ---- links ----------------------------------------------------------------------------------------

const NO_LINK: &str = "no Google Meet link or meeting code in that";

static SCHEME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-z][a-z0-9+.-]*://").expect("SCHEME"));
static LOOKUP: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_-]{3,64}$").expect("LOOKUP"));
static AUTHUSER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[\w.@+-]{1,80}$").expect("AUTHUSER"));

/// The Meet link in `text` (a link, a link without https, or a bare meeting code), normalised to
/// `https://meet.google.com/<code>?hl=en` with `authuser` kept; or (None, why not).
///
/// English is forced because the page script reads English labels (OpenClaw does the same). Only
/// meet.google.com itself, over https, with a meeting code or a lookup path: a lookalike host, another scheme
/// or another site is refused, so a message cannot send eve's tab anywhere else.
pub fn parse_link(text: &str) -> (Option<String>, String) {
    for raw in pystr::split(text) {
        let token = raw.trim_matches(|c: char| "<>()[]{}\"'.,;!".contains(c));
        if token.is_empty() {
            continue;
        }
        let low = token.to_lowercase();
        if CODE.is_match(&low) {
            return (Some(format!("https://{MEET_HOST}/{low}?hl=en")), String::new());
        }
        let token = if low.starts_with(&format!("{MEET_HOST}/")) {
            format!("https://{token}")
        } else if !SCHEME.is_match(&low) {
            continue;
        } else {
            token.to_string()
        };
        let Ok(u) = py::urlsplit(&token) else { continue };
        if u.hostname().unwrap_or_default().to_lowercase() != MEET_HOST {
            continue;
        }
        if u.scheme.to_lowercase() != "https" {
            return (None, "only an https Meet link is joined".into());
        }
        // a port Python cannot read raises out of `u.port`; it is not a plain Meet link either way
        let plain_port = matches!(u.port(), Ok(None) | Ok(Some(443)));
        if u.username().is_some_and(|n| !n.is_empty()) || u.password().is_some_and(|p| !p.is_empty()) || !plain_port {
            return (None, "that is not a plain Meet link".into());
        }
        let path = u.path.trim_end_matches('/');
        let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
        let path = if parts.len() == 1 && CODE.is_match(&parts[0].to_lowercase()) {
            format!("/{}", parts[0].to_lowercase())
        } else if parts.len() == 2 && parts[0].to_lowercase() == "lookup" && LOOKUP.is_match(parts[1]) {
            format!("/lookup/{}", parts[1])
        } else {
            return (None, "that Meet link has no meeting code in it".into());
        };
        let mut query: Vec<(String, String)> =
            py::parse_qsl(&u.query).into_iter().filter(|(k, v)| k == "authuser" && AUTHUSER.is_match(v)).collect();
        query.push(("hl".into(), "en".into()));
        return (Some(format!("https://{MEET_HOST}{path}?{}", py::urlencode(&query))), String::new());
    }
    (None, NO_LINK.into())
}

/// The meeting code a link names (its last path part), or "the meeting".
pub fn meeting_code(url: &str) -> String {
    let path = py::urlsplit(url).map(|u| u.path).unwrap_or_default();
    let path = path.trim_matches('/');
    if path.is_empty() {
        return "the meeting".into();
    }
    path.rsplit('/').next().unwrap_or(path).to_string()
}

// ---- captions ------------------------------------------------------------------------------------

static NON_WORD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[^\w]+").expect("NON_WORD"));

/// `_key`: a word for comparing, lowercased with its punctuation gone.
fn key(word: &str) -> String {
    NON_WORD.replace_all(&word.to_lowercase(), "").into_owned()
}

/// `new` as a continuation of `previous`, or None when it is a new utterance.
///
/// Meet shows a window of a speaker's turn that grows at the end, drops words at the front as it scrolls, and
/// rewrites words anywhere in it as it hears more (live, 2026-09-25: "He will most likely" became "We will
/// most likely", and "it's quite" became "it's. It's quite"). So the two are aligned word by word
/// (difflib): when enough of them agree, `new` is the same turn, and the result is whatever `previous`
/// had before the point where `new` begins, then `new` itself — Meet's latest reading wins. When too
/// little agrees, it is a new utterance.
pub fn merge(previous: &str, new: &str) -> Option<String> {
    let (pt, nt) = (pystr::split(previous), pystr::split(new));
    if nt.is_empty() {
        return Some(previous.to_string());
    }
    if pt.is_empty() {
        return Some(new.to_string());
    }
    let pk: Vec<String> = pt.iter().map(|w| key(w)).collect();
    let nk: Vec<String> = nt.iter().map(|w| key(w)).collect();
    let blocks: Vec<py::Block> = py::matching_blocks(&pk, &nk).into_iter().filter(|b| b.size > 0).collect();
    let shorter = pk.len().min(nk.len());
    let agreed: usize = blocks.iter().map(|b| b.size).sum();
    // one or two short words in common is chance, not the same turn
    if blocks.is_empty() || agreed < shorter.min(2) || agreed * 2 < shorter {
        return None;
    }
    let first = blocks[0];
    if agreed == nk.len() && blocks.len() == 1 {
        return Some(previous.to_string()); // nothing new: the window only scrolled or shrank
    }
    // `new` begins `first.b` words before its first agreement: that many of previous's words are replaced
    let keep = first.a.saturating_sub(first.b);
    Some(pt[..keep].iter().chain(nt.iter()).copied().collect::<Vec<_>>().join(" "))
}

/// One settled line of the transcript.
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub speaker: String,
    pub text: String,
    /// epoch seconds, when the words were first shown
    pub at: f64,
}

/// `_Open`: a caption block still on screen.
#[derive(Debug, Clone, PartialEq)]
pub struct OpenBlock {
    pub speaker: String,
    pub text: String,
    pub first: f64,
    pub last: f64,
    /// words of `text` already written down
    pub emitted: usize,
}

impl OpenBlock {
    fn new(speaker: String, text: String, t: f64) -> Self {
        OpenBlock { speaker, text, first: t, last: t, emitted: 0 }
    }
}

fn epoch_now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

/// Snapshots of the caption region in, settled lines out. A snapshot is `{"t": ms, "rows": [{"id",
/// "speaker", "text", "self"}]}`: one row per visible caption block, `id` stable while Meet keeps the
/// block's element. Eve's own tile (`self`) is never transcribed.
#[derive(Debug, Default)]
pub struct CaptionMerger {
    pub open: IndexMap<i128, OpenBlock>,
    pub now: f64,
}

impl CaptionMerger {
    pub fn new() -> Self {
        Self::default()
    }

    fn emit(entry: &mut OpenBlock, at: f64, out: &mut Vec<Line>, last: bool) {
        let words = pystr::split(&entry.text);
        if entry.emitted >= words.len() {
            return;
        }
        let mut rest = &words[entry.emitted..];
        if !last && rest.len() > PARAGRAPH_WORDS {
            rest = &rest[..PARAGRAPH_WORDS];
        }
        entry.emitted += rest.len();
        out.push(Line { speaker: entry.speaker.clone(), text: rest.join(" "), at });
    }

    /// One snapshot. Errors are what Python raises on a malformed snapshot (`float()` of its `t`, a `rows`
    /// that cannot be iterated, a snapshot that is not a dict).
    pub fn feed(&mut self, snap: &Value) -> anyhow::Result<Vec<Line>> {
        let Value::Object(snap) = snap else { return Err(py::no_get(snap)) };
        let mut out = Vec::new();
        let raw = match snap.get("t") {
            Some(v) if pystr::truthy(v) => py::float_of(v)?,
            _ => 0.0,
        } / 1000.0;
        let t = if raw != 0.0 { raw } else { epoch_now() };
        if t > self.now {
            self.now = t;
        }
        let mut seen: HashSet<i128> = HashSet::new();
        for row in py::iterate_or_empty(snap.get("rows"))? {
            let Value::Object(row) = row else { continue };
            if row.get("self").is_some_and(pystr::truthy) {
                continue;
            }
            let Ok(rid) = py::int_of(row.get("id").unwrap_or(&Value::Null)) else { continue };
            let speaker = pystr::head(&pystr::squash(&pystr::str_or_empty(row.get("speaker"))), 80);
            let text = pystr::squash(&pystr::str_or_empty(row.get("text")));
            if text.is_empty() {
                continue;
            }
            seen.insert(rid);
            let mut cur = self.open.contains_key(&rid);
            if !cur {
                // a re-rendered block: the same speaker's open line under a new element
                let ids: Vec<i128> = self.open.keys().copied().collect();
                for oid in ids {
                    let other = &self.open[&oid];
                    if !seen.contains(&oid)
                        && other.speaker == speaker
                        && t - other.last <= SETTLE_SECS * 3.0
                        && merge(&other.text, &text).is_some()
                    {
                        if let Some(moved) = self.open.shift_remove(&oid) {
                            self.open.insert(rid, moved);
                            cur = true;
                        }
                        break;
                    }
                }
            }
            if cur {
                let entry = self.open.get_mut(&rid).expect("the open block");
                if entry.speaker != speaker {
                    let first = entry.first;
                    Self::emit(entry, first, &mut out, true);
                    cur = false;
                }
            }
            if !cur {
                self.open.insert(rid, OpenBlock::new(speaker, text, t));
                continue;
            }
            let entry = self.open.get_mut(&rid).expect("the open block");
            let first = entry.first;
            let Some(merged) = merge(&entry.text, &text) else {
                Self::emit(entry, first, &mut out, true);
                self.open.insert(rid, OpenBlock::new(speaker, text, t));
                continue;
            };
            if merged != entry.text {
                entry.text = merged;
                entry.last = t;
            }
            if pystr::split(&entry.text).len() as i64 - entry.emitted as i64 > (PARAGRAPH_WORDS + REVISED_TAIL_WORDS) as i64
            {
                Self::emit(entry, first, &mut out, false);
            }
        }
        let ids: Vec<i128> = self.open.keys().copied().collect();
        for rid in ids {
            if !seen.contains(&rid) && t - self.open[&rid].last >= SETTLE_SECS {
                if let Some(mut entry) = self.open.shift_remove(&rid) {
                    Self::emit(&mut entry, 0.0, &mut out, true);
                }
            }
        }
        Ok(self.stamp(out))
    }

    /// Between snapshots: a block gone or unchanged long enough is written down.
    pub fn tick(&mut self, now: f64) -> Vec<Line> {
        let mut out = Vec::new();
        for entry in self.open.values_mut() {
            if now - entry.last >= IDLE_COMMIT_SECS {
                Self::emit(entry, 0.0, &mut out, true);
            }
        }
        self.stamp(out)
    }

    pub fn finish(&mut self) -> Vec<Line> {
        let mut out = Vec::new();
        for entry in self.open.values_mut() {
            Self::emit(entry, 0.0, &mut out, true);
        }
        self.open.clear();
        self.stamp(out)
    }

    fn stamp(&self, mut lines: Vec<Line>) -> Vec<Line> {
        for ln in &mut lines {
            if ln.at == 0.0 {
                ln.at = self.now;
            }
        }
        lines
    }
}

// ---- the calendar --------------------------------------------------------------------------------

/// Every calendar event in a reply, however deep it is nested (the MCP result wraps Google's list).
pub fn events_in(obj: &Value, depth: usize) -> Vec<Value> {
    if depth > 6 {
        return Vec::new();
    }
    match obj {
        Value::Object(map) => {
            if map.get("start").is_some_and(Value::is_object)
                && (map.contains_key("summary") || map.contains_key("hangoutLink") || map.contains_key("conferenceData"))
            {
                return vec![obj.clone()];
            }
            map.values().flat_map(|v| events_in(v, depth + 1)).collect()
        }
        Value::Array(items) => items.iter().flat_map(|v| events_in(v, depth + 1)).collect(),
        Value::String(s) => {
            let lead = s.trim_start_matches(pystr::is_space);
            if (lead.starts_with('{') || lead.starts_with('[')) && depth < 3 {
                match eve_transcripts::py::loads(s) {
                    Some(v) => events_in(&v, depth + 1),
                    None => Vec::new(),
                }
            } else {
                Vec::new()
            }
        }
        _ => Vec::new(),
    }
}

/// The Meet link of a calendar event: its hangoutLink, a video entry point, its location, or its description.
pub fn event_link(event: &Value) -> Option<String> {
    let get = |k: &str| event.get(k);
    let mut candidates = vec![pystr::str_or_empty(get("hangoutLink"))];
    let entry_points = get("conferenceData").and_then(|c| c.get("entryPoints")).and_then(Value::as_array);
    for ep in entry_points.into_iter().flatten() {
        if ep.is_object() && ep.get("entryPointType") == Some(&Value::from("video")) {
            candidates.push(pystr::str_or_empty(ep.get("uri")));
        }
    }
    candidates.push(pystr::str_or_empty(get("location")));
    candidates.push(pystr::head(&pystr::str_or_empty(get("description")), 4000));
    candidates.iter().find_map(|c| parse_link(c).0)
}

/// `_when`: an event's start or end as an aware time, or None (an all-day event has no call time).
fn when(value: Option<&Value>) -> Option<DateTime<FixedOffset>> {
    let value = value.filter(|v| v.is_object())?;
    let raw = value.get("dateTime").filter(|v| pystr::truthy(v))?;
    let text = pystr::value_str(raw).replace('Z', "+00:00");
    let (naive, offset) = eve_transcripts::py::fromisoformat(&text)?;
    match offset {
        Some(off) => off.from_local_datetime(&naive).single(),
        None => Some(eve_transcripts::py::local_aware(naive)),
    }
}

// (?!\w), not \b, at the end: after "p.m." there is no word boundary, so \b made the dotted forms never match
static CLOCK: LazyLock<fancy_regex::Regex> = LazyLock::new(|| {
    fancy_regex::Regex::new(r"(?i)\b(\d{1,2})(?::(\d{2}))?\s*(am|pm|a\.m\.|p\.m\.)?(?!\w)").expect("CLOCK")
});
static NOW_WORDS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^\s*(?:(?:join|my|the|a|this|current|meeting|call|meet|now|right now|in progress)\s*)*$")
        .expect("NOW_WORDS")
});
static NEXT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\bnext\b").expect("NEXT"));
static AT_MY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b(at|my)\s+\d").expect("AT_MY"));
static WORDS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[a-z0-9]+").expect("WORDS"));

/// (event, link, "") for the meeting the owner means, or (None, "", why not).
///
/// "now" or nothing: one in progress, else the next within the hour. "next": the soonest starting from ten
/// minutes ago. A time ("3pm", "15:30"): the one starting nearest it today. Other words: today's event whose
/// title has them. Only events with a Meet link count.
pub fn find_meeting(events: &[Value], words: &str, now: DateTime<FixedOffset>) -> (Option<Value>, String, String) {
    let none = |why: String| (None, String::new(), why);
    let mut timed: Vec<(DateTime<FixedOffset>, DateTime<FixedOffset>, &Value, String)> = Vec::new();
    for e in events {
        let (start, end) = (when(e.get("start")), when(e.get("end")));
        let link = event_link(e);
        let (Some(start), Some(link)) = (start, link) else { continue };
        if pystr::str_or_empty(e.get("status")) == "cancelled" {
            continue;
        }
        timed.push((start, end.unwrap_or(start + Duration::hours(1)), e, link));
    }
    timed.sort_by_key(|x| x.0);
    if timed.is_empty() {
        return none("no meeting with a Google Meet link on the calendar around then".into());
    }
    let found = |e: &Value, link: &str| (Some(e.clone()), link.to_string(), String::new());
    let w = pystr::strip(words).to_lowercase();
    if NEXT.is_match(&w) {
        for (start, _end, e, link) in &timed {
            if *start >= now - CALENDAR_LOOKBACK {
                return found(e, link);
            }
        }
        return none("no upcoming meeting with a Meet link".into());
    }
    if let Ok(Some(clock)) = CLOCK.captures(&w) {
        let (g2, g3) = (clock.get(2).map(|m| m.as_str()), clock.get(3).map(|m| m.as_str()));
        if g2.is_some() || g3.is_some() || AT_MY.is_match(&w) {
            let num = |s: &str| pystr::int_of_str(s).unwrap_or(0);
            let mut hour = num(clock.get(1).map_or("0", |m| m.as_str()));
            let minute = g2.map_or(0, num);
            let ampm = g3.unwrap_or("").replace('.', "").to_lowercase();
            if ampm == "pm" && hour < 12 {
                hour += 12;
            } else if ampm == "am" && hour == 12 {
                hour = 0;
            } else if ampm.is_empty() && (1..=7).contains(&hour) {
                hour += 12; // "my 3" at work means the afternoon
            }
            if hour > 23 || minute > 59 {
                return none("that time is not a time".into());
            }
            let target = now
                .date_naive()
                .and_hms_opt(hour as u32, minute as u32, 0)
                .and_then(|n| now.offset().from_local_datetime(&n).single())
                .unwrap_or(now);
            let mut best = &timed[0];
            for x in &timed[1..] {
                if (x.0 - target).abs() < (best.0 - target).abs() {
                    best = x;
                }
            }
            if (best.0 - target).abs() <= TIME_SLACK {
                return found(best.2, &best.3);
            }
            return none(
                format!("no meeting with a Meet link near {}", target.format("%-I:%M %p")).replace(":00 ", " "),
            );
        }
    }
    if w.is_empty() || NOW_WORDS.is_match(&w) {
        for (start, end, e, link) in &timed {
            if *start - CALENDAR_LOOKBACK <= now && now < *end {
                return found(e, link);
            }
        }
        for (start, _end, e, link) in &timed {
            if now <= *start && *start <= now + CALENDAR_AHEAD {
                return found(e, link);
            }
        }
        return none("no meeting with a Meet link is on now or in the next hour".into());
    }
    let stop = ["join", "my", "the", "meeting", "call", "meet", "with", "a", "to"];
    let keys: Vec<&str> = WORDS.find_iter(&w).map(|m| m.as_str()).filter(|k| !stop.contains(k)).collect();
    for (start, _end, e, link) in &timed {
        let title = pystr::str_or_empty(e.get("summary")).to_lowercase();
        if !keys.is_empty() && keys.iter().all(|k| title.contains(k)) && start.date_naive() == now.date_naive() {
            return found(e, link);
        }
    }
    none("no meeting today whose title matches that".into())
}

// ---- the page ------------------------------------------------------------------------------------

/// The Meet tab, as the session drives it: open the link, run the page script with options, close.
#[async_trait]
pub trait MeetPage: Send + Sync {
    async fn open(&self, url: &str) -> anyhow::Result<()>;
    async fn run(&self, opts: Value) -> anyhow::Result<Map<String, Value>>;
    async fn close(&self) -> anyhow::Result<()>;
}

/// The owner's yes or no to Chrome's "Allow remote debugging?" (chrome_consent's answer_own_connection).
pub type Answer = Arc<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>;

/// Eve's own tab in the owner's Chrome, on a browser lane of its own (browser_lane.BrowserLane in attach
/// mode): a web task on the shared lane never takes the meeting's tab. `answer` presses Chrome's "Allow
/// remote debugging?" for this connection (chrome_consent.ConsentBroker.answer_own_connection).
pub struct ChromeMeetPage {
    lane: BrowserLane,
    answer: Option<Answer>,
}

impl ChromeMeetPage {
    pub fn new(lane: BrowserLane, answer: Option<Answer>) -> Self {
        ChromeMeetPage { lane, answer }
    }

    /// The lane the tab lives on.
    pub fn lane(&self) -> &BrowserLane {
        &self.lane
    }
}

#[async_trait]
impl MeetPage for ChromeMeetPage {
    async fn open(&self, url: &str) -> anyhow::Result<()> {
        match &self.answer {
            Some(answer) => {
                let answer = answer.clone();
                self.lane.connect_answering(move || answer()).await?
            }
            None => self.lane.connect().await?,
        }
        self.lane.open_url(url).await?;
        Ok(())
    }

    async fn run(&self, opts: Value) -> anyhow::Result<Map<String, Value>> {
        let out = self
            .lane
            .run(move |core| -> anyhow::Result<Value> {
                let Some(p) = core.page.as_ref() else {
                    return Ok(json!({"closed": true})); // never reopened: a closed tab is the owner leaving
                };
                match p.page.evaluate(PAGE_SCRIPT, Some(&opts)) {
                    // the tab was closed under the lane (Playwright's page.is_closed())
                    Err(e) if eve_desktop_lanes::error_kind(&e) == "TargetClosedError" => Ok(json!({"closed": true})),
                    other => other,
                }
            })
            .await??;
        Ok(match out {
            Value::Object(m) => m,
            _ => Map::new(),
        })
    }

    async fn close(&self) -> anyhow::Result<()> {
        self.lane.close().await // eve's tab only; the owner's Chrome stays
    }
}

// ---- the notes -----------------------------------------------------------------------------------

pub const SUMMARY_PROMPT: &str = "You are writing up notes from a video call that someone's assistant sat in on. You are
given the call's live captions, one line per speaker turn, as \"Speaker: words\". Captions are automatic, so
expect wrong words and missing punctuation; the speaker names come from the call and are reliable.

Write the notes a competent person would write:
- A title of six words or fewer, naming the actual subject, the way someone labels a file to find it again.
- The gist: what this call was, in one or two sentences.
- The points that carry information. Merge repetition. Drop greetings and scheduling chatter.
- Decisions, only where something was actually decided, with who decided when the captions show it.
- Actions, each one a thing somebody is meant to do, with the person's name when the captions give it.
- Open questions: what was raised and not resolved.
- Anything you could not make out, said plainly, rather than a confident guess.

Every list may be empty; do not pad one. Output the JSON the schema asks for.";

/// The call's own shape: room and phone notes moved to the detailed one (notes.SUMMARY_SCHEMA, 2026-10-04), and the
/// Meet notes and their message (summary_message) keep this one.
pub static MEET_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    let list = json!({"type": "array", "items": {"type": "string"}});
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {"title": {"type": "string"}, "gist": {"type": "string"}, "points": list, "decisions": list,
                       "actions": list, "questions": list, "unclear": list},
        "required": ["title", "gist", "points", "decisions", "actions", "questions", "unclear"],
    })
});

/// The summary model call: the transcript in, the notes as a JSON object out.
pub type Summarize = Arc<dyn Fn(&str) -> anyhow::Result<Map<String, Value>> + Send + Sync>;

/// `make_summarizer`'s closure, as a value: the model, the SDK it calls through, and where it is metered.
pub struct Summarizer {
    pub model: String,
    pub sdk: Arc<dyn NotesSdk>,
    pub ledger: eve_spend::Ledger,
}

impl Summarizer {
    pub fn summarize(&self, transcript: &str) -> anyhow::Result<Map<String, Value>> {
        let count = transcript.chars().count();
        let input: String = transcript.chars().skip(count.saturating_sub(MAX_TRANSCRIPT_CHARS)).collect();
        let body = json!({
            "model": self.model, "instructions": SUMMARY_PROMPT, "input": input,
            "text": {"format": {"type": "json_schema", "name": "notes", "strict": true, "schema": *MEET_SCHEMA}},
            "max_output_tokens": 2500, "reasoning": {"effort": "low"},
        });
        let resp = self.sdk.responses_create(body)?;
        self.ledger.record_response(eve_spend::MEET, &resp, &self.model, eve_openrouter::PROVIDER, None);
        let text = eve_notes::output_text(&resp);
        let text = pystr::strip(&text);
        match eve_transcripts::py::loads(if text.is_empty() { "{}" } else { text }) {
            Some(Value::Object(m)) => Ok(m),
            Some(_) => Err(NotesError::NotAnObject.into()),
            None => Err(NotesError::NotJson.into()),
        }
    }
}

fn env_get(environ: Option<&HashMap<String, String>>, name: &str) -> String {
    match environ {
        Some(env) => env.get(name).cloned().unwrap_or_default(),
        None => std::env::var(name).unwrap_or_default(),
    }
}

/// The summarizer over OpenRouter with the owner's key, or None (one warning) without a key. `opts` lets a test
/// point the client at a local server.
pub fn summarizer(environ: Option<&HashMap<String, String>>, opts: eve_openrouter::ClientOptions) -> Option<Summarizer> {
    let key = eve_openrouter::key(environ);
    if key.is_empty() {
        tracing::warn!("meet: OPENROUTER_API_KEY is not set — notes are written without a summary");
        return None;
    }
    let raw = env_get(environ, "EVE_MEET_SUMMARY_MODEL");
    let raw = if raw.is_empty() { DEFAULT_SUMMARY_MODEL.to_string() } else { raw };
    let model = match pystr::strip(&raw) {
        "" => DEFAULT_SUMMARY_MODEL.to_string(),
        m => m.to_string(),
    };
    Some(Summarizer { model, sdk: Arc::new(OpenRouterSdk::new(key, opts)), ledger: eve_spend::Ledger::default() })
}

/// `make_summarizer(environ)`: `environ` None reads the process environment.
pub fn make_summarizer(environ: Option<&HashMap<String, String>>) -> Option<Summarize> {
    let s = summarizer(environ, eve_openrouter::ClientOptions::default())?;
    Some(Arc::new(move |t: &str| s.summarize(t)))
}

/// One transcript line as the notes file shows it.
pub fn line_text(ln: &Line) -> String {
    let when = if ln.at != 0.0 { local_hhmm(ln.at) } else { String::new() };
    let who = if ln.speaker.is_empty() { "Someone" } else { ln.speaker.as_str() };
    let when = if when.is_empty() { String::new() } else { format!(" ({when})") };
    format!("- **{who}**{when}: {}", ln.text)
}

/// `datetime.fromtimestamp(at).strftime("%H:%M")`.
fn local_hhmm(at: f64) -> String {
    if !at.is_finite() {
        return String::new();
    }
    let secs = at.floor();
    let nanos = ((at - secs) * 1e9) as u32;
    match Local.timestamp_opt(secs as i64, nanos.min(999_999_999)) {
        chrono::LocalResult::Single(t) | chrono::LocalResult::Ambiguous(t, _) => t.format("%H:%M").to_string(),
        chrono::LocalResult::None => String::new(),
    }
}

static STEM_CODE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[^a-z0-9-]+").expect("STEM_CODE"));

/// One file per call, 0600 in 0700 folders, under `transcripts/meetings/<date>/` where memory_search finds
/// it. Created when the call starts; the transcript is appended as lines settle; at the end it is rewritten
/// with the notes above the transcript. Never written over another call's file.
#[derive(Debug)]
pub struct NotesFile {
    pub started: DateTime<FixedOffset>,
    pub lines: Vec<Line>,
    pub path: PathBuf,
    pub code: String,
}

impl NotesFile {
    pub fn new(root: &Path, started: DateTime<FixedOffset>, code: &str) -> anyhow::Result<NotesFile> {
        let day = root.join(started.format("%Y-%m-%d").to_string());
        std::fs::create_dir_all(&day).map_err(|e| py::os_error(&e, &day))?;
        let parent = match root.parent() {
            Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
            _ => PathBuf::from("."),
        };
        for d in [parent.as_path(), root, day.as_path()] {
            let _ = std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700));
        }
        let clean = STEM_CODE.replace_all(&code.to_lowercase(), "").into_owned();
        let stem = format!("{}-meet-{}", started.format("%H%M"), if clean.is_empty() { "call" } else { &clean });
        let mut file = NotesFile { started, lines: Vec::new(), path: PathBuf::new(), code: code.to_string() };
        for n in 1..=50 {
            let path = day.join(if n == 1 { format!("{stem}.md") } else { format!("{stem}-{n}.md") });
            let opened = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(&path);
            let mut f = match opened {
                Ok(f) => f,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(py::os_error(&e, &path)),
            };
            let head = file.head("in progress", code, "") + "## Transcript\n\n";
            f.write_all(head.as_bytes()).map_err(|e| py::os_error(&e, &path))?;
            file.path = path;
            return Ok(file);
        }
        Err(LaneError::err("OSError", "fifty calls already share this minute"))
    }

    fn head(&self, status: &str, code: &str, title: &str) -> String {
        let title = if title.is_empty() { format!("Call {code}") } else { title.to_string() };
        format!(
            "---\nstatus: {status}\nsource: eve-meet\nmeeting: {code}\nstarted: {}\n---\n\n# {title}\n\n",
            self.started.format("%Y-%m-%d %H:%M")
        )
    }

    pub fn append(&mut self, lines: &[Line]) -> anyhow::Result<()> {
        if lines.is_empty() {
            return Ok(());
        }
        self.lines.extend_from_slice(lines);
        let mut f = OpenOptions::new()
            .append(true)
            .create(true)
            .open(&self.path)
            .map_err(|e| py::os_error(&e, &self.path))?;
        let text: String = lines.iter().map(|ln| line_text(ln) + "\n").collect();
        f.write_all(text.as_bytes()).map_err(|e| py::os_error(&e, &self.path))
    }

    pub fn transcript(&self) -> String {
        self.lines
            .iter()
            .map(|ln| format!("{}: {}", if ln.speaker.is_empty() { "Someone" } else { &ln.speaker }, ln.text))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Rewrite the file with the notes above the transcript. Errors are OSErrors.
    pub fn finish(&self, summary: &Map<String, Value>, label: &str, ended: &str) -> anyhow::Result<()> {
        let title_raw = match summary.get("title") {
            Some(v) if pystr::truthy(v) => pystr::value_str(v),
            _ => label.to_string(),
        };
        let title = pystr::strip(&title_raw).trim_end_matches('.');
        let head = self.head("notes", &self.code, title);
        let head = head.trim_end_matches('\n').replacen(
            "status: notes\n",
            &format!("status: notes\nended: {} ({ended})\n", Local::now().format("%Y-%m-%d %H:%M")),
            1,
        );
        let mut parts: Vec<String> = vec![head, String::new()];
        let gist = pystr::strip(&pystr::str_or_empty(summary.get("gist"))).to_string();
        if !gist.is_empty() {
            parts.extend([gist, String::new()]);
        }
        for (heading, key) in [("Points", "points"), ("Decisions", "decisions"), ("Actions", "actions"),
                               ("Open questions", "questions"), ("Not made out", "unclear")] {
            let rows = eve_notes::items(summary.get(key), None);
            if !rows.is_empty() {
                parts.push(format!("## {heading}"));
                parts.push(String::new());
                parts.extend(rows.iter().map(|r| format!("- {r}")));
                parts.push(String::new());
            }
        }
        if summary.is_empty() && !self.lines.is_empty() {
            parts.extend(["> The write-up failed, so this is the transcript alone.".to_string(), String::new()]);
        }
        parts.extend([
            "## Transcript".to_string(),
            String::new(),
            "<!-- Meet's live captions: expect wrong words; the names are Meet's -->".to_string(),
            String::new(),
        ]);
        if self.lines.is_empty() {
            parts.push("(nothing was said while eve was in the call)".into());
        } else {
            parts.extend(self.lines.iter().map(line_text));
        }
        let tmp = self.path.with_extension("tmp");
        let mut f = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&tmp)
            .map_err(|e| py::os_error(&e, &tmp))?;
        f.write_all((parts.join("\n") + "\n").as_bytes()).map_err(|e| py::os_error(&e, &tmp))?;
        drop(f);
        std::fs::rename(&tmp, &self.path).map_err(|e| py::os_error(&e, &tmp))
    }
}

/// The text the owner gets when the call is over.
pub fn summary_message(label: &str, ended: &str, summary: &Map<String, Value>, lines: usize, path: Option<&Path>) -> String {
    if lines == 0 {
        return format!("{label} is over ({ended}). Nothing was said while I was in it, so there are no notes.");
    }
    let title = pystr::strip(&pystr::str_or_empty(summary.get("title"))).to_string();
    let named = if !title.is_empty() && title != label { format!(": {title}") } else { String::new() };
    let mut parts = vec![format!("Notes from {label}{named} ({ended}).")];
    let gist = pystr::strip(&pystr::str_or_empty(summary.get("gist"))).to_string();
    if !gist.is_empty() {
        parts.push(gist);
    }
    for (heading, key) in [("Decisions", "decisions"), ("Actions", "actions"), ("Open questions", "questions")] {
        let rows = eve_notes::items(summary.get(key), Some(8));
        if !rows.is_empty() {
            parts.push(format!("{heading}:\n{}", rows.iter().map(|r| format!("• {r}")).collect::<Vec<_>>().join("\n")));
        }
    }
    if summary.is_empty() {
        parts.push(format!("The write-up failed; the transcript ({lines} lines) is saved."));
    }
    if let Some(p) = path {
        parts.push(format!("Saved as {}.", p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()));
    }
    parts.join("\n\n")
}

// ---- one call ------------------------------------------------------------------------------------

/// `(text, about)`: a text to the owner (see MeetSession's `tell`).
pub type Notify = Arc<dyn Fn(String, String) -> BoxFuture<'static, anyhow::Result<()>> + Send + Sync>;
pub type Clock = Arc<dyn Fn() -> f64 + Send + Sync>;
pub type Sleep = Arc<dyn Fn(f64) -> BoxFuture<'static, ()> + Send + Sync>;
pub type Wall = Arc<dyn Fn() -> DateTime<FixedOffset> + Send + Sync>;
/// The page's clock (Date.now) is the Mac's wall clock, in epoch seconds.
pub type Epoch = Arc<dyn Fn() -> f64 + Send + Sync>;

static MONOTONIC: LazyLock<Instant> = LazyLock::new(Instant::now);

/// `time.monotonic`.
pub fn monotonic() -> Clock {
    Arc::new(|| MONOTONIC.elapsed().as_secs_f64())
}

/// `asyncio.sleep`.
pub fn real_sleep() -> Sleep {
    Arc::new(|secs: f64| Box::pin(tokio::time::sleep(StdDuration::from_secs_f64(secs.max(0.0)))) as BoxFuture<'static, ()>)
}

/// `datetime.now` (as an aware local time).
pub fn local_wall() -> Wall {
    Arc::new(|| Local::now().fixed_offset())
}

/// `time.time`.
pub fn epoch_clock() -> Epoch {
    Arc::new(epoch_now)
}

#[derive(Debug, Clone, PartialEq)]
pub struct MeetConfig {
    pub enabled: bool,
    /// the Google account whose Chrome profile joins ("": the lane's default)
    pub profile: String,
    pub lobby_minutes: f64,
    pub max_hours: f64,
    /// the call is not played out loud on the Mac
    pub quiet: bool,
    pub guest_name: String,
}

impl Default for MeetConfig {
    fn default() -> Self {
        MeetConfig {
            enabled: MEET_DEFAULT,
            profile: String::new(),
            lobby_minutes: DEFAULT_LOBBY_MINUTES,
            max_hours: DEFAULT_MAX_HOURS,
            quiet: true,
            guest_name: DEFAULT_GUEST_NAME.into(),
        }
    }
}

/// `EVE_MEET`, `EVE_MEET_PROFILE`, `EVE_MEET_LOBBY_MINUTES`, `EVE_MEET_MAX_HOURS`, `EVE_MEET_QUIET`,
/// `EVE_MEET_GUEST_NAME`. `environ` None reads the process environment.
pub fn configured(environ: Option<&HashMap<String, String>>) -> MeetConfig {
    let get = |name: &str| env_get(environ, name);
    let or = |name: &str, default: &str| {
        let v = get(name);
        if v.is_empty() {
            default.to_string()
        } else {
            v
        }
    };
    let num = |name: &str, default: f64, lo: f64, hi: f64| -> f64 {
        let raw = or(name, &pystr::float_repr(default));
        match eve_notes::py_float(&raw) {
            // min(hi, max(lo, x)), with Python's answer for NaN (the first argument wins an undecided comparison)
            Some(x) => {
                let y = if x > lo { x } else { lo };
                if y < hi {
                    y
                } else {
                    hi
                }
            }
            None => default,
        }
    };
    let on = matches!(
        pystr::strip(&or("EVE_MEET", if MEET_DEFAULT { "1" } else { "0" })).to_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    );
    MeetConfig {
        enabled: on,
        profile: pystr::strip(&get("EVE_MEET_PROFILE")).to_lowercase(),
        lobby_minutes: num("EVE_MEET_LOBBY_MINUTES", DEFAULT_LOBBY_MINUTES, 1.0, 120.0),
        max_hours: num("EVE_MEET_MAX_HOURS", DEFAULT_MAX_HOURS, 0.25, 12.0),
        quiet: !matches!(pystr::strip(&or("EVE_MEET_QUIET", "1")).to_lowercase().as_str(), "0" | "false" | "no" | "off"),
        guest_name: pystr::head(pystr::strip(&or("EVE_MEET_GUEST_NAME", DEFAULT_GUEST_NAME)), 40),
    }
}

// The history's lines for the texts that name the meeting (meet's `tell`'s `about`): code's own words, never the
// label, which can be a calendar invite's title (review, round 4).
pub const ABOUT_IN: &str = "I texted the owner that I am in the meeting, microphone and camera off, taking notes.";
pub const ABOUT_LOBBY: &str = "I texted the owner that I am waiting in the meeting's lobby for someone to admit me.";
pub const ABOUT_CAPTIONS: &str =
    "I texted the owner that the meeting's captions won't turn on, so I can't take notes there yet.";
pub const ABOUT_JOINING: &str = "I told the owner I am joining the meeting with the microphone and camera off.";
pub const ABOUT_NOT_JOINING: &str = "I told the owner I could not join that meeting.";

#[derive(Debug, Clone, PartialEq)]
pub struct SessionState {
    /// joining → lobby → in call → done
    pub phase: String,
    pub joined_at: Option<f64>,
    pub ended: String,
    pub lines: usize,
    pub notes: Option<PathBuf>,
    pub clicks: Vec<Value>,
}

impl Default for SessionState {
    fn default() -> Self {
        SessionState {
            phase: "joining".into(),
            joined_at: None,
            ended: String::new(),
            lines: 0,
            notes: None,
            clicks: Vec::new(),
        }
    }
}

/// What the owner's side holds of a running call: its state, the leave switch and the cancel switch.
#[derive(Clone)]
pub struct SessionHandle {
    pub label: String,
    state: Arc<Mutex<SessionState>>,
    leave_asked: Arc<AtomicBool>,
    cancel: Arc<watch::Sender<bool>>,
    clock: Clock,
}

impl SessionHandle {
    pub fn state(&self) -> SessionState {
        self.state.lock().clone()
    }

    pub fn status(&self) -> Map<String, Value> {
        let s = self.state.lock().clone();
        let mut out = Map::new();
        out.insert("meeting".into(), Value::from(self.label.clone()));
        out.insert("phase".into(), Value::from(s.phase.clone()));
        out.insert("lines_heard".into(), Value::from(s.lines));
        if let Some(j) = s.joined_at {
            out.insert("minutes_in_call".into(), json!(eve_notes::py_round1(((self.clock)() - j) / 60.0)));
        }
        if !s.ended.is_empty() {
            out.insert("ended".into(), Value::from(s.ended));
        }
        out
    }

    pub fn leave(&self) {
        self.leave_asked.store(true, Ordering::SeqCst);
    }

    /// Set why the call ended (Meeter.close: "eve was shutting down").
    pub fn set_ended(&self, why: &str) {
        self.state.lock().ended = why.to_string();
    }

    /// `task.cancel()`: the call stops where it is, and the notes are still written and texted.
    pub fn cancel(&self) {
        let _ = self.cancel.send(true);
    }
}

/// The things a session borrows: the summary model, the clock, the sleep, the wall clock and the page's clock.
#[derive(Clone)]
pub struct SessionOptions {
    pub config: MeetConfig,
    pub summarize: Option<Summarize>,
    pub clock: Clock,
    pub sleep: Sleep,
    pub wall: Wall,
    pub epoch: Epoch,
}

impl Default for SessionOptions {
    fn default() -> Self {
        SessionOptions {
            config: MeetConfig::default(),
            summarize: None,
            clock: monotonic(),
            sleep: real_sleep(),
            wall: local_wall(),
            epoch: epoch_clock(),
        }
    }
}

/// One call, from opening the tab to the notes. Everything outside is lent — the page, the clock, the sleep,
/// the summary model, and `notify` (a text to the owner) — so the whole state machine runs in tests.
pub struct MeetSession {
    pub url: String,
    pub label: String,
    page: Box<dyn MeetPage>,
    pub notes_root: PathBuf,
    notify: Notify,
    pub config: MeetConfig,
    summarize: Option<Summarize>,
    clock: Clock,
    sleep: Sleep,
    wall: Wall,
    epoch: Epoch,
    state: Arc<Mutex<SessionState>>,
    pub started_at: f64,
    leave_asked: Arc<AtomicBool>,
    cancel: Arc<watch::Sender<bool>>,
    pub merger: CaptionMerger,
    pub file: Option<NotesFile>,
    cursor: i64,
}

fn truthy(st: &Map<String, Value>, key: &str) -> bool {
    st.get(key).is_some_and(pystr::truthy)
}

impl MeetSession {
    pub fn new(
        url: &str,
        label: &str,
        page: Box<dyn MeetPage>,
        notes_root: &Path,
        notify: Notify,
        opts: SessionOptions,
    ) -> MeetSession {
        let (cancel, _) = watch::channel(false);
        let started_at = (opts.clock)();
        MeetSession {
            url: url.into(),
            label: label.into(),
            page,
            notes_root: notes_root.into(),
            notify,
            config: opts.config,
            summarize: opts.summarize,
            clock: opts.clock,
            sleep: opts.sleep,
            wall: opts.wall,
            epoch: opts.epoch,
            state: Arc::new(Mutex::new(SessionState::default())),
            started_at,
            leave_asked: Arc::new(AtomicBool::new(false)),
            cancel: Arc::new(cancel),
            merger: CaptionMerger::new(),
            file: None,
            cursor: 0,
        }
    }

    /// The call's state, leave and cancel, for whoever started it.
    pub fn handle(&self) -> SessionHandle {
        SessionHandle {
            label: self.label.clone(),
            state: self.state.clone(),
            leave_asked: self.leave_asked.clone(),
            cancel: self.cancel.clone(),
            clock: self.clock.clone(),
        }
    }

    pub fn state(&self) -> SessionState {
        self.state.lock().clone()
    }

    pub fn status(&self) -> Map<String, Value> {
        self.handle().status()
    }

    pub fn leave(&self) {
        self.leave_asked.store(true, Ordering::SeqCst);
    }

    fn set_ended(&self, why: impl Into<String>) {
        self.state.lock().ended = why.into();
    }

    fn ended_or(&self, why: impl Into<String>) {
        let mut s = self.state.lock();
        if s.ended.is_empty() {
            s.ended = why.into();
        }
    }

    /// A text to the owner. `about` is what the chat's history keeps instead, for a text carrying words from
    /// the call (the summary): what was said in a meeting is never read back to the brain as eve's own words.
    async fn tell(&self, text: String, about: &str) {
        // a text that did not go costs the text, never the call
        if let Err(e) = (self.notify)(text, about.to_string()).await {
            tracing::error!("meet: could not text the owner: {e:#}");
        }
    }

    async fn poll(&self, opts: Value) -> anyhow::Result<Map<String, Value>> {
        let st = self.page.run(opts).await?;
        let clicked = py::iterate_or_empty(st.get("clicked"))?;
        self.state.lock().clicks.extend(clicked);
        Ok(st)
    }

    /// The whole call; the state when it is over. Cancelling (SessionHandle::cancel) stops the call where it is
    /// and still writes and texts the notes.
    pub async fn run(mut self) -> SessionState {
        let mut cancelled = self.cancel.subscribe();
        let outcome = {
            let body = self.body();
            tokio::select! {
                r = body => Some(r),
                _ = async {
                    if cancelled.wait_for(|c| *c).await.is_err() {
                        std::future::pending::<()>().await;
                    }
                } => None,
            }
        };
        match outcome {
            None => self.ended_or("eve stopped"),
            Some(Ok(())) => {}
            Some(Err(e)) => {
                // Chrome refused, the tab died: the owner is told why
                tracing::warn!("meet: {} failed: {}", self.label, py::error_text(&e));
                self.ended_or(pystr::head(&format!("it failed: {}", py::error_text(&e)), 200));
            }
        }
        if let Err(e) = self.finish().await {
            tracing::error!("meet: the call's task failed: {} ({})", py::error_text(&e), py::error_name(&e));
        }
        self.state()
    }

    async fn body(&mut self) -> anyhow::Result<()> {
        self.page.open(&self.url).await?;
        if self.join().await? {
            self.listen().await?;
        }
        Ok(())
    }

    async fn join(&mut self) -> anyhow::Result<bool> {
        let (mut errors, mut lobby_since, mut told_lobby) = (0u32, None::<f64>, false);
        loop {
            if self.leave_asked.load(Ordering::SeqCst) {
                self.set_ended("you asked me to leave before I was in");
                return Ok(false);
            }
            let now = (self.clock)();
            let opts = json!({"act": true, "quiet": self.config.quiet, "captions": false,
                              "guestName": self.config.guest_name,
                              "noControlsOk": now - self.started_at >= NO_CONTROLS_SECS});
            let st = match self.poll(opts).await {
                Ok(st) => {
                    errors = 0;
                    st
                }
                Err(e) => {
                    errors += 1;
                    if errors >= PAGE_ERRORS {
                        return Err(LaneError::err(
                            "RuntimeError",
                            format!("the Meet tab stopped answering ({})", py::error_name(&e)),
                        ));
                    }
                    (self.sleep)(JOIN_POLL_SECS).await;
                    continue;
                }
            };
            if truthy(&st, "closed") {
                self.set_ended("the tab was closed");
                return Ok(false);
            }
            if truthy(&st, "inCall") {
                {
                    let mut s = self.state.lock();
                    s.phase = "in call".into();
                    s.joined_at = Some((self.clock)());
                }
                let file = NotesFile::new(&self.notes_root, (self.wall)(), &meeting_code(&self.url))?;
                self.state.lock().notes = Some(file.path.clone());
                self.file = Some(file);
                // the label can be an invite's title (outside words): every text naming it leaves the history a
                // line of code's own (`about`), so it never comes back to the brain as eve's own words
                self.tell(
                    format!(
                        "I'm in {}: microphone and camera off, taking notes. Say /meet leave when you want me out.",
                        self.label
                    ),
                    ABOUT_IN,
                )
                .await;
                return Ok(true);
            }
            if truthy(&st, "signIn") {
                self.set_ended("Chrome's Google account is signed out, so Meet asked me to sign in");
                return Ok(false);
            }
            if truthy(&st, "ended") {
                self.set_ended(format!("Meet said: {}", pystr::value_str(&st["ended"])));
                return Ok(false);
            }
            if truthy(&st, "lobby") {
                self.state.lock().phase = "lobby".into();
                let since = *lobby_since.get_or_insert(now);
                if !told_lobby {
                    told_lobby = true;
                    self.tell(
                        format!("I'm waiting to be let into {}. Someone in the call has to admit me.", self.label),
                        ABOUT_LOBBY,
                    )
                    .await;
                }
                if now - since >= self.config.lobby_minutes * 60.0 {
                    self.set_ended(format!("nobody let me in within {} minutes", py::fmt_g(self.config.lobby_minutes)));
                    self.leave_page().await;
                    return Ok(false);
                }
            } else if now - self.started_at >= JOIN_SECS {
                self.set_ended(why_not_in(&st));
                return Ok(false);
            }
            (self.sleep)(JOIN_POLL_SECS).await;
        }
    }

    async fn listen(&mut self) -> anyhow::Result<()> {
        let (mut errors, mut out_polls, mut told_captions) = (0u32, 0u32, false);
        let joined_at = self.state.lock().joined_at;
        let deadline = joined_at.filter(|j| *j != 0.0).map(|j| j + self.config.max_hours * 3600.0);
        loop {
            let now = (self.clock)();
            if self.leave_asked.load(Ordering::SeqCst) {
                self.set_ended("you asked me to leave");
                self.leave_page().await;
                return Ok(());
            }
            if deadline.is_some_and(|d| now >= d) {
                self.set_ended(format!("it ran past {} hours", py::fmt_g(self.config.max_hours)));
                self.leave_page().await;
                return Ok(());
            }
            let opts = json!({"act": true, "quiet": self.config.quiet, "captions": true, "since": self.cursor});
            let st = match self.poll(opts).await {
                Ok(st) => {
                    errors = 0;
                    st
                }
                Err(e) => {
                    errors += 1;
                    if errors >= PAGE_ERRORS {
                        self.set_ended(format!("the Meet tab stopped answering ({})", py::error_name(&e)));
                        return Ok(());
                    }
                    (self.sleep)(LISTEN_POLL_SECS).await;
                    continue;
                }
            };
            if truthy(&st, "closed") {
                self.set_ended("the tab was closed");
                return Ok(());
            }
            for snap in py::iterate_or_empty(st.get("snaps"))? {
                let Value::Object(m) = &snap else { return Err(py::no_get(&snap)) };
                let seq = match m.get("seq") {
                    Some(v) if pystr::truthy(v) => py::int_of(v)?,
                    _ => 0,
                };
                self.cursor = self.cursor.max(seq.clamp(i64::MIN as i128, i64::MAX as i128) as i64);
                let lines = self.merger.feed(&snap)?;
                self.write(&lines)?;
            }
            let lines = self.merger.tick((self.epoch)());
            self.write(&lines)?;
            if truthy(&st, "inCall") {
                out_polls = 0;
            } else {
                out_polls += 1;
                if out_polls >= OUT_OF_CALL_POLLS || truthy(&st, "ended") {
                    self.set_ended(if truthy(&st, "ended") {
                        format!("Meet said: {}", pystr::value_str(&st["ended"]))
                    } else {
                        "the call ended".to_string()
                    });
                    return Ok(());
                }
            }
            let s = self.state();
            if !told_captions
                && !truthy(&st, "captionsOn")
                && s.lines == 0
                && s.joined_at.is_some_and(|j| now - j >= NO_CAPTIONS_SECS)
            {
                told_captions = true;
                self.tell(
                    format!(
                        "Captions won't turn on in {}, so I can't take notes there yet. Captions may be off for this \
                         meeting or its language.",
                        self.label
                    ),
                    ABOUT_CAPTIONS,
                )
                .await;
            }
            (self.sleep)(LISTEN_POLL_SECS).await;
        }
    }

    fn write(&mut self, lines: &[Line]) -> anyhow::Result<()> {
        if let (false, Some(file)) = (lines.is_empty(), self.file.as_mut()) {
            file.append(lines)?;
            self.state.lock().lines += lines.len();
        }
        Ok(())
    }

    async fn leave_page(&self) {
        for _ in 0..3 {
            let Ok(st) = self.poll(json!({"leave": true})).await else { return };
            if !truthy(&st, "inCall") {
                return;
            }
            (self.sleep)(0.5).await;
        }
    }

    async fn finish(&mut self) -> anyhow::Result<()> {
        self.state.lock().phase = "done".into();
        let ended = {
            let s = self.state.lock();
            if s.ended.is_empty() { "the call ended".to_string() } else { s.ended.clone() }
        };
        if self.file.is_some() {
            let lines = self.merger.finish();
            self.write(&lines)?;
            let file = self.file.as_ref().expect("the notes file");
            let mut summary: Map<String, Value> = Map::new();
            if let (false, Some(summarize)) = (file.lines.is_empty(), self.summarize.clone()) {
                let transcript = file.transcript();
                let job = tokio::task::spawn_blocking(eve_spend::carry(move || summarize(&transcript))).await;
                match job {
                    Ok(Ok(m)) => summary = m,
                    Ok(Err(e)) => tracing::warn!("meet: the summary failed ({})", py::error_name(&e)),
                    Err(_) => tracing::warn!("meet: the summary failed (RuntimeError)"),
                }
            }
            if let Err(e) = file.finish(&summary, &self.label, &ended) {
                tracing::warn!("meet: could not write the notes ({})", py::error_name(&e));
            }
            let name = file.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let text = summary_message(&self.label, &ended, &summary, file.lines.len(), Some(&file.path));
            let about = format!(
                "I texted the owner the meeting's notes ({ended}); they are saved as {name} in the meeting notes, \
                 where memory_search finds them."
            );
            self.tell(text, &about).await;
        } else {
            // the label is an invite's title (outside words): the history gets a line of code's own (v3 review)
            self.tell(
                format!("I couldn't join {}: {ended}.", self.label),
                "I texted the owner that I could not join the meeting.",
            )
            .await;
        }
        let _ = self.page.close().await;
        Ok(())
    }
}

/// `_why_not_in`: why the Join never happened, from the last state the page showed.
pub fn why_not_in(st: &Map<String, Value>) -> String {
    if truthy(st, "switchHere") && !truthy(st, "joinHereToo") {
        return "Meet only offered to move the call off your other device, and I won't take it from you".into();
    }
    if truthy(st, "permission") {
        return "Meet is asking for a microphone or camera permission I won't give".into();
    }
    if truthy(st, "micOn") || truthy(st, "camOn") {
        return "I couldn't turn the microphone and camera off, so I didn't join".into();
    }
    "the Join button never came up".into()
}

// ---- the owner's side ----------------------------------------------------------------------------

/// Opens a fresh tab connection per call.
pub type MakePage = Arc<dyn Fn() -> anyhow::Result<Box<dyn MeetPage>> + Send + Sync>;
/// Reads the owner's calendar between two times (a blocking call, run on a thread).
pub type ListEvents =
    Arc<dyn Fn(DateTime<FixedOffset>, DateTime<FixedOffset>) -> anyhow::Result<Value> + Send + Sync>;

/// The Meeter's keyword arguments.
#[derive(Clone)]
pub struct MeeterOptions {
    pub list_events: Option<ListEvents>,
    pub summarize: Option<Summarize>,
    pub notify: Option<Notify>,
    pub clock: Clock,
    pub sleep: Sleep,
    pub wall: Wall,
    pub epoch: Epoch,
}

impl Default for MeeterOptions {
    fn default() -> Self {
        MeeterOptions {
            list_events: None,
            summarize: None,
            notify: None,
            clock: monotonic(),
            sleep: real_sleep(),
            wall: local_wall(),
            epoch: epoch_clock(),
        }
    }
}

/// At most one call at a time, and the tools the chat and the voice use to start and stop it.
///
/// `make_page` opens a fresh tab connection per call; `list_events` reads the owner's calendar (a blocking
/// call, run on a thread) for "join my 3pm"; `notify` texts the owner (telegram.TelegramInlet.tell_owner).
pub struct Meeter {
    pub config: MeetConfig,
    pub make_page: MakePage,
    pub notes_root: PathBuf,
    pub list_events: Option<ListEvents>,
    pub summarize: Option<Summarize>,
    pub notify: Notify,
    pub clock: Clock,
    pub sleep: Sleep,
    pub wall: Wall,
    pub epoch: Epoch,
    /// the browser lane each page is built on (make_meeter); None for a Meeter built by hand
    pub lane_config: Option<BrowserLaneConfig>,
    session: Mutex<Option<SessionHandle>>,
    task: Mutex<Option<JoinHandle<SessionState>>>,
}

/// `_no_notify`: no chat to tell.
pub fn no_notify() -> Notify {
    Arc::new(|text: String, _about: String| {
        Box::pin(async move {
            tracing::info!("meet: (no chat to tell) {text}");
            Ok(())
        }) as BoxFuture<'static, anyhow::Result<()>>
    })
}

impl Meeter {
    pub fn new(config: MeetConfig, make_page: MakePage, notes_root: &Path, opts: MeeterOptions) -> Meeter {
        Meeter {
            config,
            make_page,
            notes_root: notes_root.into(),
            list_events: opts.list_events,
            summarize: opts.summarize,
            notify: opts.notify.unwrap_or_else(no_notify),
            clock: opts.clock,
            sleep: opts.sleep,
            wall: opts.wall,
            epoch: opts.epoch,
            lane_config: None,
            session: Mutex::new(None),
            task: Mutex::new(None),
        }
    }

    /// A call is running (its task is not done).
    pub fn busy(&self) -> bool {
        self.task.lock().as_ref().is_some_and(|t| !t.is_finished())
    }

    /// The current (or last) call.
    pub fn session(&self) -> Option<SessionHandle> {
        self.session.lock().clone()
    }

    /// `await meeter.task`: wait for the running call to end and hand back its state.
    pub async fn wait(&self) -> Option<SessionState> {
        let task = self.task.lock().take()?;
        task.await.ok()
    }

    pub fn instructions(&self) -> &'static str {
        INSTRUCTIONS_BLOCK
    }

    pub fn tools(&self) -> Vec<Value> {
        TOOLS.as_array().cloned().unwrap_or_default()
    }

    /// (link, label, "") or (None, "", why not): a link or code in the words, else the calendar.
    pub async fn resolve(&self, meeting: &str) -> (Option<String>, String, String) {
        let (link, why) = parse_link(meeting);
        if let Some(link) = link {
            let label = meeting_code(&link);
            return (Some(link), label, String::new());
        }
        if !why.is_empty() && why != NO_LINK {
            return (None, String::new(), why);
        }
        let Some(list_events) = self.list_events.clone() else {
            return (None, String::new(), "send me the meet.google.com link: I can't read your calendar from here".into());
        };
        let now = (self.wall)().with_timezone(&Local).fixed_offset();
        let day_start = now
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .and_then(|n| now.offset().from_local_datetime(&n).single())
            .unwrap_or(now);
        let day_end = day_start + Duration::days(1);
        let job = tokio::task::spawn_blocking(eve_spend::carry(move || list_events(day_start, day_end))).await;
        let reply = match job {
            Ok(Ok(reply)) => reply,
            Ok(Err(e)) => return (None, String::new(), format!("I couldn't read your calendar ({})", py::error_name(&e))),
            Err(_) => return (None, String::new(), "I couldn't read your calendar (RuntimeError)".into()),
        };
        let (event, link, why) = find_meeting(&events_in(&reply, 0), meeting, now);
        let Some(event) = event else { return (None, String::new(), why) };
        let label = match event.get("summary") {
            Some(v) if pystr::truthy(v) => pystr::value_str(v),
            _ => meeting_code(&link),
        };
        (Some(link), pystr::head(pystr::strip(&label), 80), String::new())
    }

    /// Start a call. An error is `make_page` failing (Python raises out of `join`).
    pub async fn join(&self, meeting: &str) -> anyhow::Result<Value> {
        if !self.config.enabled {
            return Ok(json!({"ok": false, "reason": OFF_REASON}));
        }
        if self.busy() {
            if let Some(s) = self.session() {
                return Ok(json!({"ok": false, "reason": format!("I'm already in {}; leave it first", s.label)}));
            }
        }
        let (link, label, why) = self.resolve(meeting).await;
        let Some(link) = link else { return Ok(json!({"ok": false, "reason": why})) };
        if self.busy() {
            // another join started while the calendar was read
            return Ok(json!({"ok": false, "reason": "I'm already joining a meeting"}));
        }
        let page = (self.make_page)()?;
        let opts = SessionOptions {
            config: self.config.clone(),
            summarize: self.summarize.clone(),
            clock: self.clock.clone(),
            sleep: self.sleep.clone(),
            wall: self.wall.clone(),
            epoch: self.epoch.clone(),
        };
        let session = MeetSession::new(&link, &label, page, &self.notes_root, self.notify.clone(), opts);
        *self.session.lock() = Some(session.handle());
        *self.task.lock() = Some(tokio::spawn(session.run()));
        tracing::info!("meet: joining {}", meeting_code(&link));
        Ok(json!({"ok": true, "joining": label, "link": link,
                  "note": "joining now with the microphone and camera off; the owner is texted when eve is in"}))
    }

    pub fn status(&self) -> Value {
        let Some(s) = self.session() else { return json!({"ok": true, "in_meeting": false}) };
        let mut out = Map::new();
        out.insert("ok".into(), Value::Bool(true));
        out.insert("in_meeting".into(), Value::Bool(self.busy()));
        out.extend(s.status());
        if let Some(notes) = s.state().notes {
            let name = notes.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            out.insert("notes_file".into(), Value::from(name));
        }
        Value::Object(out)
    }

    pub fn leave(&self) -> Value {
        match (self.busy(), self.session()) {
            (true, Some(s)) => {
                s.leave();
                json!({"ok": true, "leaving": s.label})
            }
            _ => json!({"ok": false, "reason": "I'm not in a meeting"}),
        }
    }

    /// The bare `/meet` answer, by code.
    pub fn status_line(&self) -> String {
        if !self.config.enabled {
            return "Joining meetings is off on this computer (EVE_MEET).".into();
        }
        let session = self.session();
        let Some(session) = session.filter(|_| self.busy()) else {
            return "I'm not in a meeting. Send /meet with a Google Meet link, or say which one \
                    (\"/meet now\", \"/meet 3pm\"), and I'll join muted with the camera off and take notes."
                .into();
        };
        let s = session.status();
        let phase = s.get("phase").and_then(Value::as_str).unwrap_or("").to_string();
        let where_ = match phase.as_str() {
            "joining" => "joining",
            "lobby" => "waiting in the lobby of",
            "in call" => "in",
            other => other,
        };
        let extra = if phase == "in call" {
            let minutes = s.get("minutes_in_call").and_then(Value::as_f64).unwrap_or(0.0);
            format!(", {} min, {} lines heard", py::fmt_g(minutes), s.get("lines_heard").cloned().unwrap_or(json!(0)))
        } else {
            String::new()
        };
        let meeting = s.get("meeting").and_then(Value::as_str).unwrap_or("");
        format!("I'm {where_} {meeting}{extra}. /meet leave takes me out.")
    }

    pub async fn handle(&self, name: &str, args: &Value) -> anyhow::Result<Value> {
        match name {
            "meet_join" => {
                let meeting = pystr::str_or_empty(args.get("meeting"));
                self.join(pystr::strip(&meeting)).await
            }
            "meet_status" => Ok(self.status()),
            "meet_leave" => Ok(self.leave()),
            _ => Ok(json!({"ok": false, "reason": format!("unknown tool {name}")})),
        }
    }

    /// Stop a running call (eve shutting down): the notes are still written and texted.
    pub async fn close(&self) {
        if !self.busy() {
            return;
        }
        if let Some(s) = self.session() {
            s.set_ended("eve was shutting down");
            s.cancel();
        }
        let task = self.task.lock().take();
        if let Some(task) = task {
            let _ = task.await;
        }
    }
}

/// What `calendar_lister` calls: apps_mcp.AppsBridge.execute (blocking).
pub trait CalendarApps: Send + Sync {
    fn execute(&self, tool: &str, args: Value) -> anyhow::Result<Value>;
}

/// The owner's calendar through Google's Calendar MCP server (apps_mcp.AppsBridge.execute, blocking).
pub fn calendar_lister(apps: Arc<dyn CalendarApps>) -> ListEvents {
    Arc::new(move |start: DateTime<FixedOffset>, end: DateTime<FixedOffset>| {
        let out = apps.execute(
            "calendar__list_events",
            json!({"startTime": isoformat(&start), "endTime": isoformat(&end), "orderBy": "startTime", "pageSize": 50}),
        )?;
        if out.is_object() && out.get("ok") == Some(&Value::Bool(false)) && events_in(&out, 0).is_empty() {
            let why = [out.get("reason"), out.get("error")]
                .into_iter()
                .flatten()
                .find(|v| pystr::truthy(v))
                .map(pystr::value_str)
                .unwrap_or_else(|| "the calendar call failed".into());
            return Err(LaneError::err("RuntimeError", pystr::head(&why, 200)));
        }
        Ok(out)
    })
}

/// `datetime.isoformat()` of an aware time: microseconds only when there are some, the offset as `±HH:MM`.
fn isoformat(t: &DateTime<FixedOffset>) -> String {
    let micros = t.timestamp_subsec_micros();
    let base = if micros == 0 {
        t.format("%Y-%m-%dT%H:%M:%S").to_string()
    } else {
        format!("{}.{micros:06}", t.format("%Y-%m-%dT%H:%M:%S"))
    };
    let off = t.offset().local_minus_utc();
    let (sign, off) = if off < 0 { ('-', -off) } else { ('+', off) };
    let secs = if off % 60 != 0 { format!(":{:02}", off % 60) } else { String::new() };
    format!("{base}{sign}{:02}:{:02}{secs}", off / 3600, off % 3600 / 60)
}

/// The lane a meeting joins on: never Steel (its Chrome is headless with its sound muted: a meeting needs real
/// audio, so Meet joins from eve's own Chrome — EVE'S OWN CHROME in browser_lane — whatever drives the other web
/// tasks), and the Meet profile when one is named.
pub fn meet_lane_config(cfg: &MeetConfig, mut lane_cfg: BrowserLaneConfig) -> BrowserLaneConfig {
    if lane_cfg.steel {
        lane_cfg.steel = false;
        lane_cfg.own = true;
    }
    if !cfg.profile.is_empty() {
        lane_cfg.chrome_profile = cfg.profile.clone();
    }
    lane_cfg
}

/// The real Meeter, or None (one log line) when it is off or Chrome attach is off. `environ` None reads the
/// process environment.
pub fn make_meeter(
    config: Option<MeetConfig>,
    notes_root: &Path,
    answer: Option<Answer>,
    apps: Option<Arc<dyn CalendarApps>>,
    environ: Option<&HashMap<String, String>>,
) -> Option<Meeter> {
    let cfg = config.unwrap_or_else(|| configured(environ));
    if !cfg.enabled {
        tracing::info!("meet: off (EVE_MEET)");
        return None;
    }
    let lane_cfg = eve_browser_lane::configured(environ);
    if !lane_cfg.attach {
        tracing::info!("meet: off — it joins from the owner's Chrome, and EVE_BROWSER_ATTACH is off");
        return None;
    }
    let lane_cfg = meet_lane_config(&cfg, lane_cfg);
    let page_cfg = lane_cfg.clone();
    let make_page: MakePage = Arc::new(move || -> anyhow::Result<Box<dyn MeetPage>> {
        let lane = BrowserLane::new(page_cfg.clone(), LaneOptions::default())?;
        Ok(Box::new(ChromeMeetPage::new(lane, answer.clone())))
    });
    if lane_cfg.own {
        tracing::info!("meet: on — joins from eve's own Chrome, as the account signed in there, listen only");
    } else {
        let profile = if lane_cfg.chrome_profile.is_empty() { "the first open" } else { &lane_cfg.chrome_profile };
        tracing::info!("meet: on — joins from {profile}'s Chrome profile, listen only");
    }
    let opts = MeeterOptions {
        list_events: apps.map(calendar_lister),
        summarize: make_summarizer(environ),
        ..Default::default()
    };
    let mut meeter = Meeter::new(cfg, make_page, notes_root, opts);
    meeter.lane_config = Some(lane_cfg);
    Some(meeter)
}
