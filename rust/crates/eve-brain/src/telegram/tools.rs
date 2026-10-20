//! A port of `telegram.py`, lines 716-1166: the text brain's tools, the apps blocks, the intent lines, tools_for.
//!
//! The constants are the Python values, verbatim; the tests in `tests/telegram_prompts.rs` hold the derived ones
//! (TOOL_NAMES, APPS_BLOCK_REST_NO_WEB, LENT_LABELS' keys) to their derivations.

#[allow(unused_imports)]
use crate::prelude::*;
#[allow(unused_imports)]
use crate::telegram::*;

/// py: telegram.py:716 `TOOLS`
/// The Python value as JSON (tuples are arrays).
pub static TOOLS: LazyLock<Value> =
    LazyLock::new(|| serde_json::from_str(r#"[{"type": "function", "name": "start_task", "strict": true, "description": "Delegate UI work on the owner's Mac to Codex's existing Computer Use. Returns at once; the result is texted to the owner when the task finishes.", "parameters": {"type": "object", "additionalProperties": false, "required": ["goal"], "properties": {"goal": {"type": "string", "description": "What the owner asked for, in their words."}}}}, {"type": "function", "name": "steer_task", "strict": true, "description": "Change what the running task is doing.", "parameters": {"type": "object", "additionalProperties": false, "required": ["text"], "properties": {"text": {"type": "string", "description": "The new instruction."}}}}, {"type": "function", "name": "stop_task", "strict": true, "description": "Stop the running task.", "parameters": {"type": "object", "additionalProperties": false, "required": [], "properties": {}}}, {"type": "function", "name": "take_photo", "strict": true, "description": "Take a picture with the Mac's camera (the robot's, when it is connected) and send it to the owner in this chat.", "parameters": {"type": "object", "additionalProperties": false, "required": ["note"], "properties": {"note": {"type": "string", "description": "What the owner wanted a picture of."}}}}, {"type": "function", "name": "screenshot", "strict": true, "description": "Send a picture of the Mac's screen, as it is right now, to the owner in this chat.", "parameters": {"type": "object", "additionalProperties": false, "required": ["caption"], "properties": {"caption": {"type": "string", "description": "One short line to send with it."}}}}, {"type": "function", "name": "send_file", "strict": true, "description": "Send a file from the owner's Mac to them in this chat (up to 50 MB). Files in their home folder only; hidden folders are off limits.", "parameters": {"type": "object", "additionalProperties": false, "required": ["path", "caption"], "properties": {"path": {"type": "string", "description": "Absolute path, or ~/…"}, "caption": {"type": "string", "description": "One short line, or empty."}}}}, {"type": "function", "name": "list_files", "strict": true, "description": "List a folder on the owner's Mac, newest first, with sizes — to find the file they mean. Home folder only; hidden folders are off limits.", "parameters": {"type": "object", "additionalProperties": false, "required": ["path"], "properties": {"path": {"type": "string", "description": "Absolute path, or ~/… (~/Desktop, ~/Downloads, ~/Documents)"}}}}, {"type": "function", "name": "look", "strict": true, "description": "Look through the robot's camera now: one sentence of what it sees, and how fresh that is.", "parameters": {"type": "object", "additionalProperties": false, "required": [], "properties": {}}}, {"type": "function", "name": "look_around", "strict": true, "description": "Pan the robot's head across the room, looking at each stop, then face front again. Returns what it saw in each direction. Takes several seconds.", "parameters": {"type": "object", "additionalProperties": false, "required": [], "properties": {}}}, {"type": "function", "name": "find", "strict": true, "description": "Search for something with the robot's camera — first where it is looking, then across the room — and turn to face it. Takes several seconds.", "parameters": {"type": "object", "additionalProperties": false, "required": ["target"], "properties": {"target": {"type": "string", "description": "What to find, in the owner's words."}}}}, {"type": "function", "name": "move_head", "strict": true, "description": "Turn the robot's head to a pose, or by an offset. Robot-centred degrees: yaw 0 faces the desk chair, negative = its left, positive = its right, limit ±120. pitch 45 level, 5 down at the desk, 85 up at the ceiling. Use null to keep an axis.", "parameters": {"type": "object", "additionalProperties": false, "required": ["yaw", "pitch", "relative", "hold_secs"], "properties": {"yaw": {"type": ["number", "null"]}, "pitch": {"type": ["number", "null"]}, "relative": {"type": "boolean", "description": "true: add to the current pose."}, "hold_secs": {"type": ["number", "null"], "description": "1-60 s; null for the default."}}}}, {"type": "function", "name": "go_explore", "strict": true, "description": "Send the robot off to explore the room on its own: it pans the room and takes notes.", "parameters": {"type": "object", "additionalProperties": false, "required": [], "properties": {}}}, {"type": "function", "name": "take_notes", "strict": true, "description": "Start or stop the robot taking notes of what is said in the room (its microphone, transcribed into a notes file), or ask whether it is.", "parameters": {"type": "object", "additionalProperties": false, "required": ["action"], "properties": {"action": {"type": "string", "enum": ["start", "stop", "status"]}}}}, {"type": "function", "name": "set_sound", "strict": true, "description": "Mute or unmute the robot. Muted: no beeps, chirps or voice; it still moves and lights up.", "parameters": {"type": "object", "additionalProperties": false, "required": ["on"], "properties": {"on": {"type": "boolean", "description": "false mutes, true turns sound on."}}}}, {"type": "function", "name": "remember", "strict": true, "description": "The owner said to remember something for good ('remember that …'). Stars it in your permanent memory. Only for what they explicitly asked you to remember.", "parameters": {"type": "object", "additionalProperties": false, "required": ["claim"], "properties": {"claim": {"type": "string", "description": "The fact, in one line, as they said it."}}}}, {"type": "function", "name": "start_coding_session", "strict": true, "description": "Open a new terminal on the owner's Mac running a coding agent in one of their project folders under ~/Documents. It asks the owner in this chat for anything left empty and texts the result itself.", "parameters": {"type": "object", "additionalProperties": false, "required": ["area", "folder", "harness"], "properties": {"area": {"type": "string", "enum": ["personal", "work", ""], "description": "Which side, when the owner said it; otherwise empty."}, "folder": {"type": "string", "description": "The project folder as the owner named it, 'general' for the whole area, or empty to ask."}, "harness": {"type": "string", "enum": ["claude", "era-code", ""], "description": "Empty unless the owner explicitly asked for one by name. Empty follows the area: personal runs claude, work runs era-code."}}}}, {"type": "function", "name": "think_hard", "strict": true, "description": "Hand a hard question to the slow, careful brain. Takes up to a minute.", "parameters": {"type": "object", "additionalProperties": false, "required": ["question"], "properties": {"question": {"type": "string", "description": "The whole question, self-contained."}}}}]"#).expect("TOOLS"));

/// py: telegram.py:850 `TOOL_NAMES`
pub const TOOL_NAMES: &[&str] = &["start_task", "steer_task", "stop_task", "take_photo", "screenshot", "send_file", "list_files", "look", "look_around", "find", "move_head", "go_explore", "take_notes", "set_sound", "remember", "start_coding_session", "think_hard", "web_search"];

/// py: telegram.py:855 `APPS_BLOCK`
pub const APPS_BLOCK: &str = "

Your owner's Slack, Notion, Linear and other connected apps are reachable through the COMPOSIO tools, and a
job inside one of them is done there, not on the Mac's screen: \"what did Priya say in #design\", \"add a Linear
issue for the login bug\", \"find my trip notes in Notion\". First COMPOSIO_SEARCH_TOOLS with the use case, then
COMPOSIO_MULTI_EXECUTE_TOOL with the exact slugs and arguments it returned; pass the session_id it gives you to
every later call.";

/// py: telegram.py:865 `APPS_BLOCK_REST`
pub const APPS_BLOCK_REST: &str = " Some actions are asked of the owner first in this chat by code; you do not need to ask
twice. If an app is not connected, COMPOSIO_MANAGE_CONNECTIONS returns a sign-in link: send the owner that link
as it is. Summarise what came back in your own few words; never paste a raw record. The COMPOSIO tools are for the owner's own
accounts. Anything on the public web (a course page, opening hours, a product, the news) is looked up with
web_search, which reads the pages and answers in one step; a job that needs both, like finding a date on a
website and filing it in Linear, is web_search for the finding and COMPOSIO for Linear.";

/// py: telegram.py:872 `APPS_BLOCK_REST_NO_WEB`
pub const APPS_BLOCK_REST_NO_WEB: &str = " Some actions are asked of the owner first in this chat by code; you do not need to ask
twice. If an app is not connected, COMPOSIO_MANAGE_CONNECTIONS returns a sign-in link: send the owner that link
as it is. Summarise what came back in your own few words; never paste a raw record. The COMPOSIO tools are for the owner's own
accounts. Nothing on the public web (a course page, opening hours, a product, the news) can be looked up from here.";

/// py: telegram.py:876 `def apps_direct_line(direct_apps: Sequence[str] = ()) -> str:`
///
/// The APPS_BLOCK sentence that keeps the direct apps out of COMPOSIO, naming only the ones that are up
/// (apps_mcp.AppsBridge.apps), in its own order; "" with none up.
pub fn apps_direct_line(direct_apps: &[String]) -> String {
    use eve_apps_mcp::{SEP, SERVERS};

    let up: Vec<&str> = direct_apps.iter().map(String::as_str).filter(|a| SERVERS.contains_key(a)).collect();
    if up.is_empty() {
        return String::new();
    }
    let labels: Vec<&str> = up.iter().map(|a| SERVERS[a].label).collect();
    let named = if labels.len() == 1 {
        labels[0].to_string()
    } else {
        format!("{} and {}", labels[..labels.len() - 1].join(", "), labels[labels.len() - 1])
    };
    let (verb, they) = if up.len() == 1 { ("is", "it has its") } else { ("are", "they have their") };
    let prefixes: Vec<String> = up.iter().map(|a| format!("{a}{SEP}…")).collect();
    format!(" The owner's own {named} {verb} not in COMPOSIO: {they} own tools ({}).", prefixes.join(", "))
}

/// py: telegram.py:894 `APPS_BLOCK_LITE`
pub const APPS_BLOCK_LITE: &str = "

The mail, calendar, files and work apps the owner connected are reachable through the COMPOSIO tools, and a job inside one of them is done there.";

/// py: telegram.py:896 `_APPS_LITE_FIND`
pub(crate) const APPS_LITE_FIND: &str = " First COMPOSIO_SEARCH_TOOLS with the use case, then COMPOSIO_MULTI_EXECUTE_TOOL with the exact slugs and arguments it returned; pass the session_id it gives you to every later call.";

/// py: telegram.py:898 `_APPS_LITE_ASKED`
pub(crate) const APPS_LITE_ASKED: &str = " Some actions are asked of the owner first in this chat by code; you do not need to ask twice.";

/// py: telegram.py:899 `_APPS_LITE_LINK`
pub(crate) const APPS_LITE_LINK: &str = " If an app they ask about is not connected, COMPOSIO_MANAGE_CONNECTIONS returns a sign-in link: send the owner that link as it is, in this reply. When they say they have connected it, check it with COMPOSIO_MANAGE_CONNECTIONS before you use that app.";

/// py: telegram.py:902 `_APPS_LITE_SUMMARY`
pub(crate) const APPS_LITE_SUMMARY: &str = " Summarise what came back in your own few words; never paste a raw record. The COMPOSIO tools are for the owner's own accounts.";

/// py: telegram.py:904 `_APPS_LITE_WEB`
pub(crate) const APPS_LITE_WEB: &str = " Anything on the public web (a course page, opening hours, a product, the news) is looked up with web_search, which reads the pages and answers in one step; a job that needs both, like finding a date on a website and putting it in their calendar, is web_search for the finding and COMPOSIO for the calendar.";

/// py: telegram.py:908 `_APPS_LITE_NO_WEB`
pub(crate) const APPS_LITE_NO_WEB: &str = " Nothing on the public web (a course page, opening hours, a product, the news) can be looked up from here.";

/// py: telegram.py:912 `def apps_block_lite(names: Collection[str], *, web: bool = True) -> str:`
///
/// eve lite's apps block for a turn whose COMPOSIO tools are ``names`` (what the bridge lists): what Composio
/// reaches, in kinds of app, never by name; how to find and run a tool when search and multi-execute are both
/// listed; the sign-in link when the connection manager is; and web_search for the public web when ``web``.
pub fn apps_block_lite(names: &[String], web: bool) -> String {
    use eve_composio_tools::{MANAGE_CONNECTIONS, MULTI_EXECUTE};

    let have = |name: &str| names.iter().any(|n| n == name);
    let mut block = APPS_BLOCK_LITE.to_string();
    if have("COMPOSIO_SEARCH_TOOLS") && have(MULTI_EXECUTE) {
        block.push_str(APPS_LITE_FIND);
    }
    block.push_str(APPS_LITE_ASKED);
    if have(MANAGE_CONNECTIONS) {
        block.push_str(APPS_LITE_LINK);
    }
    block.push_str(APPS_LITE_SUMMARY);
    block.push_str(if web { APPS_LITE_WEB } else { APPS_LITE_NO_WEB });
    block
}

/// py: telegram.py:930 `LENT_LABELS`
pub const LENT_LABELS: &[(&str, &str)] = &[("share", "sending a file from this Mac to another person by iMessage or email"), ("workspace", "the owner's Slack, Notion, Linear and other connected apps")];

/// py: telegram.py:934 `LENT_ORDER`
pub const LENT_ORDER: &[&str] = &["share", "workspace"];

// py: telegram.py:935 `_FROM_ENV` (object): `seams::Lent::FromEnv`.

/// py: telegram.py:943 `ROBOT_BODY_TOOLS`
/// A Python set: sorted here; membership is what matters.
pub const ROBOT_BODY_TOOLS: &[&str] = &["find", "go_explore", "look", "look_around", "move_head", "set_sound"];

/// py: telegram.py:946 `def _code_match(app: Any, hub: Any, text: str) -> Any:`
///
/// An app's code-path command in ``text``, or None: the module's own ``match`` when it has one (it matches with
/// no hub, so a switched-on app that is not set up can say so), else the hub's (the lights match their names).
///
/// ``app.mod()`` is the app's module as the inlet holds it (`AppHubs::module`), None when it lent none; ``hub`` is
/// the app's hub (`AppHubs::get`), None when it is off or not set up.
pub(crate) fn code_match(module: Option<&dyn AppModule>, hub: Option<&dyn AppHub>, text: &str) -> Option<HubCommand> {
    if let Some(module) = module {
        if module.has_match() {
            return module.module_match(text);
        }
    }
    hub.and_then(|hub| hub.match_command(text))
}

/// py: telegram.py:955 `def robot_enabled(environ: Any = None) -> bool:`
pub fn robot_enabled(environ: Option<&Environ>) -> bool {
    let raw = env_get(environ, "EVE_ROBOT").unwrap_or_default();
    matches!(raw.trim().to_lowercase().as_str(), "1" | "true" | "yes" | "on")
}

/// TOOLS as definitions (each one a JSON object).
fn tools_list() -> Vec<Obj> {
    TOOLS.as_array().map(|tools| tools.iter().filter_map(|t| t.as_object().cloned()).collect()).unwrap_or_default()
}

/// py: telegram.py:960 `def own_tools() -> list[dict[str, Any]]:`
///
/// eve's own tools as lent this turn: TOOLS, less the robot body's unless EVE_ROBOT=1. eve lite's are the ones
/// it has (profile.filter_tools): remember and think_hard.
pub fn own_tools() -> Vec<Obj> {
    own_tools_in(None)
}

/// [`own_tools`] for the environment ``environ`` (None: the process's), where Python's tests set EVE_ROBOT and
/// EVE_PROFILE with monkeypatch.
pub fn own_tools_in(environ: Option<&Environ>) -> Vec<Obj> {
    let all = tools_list();
    let tools: Vec<Obj> = if robot_enabled(environ) {
        all
    } else {
        all.into_iter()
            .filter(|t| !ROBOT_BODY_TOOLS.contains(&t.get("name").and_then(Value::as_str).unwrap_or("")))
            .collect()
    };
    if lite(environ) {
        lite_tools(&tools)
    } else {
        tools
    }
}

/// INTENT_LINES as (the tools a line needs, the line).
static INTENT_LINE_LIST: LazyLock<Vec<(Vec<String>, String)>> = LazyLock::new(|| {
    INTENT_LINES
        .as_array()
        .map(|lines| {
            lines
                .iter()
                .filter_map(|pair| {
                    let needs = pair.get(0)?.as_array()?.iter().filter_map(|n| n.as_str().map(str::to_string));
                    Some((needs.collect(), pair.get(1)?.as_str()?.to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
});

/// py: telegram.py:973 `INTENT_WORDS`
pub const INTENT_WORDS: &str = "Choosing a tool: call one only for something the owner wants done or found now. A question about what you can do or how something works, a request that starts with \"don't\" or \"no need to\", something that already happened, and small talk about their day are answered in words with no tool. When the right tool depends on something they did not say, ask one short question.";

/// py: telegram.py:979 `INTENT_TALK`
pub const INTENT_TALK: &str = "When the owner is just telling you something (how their day went, what a friend or a family member did, something they already did), reply in words: that is conversation, with no lookup. {recall} {verb} for when they ask you to recall or look something up.";

/// py: telegram.py:985 `INTENT_LINES`
/// The Python value as JSON (tuples are arrays).
pub static INTENT_LINES: LazyLock<Value> =
    LazyLock::new(|| serde_json::from_str(r#"[[["jobs_list"], "What you are working on for the owner, their open jobs and reminders, and anything waiting on them (\"what are you working on\", \"anything waiting on me?\") is jobs_list."], [["list_todos"], "The owner's own to-do list (\"my todos\", \"what do I still need to do\", \"what's left on my plate\") is list_todos."], [["web_search", "firetv_control"], "A movie: what is playing or showtimes at a theater is web_search; watching at home is firetv_control, opening the app they name. For \"I want to watch a movie tonight\" with neither said, ask one short question: a theater, or the TV at home?"], [["firetv_control"], "A show or a movie to put on at home (\"put on a show\", \"stranger things on netflix\", \"some cartoons for the kids\") is firetv_control, opening the app it is on; so is opening any app on the TV or the Fire TV stick."], [["spotify_control"], "Music (an artist, a song, an album, a playlist or a mood: \"put on some jazz\", \"play the new album\") is spotify_control."], [["camera_control", "take_photo"], "Moving the webcam (\"turn the camera left\", \"point it at the door\", \"zoom in on me\", \"follow me\") is camera_control; a picture of what it sees is take_photo. To aim at something by name, take_photo first, then camera_control."], [["web_search"], "Current facts, news, prices, opening hours and theater showtimes are web_search."], [["science_search", "web_search"], "What research, studies or the science say about something, papers or evidence on a question, a review of a topic, or a specific study or DOI is science_search; everyday facts, news and how-to stay with web_search."], [["nyt_headlines"], "The New York Times' headlines or top stories right now, its home page or one section front (\"what's on the Times\", \"NYT tech headlines\"), is nyt_headlines."], [["nyt_search", "nyt_archive", "nyt_bestsellers", "nyt_popular"], "The New York Times by name: what it wrote about a topic, person or event, its reviews, or an article the owner remembers is nyt_search; everything it printed in one month long ago is nyt_archive; its best-seller lists are nyt_bestsellers; its most read, emailed or shared articles are nyt_popular."], [["america_ask"], "Official U.S. government and legal sources (tax rules, visas, benefits, a law, an agency's own page) are america_ask."], [["opentable_search", "start_task"], "A table at a restaurant, and a reservation with no other kind named (\"make a reservation\", \"book dinner for 4 fri at 7\", \"somewhere italian tmrw\", \"does that place have a table sunday\"), is opentable_search at once, even with only a day (with no day named, it is today): it finds open tables and their times. When the owner then picks one to book, start_task books exactly that restaurant, day, time and party, and its goal says to ask the owner before the final confirm. \"Book something\" that names no kind of thing gets one question: what to book."], [["screenshot", "list_files", "send_file", "read_file", "start_task"], "The Mac's screen and files have their own tools. What is on the screen, or which tab or window is open, is screenshot. Finding a file, whether a file exists, or what a folder holds is list_files. What a file says (its text, a line in it, a number in it, a summary of it) is read_file, after list_files when the exact path is not known. A file the owner wants in this chat or on their phone (send, share, drop or attach it here) is send_file, after list_files when the exact path is not known. start_task is for doing something in an app or on a website: clicking, typing, filling in a form, uploading, signing up, paying, returning an order. When the owner names the job, start it at once, even when a detail (which file, account, item or page) is not said: the task sees the screen and asks them here for anything it needs."], [["screenshot", "list_files", "send_file", "start_task"], "The Mac's screen and files have their own tools. What is on the screen, or which tab or window is open, is screenshot. Finding a file, whether a file exists, or what a folder holds is list_files. A file the owner wants in this chat or on their phone (send, share, drop or attach it here) is send_file, after list_files when the exact path is not known. start_task is for doing something in an app or on a website: clicking, typing, filling in a form, uploading, signing up, paying, returning an order. When the owner names the job, start it at once, even when a detail (which file, account, item or page) is not said: the task sees the screen and asks them here for anything it needs."], [["stop_task"], "Stopping or cancelling what you are doing on the Mac (\"halt\", \"cancel it\", \"quit that browser job\") is stop_task, at once."], [["steer_task"], "steer_task is for new instructions to a Mac task that is running now (\"use the other account instead\"); being told to carry on or continue, with no new instruction, is a reply."], [["watch_add"], "Keeping an eye on something over time (a product's price or a sale, a resale ticket price, a stock, tickets going on sale: \"tell me when it drops\") is watch_add, at once: it finds the page itself from the thing's name, so no lookup comes first."], [["forget_preview"], "Forgetting, deleting, erasing or removing something you know or remember about the owner (an old address, an old email, a fact that changed) is forget_preview, at once, with the words to forget: it searches and counts every place itself, and nothing goes until they confirm."], [["remember", "capture_note"], "A lasting preference or fact about the owner that they tell you to keep in mind (what they always want, like or avoid, from now on) is remember, even when they call it a note for you; capture_note is for a note of their own to keep in their notes."], [["meet_join"], "Joining a video call or a meeting for the owner, to sit in, listen or take notes on it, is meet_join, at once, with the time or name they gave: it finds the meeting on their calendar itself."], [["jobs_list", "take_on", "close_card"], "Reminders and anything you do for the owner on a schedule (daily, weekly, every morning) are jobs: what is set or scheduled is jobs_list; setting one up is take_on; cancelling or stopping one is close_card, after jobs_list when its card is not known."], [["make_app"], "Making an app, a tracker or a small tool for the owner (it shows their data, a live number or a timetable) is make_app, at once, with their words."], [["set_stealth"], "Stealth (eve quiet and asleep at the desk, also for a job the owner wants done quietly) is set_stealth with on true."], [["recent_messages"], "What was said in this chat earlier (\"what did I tell you yesterday\", \"the link you sent before\") is recent_messages."], [["take_notes", "capture_note"], "Recording or capturing a conversation in the room, going on now or about to start (\"take notes of this meeting\", \"keep a log of this talk\"), is take_notes with start; capture_note keeps a note the owner writes or dictates."], [["claude_relay"], "An answer for Claude Code's question (yes, no, an option, a choice) is claude_relay with action on: it connects this chat to the session so the answer reaches it."], [["codex_relay"], "Codex by name, for any coding job (\"open codex in eve\", \"ask codex to fix the tests\") is codex_relay with action on and the folder they named: it opens Codex here, and then the owner tells it the job."], [["claude_relay", "start_coding_session"], "A new Claude Code session in a folder is start_coding_session; joining one already running, asking what it is doing, or passing it words or an answer, is claude_relay."]]"#).expect("INTENT_LINES"));

/// py: telegram.py:1081 `INTENT_APP_LINES`
pub const INTENT_APP_LINES: &[(&str, &str)] = &[("calendar", "The owner's day and schedule (their meetings, what is on a day, how busy or free a day is) are read from their calendar's own tools; when they are not here yet, open the calendar first. Moving, renaming or cancelling one of their events is the calendar's own update or delete tool, after its list or search tool finds the event."), ("github", "Anything about the owner's repositories (opening, listing or counting a repo's issues, pull requests, CI runs and builds, notifications, starring or watching a repo) is done with GitHub's own tools, whose names start with github__; when they are not here yet, open GitHub first with open_app. The workspace is Slack, Notion and Linear only.")];

/// py: telegram.py:1092 `_NO_DEVICE`
pub(crate) const NO_DEVICE: &str = "Words that name no device (\"turn it up\", \"pause it\", \"switch it off\") change nothing yet: check what is on with {reads}, or ask which one.";

/// py: telegram.py:1094 `_NO_DEVICE_READS`
pub(crate) const NO_DEVICE_READS: &[&str] = &["spotify_now_playing", "firetv_status", "lights_status"];

/// py: telegram.py:1097 `_FILES_NO_READ`
/// its read_file twin goes when offered
pub(crate) const FILES_NO_READ: &[&str] = &["screenshot", "list_files", "send_file", "start_task"];

/// py: telegram.py:1100 `def intent_block(offered: Collection[str], apps: Collection[str] = ()) -> str:`
///
/// The tool-choice lines for one turn: the opening rule, then each line whose tools are all offered. The movie
/// line needs both of its tools; with only web_search, the plainer line says where showtimes go. The no-device line
/// names only the reads this turn offers; ``apps``: the direct apps up (INTENT_APP_LINES).
pub fn intent_block(offered: &[String], apps: &[String]) -> String {
    let have = |name: &str| offered.iter().any(|o| o == name);
    let mut lines: Vec<String> = vec![INTENT_WORDS.to_string()];
    let mut movie = false;
    for (needs, line) in INTENT_LINE_LIST.iter() {
        if !needs.iter().all(|n| have(n)) || (needs.as_slice() == FILES_NO_READ && have("read_file")) {
            continue;
        }
        let mut line = line.clone();
        if needs.as_slice() == ["web_search", "firetv_control"] {
            movie = true;
        } else if needs.as_slice() == ["web_search"] && movie {
            line = "Current facts, news, prices and opening hours are web_search.".to_string();
        }
        lines.push(line);
    }
    for (app, line) in INTENT_APP_LINES {
        if apps.iter().any(|a| a == app) {
            lines.push(line.to_string());
        }
    }
    let reads: Vec<&str> = NO_DEVICE_READS.iter().copied().filter(|r| have(r)).collect();
    if !reads.is_empty() {
        lines.push(NO_DEVICE.replace("{reads}", &reads.join(" or ")));
    }
    let recall: Vec<&str> = ["memory_search", "recent_messages"].into_iter().filter(|r| have(r)).collect();
    if !recall.is_empty() {
        let verb = if recall.len() > 1 { "are" } else { "is" };
        lines.insert(1, INTENT_TALK.replace("{recall}", &recall.join(" and ")).replace("{verb}", verb));
    }
    lines.join("\n")
}

/// py: telegram.py:1128 `_MAC_JOBS`
pub(crate) const MAC_JOBS: &str = "open, play, send, find a file, read the screen. Then start the task";

/// py: telegram.py:1129 `_MAC_JOBS_ON`
pub(crate) const MAC_JOBS_ON: &str = "open, play, send, or click through something in an app or on a website. Then start the task";

/// py: telegram.py:1130 `_ROUTE_MAC`
pub(crate) const ROUTE_MAC: &str = "Route every request to do something on the Mac, including closing tabs and opening a shared link, through start_task with the owner's words.";

/// py: telegram.py:1132 `_ROUTE_MAC_ON`
pub(crate) const ROUTE_MAC_ON: &str = "Route every request to do something in an app or on a website, including closing tabs and opening a shared link, through start_task with the owner's words; the screen and the Mac's files are screenshot, list_files and send_file.";

/// py: telegram.py:1137 `def menu_instructions(instructions: str) -> str:`
///
/// ``instructions`` with the start_task lines reworded for the menu tools' switch (see _MAC_JOBS).
pub fn menu_instructions(instructions: &str) -> String {
    let out = replace_flat(instructions, MAC_JOBS, MAC_JOBS_ON);
    replace_flat(&out, ROUTE_MAC, ROUTE_MAC_ON)
}

/// py: telegram.py:1143 `def _replace_flat(text: str, old: str, new: str) -> str:`
///
/// Replace ``old`` in ``text`` where the text may wrap it over lines (a newline for any one space).
pub(crate) fn replace_flat(text: &str, old: &str, new: &str) -> String {
    let pattern = old.split(' ').map(regex::escape).collect::<Vec<_>>().join(r"\s+");
    match Regex::new(&pattern) {
        Ok(re) => re.replacen(text, 1, regex::NoExpand(new)).into_owned(),
        // every piece is escaped, so the pattern always builds; a text left as it is would be the safe miss
        Err(_) => text.to_string(),
    }
}

/// py: telegram.py:1149 `def tools_for(config: TelegramConfig, memory_tools: Sequence[dict[str, Any]] = (), extra: Sequence[dict[str, Any]] = (), vault: bool = False, watch_tools: Sequence[dict[str, Any]] = (), chat_tools: Sequence[dict[str, Any]] = ()) -> list[dict[str, Any]]:`
///
/// The tools of one turn, in a fixed order (the list is part of the cached prefix): eve's own, the
/// memory tools whenever a Memory is lent, the web search the engine calls for (websearch.tools_for: the
/// OpenRouter function tool, or the hosted one), the second brain's tools when the vault is on, and the app tools
/// lent. The memory tools no longer depend on the profile: an empty or missing profile used to hide them,
/// and with them everything eve could look up (owner, 2026-09-23). The watch tools (watch.py) come after the
/// vault's when a Watcher is lent. eve lite offers only the tools it has (profile.filter_tools), in this order.
pub fn tools_for(config: &TelegramConfig, memory_tools: &[Obj], extra: &[Obj], vault: bool, watch_tools: &[Obj], chat_tools: &[Obj]) -> Vec<Obj> {
    tools_for_in(None, config, memory_tools, extra, vault, watch_tools, chat_tools)
}

/// [`tools_for`] for the environment ``environ`` (None: the process's): EVE_ROBOT and EVE_PROFILE.
pub fn tools_for_in(
    environ: Option<&Environ>,
    config: &TelegramConfig,
    memory_tools: &[Obj],
    extra: &[Obj],
    vault: bool,
    watch_tools: &[Obj],
    chat_tools: &[Obj],
) -> Vec<Obj> {
    let objects = |tools: Vec<Value>| -> Vec<Obj> { tools.into_iter().filter_map(|t| t.as_object().cloned()).collect() };
    let mut tools = own_tools_in(environ);
    tools.extend(memory_tools.iter().cloned());
    tools.extend(chat_tools.iter().cloned());
    tools.extend(objects(eve_web::websearch::tools_for(&config.search)));
    if vault {
        tools.extend(objects(eve_second_brain::SECOND_BRAIN_TOOLS.clone()));
    }
    tools.extend(watch_tools.iter().cloned());
    tools.extend(extra.iter().cloned());
    if lite(environ) {
        lite_tools(&tools)
    } else {
        tools
    }
}

/// py: telegram.py:1166 `SCREEN_NOW`
/// Python flags 34.
pub static SCREEN_NOW: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)^/?(please |can you |could you )?(send( me)?( a| the)? |show( me)?( the)? |take( a)? |give( me)?( a)? )?(screen ?shot|screen|screen ?grab|your screen|the mac|mac screen|what('s| is) on (the |my )?screen)( now| please| pls)?[\s.!?]*(?:\n?\z)"#).expect("SCREEN_NOW"));
