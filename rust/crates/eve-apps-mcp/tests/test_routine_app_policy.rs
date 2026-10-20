//! Port of the apps_mcp half of bridge/tests/test_routine_app_policy.py (and the direct-app parts of
//! test_routine_confirmations.py and test_tool_gate.py): ordinary private edits run; communication, deletion,
//! sharing and unknown acts still ask. The Composio halves belong to eve-composio-tools.

mod common;

use std::sync::Arc;

use common::*;
use eve_apps_mcp as apps;
use eve_apps_mcp::{AppClient, AppsBridge, AppsConfig, Policy};
use eve_google_rest::{GoogleRestClient, HttpRequest, TransportError};
use serde_json::{json, Map, Value};

fn rule_policy(app: &str, rule: &str) -> Policy {
    [(app.to_string(), rule.to_string())].into_iter().collect()
}

/// `direct(name, args, rule)`: a bridge with only that tool registered, deciding.
fn direct(name: &str, args: Option<Value>, rule: &str) -> &'static str {
    let (app, raw) = apps::split_name(name);
    let mut bridge = AppsBridge::new(AppsConfig {
        policy: rule_policy(app, rule),
        ..AppsConfig::new(true)
    });
    bridge.insert_raw(
        name,
        json!({"name": raw, "annotations": {"readOnlyHint": false}}),
    );
    bridge.decide(name, args.as_ref()).action.as_str()
}

#[test]
fn test_supported_private_changes_run_and_owner_overrides_still_apply() {
    let cases = [
        (
            "drive__create_file",
            json!({"title": "Notes", "textContent": "A private draft"}),
        ),
        (
            "gmail__create_draft",
            json!({"to": ["reader@example.org"], "subject": "Draft", "body": "Unsent"}),
        ),
        (
            "calendar__create_event",
            json!({"summary": "Focus", "startTime": "2026-10-10T09:00:00Z"}),
        ),
        (
            "calendar__quick_add_event",
            json!({"text": "Focus tomorrow at 9"}),
        ),
        (
            "calendar__update_event",
            json!({"eventId": "event", "summary": "Focus", "notifyGuests": false}),
        ),
        (
            "github__create_repository",
            json!({"name": "private-notes", "private": true}),
        ),
    ];
    for (name, args) in cases {
        assert_eq!(direct(name, Some(args.clone()), "auto"), "run", "{name}");
        assert_eq!(direct(name, Some(args.clone()), "write"), "run", "{name}");
        assert_eq!(direct(name, Some(args.clone()), "ask"), "ask", "{name}");
        assert_eq!(direct(name, Some(args.clone()), "read"), "refuse", "{name}");
        assert_eq!(direct(name, None, "auto"), "ask", "{name}"); // a name without the actual arguments is insufficient
    }
}

#[test]
fn test_consequential_or_unclassified_writes_ask_even_with_write_policy() {
    let cases = [
        (
            "gmail__send_message",
            json!({"to": ["reader@example.org"], "body": "Hello"}),
        ),
        ("gmail__delete_draft", json!({"id": "draft"})),
        (
            "gmail__create_draft",
            json!({"body": "Hello", "send": true}),
        ),
        ("drive__delete_file", json!({"fileId": "file"})),
        (
            "drive__create_file",
            json!({"title": "Notes", "parents": ["shared-folder"]}),
        ),
        (
            "drive__create_file",
            json!({"title": "Notes", "permissions": [{"type": "anyone"}]}),
        ),
        (
            "drive__create_file",
            json!({"title": "Notes", "body": {"permissions": [{"type": "anyone"}]}}),
        ),
        (
            "calendar__delete_event",
            json!({"eventId": "event", "notifyGuests": false}),
        ),
        (
            "calendar__respond_to_event",
            json!({"eventId": "event", "responseStatus": "accepted"}),
        ),
        (
            "calendar__create_event",
            json!({"attendeeEmails": ["reader@example.org"], "notifyGuests": false}),
        ),
        ("calendar__create_event", json!({"notifyGuests": true})),
        (
            "calendar__create_event",
            json!({"calendarId": "shared-calendar"}),
        ),
        (
            "calendar__quick_add_event",
            json!({"text": "Meeting reader@example.org tomorrow"}),
        ),
        (
            "calendar__update_event",
            json!({"eventId": "event", "summary": "New time"}),
        ),
        (
            "calendar__update_event",
            json!({"eventId": "event", "notify_guests": false}),
        ),
        (
            "calendar__update_event",
            json!({"eventId": "event", "sendUpdates": "none"}),
        ),
        (
            "calendar__update_event",
            json!({"eventId": "event", "notifyGuests": false, "addAttendeeEmails": ["reader@example.org"]}),
        ),
        (
            "github__create_repository",
            json!({"name": "public-notes", "private": false}),
        ),
        (
            "github__create_repository",
            json!({"name": "public-notes", "PRIVATE": true}),
        ),
        (
            "github__create_repository",
            json!({"name": "team-notes", "private": true, "organization": "team"}),
        ),
        ("github__create_issue", json!({"title": "Public message"})),
        ("github__push_files", json!({"files": []})),
        ("github__merge_pull_request", json!({"pullNumber": 3})),
        ("drive__frobnicate", json!({})),
    ];
    for (name, args) in cases {
        assert_eq!(
            direct(name, Some(args.clone()), "auto"),
            "ask",
            "{name} {args}"
        );
        assert_eq!(
            direct(name, Some(args.clone()), "write"),
            "ask",
            "{name} {args}"
        );
        assert_eq!(
            direct(name, Some(args.clone()), "read"),
            "refuse",
            "{name} {args}"
        );
    }
}

#[test]
fn test_consequential_names_cannot_hide_behind_a_read_hint() {
    for name in [
        "get_and_purchase",
        "get_and_charge",
        "get_and_pay",
        "get_and_deploy",
    ] {
        assert!(
            !apps::tool_reads(&json!({"name": name, "annotations": {"readOnlyHint": true}})),
            "{name}"
        );
    }
}

#[test]
fn test_ambiguous_aliases_or_nonmapping_arguments_cannot_make_a_write_routine() {
    assert_eq!(
        direct(
            "calendar__create_event",
            Some(json!({"notifyGuests": false, "notify_guests": true})),
            "auto"
        ),
        "ask"
    );
    assert_eq!(
        direct("drive__create_file", Some(json!(["file"])), "auto"),
        "ask"
    );
}

#[test]
fn test_routine_write_reads_composio_names_and_their_arguments() {
    // the same allowlist, as composio_tools calls it with its toolkit slugs
    assert!(apps::routine_write(
        "googlecalendar",
        "CREATE_EVENT",
        Some(&json!({"summary": "Focus"}))
    ));
    assert!(apps::routine_write(
        "GOOGLECALENDAR",
        "UPDATE_EVENT",
        Some(&json!({"event_id": "e", "send_updates": "none"}))
    ));
    assert!(!apps::routine_write(
        "googlecalendar",
        "UPDATE_EVENT",
        Some(&json!({"event_id": "e"}))
    ));
    assert!(apps::routine_write(
        "googledrive",
        "UPLOAD_FILE",
        Some(&json!({"file_to_upload": "notes.txt"}))
    ));
    assert!(!apps::routine_write(
        "googledrive",
        "UPLOAD_FILE",
        Some(&json!({"file_to_upload": "n", "folder_id": "s"}))
    ));
    assert!(apps::routine_write(
        "googledocs",
        "CREATE_DOCUMENT",
        Some(&json!({"title": "Private draft"}))
    ));
    assert!(apps::routine_write(
        "googlesheets",
        "CREATE_SPREADSHEET",
        Some(&json!({"title": "Sheet"}))
    ));
    assert!(!apps::routine_write(
        "slack",
        "SEND_MESSAGE",
        Some(&json!({"channel": "c", "text": "hi"}))
    ));
    // start/end objects only with date, dateTime and timeZone; a notify of 0 is Python's False
    assert!(apps::routine_write(
        "calendar",
        "create_event",
        Some(&json!({"start": {"dateTime": "x", "timeZone": "y"}}))
    ));
    assert!(!apps::routine_write(
        "calendar",
        "create_event",
        Some(&json!({"start": {"dateTime": "x", "extra": 1}}))
    ));
    assert!(!apps::routine_write(
        "calendar",
        "create_event",
        Some(&json!({"reminders": {"useDefault": true}}))
    ));
    assert!(apps::routine_write(
        "calendar",
        "create_event",
        Some(&json!({"notifyGuests": 0, "calendarId": ""}))
    ));
    assert!(!apps::routine_write(
        "calendar",
        "quick_add_event",
        Some(&json!({"text": 5}))
    ));
}

/// A GET answers with this event; every other request answers {}.
fn rest_calendar(
    handler: impl Fn(&HttpRequest) -> Reply + Send + Sync + 'static,
) -> (Arc<Mock>, GoogleRestClient) {
    let http = Mock::new(handler);
    let client =
        GoogleRestClient::with_transport("calendar", || Some("test".to_string()), http.clone())
            .unwrap();
    (http, client)
}

fn calendar_bridge(client: Arc<dyn AppClient>, rule: &str) -> AppsBridge {
    let mut bridge = AppsBridge::new(AppsConfig {
        policy: rule_policy("calendar", rule),
        ..AppsConfig::new(true)
    });
    bridge.insert_client("calendar", client, None);
    bridge.insert_raw(
        "calendar__update_event",
        json!({"name": "update_event", "annotations": {"readOnlyHint": false}}),
    );
    bridge
}

#[tokio::test]
async fn test_routine_calendar_decision_matches_real_rest_delivery_parameters() {
    // A pre-existing event has guests; the edit must not silently inherit delivery.
    let (http, client) = rest_calendar(|_| {
        json_reply(
            200,
            json!({"id": "event", "attendees": [{"email": "guest@example.org"}]}),
        )
    });
    let create = json!({"summary": "Focus", "startTime": "2026-10-10T09:00:00Z", "endTime": "2026-10-10T10:00:00Z"});
    assert_eq!(
        direct("calendar__create_event", Some(create.clone()), "auto"),
        "run"
    );
    client.call_tool("create_event", &create).await.unwrap();
    let last = http.seen().last().cloned().unwrap();
    assert_eq!(url_parts(&last).1["sendUpdates"], "none");
    assert!(body_json(&last).get("attendees").is_none());

    let mut edit = obj(json!({"eventId": "event", "summary": "Moved", "notifyGuests": false}));
    assert_eq!(
        direct(
            "calendar__update_event",
            Some(Value::Object(edit.clone())),
            "auto"
        ),
        "run"
    );
    http.seen.lock().clear();
    client
        .call_tool("update_event", &Value::Object(edit.clone()))
        .await
        .unwrap();
    let seen = http.seen();
    assert!(seen.len() == 1 && seen[0].method == "PATCH");
    assert_eq!(url_parts(&seen[0]).1["sendUpdates"], "none");

    // Positive control: omitting the real adapter key causes guest delivery and must ask.
    edit.shift_remove("notifyGuests");
    assert_eq!(
        direct(
            "calendar__update_event",
            Some(Value::Object(edit.clone())),
            "auto"
        ),
        "ask"
    );
    http.seen.lock().clear();
    client
        .call_tool("update_event", &Value::Object(edit))
        .await
        .unwrap();
    let seen = http.seen();
    assert_eq!(
        seen.iter().map(|r| r.method.as_str()).collect::<Vec<_>>(),
        vec!["GET", "PATCH"]
    );
    assert_eq!(url_parts(seen.last().unwrap()).1["sendUpdates"], "all");
}

#[tokio::test]
async fn test_prepare_private_edit_pins_delivery_and_survives_a_guest_race() {
    for (rule, original_flag) in [
        ("auto", json!({})),
        ("write", json!({"notifyGuests": null})),
    ] {
        let (http, client) = rest_calendar(|r| {
            let (path, params) = url_parts(r);
            if r.method == "GET" {
                assert!(params.is_empty()); // neither a field mask nor maxAttendees can hide guests
                assert!(path.ends_with("/calendars/primary/events/event"));
                return json_reply(200, json!({"id": "event", "attendees": []}));
            }
            // Someone added a guest after the preflight read; execution must still explicitly suppress delivery.
            assert!(r.method == "PATCH" && params["sendUpdates"] == "none");
            assert_eq!(body_json(r), json!({"summary": "Moved"}));
            json_reply(
                200,
                json!({"id": "event", "attendees": [{"email": "guest@example.org"}]}),
            )
        });
        let bridge = calendar_bridge(Arc::new(client), rule);
        let mut args = obj(json!({"eventId": "event", "summary": "Moved"}));
        args.extend(obj(original_flag));
        let before = args.clone();
        let prepared = bridge.prepare("calendar__update_event", &args).await;
        assert_eq!(args, before);
        let mut want = args.clone();
        want.insert("notifyGuests".into(), json!(false));
        assert_eq!(prepared, want);
        assert_eq!(
            bridge
                .decide(
                    "calendar__update_event",
                    Some(&Value::Object(prepared.clone()))
                )
                .action,
            "run"
        );
        assert_eq!(
            bridge.execute("calendar__update_event", &prepared).await["ok"],
            json!(true)
        );
        assert_eq!(
            http.seen()
                .iter()
                .map(|r| r.method.clone())
                .collect::<Vec<_>>(),
            vec!["GET", "PATCH"]
        );
    }
}

#[tokio::test]
async fn test_prepare_guests_or_incomplete_event_leave_arguments_and_confirmation_unchanged() {
    let events = [
        json!({"id": "event", "attendees": [{"email": "guest@example.org"}]}),
        json!({"id": "event", "attendees": [{}]}),
        json!({"id": "event", "attendees": [], "attendeesOmitted": true}),
        json!({"id": "event", "attendeesOmitted": "false"}),
        json!({"id": "event", "attendees": null}),
        json!({"id": "event", "attendees": {}}),
        json!({"id": "event", "status": "cancelled"}),
        json!({"id": "different"}),
        json!({}),
        json!([]),
    ];
    for event in events {
        let e = event.clone();
        let (_http, client) = rest_calendar(move |_| json_reply(200, e.clone()));
        let bridge = calendar_bridge(Arc::new(client), "auto");
        let args = obj(json!({"eventId": "event", "summary": "Moved"}));
        let prepared = bridge.prepare("calendar__update_event", &args).await;
        assert_eq!(prepared, args, "{event}");
        assert_eq!(
            bridge
                .decide("calendar__update_event", Some(&Value::Object(prepared)))
                .action,
            "ask",
            "{event}"
        );
    }
}

#[tokio::test]
async fn test_prepare_complete_event_without_attendees_field_is_private() {
    let (_http, client) = rest_calendar(|_| json_reply(200, json!({"id": "event"})));
    let bridge = calendar_bridge(Arc::new(client), "auto");
    let prepared = bridge
        .prepare("calendar__update_event", &obj(json!({"eventId": "event"})))
        .await;
    assert_eq!(prepared["notifyGuests"], json!(false));
}

#[tokio::test]
async fn test_prepare_failed_read_asks_and_does_not_log_event_content() {
    for failure in ["http", "transport"] {
        let (_http, client) = rest_calendar(move |_| {
            if failure == "transport" {
                return Err(TransportError::new("ReadTimeout", "PRIVATE EVENT TEXT"));
            }
            json_reply(403, json!({"error": {"message": "PRIVATE EVENT TEXT"}}))
        });
        let bridge = calendar_bridge(Arc::new(client), "auto");
        let args = obj(json!({"eventId": "event", "summary": "Moved"}));
        let (guard, lines) = capture();
        let prepared = bridge.prepare("calendar__update_event", &args).await;
        drop(guard);
        assert_eq!(prepared, args);
        assert_eq!(
            bridge
                .decide("calendar__update_event", Some(&Value::Object(prepared)))
                .action,
            "ask"
        );
        let text = lines.text();
        assert!(
            text.contains("preparation failed") && !text.contains("PRIVATE EVENT TEXT"),
            "{failure}: {text}"
        );
    }
}

#[tokio::test]
async fn test_prepare_respects_controls_and_policy_without_an_extra_read() {
    let cases = [
        (
            "calendar__update_event",
            json!({"eventId": "event"}),
            "read",
        ),
        ("calendar__update_event", json!({"eventId": "event"}), "ask"),
        (
            "calendar__update_event",
            json!({"eventId": "event", "notifyGuests": true}),
            "auto",
        ),
        (
            "calendar__update_event",
            json!({"eventId": "event", "notifyGuests": false}),
            "auto",
        ),
        (
            "calendar__update_event",
            json!({"eventId": "event", "notify_guests": false}),
            "auto",
        ),
        (
            "calendar__update_event",
            json!({"eventId": "event", "calendarId": "shared"}),
            "auto",
        ),
        (
            "calendar__update_event",
            json!({"eventId": "event", "addAttendeeEmails": ["guest@example.org"]}),
            "auto",
        ),
        (
            "calendar__update_event",
            json!({"summary": "Moved"}),
            "auto",
        ),
        ("calendar__update_event", json!({"eventId": " "}), "auto"),
        ("calendar__update_event", json!({"eventId": 1}), "auto"),
        (
            "calendar__delete_event",
            json!({"eventId": "event"}),
            "auto",
        ),
        (
            "calendar__create_event",
            json!({"summary": "Focus"}),
            "auto",
        ),
    ];
    for (name, args, rule) in cases {
        let (http, client) = rest_calendar(|_| json_reply(200, json!({})));
        let bridge = calendar_bridge(Arc::new(client), rule);
        let args = obj(args);
        let prepared = bridge.prepare(name, &args).await;
        assert_eq!(prepared, args, "{name} {rule}");
        assert!(http.seen().is_empty(), "{name} {rule}");
    }
}

#[tokio::test]
async fn test_prepare_never_trusts_an_unknown_calendar_adapter() {
    struct UnknownCalendar;
    #[async_trait::async_trait]
    impl AppClient for UnknownCalendar {
        async fn list_tools(&self) -> Result<Vec<Value>, apps::CallError> {
            Ok(Vec::new())
        }
        async fn call_tool(
            &self,
            _name: &str,
            _args: &Map<String, Value>,
        ) -> Result<Value, apps::CallError> {
            panic!("an unknown adapter must not be inspected")
        }
    }
    let bridge = calendar_bridge(Arc::new(UnknownCalendar), "auto");
    let args = obj(json!({"eventId": "event", "summary": "Moved"}));
    assert_eq!(bridge.prepare("calendar__update_event", &args).await, args);
    assert_eq!(
        bridge
            .decide("calendar__update_event", Some(&Value::Object(args)))
            .action,
        "ask"
    );
}

#[tokio::test]
async fn test_a_guest_free_edit_runs_unasked_and_a_shared_one_asks() {
    // test_routine_confirmations.py's direct bridge: prepare, then decide, as telegram's _direct_tool does
    for has_guests in [true, false] {
        let (http, client) = rest_calendar(move |_| {
            let attendees = if has_guests {
                json!([{"email": "guest@example.org"}])
            } else {
                json!([])
            };
            json_reply(200, json!({"id": "event", "attendees": attendees}))
        });
        let mut direct = AppsBridge::new(AppsConfig::new(true));
        direct.insert_client("calendar", Arc::new(client), Some("rest"));
        direct.insert_raw(
            "calendar__update_event",
            json!({"name": "update_event", "annotations": {"readOnlyHint": false}}),
        );
        let args = obj(json!({"eventId": "event", "summary": "Quiet reading"}));
        let prepared = direct.prepare("calendar__update_event", &args).await;
        let decision = direct.decide(
            "calendar__update_event",
            Some(&Value::Object(prepared.clone())),
        );
        assert_eq!(decision.action == "ask", has_guests);
        if decision.action == "run" {
            direct.execute("calendar__update_event", &prepared).await;
        }
        assert_eq!(
            args,
            obj(json!({"eventId": "event", "summary": "Quiet reading"}))
        );
        let methods: Vec<String> = http.seen().iter().map(|r| r.method.clone()).collect();
        assert_eq!(
            methods,
            if has_guests {
                vec!["GET".to_string()]
            } else {
                vec!["GET".to_string(), "PATCH".to_string()]
            }
        );
        if !has_guests {
            assert_eq!(
                url_parts(http.seen().last().unwrap()).1["sendUpdates"],
                "none"
            );
        }
    }
}

#[test]
fn test_a_gate_question_shows_every_hidden_character() {
    // test_tool_gate.py's apps_mcp.describe_whole assertion, over hidden and bidi characters
    for ch in ['\u{200b}', '\u{202e}', '\u{2066}', '\u{feff}', '\u{00ad}'] {
        let shown = format!("[U+{:04X}]", ch as u32);
        let line = apps::describe_whole(
            "gmail__send_message",
            &obj(json!({"to": format!("sam{ch}@example.invalid"), "body": "hi"})),
            500,
        )
        .unwrap();
        assert!(!line.contains(ch) && line.contains(&shown), "{line}");
    }
}
