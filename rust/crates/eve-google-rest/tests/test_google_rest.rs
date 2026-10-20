//! Ports of the Google halves of bridge/tests/test_apps_mcp.py, test_lean_calendaredits_replay.py and
//! test_routine_app_policy.py / test_routine_confirmations.py, against the real client over a mock transport.

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use bytes::Bytes;
use chrono::{DateTime, Duration, FixedOffset, TimeZone};
use common::*;
use eve_google_rest::{self as gr, pytime, GoogleRestClient, TransportError};
use futures::StreamExt;
use serde_json::{json, Value};

fn ok(out: &Value) -> &Value {
    assert_eq!(out["isError"], json!(false), "{out}");
    &out["structuredContent"]
}

fn err_text(out: &Value) -> String {
    assert_eq!(out["isError"], json!(true), "{out}");
    out["content"][0]["text"].as_str().unwrap().to_string()
}

fn obj(v: Value) -> serde_json::Map<String, Value> {
    v.as_object().unwrap().clone()
}

// ---- Gmail ----------------------------------------------------------------------------------------------

#[tokio::test]
async fn test_gmail_search_summarises_each_thread() {
    let (client, mock) = google("gmail", |r| {
        if r.path().ends_with("/threads") {
            assert!(r.param("q").as_deref() == Some("from:sam") && r.param("maxResults").as_deref() == Some("2"));
            return json!({"threads": [{"id": "t1"}], "nextPageToken": "n"});
        }
        assert_eq!(r.params_all("metadataHeaders"), ["From", "Subject", "Date"]);
        json!({"messages": [
            {"labelIds": ["INBOX"], "payload": {"headers": [{"name": "Subject", "value": "Lunch"}]}},
            {"labelIds": ["UNREAD"], "snippet": "noon?", "payload": {"headers": [
                {"name": "From", "value": "Sam"}, {"name": "Date", "value": "Sat"}]}}]})
    });
    let out = client.call_tool("search_threads", &json!({"query": "from:sam", "pageSize": 2})).await.unwrap();
    assert_eq!(mock.seen()[0].header("authorization").as_deref(), Some("Bearer tok"));
    assert_eq!(
        *ok(&out),
        json!({"threads": [{"threadId": "t1", "subject": "Lunch", "from": "Sam", "date": "Sat", "messages": 2,
                            "unread": true, "snippet": "noon?"}], "nextPageToken": "n"})
    );
}

#[tokio::test]
async fn test_gmail_send_builds_the_email_and_posts_it() {
    let (client, mock) = google("gmail", |r| {
        assert!(r.method == "POST" && r.path().ends_with("/messages/send"));
        json!({"id": "m9", "threadId": "t9"})
    });
    let out = client
        .call_tool(
            "send_message",
            &json!({"to": ["ana@example.com"], "cc": "bo@example.com, cy@example.org", "subject": "Running late",
                    "body": "Hi Ana,\n\n15 minutes late."}),
        )
        .await
        .unwrap();
    assert_eq!(
        out,
        json!({"isError": false, "structuredContent": {"sent": true, "id": "m9", "threadId": "t9",
                                                      "to": ["ana@example.com"], "subject": "Running late"}})
    );
    let (headers, body) = sent(&mock.seen()[0]);
    assert_eq!(hget(&headers, "To").as_deref(), Some("ana@example.com"));
    assert_eq!(hget(&headers, "Cc").as_deref(), Some("bo@example.com, cy@example.org"));
    assert_eq!(hget(&headers, "Subject").as_deref(), Some("Running late"));
    assert_eq!(hget(&headers, "From"), None); // Gmail fills in the account
    assert_eq!(body.replace("\r\n", "\n").trim(), "Hi Ana,\n\n15 minutes late."); // CRLF on the wire
    assert!(body.contains("\r\n"));
    assert!(mock.seen()[0].json().get("threadId").is_none());
}

#[tokio::test]
async fn test_gmail_reply_goes_into_the_thread_with_the_originals_ids() {
    let (client, mock) = google("gmail", |r| {
        if r.method == "GET" {
            assert!(r.path().ends_with("/messages/orig1") && r.param("format").as_deref() == Some("metadata"));
            return json!({"id": "orig1", "threadId": "th1", "payload": {"headers": [
                {"name": "Message-ID", "value": "<abc@mail.example>"},
                {"name": "References", "value": "<root@mail.example>"}, {"name": "Subject", "value": "Lunch?"}]}});
        }
        json!({"id": "m2", "threadId": "th1"})
    });
    let out = client
        .call_tool("send_message", &json!({"to": ["sam@example.com"], "body": "Noon works.", "replyToMessageId": "orig1"}))
        .await
        .unwrap();
    assert_eq!(ok(&out)["subject"], "Re: Lunch?");
    let req = &mock.seen()[1];
    assert_eq!(req.json()["threadId"], "th1");
    let (headers, _) = sent(req);
    assert_eq!(hget(&headers, "In-Reply-To").as_deref(), Some("<abc@mail.example>"));
    assert_eq!(hget(&headers, "References").as_deref(), Some("<root@mail.example> <abc@mail.example>"));
}

#[tokio::test]
async fn test_gmail_send_to_me_is_the_owners_own_address() {
    let (client, mock) = google("gmail", |r| {
        if r.method == "GET" {
            assert!(r.path().ends_with("/users/me/profile"));
            return json!({"emailAddress": "owner@example.com"});
        }
        json!({"id": "m3", "threadId": "t3"})
    });
    let out = client.call_tool("send_message", &json!({"to": "Me", "cc": ["ana@example.com"], "body": "hi"})).await.unwrap();
    assert_eq!(ok(&out)["to"], json!(["owner@example.com"]));
    let (headers, _) = sent(&mock.seen()[1]);
    assert_eq!(hget(&headers, "To").as_deref(), Some("owner@example.com"));
    assert_eq!(hget(&headers, "Cc").as_deref(), Some("ana@example.com"));
    let (client, mock) = google("gmail", |_| json!({"id": "m4"}));
    client.call_tool("send_message", &json!({"to": ["ana@example.com"], "body": "hi"})).await.unwrap();
    assert_eq!(mock.seen().len(), 1); // no profile read when "me" is not used
}

#[tokio::test]
async fn test_gmail_send_refuses_what_is_not_plainly_an_email() {
    let many: Vec<String> = (0..21).map(|i| format!("p{i}@example.com")).collect();
    let cases = [
        (json!({"to": ["ana@example.com\nBcc: eve@example.com"], "body": "x"}), "not an email address"), // a header smuggled in
        (json!({"to": ["Ana"], "body": "x"}), "not an email address"), // a name, not an address
        (json!({"to": [], "body": "x"}), "no recipient"),
        (json!({"to": ["ana@example.com"], "body": "  "}), "no body"),
        (json!({"to": ["ana@example.com"], "subject": "Hi\r\nBcc: eve@example.com", "body": "x"}), "one line"),
        (json!({"to": many, "body": "x"}), "more than 20"),
    ];
    for (args, why) in cases {
        let (client, mock) = google("gmail", |_| json!({}));
        let out = client.call_tool("send_message", &args).await.unwrap();
        let text = err_text(&out);
        assert!(text.contains(why), "{text}");
        assert!(mock.seen().is_empty()); // nothing was sent
    }
}

#[test]
fn test_mail_text_prefers_plain_and_falls_back_to_html() {
    let plain = json!({"mimeType": "multipart/alternative", "parts": [
        {"mimeType": "text/html", "body": {"data": b64("<p>html</p>")}},
        {"mimeType": "text/plain", "body": {"data": b64("plain text\n\n\nend")}}]});
    assert_eq!(gr::mail_text(&plain), "plain text\nend");
    let html = json!({"mimeType": "text/html", "body": {"data": b64("<style>x{}</style><p>Hi &amp; bye</p>")}});
    assert_eq!(gr::mail_text(&html), "Hi & bye");
    let long = json!({"mimeType": "text/plain", "body": {"data": b64(&"x".repeat(gr::BODY_CLIP + 5))}});
    assert_eq!(gr::mail_text(&long).chars().count(), gr::BODY_CLIP + 1);
}

#[tokio::test]
async fn test_attachments_are_listed_and_a_text_one_is_read() {
    let payload = json!({"mimeType": "multipart/mixed", "parts": [
        {"mimeType": "text/plain", "body": {"data": b64("see attached")}},
        {"mimeType": "text/plain", "filename": "notes.txt", "body": {"attachmentId": "att1", "size": 5}}]});
    assert_eq!(gr::mail_text(&payload), "see attached"); // the .txt is not the body
    assert_eq!(
        gr::attachments(&payload),
        vec![json!({"filename": "notes.txt", "mimeType": "text/plain", "size": 5, "attachmentId": "att1"})]
    );
    let p = payload.clone();
    let (client, _) = google("gmail", move |r| {
        if r.path().ends_with("/attachments/att1") {
            return json!({"data": b64("hello")});
        }
        json!({"payload": p})
    });
    let out = client.call_tool("get_attachment", &json!({"messageId": "m1", "attachmentId": "att1"})).await.unwrap();
    assert_eq!(*ok(&out), json!({"filename": "notes.txt", "mimeType": "text/plain", "text": "hello", "clipped": false}));
}

fn hello_pdf() -> Vec<u8> {
    // A one-page PDF with the word Hello, built by hand (PDF 1.4, Helvetica).
    let stream = b"BT /F1 24 Tf 72 720 Td (Hello) Tj ET".to_vec();
    let mut obj4 = format!("<< /Length {} >>\nstream\n", stream.len()).into_bytes();
    obj4.extend_from_slice(&stream);
    obj4.extend_from_slice(b"\nendstream");
    let objs: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>"
            .to_vec(),
        obj4,
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
    ];
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = vec![];
    for (i, o) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend_from_slice(o);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for off in offsets {
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF", objs.len() + 1).as_bytes());
    out
}

#[test]
fn test_a_pdf_is_read_with_pdfkit_where_it_exists() {
    if cfg!(target_os = "macos") {
        assert!(gr::as_text(&hello_pdf(), "application/pdf").unwrap_or_default().contains("Hello"));
        assert_eq!(gr::pdf_text(b"not a pdf"), None);
    } else {
        assert_eq!(gr::as_text(&hello_pdf(), "application/pdf"), None);
    }
    assert_eq!(gr::as_text(b"x", "application/zip"), None);
    assert_eq!(gr::as_text(b"{}", "application/json").as_deref(), Some("{}"));
}

// ---- contacts -------------------------------------------------------------------------------------------

#[tokio::test]
async fn test_contacts_search_warms_once_and_merges_saved_and_other_people() {
    let (client, mock) = google("contacts", |r| {
        let q = r.param("query").unwrap();
        if r.path().ends_with("/people:searchContacts") {
            assert_eq!(r.param("readMask").as_deref(), Some(gr::CONTACT_FIELDS));
            return if q.is_empty() {
                json!({})
            } else {
                json!({"results": [{"person": {
                    "names": [{"displayName": "Ana Diaz"}], "emailAddresses": [{"value": "ana@example.com"}],
                    "phoneNumbers": [{"value": "+1 555 0100"}], "organizations": [{"name": "UW"}]}}]})
            };
        }
        assert!(r.path().ends_with("/otherContacts:search") && r.param("readMask").as_deref() == Some(gr::OTHER_FIELDS));
        if q.is_empty() {
            json!({})
        } else {
            json!({"results": [
                {"person": {"names": [{"displayName": "Ana D"}], "emailAddresses": [{"value": "ANA@example.com"}]}},
                {"person": {"emailAddresses": [{"value": "ana.work@example.org"}]}}]})
        }
    });
    let out = client.call_tool("search_contacts", &json!({"query": "ana"})).await.unwrap();
    assert_eq!(
        *ok(&out),
        json!({"people": [
            {"name": "Ana Diaz", "emails": ["ana@example.com"], "phones": ["+1 555 0100"], "organizations": ["UW"]},
            {"name": "", "emails": ["ana.work@example.org"], "saved": false}]}) // the same address twice is one person
    );
    let queries: Vec<String> = mock.seen().iter().map(|r| r.param("query").unwrap()).collect();
    assert_eq!(queries, ["", "", "ana", "ana"]); // Google's warm-up, then the search
    client.call_tool("search_contacts", &json!({"query": "bo"})).await.unwrap();
    let queries: Vec<String> = mock.seen()[4..].iter().map(|r| r.param("query").unwrap()).collect();
    assert_eq!(queries, ["bo", "bo"]); // warmed once
}

#[test]
fn test_contacts_is_a_read_only_toolset() {
    let contacts = &gr::TOOLS["contacts"];
    assert_eq!(contacts.iter().map(|t| t["name"].as_str().unwrap()).collect::<Vec<_>>(), ["search_contacts"]);
    assert!(contacts.iter().all(|t| t["annotations"]["readOnlyHint"] == json!(true)));
    let send = gr::TOOLS["gmail"].iter().find(|t| t["name"] == "send_message").unwrap();
    assert_eq!(send["annotations"]["readOnlyHint"], json!(false)); // a send is never a read
}

// ---- calendar -------------------------------------------------------------------------------------------

#[tokio::test]
async fn test_calendar_lists_a_window_and_creates_with_a_meet_link() {
    let (client, mock) = google("calendar", |r| {
        if r.path().ends_with("/calendarList") {
            return json!({"items": []}); // no list: list_events falls back to the primary
        }
        if r.method == "GET" {
            assert!(r.param("singleEvents").as_deref() == Some("true") && r.param("orderBy").as_deref() == Some("startTime"));
            assert!(r.param("timeMax").unwrap().starts_with("2026-10-12T09:00:00")); // start + 7 days
            return json!({"timeZone": "America/Los_Angeles", "items": [{"id": "e1", "summary": "Standup"}]});
        }
        let body = r.json();
        assert!(r.param("conferenceDataVersion").as_deref() == Some("1") && r.param("sendUpdates").as_deref() == Some("all"));
        assert_eq!(body["start"], json!({"dateTime": "2026-10-10T12:00:00-07:00", "timeZone": "America/Los_Angeles"}));
        assert!(body["end"].get("date").is_none()); // nothing to clear on a new event
        assert_eq!(body["attendees"], json!([{"email": "ana@example.com"}]));
        assert_eq!(body["conferenceData"]["createRequest"]["conferenceSolutionKey"], json!({"type": "hangoutsMeet"}));
        assert_eq!(body["conferenceData"]["createRequest"]["requestId"].as_str().unwrap().len(), 16);
        json!({"id": "e2"})
    });
    let listed = client.call_tool("list_events", &json!({"startTime": "2026-10-05T09:00:00-07:00"})).await.unwrap();
    assert_eq!(ok(&listed)["events"][0]["summary"], "Standup");
    let made = client
        .call_tool(
            "create_event",
            &json!({"summary": "Lunch", "startTime": "2026-10-10T12:00:00-07:00", "endTime": "2026-10-10T13:00:00-07:00",
                    "timeZone": "America/Los_Angeles", "attendeeEmails": ["ana@example.com"], "addGoogleMeetUrl": true}),
        )
        .await
        .unwrap();
    assert_eq!(*ok(&made), json!({"id": "e2"}));
    assert!(mock.seen().last().unwrap().path().ends_with("/calendars/primary/events"));
}

#[test]
fn test_an_event_gets_maps_links_only_for_a_place() {
    let cases = [
        (json!("Soda Hall 306, Berkeley, CA"), true),
        (json!("  Room 101  "), true),
        (json!(""), false),
        (Value::Null, false),
        (json!("https://zoom.us/j/123456"), false),
        (json!("meet.google.com/abc-defg-hij"), false),
        (json!("abc-defg-hij"), false),
        (json!("Zoom: zoom.us/j/99"), false),
        (json!("Microsoft Teams Meeting"), false),
        (json!("www.example.com"), false),
    ];
    for (location, links) in cases {
        let out = gr::compact_event(&json!({"id": "e", "summary": "CS 61A", "start": {"dateTime": "x"}, "location": location}));
        assert_eq!(out.get("mapsUrl").is_some(), links, "{location}");
        assert_eq!(out.get("directionsUrl").is_some(), links, "{location}");
    }
}

#[test]
fn test_maps_links_follow_googles_url_scheme() {
    let links = gr::place_links(&json!("Soda Hall 306, Berkeley, CA"));
    // developers.google.com/maps/documentation/urls/get-started: api=1, URL-encoded, spaces as %20
    assert_eq!(
        Value::Object(links),
        json!({"mapsUrl": "https://www.google.com/maps/search/?api=1&query=Soda%20Hall%20306%2C%20Berkeley%2C%20CA",
               "directionsUrl": "https://www.google.com/maps/dir/?api=1&destination=Soda%20Hall%20306%2C%20Berkeley%2C%20CA"})
    );
    assert!(gr::place_links(&json!("Caf\u{e9} & Bar #2"))["mapsUrl"].as_str().unwrap().ends_with("query=Caf%C3%A9%20%26%20Bar%20%232"));
    assert!(gr::place_links(&json!("x".repeat(5000)))["directionsUrl"].as_str().unwrap().len() < 2048);
}

#[test]
fn test_an_event_is_compacted_for_the_model_and_keeps_what_meet_reads() {
    let raw = json!({"kind": "calendar#event", "etag": "\"1\"", "id": "e1", "status": "confirmed", "htmlLink": "https://g/e1",
           "created": "x", "updated": "x", "summary": "Standup",
           "description": format!("Agenda. {} join https://meet.google.com/abc-defg-hij now", "y".repeat(900)),
           "location": "Room 4, 1 Main St",
           "creator": {"email": "me@x"}, "organizer": {"email": "boss@x", "displayName": "Boss"},
           "start": {"dateTime": "2026-10-05T09:00:00-07:00"}, "end": {"dateTime": "2026-10-05T09:15:00-07:00"},
           "recurringEventId": "r1", "iCalUID": "u", "reminders": {"useDefault": true},
           "hangoutLink": "https://meet.google.com/abc-defg-hij",
           "conferenceData": {"entryPoints": [{"entryPointType": "video", "uri": "https://meet.google.com/abc-defg-hij"}]},
           "attendees": [{"email": "me@x", "self": true, "responseStatus": "accepted", "displayName": "Me"},
                         {"email": "boss@x", "organizer": true, "responseStatus": "accepted", "comment": "hi"}]});
    let out = gr::compact_event(&raw);
    let keys: Vec<&str> = out.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        ["id", "status", "summary", "start", "end", "location", "mapsUrl", "directionsUrl", "description",
         "hangoutLink", "conferenceData", "attendees", "organizer", "recurringEventId", "htmlLink"]
    );
    assert_eq!(
        out["attendees"],
        json!([{"email": "me@x", "responseStatus": "accepted", "self": true},
               {"email": "boss@x", "responseStatus": "accepted", "organizer": true}])
    );
    assert_eq!(out["organizer"], json!({"email": "boss@x"}));
    assert_eq!(out["conferenceData"], raw["conferenceData"]);
    // clipped, but a call's link past the clip is kept, so Meet's lookup still finds it there
    let desc = out["description"].as_str().unwrap();
    assert!(desc.starts_with("Agenda. ") && desc.chars().count() < 600);
    assert!(desc.ends_with("https://meet.google.com/abc-defg-hij"));
    let only_desc = gr::compact_event(&json!({"summary": "Sync", "start": {"dateTime": "x"},
                                              "description": format!("{} https://meet.google.com/abc-defg-hij", "z".repeat(900))}));
    assert!(only_desc["description"].as_str().unwrap().ends_with(" https://meet.google.com/abc-defg-hij"));
    assert_eq!(gr::compact_event(&json!({"id": "e2"})), json!({"id": "e2"}));
    assert_eq!(gr::compact_event(&json!("x")), json!("x"));
}

#[tokio::test]
async fn test_every_calendar_call_that_returns_events_compacts_them_with_links() {
    let raw = json!({"id": "e1", "etag": "x", "summary": "CS 61A", "location": "Wheeler 150",
                     "start": {"dateTime": "2026-10-05T09:00:00-07:00"}});
    let r2 = raw.clone();
    let (client, _) = google("calendar", move |r| {
        if r.path().ends_with("/events") && r.method == "GET" {
            return json!({"items": [r2]});
        }
        if r.method == "GET" {
            let mut e = r2.clone();
            e["attendees"] = json!([{"email": "me@x", "self": true}]);
            return e;
        }
        r2.clone()
    });
    let calls = [
        ("list_events", json!({})),
        ("search_events", json!({"query": "61A"})),
        ("get_event", json!({"eventId": "e1"})),
        ("create_event", json!({"summary": "CS 61A", "startTime": "2026-10-05T09:00:00-07:00",
                                "endTime": "2026-10-05T10:00:00-07:00", "location": "Wheeler 150"})),
        ("update_event", json!({"eventId": "e1", "summary": "CS 61A"})),
        ("quick_add_event", json!({"text": "CS 61A Monday 9am at Wheeler 150"})),
        ("respond_to_event", json!({"eventId": "e1", "responseStatus": "accepted"})),
    ];
    for (name, args) in calls {
        let out = client.call_tool(name, &args).await.unwrap();
        assert_eq!(out["isError"], json!(false), "{name}: {out}");
        let got = &out["structuredContent"];
        let event = if got.get("events").is_some() { &got["events"][0] } else { got };
        assert!(event["mapsUrl"].as_str().unwrap().ends_with("query=Wheeler%20150"), "{name}");
        assert!(event["directionsUrl"].as_str().unwrap().ends_with("destination=Wheeler%20150"), "{name}");
        assert!(event.get("etag").is_none(), "{name}");
    }
}

#[test]
fn test_all_day_events_use_dates() {
    let (client, _) = google("calendar", |_| json!({}));
    let body = client
        .event_body(&obj(json!({"summary": "Trip", "startTime": "2026-10-10", "endTime": "2026-10-12", "allDay": true})))
        .unwrap();
    // dateTime null: a PATCH from a timed event clears it (nested objects merge)
    assert_eq!(
        Value::Object(body),
        json!({"summary": "Trip", "start": {"date": "2026-10-10", "dateTime": null},
               "end": {"date": "2026-10-12", "dateTime": null}})
    );
    let timed = client.event_body(&obj(json!({"startTime": "2026-10-10T09:00:00-07:00"}))).unwrap();
    assert_eq!(timed["start"], json!({"dateTime": "2026-10-10T09:00:00-07:00", "date": null}));
}

#[test]
fn test_a_recurring_event_carries_its_zone() {
    let (client, _) = google("calendar", |_| json!({}));
    let body = client
        .event_body(&obj(json!({"startTime": "2026-10-05T09:00:00-07:00", "endTime": "2026-10-05T09:30:00-07:00",
                                "recurrence": ["RRULE:FREQ=WEEKLY;BYDAY=MO"]})))
        .unwrap();
    assert_eq!(body["recurrence"], json!(["RRULE:FREQ=WEEKLY;BYDAY=MO"]));
    assert!(!body["start"]["timeZone"].as_str().unwrap().is_empty());
    let la = client.with_zone(LA);
    let body = la
        .event_body(&obj(json!({"startTime": "2026-10-05T09:00:00", "recurrence": ["RRULE:FREQ=DAILY"]})))
        .unwrap();
    assert_eq!(body["start"], json!({"dateTime": "2026-10-05T09:00:00-07:00", "date": null, "timeZone": "America/Los_Angeles"}));
}

#[tokio::test]
async fn test_suggest_time_keeps_an_explicit_midnight_workday_start() {
    // `or 9` read an explicit 0 as no answer and started the day at 9
    let (client, _) = google("calendar", |r| {
        if r.path().ends_with("/calendarList") {
            json!({"items": [{"id": "me@x"}]})
        } else {
            json!({"calendars": {}})
        }
    });
    let client = client.with_zone(LA);
    let args = json!({"startTime": "2026-10-05T00:00:00-07:00", "endTime": "2026-10-05T20:00:00-07:00", "durationMinutes": 30});
    let mut midnight = args.clone();
    midnight["workdayStartHour"] = json!(0);
    let out = client.call_tool("suggest_time", &midnight).await.unwrap();
    assert_eq!(ok(&out)["free"], json!([{"start": "2026-10-05T00:00:00-07:00", "end": "2026-10-05T18:00:00-07:00"}]));
    let out = client.call_tool("suggest_time", &args).await.unwrap();
    // unset still means 9 to 18
    assert_eq!(ok(&out)["free"], json!([{"start": "2026-10-05T09:00:00-07:00", "end": "2026-10-05T18:00:00-07:00"}]));
}

#[test]
fn test_times_without_an_offset_get_the_macs() {
    let local = chrono::Local.with_ymd_and_hms(2026, 10, 5, 9, 0, 0).single().unwrap().fixed_offset();
    assert_eq!(gr::rfc3339(&json!("2026-10-05T09:00")), pytime::isoformat(&local));
    assert!(gr::rfc3339(&json!("2026-10-05")).starts_with("2026-10-05T00:00:00"));
    assert_eq!(gr::rfc3339(&json!("2026-10-05T09:00:00Z")), "2026-10-05T09:00:00+00:00");
    assert_eq!(gr::rfc3339(&json!("next friday")), "next friday"); // Google refuses it, said as an error
}

fn dt(s: &str) -> DateTime<FixedOffset> {
    DateTime::parse_from_rfc3339(s).unwrap()
}

#[test]
fn test_free_slots_skip_busy_time_and_keep_to_working_hours() {
    let day = dt("2026-10-05T00:00:00-07:00");
    let at = |h: u32, m: u32| day + Duration::hours(h as i64) + Duration::minutes(m as i64);
    let busy = [(at(10, 0), at(11, 0)), (at(11, 15), at(17, 0))];
    let slots = gr::free_slots(&busy, day, day + Duration::days(1), 30, 9, 18, LA).unwrap();
    assert_eq!(
        slots,
        vec![json!({"start": pytime::isoformat(&at(9, 0)), "end": pytime::isoformat(&at(10, 0))}),
             json!({"start": pytime::isoformat(&at(17, 0)), "end": pytime::isoformat(&at(18, 0))})]
    );
}

#[tokio::test]
async fn test_suggest_time_reads_freebusy_over_the_owners_calendars() {
    let (client, _) = google("calendar", |r| {
        if r.path().ends_with("/calendarList") {
            return json!({"items": [{"id": "me@x", "accessRole": "owner"},
                                    {"id": "en.usa#holiday@group.v.calendar.google.com", "accessRole": "reader"},
                                    {"id": "class@cs.example.edu", "accessRole": "reader"}]});
        }
        let body = r.json();
        // a subscribed calendar is busy time; a holiday calendar is not
        let ids: Vec<&str> = body["items"].as_array().unwrap().iter().map(|i| i["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["me@x", "class@cs.example.edu"]);
        assert!(body["timeMin"].as_str().unwrap().ends_with("-07:00"));
        json!({"calendars": {"me@x": {"busy": [{"start": "2026-10-05T16:00:00Z", "end": "2026-10-06T00:00:00Z"}]}}})
    });
    let out = client
        .with_zone(LA)
        .call_tool("suggest_time", &json!({"startTime": "2026-10-05T08:00:00-07:00", "endTime": "2026-10-05T20:00:00-07:00",
                                           "durationMinutes": 45}))
        .await
        .unwrap();
    // busy 9:00-17:00 local, so the one free stretch inside 9-18 is 17:00-18:00
    assert_eq!(
        *ok(&out),
        json!({"free": [{"start": "2026-10-05T17:00:00-07:00", "end": "2026-10-05T18:00:00-07:00"}], "calendarsChecked": 2})
    );
}

#[tokio::test]
async fn test_list_events_reads_every_calendar_the_owner_sees_in_one_call() {
    // Review, 2026-10-05: list_events read the primary calendar only, so "my next class" (on a subscribed class
    // calendar) came back wrong or took list_calendars plus a call per calendar, one round each; the rundown ran
    // out of rounds. With no calendarId it now reads them all (holidays aside), merged soonest first, each tagged.
    let (client, mock) = google("calendar", |r| {
        if r.path().ends_with("/calendarList") {
            return json!({"items": [{"id": "me@x", "summary": "Me", "primary": true},
                                    {"id": "en.usa#holiday@group.v.calendar.google.com", "summary": "Holidays"},
                                    {"id": "class@cs.example.edu", "summary": "CS 101"}]});
        }
        assert!(r.param("singleEvents").as_deref() == Some("true") && r.param("orderBy").as_deref() == Some("startTime"));
        if r.url.as_str().contains("class%40cs.example.edu") || r.path().contains("class@cs.example.edu") {
            return json!({"timeZone": "America/Los_Angeles", "items": [
                {"id": "c1", "summary": "Lecture", "start": {"dateTime": "2026-10-05T10:00:00-07:00"}}]});
        }
        json!({"timeZone": "America/Los_Angeles", "items": [
            {"id": "m1", "summary": "Dentist", "start": {"dateTime": "2026-10-05T15:00:00-07:00"}},
            {"id": "m0", "summary": "Trip", "start": {"date": "2026-10-05"}}]})
    });
    let out = client.call_tool("list_events", &json!({"startTime": "2026-10-05T08:00:00-07:00"})).await.unwrap();
    let got = ok(&out);
    let summaries: Vec<&str> = got["events"].as_array().unwrap().iter().map(|e| e["summary"].as_str().unwrap()).collect();
    assert_eq!(summaries, ["Trip", "Lecture", "Dentist"]);
    let cals: Vec<&str> = got["events"].as_array().unwrap().iter().map(|e| e["calendar"].as_str().unwrap()).collect();
    assert_eq!(cals, ["Me", "CS 101", "Me"]);
    assert_eq!(got["calendarsRead"], 2);
    assert!(!mock.seen().iter().any(|r| r.path().contains("holiday")));
    // one calendar named: that one only, as before
    mock.clear();
    let one = client.call_tool("list_events", &json!({"calendarId": "primary"})).await.unwrap();
    let paths: Vec<String> = mock.seen().iter().map(|r| r.path().rsplit('/').nth(1).unwrap().to_string()).collect();
    assert_eq!(paths, ["primary"]);
    assert_eq!(ok(&one)["events"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn test_list_events_across_calendars_keeps_what_it_read_when_one_calendar_fails() {
    let (client, _) = google("calendar", |r| {
        if r.path().ends_with("/calendarList") {
            return json!({"items": [{"id": "me@x", "summary": "Me", "primary": true}, {"id": "team@x", "summary": "Team"}]})
                .into();
        }
        if r.path().contains("team") {
            return status(403, json!({"error": {"message": "no access"}}));
        }
        json!({"items": [{"id": "m1", "summary": "Dentist", "start": {"dateTime": "2026-10-05T15:00:00-07:00"}}]}).into()
    });
    let out = client.call_tool("list_events", &json!({})).await.unwrap();
    let got = ok(&out);
    assert_eq!(got["events"].as_array().unwrap().iter().map(|e| e["summary"].clone()).collect::<Vec<_>>(), [json!("Dentist")]);
    assert_eq!(got["unreadable"], json!(["Team"]));
}

#[tokio::test]
async fn test_list_events_fails_when_every_calendar_fails() {
    let (client, _) = google("calendar", |r| {
        if r.path().ends_with("/calendarList") {
            return json!({"items": [{"id": "me@x", "summary": "Me", "primary": true}]}).into();
        }
        status(403, json!({"error": {"message": "no access"}}))
    });
    let out = client.call_tool("list_events", &json!({})).await.unwrap();
    assert_eq!(err_text(&out), "HTTP 403: no access");
}

#[tokio::test]
async fn test_responding_keeps_every_guest_and_changes_only_the_owner() {
    let (client, mock) = google("calendar", |r| {
        if r.method == "GET" {
            return json!({"attendees": [{"email": "ana@x", "responseStatus": "accepted"},
                                        {"email": "me@x", "self": true, "responseStatus": "needsAction"}]});
        }
        json!({"id": "e1"})
    });
    let out = client.call_tool("respond_to_event", &json!({"eventId": "e1", "responseStatus": "declined"})).await.unwrap();
    ok(&out);
    let patch = mock.seen().into_iter().find(|r| r.method == "PATCH").unwrap();
    assert_eq!(patch.param("sendUpdates").as_deref(), Some("all"));
    assert_eq!(
        patch.json(),
        json!({"attendees": [{"email": "ana@x", "responseStatus": "accepted"},
                             {"email": "me@x", "self": true, "responseStatus": "declined"}]})
    );
    let (alone, _) = google("calendar", |_| json!({"attendees": []}));
    let out = alone.call_tool("respond_to_event", &json!({"eventId": "e", "responseStatus": "accepted"})).await.unwrap();
    assert!(err_text(&out).contains("not a guest"));
}

#[tokio::test]
async fn test_guests_are_emailed_only_when_there_are_guests_or_it_is_asked() {
    let (client, mock) = google("calendar", |r| {
        if r.method == "GET" {
            return if r.path().contains("withguests") { json!({"attendees": [{"email": "ana@x"}]}) } else { json!({}) };
        }
        json!({})
    });
    client.call_tool("delete_event", &json!({"eventId": "withguests"})).await.unwrap();
    client.call_tool("delete_event", &json!({"eventId": "alone"})).await.unwrap();
    client.call_tool("delete_event", &json!({"eventId": "withguests", "notifyGuests": false})).await.unwrap();
    let out = client.call_tool("quick_add_event", &json!({"text": "Lunch Friday noon"})).await.unwrap();
    ok(&out);
    let writes: Vec<_> = mock.seen().into_iter().filter(|r| r.method != "GET").collect();
    let sends: Vec<String> = writes.iter().map(|r| r.param("sendUpdates").unwrap()).collect();
    assert_eq!(sends, ["all", "none", "none", "none"]);
    assert_eq!(writes.last().unwrap().param("text").as_deref(), Some("Lunch Friday noon"));
    let deleted = client.call_tool("delete_event", &json!({"eventId": "x", "notifyGuests": true})).await.unwrap();
    assert_eq!(*ok(&deleted), json!({"deleted": "x"}));
}

#[tokio::test]
async fn test_update_adds_guests_to_the_ones_already_there() {
    let (client, mock) = google("calendar", |r| {
        if r.method == "GET" {
            return json!({"attendees": [{"email": "ana@x", "responseStatus": "accepted"}]});
        }
        json!({})
    });
    client
        .call_tool("update_event", &json!({"eventId": "e", "addAttendeeEmails": ["bo@x", "ana@x"], "summary": "Sync"}))
        .await
        .unwrap();
    let patch = mock.seen().into_iter().find(|r| r.method == "PATCH").unwrap();
    assert_eq!(
        patch.json(),
        json!({"summary": "Sync", "attendees": [{"email": "ana@x", "responseStatus": "accepted"}, {"email": "bo@x"}]})
    );
}

// ---- drive ----------------------------------------------------------------------------------------------

#[tokio::test]
async fn test_drive_search_quotes_plain_words_and_passes_a_drive_query_through() {
    let (client, mock) = google("drive", |_| json!({"files": []}));
    client.call_tool("search_files", &json!({"query": "Ana's budget"})).await.unwrap();
    client.call_tool("search_files", &json!({"query": "name contains 'budget'"})).await.unwrap();
    let queries: Vec<String> = mock.seen().iter().map(|r| r.param("q").unwrap()).collect();
    assert_eq!(queries, ["fullText contains 'Ana\\'s budget' and trashed = false", "name contains 'budget' and trashed = false"]);
    let r = &mock.seen()[0];
    assert_eq!(r.param("pageSize").as_deref(), Some("10"));
    assert_eq!(r.param("fields").unwrap(), format!("nextPageToken,files({})", gr::FILE_FIELDS));
}

#[tokio::test]
async fn test_drive_reads_a_doc_by_export_and_says_when_it_cannot() {
    let (client, _) = google("drive", |r| {
        if r.path().ends_with("/export") {
            assert_eq!(r.param("mimeType").as_deref(), Some("text/markdown")); // headings and tables kept
            return text("The plan.", "text/markdown");
        }
        let mime = if r.path().contains("zip") { "application/zip" } else { "application/vnd.google-apps.document" };
        json!({"name": "Plan", "mimeType": mime}).into()
    });
    let doc = client.call_tool("read_file_content", &json!({"fileId": "d1"})).await.unwrap();
    assert_eq!(
        *ok(&doc),
        json!({"name": "Plan", "mimeType": "application/vnd.google-apps.document", "text": "The plan.", "clipped": false})
    );
    let zipped = client.call_tool("read_file_content", &json!({"fileId": "zip1"})).await.unwrap();
    let z = ok(&zipped);
    assert_eq!(z["text"], "");
    assert!(z["note"].as_str().unwrap().contains("not read as text"));
}

#[tokio::test]
async fn test_drive_stops_reading_a_file_at_the_download_cap() {
    // The cap was checked after r.content had read the whole body into memory; now the read stops at it
    let pulled = Arc::new(AtomicUsize::new(0));
    let p = pulled.clone();
    let (client, _) = google("drive", move |r| {
        if r.param("alt").as_deref() == Some("media") {
            let p = p.clone();
            let endless = futures::stream::iter(0..100).map(move |_| {
                p.fetch_add(1, Ordering::SeqCst);
                Ok::<_, TransportError>(Bytes::from(vec![b'x'; 1024 * 1024]))
            });
            return Reply::Stream { status: 200, content_type: "text/plain".into(), body: Box::pin(endless) };
        }
        json!({"name": "Big", "mimeType": "text/plain"}).into()
    });
    let client = client.with_download_max(1024 * 1024);
    let out = client.call_tool("read_file_content", &json!({"fileId": "big"})).await.unwrap();
    assert!(err_text(&out).contains("larger than 1 MB"));
    assert!(pulled.load(Ordering::SeqCst) <= 3);
}

#[tokio::test]
async fn test_drive_download_still_reports_the_http_error() {
    let (client, _) = google("drive", |r| {
        if r.param("alt").as_deref() == Some("media") {
            return status(404, json!({"error": {"message": "File not found"}}));
        }
        json!({"name": "Gone", "mimeType": "text/plain"}).into()
    });
    let out = client.call_tool("read_file_content", &json!({"fileId": "gone"})).await.unwrap();
    assert!(err_text(&out).contains("HTTP 404: File not found"));
}

#[tokio::test]
async fn test_drive_creates_a_file_as_one_multipart_upload() {
    let (client, mock) = google("drive", |_| json!({"id": "f1", "name": "Notes"}));
    let out = client
        .call_tool("create_file", &json!({"title": "Caf\u{e9} notes", "textContent": "line one\nline two",
                                         "mimeType": "application/vnd.google-apps.document"}))
        .await
        .unwrap();
    assert_eq!(*ok(&out), json!({"id": "f1", "name": "Notes"}));
    let r = &mock.seen()[0];
    assert_eq!(r.param("uploadType").as_deref(), Some("multipart"));
    let ct = r.header("content-type").unwrap();
    let boundary = ct.strip_prefix("multipart/related; boundary=").unwrap();
    assert!(boundary.starts_with("eve-upload-") && boundary.len() == "eve-upload-".len() + 16);
    let body = String::from_utf8(r.body.clone().unwrap()).unwrap();
    assert_eq!(
        body,
        format!("--{boundary}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n\
                 {{\"name\": \"Caf\\u00e9 notes\", \"mimeType\": \"application/vnd.google-apps.document\"}}\r\n\
                 --{boundary}\r\nContent-Type: text/plain; charset=UTF-8\r\n\r\nline one\nline two\r\n--{boundary}--")
    );
}

// ---- errors and the tool list ---------------------------------------------------------------------------

#[tokio::test]
async fn test_a_google_error_is_a_tool_error_not_an_exception() {
    let (client, _) = google("gmail", |_| status(403, json!({"error": {"message": "Gmail API has not been used"}})));
    let out = client.call_tool("list_labels", &json!({})).await.unwrap();
    assert!(err_text(&out).contains("HTTP 403: Gmail API has not been used"));
    assert_eq!(client.call_tool("send_email", &json!({})).await.unwrap()["isError"], json!(true));
    let signed_out = GoogleRestClient::new("drive", || None, 60.0).unwrap();
    let out = signed_out.call_tool("list_recent_files", &json!({})).await.unwrap();
    assert!(err_text(&out).contains("not signed in"));
    assert_eq!(GoogleRestClient::new("maps", || None, 60.0).err().unwrap().to_string(), "maps");
}

#[tokio::test]
async fn test_a_missing_argument_is_a_key_error_and_a_dropped_connection_propagates() {
    let (client, _) = google("calendar", |_| json!({}));
    let out = client.call_tool("get_event", &json!({})).await.unwrap();
    assert_eq!(err_text(&out), "KeyError: 'eventId'");
    let out = client.call_tool("list_events", &json!({"pageSize": "lots", "calendarId": "x"})).await.unwrap();
    assert_eq!(err_text(&out), "ValueError: invalid literal for int() with base 10: 'lots'");
    let (client, _) = google("drive", |_| Reply::Fail(TransportError::new("ConnectError", "connection reset")));
    let e = client.call_tool("list_recent_files", &json!({})).await.unwrap_err();
    assert_eq!((e.type_name(), e.to_string().as_str()), ("ConnectError", "connection reset"));
    let (client, _) = google("gmail", |_| text("<html>oops</html>", "text/html"));
    let e = client.call_tool("list_labels", &json!({})).await.unwrap_err();
    assert_eq!((e.type_name(), e.to_string().as_str()), ("AttributeError", "'str' object has no attribute 'get'"));
}

#[tokio::test]
async fn test_the_rest_tools_carry_the_mcp_servers_names() {
    let names: Vec<&str> = gr::TOOLS["calendar"].iter().take(2).map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["list_calendars", "list_events"]);
    for (app, tools) in gr::TOOLS.iter() {
        let (client, _) = google(app, |_| status(500, json!({"error": {"message": "boom"}})));
        assert_eq!(client.list_tools(), *tools);
        for t in tools {
            assert!(t["annotations"]["readOnlyHint"].is_boolean(), "{app} {}", t["name"]);
            let out = client.call_tool(t["name"].as_str().unwrap(), &json!({})).await.unwrap();
            let text = err_text(&out);
            assert!(!text.starts_with("no tool"), "{app} {}: {text}", t["name"]); // every tool has its handler
        }
    }
}

// ---- Lean CalendarEdits replays (test_lean_calendaredits_replay.py) ---------------------------------------

#[tokio::test]
async fn test_list_events_reads_the_primary_when_the_calendar_list_has_a_network_error() {
    // CalendarEdits `current_violates` (P4): the calendar list's request dropped (an httpx error, not a
    // GoogleError), the primary calendar answered, and list_events still failed.
    let (client, _) = google("calendar", |r| {
        if r.path().ends_with("/calendarList") {
            return Reply::Fail(TransportError::new("ConnectError", "connection reset"));
        }
        json!({"timeZone": "America/Los_Angeles", "items": [
            {"id": "m1", "summary": "Dentist", "start": {"dateTime": "2026-10-05T15:00:00-07:00"}}]})
        .into()
    });
    let out = client.call_tool("list_events", &json!({"startTime": "2026-10-05T08:00:00-07:00"})).await.unwrap();
    assert_eq!(ok(&out)["events"][0]["summary"], "Dentist");
}

fn la_slots_hold_to_working_hours(slots: &Value) {
    let slots = slots.as_array().unwrap();
    assert!(!slots.is_empty());
    for s in slots {
        let lo = dt(s["start"].as_str().unwrap()).with_timezone(&chrono_tz::America::Los_Angeles);
        let hi = dt(s["end"].as_str().unwrap()).with_timezone(&chrono_tz::America::Los_Angeles);
        use chrono::Timelike;
        assert!(lo.date_naive() == hi.date_naive() && 9 <= lo.hour() && (hi.hour(), hi.minute()) <= (18, 0), "{s}");
    }
}

fn free_routes(r: &Req) -> Value {
    if r.path().ends_with("/calendarList") {
        json!({"items": [{"id": "me@x", "primary": true}]})
    } else {
        json!({"calendars": {"me@x": {"busy": []}}})
    }
}

#[tokio::test]
async fn test_suggest_time_keeps_to_working_hours_across_the_end_of_daylight_time() {
    // CalendarEdits `current_violates` (P5): the days after DST ends (2026-11-01) got 08:00-17:00 PST.
    let (client, _) = google("calendar", free_routes);
    let out = client
        .with_zone(LA)
        .call_tool("suggest_time", &json!({"startTime": "2026-10-30T00:00:00", "endTime": "2026-11-04T00:00:00",
                                           "durationMinutes": 60}))
        .await
        .unwrap();
    la_slots_hold_to_working_hours(&ok(&out)["free"]);
}

#[tokio::test]
async fn test_suggest_time_keeps_to_the_owners_working_hours_when_asked_in_utc() {
    // CalendarEdits `current_violates` (P5, second trace): asked in UTC, 09:00-18:00 UTC was offered.
    let (client, _) = google("calendar", free_routes);
    let out = client
        .with_zone(LA)
        .call_tool("suggest_time", &json!({"startTime": "2026-10-06T00:00:00Z", "endTime": "2026-10-07T00:00:00Z",
                                           "durationMinutes": 60}))
        .await
        .unwrap();
    la_slots_hold_to_working_hours(&ok(&out)["free"]);
}

// ---- the routine-write halves (test_routine_app_policy.py, test_routine_confirmations.py) ---------------

#[tokio::test]
async fn test_routine_calendar_decision_matches_real_rest_delivery_parameters() {
    // A pre-existing event has guests; the edit must not silently inherit delivery.
    let (client, mock) = google("calendar", |_| json!({"id": "event", "attendees": [{"email": "guest@example.org"}]}));
    let create = json!({"summary": "Focus", "startTime": "2026-10-10T09:00:00Z", "endTime": "2026-10-10T10:00:00Z"});
    client.call_tool("create_event", &create).await.unwrap();
    let last = mock.seen().pop().unwrap();
    assert_eq!(last.param("sendUpdates").as_deref(), Some("none"));
    assert!(last.json().get("attendees").is_none());

    mock.clear();
    client.call_tool("update_event", &json!({"eventId": "event", "summary": "Moved", "notifyGuests": false})).await.unwrap();
    let seen = mock.seen();
    assert!(seen.len() == 1 && seen[0].method == "PATCH");
    assert_eq!(seen[0].param("sendUpdates").as_deref(), Some("none"));

    // Positive control: omitting the flag reads the guests and emails them.
    mock.clear();
    client.call_tool("update_event", &json!({"eventId": "event", "summary": "Moved"})).await.unwrap();
    let methods: Vec<String> = mock.seen().iter().map(|r| r.method.clone()).collect();
    assert_eq!(methods, ["GET", "PATCH"]);
    assert_eq!(mock.seen()[1].param("sendUpdates").as_deref(), Some("all"));
}

#[tokio::test]
async fn test_prepare_reads_the_full_event_with_no_field_mask() {
    // apps_mcp's prepare reads `client._req("GET", f"{client._cal(prepared)}/{quote(event_id)}")`.
    let (client, mock) = google("calendar", |_| json!({"id": "event", "attendees": []}));
    let args = obj(json!({"eventId": "event"}));
    let url = format!("{}/{}", GoogleRestClient::cal(&args), "event");
    let event = client.req("GET", &url, &vec![], gr::Body::None).await.unwrap();
    assert_eq!(event, json!({"id": "event", "attendees": []}));
    let r = &mock.seen()[0];
    assert!(!r.has_params()); // neither a field mask nor maxAttendees can hide guests
    assert!(r.path().ends_with("/calendars/primary/events/event"));
    let shared = obj(json!({"calendarId": "team@x.com"}));
    assert_eq!(GoogleRestClient::cal(&shared), format!("{}/calendars/team%40x.com/events", gr::CALENDAR));
}
