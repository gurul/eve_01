//! updates_composio: Updates' Composio sources (Slack, Notion, Linear, the work Gmail and Calendar). A port of
//! `bridge/tests/test_updates_composio.py`. Each source parses Composio's answer shape
//! (bridge/tests/fixtures/updates/composio/responses.json, every value fictional), is off without a connection,
//! fails (and so is lost) on an expired one, and can only run the exact read slugs on its allow-list. No network: a
//! fake bridge answers from the fixture.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock};

use async_trait::async_trait;
use chrono::{Duration, FixedOffset, TimeZone};
use indexmap::IndexMap;
use parking_lot::Mutex;
use serde_json::{json, Value};

use eve_composio_tools::{consequential_slugs, is_read_only, ComposioError, MULTI_EXECUTE};
use eve_link::seams::{Chief, ComposioRead, DirectApps, Obj};
use eve_link::updates::{
    self, clean_cards, gather, lost_kinds, ref_kind, GatherOptions, SuggestClient, UpdateSources, Updates,
    UpdatesOptions,
};
use eve_link::updates_composio::{self as uc, Reader, READ_SLUGS};
use eve_link::{monotonic, Env, Mono, Now};
use eve_transcripts::transcripts::Pred;
use eve_transcripts::Dt;

static FIXTURE: LazyLock<Value> = LazyLock::new(|| {
    serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../bridge/tests/fixtures/updates/composio/responses.json"
    )))
    .expect("fixture")
});

/// 2026-10-12 09:30 at -07:00.
fn now() -> Dt {
    FixedOffset::west_opt(7 * 3600).unwrap().with_ymd_and_hms(2026, 10, 12, 9, 30, 0).unwrap()
}

fn composio_kinds() -> BTreeSet<String> {
    uc::KINDS.iter().map(|k| k.to_string()).collect()
}

fn obj(v: Value) -> Obj {
    match v {
        Value::Object(o) => o,
        _ => panic!("not an object"),
    }
}

fn s(v: &Value) -> &str {
    v.as_str().unwrap_or("")
}

fn fixture_accounts() -> Vec<Value> {
    FIXTURE["connected_accounts"].as_array().unwrap().clone()
}

/// ComposioBridge as updates_composio reads it: `can_read`, `connected_accounts()` and `read_tool(slug, arguments,
/// account_id)`, answering from the fixture. `fail` names slugs that fail; `accounts` replaces the listing;
/// `list_fails` makes the listing raise.
struct FakeBridge {
    can_read: AtomicBool,
    accounts: Mutex<Vec<Value>>,
    fail: Vec<String>,
    list_fails: bool,
    calls: Mutex<Vec<(String, Obj, String)>>,
    listed: AtomicUsize,
}

impl FakeBridge {
    fn new() -> Arc<Self> {
        Self::with(None, &[], false)
    }

    fn with(accounts: Option<Vec<Value>>, fail: &[&str], list_fails: bool) -> Arc<Self> {
        Arc::new(FakeBridge {
            can_read: AtomicBool::new(true),
            accounts: Mutex::new(accounts.unwrap_or_else(fixture_accounts)),
            fail: fail.iter().map(|f| f.to_string()).collect(),
            list_fails,
            calls: Mutex::new(Vec::new()),
            listed: AtomicUsize::new(0),
        })
    }

    fn calls(&self) -> Vec<(String, Obj, String)> {
        self.calls.lock().clone()
    }

    fn answer(slug: &str, args: &Obj) -> Value {
        match slug {
            "SLACK_SEARCH_MESSAGES" => {
                let q = args.get("query").and_then(Value::as_str).unwrap_or("");
                let key = if q == "from:me" {
                    "from:me"
                } else if q.starts_with("is:dm") {
                    "is:dm"
                } else {
                    "mention"
                };
                FIXTURE["SLACK_SEARCH_MESSAGES"][key].clone()
            }
            "NOTION_GET_ABOUT_ME" => FIXTURE["NOTION"]["about_me"].clone(),
            "NOTION_SEARCH_NOTION_PAGE" => FIXTURE["NOTION"]["search"].clone(),
            "NOTION_FETCH_BLOCK_CONTENTS" => {
                match FIXTURE["NOTION"]["blocks"].get(args.get("block_id").and_then(Value::as_str).unwrap_or("")) {
                    Some(v) if !v.is_null() => v.clone(),
                    _ => json!({"data": {"results": []}, "successful": true}),
                }
            }
            "LINEAR_LIST_LINEAR_ISSUES" => FIXTURE["LINEAR_LIST_LINEAR_ISSUES"].clone(),
            "GMAIL_FETCH_EMAILS" => {
                let query = args.get("query").and_then(Value::as_str).unwrap_or("");
                let key = if query.contains("in:sent") { "sent" } else { "inbox" };
                FIXTURE["GMAIL_FETCH_EMAILS"][key].clone()
            }
            "GOOGLECALENDAR_EVENTS_LIST" => FIXTURE["GOOGLECALENDAR_EVENTS_LIST"].clone(),
            other => panic!("unexpected slug {other}"),
        }
    }
}

#[async_trait]
impl ComposioRead for FakeBridge {
    fn can_read(&self) -> bool {
        self.can_read.load(Ordering::SeqCst)
    }

    async fn connected_accounts(&self) -> Result<Vec<Value>, ComposioError> {
        self.listed.fetch_add(1, Ordering::SeqCst);
        if self.list_fails {
            return Err(ComposioError::new("ConnectionError", "down"));
        }
        Ok(self.accounts.lock().clone())
    }

    async fn read_tool(&self, slug: &str, arguments: &Obj, account_id: &str) -> Value {
        self.calls.lock().push((slug.to_string(), arguments.clone(), account_id.to_string()));
        if self.fail.iter().any(|f| f == slug) {
            return json!({"ok": false, "reason": "HTTP 401: the connection has expired"});
        }
        let mut answer = obj(Self::answer(slug, arguments));
        answer.insert("ok".into(), Value::Bool(true));
        Value::Object(answer)
    }
}

fn reader(bridge: &Arc<FakeBridge>) -> Reader {
    Reader::new(bridge.clone() as Arc<dyn ComposioRead>)
}

fn mono() -> Mono {
    Arc::new(monotonic)
}

/// Python's test `Sources` subclass: no direct apps, no vault, no chief; the records folder, the hidden words and
/// the Composio bridge are lent, read through the real `updates::Sources` (which checks the switch and `usable`).
struct TestSources {
    inner: updates::Sources,
}

#[async_trait]
impl UpdateSources for TestSources {
    fn apps(&self, _app: &str) -> Option<Arc<dyn DirectApps>> {
        None
    }

    fn vault(&self) -> Option<PathBuf> {
        None
    }

    fn chief(&self) -> Option<Arc<dyn Chief>> {
        None
    }

    fn records_dir(&self) -> Option<PathBuf> {
        self.inner.records_dir()
    }

    fn composio(&self) -> anyhow::Result<Option<Arc<dyn ComposioRead>>> {
        self.inner.composio()
    }

    fn hidden(&self) -> anyhow::Result<Option<Pred>> {
        self.inner.hidden()
    }
}

type HiddenMaker = Arc<dyn Fn() -> Option<Pred> + Send + Sync>;

fn sources(bridge: Option<Arc<FakeBridge>>, hidden: Option<HiddenMaker>, environ: Option<Env>) -> TestSources {
    let bridge: Option<Arc<dyn ComposioRead>> = bridge.map(|b| b as Arc<dyn ComposioRead>);
    TestSources {
        inner: updates::Sources {
            inlet: None,
            records_dir: Some(Arc::new(|| None)),
            hidden: Some(Arc::new(move || Ok(hidden.as_ref().and_then(|h| h())))),
            composio: Some(Arc::new(move || bridge.clone())),
            environ: Some(environ.unwrap_or_default()),
        },
    }
}

async fn run_gather(src: &TestSources, exclude: &[&str]) -> (Obj, IndexMap<String, String>) {
    let mut status = IndexMap::new();
    let opts = GatherOptions {
        now: Some(now()),
        tz: Some("America/Los_Angeles".into()),
        exclude: exclude.iter().map(|e| e.to_string()).collect(),
        ..GatherOptions::default()
    };
    let snap = gather(src, opts, Some(&mut status)).await;
    (snap, status)
}

fn by_id(items: Vec<Obj>) -> IndexMap<String, Obj> {
    items.into_iter().map(|i| (s(&i["id"]).to_string(), i)).collect()
}

fn snap_items(snap: &Obj) -> Vec<Obj> {
    snap["items"].as_array().unwrap().iter().map(|i| obj(i.clone())).collect()
}

fn status_of(status: &IndexMap<String, String>, kind: &str) -> String {
    status.get(kind).cloned().unwrap_or_default()
}

fn composio_statuses(status: &IndexMap<String, String>) -> BTreeSet<String> {
    uc::KINDS.iter().map(|k| status_of(status, k)).collect()
}

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|i| i.to_string()).collect()
}

// ---- parsing, one source at a time ------------------------------------------------------------------------------

#[tokio::test]
async fn test_slack_keeps_unanswered_dms_and_mentions_one_per_conversation() {
    let rows = by_id(uc::slack_items(&reader(&FakeBridge::new()), now()).await.unwrap());
    let dm = rows.iter().find(|(k, _)| k.starts_with("slack:D0000DM01/")).map(|(_, v)| v).unwrap();
    let mention = rows.iter().find(|(k, _)| k.starts_with("slack:C0000GEN1/")).map(|(_, v)| v).unwrap();
    assert_eq!(rows.len(), 2); // the answered DM and the 11-day-old one are out
    assert_eq!(s(&dm["kind"]), "slack");
    assert_eq!(s(&dm["title"]), "Slack: direct message from sam.rivera");
    let body = s(&dm["body"]);
    assert!(body.contains("2 messages the owner has not answered"));
    assert!(body.contains("budget sheet") && !body.contains("https://")); // a Slack link reads as its label
    assert!(body.contains("@you") && !body.contains("UOWNER001"));
    assert!(s(&dm["link"]).starts_with("https://example-team.slack.com/archives/D0000DM01/"));
    let ts = s(&FIXTURE["SLACK_SEARCH_MESSAGES"]["is:dm"]["data"]["messages"]["matches"][0]["ts"]);
    assert!(s(&dm["id"]).ends_with(ts));
    assert_eq!(s(&mention["title"]), "Slack: mention in #project-alpha from alex.chen");
    assert!(s(&mention["body"]).contains("#project-alpha"));
    assert_eq!(rows.keys().next().unwrap(), s(&mention["id"])); // newest first
}

#[tokio::test]
async fn test_notion_reads_open_todos_mentions_and_assigned_open_tasks() {
    let rows = by_id(uc::notion_items(&reader(&FakeBridge::new()), now(), None, &mono()).await.unwrap());
    let (p1, p2) = ("notion:aaaaaaaa-0000-4000-8000-000000000001", "notion:aaaaaaaa-0000-4000-8000-000000000002");
    // the done task, the page with nothing to do and the old page are out
    assert_eq!(rows.keys().cloned().collect::<BTreeSet<_>>(), set(&[p1, p2]));
    let trip = &rows[p1];
    assert_eq!(s(&trip["title"]), "Notion: Trip planning");
    assert!(
        s(&trip["body"]).contains("- [ ] Book the ferry for Oct 18") && !s(&trip["body"]).contains("Renew passport")
    );
    assert!(s(&trip["body"]).contains("Mentions the owner: @Owner please confirm the dates"));
    assert!(s(&trip["link"]).starts_with("https://www.notion.so/"));
    let task = &rows[p2];
    assert!(s(&task["body"]).contains("Assigned to the owner. Status: In progress."));
    assert!(s(&task["body"]).contains("Date: 2026-10-15."));
}

#[tokio::test]
async fn test_notion_reads_no_blocks_after_its_deadline() {
    let bridge = FakeBridge::new();
    let clock: Mono = Arc::new(|| 1.0);
    let rows = uc::notion_items(&reader(&bridge), now(), Some(0.0), &clock).await.unwrap();
    let ids: Vec<&str> = rows.iter().map(|r| s(&r["id"])).collect();
    assert_eq!(ids, ["notion:aaaaaaaa-0000-4000-8000-000000000002"]); // assigned: no blocks needed
    assert!(!bridge.calls().iter().any(|c| c.0 == "NOTION_FETCH_BLOCK_CONTENTS"));
}

#[tokio::test]
async fn test_linear_keeps_open_issues_due_soon_or_recently_updated() {
    let bridge = FakeBridge::new();
    let rows = uc::linear_items(&reader(&bridge), now()).await.unwrap();
    let ids: Vec<&str> = rows.iter().map(|r| s(&r["id"])).collect();
    assert_eq!(
        ids,
        ["linear:bbbbbbbb-0000-4000-8000-000000000002", "linear:bbbbbbbb-0000-4000-8000-000000000001"] // due first
    );
    let (due, recent) = (&rows[0], &rows[1]);
    assert_eq!(s(&due["title"]), "Linear EX-102: Write the migration guide");
    assert!(s(&due["body"]).contains("Due: 2026-10-14."));
    assert_eq!(s(&due["when"]), "2026-10-14");
    assert!(s(&recent["body"]).contains("Priority: Urgent.") && s(&recent["body"]).contains("State: In Progress."));
    assert!(s(&recent["link"]).starts_with("https://linear.app/"));
    assert_eq!(Value::Object(bridge.calls()[0].1.clone()), json!({"assignee_id": "me", "first": uc::LINEAR_FIRST}));
}

#[tokio::test]
async fn test_work_email_reads_the_work_account_only_one_item_per_thread() {
    let bridge = FakeBridge::new();
    let rows = by_id(uc::work_email_items(&reader(&bridge), now()).await.unwrap());
    assert_eq!(rows.keys().cloned().collect::<BTreeSet<_>>(), set(&["wgmail:work/th1", "wgmail:work/th2"]));
    let (renewal, agenda) = (&rows["wgmail:work/th1"], &rows["wgmail:work/th2"]);
    assert_eq!(s(&renewal["kind"]), "work_email");
    assert_eq!(s(&renewal["title"]), "Work email: Contract renewal");
    assert!(s(&renewal["body"]).contains("Please sign the renewal by Oct 20.")); // the thread's newest message
    assert!(s(&renewal["from"]).starts_with("Pat Lee"));
    assert!(s(&agenda["body"]).contains("The owner already replied after this message, on 2026-10-11."));
    assert!(s(&renewal["link"]).starts_with("https://mail.google.com/"));
    let accounts: HashSet<String> = bridge.calls().into_iter().map(|c| c.2).collect();
    assert_eq!(accounts, HashSet::from(["ca_gmailwork".to_string()])); // never the personal account
    let calls = bridge.calls();
    let inbox = calls.iter().map(|c| &c.1).find(|a| s(&a["query"]).contains("in:inbox")).unwrap();
    assert_eq!(s(&inbox["query"]), updates::MAIL_QUERY);
}

#[tokio::test]
async fn test_work_calendar_reads_like_the_owners_calendar() {
    let bridge = FakeBridge::new();
    let rows = by_id(uc::work_calendar_items(&reader(&bridge), now()).await.unwrap());
    // declined and cancelled are out
    assert_eq!(rows.keys().cloned().collect::<BTreeSet<_>>(), set(&["wcal:work/w1", "wcal:work/w3"]));
    let review = &rows["wcal:work/w1"];
    assert_eq!(s(&review["title"]), "Work calendar: Design review");
    assert_eq!(s(&review["kind"]), "work_calendar");
    assert!(s(&review["body"]).contains("The owner has not answered this invitation."));
    assert!(s(&review["body"]).contains("Guests: 4"));
    assert!(s(&review["link"]).starts_with("https://www.google.com/calendar/"));
    let accounts: HashSet<String> = bridge.calls().into_iter().map(|c| c.2).collect();
    assert_eq!(accounts, HashSet::from(["ca_calwork01".to_string()]));
}

#[tokio::test]
async fn test_no_other_account_means_the_work_kinds_are_off() {
    let only_personal: Vec<Value> = fixture_accounts().into_iter().filter(|a| s(&a["alias"]) != "work").collect();
    let reader = reader(&FakeBridge::with(Some(only_personal), &[], false));
    for kind in ["work_email", "work_calendar"] {
        let err = reader.accounts_for(kind).await.unwrap_err();
        assert!(matches!(err, updates::ReadError::Off(_)), "{kind}: {err:?}");
    }
}

#[test]
fn test_links_are_kept_only_on_the_apps_own_https_hosts() {
    let ok = |url: &str, kind: &str| uc::safe_link(Some(&json!(url)), Some(kind));
    assert!(ok("https://team.slack.com/archives/C1/p1", "slack").is_some());
    assert!(ok("https://linear.app/x/issue/a-1", "linear").is_some());
    assert_eq!(ok("http://team.slack.com/archives/C1/p1", "slack"), None);
    assert_eq!(ok("https://slack.com.evil.example/x", "slack"), None);
    assert_eq!(ok("https://user@team.slack.com/x", "slack"), None);
    assert_eq!(ok("https://linear.app/x", "slack"), None);
    assert_eq!(ok("javascript:alert(1)", "notion"), None);
}

/// Beyond Python's test: what urllib.parse.urlsplit does with ports, case, brackets and odd input (checked against
/// Python 3.13).
#[test]
fn test_safe_link_reads_the_url_as_urlsplit_does() {
    let ok = |url: &str, kind: Option<&str>| uc::safe_link(Some(&json!(url)), kind);
    assert!(ok("https://a.slack.com:443/x", Some("slack")).is_some());
    assert!(ok("https://a.slack.com:0443/x", Some("slack")).is_some());
    assert!(ok("https://a.slack.com:/x", Some("slack")).is_some()); // an empty port is none
    assert_eq!(ok("https://a.slack.com:8443/x", Some("slack")), None);
    assert_eq!(ok("https://a.slack.com:99999/x", Some("slack")), None);
    assert_eq!(ok("https://a.slack.com:ab/x", Some("slack")), None);
    assert!(ok("HTTPS://A.SLACK.COM/x", Some("slack")).is_some()); // scheme and host read in lower case
    assert!(ok("https://slack.com", Some("slack")).is_some());
    assert_eq!(ok("https://notslack.com/x", Some("slack")), None);
    assert_eq!(ok("https://[::1]/x", Some("slack")), None);
    assert_eq!(ok("https://a.slack.com]/x", Some("slack")), None);
    assert!(ok("https://[v1.a.slack.com]/x", Some("slack")).is_some()); // an IPvFuture host, as Python reads it
    assert!(ok("https://linear.app/x", None).is_some()); // any kind's host
    assert_eq!(ok("https://linear.app/x", Some("nope")), None);
    assert_eq!(ok("https://a.slack.com/a b", Some("slack")), None);
    assert_eq!(ok("https://a.slack.com/a\\b", Some("slack")), None);
    assert_eq!(ok("https://a.slack.com/\u{e9}", Some("slack")), None);
    assert_eq!(ok("https://a.slack.com/\t", Some("slack")), None);
    assert_eq!(ok("", Some("slack")), None);
    assert_eq!(ok(&format!("https://a.slack.com/{}", "x".repeat(2048)), Some("slack")), None);
    assert_eq!(uc::safe_link(Some(&json!(5)), Some("slack")), None);
    assert_eq!(uc::safe_link(None, Some("slack")), None);
}

#[test]
fn test_small_readers() {
    assert_eq!(uc::alias_word("  Work Mail! "), "work-mail");
    assert_eq!(uc::alias_word("--"), "other");
    assert_eq!(uc::alias_word(&"a".repeat(40)), "a".repeat(32));
    assert_eq!(uc::day(&json!("2026-10-14T00:00:00Z")).as_deref(), Some("2026-10-14"));
    assert_eq!(uc::day(&json!("Oct 14")), None);
    assert_eq!(uc::day(&Value::Null), None);
    let when = uc::parse(&json!("2026-10-12T16:30:00Z")).unwrap();
    assert_eq!(when, now());
    let naive = uc::parse(&json!("2026-10-12T16:30:00")).unwrap(); // naive reads as UTC
    assert_eq!(naive, now());
    assert_eq!(uc::parse(&json!("yesterday")), None);
    assert_eq!(uc::parse(&json!("")), None);
    assert_eq!(uc::iso(&json!("2026-10-12T09:30:00-07:00")), "2026-10-12T09:30:00-07:00");
    assert_eq!(uc::iso(&json!("x".repeat(50))), "x".repeat(40));
    // epoch seconds read in this computer's zone, to the second
    let local =
        chrono::DateTime::from_timestamp(1_791_745_200, 0).unwrap().with_timezone(&chrono::Local).fixed_offset();
    let want = eve_google_rest::pytime::isoformat(&local);
    assert_eq!(uc::iso(&json!("1791745200.000100")), want);
    assert_eq!(uc::iso(&json!(1_791_745_200)), want);
    assert_eq!(uc::iso(&json!(1_791_745_199.9999996)), want); // the microseconds round up into the second
    assert!(uc::try_iso(&json!("999999999999")).is_err()); // past year 9999: Python's ValueError
    assert_eq!(
        uc::slack_text(
            &json!("<@U1|sam> see <#C1|general> <https://x.test|the doc> <https://y.test> <!here> &amp;"),
            Some("U1")
        ),
        "@you see #general the doc (link) @here &"
    );
    assert_eq!(uc::slack_text(&json!("<@U2>"), Some("U1")), "@someone");
    assert_eq!(uc::slack_text(&Value::Null, None), "");
    assert_eq!(uc::labels(&obj(json!({"labelIds": ["INBOX", 3]}))), ["INBOX", "3"]);
    assert!(uc::labels(&obj(json!({"labelIds": "INBOX"}))).is_empty());
    assert!(uc::slack_ok(&obj(json!({"channel": {"id": "D0000DM01"}, "ts": "1791745200.000100"}))));
    assert!(!uc::slack_ok(&obj(json!({"channel": {"id": "d1"}, "ts": "1791745200.000100"}))));
    assert!(!uc::slack_ok(&obj(json!({"channel": {"id": "D0000DM01"}, "ts": "17917"}))));
}

#[test]
fn test_notion_page_reads_its_properties() {
    let page = obj(json!({"properties": {
        "Name": {"type": "title", "title": [{"plain_text": "Ship "}, {"plain_text": "it"}]},
        "Owner": {"type": "people", "people": [{"id": "u-1"}]},
        " State ": {"type": "select", "select": {"name": "Won't do"}},
        "When": {"type": "date", "date": {"start": "2026-10-15T10:00:00.000-07:00"}},
    }}));
    let facts = uc::notion_page(&page, Some("u-1"));
    assert_eq!(
        Value::Object(facts),
        json!({"title": "Ship it", "status": "Won't do", "due": "2026-10-15", "assigned": true, "done": true})
    );
    let facts = uc::notion_page(&page, None);
    assert_eq!(facts["assigned"], json!(false));
    let ticked = obj(json!({"properties": {"Done": {"type": "checkbox", "checkbox": true}}}));
    assert_eq!(uc::notion_page(&ticked, None)["done"], json!(true));
    // a people property Python cannot iterate is its TypeError
    let odd = obj(json!({"properties": {"Owner": {"type": "people", "people": 5}}}));
    assert_eq!(uc::try_notion_page(&odd, Some("u-1")).unwrap_err().kind(), "TypeError");
}

// ---- gather: status, off, failed, lost ------------------------------------------------------------------------

#[tokio::test]
async fn test_every_composio_source_lands_in_gather_with_status_ok() {
    let (snap, status) = run_gather(&sources(Some(FakeBridge::new()), None, None), &[]).await;
    for k in uc::KINDS {
        assert_eq!(status_of(&status, k), "ok", "{k}");
    }
    let items = snap_items(&snap);
    let kinds: BTreeSet<String> = items.iter().map(|i| s(&i["kind"]).to_string()).collect();
    assert_eq!(kinds, composio_kinds());
    let allowed = set(&["id", "kind", "title", "from", "when", "body", "link"]);
    for it in &items {
        assert!(it.keys().all(|k| allowed.contains(k)), "{it:?}");
        assert_eq!(ref_kind(s(&it["id"])), Some(s(&it["kind"])));
        assert!(s(&it["body"]).chars().count() <= updates::BODY_CHARS);
        assert!(s(&it["title"]).chars().count() <= updates::TITLE_CHARS);
    }
}

#[tokio::test]
async fn test_the_model_never_reads_an_items_link() {
    let (snap, _) = run_gather(&sources(Some(FakeBridge::new()), None, None), &[]).await;
    let body = updates::prompt_input(&snap);
    assert!(!body.contains("slack.com") && !body.contains("linear.app") && !body.contains("notion.so"));
}

#[tokio::test]
async fn test_off_without_a_bridge_or_with_the_switch_off() {
    let off: Env = HashMap::from([("EVE_UPDATES_COMPOSIO".to_string(), "0".to_string())]);
    for src in [sources(None, None, None), sources(Some(FakeBridge::new()), None, Some(off))] {
        let (snap, status) = run_gather(&src, &[]).await;
        assert_eq!(composio_statuses(&status), set(&["off"]));
        assert!(snap_items(&snap).is_empty());
    }
}

#[tokio::test]
async fn test_a_bridge_that_cannot_read_is_off() {
    let bridge = FakeBridge::new();
    bridge.can_read.store(false, Ordering::SeqCst); // Composio not installed or not started
    let (_, status) = run_gather(&sources(Some(bridge.clone()), None, None), &[]).await;
    assert_eq!(composio_statuses(&status), set(&["off"]));
    assert!(bridge.calls().is_empty());
}

#[tokio::test]
async fn test_a_toolkit_that_is_not_connected_is_off_and_the_rest_still_read() {
    let accounts: Vec<Value> = fixture_accounts().into_iter().filter(|a| s(&a["toolkit"]) != "linear").collect();
    let (_, status) = run_gather(&sources(Some(FakeBridge::with(Some(accounts), &[], false)), None, None), &[]).await;
    assert_eq!(status_of(&status, "linear"), "off");
    assert_eq!(status_of(&status, "slack"), "ok");
    assert_eq!(status_of(&status, "notion"), "ok");
}

fn expired(toolkit: &str, accounts: Vec<Value>) -> Vec<Value> {
    accounts
        .into_iter()
        .map(|mut a| {
            if s(&a["toolkit"]) == toolkit {
                a["status"] = json!("EXPIRED");
            }
            a
        })
        .collect()
}

#[tokio::test]
async fn test_an_expired_connection_fails_and_is_lost() {
    let accounts = expired("notion", fixture_accounts());
    let (_, status) = run_gather(&sources(Some(FakeBridge::with(Some(accounts), &[], false)), None, None), &[]).await;
    assert_eq!(status_of(&status, "notion"), "failed");
    assert_eq!(status_of(&status, "slack"), "ok");
    let before: IndexMap<String, String> = IndexMap::from([("notion".to_string(), "ok".to_string())]);
    assert!(lost_kinds(&status, &before).contains("notion"));
    assert!(lost_kinds(&status, &IndexMap::new()).contains("notion")); // failed is lost even with no build before
}

#[tokio::test]
async fn test_a_failing_call_fails_its_source_only() {
    let bridge = FakeBridge::with(None, &["LINEAR_LIST_LINEAR_ISSUES"], false);
    let (_, status) = run_gather(&sources(Some(bridge), None, None), &[]).await;
    assert_eq!(status_of(&status, "linear"), "failed");
    assert_eq!(status_of(&status, "work_email"), "ok");
}

#[tokio::test]
async fn test_an_unreadable_listing_fails_every_composio_source() {
    let (_, status) = run_gather(&sources(Some(FakeBridge::with(None, &[], true)), None, None), &[]).await;
    assert_eq!(composio_statuses(&status), set(&["failed"]));
}

#[tokio::test]
async fn test_a_source_that_goes_off_after_a_full_build_is_lost() {
    let accounts: Vec<Value> = fixture_accounts().into_iter().filter(|a| s(&a["toolkit"]) != "slack").collect();
    let (_, status) = run_gather(&sources(Some(FakeBridge::with(Some(accounts), &[], false)), None, None), &[]).await;
    assert_eq!(status_of(&status, "slack"), "off");
    let was = |v: &str| IndexMap::from([("slack".to_string(), v.to_string())]);
    assert!(lost_kinds(&status, &was("ok")).contains("slack"));
    assert!(!lost_kinds(&status, &was("off")).contains("slack"));
}

#[tokio::test]
async fn test_the_accounts_are_listed_once_per_gather() {
    let bridge = FakeBridge::new();
    run_gather(&sources(Some(bridge.clone()), None, None), &[]).await;
    assert_eq!(bridge.listed.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn test_hidden_words_and_dismissed_refs_drop_composio_items() {
    let hidden: HiddenMaker = Arc::new(|| {
        Some(Box::new(|text: &str| {
            let t = text.to_lowercase();
            t.contains("budget sheet") || t.contains("migration guide")
        }) as Pred)
    });
    let (snap, _) = run_gather(&sources(Some(FakeBridge::new()), Some(hidden), None), &["wcal:work/w3"]).await;
    let ids: BTreeSet<String> = snap_items(&snap).iter().map(|i| s(&i["id"]).to_string()).collect();
    assert!(!ids.iter().any(|i| i.starts_with("slack:D0000DM01/"))); // matched in the body
    assert!(!ids.contains("linear:bbbbbbbb-0000-4000-8000-000000000002")); // matched in the title
    assert!(!ids.contains("wcal:work/w3") && ids.contains("wcal:work/w1")); // dismissed
    assert!(ids.contains("linear:bbbbbbbb-0000-4000-8000-000000000001"));
}

// ---- the allow-list -------------------------------------------------------------------------------------------

#[test]
fn test_every_allowed_slug_only_reads() {
    for slug in READ_SLUGS {
        assert!(is_read_only(slug), "{slug}");
        assert!(consequential_slugs(MULTI_EXECUTE, &json!({"tools": [{"tool_slug": slug}]})).is_empty(), "{slug}");
    }
}

#[tokio::test]
async fn test_a_sending_slug_is_refused_before_it_reaches_composio() {
    let bridge = FakeBridge::new();
    let reader = reader(&bridge);
    for slug in ["SLACK_SEND_MESSAGE", "GMAIL_SEND_EMAIL", "LINEAR_CREATE_LINEAR_ISSUE", "NOTION_ARCHIVE_NOTION_PAGE"] {
        let err = reader.call(slug, &Obj::new(), "ca_slack0001").await.unwrap_err();
        assert_eq!(err.kind(), "PermissionError", "{slug}");
    }
    assert!(bridge.calls().is_empty());
}

/// Positive control for the second guard: a writing slug that somehow got onto the allow-list is still refused.
#[tokio::test]
async fn test_the_consequential_check_guards_even_an_allow_listed_slug() {
    let mut widened: Vec<&str> = READ_SLUGS.to_vec();
    widened.push("SLACK_SEND_MESSAGE");
    let bridge = FakeBridge::new();
    let reader = Reader::with_allowed(bridge.clone() as Arc<dyn ComposioRead>, &widened);
    let err = reader
        .call("SLACK_SEND_MESSAGE", &obj(json!({"channel": "C1", "text": "hi"})), "ca_slack0001")
        .await
        .unwrap_err();
    assert_eq!(err.kind(), "PermissionError");
    assert_eq!(err.to_string(), "SLACK_SEND_MESSAGE is not a read");
    assert!(bridge.calls().is_empty());
}

#[tokio::test]
async fn test_every_slug_the_sources_run_is_on_the_allow_list() {
    let bridge = FakeBridge::new();
    run_gather(&sources(Some(bridge.clone()), None, None), &[]).await;
    let ran: BTreeSet<String> = bridge.calls().into_iter().map(|c| c.0).collect();
    assert!(!ran.is_empty());
    assert!(ran.iter().all(|slug| READ_SLUGS.contains(&slug.as_str())), "{ran:?}");
}

/// Beyond Python's test: the reads `reads` hands gather share one Reader (one listing) and are None without a
/// bridge that can read.
#[tokio::test]
async fn test_reads_share_one_listing_and_are_off_without_a_usable_bridge() {
    let none = uc::reads(None, now(), &HashMap::new(), mono());
    assert_eq!(none.iter().map(|(k, _)| *k).collect::<Vec<_>>(), uc::KINDS);
    assert!(none.iter().all(|(_, r)| r.is_none()));
    let idle = FakeBridge::new();
    idle.can_read.store(false, Ordering::SeqCst);
    assert!(uc::reads(Some(idle as Arc<dyn ComposioRead>), now(), &HashMap::new(), mono())
        .iter()
        .all(|(_, r)| r.is_none()));
    let bridge = FakeBridge::new();
    let reads = uc::reads(Some(bridge.clone() as Arc<dyn ComposioRead>), now(), &HashMap::new(), mono());
    let mut got = 0;
    for (_, read) in reads {
        got += read.unwrap()().await.unwrap().len();
    }
    assert!(got > 0);
    assert_eq!(bridge.listed.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn test_an_expired_only_connection_fails_and_a_missing_one_is_off() {
    let bridge = FakeBridge::with(Some(expired("linear", fixture_accounts())), &[], false);
    let err = reader(&bridge).accounts_for("linear").await.unwrap_err();
    assert_eq!((err.kind(), err.to_string().as_str()), ("ReadFailed", "the linear connection has expired"));
    let bridge = FakeBridge::with(Some(Vec::new()), &[], false);
    let err = reader(&bridge).accounts_for("slack").await.unwrap_err();
    assert_eq!(err, updates::ReadError::Off("slack is not connected".into()));
    let bridge = FakeBridge::with(None, &[], true);
    let err = reader(&bridge).accounts_for("slack").await.unwrap_err();
    assert_eq!(err.to_string(), "the connected accounts could not be listed (ConnectionError)");
    let err = reader(&bridge).call("SLACK_SEARCH_MESSAGES", &Obj::new(), "ca").await;
    assert!(err.is_ok()); // the listing is not needed for a call
    let failing = FakeBridge::with(None, &["SLACK_SEARCH_MESSAGES"], false);
    let err = reader(&failing).call("SLACK_SEARCH_MESSAGES", &Obj::new(), "ca").await.unwrap_err();
    assert_eq!((err.kind(), err.to_string().as_str()), ("ReadFailed", "SLACK_SEARCH_MESSAGES failed"));
}

// ---- cards: refs, links and the whole rebuild -----------------------------------------------------------------

#[tokio::test]
async fn test_clean_cards_accepts_every_composio_ref_and_keeps_the_link() {
    let (snap, _) = run_gather(&sources(Some(FakeBridge::new()), None, None), &[]).await;
    let refs: IndexMap<String, String> =
        snap_items(&snap).iter().map(|i| (s(&i["kind"]).to_string(), s(&i["id"]).to_string())).collect();
    let cards: Vec<Value> = refs
        .iter()
        .map(|(kind, r)| {
            json!({"verb": "Reply", "text": format!("to the {kind} item today"), "entity": null, "refs": [r], "due": null})
        })
        .collect();
    let cards = clean_cards(&json!({"cards": cards}), &snap);
    let kinds: BTreeSet<String> = cards.iter().map(|c| s(&c["kind"]).to_string()).collect();
    assert_eq!(kinds, composio_kinds());
    for c in &cards {
        assert_eq!(uc::safe_link(c.get("link"), Some(s(&c["kind"]))).as_deref(), Some(s(&c["link"])));
        let served = updates::served(c);
        assert_eq!(served["link"], c["link"]);
        assert_eq!(served["kind"], c["kind"]);
    }
}

#[tokio::test]
async fn test_a_card_citing_an_unknown_composio_ref_is_dropped() {
    let (snap, _) = run_gather(&sources(Some(FakeBridge::new()), None, None), &[]).await;
    let raw = json!({"cards": [{"verb": "Reply", "text": "to Sam", "entity": null, "refs": ["slack:D0000DM01/1.000001"],
                                "due": null}]});
    assert!(clean_cards(&raw, &snap).is_empty());
}

#[tokio::test]
async fn test_a_merged_card_keeps_the_link_of_its_first_ref_that_has_one() {
    let (mut snap, _) = run_gather(&sources(Some(FakeBridge::new()), None, None), &[]).await;
    snap["items"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id": "gmail:t1", "kind": "email", "title": "Budget", "body": "x"}));
    let slack_ref =
        snap_items(&snap).iter().find(|i| s(&i["kind"]) == "slack").map(|i| s(&i["id"]).to_string()).unwrap();
    let raw = json!({"cards": [{"verb": "Reply", "text": "to Sam about the budget", "entity": null,
                                "refs": ["gmail:t1", slack_ref], "due": null}]});
    let cards = clean_cards(&raw, &snap);
    assert_eq!(cards.len(), 1);
    let c = &cards[0];
    assert_eq!(s(&c["kind"]), "email");
    assert!(s(&updates::served(c)["link"]).contains("slack.com"));
}

#[test]
fn test_a_served_card_never_carries_a_link_off_its_apps_host() {
    let card = obj(json!({"id": "x", "verb": "Reply", "text": "to Sam", "kind": "slack", "source": "Slack",
                          "refs": ["slack:a/1.1"], "link": "https://example.test/phish"}));
    assert!(!updates::served(&card).contains_key("link"));
}

/// Answers with one card citing the snapshot's Linear item.
struct OneCardClient;

#[async_trait]
impl SuggestClient for OneCardClient {
    async fn complete(&self, _instructions: &str, body: &str) -> anyhow::Result<String> {
        let inner = body.split_once("<snapshot>\n").unwrap().1.rsplit_once("\n</snapshot>").unwrap().0;
        let snap: Value = serde_json::from_str(inner)?;
        let r = snap["items"].as_array().unwrap().iter().find(|i| s(&i["kind"]) == "linear").unwrap()["id"].clone();
        Ok(json!({"cards": [{"verb": "Fix", "text": "the login timeout, it is urgent", "entity": "EX-101",
                             "refs": [r], "due": null}]})
        .to_string())
    }
}

struct NoCards;

#[async_trait]
impl SuggestClient for NoCards {
    async fn complete(&self, _instructions: &str, _body: &str) -> anyhow::Result<String> {
        Ok(json!({"cards": []}).to_string())
    }
}

#[tokio::test]
async fn test_a_rebuild_keeps_a_lost_composio_sources_old_cards() {
    let tmp = tempfile::tempdir().unwrap();
    let bridge = FakeBridge::new();
    let clock = Arc::new(Mutex::new(now()));
    let wall = clock.clone();
    let now_fn: Now = Arc::new(move || *wall.lock());
    let u = Updates::new(UpdatesOptions {
        inlet: None,
        path: Some(tmp.path().join("u.json")),
        sources: Some(Arc::new(sources(Some(bridge.clone()), None, None))),
        client: Some(Arc::new(|| Some(Arc::new(OneCardClient) as Arc<dyn SuggestClient>))),
        now: Some(now_fn),
        environ: Some(Env::new()),
    });
    assert_eq!(u.rebuild(Arc::new(OneCardClient)).await, 1);
    let first = u.store.load();
    assert_eq!(first.sources.get("linear").map(String::as_str), Some("ok"));
    assert_eq!(s(&first.cards[0]["kind"]), "linear");
    // Linear's connection expires: the build loses it, keeps its card, and is not stamped as fresh
    let accounts = expired("linear", bridge.accounts.lock().clone());
    *bridge.accounts.lock() = accounts;
    let later = now() + Duration::hours(3);
    *clock.lock() = later;
    u.rebuild(Arc::new(NoCards)).await;
    let state = u.store.load();
    assert_eq!(u.last_status.lock().get("linear").map(String::as_str), Some("failed"));
    assert_eq!(state.cards.iter().map(|c| s(&c["kind"])).collect::<Vec<_>>(), ["linear"]);
    assert_eq!(state.generated_at, first.generated_at);
    // the loop tries again soon (Python: `u._next_try > 0`): the cache is 3 hours old, past the 2-hour interval, so
    // only the retry wait the lost build set keeps it from being due now
    assert!(!u.due(later, true));
}

#[test]
fn test_the_prompt_names_the_kinds_as_one_positive_set() {
    let line = updates::SYSTEM.lines().find(|l| l.contains("the kinds are exactly these")).unwrap();
    for kind in updates::KINDS {
        assert!(line.contains(kind), "{kind}");
    }
}

#[test]
fn test_updates_knows_every_composio_kind_and_ref_prefix() {
    assert!(uc::KINDS.iter().all(|k| updates::KINDS.contains(k)));
    for (kind, prefix) in uc::REF_PREFIX {
        assert_eq!(updates::REF_KINDS.iter().find(|(p, _)| *p == prefix).map(|(_, k)| *k), Some(kind));
        assert_eq!(ref_kind(&format!("{prefix}:x")), Some(kind));
    }
    let limits: BTreeSet<&str> = uc::LIMITS.iter().map(|(k, _)| *k).collect();
    let kinds: BTreeSet<&str> = uc::KINDS.into_iter().collect();
    let readers: BTreeSet<&str> = uc::READERS.into_iter().collect();
    assert!(limits == kinds && kinds == readers);
}
