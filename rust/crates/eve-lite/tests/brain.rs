//! test_lite_brain.py, the checks that need no turn: eve lite's brain as the model reads it, by its own tool lists and
//! prompt texts (eve-brain's public functions under eve lite's environment).
//!
//! * own_tools and tools_for leave eve lite's tools only, and full eve's lists as they were;
//! * the name checker has its positive controls (an injected Mac tool name and an app name are caught);
//! * the app block's swaps find their words, and the lite app block names eve's web app;
//! * the apps block is built from the Composio tools the bridge lists;
//! * eve lite's instructions say what eve lite is, in a positive set.
//!
//! The turn-driven tests (a lite turn's offered tools and instructions, the call notes) drive eve-brain's inlet
//! through its test rig and are not here (PARITY.md, Tests).

use std::collections::{BTreeSet, HashMap};

use eve_brain::telegram;
type Obj = serde_json::Map<String, serde_json::Value>;
use regex::Regex;
use serde_json::json;

const COMPOSIO_NAMES: &[&str] = &[
    "COMPOSIO_SEARCH_TOOLS",
    "COMPOSIO_GET_TOOL_SCHEMAS",
    "COMPOSIO_MULTI_EXECUTE_TOOL",
    "COMPOSIO_MANAGE_CONNECTIONS",
    "COMPOSIO_WAIT_FOR_CONNECTIONS",
    "COMPOSIO_REMOTE_WORKBENCH",
    "COMPOSIO_REMOTE_BASH_TOOL",
];
/// Names that are also plain English words: the checker cannot tell the tool from the word, so it leaves these out.
const ENGLISH: &[&str] = &["find", "look", "remember", "lesson"];
/// App, door and device names a lite prompt never writes.
const APP_NAMES: &[&str] = &[
    "Gmail", "Google Calendar", "Google Drive", "Google Docs", "Google Sheets", "Google Maps", "Slack", "Notion",
    "Linear", "GitHub", "Outlook", "Spotify", "Fire TV", "Telegram", "iPhone", "Mini App", "Eve Link", "Canvas",
    "OpenTable", "Ticketmaster", "Obsidian", "Codex", "Claude Code", "OBSBOT", "StackChan", "Pika", "TinyFish",
    "Firecrawl", "AgentCard",
];

/// eve lite's environment for the parts these tests build (test_telegram_lite.LITE_ENV).
fn lite_env() -> HashMap<String, String> {
    [
        ("EVE_PROFILE", "lite"),
        ("OPENROUTER_API_KEY", "CANARY-NOT-A-KEY-7f3c"),
        ("EVE_TELEGRAM", "app"),
        ("EVE_MEMORY", "1"),
        ("EVE_TOOLSET_ROUTER", "off"),
        ("EVE_GRADE_ROUTER", "off"),
        ("EVE_SEARCH_ROUTER", "off"),
        ("EVE_MENU_TOOLS", "off"),
        ("EVE_COMPOSIO", "0"),
        ("EVE_APPS", "0"),
        ("EVE_CHIEF", "off"),
        ("EVE_ROBOT", "1"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect()
}

/// The tool names the checker reads: eve's own, eve lite's, Connect's and the web search, less the English words.
fn checked() -> BTreeSet<String> {
    telegram::TOOL_NAMES
        .iter()
        .chain(eve_profile::LITE_TOOL_NAMES)
        .chain(COMPOSIO_NAMES)
        .chain([eve_web::websearch::TOOL_NAME].iter())
        .filter(|n| !ENGLISH.contains(n))
        .map(|n| n.to_string())
        .collect()
}

/// Every tool name the text writes, as a whole word.
fn names_written(text: &str) -> BTreeSet<String> {
    checked()
        .into_iter()
        .filter(|n| Regex::new(&format!(r"(?:^|[^A-Za-z0-9_]){}(?:$|[^A-Za-z0-9_])", regex::escape(n))).unwrap().is_match(text))
        .collect()
}

/// What breaks the owner's rule in `text` for a turn offering `offered`: a tool name it does not offer, or any app.
fn problems(text: &str, offered: &[&str]) -> Vec<String> {
    let mut out: Vec<String> =
        names_written(text).into_iter().filter(|n| !offered.contains(&n.as_str())).map(|n| format!("tool {n}")).collect();
    for app in APP_NAMES {
        if Regex::new(&format!(r"\b{}\b", regex::escape(app))).unwrap().is_match(text) {
            out.push(format!("app {app}"));
        }
    }
    out
}

fn tool(name: &str) -> Obj {
    json!({"type": "function", "name": name}).as_object().unwrap().clone()
}

#[test]
fn own_tools_and_tools_for_filter_in_lite_and_not_in_full_eve() {
    let mut full = lite_env();
    full.remove("EVE_PROFILE");
    let all: Vec<String> = telegram::TOOLS.as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_string()).collect();
    // full eve: the very list, as before
    assert_eq!(telegram::offered_names(&telegram::own_tools_in(Some(&full))), all);
    let lite = lite_env();
    assert_eq!(telegram::offered_names(&telegram::own_tools_in(Some(&lite))), vec!["remember", "think_hard"]);
    let cfg = telegram::configured(Some(&lite));
    let lent = telegram::tools_for_in(
        Some(&lite),
        &cfg,
        &[tool("memory_search"), tool("capture_note")],
        &[tool("COMPOSIO_SEARCH_TOOLS"), tool("gmail__send_message")],
        true,
        &[],
        &[],
    );
    assert_eq!(
        telegram::offered_names(&lent),
        vec!["remember", "think_hard", "memory_search", eve_web::websearch::TOOL_NAME, "COMPOSIO_SEARCH_TOOLS"]
    );
    // control: full eve lends them all
    let full_names = telegram::offered_names(&telegram::own_tools_in(Some(&full)));
    for name in ["start_task", "screenshot", "take_photo", "look", "move_head"] {
        assert!(full_names.iter().any(|n| n == name), "{name}");
    }
}

#[test]
fn the_checker_catches_a_mac_tool_and_an_app_name() {
    let offered = ["remember", "think_hard"];
    assert_eq!(problems(telegram::INSTRUCTIONS_LITE, &offered), Vec::<String>::new());
    // remember is a word too: not counted
    assert_eq!(names_written(telegram::INSTRUCTIONS_LITE), BTreeSet::from(["think_hard".to_string()]));
    let start_task = format!("{}\nWhen asked, call start_task.", telegram::INSTRUCTIONS_LITE);
    assert_eq!(problems(&start_task, &offered), vec!["tool start_task"]);
    let screenshot = format!("{} Use screenshot.", telegram::INSTRUCTIONS_LITE);
    assert_eq!(problems(&screenshot, &offered), vec!["tool screenshot"]);
    assert_eq!(problems("Your Gmail is connected.", &offered), vec!["app Gmail"]);
    assert_eq!(problems("start_tasks and my_start_task are other words", &offered), Vec::<String>::new());
}

#[test]
fn each_swapped_text_is_still_in_link_chats_app_block() {
    // The exact-text swaps find their words: a reworded link_chat or openui_prompt.txt fails here rather than leaving
    // the phone app's name in a lite prompt.
    let block = eve_link::link_chat::app_block(None);
    assert!(!block.is_empty(), "openui_prompt.txt is missing: run ios/EveLink/Web/scripts/gen-prompt.mjs");
    for (old, new) in telegram::LITE_APP_SWAPS {
        assert_eq!(block.matches(old).count(), 1, "{old}");
        assert!(!block.contains(new));
    }
    assert!(eve_link::link_chat::APP_RULE.starts_with(telegram::LITE_APP_SWAPS[0].0));
    assert!(eve_link::link_chat::APP_LINE.contains(telegram::LITE_APP_SWAPS[1].0));
}

#[test]
fn the_lite_app_block_names_the_web_app_and_keeps_the_rest() {
    let block = eve_link::link_chat::app_block(None);
    let lite = telegram::lite_app_block(&block);
    assert!(lite.contains("## Replying in eve's web app") && lite.contains("This turn is from eve's web app:"));
    for word in ["Eve Link", "iPhone", "native cards"] {
        assert!(!lite.contains(word), "{word}");
    }
    assert!(lite.contains(eve_link::link_chat::APP_EXAMPLE));
    assert!(lite.ends_with(&eve_link::link_chat::APP_LINE.replace("the Eve Link app", "eve's web app")));
    let grown: i64 = telegram::LITE_APP_SWAPS.iter().map(|(old, new)| new.len() as i64 - old.len() as i64).sum();
    assert_eq!(lite.len() as i64 - block.len() as i64, grown);
    assert_eq!(telegram::lite_app_block(""), "");
}

#[test]
fn the_apps_block_is_built_from_the_names_the_bridge_lists() {
    let names = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let every = telegram::apps_block_lite(&names(COMPOSIO_NAMES), true);
    for part in ["COMPOSIO_SEARCH_TOOLS", "COMPOSIO_MULTI_EXECUTE_TOOL", "COMPOSIO_MANAGE_CONNECTIONS", "sign-in link", "web_search"] {
        assert!(every.contains(part), "{part}");
    }
    let mut offered: Vec<&str> = COMPOSIO_NAMES.to_vec();
    offered.push(eve_web::websearch::TOOL_NAME);
    assert_eq!(problems(&every, &offered), Vec::<String>::new());
    assert!(every.contains("mail, calendar, files and work apps"));
    let two = telegram::apps_block_lite(&names(&["COMPOSIO_SEARCH_TOOLS", "COMPOSIO_MULTI_EXECUTE_TOOL"]), true);
    assert!(!two.contains("COMPOSIO_MANAGE_CONNECTIONS") && !two.contains("sign-in link"));
    let manage = telegram::apps_block_lite(&names(&["COMPOSIO_MANAGE_CONNECTIONS"]), false);
    assert!(!manage.contains("COMPOSIO_SEARCH_TOOLS") && !manage.contains("web_search") && manage.contains("public web"));
    assert_eq!(problems(&manage, &["COMPOSIO_MANAGE_CONNECTIONS"]), Vec::<String>::new());
    // control: full eve's block names its apps and is not lite's
    assert!(telegram::APPS_BLOCK_REST.contains("Linear") && !every.contains("Linear"));
}

#[test]
fn lite_instructions_say_what_eve_lite_is_in_a_positive_set() {
    let text = telegram::INSTRUCTIONS_LITE;
    assert!(text.contains("no lists"));
    assert!(!telegram::APP_STYLE_LITE.contains("no lists") && telegram::APP_STYLE_LITE.contains("web app section below"));
    // as the model reads it, lines aside
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    for part in ["chat", "remember", "search the web", "keep watch", "think hard", "apps they connect"] {
        assert!(flat.contains(part), "{part}");
    }
    assert!(flat.contains("eve lite does not do that")); // requests outside it get a plain no
    assert!(flat.contains("none of your tools reaches them") && flat.contains("eve's Settings, under Apps"));
    assert!(flat.contains("Keep API keys out of this chat")); // the Ori eval's safety rule stays
}
