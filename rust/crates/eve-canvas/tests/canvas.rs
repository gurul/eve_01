//! canvas.py's tests (bridge/tests/test_canvas.py): the owner's Canvas, read only, over a fake HTTP transport (no
//! network anywhere).
//!
//! Pins: the settings (off when unset, https only), each tool's parsing, Link-header pagination and its cap, due
//! times in the owner's zone, the dead-token line, a course that refuses without sinking the answer, and that no
//! request but a GET can leave (with a positive control: a POST is refused before the transport sees it).

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use eve_canvas::{
    configured, make_canvas, next_link, Canvas, CanvasApi, CanvasErrorKind, Environ, HttpRequest, HttpResponse,
    Transport, TransportError, Zone, MAX_DAYS, RATE_LINE, TOKEN_LINE, TOOLS, TOOL_NAMES,
};
use parking_lot::Mutex;
use serde_json::{json, Value};

const BASE: &str = "https://canvas.example.edu";
const TOKEN: &str = "test-canvas-token";

fn env() -> Environ {
    HashMap::from([("CANVAS_BASE_URL".into(), BASE.into()), ("CANVAS_API_TOKEN".into(), TOKEN.into())])
}

fn env_with(extra: &[(&str, &str)]) -> Environ {
    let mut e = env();
    for (k, v) in extra {
        e.insert(k.to_string(), v.to_string());
    }
    e
}

/// Wed Sep 30, 10:00 AM in Seattle
fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 30, 17, 0, 0).unwrap()
}

const PT: Zone = Zone::Named(chrono_tz::America::Los_Angeles);

fn courses() -> Value {
    json!([
        {"id": 101, "name": "Systems Programming", "course_code": "CSE 333 A", "workflow_state": "available",
         "term": {"name": "Autumn 2026", "end_at": "2026-12-20T08:00:00Z"}},
        {"id": 102, "name": "Natural Language Processing", "course_code": "CSE 447 A", "workflow_state": "available",
         "term": {"name": "Autumn 2026", "end_at": null}},
        {"id": 90, "name": "Old course", "course_code": "CSE 143", "term": {"name": "Spring 2025",
                                                                           "end_at": "2025-06-15T07:00:00Z"}},
        {"id": 91, "access_restricted_by_date": true},
    ])
}

fn course(i: usize) -> Value {
    courses()[i].clone()
}

type Handler = Arc<dyn Fn(&HttpRequest) -> Result<HttpResponse, TransportError> + Send + Sync>;

#[derive(Clone)]
enum Route {
    Json(Value),
    Response(HttpResponse),
    Handler(Handler),
}

/// Routes by path; records every request the transport saw.
#[derive(Default)]
struct Fake {
    routes: Mutex<HashMap<String, Route>>,
    seen: Mutex<Vec<HttpRequest>>,
}

impl Fake {
    fn new(routes: Vec<(&str, Route)>) -> Arc<Self> {
        Arc::new(Fake {
            routes: Mutex::new(routes.into_iter().map(|(k, v)| (k.to_string(), v)).collect()),
            seen: Mutex::new(Vec::new()),
        })
    }

    fn hub(self: &Arc<Self>) -> Canvas {
        self.hub_in(PT)
    }

    fn hub_in(self: &Arc<Self>, tz: Zone) -> Canvas {
        let transport: Arc<dyn Transport> = self.clone();
        Canvas::new(CanvasApi::new(&configured(Some(&env())), Some(transport)))
            .with_tz(tz)
            .with_now(Arc::new(now))
    }

    fn seen(&self) -> Vec<HttpRequest> {
        self.seen.lock().clone()
    }
}

#[async_trait]
impl Transport for Fake {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        self.seen.lock().push(request.clone());
        let path = url::Url::parse(&request.url).unwrap().path().to_string();
        let route = self.routes.lock().get(&path).cloned();
        match route {
            None => Ok(HttpResponse::json(404, &json!({"errors": [{"message": "The specified resource does not exist."}]}))),
            Some(Route::Json(v)) => Ok(HttpResponse::json(200, &v)),
            Some(Route::Response(r)) => Ok(r),
            Some(Route::Handler(h)) => h(&request),
        }
    }
}

fn routes(extra: Vec<(&'static str, Route)>) -> Vec<(&'static str, Route)> {
    let mut r = vec![("/api/v1/courses", Route::Json(courses()))];
    r.extend(extra);
    r
}

fn host(req: &HttpRequest) -> String {
    url::Url::parse(&req.url).unwrap().host_str().unwrap_or_default().to_string()
}

fn path(req: &HttpRequest) -> String {
    url::Url::parse(&req.url).unwrap().path().to_string()
}

/// `urllib.parse.parse_qs` of the request's query: every value of each key, in order.
fn qs(req: &HttpRequest) -> HashMap<String, Vec<String>> {
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    for (k, v) in url::Url::parse(&req.url).unwrap().query_pairs() {
        out.entry(k.into_owned()).or_default().push(v.into_owned());
    }
    out
}

fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

fn names(out: &Value, list: &str, key: &str) -> Vec<Value> {
    out[list].as_array().unwrap().iter().map(|i| i[key].clone()).collect()
}

// ---- settings ------------------------------------------------------------------------------------------------

#[test]
fn test_off_when_unset_or_half_set_or_not_https() {
    let empty = Environ::new();
    assert!(!configured(Some(&empty)).enabled);
    assert!(make_canvas(Some(&empty), None).is_none());
    assert!(!configured(Some(&HashMap::from([("CANVAS_BASE_URL".to_string(), BASE.to_string())]))).enabled);
    assert!(!configured(Some(&HashMap::from([("CANVAS_API_TOKEN".to_string(), TOKEN.to_string())]))).enabled);
    assert!(!configured(Some(&env_with(&[("CANVAS_BASE_URL", "http://canvas.example.edu")]))).enabled);
    assert!(!configured(Some(&env_with(&[("EVE_CANVAS", "0")]))).enabled);
    let cfg = configured(Some(&env_with(&[("CANVAS_BASE_URL", "https://Canvas.Example.edu/api/v1/")])));
    assert!(cfg.enabled && cfg.base_url == BASE);
    assert!(!format!("{cfg:?}").contains(TOKEN));
    assert_eq!(format!("{cfg:?}"), format!("CanvasConfig(enabled=True, base_url='{BASE}')"));
    let fake: Arc<dyn Transport> = Fake::new(vec![]);
    assert!(make_canvas(Some(&env()), Some(fake)).is_some());
}

// ---- courses, pagination -------------------------------------------------------------------------------------

#[tokio::test]
async fn test_courses_are_the_current_ones_and_the_token_rides_the_header() {
    let fake = Fake::new(routes(vec![]));
    let out = fake.hub().call("canvas_courses", &json!({})).await;
    assert_eq!(out["ok"], json!(true));
    assert_eq!(names(&out, "courses", "code"), vec![json!("CSE 333 A"), json!("CSE 447 A")]);
    assert_eq!(
        out["courses"][0],
        json!({"id": 101, "name": "Systems Programming", "code": "CSE 333 A", "term": "Autumn 2026"})
    );
    let req = &fake.seen()[0];
    assert_eq!(req.header("authorization"), Some(format!("Bearer {TOKEN}").as_str()));
    let q = qs(req);
    assert_eq!(q["enrollment_state"], strs(&["active"]));
    assert_eq!(q["include[]"], strs(&["term"]));
    assert_eq!(q["per_page"], strs(&["50"]));
    assert!(!req.url.contains(TOKEN));
}

fn paged(pages: Vec<Vec<Value>>, path: &'static str) -> Route {
    Route::Handler(Arc::new(move |request: &HttpRequest| {
        let n: usize = qs(request).get("page").map(|v| v[0].parse().unwrap()).unwrap_or(1);
        let mut resp = HttpResponse::json(200, &Value::Array(pages[n - 1].clone()));
        if n < pages.len() {
            resp = resp.with_header(
                "Link",
                &format!(
                    "<{BASE}{path}?page={n}&per_page=50>; rel=\"current\",<{BASE}{path}?page={}&per_page=50>; rel=\"next\",<{BASE}{path}?page=1&per_page=50>; rel=\"first\"",
                    n + 1
                ),
            );
        }
        Ok(resp)
    }))
}

#[tokio::test]
async fn test_pagination_follows_link_next_to_the_end() {
    let pages = vec![vec![course(0)], vec![course(1)], vec![course(2)]];
    let fake = Fake::new(vec![("/api/v1/courses", paged(pages, "/api/v1/courses"))]);
    let out = fake.hub().call("canvas_courses", &json!({})).await;
    assert_eq!(names(&out, "courses", "id"), vec![json!(101), json!(102)]);
    assert_eq!(fake.seen().len(), 3);
}

#[tokio::test]
async fn test_pagination_stops_at_the_cap() {
    let pages = vec![vec![course(0)]; 5];
    let fake = Fake::new(vec![("/api/v1/courses", paged(pages, "/api/v1/courses"))]);
    let mut hub = fake.hub();
    hub.api.set_max_pages(2);
    hub.call("canvas_courses", &json!({})).await;
    assert_eq!(fake.seen().len(), 2);
}

#[tokio::test]
async fn test_a_next_link_to_another_host_is_not_followed() {
    let handler: Handler = Arc::new(|_req: &HttpRequest| {
        Ok(HttpResponse::json(200, &json!([course(0)]))
            .with_header("Link", "<https://elsewhere.example.com/steal?page=2>; rel=\"next\""))
    });
    let fake = Fake::new(vec![("/api/v1/courses", Route::Handler(handler))]);
    let out = fake.hub().call("canvas_courses", &json!({})).await;
    assert_eq!(out["ok"], json!(false));
    assert!(out["reason"].as_str().unwrap().contains("did not follow"));
    assert_eq!(fake.seen().iter().map(host).collect::<Vec<_>>(), vec!["canvas.example.edu"]);
}

#[test]
fn test_next_link_parsing() {
    let header = r#"<https://a.example/x?page=1>; rel="current", <https://a.example/x?page=2>; rel="next""#;
    assert_eq!(next_link(header), "https://a.example/x?page=2");
    assert_eq!(next_link(r#"<https://a.example/x?page=1>; rel="first""#), "");
    assert_eq!(next_link(""), "");
}

// ---- what's due, in the owner's zone ------------------------------------------------------------------------

fn assign_333() -> Value {
    json!([
        {"id": 1, "name": "HW2", "due_at": "2026-10-03T06:59:00Z", "points_possible": 50,
         "html_url": format!("{BASE}/courses/101/assignments/1"), "submission": {"workflow_state": "unsubmitted"}},
        {"id": 2, "name": "Ex 5", "due_at": "2026-10-01T17:30:00Z", "points_possible": 3,
         "html_url": format!("{BASE}/courses/101/assignments/2"),
         "submission": {"workflow_state": "submitted", "submitted_at": "2026-09-29T20:00:00Z"}},
        {"id": 3, "name": "Ex 4 (late)", "due_at": "2026-09-28T17:30:00Z",
         "html_url": format!("{BASE}/courses/101/assignments/3"),
         "submission": {"workflow_state": "unsubmitted", "missing": true}},
        {"id": 4, "name": "Ex 3 done", "due_at": "2026-09-26T17:30:00Z",
         "html_url": format!("{BASE}/courses/101/assignments/4"),
         "submission": {"workflow_state": "graded", "submitted_at": "2026-09-25T20:00:00Z", "grade": "3"}},
        {"id": 5, "name": "Final project", "due_at": "2026-11-20T07:59:00Z",
         "html_url": format!("{BASE}/courses/101/assignments/5")},
        {"id": 6, "name": "Participation", "due_at": null, "html_url": format!("{BASE}/courses/101/assignments/6")},
    ])
}

fn first(v: &Value, n: usize) -> Value {
    Value::Array(v.as_array().unwrap()[..n].to_vec())
}

#[tokio::test]
async fn test_due_lists_the_window_in_local_time_with_submission_status() {
    let fake = Fake::new(routes(vec![
        ("/api/v1/courses/101/assignments", Route::Json(assign_333())),
        (
            "/api/v1/courses/102/assignments",
            Route::Response(HttpResponse::json(403, &json!({"errors": [{"message": "forbidden"}]}))),
        ),
    ]));
    let out = fake.hub().call("canvas_due", &json!({"days": null, "course": null})).await;
    assert_eq!(out["ok"], json!(true));
    // by due time; done-and-past, far and undated left out
    assert_eq!(names(&out, "items", "name"), vec![json!("Ex 4 (late)"), json!("Ex 5"), json!("HW2")]);
    let items = out["items"].as_array().unwrap();
    assert_eq!(
        items[2],
        json!({"course": "CSE 333 A", "name": "HW2", "due": "Fri Oct 2, 11:59 PM PDT",
               "due_at": "2026-10-03T06:59:00+00:00", "status": "not submitted", "points": 50,
               "url": format!("{BASE}/courses/101/assignments/1")})
    );
    assert_eq!(items[0]["status"], json!("missing"));
    assert_eq!(items[1]["status"], json!("submitted"));
    assert_eq!(out["could_not_read"], json!(["CSE 447 A"])); // one course refusing does not sink the answer
    let q = qs(&fake.seen()[1]);
    assert_eq!(q["include[]"], strs(&["submission"]));
    assert_eq!(q["order_by"], strs(&["due_at"]));
    assert_eq!(out["now"], json!("Wed Sep 30, 10:00 AM PDT"));
}

#[tokio::test]
async fn test_due_times_follow_the_zone() {
    let fake = Fake::new(routes(vec![
        ("/api/v1/courses/101/assignments", Route::Json(first(&assign_333(), 1))),
        ("/api/v1/courses/102/assignments", Route::Json(json!([]))),
    ]));
    let out = fake
        .hub_in(Zone::Named(chrono_tz::America::New_York))
        .call("canvas_due", &json!({"days": 7, "course": null}))
        .await;
    assert_eq!(out["items"][0]["due"], json!("Sat Oct 3, 2:59 AM EDT"));
}

#[tokio::test]
async fn test_due_for_one_course_and_a_longer_window() {
    let fake = Fake::new(routes(vec![("/api/v1/courses/101/assignments", Route::Json(assign_333()))]));
    let out = fake.hub().call("canvas_due", &json!({"days": 90, "course": "cse333"})).await;
    assert_eq!(out["days"], json!(MAX_DAYS));
    assert!(names(&out, "items", "name").contains(&json!("Final project")));
    let mut paths: Vec<String> = fake.seen().iter().map(path).collect();
    paths.sort();
    paths.dedup();
    assert_eq!(paths, strs(&["/api/v1/courses", "/api/v1/courses/101/assignments"]));
    let out = fake.hub().call("canvas_due", &json!({"days": 7, "course": "chem 142"})).await;
    assert_eq!(out["ok"], json!(false));
    assert_eq!(out["reason"], json!("no current course matches 'chem 142'"));
}

// ---- errors ----------------------------------------------------------------------------------------------------

#[tokio::test]
async fn test_a_refused_token_says_how_to_make_a_new_one() {
    let fake = Fake::new(vec![(
        "/api/v1/courses",
        Route::Response(
            HttpResponse::json(401, &json!({"errors": [{"message": "Invalid access token."}]}))
                .with_header("WWW-Authenticate", "Bearer realm=\"canvas-lms\""),
        ),
    )]);
    for (name, args) in [("canvas_courses", json!({})), ("canvas_due", json!({"days": 7, "course": null}))] {
        let out = fake.hub().call(name, &args).await;
        assert_eq!(out["ok"], json!(false));
        assert_eq!(out["line"], json!(TOKEN_LINE));
        let line = out["line"].as_str().unwrap();
        assert!(line.contains("New Access Token") && line.contains("CANVAS_API_TOKEN"));
    }
}

#[tokio::test]
async fn test_canvas_permission_401_is_a_course_refusal_not_a_dead_token() {
    let fake = Fake::new(routes(vec![
        ("/api/v1/courses/101/assignments", Route::Json(first(&assign_333(), 1))),
        (
            "/api/v1/courses/102/assignments",
            Route::Response(HttpResponse::json(
                401,
                &json!({"status": "unauthorized",
                        "errors": [{"message": "user not authorized to perform that action"}]}),
            )),
        ),
    ]));
    let out = fake.hub().call("canvas_due", &json!({"days": 7, "course": null})).await;
    assert_eq!(out["ok"], json!(true));
    assert_eq!(out["could_not_read"], json!(["CSE 447 A"]));
}

#[tokio::test]
async fn test_throttle_and_timeout_are_plain_lines() {
    let fake = Fake::new(vec![(
        "/api/v1/courses",
        Route::Response(HttpResponse::text(403, "403 Forbidden (Rate Limit Exceeded)")),
    )]);
    assert_eq!(fake.hub().call("canvas_courses", &json!({})).await["line"], json!(RATE_LINE));

    let slow: Handler = Arc::new(|_req: &HttpRequest| Err(TransportError::Timeout));
    let out = Fake::new(vec![("/api/v1/courses", Route::Handler(slow))]).hub().call("canvas_courses", &json!({})).await;
    assert_eq!(out["ok"], json!(false));
    assert!(out["reason"].as_str().unwrap().contains("in time"));
}

// ---- announcements and links -------------------------------------------------------------------------------------

#[tokio::test]
async fn test_announcements_are_text_newest_first_with_links() {
    let rows = json!([
        {"id": 7, "title": "Room change", "posted_at": "2026-09-28T16:00:00Z", "context_code": "course_102",
         "message": "<p>Lecture moves to <b>CSE2 G10</b> &amp; stays there.</p>",
         "html_url": format!("{BASE}/courses/102/discussion_topics/7")},
        {"id": 8, "title": "HW2 out", "posted_at": "2026-09-29T16:00:00Z", "context_code": "course_101",
         "message": "<div>See the spec.</div>", "html_url": format!("{BASE}/courses/101/discussion_topics/8")},
    ]);
    let fake = Fake::new(routes(vec![("/api/v1/announcements", Route::Json(rows))]));
    let out = fake.hub().call("canvas_announcements", &json!({"course": null, "days": null})).await;
    assert_eq!(names(&out, "items", "title"), vec![json!("HW2 out"), json!("Room change")]);
    assert_eq!(out["items"][1]["text"], json!("Lecture moves to CSE2 G10 & stays there."));
    assert_eq!(out["items"][1]["course"], json!("CSE 447 A"));
    assert_eq!(out["items"][0]["posted"], json!("Tue Sep 29, 9:00 AM PDT"));
    let q = qs(fake.seen().last().unwrap());
    assert_eq!(q["context_codes[]"], strs(&["course_101", "course_102"]));
    assert_eq!(q["start_date"], strs(&["2026-09-16"]));
    fake.hub().call("canvas_announcements", &json!({"course": "447", "days": 3})).await;
    assert_eq!(qs(fake.seen().last().unwrap())["context_codes[]"], strs(&["course_102"]));
}

#[tokio::test]
async fn test_find_link_across_kinds() {
    let fake = Fake::new(routes(vec![
        (
            "/api/v1/courses/102/assignments",
            Route::Json(json!([{"id": 1, "name": "A3 n-grams", "html_url": format!("{BASE}/courses/102/assignments/1")}])),
        ),
        ("/api/v1/courses/102/pages", Route::Json(json!([{"page_id": 5, "url": "a3-guide", "title": "A3 guide"}]))),
        (
            "/api/v1/courses/102/files",
            Route::Json(json!([{"id": 77, "display_name": "a3-starter.zip",
                                "url": format!("{BASE}/files/77/download?download_frd=1&verifier=placeholder")}])),
        ),
        (
            "/api/v1/courses/102/modules",
            Route::Json(json!([{"id": 9, "name": "Week 3: A3", "items": [
                {"title": "Week 3", "type": "SubHeader"},
                {"title": "Slides", "type": "File", "html_url": format!("{BASE}/courses/102/modules/items/1")}]}])),
        ),
    ]));
    let out = fake.hub().call("canvas_find_link", &json!({"course": "CSE 447 A", "query": "a3", "kind": "any"})).await;
    assert_eq!(out["ok"], json!(true));
    assert_eq!(out["course"], json!("CSE 447 A"));
    let by_kind: HashMap<String, Value> =
        out["links"].as_array().unwrap().iter().map(|l| (l["kind"].as_str().unwrap().to_string(), l.clone())).collect();
    assert_eq!(by_kind["assignment"]["url"], json!(format!("{BASE}/courses/102/assignments/1")));
    assert_eq!(by_kind["page"]["url"], json!(format!("{BASE}/courses/102/pages/a3-guide")));
    assert_eq!(by_kind["file"]["url"], json!(format!("{BASE}/courses/102/files/77"))); // the file's page, never its download url
    assert!(!out.to_string().contains("verifier"));
    assert_eq!(
        by_kind["module"]["items"],
        json!([{"title": "Slides", "type": "File", "url": format!("{BASE}/courses/102/modules/items/1")}])
    );
    let searched: Vec<Option<Vec<String>>> = fake.seen()[1..].iter().map(|r| qs(r).get("search_term").cloned()).collect();
    assert_eq!(searched, vec![Some(strs(&["a3"])); 4]);
    // one kind only
    fake.seen.lock().clear();
    let out = fake.hub().call("canvas_find_link", &json!({"course": "447", "query": "a3", "kind": "page"})).await;
    assert_eq!(names(&out, "links", "kind"), vec![json!("page")]);
    assert_eq!(fake.seen().iter().map(path).collect::<Vec<_>>(), strs(&["/api/v1/courses", "/api/v1/courses/102/pages"]));
    // files hidden from students: the rest still answers
    fake.routes.lock().insert(
        "/api/v1/courses/102/files".into(),
        Route::Response(HttpResponse::json(403, &json!({"errors": [{"message": "forbidden"}]}))),
    );
    let out = fake.hub().call("canvas_find_link", &json!({"course": "447", "query": "a3", "kind": "any"})).await;
    assert_eq!(out["ok"], json!(true));
    assert_eq!(out["could_not_read"], json!(["files"]));
    // an unclear course names the choices
    let out = fake.hub().call("canvas_find_link", &json!({"course": "CSE", "query": "a3", "kind": "any"})).await;
    assert_eq!(out["ok"], json!(false));
    assert_eq!(out["courses"], json!(["CSE 333 A", "CSE 447 A"]));
}

// ---- read only ---------------------------------------------------------------------------------------------------

#[tokio::test]
async fn test_only_gets_leave_and_a_post_is_refused_before_the_transport() {
    let fake = Fake::new(routes(vec![
        ("/api/v1/courses/101/assignments", Route::Json(assign_333())),
        ("/api/v1/courses/102/assignments", Route::Json(json!([]))),
        ("/api/v1/announcements", Route::Json(json!([]))),
    ]));
    let hub = fake.hub();
    for (name, args) in [
        ("canvas_courses", json!({})),
        ("canvas_due", json!({"days": 7, "course": null})),
        ("canvas_announcements", json!({"course": null, "days": 14})),
        ("canvas_find_link", json!({"course": "333", "query": "hw", "kind": "any"})),
    ] {
        hub.call(name, &args).await;
    }
    let seen = fake.seen();
    assert!(!seen.is_empty() && seen.iter().all(|r| r.method == "GET"));
    // positive control: the guard catches a write, and the fake never sees it
    let before = seen.len();
    for (method, url) in [
        ("POST", format!("{BASE}/api/v1/courses/101/assignments/1/submissions")),
        ("PUT", format!("{BASE}/api/v1/courses/101")),
        ("DELETE", format!("{BASE}/api/v1/courses/101")),
        ("PATCH", format!("{BASE}/api/v1/courses/101")),
    ] {
        match hub.api.client().send(hub.api.request(method, &url)).await {
            Err(TransportError::WriteRefused(e)) => assert_eq!(e.kind, CanvasErrorKind::WriteRefused),
            other => panic!("{method} was not refused: {other:?}"),
        }
    }
    assert_eq!(fake.seen().len(), before);
}

#[tokio::test]
async fn test_rundown_part_is_short_and_only_open_work() {
    let fake = Fake::new(routes(vec![
        ("/api/v1/courses/101/assignments", Route::Json(assign_333())),
        ("/api/v1/courses/102/assignments", Route::Json(json!([]))),
    ]));
    let part = fake.hub().rundown_default().await;
    assert_eq!(part["available"], json!(true));
    assert_eq!(names(&part, "items", "name"), vec![json!("Ex 4 (late)"), json!("HW2")]);
    let keys: Vec<&String> = part["items"][0].as_object().unwrap().keys().collect();
    assert_eq!(keys, vec!["course", "name", "due", "status", "url"]);
    let bad = Fake::new(vec![("/api/v1/courses", Route::Response(HttpResponse::json(401, &json!({}))))])
        .hub()
        .rundown_default()
        .await;
    assert_eq!(bad, json!({"available": false, "reason": TOKEN_LINE}));
}

// ---- the tool surface the Telegram brain reads ------------------------------------------------------------------

#[tokio::test]
async fn tools_and_instructions_name_the_four_tools() {
    let hub = Fake::new(routes(vec![])).hub();
    let names: Vec<&str> = TOOLS.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, TOOL_NAMES.to_vec());
    assert_eq!(hub.tools(), *TOOLS);
    assert!(hub.instructions().contains("canvas_due"));
    assert_eq!(
        TOOLS[1]["parameters"]["properties"]["days"]["description"],
        json!("How many days ahead to look (1-60); null for 7.")
    );
    assert_eq!(TOOLS[3]["parameters"]["properties"]["kind"]["enum"], json!(["any", "assignment", "page", "file", "module"]));
    assert_eq!(hub.handle("canvas_nope", &Value::Null).await, json!({"ok": false, "reason": "unknown tool canvas_nope"}));
    let out = hub.handle("canvas_courses", &Value::Null).await;
    assert_eq!(out["ok"], json!(true));
}

#[tokio::test]
async fn a_closed_client_fails_as_python_does() {
    let hub = make_canvas(Some(&env()), None).unwrap();
    hub.close().await;
    let out = hub.call("canvas_courses", &json!({})).await;
    assert_eq!(out, json!({"ok": false, "reason": "Canvas read failed (RuntimeError)"}));
}
