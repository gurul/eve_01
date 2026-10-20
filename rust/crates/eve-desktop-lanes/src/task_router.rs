//! The request classifier: which engine should carry a computer-use request. Port of `task_router.py`.
//!
//! Every request that reaches `start_task` used to go to the planner (a screenshot, a 3.4 s turn, a few lines of
//! Python, another turn to say so). The owner's own run logs say what those requests are (79 runs, 2026-09):
//!
//! ```text
//! about a quarter   "Open Spotify", "Open up a terminal", "open Apple TV on my Mac"   8–32 s each
//! about a quarter   "Search Google for …", "pull up the weather in SF on Google"     9–25 s each
//! a handful         in-app navigation ("year view and then next year")               10–19 s each
//! the rest          looking and telling, typing, playing, ordering, long tasks       10–120 s
//! ```
//!
//! The first two rows need no model at all: `open -a Spotify` and one search URL. So the classifier is tiers,
//! cheapest first, and each tier declines unless it is sure:
//!
//! ```text
//! reflex   code only          launch an installed app; open a web search          ~1 s
//! lane     code only          lane_router: a request one labelled control         ~1–2 s
//!                             fully accounts for, clicked before the planner
//! astra    the planner        everything else: vision, reading, typing, judgement 10 s+
//! ```
//!
//! [`classify`] is pure: text in, a Plan out, no I/O. A Plan names the tiers to try IN ORDER; the last is always
//! astra unless a reflex fully answers the request.
//!
//! What makes a tier decline is as important as what makes it engage:
//!
//! - A request that asks to be TOLD something needs eyes. A reflex may still do the first part (open the search)
//!   and the planner starts from there.
//! - A CONSEQUENTIAL request (send, order, delete, quit, kill, pay, post, …) is the planner's, because only the
//!   planner has ask_user. No reflex and no lane touches it.
//! - A request with text to type is the planner's.
//! - A launch reflex needs the app to be INSTALLED (the same list lane_router uses), and the request to be nothing
//!   but the launch once the filler is gone ("on my laptop", "for me").
//! - A search reflex needs explicit search wording and a query.

use std::sync::{LazyLock, OnceLock};

use indexmap::IndexMap;

/// The shipped default for EVE_REFLEXES. Decided 2026-09-21 on holdout 3 (110 requests by an author who saw
/// neither the code nor any other set; the bar — precision ≥ 0.95 on ≥ 15 fired, zero unsafe — was fixed before the
/// first run): 52 fired, 50 right (96.2%), 0 unsafe.
pub const REFLEX_DEFAULT: bool = true;
/// EVE_REFLEXES = launch: ONLY a request that is nothing but a launch of an installed app. Holdout 3: 19 bare
/// launches fired, 19 right, 0 unsafe.
pub const REFLEX_LAUNCH_DEFAULT: bool = true;
/// Wording that means "this can cost the human something": only the planner, with ask_user, may act.
pub static CONSEQUENTIAL: LazyLock<regex::Regex> = LazyLock::new(|| {
    re(concat!(
        r"(?i)\b(send|sent|email (?:him|her|them|it)|reply|post|tweet|publish|order|buy|purchase|pay|checkout|book|",
        r"subscribe|delete|remove|erase|trash|empty|uninstall|install|quit|kill|force quit|close|shut ?down|restart|",
        r"log ?out|sign ?out|sign ?in|log ?in|password|transfer|submit|share|upload|format|reset|turn off)\b",
    ))
});
/// The request wants an answer in words, so somebody has to look at the screen.
pub static TELL_ME: LazyLock<regex::Regex> = LazyLock::new(|| {
    re(concat!(
        r"(?i)\b(tell me|let me know|read (?:it|me|out)|what (?:does|is|are|do)|how (?:many|much)|",
        r"summari[sz]e|summary|give me (?:the |a )?(?:gist|summary|highlights|rundown|short version)|",
        r"check (?:if|whether|who|what|my)|and return|report back|say what)\b",
    ))
});
/// Making content is typing, whatever the verb: "add a reminder to call the dentist" (holdout 1).
pub static TYPING: LazyLock<regex::Regex> = LazyLock::new(|| {
    re(concat!(
        r"(?i)\b(type|write|enter|fill (?:in|out)|paste|dictate|add to notes|take notes|taking notes|note down|jot|compose|draft|",
        r"remind me|(?:add|create|make|start|set) (?:a |an |the |me a |me an )?(?:new )?(?:reminder|event|note|task|to-?do|item|entry|",
        r"contact|appointment|meeting|alarm|timer|playlist|folder|document|doc|file|list|message|email))\b",
    ))
});
/// A request worded as a question out of politeness is still a request ("Can you fire up Slack?").
pub static POLITE: LazyLock<regex::Regex> = LazyLock::new(|| {
    re(concat!(
        r"(?i)^(?:hey eve[,\s]*|eve[,\s]*|ok(?:ay)?[,\s]*)?(?:would you mind\s+|do you mind\s+|",
        r"(?:can|could|would|will) you(?: please| kindly)?\s+|please\s+|i(?:'d| would) like you to\s+|i (?:want|need) you to\s+)+",
    ))
});
/// "would you mind opening Preview" → "open Preview", in Python's dict order.
pub const GERUND: &[(&str, &str)] = &[
    ("opening", "open"),
    ("launching", "launch"),
    ("starting", "start"),
    ("bringing up", "bring up"),
    ("pulling up", "pull up"),
    ("firing up", "fire up"),
    ("searching", "search"),
    ("googling", "google"),
    ("looking up", "look up"),
];
/// Apps whose whole point is writing or sending: whatever follows the launch is content or a message, so the
/// request is the planner's from its first word.
pub const WRITE_APPS: &[&str] = &[
    "messages", "mail", "slack", "notes", "reminders", "calendar", "terminal", "warp", "contacts", "textedit",
    "pages", "discord", "whatsapp", "telegram", "signal", "zoom", "facetime",
];
pub static PRONOUN_QUERY: LazyLock<regex::Regex> = LazyLock::new(|| {
    re(r"(?i)\b(?:pull|bring|put|open|show) (?:that|it|this|them) up\b|^(?:that|it|this|them)\b")
});
/// Where a search looks: "… in Slack", "… in my contacts" is inside an app or the owner's own data, not the web.
/// Has a negative lookahead (`(?!web\b|internet\b)`).
pub static LOCAL_SCOPE: LazyLock<fancy_regex::Regex> = LazyLock::new(|| {
    fancy_regex::Regex::new(
        r"(?i)\b(?:in|inside|on|from|through) (?:my|the) (?!web\b|internet\b)[a-z]+(?: [a-z]+)?\s*$",
    )
    .expect("task_router: LOCAL_SCOPE compiles")
});
pub static FILLER: LazyLock<regex::Regex> = LazyLock::new(|| {
    re(concat!(
        r"(?i)\b(?:on|in|from) (?:my|the|your|this|the owner'?s) (?:mac(?:book)?|laptop|computer|machine|desktop|screen)\b|",
        r"\bfor me\b|\bright now\b|\bplease\b|\breal quick\b|\bquickly\b|\bnow\b|\bthe app\b|\bapp\b|\bapplication\b|",
        r"\bso (?:that )?(?:it|the app) is .*$|\band bring it to the front.*$|\bso you can .*$",
    ))
});
pub static LAUNCH_VERB: LazyLock<regex::Regex> = LazyLock::new(|| {
    re(r"(?i)^(?:open(?: up)?|launch|start(?: up)?|fire up|bring up|pull up|run|go to|switch to|show(?: me)?)\s+")
});
pub static SEARCH_LEAD: LazyLock<regex::Regex> = LazyLock::new(|| {
    re(concat!(
        r"(?i)^(?:(?:do a |run a )?(?:google|web) search (?:for|on|about)|search (?:google|the web|the internet|online) for|",
        r"search (?:up|for)|look up|google|pull up|bring up|find|search)\s+",
    ))
});
pub static SEARCH_TAIL: LazyLock<regex::Regex> = LazyLock::new(|| {
    re(concat!(
        r"(?i)\s+(?:on|in|using|with|via) (?:google(?: search)?|the web|the internet|a web search|",
        r"(?:google )?chrome|safari)\s*$",
    ))
});
pub static NAMES_WEB: LazyLock<regex::Regex> =
    LazyLock::new(|| re(r"(?i)\b(google|web search|the web|the internet|online)\b"));
/// Browser words → the installed browser, in Python's dict order.
pub const BROWSERS: &[(&str, &str)] = &[
    ("safari", "Safari"),
    ("chrome", "Google Chrome"),
    ("google chrome", "Google Chrome"),
    ("firefox", "Firefox"),
    ("arc", "Arc"),
    ("brave", "Brave Browser"),
    ("edge", "Microsoft Edge"),
];
pub static IN_BROWSER: LazyLock<regex::Regex> =
    LazyLock::new(|| re(r"(?i)^in (safari|google chrome|chrome|firefox|arc|brave|edge),?\s+"));
pub static AND_THEN: LazyLock<regex::Regex> =
    LazyLock::new(|| re(r"(?i)\s*(?:,\s*)?\b(?:and then|and|then|after that)\b\s*"));
pub static VAGUE_QUERY: LazyLock<regex::Regex> = LazyLock::new(|| {
    re(r"(?i)\b(something|anything|some (?:cool|fun|good|random)|a cool|cool (?:shit|stuff|thing)|whatever)\b")
});
pub const MAX_QUERY_CHARS: usize = 120;
pub const REFLEX_WAIT_SECS: f64 = 4.0;
pub const SEARCH_URL: &str = "https://www.google.com/search?q=";

/// A pattern this module ships: a compile failure is a bug in this file, never a runtime condition.
fn re(pattern: &str) -> regex::Regex {
    regex::Regex::new(pattern).unwrap_or_else(|e| panic!("task_router: bad pattern {pattern:?}: {e}"))
}

/// The ASCII words `re.findall(r"[a-z0-9.+&-]+", …)` reads out of a case-folded phrase.
static APP_WORDS: LazyLock<regex::Regex> = LazyLock::new(|| re(r"[a-z0-9.+&-]+"));
static LEADING_THE_MY: LazyLock<regex::Regex> = LazyLock::new(|| re(r"(?i)^(?:the|my)\s+"));
static LEADING_ARTICLE: LazyLock<regex::Regex> = LazyLock::new(|| re(r"(?i)^(?:the|a|an|my)\s+"));
static PREPOSITION: LazyLock<regex::Regex> = LazyLock::new(|| re(r"(?i)\b(?:in|inside|on|from|through) "));
static OWN_DATA_SEARCH: LazyLock<regex::Regex> =
    LazyLock::new(|| re(r"(?i)^(?:search|look (?:in|through)) (?:my|our) "));
static TRAILING_JOIN: LazyLock<regex::Regex> = LazyLock::new(|| re(r"(?i)\s*\b(?:and|then|,)\s*$"));
static LEADING_FOR: LazyLock<regex::Regex> = LazyLock::new(|| re(r"(?i)^(?:for|about|up)\s+"));
static ENGINE_NAME: LazyLock<regex::Regex> =
    LazyLock::new(|| re(r"(?i)\s+(?:on|in|using|from) (?:google|the web|the internet)\b"));

/// Python's `str.strip(chars)`.
fn strip_chars<'a>(text: &'a str, chars: &str) -> &'a str {
    text.trim_matches(|c| chars.contains(c))
}

/// Python's `str.rstrip(chars)`.
fn rstrip_chars<'a>(text: &'a str, chars: &str) -> &'a str {
    text.trim_end_matches(|c| chars.contains(c))
}

/// `urllib.parse.quote_plus`: UTF-8 bytes, `A-Za-z0-9_.-~` kept, a space as "+".
fn quote_plus(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 3);
    for &b in text.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-' | b'~' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Python's `str.isprintable()` for one character, as far as names and URLs need it: control characters, the
/// separators other than the space, and the common format characters are not printable.
fn py_printable(c: char) -> bool {
    if c == ' ' {
        return true;
    }
    if c.is_control() || c.is_whitespace() {
        return false;
    }
    !matches!(c,
        '\u{ad}' | '\u{600}'..='\u{605}' | '\u{61c}' | '\u{6dd}' | '\u{70f}' | '\u{180e}'
        | '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{206f}'
        | '\u{feff}' | '\u{fff9}'..='\u{fffb}' | '\u{e000}'..='\u{f8ff}')
}

/// Python's `repr()` of a str: single quotes unless the text has a single quote and no double one.
fn py_repr(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') { '"' } else { '\'' };
    let mut out = String::with_capacity(text.len() + 2);
    out.push(quote);
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if !py_printable(c) => {
                let n = c as u32;
                if n <= 0xff {
                    out.push_str(&format!("\\x{n:02x}"));
                } else if n <= 0xffff {
                    out.push_str(&format!("\\u{n:04x}"));
                } else {
                    out.push_str(&format!("\\U{n:08x}"));
                }
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// What to try, in order. `kind` is the first tier's action; `tiers` always ends in astra unless a reflex fully
/// answers the request (`complete`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// launch | search | quit | quit_all | lane | astra
    pub kind: String,
    /// e.g. ["reflex"] · ["reflex", "astra"] · ["lane", "astra"] · ["astra"]
    pub tiers: Vec<String>,
    /// launch: the installed app's name, as installed
    pub app: String,
    /// search: the query
    pub query: String,
    /// search: a browser the request named, "" for the default
    pub browser: String,
    /// what is left for the planner after the reflex, in the human's words
    pub rest: String,
    pub reasons: Vec<String>,
}

impl Plan {
    /// `Plan(kind, tiers)` with every other field at its default.
    pub fn new(kind: &str, tiers: &[&str]) -> Self {
        Plan {
            kind: kind.to_string(),
            tiers: tiers.iter().map(|t| t.to_string()).collect(),
            app: String::new(),
            query: String::new(),
            browser: String::new(),
            rest: String::new(),
            reasons: Vec::new(),
        }
    }

    /// `Plan(kind, tiers, reasons=(reason,))`.
    fn because(kind: &str, tiers: &[&str], reason: &str) -> Self {
        Plan { reasons: vec![reason.to_string()], ..Plan::new(kind, tiers) }
    }

    /// The reflex alone answers the request: no planner, no lane.
    pub fn complete(&self) -> bool {
        self.tiers.len() == 1 && self.tiers[0] == "reflex"
    }

    /// The one line the desktop worker runs for a reflex ("" for the other kinds), with Python's repr() quoting:
    /// `open_app('Spotify', 4.0)`, `open_url('https://…')`, `open_url('https://…', app='Safari')`. repr() quotes
    /// the slot, and the slot is an installed app's own name or a URL-encoded query, never raw speech.
    pub fn code(&self) -> String {
        if self.kind == "launch" && !self.app.is_empty() {
            // 4 s, not the helper's 8: an app that never takes focus (Preview with no document) must not cost the
            // human eight seconds before the planner is even asked (live run, 2026-09-21).
            return format!("open_app({}, {:?})", py_repr(&self.app), REFLEX_WAIT_SECS);
        }
        if self.kind == "search" && !self.query.is_empty() {
            let url = format!("{SEARCH_URL}{}", quote_plus(&self.query));
            return if self.browser.is_empty() {
                format!("open_url({})", py_repr(&url))
            } else {
                format!("open_url({}, app={})", py_repr(&url), py_repr(&self.browser))
            };
        }
        String::new()
    }

    pub fn sentence(&self) -> String {
        match self.kind.as_str() {
            "launch" => format!("Opened {}.", self.app),
            "quit" => format!("Quit {}.", self.app),
            "search" => {
                let shown = if crate::py_len(&self.query) <= 60 {
                    self.query.clone()
                } else {
                    format!("{}…", crate::py_take(&self.query, 59).trim_end_matches(crate::py_is_space))
                };
                format!("Here's a search for {shown}.")
            }
            _ => String::new(),
        }
    }
}

pub const FINDER: &str = "/System/Library/CoreServices/Finder.app";

static INSTALLED: OnceLock<Vec<String>> = OnceLock::new();

/// `os.listdir`: the entry names of one folder.
fn os_listdir(dir: &str) -> std::io::Result<Vec<String>> {
    std::fs::read_dir(dir)?
        .map(|entry| entry.map(|e| e.file_name().to_string_lossy().into_owned()))
        .collect()
}

/// `os.path.expanduser`: a leading "~" is the home folder ($HOME, else the account's).
fn expanduser(path: &str) -> String {
    let rest = match path.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => rest,
        _ => return path.to_string(),
    };
    let home = std::env::var("HOME")
        .ok()
        .or_else(|| std::env::home_dir().map(|h| h.to_string_lossy().into_owned()));
    match home {
        Some(home) => format!("{}{rest}", home.trim_end_matches('/')),
        None => path.to_string(),
    }
}

/// The scan behind both forms: every `<name>.app` in `dirs`, names sorted and once each.
fn scan_apps(dirs: &[&str], listdir: &dyn Fn(&str) -> std::io::Result<Vec<String>>, finder: bool) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for d in dirs {
        let Ok(entries) = listdir(&expanduser(d)) else {
            continue;
        };
        names.extend(entries.iter().filter_map(|e| e.strip_suffix(".app")).map(str::to_string));
    }
    if finder && std::path::Path::new(FINDER).is_dir() {
        // in CoreServices, not an app folder; "open finder" is a launch like any other
        names.push("Finder".to_string());
    }
    names.sort();
    names.dedup();
    names
}

/// The installed apps' names as installed ("Spotify", "TV", "Google Chrome"), sorted, Finder included when its
/// bundle exists. Read once per process.
pub fn installed_apps() -> Vec<String> {
    INSTALLED.get_or_init(|| scan_apps(&crate::lane_router::APP_DIRS, &os_listdir, true)).clone()
}

/// installed_apps over other folders (`dirs` empty means lane_router::APP_DIRS) or another `listdir`: uncached,
/// and no Finder, as Python's non-default call.
pub fn installed_apps_in(dirs: &[&str], listdir: &dyn Fn(&str) -> std::io::Result<Vec<String>>) -> Vec<String> {
    let dirs: &[&str] = if dirs.is_empty() { &crate::lane_router::APP_DIRS } else { dirs };
    scan_apps(dirs, listdir, false)
}

/// Python `_strip_filler`.
pub fn strip_filler(text: &str) -> String {
    let out = FILLER.replace_all(text, " ");
    let collapsed = crate::py_collapse(&out.replace(" ,", ","));
    strip_chars(&collapsed, " ,.!").to_string()
}

/// Python `str.title()`: a cased character after a cased one is lowered, any other is raised.
fn py_title(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut previous_cased = false;
    for c in text.chars() {
        let cased = c.is_lowercase() || c.is_uppercase();
        if previous_cased {
            out.extend(c.to_lowercase());
        } else {
            out.extend(c.to_uppercase());
        }
        previous_cased = cased;
    }
    out
}

/// Python `_display_names`: {case-folded name: the name as given}. lane_router::installed_app_names() gives folded
/// names only, so a folded name is title-cased for display; callers with real names pass them as they are.
pub fn display_names(apps: &[String]) -> IndexMap<String, String> {
    let mut out = IndexMap::new();
    for name in apps {
        let clean = crate::py_collapse(name);
        if !clean.is_empty() {
            let folded = crate::casefold(&clean);
            let shown = if clean != folded { clean } else { py_title(&clean) };
            out.insert(folded, shown);
        }
    }
    out
}

/// " ".join(re.findall(r"[a-z0-9.+&-]+", text.casefold())).
fn said_words(text: &str) -> String {
    let folded = crate::casefold(text);
    APP_WORDS.find_iter(&folded).map(|m| m.as_str()).collect::<Vec<_>>().join(" ")
}

fn alias_for(said: &str) -> Option<&'static str> {
    APP_ALIASES.iter().find(|(k, _)| *k == said).map(|(_, v)| *v)
}

/// The installed app `phrase` names, exactly — "spotify" → "Spotify", "apple tv" → "TV" only through the alias
/// table, "photobooth" → "Photo Booth" when one app has those letters. "" for none.
pub fn match_app(phrase: &str, apps: &[String]) -> String {
    let names = display_names(apps);
    let said = said_words(phrase);
    if said.is_empty() {
        return String::new();
    }
    if let Some(shown) = names.get(&said) {
        return shown.clone();
    }
    if let Some(alias) = alias_for(&said) {
        if let Some(shown) = names.get(&crate::casefold(alias)) {
            return shown.clone();
        }
    }
    // "photobooth" → "Photo Booth": the same letters with the spaces dropped, and only when one app has them.
    let squashed = said.replace(' ', "");
    let joined: Vec<&String> =
        names.iter().filter(|(folded, _)| folded.replace(' ', "") == squashed).map(|(_, shown)| shown).collect();
    if joined.len() == 1 {
        joined[0].clone()
    } else {
        String::new()
    }
}

/// What people say → what is installed. Closed and small on purpose: a wrong alias opens the wrong app.
pub const APP_ALIASES: &[(&str, &str)] = &[
    ("apple tv", "TV"),
    ("chrome", "Google Chrome"),
    ("settings", "System Settings"),
    ("system preferences", "System Settings"),
    ("preferences", "System Settings"),
    ("apple music", "Music"),
    ("itunes", "Music"),
    ("vs code", "Visual Studio Code"),
    ("vscode", "Visual Studio Code"),
    ("imessage", "Messages"),
];

// Quitting (owner, 2026-09-23): only the explicit word QUIT, a graceful quit (Cmd+Q: an app with unsaved work asks
// to save), never "close", "exit", "kill" or "force quit" — those stay the planner's. "quit all" quits every
// regular app except the ones eve works through (app_reflex.QUIT_ALL_KEEP).
pub static QUIT_VERB: LazyLock<regex::Regex> = LazyLock::new(|| re(r"(?i)^(?:go ahead and |just )?quit(?: out of)?\s+"));
pub static QUIT_ALL_TARGET: LazyLock<regex::Regex> = LazyLock::new(|| {
    re(concat!(
        r"(?i)^(?:all|everything|every ?thing|all of (?:it|them)|every (?:app|application|program)|",
        r"all (?:of )?(?:my |the |the open |my open |open |running |the running )?(?:apps|applications|programs)",
        r"(?: (?:that are|i have|i've got) (?:open|running))?)$",
    ))
});
pub static FORCE: LazyLock<regex::Regex> = LazyLock::new(|| re(r"(?i)\bforce\b"));
pub static QUIT_WORD: LazyLock<regex::Regex> = LazyLock::new(|| re(r"(?i)\bquit\b"));

/// Python `_quit`: the installed app a request asks to quit, "*" for quit all, None when it is not nothing but that.
pub fn quit(text: &str, apps: &[String]) -> Option<String> {
    if FORCE.is_match(text) {
        return None;
    }
    let t = strip_filler(text);
    if AND_THEN.is_match(&t) && !QUIT_ALL_TARGET.is_match(QUIT_VERB.replacen(&t, 1, "").trim_matches(crate::py_is_space)) {
        return None; // "quit Spotify and open Music": the planner's
    }
    let m = QUIT_VERB.find(&t)?;
    let target = LEADING_THE_MY.replace_all(strip_chars(&t[m.end()..], " ,.!"), "").into_owned();
    if QUIT_ALL_TARGET.is_match(&target) {
        return Some("*".to_string());
    }
    let app = match_app(&target, apps);
    if app.is_empty() {
        None
    } else {
        Some(app)
    }
}

/// Python `_first_clause`: ("open spotify", "play don toliver") from "open spotify and play don toliver".
pub fn first_clause(text: &str) -> (String, String) {
    let parts: Vec<&str> = AND_THEN.splitn(text, 2).collect();
    let head = strip_chars(parts[0], " ,.").to_string();
    let rest = parts.get(1).map(|r| strip_chars(r, " ,.").to_string()).unwrap_or_default();
    (head, rest)
}

/// Python `_launch`: (app, rest) when the first clause is nothing but a launch of an installed app.
pub fn launch(text: &str, apps: &[String]) -> Option<(String, String)> {
    let (head, rest) = first_clause(&strip_filler(text));
    let m = LAUNCH_VERB.find(&head)?;
    let target = strip_filler(&head[m.end()..]);
    let target = LEADING_ARTICLE.replace_all(&target, "").into_owned();
    let app = match_app(&target, apps);
    if app.is_empty() {
        return None;
    }
    Some((app, strip_filler(&rest)))
}

pub static IN_APP_SEARCH: LazyLock<regex::Regex> =
    LazyLock::new(|| re(r"(?i)^(?:search|look)\s+(?:in(?:side)?\s+|through\s+|on\s+)?(.+?)\s+for\s+"));

fn browser_for(word: &str) -> &'static str {
    BROWSERS.iter().find(|(k, _)| *k == word).map(|(_, v)| *v).unwrap_or("")
}

/// Python `_search`: (query, browser, rest) when the request is an explicit web search with a real query.
pub fn search(text: &str, apps: &[String]) -> Option<(String, String, String)> {
    let mut browser = String::new();
    let mut text = text;
    if let Some(m) = IN_BROWSER.captures(text) {
        browser = browser_for(&crate::casefold(&m[1])).to_string();
        text = &text[m.get(0).map_or(0, |g| g.end())..];
    }
    if let Some(inside) = IN_APP_SEARCH.captures(text) {
        let named = &inside[1];
        if !match_app(named, apps).is_empty() && !["google", "the web"].contains(&crate::casefold(named).as_str()) {
            return None; // "search Spotify for jazz" is a search INSIDE that app
        }
    }
    let stripped = strip_filler(text);
    let bare = rstrip_chars(&stripped, " .!?");
    for prep in PREPOSITION.find_iter(bare) {
        // every "in …" suffix, so "from Harbor Supply in Mail" finds Mail
        let place = match_app(&bare[prep.end()..], apps);
        if !place.is_empty() && !BROWSERS.iter().any(|(_, v)| *v == place) {
            return None; // "search for the budget thread in Slack": inside that app
        }
    }
    if OWN_DATA_SEARCH.is_match(text) {
        return None; // "search my photos for the beach trip": the owner's own data
    }
    if LOCAL_SCOPE.is_match(rstrip_chars(&stripped, " .!")).unwrap_or(false) {
        return None; // "look up Priya in my contacts": the owner's own data
    }
    let lead = SEARCH_LEAD.find(text)?;
    let verb = crate::casefold(lead.as_str());
    let body = &text[lead.end()..];
    let explicit = verb.contains("search") || verb.starts_with("google") || verb.starts_with("look up");
    if !explicit && !NAMES_WEB.is_match(body) {
        return None; // "pull up the budget" is not a web search unless it says so
    }
    let (mut head, mut rest) = (body.to_string(), String::new());
    if let Some(tell) = TELL_ME.find(body) {
        rest = body[tell.start()..].to_string();
        head = TRAILING_JOIN.replace_all(crate::py_strip(&body[..tell.start()]), "").into_owned();
    }
    let mut head = strip_filler(&head);
    if let Some(tail) = SEARCH_TAIL.find(&head) {
        let named = crate::casefold(tail.as_str());
        for (key, value) in BROWSERS {
            if named.contains(key) && browser.is_empty() {
                browser = value.to_string();
            }
        }
        head.truncate(tail.start());
    }
    let query = strip_filler(&head);
    let query = LEADING_FOR.replace_all(&query, "").into_owned();
    // "an article on Google about the summit": the engine's name goes, the words after it stay
    let query = ENGINE_NAME.replace_all(&query, "").into_owned();
    let mut query = strip_chars(&crate::py_collapse(&query), " ,.'").to_string();
    if query.matches('"').count() % 2 == 1 {
        query = query.replace('"', "");
    }
    let query = crate::py_strip(&query).to_string();
    let n = crate::py_len(&query);
    if !(3..=MAX_QUERY_CHARS).contains(&n) || VAGUE_QUERY.is_match(&query) || PRONOUN_QUERY.is_match(&query) {
        return None; // "Google Maps, pull that up" has no query, only a pointer
    }
    Some((query, browser, strip_filler(&rest)))
}

// ---- a typed-decision model behind the code gates ------------------------------------------------
//
// What a model is for here is the wording the rules do not know ("Notes, please.", "Take me to Safari."). It is
// asked only AFTER the code gates (consequential, typing, a question) have passed the request and the rules have
// found no reflex, and it may only add a BARE LAUNCH of an app that is really installed. How it is asked matters
// more than which model it is (typed_ask): Jev asked one relative question fired 8 unsafe reflexes in 100
// requests; asked its own way — absolute nouls in one request, the app chosen from the installed list — it fired
// none in 110 and lifted the coverage of the bare reflexes from 74% to 96%. `model(goal, apps) -> app name or ""`;
// a failure is an abstention.
pub const ROUTER_MODELS: [&str; 2] = ["off", "jev"];
/// hosted: the request's words leave the Mac, so it is the owner's switch
pub const ROUTER_MODEL_DEFAULT: &str = "off";

/// The ONE installed app the text mentions as whole words (aliases included), "" for none or two.
pub fn find_app_mention(text: &str, apps: &[String]) -> String {
    let found = app_mentions(text, apps);
    if found.len() == 1 {
        found[0].clone()
    } else {
        String::new()
    }
}

/// Every installed app the text mentions as whole words (aliases included), in the order they are said. A name
/// inside a longer one that is also said ("Photo" in "Photo Booth") is not counted twice.
pub fn app_mentions(text: &str, apps: &[String]) -> Vec<String> {
    let names = display_names(apps);
    let folded = format!(" {} ", said_words(text));
    let mut spoken: IndexMap<String, String> = names.clone();
    for (alias, target) in APP_ALIASES {
        if let Some(shown) = names.get(&crate::casefold(target)) {
            spoken.insert(alias.to_string(), shown.clone());
        }
    }
    let mut by_length: Vec<&String> = spoken.keys().collect();
    by_length.sort_by_key(|said| std::cmp::Reverse(crate::py_len(said)));
    let mut found: IndexMap<String, usize> = IndexMap::new();
    for said in by_length {
        let Some(at) = folded.find(&format!(" {said} ")) else {
            continue;
        };
        let inside_longer = spoken.keys().any(|longer| {
            crate::py_len(longer) > crate::py_len(said)
                && longer.contains(said.as_str())
                && folded.contains(&format!(" {longer} "))
        });
        if !inside_longer {
            found.entry(spoken[said].clone()).or_insert(at);
        }
    }
    let mut ordered: Vec<(String, usize)> = found.into_iter().collect();
    ordered.sort_by_key(|(_, at)| *at);
    ordered.into_iter().map(|(app, _)| app).collect()
}

pub static EXPLICIT_SEARCH: LazyLock<regex::Regex> =
    LazyLock::new(|| re(r"(?i)^(?:(?:do a |run a )?(?:google|web) search\b|search\b|google\b|look up\b)"));

/// Python `_explicit_search`.
pub fn explicit_search(text: &str) -> bool {
    EXPLICIT_SEARCH.is_match(text)
}

/// `pattern.match(text)`: a match that begins at the first character.
fn matches_at_start(pattern: &regex::Regex, text: &str) -> bool {
    pattern.find(text).is_some_and(|m| m.start() == 0)
}

/// Python `_consequential`: does the request ask for something only the planner (with ask_user) may do?
///
/// A web search is harmless whatever its query says, so in an explicit search the risky words of the QUERY do not
/// count ("google how to reset a Casio watch") — but a later clause that itself starts with a risky verb does
/// ("search for earbuds and buy the cheapest pair").
pub fn consequential(text: &str) -> bool {
    if !explicit_search(text) {
        return CONSEQUENTIAL.is_match(text);
    }
    AND_THEN
        .split(text)
        .skip(1)
        .any(|clause| matches_at_start(&CONSEQUENTIAL, crate::py_strip(clause)))
}

/// `model(goal, installed apps) -> app or ""` (or `quit_model(...) -> app, "*" or ""`); an error abstains.
pub type RouterModel<'a> = &'a dyn Fn(&str, &[String]) -> anyhow::Result<String>;

/// Ask a router model; an error (or a panic inside it) is an abstention, as Python's `except Exception`.
fn ask_model(model: RouterModel<'_>, text: &str, installed: &[String]) -> String {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| model(text, installed))) {
        Ok(Ok(said)) => said,
        _ => String::new(),
    }
}

/// The Plan for one request (the module docs have the rules). Pure unless `model` is given (asked only when the
/// gates pass and the rules find no reflex) or `quit_model` (asked only for a request that says "quit", not
/// "force", is no question, and that the quit rules did not take); never fails.
pub fn classify(
    goal: &str,
    frontmost_app: &str,
    apps: &[String],
    model: Option<RouterModel<'_>>,
    quit_model: Option<RouterModel<'_>>,
) -> Plan {
    let installed = apps;
    let raw = crate::py_collapse(goal);
    let polite = POLITE.is_match(&raw);
    // "git hub" → "GitHub", as speech splits it
    let mut text = eve_browser_intent::join_site_names(&crate::lane_router::normalise(&POLITE.replace_all(&raw, "")));
    for (ing, base) in GERUND {
        // "would you mind opening Preview" → "open Preview"
        if crate::casefold(&text).starts_with(&format!("{ing} ")) {
            let tail: String = text.chars().skip(ing.chars().count()).collect();
            text = format!("{base}{tail}");
        }
    }
    let mut reasons: Vec<String> = Vec::new();
    if text.is_empty() {
        return Plan::because("astra", &["astra"], "empty");
    }
    if QUIT_WORD.is_match(&text) && !FORCE.is_match(&text) && !crate::lane_router::is_question(&text, polite) {
        let mut target = quit(crate::py_strip(text.trim_end_matches('?')), installed);
        let mut how = "the quit rules";
        if target.is_none() {
            if let Some(quit_model) = quit_model {
                let said = ask_model(quit_model, &text, installed);
                target = (said == "*" || installed.contains(&said)).then_some(said);
                how = "the model, for wording the quit rules did not know";
            }
        }
        match target.as_deref() {
            Some("*") => return Plan::because("quit_all", &["reflex"], &format!("{how}: quit every app but eve's own")),
            Some(app) if !app.is_empty() => {
                return Plan {
                    app: app.to_string(),
                    ..Plan::because("quit", &["reflex"], &format!("{how}: a bare quit of an installed app"))
                };
            }
            _ => {}
        }
    }
    if consequential(&text) {
        return Plan::because("astra", &["astra"], "consequential: only the planner can ask first");
    }
    if TYPING.is_match(&text) && !explicit_search(&text) {
        return Plan::because("astra", &["astra"], "text to type: the planner's");
    }
    // "?" alone is politeness when the request began "can you …"; a question word in front is a question
    let question = crate::lane_router::is_question(&text, polite);
    let text = crate::py_strip(text.trim_end_matches('?')).to_string();
    if let Some((app, rest)) = launch(&text, apps).filter(|_| !question) {
        if crate::casefold(&app) == crate::py_collapse(&crate::casefold(frontmost_app)) && rest.is_empty() {
            reasons.push("already frontmost".to_string());
        }
        if !rest.is_empty() && WRITE_APPS.contains(&crate::casefold(&app).as_str()) {
            return Plan::because(
                "astra",
                &["astra"],
                &format!("{app} is for writing or sending: what follows is the planner's"),
            );
        }
        if !rest.is_empty() && TELL_ME.is_match(&rest) {
            // "open Music and tell me what's playing": the answer is the job, and only the planner has eyes
            return Plan::because("astra", &["astra"], "wants an answer in words: needs eyes");
        }
        if !rest.is_empty() {
            reasons.push("launch, then the planner does the rest".to_string());
            return Plan { app, rest, reasons, ..Plan::new("launch", &["reflex", "astra"]) };
        }
        reasons.push("nothing but a launch of an installed app".to_string());
        return Plan { app, reasons, ..Plan::new("launch", &["reflex"]) };
    }
    if let Some((query, browser, rest)) = search(&text, apps) {
        if !rest.is_empty() || TELL_ME.is_match(&text) {
            let rest = if rest.is_empty() { "read the result".to_string() } else { rest };
            return Plan {
                query,
                browser,
                rest,
                ..Plan::because("search", &["reflex", "astra"], "search, then the planner reads it")
            };
        }
        return Plan { query, browser, ..Plan::because("search", &["reflex"], "an explicit web search with a query") };
    }
    if question || TELL_ME.is_match(&text) {
        return Plan::because("astra", &["astra"], "wants an answer in words: needs eyes");
    }
    if let Some(model) = model {
        // a model that fails is an abstention: the lane, then the planner
        let app = ask_model(model, &text, installed);
        if !app.is_empty() && installed.contains(&app) {
            return Plan {
                app,
                ..Plan::because(
                    "launch",
                    &["reflex"],
                    "the rules did not know the wording; the model named a bare launch of an installed app",
                )
            };
        }
    }
    Plan::because("lane", &["lane", "astra"], "the lane may try; it declines unless one control covers every word")
}
