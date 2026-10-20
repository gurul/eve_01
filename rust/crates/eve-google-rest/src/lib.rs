//! Gmail, Calendar and Drive through Google's plain REST APIs, shaped like their MCP servers.
//!
//! Google's Workspace MCP servers answer only for a Cloud project enrolled in the Workspace Developer Preview
//! Program. Live 2026-10-04 with the owner's personal gmail: "Access to this tool requires that your Google Cloud
//! project (...) is enrolled in the Google Workspace Developer Preview Program", and a personal account does not
//! get in. The same sign-in reads the plain APIs, so eve uses those. The tool names and arguments follow the MCP
//! servers' own (gmail search_threads, calendar list_events, suggest_time, respond_to_event, drive search_files,
//! download_file_content...), so the brain and Meet's calendar lookup read the same either way.
//!
//! Every call is grounded in Google's reference (checked 2026-10-04):
//!
//! * Gmail v1 users.threads / users.messages / users.messages.attachments / users.labels to read (gmail.readonly).
//!   Bodies: the first text/plain part that is not an attachment, else text/html stripped. users.messages.send to
//!   send (gmail.send, which can send and nothing else): a base64url RFC 2822 message in `raw`; a reply carries
//!   the thread's threadId and In-Reply-To/References set to the original's Message-ID, or Gmail starts a new
//!   thread. Every send is the owner's yes first (apps_mcp's "ask" policy), shown whole.
//! * People v1 people:searchContacts and otherContacts:search (contacts.readonly, contacts.other.readonly): the
//!   owner's saved contacts, and the people they have emailed who were never saved. Google asks for one search
//!   with an empty query first, to warm its cache, else results lag; the client does that once.
//! * Calendar v3 events.list / get / insert / patch / delete / quickAdd / instances, freeBusy.query,
//!   calendarList.list. timeMin/timeMax must carry an offset (RFC 3339), so a bare local time gets the Mac's.
//!   A PATCH merges nested objects, so switching all-day and timed sends the other key as null. sendUpdates
//!   decides whether guests are emailed (the API's default is no email). Every event handed back is compacted
//!   (`compact_event`) and carries Google Maps links for its place (`place_links`), so "my next class and a
//!   maps link" is one calendar call.
//! * Drive v3 files.list / get / export, alt=media downloads. Google Docs export as Markdown (headings and
//!   tables kept), Sheets as CSV, Slides as text; exports are capped at 10 MB by Google. PDFs are read with
//!   macOS's PDFKit, so no PDF library is added.
//!
//! The client has apps_mcp's McpClient shape: `list_tools()` and `call_tool()`, results in MCP's shape
//! (`structuredContent`, `isError`). Lists carry `nextPageToken` and take `pageToken`. Mail bodies and file text
//! are clipped; nothing is logged but types.

mod html;
mod html5_entities;
mod mail;
pub mod py;
pub mod pytime;

use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::Duration as StdDuration;

use async_trait::async_trait;
use base64::Engine;
use bytes::Bytes;
use chrono::{DateTime, Duration, FixedOffset, NaiveDate, TimeZone};
use futures::{Stream, StreamExt};
use indexmap::IndexMap;
use regex::Regex;
use serde_json::{json, Map, Value};

pub use html::unescape as html_unescape;
pub use pytime::{PyDateTime, Zone};

use py::{py_int, py_str, repr_str, squash_ws, strip, take_chars, truthy, type_name};
use pytime::isoformat;

pub const GMAIL: &str = "https://gmail.googleapis.com/gmail/v1/users/me";
pub const PEOPLE: &str = "https://people.googleapis.com/v1";
pub const CALENDAR: &str = "https://www.googleapis.com/calendar/v3";
pub const DRIVE: &str = "https://www.googleapis.com/drive/v3";
pub const DRIVE_UPLOAD: &str = "https://www.googleapis.com/upload/drive/v3/files";
/// Characters of one mail body.
pub const BODY_CLIP: usize = 4000;
/// Characters of one file's or attachment's text.
pub const FILE_CLIP: usize = 50_000;
/// Threads summarised per search (each is one more request).
pub const THREADS_MAX: i64 = 10;
/// Bytes read from one file or attachment.
pub const DOWNLOAD_MAX: usize = 25 * 1024 * 1024;
/// search_events looks this far back (and forward without a limit).
pub const SEARCH_BACK_DAYS: i64 = 365;
/// list_events with no calendarId reads this many calendars at most, one request each.
pub const CALENDARS_MAX: usize = 20;
/// Characters of one event's description handed to the model.
pub const EVENT_DESC_CLIP: usize = 500;
/// Characters of a location put in a Maps link (Google's URL limit is 2,048).
pub const PLACE_CLIP: usize = 500;
/// Addresses in one email's To, Cc and Bcc together.
pub const RECIPIENTS_MAX: usize = 20;
/// People per contacts search (Google's own cap is 30).
pub const CONTACTS_MAX: i64 = 10;

// ---- errors ------------------------------------------------------------------------------------------------

/// A failed HTTP exchange below the status line (httpx's `HTTPError` family): `kind` is the httpx class name
/// the Python would have raised (ConnectError, ReadTimeout...).
#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct TransportError {
    pub kind: String,
    pub message: String,
}

impl TransportError {
    pub fn new(kind: impl Into<String>, message: impl Into<String>) -> Self {
        Self { kind: kind.into(), message: message.into() }
    }
}

/// What the Python raises, by class. `call_tool` turns `Google`, `Key` and `Value` into an `isError` result, as
/// the Python catches `(GoogleError, KeyError, ValueError)`; the others leave `call_tool` as errors, as they
/// propagate in Python. `type_name()` and `Display` are `type(e).__name__` and `str(e)`.
#[derive(Debug, Clone, thiserror::Error)]
pub enum Error {
    /// `GoogleError`: Google refused, or eve is not signed in.
    #[error("{0}")]
    Google(String),
    /// `KeyError`: a required argument is missing (the key, printed as Python's repr of it).
    #[error("{}", repr_str(.0))]
    Key(String),
    /// `ValueError` (or its subclass `JSONDecodeError`, named in `name`).
    #[error("{msg}")]
    Value { name: &'static str, msg: String },
    /// An httpx transport error.
    #[error("{0}")]
    Http(TransportError),
    /// `AttributeError`: Google's reply was not the JSON object the call reads.
    #[error("{0}")]
    Attribute(String),
    /// `TypeError`.
    #[error("{0}")]
    Type(String),
    /// `OverflowError`: a date past Python's range.
    #[error("{0}")]
    Overflow(String),
}

impl Error {
    pub fn value(msg: impl Into<String>) -> Self {
        Error::Value { name: "ValueError", msg: msg.into() }
    }

    /// `type(e).__name__`.
    pub fn type_name(&self) -> &str {
        match self {
            Error::Google(_) => "GoogleError",
            Error::Key(_) => "KeyError",
            Error::Value { name, .. } => name,
            Error::Http(e) => &e.kind,
            Error::Attribute(_) => "AttributeError",
            Error::Type(_) => "TypeError",
            Error::Overflow(_) => "OverflowError",
        }
    }

    /// Whether `call_tool` catches it into an `isError` result.
    pub fn is_tool_error(&self) -> bool {
        matches!(self, Error::Google(_) | Error::Key(_) | Error::Value { .. })
    }
}

impl From<TransportError> for Error {
    fn from(e: TransportError) -> Self {
        Error::Http(e)
    }
}

/// `GoogleError`, the class the Python names.
pub type GoogleError = Error;

// ---- tools ---------------------------------------------------------------------------------------------------

fn tool(name: &str, description: &str, props: Value, required: &[&str], reads: bool) -> Value {
    json!({"name": name, "description": description,
           "inputSchema": {"type": "object", "properties": props, "required": required},
           "annotations": {"readOnlyHint": reads}})
}

/// The tools each app offers, named and shaped as the MCP servers' own.
pub static TOOLS: LazyLock<IndexMap<&'static str, Vec<Value>>> = LazyLock::new(|| {
    let s = json!({"type": "string"});
    let b = json!({"type": "boolean"});
    let page = json!({"type": "integer", "description": "How many to return (default 10)."});
    let token = json!({"type": "string", "description": "nextPageToken from the previous call, for the next page."});
    let when = json!({"type": "string", "description": "ISO 8601, e.g. 2026-10-10T12:00:00-07:00 (no offset: the owner's own)."});
    let notify = json!({"type": "boolean", "description": "Email the guests about it (default: yes when there are guests)."});
    let strings = json!({"type": "array", "items": s});
    let mut tools = IndexMap::new();
    tools.insert("gmail", vec![
        tool("search_threads", "Search mail threads with Gmail search syntax (from:, to:, is:unread, \
              newer_than:2d, has:attachment, subject:...). Returns each thread's subject, sender, date and snippet.",
             json!({"query": s, "pageSize": page, "pageToken": token}), &[], true),
        tool("get_thread", "Read a whole thread: every message's sender, date, subject, text and attachments.",
             json!({"threadId": s}), &["threadId"], true),
        tool("get_message", "Read one message: sender, date, subject, text and attachments.",
             json!({"messageId": s}), &["messageId"], true),
        tool("get_attachment", "Read an attachment's text (PDF and text files) by the attachmentId that \
              get_message or get_thread listed.", json!({"messageId": s, "attachmentId": s, "filename": s}),
             &["messageId", "attachmentId"], true),
        tool("list_labels", "The mailbox's labels, with the inbox's total and unread counts.", json!({}), &[], true),
        tool("send_message", "Send an email from the owner's Gmail. to, cc and bcc are email addresses; \"me\" \
              is the owner's own. To answer \
              a message, pass its messageId as replyToMessageId: the email goes into that thread, and the subject \
              defaults to the original's with Re:. body is plain text.",
             json!({"to": strings, "cc": strings, "bcc": strings, "subject": s, "body": s, "replyToMessageId": s}),
             &["to", "body"], false),
    ]);
    tools.insert("contacts", vec![
        tool("search_contacts", "Find people by name, email or phone in the owner's Google contacts, and among \
              the people they have emailed. Returns names, email addresses, phone numbers and organizations.",
             json!({"query": s, "pageSize": page}), &["query"], true),
    ]);
    tools.insert("calendar", vec![
        tool("list_calendars", "The owner's calendars (id, name, primary).", json!({"pageSize": page}), &[], true),
        tool("list_events", "Events between startTime and endTime, soonest first, recurring events as single \
              occurrences, each tagged with its calendar. Default: every calendar the owner sees except \
              holidays (a calendarId reads that one only), from now, 7 days.",
             json!({"calendarId": s, "startTime": when, "endTime": when, "fullText": s, "pageSize": page,
                    "pageToken": token}), &[], true),
        tool("search_events", "Find events on the primary calendar by words in the title, description, place \
              or attendees, from a year ago on, soonest first.",
             json!({"query": s, "pageSize": page, "pageToken": token}), &["query"], true),
        tool("get_event", "One event in full.", json!({"calendarId": s, "eventId": s}), &["eventId"], true),
        tool("suggest_time", "Free slots of at least durationMinutes between startTime and endTime, busy time taken \
              from every calendar the owner sees except holidays, within working hours (default 9 to 18).",
             json!({"startTime": when, "endTime": when, "durationMinutes": {"type": "integer"},
                    "workdayStartHour": {"type": "integer"}, "workdayEndHour": {"type": "integer"}}),
             &["startTime", "endTime", "durationMinutes"], true),
        tool("create_event", "Create an event. With allDay, startTime and endTime are dates (YYYY-MM-DD, end \
              exclusive). recurrence takes RRULE lines, e.g. [\"RRULE:FREQ=WEEKLY;BYDAY=MO\"].",
             json!({"summary": s, "startTime": when, "endTime": when, "allDay": b, "timeZone": s,
                    "description": s, "location": s, "attendeeEmails": strings,
                    "recurrence": strings, "addGoogleMeetUrl": b, "calendarId": s,
                    "notifyGuests": notify}), &["summary", "startTime", "endTime"], false),
        tool("quick_add_event", "Create an event from one plain sentence, the way Google Calendar's quick add \
              reads it: \"Lunch with Ana Friday 12pm at Zuni\".", json!({"text": s, "calendarId": s}), &["text"],
             false),
        tool("update_event", "Change an event: only the fields given change. For a recurring event's one \
              occurrence, use that occurrence's eventId (list_events returns them).",
             json!({"eventId": s, "calendarId": s, "summary": s, "startTime": when, "endTime": when, "allDay": b,
                    "timeZone": s, "description": s, "location": s,
                    "addAttendeeEmails": strings, "notifyGuests": notify}),
             &["eventId"], false),
        tool("respond_to_event", "Answer an invitation as the owner: accepted, declined or tentative.",
             json!({"eventId": s, "calendarId": s, "responseStatus": {"type": "string", "enum": [
                 "accepted", "declined", "tentative"]}, "notifyGuests": notify}),
             &["eventId", "responseStatus"], false),
        tool("delete_event", "Delete an event.", json!({"eventId": s, "calendarId": s, "notifyGuests": notify}),
             &["eventId"], false),
    ]);
    tools.insert("drive", vec![
        tool("search_files", "Find files by words in their name or text (or a Drive query such as \
              \"name contains 'budget'\"). Returns id, name, type, modified time and link.",
             json!({"query": s, "pageSize": page, "pageToken": token}), &["query"], true),
        tool("list_recent_files", "The most recently changed files.", json!({"pageSize": page, "pageToken": token}),
             &[], true),
        tool("get_file_metadata", "One file's name, type, owners, size, modified time and link.",
             json!({"fileId": s}), &["fileId"], true),
        tool("read_file_content", "A file's text: Docs as Markdown, Sheets as CSV (first sheet), Slides as text, \
              PDFs and plain text files as text.", json!({"fileId": s}), &["fileId"], true),
        tool("create_file", "Create a file in the owner's Drive from text. mimeType \
              application/vnd.google-apps.document makes it a Google Doc.",
             json!({"title": s, "textContent": s, "mimeType": s}), &["title"], false),
    ]);
    tools
});

// read_file_content is also the MCP server's download_file_content for what it can read.
pub const FILE_FIELDS: &str = "id,name,mimeType,modifiedTime,webViewLink,owners(displayName),size";
pub const EXPORTS: [(&str, &str); 3] = [
    ("application/vnd.google-apps.document", "text/markdown"),
    ("application/vnd.google-apps.spreadsheet", "text/csv"),
    ("application/vnd.google-apps.presentation", "text/plain"),
];
pub const TEXT_TYPES: [&str; 3] = ["application/json", "application/xml", "application/csv"];
pub const CONTACT_FIELDS: &str = "names,emailAddresses,phoneNumbers,organizations";
pub const OTHER_FIELDS: &str = "names,emailAddresses,phoneNumbers";

// Python's `\s` is Unicode whitespace plus the ASCII separators \x1c-\x1f, which Rust's `\s` leaves out.
static ADDRESS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"^(?:[^@\s\x1c-\x1f<>,;"]+@[^@\s\x1c-\x1f<>,;"]+\.[^@\s\x1c-\x1f<>,;"]+)$"#).expect("valid regex")
});

fn export_for(mime: &str) -> Option<&'static str> {
    EXPORTS.iter().find(|(k, _)| *k == mime).map(|(_, v)| *v)
}

// ---- reading Python-shaped values ------------------------------------------------------------------------

static NULL: Value = Value::Null;

/// `v.get(k)` where `v` is a reply Google sent: an AttributeError when it is not an object, as in Python.
fn get<'a>(v: &'a Value, k: &str) -> Result<&'a Value, Error> {
    match v {
        Value::Object(m) => Ok(m.get(k).unwrap_or(&NULL)),
        other => Err(Error::Attribute(format!("'{}' object has no attribute 'get'", type_name(other)))),
    }
}

/// `v.get(k)` on a nested part of a reply, read as absent when the part is not an object.
fn nget<'a>(v: &'a Value, k: &str) -> &'a Value {
    v.get(k).unwrap_or(&NULL)
}

/// `a[k]` on the tool's arguments: a KeyError when it is missing.
fn arg<'a>(a: &'a Map<String, Value>, k: &str) -> Result<&'a Value, Error> {
    a.get(k).ok_or_else(|| Error::Key(k.to_string()))
}

fn aget<'a>(a: &'a Map<String, Value>, k: &str) -> &'a Value {
    a.get(k).unwrap_or(&NULL)
}

/// `for x in (v or [])`: a list's items, a dict's keys, a string's characters; a TypeError for a number.
fn py_iter(v: &Value) -> Result<Vec<Value>, Error> {
    match v {
        _ if !truthy(v) => Ok(vec![]),
        Value::Array(a) => Ok(a.clone()),
        Value::Object(o) => Ok(o.keys().map(|k| Value::String(k.clone())).collect()),
        Value::String(s) => Ok(s.chars().map(|c| Value::String(c.to_string())).collect()),
        other => Err(Error::Type(format!("'{}' object is not iterable", type_name(other)))),
    }
}

/// A nested list read leniently: its items, or nothing.
fn nlist(v: &Value) -> Vec<Value> {
    py_iter(v).unwrap_or_default()
}

fn or_value(v: &Value, default: Value) -> Value {
    if truthy(v) {
        v.clone()
    } else {
        default
    }
}

fn quote(s: &str) -> String {
    urlencoding::encode(s).into_owned()
}

fn token_hex(n: usize) -> String {
    (0..n).map(|_| format!("{:02x}", rand::random::<u8>())).collect()
}

// ---- pure helpers ------------------------------------------------------------------------------------------

fn header(msg: &Value, name: &str) -> String {
    let payload = or_value(nget(msg, "payload"), json!({}));
    for h in nlist(nget(&payload, "headers")) {
        let n = match h.get("name") {
            Some(v) => py_str(v),
            None => String::new(),
        };
        if n.to_lowercase() == name.to_lowercase() {
            return match h.get("value") {
                Some(v) if truthy(v) => py_str(v),
                _ => String::new(),
            };
        }
    }
    String::new()
}

/// `_decode`: base64url with its padding restored; nothing when it is not base64.
fn decode_b64(data: &Value) -> Vec<u8> {
    let Value::String(s) = data else { return Vec::new() };
    let pad = (4 - s.chars().count() % 4) % 4;
    py::b64decode_urlsafe(&format!("{s}{}", "=".repeat(pad))).unwrap_or_default()
}

fn walk(part: &Value, out: &mut Vec<Value>) {
    out.push(part.clone());
    for child in nlist(nget(part, "parts")) {
        walk(&child, out);
    }
}

static TAGS: LazyLock<Regex> = LazyLock::new(|| {
    // `<(script|style)[^>]*>.*?</\1>|<[^>]+>`, re.S | re.I: Rust has no backreference, so the two names
    // are spelled out; the case-insensitive match is the same.
    Regex::new(r"(?is)<script[^>]*>.*?</script>|<style[^>]*>.*?</style>|<[^>]+>").expect("valid regex")
});

/// A message's readable text: the first text/plain part that is not an attachment, else text/html with the
/// tags dropped (a .txt attachment is never taken for the body).
pub fn mail_text(part: &Value) -> String {
    let mut parts = Vec::new();
    walk(part, &mut parts);
    let (mut plain, mut html) = (Vec::new(), Vec::new());
    for p in &parts {
        let data = nget(nget(p, "body"), "data");
        if !truthy(data) || truthy(nget(p, "filename")) {
            continue;
        }
        let mime = py_str(&or_value(nget(p, "mimeType"), json!("")));
        if mime == "text/plain" {
            plain.push(py::decode_utf8(&decode_b64(data)));
        } else if mime == "text/html" {
            html.push(py::decode_utf8(&decode_b64(data)));
        }
    }
    let text = if let Some(t) = plain.first() {
        t.clone()
    } else if let Some(h) = html.first() {
        html::unescape(&TAGS.replace_all(h, " "))
    } else {
        String::new()
    };
    let text = py::splitlines(&text).into_iter().map(strip).filter(|l| !l.is_empty()).collect::<Vec<_>>().join("\n");
    if text.chars().count() <= BODY_CLIP {
        text
    } else {
        format!("{}…", take_chars(&text, BODY_CLIP))
    }
}

/// The attachments of a message: filename, mimeType, size and attachmentId of each part that names a file.
pub fn attachments(part: &Value) -> Vec<Value> {
    let mut parts = Vec::new();
    walk(part, &mut parts);
    parts
        .iter()
        .filter(|p| truthy(nget(p, "filename")) && truthy(nget(nget(p, "body"), "attachmentId")))
        .map(|p| {
            let body = nget(p, "body");
            json!({"filename": nget(p, "filename"), "mimeType": nget(p, "mimeType"), "size": nget(body, "size"),
                   "attachmentId": nget(body, "attachmentId")})
        })
        .collect()
}

fn message(msg: &Value, full: bool) -> Result<Value, Error> {
    let mut out = json!({"id": get(msg, "id")?, "threadId": get(msg, "threadId")?, "from": header(msg, "From"),
                         "to": header(msg, "To"), "date": header(msg, "Date"), "subject": header(msg, "Subject"),
                         "labels": or_value(get(msg, "labelIds")?, json!([])),
                         "snippet": msg.get("snippet").cloned().unwrap_or(json!(""))});
    if full {
        let payload = or_value(get(msg, "payload")?, json!({}));
        out["text"] = json!(mail_text(&payload));
        out["attachments"] = Value::Array(attachments(&payload));
    }
    Ok(out)
}

static DATE_ONLY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d{4}-\d{2}-\d{2}$").expect("valid regex"));

/// A time Google accepts in timeMin/timeMax: with an offset. A bare date is its local midnight; a time with
/// no offset is the Mac's local time. Anything unreadable is passed as it is, for Google to refuse.
pub fn rfc3339(value: &Value) -> String {
    rfc3339_in(value, Zone::Local)
}

/// `rfc3339` with the owner's zone given (the Mac's is `Zone::Local`).
pub fn rfc3339_in(value: &Value, zone: Zone) -> String {
    let text = strip(&py_str(&or_value(value, json!("")))).to_string();
    let parsed = if DATE_ONLY.is_match(&text) {
        pytime::date_fromisoformat(&text).map(|d| PyDateTime::Naive(d.and_time(chrono::NaiveTime::MIN)))
    } else {
        pytime::fromisoformat(&text.replace('Z', "+00:00"))
    };
    match parsed {
        Ok(dt) => isoformat(&dt.aware_or_local(zone)),
        Err(_) => text,
    }
}

/// An event's start or end. Timed: dateTime (with tz when given, else an offset is added) and date null, so a
/// PATCH from all-day clears the date; all-day: the reverse.
fn when(value: &Value, all_day: bool, tz: &Value, zone: Zone) -> Option<Value> {
    if !truthy(value) {
        return None;
    }
    if all_day {
        return Some(json!({"date": take_chars(&py_str(value), 10), "dateTime": null}));
    }
    let date_time = if truthy(tz) { value.clone() } else { json!(rfc3339_in(value, zone)) };
    let mut out = json!({"dateTime": date_time, "date": null});
    if truthy(tz) {
        out["timeZone"] = tz.clone();
    }
    Some(out)
}

fn send_updates(a: &Map<String, Value>, has_guests: bool) -> &'static str {
    let notify = aget(a, "notifyGuests");
    let yes = if notify.is_null() { has_guests } else { truthy(notify) };
    if yes {
        "all"
    } else {
        "none"
    }
}

fn pymax(a: DateTime<FixedOffset>, b: DateTime<FixedOffset>) -> DateTime<FixedOffset> {
    if b > a {
        b
    } else {
        a
    }
}

fn pymin(a: DateTime<FixedOffset>, b: DateTime<FixedOffset>) -> DateTime<FixedOffset> {
    if b < a {
        b
    } else {
        a
    }
}

fn minutes(n: i64) -> Result<Duration, Error> {
    Duration::try_minutes(n)
        .filter(|d| d.num_days().abs() <= 999_999_999)
        .ok_or_else(|| Error::Overflow(format!("days={}; must have magnitude <= 999999999", n.div_euclid(1440))))
}

/// The free stretches of at least `minutes` between start and end, inside working hours each day.
///
/// The days and their working hours are the owner's: wall-clock hours in the Mac's zone, resolved per day
/// (Lean CalendarEdits P5, 2026-10-05). Built in start's fixed offset, a request in UTC offered the owner's
/// night, and a span across the end of daylight time offered the hour before their day.
pub fn free_slots(
    busy: &[(DateTime<FixedOffset>, DateTime<FixedOffset>)],
    start: DateTime<FixedOffset>,
    end: DateTime<FixedOffset>,
    minutes_: i64,
    day_start: i64,
    day_end: i64,
    zone: Zone,
) -> Result<Vec<Value>, Error> {
    let mut busy = busy.to_vec();
    busy.sort();
    let want = minutes(minutes_)?;
    let mut out = Vec::new();
    let mut day = zone.convert(&start).date_naive();
    let local = |d: NaiveDate, hour: i64| -> Result<DateTime<FixedOffset>, Error> {
        Ok(zone.localize(pytime::combine(d, hour)?))
    };
    while local(day, 0)? < end && out.len() < 50 {
        let lo = pymax(start, local(day, day_start)?);
        let hi = pymin(end, local(day, day_end.min(23))?);
        let mut cursor = lo;
        for (b0, b1) in &busy {
            if *b1 <= cursor || *b0 >= hi {
                continue;
            }
            if (*b0 - cursor) >= want {
                out.push(json!({"start": isoformat(&cursor), "end": isoformat(b0)}));
            }
            cursor = pymax(cursor, *b1);
        }
        if cursor < hi && (hi - cursor) >= want {
            out.push(json!({"start": isoformat(&cursor), "end": isoformat(&hi)}));
        }
        day = day
            .succ_opt()
            .filter(|d| chrono::Datelike::year(d) <= 9999)
            .ok_or_else(|| Error::Overflow("date value out of range".into()))?;
    }
    Ok(out)
}

/// A calendar of dates, not commitments: Google's holiday, birthday and week-number calendars.
fn date_only(calendar_id: &str) -> bool {
    calendar_id.ends_with("group.v.calendar.google.com")
        || calendar_id.contains("#holiday@")
        || calendar_id.contains("#contacts@")
        || calendar_id.contains("#weeknum@")
}

// -- events: compact, with Maps links --
// Google Maps URLs (https://developers.google.com/maps/documentation/urls/get-started, checked 2026-10-04):
// "https://www.google.com/maps/search/?api=1&query=..." opens a place by its name or address, and
// "https://www.google.com/maps/dir/?api=1&destination=..." opens directions to it, the origin defaulting to
// where the phone is. api=1 is required; the value must be URL-encoded (spaces as %20) and a URL stays
// under 2,048 characters. No key is needed, so the link is built here and costs no Maps call. Owner's goal,
// 2026-10-04: "look for my next class on calendar and give me maps link" should be one fast step.
pub const MAPS_SEARCH: &str = "https://www.google.com/maps/search/?api=1&query=";
pub const MAPS_DIR: &str = "https://www.google.com/maps/dir/?api=1&destination=";
static URLISH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)[a-z][a-z0-9+.-]*://|\bwww\.|\b[\w-]+\.(?:com|us|net|org|io|me|co)/[^\s\x1c-\x1f]")
        .expect("valid regex")
});
// A location that names a call, not a place: Outlook and Zoom fill these in on their own, and a bare Meet
// code (abc-defg-hij) is a call too.
static NOT_A_PLACE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^(?:(?:microsoft\s+)?teams(?:\s+meeting)?|zoom(?:\s+meeting)?|google\s+meet|webex|online|virtual|remote|phone(?:\s+call)?|tbd|tba|n/?a|[a-z]{3}-[a-z]{4}-[a-z]{3})$",
    )
    .expect("valid regex")
});
// A call's link past the clip is kept: meet.event_link reads the description for one.
static CALL_LINK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)https?://(?:meet\.google\.com|[\w.-]*zoom\.us|teams\.microsoft\.com|teams\.live\.com|[\w.-]*webex\.com)/[^\s\x1c-\x1f]+",
    )
    .expect("valid regex")
});
pub const ATTENDEE_KEYS: [&str; 4] = ["email", "responseStatus", "self", "organizer"];

/// A location Maps can find: not empty, not a link (a Zoom or Meet URL), not a call's name.
pub fn is_place(location: &Value) -> bool {
    let text = squash_ws(&py_str(&or_value(location, json!(""))));
    !text.is_empty() && !URLISH.is_match(&text) && !NOT_A_PLACE.is_match(&text)
}

/// mapsUrl (the place) and directionsUrl (how to get there) for a place, else nothing.
pub fn place_links(location: &Value) -> Map<String, Value> {
    let mut out = Map::new();
    if !is_place(location) {
        return out;
    }
    let q = quote(take_chars(&squash_ws(&py_str(location)), PLACE_CLIP));
    out.insert("mapsUrl".into(), json!(format!("{MAPS_SEARCH}{q}")));
    out.insert("directionsUrl".into(), json!(format!("{MAPS_DIR}{q}")));
    out
}

fn event_description(text: &str) -> String {
    if text.chars().count() <= EVENT_DESC_CLIP {
        return text.to_string();
    }
    let clipped = format!("{}…", py::rstrip(take_chars(text, EVENT_DESC_CLIP)));
    let link = CALL_LINK.find_at(text, py::char_to_byte(text, EVENT_DESC_CLIP));
    match link {
        Some(m) if !CALL_LINK.is_match(&clipped) => format!("{clipped} {}", m.as_str()),
        _ => clipped,
    }
}

/// An event as the model needs it: when, what, where (with its Maps links), how to join, who. Google's
/// full resource (etag, creator, reminders, iCalUID, every guest's name...) is several times larger. meet.py
/// reads start, summary, hangoutLink, conferenceData.entryPoints, location and description, all kept.
pub fn compact_event(event: &Value) -> Value {
    let Value::Object(ev) = event else { return event.clone() };
    let mut out = Map::new();
    for k in ["id", "status", "summary", "start", "end", "location"] {
        match ev.get(k) {
            None | Some(Value::Null) => {}
            Some(Value::String(s)) if s.is_empty() => {}
            Some(v) => {
                out.insert(k.into(), v.clone());
            }
        }
    }
    out.extend(place_links(aget(ev, "location")));
    if truthy(aget(ev, "description")) {
        out.insert("description".into(), json!(event_description(&py_str(&ev["description"]))));
    }
    for k in ["hangoutLink", "conferenceData"] {
        if truthy(aget(ev, k)) {
            out.insert(k.into(), ev[k].clone());
        }
    }
    if let Some(Value::Array(guests)) = ev.get("attendees") {
        let kept: Vec<Value> = guests
            .iter()
            .filter_map(|g| g.as_object())
            .map(|g| {
                Value::Object(
                    ATTENDEE_KEYS.iter().filter_map(|k| g.get(*k).map(|v| (k.to_string(), v.clone()))).collect(),
                )
            })
            .collect();
        out.insert("attendees".into(), Value::Array(kept));
    }
    if let Some(Value::Object(org)) = ev.get("organizer") {
        if truthy(aget(org, "email")) {
            out.insert("organizer".into(), json!({"email": org["email"]}));
        }
    }
    for k in ["recurringEventId", "htmlLink"] {
        if truthy(aget(ev, k)) {
            out.insert(k.into(), ev[k].clone());
        }
    }
    Value::Object(out)
}

/// When an event starts, as an aware datetime for sorting: an all-day date is midnight in the calendar's own
/// time zone (the Mac's when it has none); one that cannot be read sorts last.
fn start_key(event: &Value, tz_name: &Value, zone: Zone) -> DateTime<FixedOffset> {
    let start = match event.get("start") {
        Some(v @ Value::Object(_)) => v,
        _ => &NULL,
    };
    let date_time = nget(start, "dateTime");
    if truthy(date_time) {
        return match pytime::fromisoformat(&py_str(date_time).replace('Z', "+00:00")) {
            Ok(when) => when.aware_or_local(zone),
            Err(_) => pytime::datetime_max_utc(),
        };
    }
    let date = nget(start, "date");
    if truthy(date) {
        let Ok(day) = pytime::date_fromisoformat(&py_str(date)) else { return pytime::datetime_max_utc() };
        let midnight = day.and_time(chrono::NaiveTime::MIN);
        let named = if truthy(tz_name) { py_str(tz_name).parse::<chrono_tz::Tz>().ok() } else { None };
        return match named {
            Some(tz) => match tz.from_local_datetime(&midnight) {
                chrono::LocalResult::Single(d) | chrono::LocalResult::Ambiguous(d, _) => d.fixed_offset(),
                chrono::LocalResult::None => {
                    // a ZoneInfo time in a gap (fold 0) reads with the offset before the change
                    let before = chrono::Offset::fix(&tz.offset_from_utc_datetime(&(midnight - Duration::hours(24))));
                    before.from_local_datetime(&midnight).single().expect("fixed offsets have one reading")
                }
            },
            None => zone.localize(midnight),
        };
    }
    pytime::datetime_max_utc()
}

fn events(items: &Value) -> Result<Vec<Value>, Error> {
    Ok(py_iter(items)?.iter().map(compact_event).collect())
}

/// A PDF's text through macOS's PDFKit; None where that is not available.
#[cfg(target_os = "macos")]
pub fn pdf_text(data: &[u8]) -> Option<String> {
    use objc2::AllocAnyThread;
    use objc2_foundation::NSData;
    use objc2_pdf_kit::PDFDocument;

    let ns = NSData::with_bytes(data);
    // SAFETY: PDFDocument's initWithData: takes any NSData and answers nil for bytes that are not a PDF.
    let doc = unsafe { PDFDocument::initWithData(PDFDocument::alloc(), &ns) }?;
    // SAFETY: string is a plain getter on a document we hold.
    let text = unsafe { doc.string() };
    Some(text.map(|s| s.to_string()).unwrap_or_default())
}

/// A PDF's text through macOS's PDFKit; None where that is not available.
#[cfg(not(target_os = "macos"))]
pub fn pdf_text(_data: &[u8]) -> Option<String> {
    None
}

/// Bytes as readable text by their type, or None when they are not text eve can read.
pub fn as_text(data: &[u8], mime: &str) -> Option<String> {
    if mime == "application/pdf" {
        return pdf_text(data);
    }
    if mime.starts_with("text/") || TEXT_TYPES.contains(&mime) {
        return Some(py::decode_utf8(data));
    }
    None
}

fn clip_text(text: &str) -> (String, bool) {
    (take_chars(text, FILE_CLIP).to_string(), text.chars().count() > FILE_CLIP)
}

/// The email addresses in a To, Cc or Bcc argument (a list, or one comma-separated string), each checked
/// whole: a value that is not exactly an address (a name, a newline that would start another header) is refused,
/// never trimmed into one.
pub fn addresses(value: &Value) -> Result<Vec<String>, Error> {
    let items: Vec<Value> = match value {
        Value::String(_) => vec![value.clone()],
        Value::Array(a) => a.clone(),
        _ => vec![],
    };
    let mut out = Vec::new();
    for item in &items {
        for piece in py_str(item).split(',') {
            let piece = strip(piece);
            if piece.is_empty() {
                continue;
            }
            if !ADDRESS.is_match(piece) {
                return Err(Error::value(format!("not an email address: {}", repr_str(take_chars(piece, 80)))));
            }
            out.push(piece.to_string());
        }
    }
    Ok(out)
}

fn one_line(value: &Value) -> Result<String, Error> {
    let text = py_str(&or_value(value, json!("")));
    if text.contains(['\r', '\n']) {
        return Err(Error::value("a subject is one line"));
    }
    Ok(strip(&text).to_string())
}

/// The base64url RFC 2822 message Gmail's send takes as `raw`. From is left out: Gmail fills in the
/// signed-in account.
pub fn build_mail(
    to: &[String],
    cc: &[String],
    bcc: &[String],
    subject: &str,
    body: &str,
    reply_headers: &[(String, String)],
) -> Result<String, Error> {
    let bytes = mail::message_bytes(to, cc, bcc, subject, body, reply_headers)?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
}

fn person(p: &Value) -> Value {
    let values = |key: &str, field: &str| -> Vec<Value> {
        nlist(nget(p, key))
            .iter()
            .filter(|x| x.is_object() && truthy(nget(x, field)))
            .map(|x| json!(py_str(nget(x, field))))
            .collect()
    };
    let mut out = Map::new();
    out.insert("name".into(), values("names", "displayName").into_iter().next().unwrap_or(json!("")));
    for (key, field, label) in [
        ("emailAddresses", "value", "emails"),
        ("phoneNumbers", "value", "phones"),
        ("organizations", "name", "organizations"),
    ] {
        let got = values(key, field);
        if !got.is_empty() {
            out.insert(label.into(), Value::Array(got));
        }
    }
    Value::Object(out)
}

fn http_error(status: u16, body: &[u8]) -> Error {
    let text = py::decode_utf8(body);
    let msg = match serde_json::from_slice::<Value>(body) {
        Ok(Value::Object(m)) => match m.get("error") {
            None => text.clone(),
            Some(Value::Object(e)) => match e.get("message") {
                Some(v) if truthy(v) => py_str(v),
                _ => text.clone(),
            },
            Some(_) => text.clone(),
        },
        _ => text.clone(),
    };
    Error::Google(format!("HTTP {status}: {}", take_chars(&msg, 200)))
}

/// The Mac's IANA zone name (America/Los_Angeles), which a recurring event needs; UTC when unknown.
pub fn local_zone() -> String {
    if let Ok(link) = std::fs::canonicalize("/etc/localtime") {
        let link = link.to_string_lossy();
        if let Some((_, name)) = link.split_once("zoneinfo/") {
            return name.to_string();
        }
    }
    "UTC".into()
}

/// `json.dumps(value)` with Python's defaults: ", " and ": " separators, non-ASCII escaped.
fn py_json_dumps(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => match n.as_f64() {
            Some(f) if n.is_f64() => py::float_repr(f),
            _ => n.to_string(),
        },
        Value::String(s) => {
            let mut out = String::from("\"");
            for c in s.chars() {
                match c {
                    '"' => out.push_str("\\\""),
                    '\\' => out.push_str("\\\\"),
                    '\n' => out.push_str("\\n"),
                    '\r' => out.push_str("\\r"),
                    '\t' => out.push_str("\\t"),
                    '\u{08}' => out.push_str("\\b"),
                    '\u{0c}' => out.push_str("\\f"),
                    c if (c as u32) < 0x20 || (c as u32) > 0x7e => {
                        let mut buf = [0u16; 2];
                        for unit in c.encode_utf16(&mut buf) {
                            out.push_str(&format!("\\u{unit:04x}"));
                        }
                    }
                    c => out.push(c),
                }
            }
            out.push('"');
            out
        }
        Value::Array(a) => format!("[{}]", a.iter().map(py_json_dumps).collect::<Vec<_>>().join(", ")),
        Value::Object(o) => format!(
            "{{{}}}",
            o.iter().map(|(k, v)| format!("{}: {}", py_json_dumps(&json!(k)), py_json_dumps(v))).collect::<Vec<_>>().join(", ")
        ),
    }
}

// ---- HTTP ------------------------------------------------------------------------------------------------

/// A response body, read as it arrives.
pub type BodyStream = Pin<Box<dyn Stream<Item = Result<Bytes, TransportError>> + Send>>;

/// One request as the client sends it: the URL carries its query already.
#[derive(Debug, Clone)]
pub struct HttpRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
}

pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: BodyStream,
}

impl HttpResponse {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

/// How requests leave the client: reqwest in the daemon, a scripted reply in tests (the Python tests' httpx
/// MockTransport).
#[async_trait]
pub trait Transport: Send + Sync {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError>;
}

/// The daemon's transport: reqwest with httpx's 60 s connect and read timeouts.
pub struct ReqwestTransport {
    client: reqwest::Client,
}

impl ReqwestTransport {
    pub fn new(timeout_secs: f64) -> Self {
        let t = StdDuration::from_secs_f64(timeout_secs.max(0.0));
        let client = reqwest::Client::builder()
            .connect_timeout(t)
            .read_timeout(t)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self { client }
    }
}

fn transport_error(e: reqwest::Error) -> TransportError {
    let kind = if e.is_timeout() && e.is_connect() {
        "ConnectTimeout"
    } else if e.is_timeout() {
        "ReadTimeout"
    } else if e.is_connect() {
        "ConnectError"
    } else if e.is_body() || e.is_decode() {
        "ReadError"
    } else if e.is_builder() {
        "UnsupportedProtocol"
    } else {
        "RequestError"
    };
    TransportError::new(kind, e.to_string())
}

#[async_trait]
impl Transport for ReqwestTransport {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        let method = reqwest::Method::from_bytes(request.method.as_bytes())
            .map_err(|e| TransportError::new("UnsupportedProtocol", e.to_string()))?;
        let mut b = self.client.request(method, &request.url);
        for (k, v) in &request.headers {
            b = b.header(k, v);
        }
        if let Some(body) = request.body {
            b = b.body(body);
        }
        let resp = b.send().await.map_err(transport_error)?;
        let status = resp.status().as_u16();
        let headers = resp
            .headers()
            .iter()
            .map(|(k, v)| (k.as_str().to_string(), String::from_utf8_lossy(v.as_bytes()).into_owned()))
            .collect();
        let body = resp.bytes_stream().map(|r| r.map_err(transport_error));
        Ok(HttpResponse { status, headers, body: Box::pin(body) })
    }
}

/// Where the client gets the owner's Google access token: None when not signed in. A plain closure is one.
#[async_trait]
pub trait TokenSource: Send + Sync {
    async fn token(&self) -> Option<String>;
}

#[async_trait]
impl<F> TokenSource for F
where
    F: Fn() -> Option<String> + Send + Sync,
{
    async fn token(&self) -> Option<String> {
        self()
    }
}

/// `quote_plus` as httpx encodes a query value: letters, digits and `_.-~` kept, a space as `+`.
fn quote_plus(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-' | b'~' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// A query value as httpx writes it: true/false lowercase, None empty, anything else `str()`.
fn param_str(v: &Value) -> String {
    match v {
        Value::Bool(true) => "true".into(),
        Value::Bool(false) => "false".into(),
        Value::Null => String::new(),
        other => py_str(other),
    }
}

/// Query parameters in order; a list value repeats its key, as httpx does.
pub type Params = Vec<(String, Value)>;

fn with_query(url: &str, params: &Params) -> String {
    let mut pairs = Vec::new();
    for (k, v) in params {
        match v {
            Value::Array(items) => {
                for item in items {
                    pairs.push(format!("{}={}", quote_plus(k), quote_plus(&param_str(item))));
                }
            }
            other => pairs.push(format!("{}={}", quote_plus(k), quote_plus(&param_str(other)))),
        }
    }
    if pairs.is_empty() {
        return url.to_string();
    }
    let sep = if url.contains('?') { '&' } else { '?' };
    format!("{url}{sep}{}", pairs.join("&"))
}

/// The body of a request: none, JSON (httpx's compact encoding), or raw bytes with their own content type.
#[derive(Debug, Clone, Default)]
pub enum Body {
    #[default]
    None,
    Json(Value),
    Raw { content: Vec<u8>, content_type: String },
}

fn params(pairs: &[(&str, Value)]) -> Params {
    pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
}

// ---- the client --------------------------------------------------------------------------------------------

/// One Google app (gmail | calendar | drive | contacts) over its plain API, with apps_mcp.McpClient's two
/// methods.
pub struct GoogleRestClient {
    pub app: String,
    token: Arc<dyn TokenSource>,
    http: Arc<dyn Transport>,
    contacts_warm: AtomicBool,
    zone: Zone,
    download_max: usize,
}

impl GoogleRestClient {
    /// `GoogleRestClient(app, token, timeout)`: a ValueError naming the app when it is not one of TOOLS'.
    pub fn new(app: &str, token: impl TokenSource + 'static, timeout_secs: f64) -> Result<Self, Error> {
        Self::with_transport(app, token, Arc::new(ReqwestTransport::new(timeout_secs)))
    }

    /// `GoogleRestClient(app, token, http=...)`: the client over a given transport.
    pub fn with_transport(
        app: &str,
        token: impl TokenSource + 'static,
        http: Arc<dyn Transport>,
    ) -> Result<Self, Error> {
        if !TOOLS.contains_key(app) {
            return Err(Error::value(app));
        }
        Ok(Self {
            app: app.to_string(),
            token: Arc::new(token),
            http,
            contacts_warm: AtomicBool::new(false),
            zone: Zone::Local,
            download_max: DOWNLOAD_MAX,
        })
    }

    /// The zone bare local times are read in (the Mac's by default; tests name one instead of setting TZ).
    pub fn with_zone(mut self, zone: Zone) -> Self {
        self.zone = zone;
        self
    }

    /// The download cap (DOWNLOAD_MAX by default).
    pub fn with_download_max(mut self, bytes: usize) -> Self {
        self.download_max = bytes;
        self
    }

    pub fn list_tools(&self) -> Vec<Value> {
        TOOLS.get(self.app.as_str()).cloned().unwrap_or_default()
    }

    async fn send(&self, method: &str, url: &str, query: &Params, body: Body) -> Result<HttpResponse, Error> {
        let token = self.token.token().await.filter(|t| !t.is_empty());
        let Some(token) = token else {
            return Err(Error::Google("not signed in to Google (apps login google)".into()));
        };
        let mut headers = vec![("Authorization".to_string(), format!("Bearer {token}"))];
        let body = match body {
            Body::None => None,
            Body::Json(v) => {
                headers.push(("Content-Type".into(), "application/json".into()));
                Some(serde_json::to_vec(&v).unwrap_or_default())
            }
            Body::Raw { content, content_type } => {
                headers.push(("Content-Type".into(), content_type));
                Some(content)
            }
        };
        let request = HttpRequest { method: method.into(), url: with_query(url, query), headers, body };
        Ok(self.http.send(request).await?)
    }

    async fn read_all(body: &mut BodyStream) -> Result<Vec<u8>, Error> {
        let mut data = Vec::new();
        while let Some(chunk) = body.next().await {
            data.extend_from_slice(&chunk?);
        }
        Ok(data)
    }

    /// `_req`: one call, Bearer-signed. A 4xx/5xx is a GoogleError with Google's message; an empty reply is
    /// `{}`; a JSON reply is parsed and anything else is its text.
    pub async fn req(&self, method: &str, url: &str, query: &Params, body: Body) -> Result<Value, Error> {
        let mut r = self.send(method, url, query, body).await?;
        let data = Self::read_all(&mut r.body).await?;
        if r.status >= 400 {
            return Err(http_error(r.status, &data));
        }
        if data.is_empty() {
            return Ok(json!({}));
        }
        if r.header("content-type").unwrap_or("").contains("json") {
            return serde_json::from_slice(&data)
                .map_err(|e| Error::Value { name: "JSONDecodeError", msg: e.to_string() });
        }
        Ok(Value::String(py::decode_utf8(&data)))
    }

    /// A file's bytes, read as they arrive and stopped at the download cap: reading the whole body first and
    /// measuring it after held a file of any size in memory.
    pub async fn download(&self, url: &str, query: &Params) -> Result<Vec<u8>, Error> {
        let mut r = self.send("GET", url, query, Body::None).await?;
        if r.status >= 400 {
            let data = Self::read_all(&mut r.body).await?;
            return Err(http_error(r.status, &data));
        }
        let mut data = Vec::new();
        while let Some(chunk) = r.body.next().await {
            data.extend_from_slice(&chunk?);
            if data.len() > self.download_max {
                return Err(Error::Google(format!("the file is larger than {} MB", self.download_max / (1024 * 1024))));
            }
        }
        Ok(data)
    }

    async fn get(&self, url: &str, query: Params) -> Result<Value, Error> {
        self.req("GET", url, &query, Body::None).await
    }

    /// Run one tool: `{"isError": false, "structuredContent": ...}`, or `{"isError": true, "content": [...]}` for
    /// a GoogleError, a missing argument (KeyError) or a bad value (ValueError). Any other failure is returned
    /// as `Err`, as it propagates out of the Python's `call_tool`.
    pub async fn call_tool(&self, name: &str, arguments: &Value) -> Result<Value, Error> {
        let known = TOOLS.get(self.app.as_str()).is_some_and(|ts| ts.iter().any(|t| t["name"] == name));
        if !known {
            return Ok(json!({"isError": true, "content": [{"type": "text", "text": format!("no tool {name}")}]}));
        }
        let a = match arguments {
            Value::Object(m) => m.clone(),
            v if !truthy(v) => Map::new(),
            other => {
                return Err(Error::Type(format!("'{}' object is not a mapping of arguments", type_name(other))))
            }
        };
        match self.dispatch(name, a).await {
            Ok(v) => Ok(json!({"isError": false, "structuredContent": v})),
            Err(e) if e.is_tool_error() => {
                let text = if matches!(e, Error::Google(_)) { e.to_string() } else { format!("{}: {e}", e.type_name()) };
                Ok(json!({"isError": true, "content": [{"type": "text", "text": text}]}))
            }
            Err(e) => Err(e),
        }
    }

    async fn dispatch(&self, name: &str, a: Map<String, Value>) -> Result<Value, Error> {
        match (self.app.as_str(), name) {
            ("gmail", "search_threads") => self.gmail_search_threads(&a).await,
            ("gmail", "get_thread") => self.gmail_get_thread(&a).await,
            ("gmail", "get_message") => self.gmail_get_message(&a).await,
            ("gmail", "get_attachment") => self.gmail_get_attachment(&a).await,
            ("gmail", "list_labels") => self.gmail_list_labels(&a).await,
            ("gmail", "send_message") => self.gmail_send_message(&a).await,
            ("contacts", "search_contacts") => self.contacts_search_contacts(&a).await,
            ("calendar", "list_calendars") => self.calendar_list_calendars(&a).await,
            ("calendar", "list_events") => self.calendar_list_events(&a).await,
            ("calendar", "search_events") => self.calendar_search_events(&a).await,
            ("calendar", "get_event") => self.calendar_get_event(&a).await,
            ("calendar", "suggest_time") => self.calendar_suggest_time(&a).await,
            ("calendar", "create_event") => self.calendar_create_event(&a).await,
            ("calendar", "quick_add_event") => self.calendar_quick_add_event(&a).await,
            ("calendar", "update_event") => self.calendar_update_event(&a).await,
            ("calendar", "respond_to_event") => self.calendar_respond_to_event(&a).await,
            ("calendar", "delete_event") => self.calendar_delete_event(&a).await,
            ("drive", "search_files") => self.drive_search_files(&a).await,
            ("drive", "list_recent_files") => self.drive_list_recent_files(&a).await,
            ("drive", "get_file_metadata") => self.drive_get_file_metadata(&a).await,
            ("drive", "read_file_content") => self.drive_read_file_content(&a).await,
            ("drive", "create_file") => self.drive_create_file(&a).await,
            _ => Ok(json!({"isError": true, "content": [{"type": "text", "text": format!("no tool {name}")}]})),
        }
    }

    /// `_page`: maxResults clamped to [1, top], and the pageToken when one is given.
    fn page(a: &Map<String, Value>, default: i64, top: i64) -> Result<Params, Error> {
        let size = or_value(aget(a, "pageSize"), json!(default));
        let mut out = vec![("maxResults".to_string(), json!(py_int(&size)?.min(top).max(1)))];
        if truthy(aget(a, "pageToken")) {
            out.push(("pageToken".into(), a["pageToken"].clone()));
        }
        Ok(out)
    }

    // -- gmail --
    async fn gmail_search_threads(&self, a: &Map<String, Value>) -> Result<Value, Error> {
        let page = Self::page(a, THREADS_MAX, THREADS_MAX)?;
        let mut query = vec![("q".to_string(), or_value(aget(a, "query"), json!("")))];
        query.extend(page);
        let listing = self.get(&format!("{GMAIL}/threads"), query).await?;
        let mut threads = Vec::new();
        for t in py_iter(get(&listing, "threads")?)? {
            let id = match &t {
                Value::Object(m) => m.get("id").cloned().ok_or_else(|| Error::Key("id".into()))?,
                other => return Err(Error::Type(format!("{} indices must be integers", type_name(other)))),
            };
            let full = self
                .get(
                    &format!("{GMAIL}/threads/{}", py_str(&id)),
                    params(&[("format", json!("metadata")), ("metadataHeaders", json!(["From", "Subject", "Date"]))]),
                )
                .await?;
            let mut msgs = py_iter(get(&full, "messages")?)?;
            if msgs.is_empty() {
                msgs.push(json!({}));
            }
            let (first, last) = (&msgs[0], &msgs[msgs.len() - 1]);
            let unread = msgs.iter().any(|m| match nget(m, "labelIds") {
                Value::Array(l) => l.iter().any(|x| x == "UNREAD"),
                Value::String(s) => s.contains("UNREAD"),
                _ => false,
            });
            threads.push(json!({"threadId": id, "subject": header(first, "Subject"), "from": header(last, "From"),
                                "date": header(last, "Date"), "messages": msgs.len(), "unread": unread,
                                "snippet": last.get("snippet").cloned().unwrap_or(json!(""))}));
        }
        Ok(json!({"threads": threads, "nextPageToken": get(&listing, "nextPageToken")?}))
    }

    async fn gmail_get_thread(&self, a: &Map<String, Value>) -> Result<Value, Error> {
        let id = arg(a, "threadId")?;
        let full = self.get(&format!("{GMAIL}/threads/{}", py_str(id)), params(&[("format", json!("full"))])).await?;
        let msgs = py_iter(get(&full, "messages")?)?;
        let messages = msgs.iter().map(|m| message(m, true)).collect::<Result<Vec<_>, _>>()?;
        Ok(json!({"threadId": id, "messages": messages}))
    }

    async fn gmail_get_message(&self, a: &Map<String, Value>) -> Result<Value, Error> {
        let id = arg(a, "messageId")?;
        let msg = self.get(&format!("{GMAIL}/messages/{}", py_str(id)), params(&[("format", json!("full"))])).await?;
        message(&msg, true)
    }

    async fn gmail_get_attachment(&self, a: &Map<String, Value>) -> Result<Value, Error> {
        let mid = py_str(arg(a, "messageId")?);
        let msg = self.get(&format!("{GMAIL}/messages/{mid}"), params(&[("format", json!("full"))])).await?;
        let listed = attachments(&or_value(get(&msg, "payload")?, json!({})));
        let mut meta = None;
        for x in &listed {
            if x["attachmentId"] == *arg(a, "attachmentId")? {
                meta = Some(x.clone());
                break;
            }
        }
        let meta = meta.unwrap_or_else(|| json!({"filename": or_value(aget(a, "filename"), json!("")), "mimeType": ""}));
        let aid = py_str(arg(a, "attachmentId")?);
        let body = self.get(&format!("{GMAIL}/messages/{mid}/attachments/{aid}"), vec![]).await?;
        let data = decode_b64(&json!(py_str(&or_value(get(&body, "data")?, json!("")))));
        let mut mime = py_str(&or_value(&meta["mimeType"], json!("")));
        let filename = meta.get("filename").cloned().unwrap_or(json!(""));
        if mime == "application/octet-stream" && py_str(&filename).to_lowercase().ends_with(".pdf") {
            mime = "application/pdf".into();
        }
        match as_text(&data, &mime) {
            None => Ok(json!({"filename": filename, "mimeType": mime, "text": "",
                              "note": "this kind of attachment is not read as text here"})),
            Some(text) => {
                let (text, clipped) = clip_text(&text);
                Ok(json!({"filename": filename, "mimeType": mime, "text": text, "clipped": clipped}))
            }
        }
    }

    async fn gmail_list_labels(&self, _a: &Map<String, Value>) -> Result<Value, Error> {
        let reply = self.get(&format!("{GMAIL}/labels"), vec![]).await?;
        let labels = py_iter(get(&reply, "labels")?)?;
        let inbox = self.get(&format!("{GMAIL}/labels/INBOX"), vec![]).await?;
        let labels: Vec<Value> = labels
            .iter()
            .map(|x| json!({"id": nget(x, "id"), "name": nget(x, "name"), "type": nget(x, "type")}))
            .collect();
        Ok(json!({"inbox": {"total": get(&inbox, "threadsTotal")?, "unread": get(&inbox, "threadsUnread")?},
                  "labels": labels}))
    }

    /// A recipient list with each "me" made the owner's own address (Gmail's profile), asked only when used.
    async fn mine(&self, value: &Value) -> Result<Value, Error> {
        let items: Vec<Value> = match value {
            Value::String(_) => vec![value.clone()],
            Value::Array(a) => a.clone(),
            _ => return Ok(value.clone()),
        };
        let pieces: Vec<String> =
            items.iter().flat_map(|item| py_str(item).split(',').map(|p| strip(p).to_string()).collect::<Vec<_>>()).collect();
        if !pieces.iter().any(|p| p.to_lowercase() == "me") {
            return Ok(value.clone());
        }
        let profile = self.get(&format!("{GMAIL}/profile"), vec![]).await?;
        let own = py_str(&or_value(get(&profile, "emailAddress")?, json!("")));
        Ok(Value::Array(
            pieces
                .into_iter()
                .filter(|p| !p.is_empty())
                .map(|p| if p.to_lowercase() == "me" { json!(own) } else { json!(p) })
                .collect(),
        ))
    }

    async fn gmail_send_message(&self, a: &Map<String, Value>) -> Result<Value, Error> {
        let to = addresses(&self.mine(aget(a, "to")).await?)?;
        let cc = addresses(&self.mine(aget(a, "cc")).await?)?;
        let bcc = addresses(&self.mine(aget(a, "bcc")).await?)?;
        if to.is_empty() {
            return Err(Error::value("no recipient: to needs at least one email address"));
        }
        if to.len() + cc.len() + bcc.len() > RECIPIENTS_MAX {
            return Err(Error::value(format!("more than {RECIPIENTS_MAX} recipients")));
        }
        let body = py_str(&or_value(aget(a, "body"), json!("")));
        if strip(&body).is_empty() {
            return Err(Error::value("the email has no body"));
        }
        let mut subject = one_line(aget(a, "subject"))?;
        let mut send = Map::new();
        let mut reply: Vec<(String, String)> = Vec::new();
        if truthy(aget(a, "replyToMessageId")) {
            let orig = self
                .get(
                    &format!("{GMAIL}/messages/{}", py_str(&a["replyToMessageId"])),
                    params(&[
                        ("format", json!("metadata")),
                        ("metadataHeaders", json!(["Message-ID", "References", "Subject"])),
                    ]),
                )
                .await?;
            let mid = header(&orig, "Message-ID");
            let refs: Vec<String> = [header(&orig, "References"), mid.clone()].into_iter().filter(|x| !x.is_empty()).collect();
            reply = vec![("In-Reply-To".into(), mid), ("References".into(), refs.join(" "))];
            if truthy(get(&orig, "threadId")?) {
                send.insert("threadId".into(), orig["threadId"].clone());
            }
            if subject.is_empty() {
                let was = header(&orig, "Subject");
                subject = if was.to_lowercase().starts_with("re:") { was } else { strip(&format!("Re: {was}")).to_string() };
            }
        }
        send.insert("raw".into(), json!(build_mail(&to, &cc, &bcc, &subject, &body, &reply)?));
        let sent = self.req("POST", &format!("{GMAIL}/messages/send"), &vec![], Body::Json(Value::Object(send))).await?;
        Ok(json!({"sent": true, "id": get(&sent, "id")?, "threadId": get(&sent, "threadId")?, "to": to,
                  "subject": subject}))
    }

    // -- contacts --
    async fn contacts_search_contacts(&self, a: &Map<String, Value>) -> Result<Value, Error> {
        let query = strip(&py_str(&or_value(aget(a, "query"), json!("")))).to_string();
        if query.is_empty() {
            return Err(Error::value("query is empty"));
        }
        let size = py_int(&or_value(aget(a, "pageSize"), json!(CONTACTS_MAX)))?.min(30).max(1);
        if !self.contacts_warm.load(Ordering::SeqCst) {
            for (url, mask) in [
                (format!("{PEOPLE}/people:searchContacts"), CONTACT_FIELDS),
                (format!("{PEOPLE}/otherContacts:search"), OTHER_FIELDS),
            ] {
                match self.get(&url, params(&[("query", json!("")), ("readMask", json!(mask))])).await {
                    Ok(_) | Err(Error::Google(_)) => {} // the real search below says what is wrong
                    Err(e) => return Err(e),
                }
            }
            self.contacts_warm.store(true, Ordering::SeqCst);
        }
        let saved = self
            .get(
                &format!("{PEOPLE}/people:searchContacts"),
                params(&[("query", json!(query)), ("readMask", json!(CONTACT_FIELDS)), ("pageSize", json!(size))]),
            )
            .await?;
        let mut people: Vec<Value> = py_iter(get(&saved, "results")?)?
            .iter()
            .map(|r| person(&or_value(nget(r, "person"), json!({}))))
            .collect();
        let other = match self
            .get(
                &format!("{PEOPLE}/otherContacts:search"),
                params(&[("query", json!(query)), ("readMask", json!(OTHER_FIELDS)), ("pageSize", json!(size))]),
            )
            .await
        {
            Ok(v) => v,
            Err(Error::Google(_)) => json!({}), // saved contacts still answer without the other scope
            Err(e) => return Err(e),
        };
        let emails = |p: &Value| -> Vec<String> {
            p.get("emails").and_then(Value::as_array).map(|l| l.iter().map(py_str).collect()).unwrap_or_default()
        };
        let seen: Vec<String> = people.iter().flat_map(|p| emails(p)).map(|e| e.to_lowercase()).collect();
        for r in py_iter(get(&other, "results")?)? {
            let mut p = person(&or_value(nget(&r, "person"), json!({})));
            if !emails(&p).iter().any(|e| seen.contains(&e.to_lowercase())) {
                p["saved"] = json!(false);
                people.push(p);
            }
        }
        people.truncate(size as usize);
        Ok(json!({"people": people}))
    }

    // -- calendar --
    /// `_cal`: the events URL of the call's calendar (the primary when none is named).
    pub fn cal(a: &Map<String, Value>) -> String {
        let id = or_value(aget(a, "calendarId"), json!("primary"));
        format!("{CALENDAR}/calendars/{}/events", quote(&py_str(&id)))
    }

    fn event_url(a: &Map<String, Value>) -> Result<String, Error> {
        Ok(format!("{}/{}", Self::cal(a), quote(&py_str(arg(a, "eventId")?))))
    }

    async fn calendar_list_calendars(&self, a: &Map<String, Value>) -> Result<Value, Error> {
        let reply = self.get(&format!("{CALENDAR}/users/me/calendarList"), Self::page(a, 50, 250)?).await?;
        let items = py_iter(get(&reply, "items")?)?;
        let calendars: Vec<Value> = items
            .iter()
            .map(|c| {
                json!({"id": nget(c, "id"), "summary": nget(c, "summary"), "primary": truthy(nget(c, "primary")),
                       "accessRole": nget(c, "accessRole")})
            })
            .collect();
        Ok(json!({"calendars": calendars}))
    }

    /// One calendar when calendarId is given; else every calendar the owner sees but the date-only ones,
    /// merged soonest first (review, 2026-10-05: primary alone missed a subscribed class calendar, and reading
    /// each calendar was a model round each, so a rundown with five calendars ran out of rounds). A calendar
    /// that fails is named in `unreadable`; the others' events still come back.
    async fn calendar_list_events(&self, a: &Map<String, Value>) -> Result<Value, Error> {
        if truthy(aget(a, "calendarId")) {
            return self.list_one_calendar(a).await;
        }
        let mut probe = Map::new();
        probe.insert("pageSize".into(), json!(50));
        // a dropped list still reads the primary (Lean CalendarEdits P4)
        let mut cals: Vec<Value> = match self.calendar_list_calendars(&probe).await {
            Ok(v) => v["calendars"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|c| truthy(nget(c, "id")) && !date_only(&py_str(&c["id"])))
                .collect(),
            Err(Error::Google(_) | Error::Http(_)) => vec![],
            Err(e) => return Err(e),
        };
        if cals.is_empty() {
            return self.list_one_calendar(a).await;
        }
        cals.sort_by_key(|c| !truthy(nget(c, "primary"))); // the primary first, so a cap never drops it
        let read = &cals[..cals.len().min(CALENDARS_MAX)];
        let mut events: Vec<(DateTime<FixedOffset>, Value)> = Vec::new();
        let mut unreadable: Vec<String> = Vec::new();
        let (mut more, mut tz, mut failure) = (cals.len() > CALENDARS_MAX, Value::Null, None);
        for cal in read {
            let name = py_str(&or_value(nget(cal, "summary"), cal["id"].clone()));
            let mut one = a.clone();
            one.insert("calendarId".into(), cal["id"].clone());
            one.insert("pageToken".into(), Value::Null);
            let got = match self.list_one_calendar(&one).await {
                Ok(g) => g,
                Err(e @ (Error::Google(_) | Error::Http(_))) => {
                    unreadable.push(name);
                    failure = Some(e);
                    continue;
                }
                Err(e) => return Err(e),
            };
            if !truthy(&tz) {
                tz = got["timeZone"].clone();
            }
            more = more || truthy(&got["nextPageToken"]);
            for ev in got["events"].as_array().cloned().unwrap_or_default() {
                if let Value::Object(mut m) = ev {
                    let key = start_key(&Value::Object(m.clone()), &got["timeZone"], self.zone);
                    m.insert("calendar".into(), json!(name));
                    events.push((key, Value::Object(m)));
                }
            }
        }
        if let Some(f) = failure {
            if unreadable.len() == read.len() {
                return Err(f);
            }
        }
        events.sort_by(|x, y| x.0.cmp(&y.0));
        let page = py_int(&or_value(aget(a, "pageSize"), json!(50)))?.min(250).max(1) as usize;
        let total = events.len();
        let mut out = Map::new();
        out.insert("timeZone".into(), tz);
        out.insert("events".into(), Value::Array(events.into_iter().take(page).map(|(_, e)| e).collect()));
        out.insert("calendarsRead".into(), json!(read.len() - unreadable.len()));
        if more || total > page {
            out.insert("more".into(), json!("there are more events: narrow startTime/endTime, or read one calendarId"));
        }
        if !unreadable.is_empty() {
            out.insert("unreadable".into(), json!(unreadable));
        }
        Ok(Value::Object(out))
    }

    async fn list_one_calendar(&self, a: &Map<String, Value>) -> Result<Value, Error> {
        let start = if truthy(aget(a, "startTime")) {
            rfc3339_in(&a["startTime"], self.zone)
        } else {
            isoformat(&pytime::now_utc())
        };
        let end = if truthy(aget(a, "endTime")) {
            Some(rfc3339_in(&a["endTime"], self.zone))
        } else {
            match pytime::fromisoformat(&start.replace('Z', "+00:00")) {
                Ok(PyDateTime::Aware(dt)) => Some(isoformat(&pytime::add(&dt, Duration::days(7))?)),
                Ok(PyDateTime::Naive(n)) => {
                    let later = n.checked_add_signed(Duration::days(7)).filter(|d| chrono::Datelike::year(d) <= 9999);
                    let later = later.ok_or_else(|| Error::Overflow("date value out of range".into()))?;
                    Some(PyDateTime::Naive(later).isoformat())
                }
                Err(Error::Value { .. }) => None,
                Err(e) => return Err(e),
            }
        };
        let mut query = params(&[("timeMin", json!(start)), ("singleEvents", json!(true)), ("orderBy", json!("startTime"))]);
        query.extend(Self::page(a, 50, 250)?);
        if let Some(end) = end.filter(|e| !e.is_empty()) {
            query.push(("timeMax".into(), json!(end)));
        }
        if truthy(aget(a, "fullText")) {
            query.push(("q".into(), a["fullText"].clone()));
        }
        let body = self.get(&Self::cal(a), query).await?;
        Ok(json!({"timeZone": get(&body, "timeZone")?, "events": events(get(&body, "items")?)?,
                  "nextPageToken": get(&body, "nextPageToken")?}))
    }

    async fn calendar_search_events(&self, a: &Map<String, Value>) -> Result<Value, Error> {
        let since = isoformat(&pytime::add(&pytime::now_utc(), Duration::days(-SEARCH_BACK_DAYS))?);
        let mut query = params(&[
            ("q", or_value(aget(a, "query"), json!(""))),
            ("singleEvents", json!(true)),
            ("orderBy", json!("startTime")),
            ("timeMin", json!(since)),
        ]);
        query.extend(Self::page(a, 25, 250)?);
        let body = self.get(&Self::cal(&Map::new()), query).await?;
        Ok(json!({"events": events(get(&body, "items")?)?, "nextPageToken": get(&body, "nextPageToken")?}))
    }

    async fn calendar_get_event(&self, a: &Map<String, Value>) -> Result<Value, Error> {
        Ok(compact_event(&self.get(&Self::event_url(a)?, vec![]).await?))
    }

    async fn calendar_suggest_time(&self, a: &Map<String, Value>) -> Result<Value, Error> {
        let start = pytime::fromisoformat(&rfc3339_in(arg(a, "startTime")?, self.zone))?.aware_or_local(self.zone);
        let end = pytime::fromisoformat(&rfc3339_in(arg(a, "endTime")?, self.zone))?.aware_or_local(self.zone);
        let mut probe = Map::new();
        probe.insert("pageSize".into(), json!(50));
        let cals = self.calendar_list_calendars(&probe).await?["calendars"].as_array().cloned().unwrap_or_default();
        // Every calendar the owner sees counts as busy time (a subscribed class or team calendar is time they
        // are not free), except the ones that are only dates: holidays, birthdays, week numbers.
        let mut ids: Vec<Value> = cals
            .iter()
            .filter(|c| !date_only(&py_str(&or_value(nget(c, "id"), json!("")))))
            .map(|c| c["id"].clone())
            .collect();
        if ids.is_empty() {
            ids.push(json!("primary"));
        }
        let items: Vec<Value> = ids.iter().take(50).map(|i| json!({"id": i})).collect();
        let body = self
            .req(
                "POST",
                &format!("{CALENDAR}/freeBusy"),
                &vec![],
                Body::Json(json!({"timeMin": isoformat(&start), "timeMax": isoformat(&end), "items": items})),
            )
            .await?;
        let tz = *start.offset();
        let mut busy = Vec::new();
        let calendars = or_value(get(&body, "calendars")?, json!({}));
        for cal in calendars.as_object().map(|m| m.values().cloned().collect::<Vec<_>>()).unwrap_or_default() {
            for b in nlist(nget(&cal, "busy")) {
                let Value::Object(b) = b else { continue };
                let read = |k: &str| -> Result<DateTime<FixedOffset>, Error> {
                    let v = b.get(k).ok_or_else(|| Error::Key(k.to_string()))?;
                    let Value::String(s) = v else {
                        return Err(Error::Attribute(format!("'{}' object has no attribute 'replace'", type_name(v))));
                    };
                    Ok(pytime::fromisoformat(&s.replace('Z', "+00:00"))?.astimezone_to(self.zone, tz))
                };
                busy.push((read("start")?, read("end")?));
            }
        }
        // `is None`, not `or`: an explicit 0 is midnight, not "unset"
        let duration = py_int(arg(a, "durationMinutes")?)?;
        let day_start = match aget(a, "workdayStartHour") {
            Value::Null => 9,
            v => py_int(v)?,
        };
        let day_end = match aget(a, "workdayEndHour") {
            Value::Null => 18,
            v => py_int(v)?,
        };
        let slots = free_slots(&busy, start, end, duration, day_start, day_end, self.zone)?;
        Ok(json!({"free": slots, "calendarsChecked": ids.len()}))
    }

    /// `_event_body`: the event fields of a create or an update.
    pub fn event_body(&self, a: &Map<String, Value>) -> Result<Map<String, Value>, Error> {
        let all_day = truthy(aget(a, "allDay"));
        let tz = aget(a, "timeZone");
        let mut body = Map::new();
        for k in ["summary", "description", "location"] {
            if let Some(v) = a.get(k).filter(|v| !v.is_null()) {
                body.insert(k.into(), v.clone());
            }
        }
        for (key, field) in [("startTime", "start"), ("endTime", "end")] {
            if let Some(w) = when(aget(a, key), all_day, tz, self.zone) {
                body.insert(field.into(), w);
            }
        }
        if truthy(aget(a, "attendeeEmails")) {
            let guests: Vec<Value> = py_iter(&a["attendeeEmails"])?.into_iter().map(|e| json!({"email": e})).collect();
            body.insert("attendees".into(), Value::Array(guests));
        }
        if truthy(aget(a, "recurrence")) {
            body.insert("recurrence".into(), Value::Array(py_iter(&a["recurrence"])?));
            for field in ["start", "end"] {
                // a recurring event needs its zone (events.insert)
                if let Some(Value::Object(f)) = body.get_mut(field) {
                    if f.get("dateTime").is_some_and(truthy) && !truthy(tz) {
                        f.insert("timeZone".into(), json!(self.zone.iana_name()));
                    }
                }
            }
        }
        Ok(body)
    }

    async fn calendar_create_event(&self, a: &Map<String, Value>) -> Result<Value, Error> {
        let mut body = self.event_body(a)?;
        for field in ["start", "end"] {
            // nothing to clear on a new event
            if let Some(Value::Object(f)) = body.get_mut(field) {
                let drop = if f.get("dateTime").is_some_and(truthy) { "date" } else { "dateTime" };
                f.shift_remove(drop);
            }
        }
        let mut query = vec![("sendUpdates".to_string(), json!(send_updates(a, truthy(aget(a, "attendeeEmails")))))];
        if truthy(aget(a, "addGoogleMeetUrl")) {
            body.insert(
                "conferenceData".into(),
                json!({"createRequest": {"requestId": token_hex(8), "conferenceSolutionKey": {"type": "hangoutsMeet"}}}),
            );
            query.push(("conferenceDataVersion".into(), json!(1)));
        }
        Ok(compact_event(&self.req("POST", &Self::cal(a), &query, Body::Json(Value::Object(body))).await?))
    }

    async fn calendar_quick_add_event(&self, a: &Map<String, Value>) -> Result<Value, Error> {
        let query = params(&[("text", arg(a, "text")?.clone()), ("sendUpdates", json!("none"))]);
        Ok(compact_event(&self.req("POST", &format!("{}/quickAdd", Self::cal(a)), &query, Body::None).await?))
    }

    async fn calendar_update_event(&self, a: &Map<String, Value>) -> Result<Value, Error> {
        let url = Self::event_url(a)?;
        let mut rest = a.clone();
        rest.shift_remove("attendeeEmails");
        let mut body = self.event_body(&rest)?;
        let mut guests: Vec<Value> = Vec::new();
        if truthy(aget(a, "addAttendeeEmails")) || aget(a, "notifyGuests").is_null() {
            let event = self.get(&url, vec![]).await?;
            guests = py_iter(get(&event, "attendees")?)?;
        }
        if truthy(aget(a, "addAttendeeEmails")) {
            let known: Vec<Value> = guests.iter().map(|g| nget(g, "email").clone()).collect();
            let mut all = guests.clone();
            for e in py_iter(&a["addAttendeeEmails"])? {
                if !known.contains(&e) {
                    all.push(json!({"email": e}));
                }
            }
            body.insert("attendees".into(), Value::Array(all));
        }
        let has = !guests.is_empty() || body.get("attendees").is_some_and(truthy);
        let query = vec![("sendUpdates".to_string(), json!(send_updates(a, has)))];
        Ok(compact_event(&self.req("PATCH", &url, &query, Body::Json(Value::Object(body))).await?))
    }

    async fn calendar_respond_to_event(&self, a: &Map<String, Value>) -> Result<Value, Error> {
        let url = Self::event_url(a)?;
        let event = self.get(&url, vec![]).await?;
        let mut guests = py_iter(get(&event, "attendees")?)?;
        if !guests.iter().any(|g| truthy(nget(g, "self"))) {
            return Err(Error::Google("the owner is not a guest of this event".into()));
        }
        for g in guests.iter_mut() {
            if truthy(nget(g, "self")) {
                if let Value::Object(m) = g {
                    m.insert("responseStatus".into(), arg(a, "responseStatus")?.clone());
                }
            }
        }
        // attendees is an array, so a PATCH replaces it whole: every guest is sent back as it was
        let query = vec![("sendUpdates".to_string(), json!(send_updates(a, true)))];
        let reply = self.req("PATCH", &url, &query, Body::Json(json!({"attendees": guests}))).await?;
        Ok(compact_event(&reply))
    }

    async fn calendar_delete_event(&self, a: &Map<String, Value>) -> Result<Value, Error> {
        let url = Self::event_url(a)?;
        let guests = if aget(a, "notifyGuests").is_null() {
            let event = self.get(&url, vec![]).await?;
            py_iter(get(&event, "attendees")?)?
        } else {
            vec![]
        };
        let query = vec![("sendUpdates".to_string(), json!(send_updates(a, !guests.is_empty())))];
        self.req("DELETE", &url, &query, Body::None).await?;
        Ok(json!({"deleted": a["eventId"]}))
    }

    // -- drive --
    async fn files(&self, mut query: Params, a: &Map<String, Value>) -> Result<Value, Error> {
        let mut page = Self::page(a, 10, 250)?;
        let size = page.remove(0).1;
        query.push(("pageSize".into(), size));
        query.push(("fields".into(), json!(format!("nextPageToken,files({FILE_FIELDS})"))));
        query.extend(page);
        let body = self.get(&format!("{DRIVE}/files"), query).await?;
        Ok(json!({"files": or_value(get(&body, "files")?, json!([])), "nextPageToken": get(&body, "nextPageToken")?}))
    }

    async fn drive_search_files(&self, a: &Map<String, Value>) -> Result<Value, Error> {
        let mut q = strip(&py_str(&or_value(aget(a, "query"), json!("")))).to_string();
        if ![" contains ", " = ", " in parents", "mimeType", "modifiedTime"].iter().any(|op| q.contains(op)) {
            q = format!("fullText contains '{}'", q.replace('\\', "\\\\").replace('\'', "\\'"));
        }
        self.files(params(&[("q", json!(format!("{q} and trashed = false")))]), a).await
    }

    async fn drive_list_recent_files(&self, a: &Map<String, Value>) -> Result<Value, Error> {
        self.files(params(&[("orderBy", json!("modifiedTime desc")), ("q", json!("trashed = false"))]), a).await
    }

    async fn drive_get_file_metadata(&self, a: &Map<String, Value>) -> Result<Value, Error> {
        let id = py_str(arg(a, "fileId")?);
        self.get(&format!("{DRIVE}/files/{id}"), params(&[("fields", json!(FILE_FIELDS))])).await
    }

    async fn drive_read_file_content(&self, a: &Map<String, Value>) -> Result<Value, Error> {
        let meta = self.drive_get_file_metadata(a).await?;
        let mime = py_str(&or_value(get(&meta, "mimeType")?, json!("")));
        let id = py_str(arg(a, "fileId")?);
        let text = if let Some(export) = export_for(&mime) {
            let data = self.download(&format!("{DRIVE}/files/{id}/export"), &params(&[("mimeType", json!(export))])).await?;
            Some(py::decode_utf8(&data))
        } else if mime == "application/pdf" || mime.starts_with("text/") || TEXT_TYPES.contains(&mime.as_str()) {
            let data = self.download(&format!("{DRIVE}/files/{id}"), &params(&[("alt", json!("media"))])).await?;
            as_text(&data, &mime)
        } else {
            None
        };
        match text {
            None => Ok(json!({"name": get(&meta, "name")?, "mimeType": mime, "text": "",
                              "link": get(&meta, "webViewLink")?,
                              "note": "this kind of file is not read as text here; open its link"})),
            Some(text) => {
                let (text, clipped) = clip_text(&text);
                Ok(json!({"name": get(&meta, "name")?, "mimeType": mime, "text": text, "clipped": clipped}))
            }
        }
    }

    async fn drive_create_file(&self, a: &Map<String, Value>) -> Result<Value, Error> {
        let mut meta = Map::new();
        meta.insert("name".into(), or_value(aget(a, "title"), json!("Untitled")));
        if truthy(aget(a, "mimeType")) {
            meta.insert("mimeType".into(), a["mimeType"].clone());
        }
        let boundary = format!("eve-upload-{}", token_hex(8));
        let text = py_str(&or_value(aget(a, "textContent"), json!("")));
        let body = format!(
            "--{boundary}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n{}\r\n\
             --{boundary}\r\nContent-Type: text/plain; charset=UTF-8\r\n\r\n{text}\r\n--{boundary}--",
            py_json_dumps(&Value::Object(meta))
        );
        let query = params(&[("uploadType", json!("multipart")), ("fields", json!(FILE_FIELDS))]);
        let content_type = format!("multipart/related; boundary={boundary}");
        self.req("POST", DRIVE_UPLOAD, &query, Body::Raw { content: body.into_bytes(), content_type }).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_dumps_is_pythons() {
        assert_eq!(py_json_dumps(&json!({"name": "Café \"x\"", "n": [1, 2.0, null, true]})),
                   "{\"name\": \"Caf\\u00e9 \\\"x\\\"\", \"n\": [1, 2.0, null, true]}");
        assert_eq!(py_json_dumps(&json!("😀")), "\"\\ud83d\\ude00\"");
    }

    #[test]
    fn query_values_are_written_as_httpx_writes_them() {
        let q = params(&[("q", json!("a b*~:é")), ("b", json!(true)), ("n", Value::Null), ("l", json!(["From", "Subject"]))]);
        assert_eq!(with_query("https://x/y", &q), "https://x/y?q=a+b%2A~%3A%C3%A9&b=true&n=&l=From&l=Subject");
    }

    #[test]
    fn a_google_error_reads_googles_message_or_the_body() {
        assert_eq!(http_error(403, br#"{"error": {"message": "no"}}"#).to_string(), "HTTP 403: no");
        assert_eq!(http_error(500, b"oops").to_string(), "HTTP 500: oops");
        assert_eq!(http_error(400, br#"{"error": "bad"}"#).to_string(), "HTTP 400: {\"error\": \"bad\"}");
        assert_eq!(http_error(404, br#"{"error": {}}"#).to_string(), "HTTP 404: {\"error\": {}}");
    }
}
