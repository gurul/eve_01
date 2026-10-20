//! Read-only context and tool policy for Eve's explicit daily rundown skill.
//!
//! Port of `bridge/src/eve_bridge/rundown.py`: the rundown's trigger (`matches`), its read-only tool policy
//! (`tools`, `allows`), the vault todo reader that groups open checkboxes by the date written in each line
//! (`task_date`, `todo_context`), and the skill text with the source context the model is handed (`context`).

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use chrono::{DateTime, Datelike, FixedOffset, NaiveDate, NaiveTime, TimeZone};
use eve_apps_mcp::py::dumps;
use eve_google_rest::py::{is_space, py_str, splitlines, take_chars, truthy};
use eve_google_rest::pytime::{date_fromisoformat, isoformat};
use eve_google_rest::Zone;
use fancy_regex::Regex as FancyRegex;
use regex::Regex;
use serde_json::{Map, Value};

/// `skills/rundown/SKILL.md`, the same bytes the Python daemon reads.
pub const SKILL: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../bridge/src/eve_bridge/skills/rundown/SKILL.md"));

/// Python's `\s` in a str pattern: what `str.isspace()` accepts, which adds U+001C-U+001F to Unicode White_Space.
const S: &str = r"[\s\x1c-\x1f]";
/// Python's `\w` in a str pattern: `str.isalnum()` (letters and numbers) or `_`. The `regex` crate's own `\w` and
/// `\b` also count combining marks, variation selectors and joiners, so "✈️ 10/22" (an emoji with U+FE0F) or
/// "⭐️Oct 22" would lose their dates; the word boundary is written out with this class instead.
const W: &str = r"[\p{L}\p{N}_]";
/// Python's `[\W_]`: anything that is not a letter or a number.
const NOT_ALNUM: &str = r"[^\p{L}\p{N}]";

/// Python's `\b`, over Python's `\w`.
fn boundary() -> String {
    format!(r"(?:(?<={W})(?!{W})|(?<!{W})(?={W}))")
}

/// Python's `re` has no backtracking limit; these patterns are near linear, so the limit only guards a pathological
/// line.
fn fancy(pattern: &str) -> FancyRegex {
    fancy_regex::RegexBuilder::new(pattern).backtrack_limit(50_000_000).build().expect("rundown pattern compiles")
}

pub static OPEN_TASK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!(r"^{S}*[-*+]{S}+\[ \]{S}+(.+)$")).expect("OPEN_TASK compiles"));
pub static TASK_DATE: LazyLock<FancyRegex> = LazyLock::new(|| {
    fancy(&format!(r"(?i)(?:📅|⏳|🛫|{b}(?:due|scheduled|start)::?){S}*(\d{{4}}-\d{{2}}-\d{{2}})", b = boundary()))
});
pub const READ_META_TOOLS: [&str; 2] = ["COMPOSIO_SEARCH_TOOLS", "COMPOSIO_GET_TOOL_SCHEMAS"];
/// The direct apps a rundown reads (apps_mcp), already open when it starts; Slack comes through Composio.
pub const DIRECT_APPS: [&str; 2] = ["gmail", "calendar"];

// A date written in plain words is a due date too. 2026-09-30 the rundown called "Oct 22: go to Mount
// Tam" undated backlog because only the Obsidian Tasks markers above were read, and every future item was
// dropped. Dates are found here, in code, so the model is handed items that are already grouped.
const MONTH_NAMES: [&[&str]; 12] = [
    &["january", "jan"],
    &["february", "feb"],
    &["march", "mar"],
    &["april", "apr"],
    &["may"],
    &["june", "jun"],
    &["july", "jul"],
    &["august", "aug"],
    &["september", "sept", "sep"],
    &["october", "oct"],
    &["november", "nov"],
    &["december", "dec"],
];

static MONTHS: LazyLock<HashMap<&'static str, u32>> = LazyLock::new(|| {
    let mut out = HashMap::new();
    for (i, names) in MONTH_NAMES.iter().enumerate() {
        for name in names.iter() {
            out.insert(*name, i as u32 + 1);
        }
    }
    out
});

/// `_MONTH`: the names longest first (a stable sort, as Python's `sorted`), then a word end and an optional dot.
fn month_pattern() -> String {
    let mut names: Vec<&str> = MONTH_NAMES.iter().flat_map(|n| n.iter().copied()).collect();
    names.sort_by_key(|n| std::cmp::Reverse(n.len()));
    format!(r"(?:{})(?:{})\.?", names.join("|"), boundary())
}

/// `_ORD`.
fn ord_pattern() -> String {
    format!(r"(?:st|nd|rd|th)?{}", boundary())
}

fn year_pattern(group: &str) -> String {
    format!(r"(?:,?{S}+(?P<{group}>\d{{4}}){})?", boundary())
}

static NAMED: LazyLock<FancyRegex> = LazyLock::new(|| {
    let month = month_pattern();
    fancy(&format!(
        r"(?i){b}(?P<m1>{month}){S}+(?P<d1>\d{{1,2}}){ord}{y1}|{b}(?P<d2>\d{{1,2}}){ord}{S}+(?:of{S}+)?(?P<m2>{month}){y2}",
        b = boundary(),
        ord = ord_pattern(),
        y1 = year_pattern("y1"),
        y2 = year_pattern("y2"),
    ))
});
static NUMERIC: LazyLock<FancyRegex> =
    LazyLock::new(|| fancy(r"(?<![\d/.])(?P<m>\d{1,2})/(?P<d>\d{1,2})(?:/(?P<y>\d{4}|\d{2}))?(?![\d/])"));
static ISO: LazyLock<FancyRegex> = LazyLock::new(|| fancy(r"(?<![\d-])(\d{4})-(\d{2})-(\d{2})(?![\d-])"));
// When an item was written down or finished is not when it is due: capture adds "(added YYYY-MM-DD)".
static NOT_DUE: LazyLock<FancyRegex> = LazyLock::new(|| {
    fancy(&format!(
        r"(?i)(?:➕|✅|❌|{b}(?:added|created|done|completed|cancelled):{{0,2}}){S}*\d{{4}}-\d{{2}}-\d{{2}}",
        b = boundary()
    ))
});
// A yearless "10/22" counts only where a date is plainly meant, so "1/2 gallon" stays a quantity.
// Python's `$` (no MULTILINE) also matches before one final newline, hence `\n?\z`.
static DATE_SLOT: LazyLock<FancyRegex> = LazyLock::new(|| {
    fancy(&format!(
        r"(?i)(?:^{NOT_ALNUM}*(?:p[0-3]{b}{NOT_ALNUM}*)?|{b}(?:on|by|due|before|until|till){S}+)\n?\z",
        b = boundary()
    ))
});

static DECIMAL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d$").expect("DECIMAL compiles"));

fn is_decimal(c: char) -> bool {
    let mut buf = [0u8; 4];
    DECIMAL.is_match(c.encode_utf8(&mut buf))
}

/// A Unicode decimal digit's value. Unicode assigns decimal digits only in runs of ten, 0 to 9, so a digit's value
/// is its distance from the start of its run of digits, modulo ten.
fn digit_value(c: char) -> Option<i64> {
    if let Some(v) = c.to_digit(10) {
        return Some(v.into());
    }
    if !is_decimal(c) {
        return None;
    }
    let mut start = c as u32;
    while let Some(prev) = start.checked_sub(1).and_then(char::from_u32) {
        if !is_decimal(prev) {
            break;
        }
        start -= 1;
    }
    Some(i64::from((c as u32 - start) % 10))
}

/// `int(s)` for a run of `\d` the patterns matched: Python reads any Unicode decimal digits ("٢٢" is 22).
fn py_int(s: &str) -> Option<i64> {
    if s.is_empty() {
        return None;
    }
    s.chars().try_fold(0i64, |n, c| n.checked_mul(10)?.checked_add(digit_value(c)?))
}

/// `date(year, month, day)`, None where Python raises ValueError (Python's years run 1 to 9999).
fn make_date(year: i64, month: u32, day: u32) -> Option<NaiveDate> {
    if !(1..=9999).contains(&year) {
        return None;
    }
    NaiveDate::from_ymd_opt(year as i32, month, day)
}

/// A date with no year is the occurrence nearest today (a tie goes forward): up to about six months
/// back it is overdue, up to about six months ahead it is upcoming.
fn yearless(month: u32, day: u32, today: NaiveDate) -> Option<NaiveDate> {
    let year = today.year() as i64;
    [year - 1, year, year + 1]
        .into_iter()
        .filter_map(|y| make_date(y, month, day))
        .min_by_key(|d| ((*d - today).num_days().abs(), *d < today))
}

fn dated(year: Option<&str>, month: u32, day: u32, today: NaiveDate) -> Option<NaiveDate> {
    let year = match year {
        Some(y) if !y.is_empty() => y,
        _ => return yearless(month, day, today),
    };
    let n = py_int(year)?;
    make_date(n + if year.chars().count() == 2 { 2000 } else { 0 }, month, day)
}

fn iso(d: NaiveDate) -> String {
    format!("{:04}-{:02}-{:02}", d.year(), d.month(), d.day())
}

/// The earliest due date written in a todo line, or None when it has none.
///
/// Obsidian Tasks markers win when present. Otherwise a date in words or numbers counts anywhere in the
/// line: "Oct 22", "October 22nd", "22 Oct", "Oct 22, 2027", "2026-10-22", "10/22/2026", and a
/// yearless "10/22" at the start of the line or after on/by/due/before/until. Numbers are month/day.
pub fn task_date(text: &str, today: NaiveDate) -> Option<NaiveDate> {
    let marked: Vec<NaiveDate> = TASK_DATE
        .captures_iter(text)
        .map_while(Result::ok)
        .filter_map(|c| date_fromisoformat(c.get(1).map_or("", |m| m.as_str())).ok())
        .collect();
    if let Some(first) = marked.iter().min() {
        return Some(*first);
    }
    let text = NOT_DUE.try_replacen(text, 0, " ").map_or_else(|_| text.to_string(), |t| t.into_owned());
    let small = |m: Option<fancy_regex::Match<'_>>| m.and_then(|x| py_int(x.as_str())).and_then(|n| u32::try_from(n).ok());
    let mut found: Vec<NaiveDate> = Vec::new();
    for m in NAMED.captures_iter(&text) {
        let Ok(m) = m else { break };
        let Some(name) = m.name("m1").or_else(|| m.name("m2")).map(|n| n.as_str()) else {
            continue;
        };
        let key = name.trim_end_matches('.').to_lowercase();
        if key == "may" && !name.starts_with('M') {
            continue; // "you may 2 times" is a verb; the month is written with a capital
        }
        let Some(&month) = MONTHS.get(key.as_str()) else {
            continue;
        };
        let Some(day) = small(m.name("d1").or_else(|| m.name("d2"))) else {
            continue;
        };
        let year = m.name("y1").or_else(|| m.name("y2")).map(|y| y.as_str());
        if let Some(when) = dated(year, month, day, today) {
            found.push(when);
        }
    }
    for m in ISO.captures_iter(&text) {
        let Ok(m) = m else { break };
        let (Some(y), Some(mo), Some(d)) = (m.get(1), small(m.get(2)), small(m.get(3))) else {
            continue;
        };
        if let Some(when) = dated(Some(y.as_str()), mo, d, today) {
            found.push(when);
        }
    }
    for m in NUMERIC.captures_iter(&text) {
        let Ok(m) = m else { break };
        let start = m.get(0).map_or(0, |w| w.start());
        let year = m.name("y").map(|y| y.as_str());
        if year.is_none() && !DATE_SLOT.is_match(&text[..start]).unwrap_or(false) {
            continue;
        }
        let (Some(month), Some(day)) = (small(m.name("m")), small(m.name("d"))) else {
            continue;
        };
        let when = if (1..=12).contains(&month) { dated(year, month, day, today) } else { None };
        if let Some(when) = when {
            found.push(when);
        }
    }
    found.into_iter().min()
}

// Asking for today's plan is asking for the rundown: the calendar is most of a day's plan. "What is my
// plane today" (2026-09-24 08:44) missed the word "rundown", took the vault-only plan and a 20 s second
// model, and came back with no calendar. Whole-text matches only, each anchored on today or "my day":
// "plan a trip", "today's news" and "what's on netflix today" stay ordinary turns.
const WHAT_IS: &str = r"(?:what(?:'s| is|s)|whats)";
const PLAN: &str = r"(?:plan|plane|plans|schedule|agenda|calendar)";

static DAILY: LazyLock<Regex> = LazyLock::new(|| {
    let alternatives = [
        r"/?rundown".to_string(),
        format!(r"{WHAT_IS} (?:my|the) {PLAN}(?: like)? (?:for |on )?(?:today|the day)"),
        format!(r"{WHAT_IS} today(?:'s|s)? {PLAN}"),
        format!(r"(?:my |the )?today(?:'s|s)? {PLAN}"),
        format!(r"(?:my |the )?{PLAN} (?:for )?today"),
        r"(?:(?:please|can you|could you|help me) )?plan (?:out )?(?:my day|the day|today)(?: today)?(?: please)?"
            .to_string(),
        format!(r"{WHAT_IS} on (?:for |my calendar |my schedule |my agenda )?today"),
        r"what (?:do|have) i (?:got |have )?(?:on |going on |planned |to do )?(?:for )?today".to_string(),
        format!(r"{WHAT_IS} my day (?:look )?like(?: today)?"),
        r"(?:what does|how does) (?:my day|today) look(?: like)?(?: today)?".to_string(),
    ];
    // `fullmatch`: anchored at both ends.
    Regex::new(&format!(
        r"^(?:(?:(?:hey|hi|ok|okay|so){S}+)?(?:eve[,:]?{S}+)?(?:{}))\z",
        alternatives.join("|")
    ))
    .expect("_DAILY compiles")
});

/// Whether the owner's whole message asks for the rundown (or today's plan).
pub fn matches(text: &str) -> bool {
    let replaced = text.replace('\u{2019}', "'");
    let lowered = eve_google_rest::py::strip(&replaced).to_lowercase();
    let mut said = String::with_capacity(lowered.len());
    let mut in_space = false;
    for c in lowered.chars() {
        if is_space(c) {
            if !in_space {
                said.push(' ');
            }
            in_space = true;
        } else {
            said.push(c);
            in_space = false;
        }
    }
    let said = said.trim_end_matches(['.', '!', '?', ' ']);
    DAILY.is_match(said)
}

/// What the rundown needs from the direct apps bridge (`apps_mcp.AppsBridge`): whether a tool is read only.
pub trait Direct {
    fn reads(&self, name: &str) -> bool;
}

impl Direct for eve_apps_mcp::AppsBridge {
    fn reads(&self, name: &str) -> bool {
        eve_apps_mcp::AppsBridge::reads(self, name)
    }
}

/// The tools a rundown is offered: the read tools of mail and the calendar, and Composio's search and
/// multi-execute (for Slack, read only by `allows`).
pub fn tools(app_tools: &[Value], direct: Option<&dyn Direct>) -> Vec<Value> {
    let empty = Map::new();
    app_tools
        .iter()
        .filter(|t| {
            let Some(t) = t.as_object() else { return false };
            let name = t.get("name");
            let named = |want: &str| matches!(name, Some(Value::String(s)) if s == want);
            if READ_META_TOOLS.iter().any(|m| named(m)) || named(eve_composio_tools::MULTI_EXECUTE) {
                return true;
            }
            let name = match name {
                Some(v) if truthy(v) => py_str(v),
                _ => String::new(),
            };
            allows(&name, &empty, direct)
        })
        .cloned()
        .collect()
}

/// A rundown only reads: a direct mail or calendar tool its server marks read only, or a Composio call
/// whose every slug reads mail, the calendar, Outlook or Slack.
pub fn allows(name: &str, args: &Map<String, Value>, direct: Option<&dyn Direct>) -> bool {
    if READ_META_TOOLS.contains(&name) {
        return true;
    }
    let (app, _tool) = eve_apps_mcp::split_name(name);
    if !app.is_empty() {
        return direct.is_some_and(|d| DIRECT_APPS.contains(&app) && d.reads(name));
    }
    if name != eve_composio_tools::MULTI_EXECUTE {
        return false;
    }
    let Some(Value::Array(entries)) = args.get("tools") else {
        return false;
    };
    !entries.is_empty()
        && entries.iter().all(|t| {
            let Some(t) = t.as_object() else { return false };
            let slug = t.get("tool_slug").map(py_str).unwrap_or_default();
            eve_composio_tools::is_read_only(&slug)
                && ["gmail", "googlecalendar", "outlook", "slack"]
                    .contains(&eve_composio_tools::slug_words(&slug).0.as_str())
        })
}

pub const TODO_GROUPS: [&str; 4] = ["today", "overdue", "upcoming", "undated"];

const MAX_FILES: usize = 5000;
const MAX_TASKS: usize = 200;
const MAX_BYTES: u64 = 1024 * 1024;

/// The vault's open checkboxes, grouped today / overdue / upcoming / undated by the date written in each line.
///
/// `day` is read with `date.fromisoformat`; a day it refuses is the error (only once a vault is present, as in
/// Python).
pub fn todo_context(root: Option<&Path>, day: &str) -> Result<Value, eve_google_rest::Error> {
    let Some(root) = root.filter(|r| r.is_dir()) else {
        return Ok(unavailable());
    };
    let root = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let today = date_fromisoformat(day)?;
    Ok(scan(&root, today, day))
}

/// `todo_context` for a day already read as a date.
fn todo_context_on(root: Option<&Path>, today: NaiveDate, day: &str) -> Value {
    match root.filter(|r| r.is_dir()) {
        None => unavailable(),
        Some(r) => scan(&fs::canonicalize(r).unwrap_or_else(|_| r.to_path_buf()), today, day),
    }
}

fn unavailable() -> Value {
    let mut out = Map::new();
    out.insert("available".into(), Value::Bool(false));
    out.insert("reason".into(), Value::String("Obsidian vault is not configured or not present".into()));
    Value::Object(out)
}

struct Scan<'a> {
    root: &'a Path,
    today: NaiveDate,
    day: &'a str,
    groups: [Vec<Value>; 4],
    skipped: u64,
    truncated: bool,
    count: usize,
}

fn is_symlink(p: &Path) -> bool {
    fs::symlink_metadata(p).map(|m| m.file_type().is_symlink()).unwrap_or(false)
}

fn scan(root: &Path, today: NaiveDate, day: &str) -> Value {
    let mut state = Scan {
        root,
        today,
        day,
        groups: Default::default(),
        skipped: 0,
        truncated: false,
        count: 0,
    };
    walk(&mut state, root);
    let [today_items, mut overdue, mut upcoming, undated] = std::mem::take(&mut state.groups);
    let by_date = |a: &Value, b: &Value| a["date"].as_str().unwrap_or("").cmp(b["date"].as_str().unwrap_or(""));
    overdue.sort_by(by_date);
    upcoming.sort_by(by_date);
    let mut out = Map::new();
    out.insert("available".into(), Value::Bool(true));
    out.insert("today".into(), Value::Array(today_items));
    out.insert("overdue".into(), Value::Array(overdue));
    out.insert("upcoming".into(), Value::Array(upcoming));
    out.insert("undated".into(), Value::Array(undated));
    out.insert("skipped_files".into(), Value::from(state.skipped));
    if state.truncated {
        out.insert("truncated_tasks".into(), Value::Bool(true));
    }
    Value::Object(out)
}

/// One step of `os.walk(root, followlinks=False)`, top down: this folder's files in sorted order, then each
/// kept subfolder (no dot folders, no `09-archive`, no symlinks) in sorted order. A folder that cannot be listed
/// is passed over, as `os.walk` does with no `onerror`.
fn walk(state: &mut Scan<'_>, folder: &Path) {
    let Ok(entries) = fs::read_dir(folder) else { return };
    let mut dirs: Vec<(std::ffi::OsString, PathBuf)> = Vec::new();
    let mut files: Vec<(std::ffi::OsString, PathBuf)> = Vec::new();
    for entry in entries {
        // os.walk drops the whole folder when listing fails part way.
        let Ok(entry) = entry else { return };
        let path = entry.path();
        let is_dir = fs::metadata(&path).map(|m| m.is_dir()).unwrap_or(false);
        if is_dir {
            dirs.push((entry.file_name(), path));
        } else {
            files.push((entry.file_name(), path));
        }
    }
    dirs.sort();
    files.sort();
    for (name, p) in &files {
        let name = name.to_string_lossy();
        if !name.ends_with(".md") || name.starts_with('.') || is_symlink(p) {
            continue;
        }
        read_note(state, p, &name);
    }
    for (name, p) in &dirs {
        let name = name.to_string_lossy();
        if name.starts_with('.') || name == "09-archive" || is_symlink(p) {
            continue;
        }
        walk(state, p);
    }
}

fn read_note(state: &mut Scan<'_>, p: &Path, name: &str) {
    state.count += 1;
    if state.count > MAX_FILES {
        state.skipped += 1;
        return;
    }
    let inside = fs::canonicalize(p).map(|r| r.starts_with(state.root)).unwrap_or(false);
    let small = fs::metadata(p).map(|m| m.len() <= MAX_BYTES);
    let text = match (inside, small) {
        (true, Ok(true)) => fs::read(p).ok().and_then(|b| String::from_utf8(b).ok()),
        _ => None,
    };
    let Some(text) = text else {
        state.skipped += 1;
        return;
    };
    let stem = name.strip_suffix(".md").unwrap_or(name);
    let note = p
        .strip_prefix(state.root)
        .unwrap_or(p)
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/");
    let mut fence: Option<&str> = None;
    for (i, line) in splitlines(&text).into_iter().enumerate() {
        let line_no = i + 1;
        let stripped = line.trim_start_matches(is_space);
        if stripped.starts_with("```") || stripped.starts_with("~~~") {
            let marker = &stripped[..3];
            fence = if fence == Some(marker) {
                None
            } else if fence.is_none() {
                Some(marker)
            } else {
                fence
            };
            continue;
        }
        if fence.is_some() {
            continue;
        }
        let Some(task) = OPEN_TASK.captures(line).and_then(|c| c.get(1)).map(|m| m.as_str()) else {
            continue;
        };
        let when = match task_date(task, state.today) {
            Some(d) => iso(d),
            None if stem == state.day => state.day.to_string(),
            None => String::new(),
        };
        let group = if when == state.day {
            0
        } else if !when.is_empty() && when.as_str() < state.day {
            1
        } else if !when.is_empty() {
            2
        } else {
            3
        };
        if state.groups.iter().map(Vec::len).sum::<usize>() >= MAX_TASKS {
            state.truncated = true;
            continue;
        }
        let mut item = Map::new();
        item.insert("text".into(), Value::String(take_chars(task, 2000).to_string()));
        item.insert("note".into(), Value::String(note.clone()));
        item.insert("line".into(), Value::from(line_no));
        if !when.is_empty() {
            item.insert("date".into(), Value::String(when));
        }
        state.groups[group].push(Value::Object(item));
    }
}

fn join_and(items: &[String]) -> String {
    match items.split_last() {
        Some((last, rest)) if !rest.is_empty() => format!("{} and {last}", rest.join(", ")),
        Some((last, _)) => last.clone(),
        None => String::new(),
    }
}

/// Step 1 of the skill: where each source is read, naming only the tools that are present this turn.
///
/// Review, 2026-10-05: the step was fixed text naming the direct mail and calendar tools and Composio's, so
/// with either one off the model called a name the rundown never offered and the owner lost the rundown. A
/// source with nothing to read it is said to be not connected, and no tool is named for it.
pub fn sources(direct_apps: &[&str], composio: bool) -> String {
    let up: Vec<&str> = DIRECT_APPS.iter().copied().filter(|a| direct_apps.contains(a)).collect();
    let words = |a: &str| if a == "gmail" { "Email" } else { "Calendar" };
    let mut lines: Vec<String> = Vec::new();
    if !up.is_empty() {
        let mut line = up.iter().map(|a| words(a)).collect::<Vec<_>>().join(" and ");
        line.push_str(": use the read tools you were given, ");
        line.push_str(
            &up.iter().map(|a| format!("{a}{}…", eve_apps_mcp::SEP)).collect::<Vec<_>>().join(" and "),
        );
        line.push('.');
        if up.contains(&"calendar") {
            line.push_str(" calendar__list_events with no calendarId reads every calendar in one call.");
        }
        lines.push(line);
    }
    let mut via_composio: Vec<String> = vec!["Slack".into()];
    via_composio.extend(DIRECT_APPS.iter().filter(|a| !up.contains(a)).map(|a| words(a).to_lowercase()));
    if composio {
        lines.push(
            join_and(&via_composio)
                + ": find their read tools through COMPOSIO_SEARCH_TOOLS and run the exact discovered tools with "
                + "COMPOSIO_MULTI_EXECUTE_TOOL. Do not guess schemas.",
        );
    } else {
        let missing: Vec<String> = via_composio.iter().map(|w| capitalize(w)).collect();
        let verb = if missing.len() == 1 { " is" } else { " are" };
        lines.push(join_and(&missing) + verb + " not connected here; say so in one line.");
    }
    lines.push("A source whose read fails is said to have failed in one line; go on with the rest.".into());
    format!("1. {}", lines.join("\n   "))
}

/// `str.capitalize()` for the source names (plain ASCII words).
fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars.flat_map(char::to_lowercase)).collect(),
        None => String::new(),
    }
}

/// The skill text with step 1 filled and today's source context appended, for the Mac's own zone.
pub fn context(
    root: Option<&Path>,
    now: &DateTime<FixedOffset>,
    canvas: Option<&Value>,
    direct_apps: &[&str],
    composio: bool,
) -> String {
    context_and_outside(root, now, canvas, direct_apps, composio).0
}

/// Whether the source context holds words eve did not write: a vault todo's text, a Canvas assignment's or
/// course's name. Then the rundown's reply is written from outside words, like a turn after a read (round-4
/// review): kept as outside words (tool_gate), so a later turn showing it starts tainted.
pub fn outside(data: &Value) -> bool {
    if let Some(Value::Object(todos)) = data.get("obsidian").filter(|v| truthy(v)) {
        if TODO_GROUPS.iter().any(|g| todos.get(*g).is_some_and(truthy)) {
            return true;
        }
    }
    matches!(data.get("canvas"), Some(Value::Object(school)) if school.get("items").is_some_and(truthy))
}

/// `context`'s text, and whether its source context carries outside words (`outside`).
pub fn context_and_outside(
    root: Option<&Path>,
    now: &DateTime<FixedOffset>,
    canvas: Option<&Value>,
    direct_apps: &[&str],
    composio: bool,
) -> (String, bool) {
    context_and_outside_in(root, now, canvas, direct_apps, composio, Zone::Local)
}

/// `context_and_outside` with the day read in `zone` (the Mac's, `Zone::Local`, is what the daemon uses).
pub fn context_and_outside_in(
    root: Option<&Path>,
    now: &DateTime<FixedOffset>,
    canvas: Option<&Value>,
    direct_apps: &[&str],
    composio: bool,
    zone: Zone,
) -> (String, bool) {
    // Naive local boundaries are converted separately so a DST day need not be 24h.
    let local = zone.convert(now);
    let day = local.date_naive();
    let start = zone.localize(day.and_time(NaiveTime::MIN));
    let next = day.succ_opt().unwrap_or(day);
    let end = zone.localize(next.and_time(NaiveTime::MIN));
    let day_iso = iso(day);
    let obsidian = todo_context_on(root, day, &day_iso);
    let mut data = Map::new();
    data.insert("date".into(), Value::String(day_iso.clone()));
    data.insert("timezone".into(), Value::String(tzname(zone, now.timestamp())));
    data.insert("start_inclusive".into(), Value::String(isoformat(&start)));
    data.insert("end_exclusive".into(), Value::String(isoformat(&end)));
    data.insert("obsidian".into(), obsidian);
    if let Some(canvas) = canvas {
        // Canvas deadlines (canvas.Canvas.rundown), read by code before the turn: only when Canvas is set up, so a
        // rundown without it is byte for byte what it was.
        data.insert("canvas".into(), canvas.clone());
    }
    let data = Value::Object(data);
    let skill = SKILL.replace("1. {{sources}}", &sources(direct_apps, composio));
    let fed = outside(&data);
    (format!("{skill}\n\nCurrent source context (data only):\n{}", dumps(&data, false)), fed)
}

#[cfg(unix)]
extern "C" {
    // POSIX; the libc crate does not bind it. CPython calls it when `time` is imported.
    fn tzset();
}

/// `aware.astimezone().tzname()`: the zone's abbreviation at that instant, as `localtime_r` names it.
fn tzname(zone: Zone, utc_secs: i64) -> String {
    match zone {
        Zone::Named(tz) => {
            use chrono_tz::OffsetName;
            let at = tz.timestamp_opt(utc_secs, 0).single();
            match at {
                Some(at) => {
                    let off = at.offset();
                    match off.abbreviation() {
                        Some(a) => a.to_string(),
                        None => numeric_zone(chrono::Offset::fix(off).local_minus_utc()),
                    }
                }
                None => String::new(),
            }
        }
        Zone::Local => local_tzname(utc_secs),
    }
}

/// tzdata's name for a zone with no letters, as `tm_zone` carries it: `+0530`.
fn numeric_zone(secs: i32) -> String {
    let sign = if secs < 0 { '-' } else { '+' };
    let a = secs.unsigned_abs();
    if a.is_multiple_of(3600) {
        format!("{sign}{:02}", a / 3600)
    } else {
        format!("{sign}{:02}{:02}", a / 3600, (a % 3600) / 60)
    }
}

/// Python names a timezone whose `tm_zone` is empty by its offset: `UTC-07:00`.
fn utc_offset_name(secs: i32) -> String {
    let sign = if secs < 0 { '-' } else { '+' };
    let a = secs.unsigned_abs();
    format!("UTC{sign}{:02}:{:02}", a / 3600, (a % 3600) / 60)
}

#[cfg(unix)]
fn local_tzname(utc_secs: i64) -> String {
    let t = utc_secs as libc::time_t;
    // SAFETY: tzset has no preconditions; localtime_r writes only into the zeroed tm we own, and a non-null
    // tm_zone points at a NUL-terminated name owned by the C library's zone tables.
    unsafe {
        tzset();
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&t, &mut tm).is_null() {
            return utc_offset_name(Zone::Local.offset_at(utc_secs));
        }
        if tm.tm_zone.is_null() {
            #[allow(clippy::useless_conversion)]
            return utc_offset_name(i64::from(tm.tm_gmtoff) as i32);
        }
        let name = std::ffi::CStr::from_ptr(tm.tm_zone).to_string_lossy().into_owned();
        if name.is_empty() {
            #[allow(clippy::useless_conversion)]
            return utc_offset_name(i64::from(tm.tm_gmtoff) as i32);
        }
        name
    }
}

#[cfg(not(unix))]
fn local_tzname(utc_secs: i64) -> String {
    utc_offset_name(Zone::Local.offset_at(utc_secs))
}

#[cfg(test)]
mod tests;
