//! Consequential-action approval and tracking of outside content (port of `tool_gate.py`).
//!
//! Reads and reversible work run without extra confirmation. [`needs_confirmation`] identifies native
//! consequential actions; app policies classify connected-app writes, and purchase/share executors own their
//! final exact-action approval. [`acting`] remains separate so this policy does not change provenance.
//!
//! A turn is tainted when its prompt carries outside text, attachments, or earlier tainted replies. Reads keep
//! that taint across subsequent rounds and persisted history. A consequential approval shows the complete
//! arguments or refuses unasked if they cannot fit. Voice approval requires the question to have been spoken
//! whole and binds one bare owner yes to the exact call, once. An owner's actual answer to a delegated task
//! question passes through without a second approval.
//!
//! Every name below is a tool eve offers somewhere; none is written into a prompt.

use std::collections::{BTreeSet, VecDeque};
use std::sync::LazyLock;

use eve_profile::{EnvMap, ProcessEnv, ProfileError};
use indexmap::IndexMap;
use parking_lot::Mutex;
use regex::Regex;
use serde_json::{json, Map, Value};

mod py;

pub use py::{py_str, py_truthy};

/// A tool call's arguments (Python's `Mapping[str, Any]`).
pub type Args = Map<String, Value>;

fn set(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|n| n.to_string()).collect()
}

/// Tools that change something, open a session or send something, whatever their arguments. The direct apps' and
/// Composio's writes are decided by their own policies (telegram._direct_tool, _app_tool), not listed here. eve's
/// apps (lights, music, the TV, the camera, ...) declare theirs in app_registry; every set below is eve's own
/// tools unioned with the apps' declarations.
pub static ACTING: LazyLock<BTreeSet<String>> = LazyLock::new(|| {
    let mut s = set(&[
        // the / menu's hand-offs (menu_tools.py): each sends a button, a link or a line, or flips a relay
        "claude_relay", "codex_relay", "set_stealth", "pair_eve_link",
        // the Mac and coding sessions: a task started, steered or stopped (a stop also drops a purchase's consent)
        "start_task", "steer_task", "stop_task", "start_coding_session", "screenshot", "take_photo", "send_file",
        "opentable_search", // a browser task in the owner's signed-in Chrome
        // the watcher, Meet, the room's notes, memory and the vault, the app maker, the chief
        "watch_add", "watch_remove", "watch_pause", "watch_resume", "watch_set_end",
        "meet_join", "meet_leave", "take_notes",
        "remember", "forget_apply", // a starred line is read by every later turn (the profile)
        "capture_note", "edit_note", "file_note", "undo_note",
        "make_app", "change_app", "delete_app", "rename_app", "restore_app", "undo_app",
        "take_on", "close_card",
        // buying
        "agentcard_buy", "agentcard_confirm",
        // the voice's own (voice_agent.TOOLS): the robot sent off, its sound, a task's question answered, Meet
        "go_explore", "set_sound", "answer_question", "join_meeting", "leave_meeting",
    ]);
    // the apps' acts: every Fire TV action changes the TV, every camera_control moves it
    s.extend(eve_app_registry::acting());
    s
});

/// Acting calls whose results still carry outside words: the page a new watch read first, what a photo or the
/// screen shows. They taint the turn after them like any read.
pub static ACTING_READS: LazyLock<BTreeSet<String>> = LazyLock::new(|| {
    set(&["watch_add", "take_photo", "screenshot", "meet_join", "join_meeting"]) // an invite's title comes back
});

/// Acting calls whose result carries outside words for most of their actions: spotify_control answers with the
/// catalog's own text (a track's and an artist's names, a public playlist's name and its owner's display name) when
/// it plays something by name, skips, or says what is on (round-4 review). These actions answer with eve's own words
/// only; every other action, an unknown one included, taints the turn after it like any read.
pub static OWN_WORDS_ACTIONS: LazyLock<IndexMap<String, BTreeSet<String>>> =
    LazyLock::new(eve_app_registry::own_words);

/// A call whose one read-only action only says what is on: that action is a read (Python's `_STATUS_ONLY`).
pub const STATUS_ONLY: &[(&str, &str)] = &[("take_notes", "status")];

fn status_only(name: &str) -> Option<&'static str> {
    STATUS_ONLY.iter().find(|(n, _)| *n == name).map(|(_, a)| *a)
}

/// Calls whose results are eve's own state, with no text from outside: they never taint a turn. (task_status is
/// not one: it answers with the running task's own words.)
pub static CLEAN: LazyLock<BTreeSet<String>> = LazyLock::new(|| {
    let mut s = set(&["spend_report", "open_app", "open_apps", "move_head", "forget_preview"]); // forget_preview: counts and a token
    s.extend(eve_app_registry::clean());
    s
});

/// What each kind of read is called in the owner's question ("after reading ..."); anything else is "a tool result"
/// (Python's `_SOURCES`).
pub static SOURCES: LazyLock<IndexMap<String, String>> = LazyLock::new(|| {
    let mut m: IndexMap<String, String> = IndexMap::new();
    let mut put = |k: &str, v: &str| {
        m.insert(k.to_string(), v.to_string());
    };
    put("web_search", "a web search");
    put("events_search", "Ticketmaster's listings");
    put("opentable_search", "OpenTable");
    // a read, never acting, so it never asks; not CLEAN: its titles and abstracts are other people's words
    put("science_search", "a search of research papers");
    // reads, never acting, so they never ask; not CLEAN: headlines and abstracts are the paper's words (nyt.py)
    for n in ["nyt_search", "nyt_archive", "nyt_bestsellers", "nyt_popular", "nyt_headlines"] {
        put(n, "the New York Times");
    }
    put("find_image", "Wikipedia");
    put("recent_messages", "earlier chat messages");
    put("memory_search", "memory");
    put("memory_read", "memory");
    put("read_note", "a note");
    put("search_notes", "your notes");
    put("list_inbox", "your notes");
    put("list_todos", "your todos");
    put("daily_rundown", "the rundown's sources");
    put("america_ask", "a government site");
    put("think_hard", "a long think");
    put("jobs_list", "the job cards");
    put("watch_list", "the watches");
    put("list_files", "the Mac's file list");
    put("read_file", "a file on the Mac");
    // a read, never acting, so it never asks; not CLEAN: its place is the phone's geocoder's words, which can be a
    // business's own name (phone_location.py)
    put("phone_location", "the phone's location");
    put("meet_join", "a meeting invite");
    put("join_meeting", "a meeting invite");
    put("watch_add", "the watched page");
    put("take_photo", "a photo");
    put("screenshot", "the screen");
    put("look", "the camera");
    put("look_around", "the camera");
    put("find", "the camera");
    for (k, v) in eve_app_registry::sources() {
        m.insert(k, v);
    }
    m
});

/// A Composio read in eve lite (EVE_PROFILE=lite), where Composio holds whichever apps the owner connected (source).
pub const COMPOSIO_LITE_SOURCE: &str = "your connected apps";
pub const TASK_RESULT: &str = "a computer task's result";
pub const TASK_QUESTION: &str = "a computer task's question";
pub const ATTACHED: &str = "the attached file";
pub const PICTURE: &str = "the attached picture";

/// `str(args.get(key) or "").strip().lower()`.
fn arg_word(args: &Args, key: &str) -> String {
    let v = args.get(key).filter(|v| py_truthy(v));
    py::strip(&v.map(py_str).unwrap_or_default()).to_lowercase()
}

/// Whether this call changes something, opens a session or sends something (provenance classification).
pub fn acting(name: &str, args: &Args) -> bool {
    if let Some(read) = status_only(name) {
        return arg_word(args, "action") != read;
    }
    ACTING.contains(name)
}

/// Deletions always ask; recording, joining or answering ask only after outside text. Delegated work keeps
/// its final approval in its executor.
///
/// The owner's rule (2026-10-09): ask for sending, deleting and purchasing, not routine work. ACTING remains the
/// provenance classification: routine edits still change state, but reading outside text does not revoke the
/// owner's authorization to do routine work. Starting a recording, joining a meeting or answering a task's
/// question is the owner's own request in a clean turn; after a read, a page's words could have asked for it,
/// so it asks then. Purchases and sharing have their own exact confirmation handlers; asking before those
/// handlers would ask twice for one action.
///
/// `tainted` is the turn's taint (Python's default `""`: a clean turn); any non-empty value is tainted.
pub fn needs_confirmation(name: &str, args: &Args, tainted: &str) -> bool {
    if matches!(name, "forget_apply" | "delete_app" | "watch_remove") {
        return true;
    }
    if name == "pair_eve_link" {
        // match the handler: replacing credentials revokes pairing
        return args.get("new_link").is_some_and(py_truthy);
    }
    if tainted.is_empty() {
        return false;
    }
    if matches!(name, "answer_question" | "meet_join" | "join_meeting") {
        return true;
    }
    if name == "take_notes" {
        return arg_word(args, "action") == "start"; // recording the room
    }
    false
}

/// Whether this call's result can carry words from outside eve. Most acting calls answer with eve's own
/// status; ACTING_READS answer with what they saw.
pub fn taints(name: &str, args: &Args) -> bool {
    if CLEAN.contains(name) {
        return false;
    }
    if let Some(own) = OWN_WORDS_ACTIONS.get(name) {
        let action = arg_word(args, "action");
        if action == "play" {
            // a resume says "Playing."; a play by name names the catalog's item
            return !arg_word(args, "query").is_empty();
        }
        return !own.contains(&action);
    }
    ACTING_READS.contains(name) || !acting(name, args)
}

/// The read, in the owner's words, for the question. In eve lite (profile.is_lite) Composio's reads are the
/// owner's own connected apps, whichever they are, never three named ones. Reads this process's environment;
/// a malformed EVE_PROFILE is the profile's error, as in Python.
pub fn source(name: &str) -> Result<String, ProfileError> {
    source_in(name, &ProcessEnv)
}

/// [`source`] against an explicit environment mapping.
pub fn source_in<E: EnvMap + ?Sized>(name: &str, env: &E) -> Result<String, ProfileError> {
    if let Some(s) = SOURCES.get(name) {
        return Ok(s.clone());
    }
    if let Some((head, _)) = name.split_once("__") {
        return Ok(py::capitalize(head));
    }
    if name.starts_with("COMPOSIO_") {
        return Ok(if eve_profile::is_lite_in(env)? { COMPOSIO_LITE_SOURCE } else { "Slack, Notion or Linear" }
            .to_string());
    }
    Ok("a tool result".to_string())
}

/// The question shows the act whole; a call longer than this is refused unasked ([`describe`] returns None).
pub const SHOW_CHARS: usize = 1500;

// Characters that show nothing yet change what a line says or how it reads: zero-width ones (U+200B-U+200F, the
// word joiner and invisible operators U+2060-U+2064, the BOM U+FEFF) and bidi controls (U+202A-U+202E,
// U+2066-U+2069), which can make "send to evil" display as something else. A question shows each one visibly,
// "[U+202E]", never strips it: the call runs with its arguments as written, so the owner sees what will run
// (round-4 review).
// Beyond those named, every format character (Unicode category Cf: the Arabic letter mark, the tag characters
// U+E0000-U+E007F that can hide whole words, the soft hyphen), the variation selectors and the blank fillers
// (U+180E, U+3164, U+115F, U+1160, U+FFA0) are written out too (round-4 verify).
const FILLERS: [char; 5] = ['\u{180e}', '\u{3164}', '\u{115f}', '\u{1160}', '\u{ffa0}'];

fn hidden(c: char) -> bool {
    let o = c as u32;
    py::is_format(c)
        || FILLERS.contains(&c)
        || (0xFE00..=0xFE0F).contains(&o)
        || (0xE0100..=0xE01EF).contains(&o)
        || (0xE0000..=0xE007F).contains(&o)
}

/// `text` with every hidden or direction-changing character written out as [U+XXXX].
pub fn visible(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if hidden(c) {
            out.push_str(&format!("[U+{:04X}]", c as u32));
        } else {
            out.push(c);
        }
    }
    out
}

/// [`visible`] of a JSON value as Python's `str()` renders it.
pub fn visible_value(text: &Value) -> String {
    visible(&py_str(text))
}

fn flat(text: &str) -> String {
    py::split(&visible(text)).join(" ")
}

fn flat_value(v: &Value) -> String {
    flat(&py_str(v))
}

/// `{k: v for k, v in args.items() if v not in (None, "", [], {})}`.
fn present(args: &Args) -> Args {
    args.iter()
        .filter(|(_, v)| match v {
            Value::Null => false,
            Value::String(s) => !s.is_empty(),
            Value::Array(a) => !a.is_empty(),
            Value::Object(o) => !o.is_empty(),
            _ => true,
        })
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

fn chars(s: &str) -> usize {
    s.chars().count()
}

/// The act, whole, in a line the owner can judge; None when it is too long to show whole (SHOW_CHARS).
pub fn describe(name: &str, args: &Args) -> Option<String> {
    let a = present(args);
    let line = match name {
        "start_task" => format!("start a Mac task: {}", a.get("goal").map(flat_value).unwrap_or_default()),
        "steer_task" => format!("tell the running task: {}", a.get("text").map(flat_value).unwrap_or_default()),
        "claude_relay" => {
            let action = a.get("action").filter(|v| py_truthy(v)).map(py_str).unwrap_or_else(|| "on".into());
            if action.to_lowercase() != "off" {
                let session = match a.get("session") {
                    Some(s) if py_truthy(s) => format!(" ({})", flat_value(s)),
                    _ => String::new(),
                };
                format!("connect this chat to Claude Code{session}")
            } else {
                "turn the Claude relay off".to_string()
            }
        }
        "codex_relay" => {
            let action = a.get("action").map(flat_value).unwrap_or_else(|| flat("on"));
            let folder = match a.get("folder") {
                Some(f) if py_truthy(f) => format!(" in {}", flat_value(f)),
                _ => String::new(),
            };
            format!("Codex {action}{folder}")
        }
        "set_stealth" => {
            format!("turn stealth mode {}", if args.get("on").is_some_and(py_truthy) { "on" } else { "off" })
        }
        _ => {
            let shown = py::dumps_sorted(&Value::Object(a.clone()));
            let mut line = name.replace('_', " ");
            if !a.is_empty() {
                line.push_str(": ");
                line.push_str(&flat(&shown));
            }
            line
        }
    };
    (chars(&line) <= SHOW_CHARS).then_some(line)
}

/// The owner's yes/no: what would happen, and what was read before it. None: too long to show whole.
pub fn question(name: &str, args: &Args, read: &str) -> Option<String> {
    let act = describe(name, args)?;
    Some(format!("Before I do this, a check: it comes after reading {read}, and I am about to {act}. Go ahead?"))
}

/// The history's line for the question: no words from the read become eve's own (as tell_owner's about).
pub fn about(name: &str, read: &str) -> String {
    format!("(asked the owner before {name} because it came after reading {read})")
}

pub static REFUSED: LazyLock<Value> = LazyLock::new(|| {
    json!({"ok": false, "reason": "the owner did not approve it; it did not run. Do not try it again this turn."})
});
pub static TOO_LONG: LazyLock<Value> = LazyLock::new(|| {
    json!({"ok": false, "reason": "too long to show the owner whole in a question, so it did not run; ask the owner \
to say what they want in their own words."})
});

/// Out loud the act must be short enough to hear whole: a call whose spoken line is longer is refused unasked.
pub const SAY_CHARS: usize = 220;

/// Python's `_SAY_HEADS`.
pub static SAY_HEADS: LazyLock<IndexMap<String, String>> = LazyLock::new(|| {
    let mut m: IndexMap<String, String> = [
        ("start_task", "start a computer task to"),
        ("steer_task", "tell the running task"),
        ("join_meeting", "join the meeting"),
        ("meet_join", "join the meeting"),
        ("answer_question", "answer the task's question with"),
        ("take_notes", "take notes"),
        ("remember", "remember that"),
        ("watch_add", "watch"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    for (k, v) in eve_app_registry::say() {
        m.insert(k, v);
    }
    m
});
const SAY_WORDS: &[(&str, &str)] = &[("start_task", "goal"), ("steer_task", "text"), ("answer_question", "answer")];
const SAY_FIXED: &[(&str, &str)] = &[
    ("leave_meeting", "leave the meeting"),
    ("meet_leave", "leave the meeting"),
    ("go_explore", "go off exploring"),
    ("stop_task", "stop the running task"),
];

fn lookup(table: &'static [(&'static str, &'static str)], name: &str) -> Option<&'static str> {
    table.iter().find(|(k, _)| *k == name).map(|(_, v)| *v)
}

fn said_value(value: &Value) -> String {
    match value {
        Value::Bool(b) => (if *b { "yes" } else { "no" }).to_string(),
        Value::String(_) | Value::Number(_) => flat_value(value),
        other => flat(&py::dumps_sorted(other)),
    }
}

/// The act as a short line code writes for the voice to say, from the call's name and arguments alone (never
/// the model's own sentence); None when it cannot be said briefly (SAY_CHARS).
pub fn say_act(name: &str, args: &Args) -> Option<String> {
    let a = present(args);
    let line = if let Some(fixed) = lookup(SAY_FIXED, name) {
        fixed.to_string()
    } else if name == "set_sound" {
        format!("turn my sound {}", if args.get("on").is_some_and(py_truthy) { "on" } else { "off" })
    } else if let Some(field) = lookup(SAY_WORDS, name) {
        let words = a.get(field).map(flat_value).unwrap_or_default();
        let head = &SAY_HEADS[name];
        if words.is_empty() {
            head.clone()
        } else {
            format!("{head}: {words}")
        }
    } else {
        let head = SAY_HEADS.get(name).cloned().unwrap_or_else(|| name.replace('_', " "));
        let mut items: Vec<(&String, &Value)> = a.iter().collect();
        items.sort_by(|x, y| x.0.cmp(y.0));
        let said =
            items.iter().map(|(k, v)| format!("{} {}", k.replace('_', " "), said_value(v))).collect::<Vec<_>>().join(", ");
        if said.is_empty() {
            head
        } else {
            format!("{head}: {said}")
        }
    };
    (chars(&line) <= SAY_CHARS).then_some(line)
}

/// What the voice is handed to say, word for word: code's own question about this exact call. None: refused
/// unasked (it cannot be said briefly).
pub fn spoken_question(name: &str, args: &Args, read: &str) -> Option<String> {
    let act = say_act(name, args)?;
    Some(format!("Quick check first, because I just read {read}. Should I {act}? Say yes to go ahead."))
}

/// `" ".join(re.sub(r"[^\w@.]+|(?<!\w)[.@]|[.@](?!\w)", " ", str(text).lower()).split())`: the words, with a
/// full stop or an at sign kept only between two word characters.
fn words(text: &str) -> String {
    let lower: Vec<char> = text.to_lowercase().chars().collect();
    let mut out = String::with_capacity(lower.len());
    for (i, &c) in lower.iter().enumerate() {
        let keep = if py::is_word(c) {
            true
        } else if c == '.' || c == '@' {
            let before = i > 0 && py::is_word(lower[i - 1]);
            let after = lower.get(i + 1).is_some_and(|&n| py::is_word(n));
            before && after
        } else {
            false
        };
        out.push(if keep { c } else { ' ' });
    }
    out.split(' ').filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ")
}

/// Whether the voice's own output transcript (what it said) ends with the question code wrote, word for word,
/// punctuation and case aside: the last thing the owner heard before their yes is code's whole question. The voice
/// (a Live speech model) has no say-this-verbatim path: code hands it the question as commentary, and this is the
/// check that it said it. A yes counts for the held call only when it did (voice_agent._gate); a paraphrase, or the
/// act framed by words of the model's own ("should I not …", an aside after it), drops the held call, so it is
/// asked again (round-4 verify: containment let the act be framed falsely). Words said earlier in the same turn
/// come before the question and change nothing it asks.
pub fn said_whole(question: &str, spoken: &str) -> bool {
    let want = words(question);
    let said = words(spoken);
    // the voice's transcript arrives in pieces joined as they came ("one.Quick"), so a full stop also bounds it
    !want.is_empty()
        && (said == want || said.ends_with(&format!(" {want}")) || said.ends_with(&format!(".{want}")))
}

pub const SAY_EXACTLY: &str = "Say exactly this, word for word, and nothing else: ";
pub static ASKED_ALOUD: LazyLock<Value> = LazyLock::new(|| {
    json!({"ok": false,
           "reason": "Not run yet: eve has asked the owner out loud whether to do exactly this. Say nothing \
more about it now. Only after they say yes, call it again with the same arguments."})
});
pub static TOO_LONG_ALOUD: LazyLock<Value> = LazyLock::new(|| {
    json!({"ok": false, "reason": "too long to say to the owner whole, so it did not run; ask the owner to \
say in their own words what they want."})
});

// ---- carried taint: outside words already in a turn's prompt ----------------------------------------------------

/// A fingerprint is a line's first FP_CHARS characters, whitespace flattened: short enough to survive the voice
/// block's clip (transcripts.VOICE_LINE_CHARS), long enough to name one line. Shorter lines carry too little to act
/// on.
pub const FP_CHARS: usize = 96;
pub const FP_MIN: usize = 24;
pub const CARRIED: &str = "an earlier tool result";
/// A tool line whose words are not eve's although the call itself is acting: a computer task's closing sentence,
/// written after it read screens and pages (voice_agent._run_agent). carried() treats it as a read.
pub static RESULT_LINES: LazyLock<BTreeSet<String>> = LazyLock::new(|| set(&["start_task"]));
/// A tool's line as a transcript block renders it: "(texted) eve (web search): ..." (transcripts.render_line).
static TOOL_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\((?:spoken|texted|sent)\) eve \(([a-z0-9 ]{1,40})\): ").expect("valid regex"));

pub fn fingerprint(text: &str) -> String {
    let f = flat(text);
    if chars(&f) >= FP_MIN {
        f.chars().take(FP_CHARS).collect()
    } else {
        String::new()
    }
}

/// Lines eve wrote with outside words in them (a tainted turn's reply, a task's closing sentence), by
/// fingerprint: one per process, shared by the chat and the voice, which read one transcript. Bounded; a line long
/// gone from every window costs nothing but a slot. In memory only: after a restart the transcript's tool lines
/// still carry ([`carried`] reads them from the prompt itself), a tainted reply from before it does not.
#[derive(Debug)]
pub struct Marks {
    size: usize,
    prints: Mutex<VecDeque<String>>,
}

impl Default for Marks {
    fn default() -> Self {
        Marks::new(400)
    }
}

impl Marks {
    pub fn new(size: usize) -> Self {
        Marks { size, prints: Mutex::new(VecDeque::with_capacity(size.min(4096))) }
    }

    pub fn add<I, S>(&self, texts: I)
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut prints = self.prints.lock();
        for text in texts {
            let fp = fingerprint(text.as_ref());
            if !fp.is_empty() && !prints.contains(&fp) {
                // deque(maxlen=size): the oldest goes when it is full; a size of 0 keeps nothing
                if self.size == 0 {
                    continue;
                }
                if prints.len() >= self.size {
                    prints.pop_front();
                }
                prints.push_back(fp);
            }
        }
    }

    pub fn clear(&self) {
        self.prints.lock().clear();
    }

    pub fn found(&self, prompt: &str) -> bool {
        self.prints.lock().iter().any(|fp| prompt.contains(fp.as_str()))
    }
}

/// The process's marks, shared by the chat and the voice.
pub static MARKS: LazyLock<Marks> = LazyLock::new(Marks::default);

/// Every string in a request's instructions and input, pictures left out.
fn texts<'a>(value: &'a Value, out: &mut Vec<&'a str>) {
    match value {
        Value::String(s) => {
            if !s.starts_with("data:") {
                out.push(s);
            }
        }
        Value::Object(m) => m.values().for_each(|v| texts(v, out)),
        Value::Array(a) => a.iter().for_each(|v| texts(v, out)),
        _ => {}
    }
}

/// CARRIED when the prompt (`parts`: its instructions, its input items) already holds outside words: a
/// tainting tool's line in a transcript block, or a marked line. "" when it holds only eve's and the owner's.
/// `marks` None reads [`MARKS`].
pub fn carried<'a, I>(parts: I, marks: Option<&Marks>) -> &'static str
where
    I: IntoIterator<Item = &'a Value>,
{
    let mut all = Vec::new();
    for p in parts {
        texts(p, &mut all);
    }
    let prompt = flat(&all.join(" "));
    for m in TOOL_LINE.captures_iter(&prompt) {
        let tool = m[1].replace(' ', "_");
        if RESULT_LINES.contains(&tool) || taints(&tool, &Args::new()) {
            return CARRIED;
        }
    }
    if marks.unwrap_or(&MARKS).found(&prompt) {
        CARRIED
    } else {
        ""
    }
}

#[cfg(test)]
mod tests;
