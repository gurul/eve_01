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

use std::sync::LazyLock;

use indexmap::IndexMap;

/// The shipped default for EVE_REFLEXES. Decided 2026-09-21 on holdout 3 (110 requests by an author who saw
/// neither the code nor any other set; the bar — precision ≥ 0.95 on ≥ 15 fired, zero unsafe — was fixed before the
/// first run): 52 fired, 50 right (96.2%), 0 unsafe.
pub const REFLEX_DEFAULT: bool = true;
/// EVE_REFLEXES = launch: ONLY a request that is nothing but a launch of an installed app. Holdout 3: 19 bare
/// launches fired, 19 right, 0 unsafe.
pub const REFLEX_LAUNCH_DEFAULT: bool = true;

/// Wording that means "this can cost the human something": only the planner, with ask_user, may act.
pub static CONSEQUENTIAL: LazyLock<regex::Regex> = LazyLock::new(|| todo!("task_router: CONSEQUENTIAL"));
/// The request wants an answer in words, so somebody has to look at the screen.
pub static TELL_ME: LazyLock<regex::Regex> = LazyLock::new(|| todo!("task_router: TELL_ME"));
/// Making content is typing, whatever the verb: "add a reminder to call the dentist" (holdout 1).
pub static TYPING: LazyLock<regex::Regex> = LazyLock::new(|| todo!("task_router: TYPING"));
/// A request worded as a question out of politeness is still a request ("Can you fire up Slack?").
pub static POLITE: LazyLock<regex::Regex> = LazyLock::new(|| todo!("task_router: POLITE"));
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
pub static PRONOUN_QUERY: LazyLock<regex::Regex> = LazyLock::new(|| todo!("task_router: PRONOUN_QUERY"));
/// Has a negative lookahead (`(?!web\b|internet\b)`).
pub static LOCAL_SCOPE: LazyLock<fancy_regex::Regex> = LazyLock::new(|| todo!("task_router: LOCAL_SCOPE"));
pub static FILLER: LazyLock<regex::Regex> = LazyLock::new(|| todo!("task_router: FILLER"));
pub static LAUNCH_VERB: LazyLock<regex::Regex> = LazyLock::new(|| todo!("task_router: LAUNCH_VERB"));
pub static SEARCH_LEAD: LazyLock<regex::Regex> = LazyLock::new(|| todo!("task_router: SEARCH_LEAD"));
pub static SEARCH_TAIL: LazyLock<regex::Regex> = LazyLock::new(|| todo!("task_router: SEARCH_TAIL"));
pub static NAMES_WEB: LazyLock<regex::Regex> = LazyLock::new(|| todo!("task_router: NAMES_WEB"));
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
pub static IN_BROWSER: LazyLock<regex::Regex> = LazyLock::new(|| todo!("task_router: IN_BROWSER"));
pub static AND_THEN: LazyLock<regex::Regex> = LazyLock::new(|| todo!("task_router: AND_THEN"));
pub static VAGUE_QUERY: LazyLock<regex::Regex> = LazyLock::new(|| todo!("task_router: VAGUE_QUERY"));
pub const MAX_QUERY_CHARS: usize = 120;
pub const REFLEX_WAIT_SECS: f64 = 4.0;
pub const SEARCH_URL: &str = "https://www.google.com/search?q=";

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

    /// The reflex alone answers the request: no planner, no lane.
    pub fn complete(&self) -> bool {
        todo!("task_router: Plan.complete")
    }

    /// The one line the desktop worker runs for a reflex ("" for the other kinds), with Python's repr() quoting:
    /// `open_app('Spotify', 4.0)`, `open_url('https://…')`, `open_url('https://…', app='Safari')`.
    pub fn code(&self) -> String {
        todo!("task_router: Plan.code")
    }

    pub fn sentence(&self) -> String {
        todo!("task_router: Plan.sentence")
    }
}

pub const FINDER: &str = "/System/Library/CoreServices/Finder.app";

/// The installed apps' names as installed ("Spotify", "TV", "Google Chrome"), sorted, Finder included when its
/// bundle exists. Read once per process.
pub fn installed_apps() -> Vec<String> {
    todo!("task_router: installed_apps")
}

/// installed_apps over other folders (`dirs` empty means lane_router::APP_DIRS) or another `listdir`: uncached,
/// and no Finder, as Python's non-default call.
pub fn installed_apps_in(dirs: &[&str], listdir: &dyn Fn(&str) -> std::io::Result<Vec<String>>) -> Vec<String> {
    todo!("task_router: installed_apps (dirs, listdir)")
}

/// Python `_strip_filler`.
pub fn strip_filler(text: &str) -> String {
    todo!("task_router: _strip_filler")
}

/// Python `_display_names`: {case-folded name: the name as given}. A folded name is title-cased for display.
pub fn display_names(apps: &[String]) -> IndexMap<String, String> {
    todo!("task_router: _display_names")
}

/// The installed app `phrase` names, exactly — "spotify" → "Spotify", "apple tv" → "TV" only through the alias
/// table, "photobooth" → "Photo Booth" when one app has those letters. "" for none.
pub fn match_app(phrase: &str, apps: &[String]) -> String {
    todo!("task_router: match_app")
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
pub static QUIT_VERB: LazyLock<regex::Regex> = LazyLock::new(|| todo!("task_router: QUIT_VERB"));
pub static QUIT_ALL_TARGET: LazyLock<regex::Regex> = LazyLock::new(|| todo!("task_router: QUIT_ALL_TARGET"));
pub static FORCE: LazyLock<regex::Regex> = LazyLock::new(|| todo!("task_router: FORCE"));
pub static QUIT_WORD: LazyLock<regex::Regex> = LazyLock::new(|| todo!("task_router: QUIT_WORD"));

/// Python `_quit`: the installed app a request asks to quit, "*" for quit all, None when it is not nothing but that.
pub fn quit(text: &str, apps: &[String]) -> Option<String> {
    todo!("task_router: _quit")
}

/// Python `_first_clause`: ("open spotify", "play don toliver") from "open spotify and play don toliver".
pub fn first_clause(text: &str) -> (String, String) {
    todo!("task_router: _first_clause")
}

/// Python `_launch`: (app, rest) when the first clause is nothing but a launch of an installed app.
pub fn launch(text: &str, apps: &[String]) -> Option<(String, String)> {
    todo!("task_router: _launch")
}

pub static IN_APP_SEARCH: LazyLock<regex::Regex> = LazyLock::new(|| todo!("task_router: IN_APP_SEARCH"));

/// Python `_search`: (query, browser, rest) when the request is an explicit web search with a real query.
pub fn search(text: &str, apps: &[String]) -> Option<(String, String, String)> {
    todo!("task_router: _search")
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
    todo!("task_router: find_app_mention")
}

/// Every installed app the text mentions as whole words (aliases included), in the order they are said. A name
/// inside a longer one that is also said ("Photo" in "Photo Booth") is not counted twice.
pub fn app_mentions(text: &str, apps: &[String]) -> Vec<String> {
    todo!("task_router: app_mentions")
}

pub static EXPLICIT_SEARCH: LazyLock<regex::Regex> = LazyLock::new(|| todo!("task_router: EXPLICIT_SEARCH"));

/// Python `_explicit_search`.
pub fn explicit_search(text: &str) -> bool {
    todo!("task_router: _explicit_search")
}

/// Python `_consequential`: does the request ask for something only the planner (with ask_user) may do? In an
/// explicit search the risky words of the QUERY do not count, but a later clause that itself starts with a risky
/// verb does.
pub fn consequential(text: &str) -> bool {
    todo!("task_router: _consequential")
}

/// `model(goal, installed apps) -> app or ""` (or `quit_model(...) -> app, "*" or ""`); an error abstains.
pub type RouterModel<'a> = &'a dyn Fn(&str, &[String]) -> anyhow::Result<String>;

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
    todo!("task_router: classify")
}
