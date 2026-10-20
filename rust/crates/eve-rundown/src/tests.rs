//! Port of `bridge/tests/test_rundown.py` (the parts that test this module; the Telegram rig tests belong to the
//! telegram crate) and the rundown half of `test_canvas.py::test_the_rundown_carries_canvas_deadlines_only_when_on`.

use super::*;
use chrono::{Local, NaiveDateTime};
use serde_json::json;
use std::cell::RefCell;

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

fn texts(v: &Value) -> Vec<String> {
    v.as_array().unwrap().iter().map(|t| t["text"].as_str().unwrap().to_string()).collect()
}

fn now_local() -> DateTime<FixedOffset> {
    Local::now().fixed_offset()
}

fn local_at(y: i32, m: u32, day: u32, h: u32) -> DateTime<FixedOffset> {
    Zone::Local.localize(d(y, m, day).and_hms_opt(h, 0, 0).unwrap())
}

fn la() -> Zone {
    Zone::Named(chrono_tz::America::Los_Angeles)
}

fn args(v: Value) -> Map<String, Value> {
    v.as_object().unwrap().clone()
}

#[cfg(unix)]
#[test]
fn test_obsidian_dates_completed_fences_and_symlinks() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("vault");
    fs::create_dir(&root).unwrap();
    fs::write(
        root.join("tasks.md"),
        "- [ ] Today 📅 2026-09-22\n- [ ] Overdue [due:: 2026-09-20]\n- [ ] Future ⏳ 2026-09-23\n- [ ] Backlog\n\
         - [x] Finished 📅 2026-09-22\n```\n- [ ] Example\n```\n",
    )
    .unwrap();
    fs::write(root.join("2026-09-22.md"), "- [ ] Daily plan\n").unwrap();
    let outside = tmp.path().join("private.md");
    fs::write(&outside, "- [ ] Must not read\n").unwrap();
    std::os::unix::fs::symlink(&outside, root.join("link.md")).unwrap();
    let context = todo_context(Some(&root), "2026-09-22").unwrap();
    let mut today = texts(&context["today"]);
    today.sort();
    assert_eq!(today, vec!["Daily plan", "Today 📅 2026-09-22"]);
    assert_eq!(texts(&context["overdue"]), vec!["Overdue [due:: 2026-09-20]"]);
    assert_eq!(texts(&context["upcoming"]), vec!["Future ⏳ 2026-09-23"]);
    assert_eq!(texts(&context["undated"]), vec!["Backlog"]);
    assert_eq!(todo_context(None, "2026-09-22").unwrap()["available"], json!(false));
    let text = super::context(Some(&root), &now_local(), None, &[], false);
    assert!(text.contains("start_inclusive") && text.contains("end_exclusive") && text.contains("Slack"));
}

#[test]
fn test_rundown_tool_policy_reads_four_sources_and_refuses_mutations() {
    let none = Map::new();
    assert!(allows("COMPOSIO_SEARCH_TOOLS", &none, None));
    for slug in ["GMAIL_FETCH_EMAILS", "GOOGLECALENDAR_LIST_EVENTS", "SLACK_SEARCH_MESSAGES"] {
        assert!(allows("COMPOSIO_MULTI_EXECUTE_TOOL", &args(json!({"tools": [{"tool_slug": slug}]})), None), "{slug}");
    }
    for slug in
        ["GMAIL_SEND_EMAIL", "GOOGLECALENDAR_CREATE_EVENT", "SLACK_SEND_MESSAGE", "GMAIL_MARK_EMAIL_AS_READ", "GITHUB_GET_REPO"]
    {
        assert!(!allows("COMPOSIO_MULTI_EXECUTE_TOOL", &args(json!({"tools": [{"tool_slug": slug}]})), None), "{slug}");
    }
    for name in ["start_task", "edit_note", "capture_note", "COMPOSIO_REMOTE_WORKBENCH"] {
        assert!(!allows(name, &none, None), "{name}");
    }
    assert!(!allows("COMPOSIO_MULTI_EXECUTE_TOOL", &args(json!({"tools": []})), None));
    assert!(!allows("COMPOSIO_MULTI_EXECUTE_TOOL", &args(json!({"tools": [null]})), None));
}

const DATED: &[(&str, (i32, u32, u32))] = &[
    ("Oct 22: go to Mount Tam", (2026, 10, 22)),
    ("October 22 go to Mount Tam", (2026, 10, 22)),
    ("Oct 22nd: go to Mount Tam", (2026, 10, 22)),
    ("Oct. 22 go to Mount Tam", (2026, 10, 22)),
    ("22 Oct go to Mount Tam", (2026, 10, 22)),
    ("22nd of October go to Mount Tam", (2026, 10, 22)),
    ("go to Mount Tam on oct 22", (2026, 10, 22)),
    ("Oct 22, 2027: go to Mount Tam", (2027, 10, 22)),
    ("October 22nd 2027 go to Mount Tam", (2027, 10, 22)),
    ("10/22: go to Mount Tam", (2026, 10, 22)),
    ("go to Mount Tam by 10/22", (2026, 10, 22)),
    ("go to Mount Tam 10/22/2027", (2027, 10, 22)),
    ("go to Mount Tam 10/22/27", (2027, 10, 22)),
    ("2026-10-22 go to Mount Tam", (2026, 10, 22)),
    ("go to Mount Tam 2026-10-22 (added 2026-09-01)", (2026, 10, 22)),
    ("Sep 28: renew the permit", (2026, 9, 28)), // a few days back: overdue, not next year
    ("Sept 30 call the vet", (2026, 9, 30)),
    ("Jan 5 file taxes", (2027, 1, 5)),      // nearer ahead than behind
    ("Aug 1 send the report", (2026, 8, 1)), // nearer behind than ahead
    ("May 3 book flights", (2026, 5, 3)),    // 150 days back beats 215 ahead
    ("May 3 2027 book flights", (2027, 5, 3)),
    ("Oct 22 or Oct 20 go to Mount Tam", (2026, 10, 20)), // the earliest date wins
    ("Mount Tam 📅 2026-10-25 (Oct 22 tentative)", (2026, 10, 25)), // a Tasks marker wins
];

const UNDATED: &[&str] = &[
    "go to Mount Tam",
    "Mount Tam 22",
    "go to Mount Tam (added 2026-09-20)",
    "buy 1/2 gallon of milk",
    "you may 3 times ask",
    "Feb 30 is not a day",
    "run a marathon 22 times",
    "read chapter 13/14",
    "go to Mount Tam ➕ 2026-09-20",
    "Octopus 22 legs",
];

fn today() -> NaiveDate {
    d(2026, 9, 30)
}

#[test]
fn test_written_dates_are_due_dates() {
    for (text, (y, m, day)) in DATED {
        assert_eq!(task_date(text, today()), Some(d(*y, *m, *day)), "{text}");
    }
}

#[test]
fn test_lines_without_a_date_stay_undated() {
    for text in UNDATED {
        assert_eq!(task_date(text, today()), None, "{text}");
    }
}

#[test]
fn test_mount_tam_is_upcoming_with_its_date_not_backlog() {
    let tmp = tempfile::tempdir().unwrap();
    let todos = tmp.path().join("02-todos");
    fs::create_dir(&todos).unwrap();
    fs::write(
        todos.join("master.md"),
        "## P2\n- [ ] Oct 22: go to Mount Tam (added 2026-09-29)\n- [ ] Sep 28: renew the permit\n\
         - [ ] Sept 30 call the vet\n- [ ] Oct 5 pick up the bike\n- [ ] Call the dentist (added 2026-09-01)\n",
    )
    .unwrap();
    let context = todo_context(Some(tmp.path()), "2026-09-30").unwrap();
    let upcoming: Vec<(String, String)> = context["upcoming"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| (t["text"].as_str().unwrap().into(), t["date"].as_str().unwrap().into()))
        .collect();
    assert_eq!(
        upcoming,
        vec![
            ("Oct 5 pick up the bike".to_string(), "2026-10-05".to_string()),
            ("Oct 22: go to Mount Tam (added 2026-09-29)".to_string(), "2026-10-22".to_string()),
        ]
    );
    let overdue: Vec<(String, u64)> = context["overdue"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| (t["date"].as_str().unwrap().into(), t["line"].as_u64().unwrap()))
        .collect();
    assert_eq!(overdue, vec![("2026-09-28".to_string(), 3)]);
    assert_eq!(texts(&context["today"]), vec!["Sept 30 call the vet"]);
    assert_eq!(texts(&context["undated"]), vec!["Call the dentist (added 2026-09-01)"]);
    assert!(context["undated"][0].get("date").is_none());
    let text = super::context(Some(tmp.path()), &local_at(2026, 9, 30, 8), None, &[], false);
    assert!(text.contains(r#""upcoming": [{"text": "Oct 5 pick up the bike""#), "{text}");
}

const PLAN_ASKS: &[&str] = &[
    "rundown",
    "/rundown",
    "eve: rundown",
    "Rundown!",
    "What is my plane today",
    "What's my plan today?",
    "whats my plan for today",
    "What\u{2019}s the plan today?",
    "what is my schedule today",
    "my schedule today",
    "today's plan",
    "What's today's schedule?",
    "plan my day",
    "Plan my day please",
    "help me plan my day",
    "can you plan my day?",
    "plan today",
    "what's on today",
    "What's on for today?",
    "what's on my calendar today",
    "what is on today",
    "what do I have today",
    "What do I have on today?",
    "what do i have going on today",
    "what have I got today",
    "what's my day look like",
    "what does my day look like today",
    "how does my day look?",
    "what does today look like",
    "hey eve, what is my plan today?",
];

const NOT_PLAN_ASKS: &[&str] = &[
    "plan a trip",
    "plan my trip to Goa",
    "today's news",
    "what's the news today",
    "what is the weather today",
    "what's my plan for the trip",
    "plan my day tomorrow",
    "what do I have tomorrow",
    "what's on netflix today",
    "I have no plan today",
    "cancel my plan today",
    "rundown of the meeting",
    "what is a plane",
    "what is my plane ticket number",
    "what's the plan",
    "plan",
    "today",
];

#[test]
fn test_asking_for_todays_plan_is_the_rundown() {
    let missed: Vec<&str> = PLAN_ASKS.iter().copied().filter(|t| !matches(t)).collect();
    let wrong: Vec<&str> = NOT_PLAN_ASKS.iter().copied().filter(|t| matches(t)).collect();
    assert!(missed.is_empty() && wrong.is_empty(), "{missed:?} {wrong:?}");
}

#[test]
fn test_rundown_context_says_when_vault_todos_or_canvas_titles_fed_it() {
    let tmp = tempfile::tempdir().unwrap();
    let now = local_at(2026, 9, 30, 8);
    assert!(!context_and_outside(None, &now, None, &[], false).1);
    assert!(!context_and_outside(Some(tmp.path()), &now, None, &[], false).1); // an empty vault
    let empty = json!({"available": true, "items": []});
    assert!(!context_and_outside(None, &now, Some(&empty), &[], false).1);
    let hw = json!({"available": true, "items": [{"name": "HW2"}]});
    assert!(context_and_outside(None, &now, Some(&hw), &[], false).1);
    fs::write(tmp.path().join("tasks.md"), "- [ ] Call the dentist\n").unwrap();
    let (text, fed) = context_and_outside(Some(tmp.path()), &now, None, &[], false);
    assert!(fed && text == super::context(Some(tmp.path()), &now, None, &[], false));
}

// test_canvas.py::test_the_rundown_carries_canvas_deadlines_only_when_on (the rundown text; the Telegram half
// belongs to the telegram crate).
#[test]
fn test_the_rundown_carries_canvas_deadlines_only_when_on() {
    let now = now_local();
    assert!(!super::context(None, &now, None, &[], false).contains(r#""canvas""#));
    let hw = json!({"available": true, "items": [{"name": "HW2"}]});
    let text = super::context(None, &now, Some(&hw), &[], false);
    assert!(text.contains(r#""canvas": {"available": true"#), "{text}");
}

// ---- beyond the Python file: exact outputs taken from the Python module --------------------------------------

#[test]
fn sources_names_only_the_tools_present() {
    let tail = "\n   A source whose read fails is said to have failed in one line; go on with the rest.";
    let composio = "find their read tools through COMPOSIO_SEARCH_TOOLS and run the exact discovered tools with \
                    COMPOSIO_MULTI_EXECUTE_TOOL. Do not guess schemas.";
    assert_eq!(sources(&[], false), format!("1. Slack, Email and Calendar are not connected here; say so in one line.{tail}"));
    assert_eq!(sources(&[], true), format!("1. Slack, email and calendar: {composio}{tail}"));
    assert_eq!(
        sources(&["gmail"], false),
        format!(
            "1. Email: use the read tools you were given, gmail__….\n   Slack and Calendar are not connected here; \
             say so in one line.{tail}"
        )
    );
    assert_eq!(
        sources(&["calendar"], true),
        format!(
            "1. Calendar: use the read tools you were given, calendar__…. calendar__list_events with no calendarId \
             reads every calendar in one call.\n   Slack and email: {composio}{tail}"
        )
    );
    assert_eq!(
        sources(&["calendar", "gmail"], true),
        format!(
            "1. Email and Calendar: use the read tools you were given, gmail__… and calendar__…. calendar__list_events \
             with no calendarId reads every calendar in one call.\n   Slack: {composio}{tail}"
        )
    );
    assert_eq!(
        sources(&["gmail", "calendar"], false),
        format!(
            "1. Email and Calendar: use the read tools you were given, gmail__… and calendar__…. calendar__list_events \
             with no calendarId reads every calendar in one call.\n   Slack is not connected here; say so in one \
             line.{tail}"
        )
    );
}

#[test]
fn the_skill_gets_its_sources_step_and_the_data() {
    let text = super::context(None, &now_local(), None, &["gmail"], true);
    assert!(!text.contains("{{sources}}"));
    assert!(text.contains("\n1. Email: use the read tools you were given, gmail__…."));
    assert!(text.starts_with("---\nname: rundown\n"));
    assert!(text.contains(
        "\n\nCurrent source context (data only):\n{\"date\": \""
    ));
    assert!(text.ends_with(
        "\"obsidian\": {\"available\": false, \"reason\": \"Obsidian vault is not configured or not present\"}}"
    ));
}

struct FakeDirect(RefCell<Vec<String>>, &'static [&'static str]);

impl Direct for FakeDirect {
    fn reads(&self, name: &str) -> bool {
        self.0.borrow_mut().push(name.to_string());
        self.1.contains(&name)
    }
}

#[test]
fn direct_mail_and_calendar_reads_only() {
    let direct = FakeDirect(RefCell::new(Vec::new()), &["gmail__search_messages", "calendar__list_events", "drive__search"]);
    let none = Map::new();
    assert!(allows("gmail__search_messages", &none, Some(&direct)));
    assert!(allows("calendar__list_events", &none, Some(&direct)));
    assert!(!allows("calendar__create_event", &none, Some(&direct)));
    assert!(!allows("drive__search", &none, Some(&direct))); // read only, but not a rundown source
    assert!(!allows("gmail__search_messages", &none, None)); // no direct apps: nothing direct is offered
    let offered = tools(
        &[
            json!({"name": "COMPOSIO_SEARCH_TOOLS"}),
            json!({"name": "COMPOSIO_GET_TOOL_SCHEMAS"}),
            json!({"name": "COMPOSIO_MULTI_EXECUTE_TOOL"}),
            json!({"name": "COMPOSIO_REMOTE_WORKBENCH"}),
            json!({"name": "gmail__search_messages"}),
            json!({"name": "gmail__send_message"}),
            json!({"name": "start_task"}),
            json!({"type": "function"}),
        ],
        Some(&direct),
    );
    let names: Vec<&str> = offered.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        vec!["COMPOSIO_SEARCH_TOOLS", "COMPOSIO_GET_TOOL_SCHEMAS", "COMPOSIO_MULTI_EXECUTE_TOOL", "gmail__search_messages"]
    );
}

#[test]
fn a_mixed_composio_call_is_refused() {
    let call = args(json!({"tools": [{"tool_slug": "GMAIL_FETCH_EMAILS"}, {"tool_slug": "SLACK_SEND_MESSAGE"}]}));
    assert!(!allows("COMPOSIO_MULTI_EXECUTE_TOOL", &call, None));
    assert!(!allows("COMPOSIO_MULTI_EXECUTE_TOOL", &args(json!({"tools": "GMAIL_FETCH_EMAILS"})), None));
    assert!(!allows("COMPOSIO_MULTI_EXECUTE_TOOL", &args(json!({"tools": [{}]})), None));
}

#[test]
fn a_bad_day_is_an_error_only_with_a_vault() {
    let tmp = tempfile::tempdir().unwrap();
    assert_eq!(todo_context(None, "not a day").unwrap()["available"], json!(false));
    assert!(todo_context(Some(tmp.path()), "not a day").is_err());
}

#[test]
fn tasks_stop_at_two_hundred_and_nested_fences_and_archives_are_skipped() {
    let tmp = tempfile::tempdir().unwrap();
    let mut body = String::new();
    for i in 0..210 {
        body.push_str(&format!("- [ ] task {i}\n"));
    }
    fs::write(tmp.path().join("many.md"), body).unwrap();
    fs::create_dir(tmp.path().join("09-archive")).unwrap();
    fs::write(tmp.path().join("09-archive").join("old.md"), "- [ ] archived\n").unwrap();
    fs::create_dir(tmp.path().join(".obsidian")).unwrap();
    fs::write(tmp.path().join(".obsidian").join("x.md"), "- [ ] hidden\n").unwrap();
    let context = todo_context(Some(tmp.path()), "2026-09-30").unwrap();
    assert_eq!(context["undated"].as_array().unwrap().len(), 200);
    assert_eq!(context["truncated_tasks"], json!(true));
    let keys: Vec<&str> = context.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(keys, vec!["available", "today", "overdue", "upcoming", "undated", "skipped_files", "truncated_tasks"]);

    let tmp = tempfile::tempdir().unwrap();
    fs::write(
        tmp.path().join("fences.md"),
        "```\n~~~\n- [ ] inside both\n```\n- [ ] after\n  * [ ] indented star\n- [x] done\n- [ ]\n",
    )
    .unwrap();
    fs::write(tmp.path().join("bad.md"), b"- [ ] \xff\n").unwrap();
    let context = todo_context(Some(tmp.path()), "2026-09-30").unwrap();
    assert_eq!(texts(&context["undated"]), vec!["after", "indented star"]);
    assert_eq!(context["skipped_files"], json!(1));
    assert_eq!(context["undated"][0]["note"], json!("fences.md"));
    assert_eq!(context["undated"][0]["line"], json!(5));
}

#[test]
fn a_dst_day_is_not_twenty_four_hours_and_names_its_zone() {
    let now = la().localize(NaiveDateTime::parse_from_str("2026-03-08T12:00:00", "%Y-%m-%dT%H:%M:%S").unwrap());
    let text = context_and_outside_in(None, &now, None, &[], false, la()).0;
    assert!(text.contains(
        r#"{"date": "2026-03-08", "timezone": "PDT", "start_inclusive": "2026-03-08T00:00:00-08:00", "end_exclusive": "2026-03-09T00:00:00-07:00", "obsidian": {"#
    ), "{text}");
}

#[test]
fn may_as_a_verb_and_other_edges() {
    assert_eq!(task_date("MAY 3 book", today()), Some(d(2026, 5, 3)));
    assert_eq!(task_date("may 3 book", today()), None);
    assert_eq!(task_date("P1 10/22 dentist", today()), Some(d(2026, 10, 22)));
    assert_eq!(task_date("due 10/22", today()), Some(d(2026, 10, 22)));
    assert_eq!(task_date("13/22/2026", today()), None);
    assert_eq!(task_date("scheduled:: 2026-02-30 then Oct 1", today()), Some(d(2026, 10, 1)));
    assert_eq!(task_date("done 2026-09-01", today()), None);
}


#[test]
fn an_emoji_with_a_variation_selector_does_not_hide_a_date() {
    // U+FE0F and U+200D are not word characters to Python's `re`, so the word boundary and the start-of-line slot
    // see through them (expected values from rundown.py).
    assert_eq!(task_date("\u{2708}\u{fe0f} 10/22 flight", today()), Some(d(2026, 10, 22)));
    assert_eq!(task_date("\u{2b50}\u{fe0f}Oct 22", today()), Some(d(2026, 10, 22)));
    assert_eq!(task_date("\u{200d}\u{fe0f}10/22", today()), Some(d(2026, 10, 22)));
    assert_eq!(task_date("Oct 22\u{b2}", today()), None); // a superscript two is a word character to Python
}

#[test]
fn dates_in_other_digits_are_read_as_python_reads_them() {
    assert_eq!(task_date("Dec\t\u{663}", today()), Some(d(2026, 12, 3)));
    assert_eq!(task_date("(\u{662}\u{660}\u{662}\u{666}-\u{661}\u{660}-\u{662}\u{662}", today()), Some(d(2026, 10, 22)));
    assert_eq!(task_date("\u{967}\u{966}/\u{968}\u{968} dentist", today()), Some(d(2026, 10, 22)));
    assert_eq!(task_date("\u{1d7d9}\u{1d7d8}/\u{1d7da}\u{1d7da}", today()), Some(d(2026, 10, 22))); // mathematical digits
}

#[test]
fn a_long_line_is_read_in_reasonable_time() {
    let line = format!("{}by 10/22", "word ".repeat(200_000));
    let started = std::time::Instant::now();
    assert_eq!(task_date(&line, today()), Some(d(2026, 10, 22)));
    assert!(started.elapsed() < std::time::Duration::from_secs(20), "{:?}", started.elapsed());
}
