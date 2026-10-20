//! test_lite_brain.py's name checker: every tool name eve offers somewhere (UNIVERSE), the names a lite prompt may
//! write only when the turn offers them, and the app names a lite prompt never writes. Shared by brain.rs (the checks
//! that need no turn) and brain_turns.rs (the turn-driven ones). Include with `mod names;`.

#![allow(dead_code)]

use std::collections::BTreeSet;

use eve_brain::telegram;
use regex::Regex;
use serde_json::Value;

/// Composio Connect's seven meta tools (test_telegram_lite.COMPOSIO_NAMES), in META_TOOLS order.
pub const COMPOSIO_NAMES: &[&str] = &[
    "COMPOSIO_SEARCH_TOOLS",
    "COMPOSIO_GET_TOOL_SCHEMAS",
    "COMPOSIO_MULTI_EXECUTE_TOOL",
    "COMPOSIO_MANAGE_CONNECTIONS",
    "COMPOSIO_WAIT_FOR_CONNECTIONS",
    "COMPOSIO_REMOTE_WORKBENCH",
    "COMPOSIO_REMOTE_BASH_TOOL",
];
/// Names that are also plain English words ("find my keys", "look", "remember that", "a lesson plan"): the checker
/// cannot tell the tool from the word, so it leaves these out and reads every other name.
pub const ENGLISH: &[&str] = &["find", "look", "remember", "lesson"];
/// App, door and device names a lite prompt never writes: Composio Connect holds whichever apps the owner connected.
pub const APP_NAMES: &[&str] = &[
    "Gmail", "Google Calendar", "Google Drive", "Google Docs", "Google Sheets", "Google Maps", "Slack", "Notion",
    "Linear", "GitHub", "Outlook", "Spotify", "Fire TV", "Telegram", "iPhone", "Mini App", "Eve Link", "Canvas",
    "OpenTable", "Ticketmaster", "Obsidian", "Codex", "Claude Code", "OBSBOT", "StackChan", "Pika", "TinyFish",
    "Firecrawl", "AgentCard",
];

fn names_of(tools: &[Value]) -> Vec<String> {
    tools.iter().filter_map(|t| t.get("name").and_then(Value::as_str)).filter(|n| !n.is_empty()).map(str::to_string).collect()
}

/// The tool names of a registry app that gives the models tools (`app.mod().TOOL_NAMES`).
fn app_tool_names(slot: &str) -> Vec<&'static str> {
    match slot {
        "lights" => eve_lights::TOOL_NAMES.to_vec(),
        "spotify" => eve_spotify::TOOL_NAMES.to_vec(),
        "firetv" => eve_firetv::TOOL_NAMES.to_vec(),
        "obsbot" => eve_obsbot_panel::TOOL_NAMES.to_vec(),
        other => panic!("app_registry.with_tools() lists {other}: add its TOOL_NAMES to the checker"),
    }
}

/// Every tool name eve offers somewhere, in any door (Python's UNIVERSE).
pub fn universe() -> BTreeSet<String> {
    let mut all: BTreeSet<String> = BTreeSet::new();
    let mut add = |names: &[&str]| all.extend(names.iter().map(|n| n.to_string()));
    add(telegram::TOOL_NAMES);
    add(&eve_menu_tools::TOOL_NAMES);
    add(&eve_web::watch::TOOL_NAMES);
    add(&eve_science_search::TOOL_NAMES);
    add(&eve_second_brain::SECOND_BRAIN_TOOL_NAMES);
    add(&[eve_web::websearch::TOOL_NAME]);
    add(COMPOSIO_NAMES);
    add(&[eve_apps_mcp::OPEN_TOOL]);
    add(&eve_meet::TOOL_NAMES);
    add(eve_chief::TOOL_NAMES);
    add(&eve_canvas::TOOL_NAMES);
    add(eve_america::TOOL_NAMES);
    add(eve_opentable::TOOL_NAMES);
    add(eve_brain::share::TOOL_NAMES);
    add(&eve_agentcard::TOOL_NAMES);
    add(&eve_places::phone_location::TOOL_NAMES);
    add(&eve_events::TOOL_NAMES);
    add(eve_brain::apps_maker::MAKER_TOOL_NAMES);
    for app in eve_app_registry::with_tools() {
        add(&app_tool_names(&app.slot));
    }
    all.extend(names_of(&eve_menu_tools::FILE_TOOLS));
    all.extend(names_of(&eve_memory::VOICE_TOOLS));
    all.extend(names_of(&[telegram::RECENT_MESSAGES_TOOL.clone()]));
    all.extend(names_of(eve_brain::voice_agent::TOOLS.as_array().expect("voice_agent.TOOLS is a list")));
    all
}

/// The names the checker reads: the universe less the English words.
pub fn checked() -> BTreeSet<String> {
    universe().into_iter().filter(|n| !ENGLISH.contains(&n.as_str())).collect()
}

/// Every tool name of eve's the text writes, as a whole word.
pub fn names_written(text: &str) -> BTreeSet<String> {
    checked()
        .into_iter()
        .filter(|n| Regex::new(&format!(r"(?:^|[^A-Za-z0-9_]){}(?:$|[^A-Za-z0-9_])", regex::escape(n))).unwrap().is_match(text))
        .collect()
}

/// What breaks the owner's rule in `text` for a turn offering `offered`: a tool name it does not offer, or any app.
pub fn problems<S: AsRef<str>>(text: &str, offered: &[S]) -> Vec<String> {
    let mut out: Vec<String> = names_written(text)
        .into_iter()
        .filter(|n| !offered.iter().any(|o| o.as_ref() == n))
        .map(|n| format!("tool {n}"))
        .collect();
    for app in APP_NAMES {
        if Regex::new(&format!(r"\b{}\b", regex::escape(app))).unwrap().is_match(text) {
            out.push(format!("app {app}"));
        }
    }
    out
}
