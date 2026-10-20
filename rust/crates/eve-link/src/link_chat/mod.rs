//! Eve Link's chat: the owner texts eve in the iPhone app instead of Telegram (docs/action-button.md). A port of
//! `link_chat.py`.
//!
//! Owner, 2026-10-08: the chat moves from Telegram into Eve Link. The app draws the conversation with OpenUI in a web
//! view and does all its networking natively (ios/EveLink); this module is the daemon's half, on `/api/link/*` beside
//! `/api/stick` on the Mini App's server (miniapp.py), reached through its Cloudflare tunnel. The routes, who may
//! call, where an app turn's answer goes and the generative UI rules are in `link_chat.py`'s module docstring, which
//! is the spec; [`door::LinkChat`] serves them.
//!
//! This file holds what the door and the other modules share: the paths and limits, the OpenUI guide
//! ([`app_block`], [`strip_openui`], [`history_text`]), the conversation read back from the transcript
//! ([`row`], [`read_messages`], [`read_before`], [`split_cursor`]), one turn's SSE framing ([`sse`]) and the chief's
//! cards for the Work view ([`list_jobs`], [`HiddenJobs`]).
//!
//! Logs carry counts and ids, never what was said.

pub mod door;

pub use door::LinkChat;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use indexmap::IndexMap;
use regex::Regex;
use serde_json::{Map, Value};

use eve_link_uploads::Upload;
use eve_transcripts::{Line, Transcripts};

use crate::seams::{Chief, DynWrite, Headers, Hub, JobCard, Obj};

pub const TURN_PATH: &str = "/api/link/turn";
pub const MESSAGES_PATH: &str = "/api/link/messages";
pub const JOBS_PATH: &str = "/api/link/jobs";
pub const APPS_PATH: &str = "/api/link/apps";
pub const UPLOAD_PATH: &str = "/api/link/upload";
pub static JOB_ACTION_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^/api/link/jobs/(c\d{1,6})/([a-z]{1,16})\n?\z").expect("JOB_ACTION_RE"));
pub static APP_OPEN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^/api/link/apps/([A-Za-z0-9_-]{1,80})/open\n?\z").expect("APP_OPEN_RE"));
pub const CONTROLS_PATH: &str = "/api/link/controls";
/// an app's slot (app_registry.name_problem)
pub static CONTROL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^/api/link/controls/([a-z][a-z0-9]{0,15})\n?\z").expect("CONTROL_RE"));
/// The Mini App server's attributes, each with `panel(body)`: eve's apps (app_registry.APPS) and eve's own Memory and
/// Notes panels.
pub static CONTROLS: LazyLock<Vec<String>> = LazyLock::new(|| {
    let mut out: Vec<String> =
        eve_app_registry::APPS.iter().filter(|a| a.controls).map(|a| a.slot.clone()).collect();
    out.push("memory".to_string());
    out.push("notes".to_string());
    out
});
/// GET /api/link/controls/<slot>/stream for an app with a picture (pika.py: the car's video; obsbot_panel.py: the
/// webcam's), one viewer: path → slot.
pub static STREAM_PATHS: LazyLock<IndexMap<String, String>> = LazyLock::new(|| {
    eve_app_registry::APPS
        .iter()
        .filter(|a| a.stream)
        .map(|a| (format!("{CONTROLS_PATH}/{}/stream", a.slot), a.slot.clone()))
        .collect()
});
pub const PIKA_STREAM_PATH: &str = "/api/link/controls/pika/stream";
pub const OBSBOT_STREAM_PATH: &str = "/api/link/controls/obsbot/stream";
/// link_today.py: the home-screen widget's one read
pub const TODAY_PATH: &str = "/api/link/today";
/// updates.py: the Updates pane's cards
pub const UPDATES_PATH: &str = "/api/link/updates";
pub const UPDATES_DISMISS_PATH: &str = "/api/link/updates/dismiss";
pub const UPDATES_REFRESH_PATH: &str = "/api/link/updates/refresh";
/// link_topics.py: Horizontal History's cards
pub const TOPICS_PATH: &str = "/api/link/topics";
pub const TOPICS_BOOKMARK_PATH: &str = "/api/link/topics/bookmark";
/// phone_location.py: the phone's latest fix
pub const LOCATION_PATH: &str = "/api/link/location";
/// places.py: iOS's visits
pub const VISIT_PATH: &str = "/api/link/visit";
/// places.py: the Settings row's counts
pub const PLACES_PATH: &str = "/api/link/places";
pub const PLACES_FORGET_PATH: &str = "/api/link/places/forget";
/// work_sessions.py: the Work tab's Claude Code sessions
pub const SESSIONS_PATH: &str = "/api/link/sessions";
pub const SESSION_START_PATH: &str = "/api/link/sessions/start";
/// the composer's eve/Muse switch: GET {"available", "on"}; POST {"on"}
pub const MUSE_PATH: &str = "/api/link/muse";
pub static SESSION_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^/api/link/sessions/([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})(?:/(reply|stop))?\n?\z",
    )
    .expect("SESSION_RE")
});
/// one kept upload's bytes, full size; anything after the prefix that is not 32 hex is a 404 (never a path)
pub static FILE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^/api/link/file/([^/]{0,64})\n?\z").expect("FILE_RE"));
/// Every fixed path the door answers, in Python's order (`PATHS`).
pub static PATHS: LazyLock<Vec<String>> = LazyLock::new(|| {
    let mut out: Vec<String> =
        [TURN_PATH, MESSAGES_PATH, JOBS_PATH, APPS_PATH, UPLOAD_PATH, CONTROLS_PATH, TODAY_PATH]
            .iter()
            .map(|s| s.to_string())
            .collect();
    out.extend(STREAM_PATHS.keys().cloned());
    out.extend(
        [
            UPDATES_PATH,
            UPDATES_DISMISS_PATH,
            UPDATES_REFRESH_PATH,
            LOCATION_PATH,
            SESSIONS_PATH,
            SESSION_START_PATH,
            TOPICS_PATH,
            TOPICS_BOOKMARK_PATH,
            MUSE_PATH,
            VISIT_PATH,
            PLACES_PATH,
            PLACES_FORGET_PATH,
        ]
        .iter()
        .map(|s| s.to_string()),
    );
    out
});
pub const POST_PATHS: [&str; 9] = [
    TURN_PATH,
    UPLOAD_PATH,
    UPDATES_DISMISS_PATH,
    UPDATES_REFRESH_PATH,
    LOCATION_PATH,
    SESSION_START_PATH,
    TOPICS_BOOKMARK_PATH,
    VISIT_PATH,
    PLACES_FORGET_PATH,
];
/// a dismissed Updates card's id (updates.card_id: 12 hex)
pub const MAX_CARD_ID_CHARS: usize = 64;
/// telegram.GRADES: the speed dial
pub const GRADES: [&str; 3] = ["auto", "instant", "think"];
pub const JOB_ACTIONS: [&str; 7] = ["go", "done", "drop", "raise", "happened", "didnt", "hide"];
/// the finished cards the owner removed from Work
pub const HIDDEN_JOBS_FILE: &str = "~/.config/eve-bridge/chat/hidden-jobs.json";
/// finished cards listed after the open ones
pub const FINISHED_SHOWN: usize = 20;
/// a New job's goal, after trimming
pub const MAX_GOAL_CHARS: usize = 1000;
/// and its details
pub const MAX_DETAIL_CHARS: usize = 2000;
/// What a New job's words are led by when heard: the Work tab's button said it, so the brain hears it. No word in it
/// is consequential (chief_card.verbs), so it never grounds a one-way step the owner did not name (test_link_chat).
pub const JOB_LEAD: &str = "Take this on as a job: ";
pub const JOB_DETAIL_LEAD: &str = "Details: ";
/// the longest a New job waits for its turn (the app waits 120 s)
pub const JOB_SECS: f64 = 90.0;
/// once the card is filed, how long the turn gets to finish its backbrief
pub const JOB_GRACE_SECS: f64 = 5.0;
/// one message from the app, after trimming
pub const MAX_TEXT_CHARS: usize = 4000;
pub const MAX_CLIENT_ID_CHARS: usize = 64;
/// attachments on one turn
pub const MAX_ATTACHMENTS: usize = 4;
/// Photos and PDFs go to the model inside the request (base64, a third larger), so their sum on one turn is capped:
/// past it the turn is refused with a line the composer shows, rather than the model call failing (review,
/// 2026-10-08).
pub const MAX_INLINE_BYTES: u64 = 20 * 1024 * 1024;
pub const INLINE_KINDS: [&str; 2] = ["image", "pdf"];
/// to read an upload's body (20 MB through the tunnel on a phone's uplink)
pub const UPLOAD_SECS: f64 = 180.0;
/// 4000 characters of any script, JSON-escaped, fit with room to spare
pub const MAX_BODY_BYTES: i64 = 64_000;
/// to read a request's body
pub const BODY_SECS: f64 = 30.0;
/// a turn's stream ends after this, done or not
pub const TURN_SECS: f64 = 300.0;
/// ": ping" this long into a quiet stream (the tunnel drops idle streams)
pub const PING_SECS: f64 = 15.0;
/// how often a stream checks whether the owner's turn is still in flight
pub const SETTLE_SECS: f64 = 0.2;
/// the same client_id within this is the same turn
pub const DEDUPE_SECS: f64 = 600.0;
/// remembered turns, for dedupe; the oldest finished ones go first
pub const MAX_KEPT_TURNS: usize = 256;
/// lines returned without a cursor
pub const HISTORY_LIMIT: usize = 100;
/// lines returned after a cursor; the next poll takes the rest
pub const PAGE_LIMIT: usize = 200;
/// the longest long poll (the tunnel and URLSession both outlast it)
pub const WAIT_MAX_SECS: f64 = 25.0;
/// on close, how long open streams get to write their end
pub const CLOSE_SETTLE_SECS: f64 = 0.5;
/// a long poll re-reads the files this often, woken or not
pub const POLL_SECS: f64 = 2.0;
/// a refused body read and dropped before the refusal, at most this much
pub const DISCARD_MAX_BYTES: i64 = 1024 * 1024;
/// and for at most this long
pub const DISCARD_SECS: f64 = 2.0;
pub const ROLES: [&str; 2] = ["owner", "eve"];
/// "media": a picture or file eve sent (TelegramInlet._send_media)
pub const KINDS: [&str; 4] = ["say", "tool", "image", "media"];
pub const VIAS: [&str; 4] = ["app", "telegram", "voice", "other"];
pub static CURSOR_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(\d{4}-\d{2}-\d{2}):(-1|\d{1,9})\n?\z").expect("CURSOR_RE"));
/// a cursor's "~<version>" (link_reactions.Reactions.version)
pub static REACTION_VERSION_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\d{1,16}\n?\z").expect("REACTION_VERSION_RE"));
/// a client_id that is also a good turn id
pub static TURN_ID_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_-]{8,64}\n?\z").expect("TURN_ID_RE"));

/// `REASONS.get(status, "OK")`: the status line's words.
pub fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        411 => "Length Required",
        413 => "Payload Too Large",
        415 => "Unsupported Media Type",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "OK",
    }
}

pub const NO_CHAT: &str = "eve's chat isn't running, or no owner has texted it yet";
pub const NO_CHIEF: &str = "eve's chief of staff is off on this Mac (EVE_CHIEF), so there are no jobs";
pub const NO_APPS: &str = "the app builder is off on this Mac, so there are no apps";
/// Each control's 404 line: the registry's apps, then eve's Memory and Notes.
pub static NO_CONTROL: LazyLock<IndexMap<String, String>> = LazyLock::new(|| {
    let mut out: IndexMap<String, String> =
        eve_app_registry::APPS.iter().map(|a| (a.slot.clone(), a.no_control.clone())).collect();
    out.insert("memory".to_string(), "eve's memory is off on this Mac (EVE_MEMORY).".to_string());
    out.insert("notes".to_string(), "Your notes are off on this Mac (EVE_SECOND_BRAIN).".to_string());
    out
});
pub const NO_MEMORY: &str = "eve's memory is off on this Mac (EVE_MEMORY), so there is no conversation to read";
pub const NO_LOCATION: &str = "phone location is off on this Mac (EVE_PHONE_LOCATION=0)";
pub const NO_PLACES: &str = "places are off on this Mac (EVE_PLACES=0 or EVE_PHONE_LOCATION=0)";
pub const NO_WORK: &str = "Work sessions are off on this Mac";

/// `Path(__file__).with_name("openui_prompt.txt")`: the openui-lang reference, where the Python package keeps it,
/// so both daemons read the same bytes.
pub const PROMPT_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../bridge/src/eve_bridge/openui_prompt.txt");

// What an app turn's instructions end with (app_block), in this order: APP_RULE, the openui-lang reference
// (PROMPT_PATH), APP_LINE. The rule is first because it decides whether the reference is used at all: with the rule as
// one sentence after 54,000 characters of signatures (until 2026-10-08), 1 of 24 app replies in a week used OpenUI.
// GPT-6 models follow format instructions literally and stall on conflicting ones (OpenAI's GPT-6 guide), so the rule
// is a plain two-way decision, the texting style that forbade lists is swapped out for app turns
// (telegram.APP_STYLE), and the example below is one small program that the app's own parser accepts
// (tests/test_openui_eval.py runs ios/EveLink/Web/scripts/validate.mjs on it). Every component it names is in the
// reference (ios/EveLink/Web/scripts/gen-prompt.mjs, CHAT_COMPONENTS).
pub const APP_EXAMPLE: &str = "Here's how they compare.
```openui-lang
root = Card([header, table, next])
header = CardHeader(\"Tea or coffee\", \"One cup\")
table = Table([Col(\"Drink\", [\"Tea\", \"Coffee\"]), Col(\"Caffeine (mg)\", [47, 95], \"number\")])
next = FollowUpBlock([f1, f2])
f1 = FollowUpItem(\"Which is better in the evening?\")
f2 = FollowUpItem(\"What about green tea?\")
```";
pub static APP_RULE: LazyLock<String> = LazyLock::new(|| {
    String::from(
        "## Replying in the Eve Link app\n\n\
         This message came from the Eve Link app on the owner's iPhone. The app shows plain text as Markdown and \
         draws an openui-lang block as native cards, tables, charts and buttons.\n\n\
         Before you write, decide the form of the reply:\n\
         - Write an openui-lang block when the answer has a shape the owner will scan: a comparison of two or more \
         things, options to pick from, steps to follow, a plan or schedule by time or day, numbers that make a chart, \
         articles, books or headlines from a tool, or a question the owner can answer with a tap.\n\
         - Write plain text, one to three short sentences, for everything else: chat, a greeting, thanks, feelings, a \
         single fact or number, a yes or no, a confirmation that you started something, or a question back to the \
         owner.\n\
         In the app, a table, a list of options or a set of steps goes in the openui-lang block rather than in \
         Markdown. When the owner asks how to do something or for a plan, give the usual answer in the block first; \
         if a detail would change it, ask about that in one short sentence after the block. You will see the block in \
         the next turn, so the owner's answer can change it.\n\n\
         The block: at most one short sentence of plain text before it, then the program in a fence that opens with \
         ```openui-lang and closes with ```. The program starts with root = Card(...) and stays small enough for a \
         phone screen. A tool's answer can be shown this way too: a list of events, results or prices is a table or \
         a list.\n\n\
         A complete reply with a block:\n",
    ) + APP_EXAMPLE
});
pub const APP_LINE: &str = "This turn is from the Eve Link app: use one openui-lang block when a comparison, options, steps, a \
schedule, a chart, articles or books from a tool, or tap choices make the answer easier to scan, and \
one to three short plain sentences otherwise.";

// ---- the generative UI prompt ----------------------------------------------------------------------------

/// The keys an Eve Link from before the registry reads (2026-10-12): the owner's five apps. An app added since is
/// listed only in `apps`.
pub const LEGACY_CONTROLS: [&str; 5] = ["lights", "spotify", "firetv", "pika", "obsbot"];

/// `GET /api/link/controls`: which of eve's controls this Mac has. `get(name)` is the Mini App server's attribute,
/// read now. The legacy keys keep their meaning for an installed app: `lights` is the lights' names and rooms
/// (`listing()`), every other app and eve's Memory and Notes a bool. `apps` (added 2026-10-12) is the phone's Apps
/// list from the registry (app_registry.for_phone): only the apps present here, each with its row's words and which
/// sheet opens it ("native" when the phone has Swift for it, else "generic"). An older phone reads only the keys it
/// knows.
pub fn controls_listing(get: &dyn Fn(&str) -> Option<std::sync::Arc<dyn Hub>>) -> Obj {
    let _ = get;
    todo!()
}

/// What an app turn's instructions end with: `APP_RULE` (when to use OpenUI, with one example), the openui-lang
/// reference (`PROMPT_PATH`, or `path`) and `APP_LINE`. The file is read once per change (mtime and size). An absent
/// or empty file means no addition at all, logged once: the brain then answers in plain Markdown, which the app also
/// shows, and is never told to write a language it has no reference for.
pub fn app_block(path: Option<&Path>) -> String {
    let _ = path;
    todo!()
}

/// The default reference's path (`PROMPT_PATH`).
pub fn prompt_path() -> PathBuf {
    PathBuf::from(PROMPT_PATH)
}

pub const OPENUI_PLACEHOLDER: &str = "(A table or chart is here: open Eve Link to see it.)";
/// ```` ```openui-lang[ \t]*\n.*?(?:\n```|\Z) ```` with DOTALL
pub static OPENUI_FENCE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)```openui-lang[ \t]*\n.*?(?:\n```|\z)").expect("OPENUI_FENCE"));
/// replyParts.ts ROOT_RE
pub static OPENUI_BARE_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:^|\n)[ \t]*root[ \t]*=[ \t]*[A-Z][A-Za-z0-9]*\(").expect("OPENUI_BARE_LINE"));

/// `text` with each OpenUI Lang program (a ```openui-lang fence, or a whole reply that is a bare `root =` program,
/// as the app's replyParts.ts reads them) swapped for one plain line. For an app turn's line that goes to Telegram
/// after all (nobody reading in the app), and for eve's lines in the history of a Telegram, voice or call turn
/// (`history_text`), so Telegram never shows a raw program and those turns never read past replies in OpenUI Lang
/// and copy the format. An unclosed fence runs to the end, as the app renders it.
pub fn strip_openui(text: &str) -> String {
    let _ = text;
    todo!()
}

/// An eve line the chat log holds from before app history kept programs (2026-10-08): the placeholder is all that is
/// left of its block. An app turn is shown this instead, since "open Eve Link" makes no sense to a turn in the app.
pub const OPENUI_GONE: &str = "(A card was shown here; what it held is no longer in this history.)";

/// One of eve's lines as the model's history shows it. The chat log keeps the reply whole, programs included. An app
/// turn (`app`) reads it as written, so a follow-up has the block's titles, rows and options to work from. A
/// Telegram, voice or call turn reads each program as `OPENUI_PLACEHOLDER` (`strip_openui`).
pub fn history_text(text: &str, app: bool) -> String {
    let _ = (text, app);
    todo!()
}

// ---- the conversation, read back from the transcript -------------------------------------------------------

/// A cursor or message id this door never made (Python's `ValueError("bad cursor")` / `"bad message id"`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct BadCursor(pub String);

/// One transcript line as the app's message (`{"id", "role", "text", "t", "via", "kind"}` plus `rid`,
/// `attachments` and `sources` when it has them), or None for a line the app does not show: the system's markers,
/// a command word, a line sent over the memory bus, and a forgotten line. "claude on" relays (`kind: "relay"`)
/// show the owner's relayed line as theirs and Claude's answers as eve's (`via: "other"`). Python's `_row`.
pub fn row(day: &str, i: usize, line: &Line) -> Option<Obj> {
    let _ = (day, i, line);
    todo!()
}

/// telegram._attached's note at the end of an owner line: "[image attached]", "[2 images, file a.pdf attached]".
pub static ATTACHED_NOTE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"\s*\[(?:\d+ images|image|file [^\[\]]+)(?:, (?:\d+ images|image|file [^\[\]]+))* attached\]\z",
    )
    .expect("ATTACHED_NOTE")
});

/// link_uploads.KINDS; "file" is eve's own only
pub const ATT_KINDS: [&str; 5] = ["image", "pdf", "text", "doc", "file"];

/// What the owner's transcript line keeps of a turn's attachments (link_uploads.Upload): id, name and kind.
pub fn att_record(uploads: &[Upload]) -> Vec<Value> {
    let _ = uploads;
    todo!()
}

/// A transcript line's `att`, as the app may show it: well-formed entries only, at most MAX_ATTACHMENTS. Python's
/// `_att`.
pub fn att(raw: Option<&Value>) -> Vec<Obj> {
    let _ = raw;
    todo!()
}

/// (messages oldest first, cursor). A message's id and the cursor are `<day>:<line index>`: a day's file is only ever
/// appended to (a forget rewrites a line in place), so a position stays the same line. The cursor is the last line
/// looked at, shown or not, so the next poll starts after it, across the 04:00 day boundary too. Without `after`:
/// the newest HISTORY_LIMIT shown lines, however far back. Blocking: call it on a blocking thread.
pub fn read_messages(store: &Transcripts, after: Option<&str>) -> Result<(Vec<Obj>, String), BadCursor> {
    let _ = (store, after);
    todo!()
}

/// (up to `limit` shown lines just before message `before`, oldest first; whether older ones exist). None: the
/// newest. Infinite scroll's older pages (`?before=<id>`). Nothing is ever pruned (transcripts.py), so this reaches
/// the first day. Blocking.
pub fn read_before(store: &Transcripts, before: Option<&str>, limit: usize) -> Result<(Vec<Obj>, bool), BadCursor> {
    let _ = (store, before, limit);
    todo!()
}

/// A cursor as the app sends it back → (the transcript position, the reaction version it saw or None). An error
/// for one this door never made.
pub fn split_cursor(after: Option<&str>) -> Result<(Option<String>, Option<i64>), BadCursor> {
    let _ = after;
    todo!()
}

/// The request's Content-Length, -1 when it is not a number (`int(headers.get("content-length") or 0)`).
pub fn length(headers: &Headers) -> i64 {
    let _ = headers;
    todo!()
}

/// An app turn's id: the app's `client_id` when it is a plain id not in use (`taken`), else a random one
/// (`secrets.token_hex(8)`).
pub fn turn_id(client_id: &str, taken: &dyn Fn(&str) -> bool) -> String {
    let _ = (client_id, taken);
    todo!()
}

// ---- one turn's stream ------------------------------------------------------------------------------------

/// One OpenAI Chat Completions streaming chunk carrying `content`, framed as one SSE event:
/// `data: {"choices": [{"index": 0, "delta": {"content": ...}}]}\n\n` with Python's `json.dumps` spacing.
pub fn sse(content: &str) -> Vec<u8> {
    let _ = content;
    todo!()
}

pub const DONE: &[u8] = b"data: [DONE]\n\n";
pub const PING: &[u8] = b": ping\n\n";
pub const STREAM_HEAD: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-store\r\n\
X-Accel-Buffering: no\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n";

/// One HTTP/1.1 chunk (the stream's length is not known up front), flushed now: the tunnel passes each on.
pub async fn chunk(writer: &mut DynWrite, data: &[u8]) -> std::io::Result<()> {
    let _ = (writer, data);
    todo!()
}

// ---- the chief's cards, for the Work view ------------------------------------------------------------------

/// A running step's line, by what it does.
pub const RUNNING: [(&str, &str); 4] =
    [("research", "Researching"), ("assess", "Weighing the options"), ("act", "Doing it"), ("watch", "Watching")];
/// A waiting card's line, by what it waits for.
pub const WAITING: [(&str, &str); 4] = [
    ("go", "Waiting on your Go"),
    ("budget", "Waiting on a budget raise"),
    ("question", "Waiting on your answer"),
    ("mac_slot", "Waiting for the Mac"),
];

/// A Go would approve something now (Chief.approve's own conditions): the first queued one-way step, every step
/// before it done, no yes on it yet. Python's `_go_ready`.
pub fn go_ready(card: &JobCard) -> bool {
    let _ = card;
    todo!()
}

/// The owner's move: a Go, a budget, an answer, a receipt not confirmed, or an act that may have happened.
pub fn job_needs_you(card: &JobCard) -> bool {
    let _ = card;
    todo!()
}

/// The subset of JOB_ACTIONS that does something to this card now, the way Chief.choices picks its buttons.
pub fn job_actions(card: &JobCard) -> Vec<String> {
    let _ = card;
    todo!()
}

/// One muted status line, composed by code: "Waiting on your Go", "Researching", "Done".
pub fn job_line(card: &JobCard) -> String {
    let _ = card;
    todo!()
}

/// One card for `GET /api/link/jobs`: `Chief._as_data` (`card.data`) and what the app needs to draw and act on it:
/// `rev`, `updated`, `line`, `steps`, `steps_done`, `needs_you`, `actions`.
pub fn job_entry(card: &JobCard) -> Obj {
    let _ = card;
    todo!()
}

/// Open cards first (the ones that need the owner first, then the newest), then the last `FINISHED_SHOWN` finished,
/// newest first, without the ones the owner removed (`hidden`); `needs_you` counts the cards that need the owner.
/// `{"jobs": [...], "needs_you": n}`.
pub fn list_jobs(chief: &dyn Chief, hidden: &BTreeSet<String>) -> Obj {
    let _ = (chief, hidden);
    todo!()
}

/// The finished cards' ids the owner removed from the Work list, kept in one small file (`{"hidden": [sorted ids]}`,
/// 0600, its folder 0700). Card ids are never reused (chief_card.Card.id), so an id alone names its card for good. A
/// file that cannot be read is an empty list: every finished card shows again, nothing is lost.
#[derive(Debug)]
pub struct HiddenJobs {
    pub path: PathBuf,
    pub ids: parking_lot::Mutex<BTreeSet<String>>,
}

impl HiddenJobs {
    /// Load `path` (`~` expanded; `HIDDEN_JOBS_FILE` by default).
    pub fn new(path: Option<&Path>) -> Self {
        let _ = path;
        todo!()
    }

    /// The ids now.
    pub fn ids(&self) -> BTreeSet<String> {
        self.ids.lock().clone()
    }

    /// Add `cid` and write the file (a `.tmp` beside it, 0600, then a rename).
    pub fn add(&self, cid: &str) -> std::io::Result<()> {
        let _ = cid;
        todo!()
    }
}

/// `json.dumps(value)`: Python's default separators (`", "` and `": "`), ensure_ascii on, keys in insertion order.
/// Every JSON body the door writes, and `sse`'s chunks, go through it.
pub fn py_json(value: &Value) -> String {
    eve_apps_mcp::py::dumps(value, true)
}

/// An empty JSON object, for callers that build one up.
pub fn obj() -> Obj {
    Map::new()
}
