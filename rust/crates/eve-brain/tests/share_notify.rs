//! Unit `share_notify`: share.py 1-798, notify.py 1-248; tests: test_share.py, test_notify.py.
//!
//! The shared fakes are in `support` (architect-owned); this unit's own go here.
//!
//! test_share.py: nothing here sends anything: osascript is a fake runner and Drive is a fake transport. Phone
//! numbers are from the 555-01xx fiction range and addresses use example.com.

mod support;

use std::io::Write as _;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, UNIX_EPOCH};

use async_trait::async_trait;
use base64::Engine as _;
use futures::FutureExt;
use parking_lot::Mutex;
use serde_json::{json, Map, Value};

use eve_brain::notify::{self, Notifier, PostError, PostFn};
use eve_brain::prelude::{HttpError, HttpRequest, HttpResponse, HttpTransport};
use eve_brain::share::{self as S, Ask, Runner, Share, ShareConfig, TokenFn};
use eve_brain::telegram::STOP_BUTTON;

type Obj = Map<String, Value>;
type Calls = Arc<Mutex<Vec<(Vec<String>, Vec<u8>)>>>;
type Posts = Arc<Mutex<Vec<(String, String, Vec<(String, String)>)>>>;

fn obj(v: Value) -> Obj {
    v.as_object().cloned().expect("an object")
}

// ================================================================================================================
// test_share.py
// ================================================================================================================

const ON: ShareConfig = ShareConfig { enabled: true, imessage: true };

fn ana() -> Value {
    json!({"name": "Ana Lee", "phones": [{"label": "_$!<Mobile>!$_", "value": "+1 (555) 010-0199"}],
           "emails": [{"label": "_$!<Home>!$_", "value": "ana@example.com"}]})
}

fn ana2() -> Value {
    json!({"name": "Ana Lopez", "phones": [{"label": "_$!<Mobile>!$_", "value": "+1 555 010-0123"}], "emails": []})
}

/// FakeRunner: Contacts answers with `people`; a send answers `send_rc`, `send_err`.
#[derive(Clone)]
struct FakeRunner {
    people: Vec<Value>,
    send_rc: i64,
    send_err: String,
    calls: Calls,
}

impl FakeRunner {
    fn new(people: Vec<Value>) -> Self {
        Self { people, send_rc: 0, send_err: String::new(), calls: Arc::new(Mutex::new(Vec::new())) }
    }

    fn failing(people: Vec<Value>, rc: i64, err: &str) -> Self {
        Self { send_rc: rc, send_err: err.into(), ..Self::new(people) }
    }

    fn runner(&self) -> Runner {
        let me = self.clone();
        Arc::new(move |argv: Vec<String>, stdin: Vec<u8>, _timeout: f64| {
            let me = me.clone();
            async move {
                me.calls.lock().push((argv.clone(), stdin));
                if argv.iter().any(|a| a == "JavaScript") {
                    let body = json!({"count": me.people.len(), "people": me.people});
                    return (0, format!("{body}\n"), String::new());
                }
                (me.send_rc, String::new(), me.send_err.clone())
            }
            .boxed()
        })
    }

    fn calls(&self) -> Vec<(Vec<String>, Vec<u8>)> {
        self.calls.lock().clone()
    }

    fn sends(&self) -> Vec<(Vec<String>, Vec<u8>)> {
        self.calls().into_iter().filter(|(argv, _)| !argv.iter().any(|a| a == "JavaScript")).collect()
    }

    fn lookups(&self) -> usize {
        self.calls().iter().filter(|(argv, _)| argv.iter().any(|a| a == "JavaScript")).count()
    }
}

/// The `home` fixture: ~/Documents/report.pdf, about 2 KB.
struct Home {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
}

fn home() -> Home {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(home.join("Documents")).unwrap();
    let mut bytes = b"%PDF-1.4 ".to_vec();
    bytes.extend(std::iter::repeat_n(b'x', 2000));
    std::fs::write(home.join("Documents").join("report.pdf"), bytes).unwrap();
    Home { _tmp: tmp, root, home }
}

fn token(t: &str) -> TokenFn {
    let t = t.to_string();
    Arc::new(move || {
        let t = t.clone();
        async move { Some(t) }.boxed()
    })
}

fn make(h: &Home, runner: &FakeRunner) -> Share {
    Share::new(ON).with_runner(runner.runner()).with_home(&h.home).with_outbox(h.home.join("Pictures").join("eve-outbox"))
}

fn share_args(to: &str, via: &str, path: &str, message: Option<&str>) -> Obj {
    obj(json!({"path": path, "to": to, "via": via, "message": message}))
}

fn args() -> Obj {
    share_args("Ana Lee", "imessage", "~/Documents/report.pdf", Some("here it is"))
}

fn s(out: &Obj, key: &str) -> String {
    out.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}

fn b(out: &Obj, key: &str) -> bool {
    out.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn same_stamp(f: &Path, like: &std::fs::Metadata) {
    let at = |secs: i64, nsec: i64| UNIX_EPOCH + Duration::new(secs as u64, nsec as u32);
    let times = std::fs::FileTimes::new()
        .set_accessed(at(like.atime(), like.atime_nsec()))
        .set_modified(at(like.mtime(), like.mtime_nsec()));
    std::fs::OpenOptions::new().write(true).open(f).unwrap().set_times(times).unwrap();
}

// ---- contacts lookup ----------------------------------------------------------------------------------------

#[tokio::test]
async fn test_contacts_lookup_passes_the_name_only_through_argv() {
    let h = home();
    let r = FakeRunner::new(vec![ana()]);
    let sh = make(&h, &r);
    let name = "Ana\" & do shell script \"rm -rf ~";
    let out = sh.handle("find_contact", &obj(json!({"name": name}))).await;
    assert!(b(&out, "ok"));
    assert_eq!(out["people"][0]["name"], "Ana Lee");
    let (argv, script) = r.calls()[0].clone();
    assert_eq!(argv[..4], ["osascript", "-l", "JavaScript", "-"]);
    assert_eq!(argv[4], name);
    let script = String::from_utf8(script).unwrap();
    assert!(!script.contains(name) && script.contains("function run(argv)"));
    // Contacts' built-in labels are cleaned
    assert_eq!(out["people"][0]["phones"][0]["label"], "mobile");
}

#[tokio::test]
async fn test_ambiguous_contact_returns_both_and_asks_nothing() {
    let h = home();
    let r = FakeRunner::new(vec![ana(), ana2()]);
    let sh = make(&h, &r);
    let out = sh.handle("share_file", &share_args("Ana", "imessage", "~/Documents/report.pdf", Some("here it is"))).await;
    assert_eq!(out["ok"], false);
    assert_eq!(out["ambiguous"], true);
    let names: Vec<&str> = out["matches"].as_array().unwrap().iter().map(|p| p["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["Ana Lee", "Ana Lopez"]);
    assert!(!out.contains_key("needs_approval") && sh.pending_count() == 0 && r.sends().is_empty());
}

#[tokio::test]
async fn test_one_person_with_several_handles_for_email_is_ambiguous() {
    let h = home();
    let mut two = ana();
    two["emails"] = json!([{"label": "home", "value": "ana@example.com"}, {"label": "work", "value": "ana.lee@example.org"}]);
    let r = FakeRunner::new(vec![two]);
    let sh = make(&h, &r).with_token(Some(token("tok")));
    let out = sh.handle("share_file", &share_args("Ana Lee", "email", "~/Documents/report.pdf", Some("here it is"))).await;
    assert_eq!(out["ambiguous"], true);
    assert_eq!(sh.pending_count(), 0);
}

#[tokio::test]
async fn test_no_match() {
    let h = home();
    let out = make(&h, &FakeRunner::new(vec![]))
        .handle("share_file", &share_args("Nobody", "imessage", "~/Documents/report.pdf", Some("here it is")))
        .await;
    assert_eq!(out["ok"], false);
    assert!(s(&out, "reason").contains("Contacts"));
}

#[tokio::test]
async fn test_raw_phone_or_email_skips_the_lookup() {
    for (to, handle) in [("+1 (555) 010-0199", "+15550100199"), ("ana@example.com", "ana@example.com")] {
        let h = home();
        let r = FakeRunner::new(vec![ana()]);
        let sh = make(&h, &r);
        let out = sh.handle("share_file", &share_args(to, "imessage", "~/Documents/report.pdf", Some("here it is"))).await;
        assert_eq!(out["needs_approval"], true, "{to}");
        assert_eq!(r.lookups(), 0);
        assert_eq!(sh.pending(&s(&out, "token")).unwrap().handle, handle);
    }
}

#[tokio::test]
async fn test_phone_is_refused_for_email() {
    let h = home();
    let out = make(&h, &FakeRunner::new(vec![ana()]))
        .with_token(Some(token("t")))
        .handle("share_file", &share_args("+15550100199", "email", "~/Documents/report.pdf", Some("here it is")))
        .await;
    assert_eq!(out["ok"], false);
    assert!(s(&out, "reason").contains("email address"));
}

// ---- approval gate + Messages send --------------------------------------------------------------------------

#[tokio::test]
async fn test_share_asks_first_and_sends_nothing() {
    let h = home();
    let r = FakeRunner::new(vec![ana()]);
    let sh = make(&h, &r);
    let out = sh.handle("share_file", &args()).await;
    assert_eq!(out["needs_approval"], true);
    assert!(!s(&out, "token").is_empty());
    assert!(s(&out, "question").starts_with("Send report.pdf (2 KB) to Ana Lee (+1 (555) 010-0199) by iMessage?"));
    assert!(s(&out, "question").contains("Note: \"here it is\""));
    assert!(r.sends().is_empty());
}

#[tokio::test]
async fn test_confirm_sends_with_values_only_in_argv() {
    let h = home();
    let r = FakeRunner::new(vec![ana()]);
    let sh = make(&h, &r);
    let out = sh.handle("share_file", &share_args("Ana Lee", "imessage", "~/Documents/report.pdf", Some("hi\" & quit"))).await;
    let done = sh.confirm(&s(&out, "token")).await;
    assert_eq!(done["ok"], true);
    assert_eq!(done["via"], "imessage");
    let sends = r.sends();
    assert_eq!(sends.len(), 1);
    let (argv, script) = sends[0].clone();
    assert_eq!(argv[0..2], ["osascript", "-"]);
    let (handle, staged, note) = (&argv[2], &argv[3], &argv[4]);
    assert_eq!(argv.len(), 5);
    assert!(handle == "+15550100199" && note == "hi\" & quit");
    let staged_path = PathBuf::from(staged);
    assert_eq!(staged_path.parent().unwrap(), h.home.join("Pictures").join("eve-outbox"));
    assert!(staged_path.file_name().unwrap().to_string_lossy().ends_with("report.pdf"));
    assert_eq!(std::fs::read(&staged_path).unwrap(), std::fs::read(h.home.join("Documents/report.pdf")).unwrap());
    let script = String::from_utf8(script).unwrap();
    for value in [handle, staged, note] {
        assert!(!script.contains(value.as_str()));
    }
    assert!(script.contains("on run argv") && script.contains("service type = iMessage") && script.contains("POSIX file"));
}

#[tokio::test]
async fn test_token_is_single_use_and_unknown_tokens_run_nothing() {
    let h = home();
    let r = FakeRunner::new(vec![ana()]);
    let sh = make(&h, &r);
    let out = sh.handle("share_file", &args()).await;
    assert_eq!(sh.confirm(&s(&out, "token")).await["ok"], true);
    let again = sh.confirm(&s(&out, "token")).await;
    assert_eq!(again["ok"], false);
    assert_eq!(sh.confirm("made-up").await["ok"], false);
    assert_eq!(r.sends().len(), 1);
}

#[tokio::test]
async fn test_expired_token_runs_nothing() {
    let h = home();
    let now = Arc::new(Mutex::new(1000.0));
    let r = FakeRunner::new(vec![ana()]);
    let clock = now.clone();
    let sh = make(&h, &r).with_clock(Arc::new(move || *clock.lock()));
    let out = sh.handle("share_file", &args()).await;
    *now.lock() += S::APPROVAL_TTL_SECS + 1.0;
    assert_eq!(sh.confirm(&s(&out, "token")).await["ok"], false);
    assert!(r.sends().is_empty());
}

#[tokio::test]
async fn test_stale_file_is_refused() {
    let h = home();
    let r = FakeRunner::new(vec![ana()]);
    let sh = make(&h, &r);
    let out = sh.handle("share_file", &args()).await;
    let f = h.home.join("Documents/report.pdf");
    std::fs::write(&f, b"changed").unwrap();
    let epoch = UNIX_EPOCH + Duration::new(0, 1);
    std::fs::OpenOptions::new()
        .write(true)
        .open(&f)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_accessed(epoch).set_modified(epoch))
        .unwrap();
    let done = sh.confirm(&s(&out, "token")).await;
    assert_eq!(done["ok"], false);
    assert!(s(&done, "reason").contains("changed") && r.sends().is_empty());
}

/// Review, 2026-10-05: confirm checked size and mtime through stat(), which follows a link, so a report.pdf
/// swapped for a link to ~/.ssh's key of the same size and mtime sent the key. The hidden-path rule ran only
/// when the question was asked.
#[tokio::test]
async fn test_a_file_swapped_for_a_symlink_after_the_question_is_not_sent() {
    let h = home();
    let r = FakeRunner::new(vec![ana()]);
    let sh = make(&h, &r);
    let f = h.home.join("Documents/report.pdf");
    let out = sh.handle("share_file", &args()).await;
    let before = std::fs::metadata(&f).unwrap();
    std::fs::create_dir(h.home.join(".ssh")).unwrap();
    let key = h.home.join(".ssh/id_ed25519");
    let mut bytes = b"-----BEGIN OPENSSH PRIVATE KEY-----".to_vec();
    bytes.resize(before.len() as usize, b'A');
    std::fs::write(&key, bytes).unwrap();
    same_stamp(&key, &before);
    std::fs::remove_file(&f).unwrap();
    std::os::unix::fs::symlink(&key, &f).unwrap();
    let through = std::fs::metadata(&f).unwrap();
    assert!(through.len() == before.len() && through.mtime_nsec() == before.mtime_nsec() && through.mtime() == before.mtime());
    let done = sh.confirm(&s(&out, "token")).await;
    assert_eq!(done["ok"], false);
    assert!(r.sends().is_empty());
    let outbox = h.home.join("Pictures/eve-outbox");
    if outbox.exists() {
        for p in std::fs::read_dir(&outbox).unwrap().flatten() {
            let data = std::fs::read(p.path()).unwrap();
            assert!(!data.windows(11).any(|w| w == b"PRIVATE KEY"));
        }
    }
}

#[tokio::test]
async fn test_a_file_whose_bytes_changed_with_the_same_size_and_mtime_is_not_sent() {
    let h = home();
    let r = FakeRunner::new(vec![ana()]);
    let sh = make(&h, &r);
    let f = h.home.join("Documents/report.pdf");
    let out = sh.handle("share_file", &args()).await;
    let before = std::fs::metadata(&f).unwrap();
    {
        // same inode, same size, new bytes
        let mut fh = std::fs::OpenOptions::new().write(true).open(&f).unwrap();
        fh.write_all(b"SECRET!!").unwrap();
    }
    same_stamp(&f, &before);
    let done = sh.confirm(&s(&out, "token")).await;
    assert_eq!(done["ok"], false);
    assert!(s(&done, "reason").contains("changed") && r.sends().is_empty());
}

#[tokio::test]
async fn test_a_file_replaced_by_another_file_of_the_same_size_and_mtime_is_not_sent_by_email() {
    let h = home();
    let drive = FakeDrive::new(true);
    let sh = make(&h, &FakeRunner::new(vec![ana()])).with_token(Some(token("tok"))).with_http(drive.clone());
    let f = h.home.join("Documents/report.pdf");
    let out = sh.handle("share_file", &share_args("ana@example.com", "email", "~/Documents/report.pdf", Some("here it is"))).await;
    let before = std::fs::metadata(&f).unwrap();
    let other = h.home.join("Documents/other.pdf");
    std::fs::write(&other, vec![b'x'; before.len() as usize]).unwrap();
    same_stamp(&other, &before);
    std::fs::rename(&other, &f).unwrap(); // a new inode at the same path
    let done = sh.confirm(&s(&out, "token")).await;
    assert_eq!(done["ok"], false);
    assert!(drive.requests().is_empty());
}

fn ask(answer: &'static str, asked: Arc<Mutex<Vec<String>>>) -> Ask {
    Arc::new(move |q: String| {
        asked.lock().push(q);
        async move { answer.to_string() }.boxed()
    })
}

#[tokio::test]
async fn test_ask_and_share_denied_sends_nothing() {
    let h = home();
    let r = FakeRunner::new(vec![ana()]);
    let sh = make(&h, &r);
    let asked = Arc::new(Mutex::new(Vec::new()));
    let out = sh.ask_and_share(&args(), ask("no", asked.clone())).await;
    assert_eq!(out["ok"], false);
    assert!(!asked.lock().is_empty() && r.sends().is_empty() && sh.pending_count() == 0);
}

#[tokio::test]
async fn test_ask_and_share_allowed_sends_once() {
    let h = home();
    let r = FakeRunner::new(vec![ana()]);
    let sh = make(&h, &r);
    let out = sh.ask_and_share(&args(), ask("yes", Arc::new(Mutex::new(Vec::new())))).await;
    assert_eq!(out["ok"], true);
    assert!(!out.contains_key("token") && r.sends().len() == 1);
}

#[tokio::test]
async fn test_ask_and_share_cancelled_while_asking_forgets_the_token() {
    // Python: `except BaseException: self.cancel(token); raise` around the ask
    let h = home();
    let r = FakeRunner::new(vec![ana()]);
    let sh = make(&h, &r);
    let never: Ask = Arc::new(|_q: String| futures::future::pending::<String>().boxed());
    let a = args();
    let asking = sh.ask_and_share(&a, never);
    assert!(tokio::time::timeout(Duration::from_millis(50), asking).await.is_err());
    assert_eq!(sh.pending_count(), 0);
    assert!(r.sends().is_empty());
}

#[tokio::test]
async fn test_messages_not_authorized_is_explained() {
    let h = home();
    let sh = make(&h, &FakeRunner::failing(vec![ana()], 1, "execution error: Not authorized (-1743)"));
    let out = sh.handle("share_file", &args()).await;
    let done = sh.confirm(&s(&out, "token")).await;
    assert_eq!(done["ok"], false);
    assert!(s(&done, "reason").contains("Automation"));
}

// ---- path rules ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn test_path_rules() {
    let h = home();
    std::fs::create_dir(h.home.join(".ssh")).unwrap();
    std::fs::write(h.home.join(".ssh/id"), "k").unwrap();
    let outside = h.root.join("outside.txt");
    std::fs::write(&outside, "x").unwrap();
    let r = FakeRunner::new(vec![ana()]);
    let sh = make(&h, &r);
    let outside = outside.to_string_lossy().into_owned();
    for (path, words) in [
        (outside.as_str(), "home"),
        ("~/.ssh/id", "hidden"),
        ("~/Documents", "folder"),
        ("~/Documents/missing.pdf", "no such file"),
    ] {
        let out = sh.handle("share_file", &share_args("Ana Lee", "imessage", path, Some("here it is"))).await;
        assert!(out["ok"] == false && s(&out, "reason").contains(words), "{path}: {out:?}");
    }
    assert!(r.calls().is_empty() && sh.pending_count() == 0);
}

#[test]
fn test_off_lends_nothing() {
    let env = |pairs: &[(&str, &str)]| pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    assert!(S::make_share(Some(&env(&[("EVE_SHARE", "0")])), None).is_none());
    assert!(!S::configured(Some(&env(&[])), S::PLATFORM).enabled);
    assert!(!S::configured(Some(&env(&[("EVE_SHARE", "1")])), "linux").imessage);
}

#[test]
fn test_tools_offer_only_the_live_ways() {
    let h = home();
    let sh = make(&h, &FakeRunner::new(vec![ana()]));
    assert_eq!(sh.tools()[0]["parameters"]["properties"]["via"]["enum"], json!(["imessage"]));
    let with_email = make(&h, &FakeRunner::new(vec![ana()])).with_token(Some(token("t")));
    assert_eq!(with_email.tools()[0]["parameters"]["properties"]["via"]["enum"], json!(["imessage", "email"]));
    let names: std::collections::HashSet<String> =
        sh.tools().iter().map(|t| t["name"].as_str().unwrap().to_string()).collect();
    assert_eq!(names, S::TOOL_NAMES.iter().map(|n| n.to_string()).collect());
}

#[test]
fn test_off_the_mac_the_tools_and_the_block_name_share_file_alone() {
    // instructions(): no Contacts off the Mac (Lean RoutedTurn P5: the block and the tools agree)
    let h = home();
    let sh = make(&h, &FakeRunner::new(vec![]));
    let off_mac = Share::new(ShareConfig { enabled: true, imessage: false }).with_token(Some(token("t")));
    assert_eq!(off_mac.tools().len(), 1);
    assert!(!off_mac.instructions().contains("find_contact"));
    assert!(off_mac.instructions().starts_with("Sharing a file with someone (share_file)."));
    assert_eq!(sh.instructions(), S::INSTRUCTIONS);
    assert_eq!(off_mac.tools()[0]["parameters"]["properties"]["via"]["enum"], json!(["email"]));
}

// ---- Drive ---------------------------------------------------------------------------------------------------

fn response(status: u16, body: Value, headers: &[(&str, &str)]) -> HttpResponse {
    HttpResponse {
        status,
        headers: headers.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        body: serde_json::to_vec(&body).unwrap(),
    }
}

fn params(req: &HttpRequest) -> std::collections::HashMap<String, String> {
    url::Url::parse(&req.url).unwrap().query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect()
}

fn url_path(req: &HttpRequest) -> String {
    url::Url::parse(&req.url).unwrap().path().to_string()
}

type Route = Arc<dyn Fn(&HttpRequest) -> Option<HttpResponse> + Send + Sync>;

/// FakeDrive: Drive's answers, every request kept.
struct FakeDrive {
    folder_exists: bool,
    requests: Mutex<Vec<HttpRequest>>,
    /// an answer that comes first when it has one (a test's own drive function)
    before: Option<Route>,
}

impl FakeDrive {
    fn new(folder_exists: bool) -> Arc<Self> {
        Arc::new(Self { folder_exists, requests: Mutex::new(Vec::new()), before: None })
    }

    fn requests(&self) -> Vec<HttpRequest> {
        self.requests.lock().clone()
    }

    fn answer(&self, req: &HttpRequest) -> HttpResponse {
        let url = req.url.as_str();
        assert_eq!(req.header("authorization"), Some("Bearer tok"));
        if req.method == "GET" && url.starts_with(&format!("{}/files", S::DRIVE)) {
            let files = if self.folder_exists { json!([{"id": "fold1"}]) } else { json!([]) };
            return response(200, json!({"files": files}), &[]);
        }
        if req.method == "POST" && url.starts_with(&format!("{}/files?", S::DRIVE)) {
            return response(200, json!({"id": "fold1"}), &[]);
        }
        if req.method == "POST" && url.starts_with(S::DRIVE_UPLOAD) && url.contains("uploadType=multipart") {
            return response(
                200,
                json!({"id": "file1", "name": "report.pdf", "webViewLink": "https://drive.google.com/file/d/file1/view"}),
                &[],
            );
        }
        if req.method == "POST" && url.starts_with(S::DRIVE_UPLOAD) && url.contains("uploadType=resumable") {
            return response(200, json!({}), &[("Location", "https://upload.example.test/session1")]);
        }
        if req.method == "PUT" {
            return response(200, json!({"id": "file2", "webViewLink": "https://drive.google.com/file/d/file2"}), &[]);
        }
        if url.contains("/permissions") {
            return response(200, json!({"id": "perm1"}), &[]);
        }
        response(404, json!({"error": {"message": "unexpected"}}), &[])
    }
}

#[async_trait]
impl HttpTransport for FakeDrive {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, HttpError> {
        self.requests.lock().push(request.clone());
        if let Some(r) = self.before.as_ref().and_then(|before| before(&request)) {
            return Ok(r);
        }
        Ok(self.answer(&request))
    }
}

#[tokio::test]
async fn test_email_uploads_then_shares_as_reader_with_notification() {
    let h = home();
    let drive = FakeDrive::new(false);
    let sh = make(&h, &FakeRunner::new(vec![ana()])).with_token(Some(token("tok"))).with_http(drive.clone());
    let out = sh.handle("share_file", &share_args("Ana Lee", "email", "~/Documents/report.pdf", Some("here it is"))).await;
    assert!(s(&out, "question").contains("by email") && drive.requests().is_empty());
    let done = sh.confirm(&s(&out, "token")).await;
    assert_eq!(done["ok"], true, "{done:?}");
    assert!(s(&done, "link").starts_with("https://drive.google.com/"));
    let requests = drive.requests();
    let methods: Vec<(String, String)> = requests.iter().map(|r| (r.method.clone(), url_path(r))).collect();
    assert_eq!(methods[0], ("GET".into(), "/drive/v3/files".into())); // find eve's folder
    assert_eq!(methods[1], ("POST".into(), "/drive/v3/files".into())); // make it
    let made: Value = serde_json::from_slice(&requests[1].body).unwrap();
    assert_eq!(made["mimeType"], S::FOLDER_MIME);
    let upload = &requests[2];
    assert_eq!(params(upload)["uploadType"], "multipart");
    let content = String::from_utf8_lossy(&upload.body);
    assert!(content.contains("\"parents\": [\"fold1\"]") && content.contains("%PDF-1.4"));
    let perm = &requests[3];
    assert_eq!(url_path(perm), "/drive/v3/files/file1/permissions");
    assert_eq!(params(perm)["sendNotificationEmail"], "true");
    assert_eq!(params(perm)["emailMessage"], "here it is");
    let body: Value = serde_json::from_slice(&perm.body).unwrap();
    assert_eq!(body, json!({"type": "user", "role": "reader", "emailAddress": "ana@example.com"}));
}

#[tokio::test]
async fn test_email_large_file_goes_resumable() {
    let h = home();
    let drive = FakeDrive::new(true);
    let mut sh = make(&h, &FakeRunner::new(vec![ana()])).with_token(Some(token("tok"))).with_http(drive.clone());
    sh.multipart_max = 100; // monkeypatch.setattr(S, "MULTIPART_MAX", 100)
    let out = sh.handle("share_file", &share_args("ana@example.com", "email", "~/Documents/report.pdf", None)).await;
    let done = sh.confirm(&s(&out, "token")).await;
    assert_eq!(done["ok"], true, "{done:?}");
    let requests = drive.requests();
    let kinds: Vec<&str> = requests.iter().map(|r| r.method.as_str()).collect();
    assert_eq!(kinds, ["GET", "POST", "PUT", "POST"]);
    let put = &requests[2];
    assert_eq!(put.url, "https://upload.example.test/session1");
    let size = std::fs::metadata(h.home.join("Documents/report.pdf")).unwrap().len();
    assert_eq!(put.header("content-length"), Some(size.to_string().as_str()));
    assert_eq!(put.body, std::fs::read(h.home.join("Documents/report.pdf")).unwrap());
    assert!(!params(&requests[3]).contains_key("emailMessage"));
}

#[tokio::test]
async fn test_email_share_failure_says_the_file_was_not_shared() {
    let h = home();
    let before: Route = Arc::new(|req: &HttpRequest| {
        req.url.contains("/permissions").then(|| response(403, json!({"error": {"message": "forbidden"}}), &[]))
    });
    let drive = Arc::new(FakeDrive { folder_exists: true, requests: Mutex::new(Vec::new()), before: Some(before) });
    let sh = make(&h, &FakeRunner::new(vec![ana()])).with_token(Some(token("tok"))).with_http(drive);
    let out = sh.handle("share_file", &share_args("ana@example.com", "email", "~/Documents/report.pdf", Some("here it is"))).await;
    let done = sh.confirm(&s(&out, "token")).await;
    assert_eq!(done["ok"], false);
    assert!(s(&done, "reason").contains("NOT shared"));
    assert!(s(&done, "reason").starts_with("Drive said HTTP 403: forbidden; "));
}

#[tokio::test]
async fn test_email_off_without_google() {
    let h = home();
    let out = make(&h, &FakeRunner::new(vec![ana()]))
        .handle("share_file", &share_args("Ana Lee", "email", "~/Documents/report.pdf", Some("here it is")))
        .await;
    assert_eq!(out["ok"], false);
    assert!(s(&out, "reason").contains("Google"));
}

// ---- email as a Gmail attachment (owner, 2026-10-09) --------------------------------------------------------------

/// FakeGmail: Gmail's send answers `status`.
struct FakeGmail {
    status: u16,
    requests: Mutex<Vec<HttpRequest>>,
}

#[async_trait]
impl HttpTransport for FakeGmail {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, HttpError> {
        self.requests.lock().push(request);
        let body = if self.status == 200 { json!({"id": "m1"}) } else { json!({"error": {"message": "no"}}) };
        Ok(response(self.status, body, &[]))
    }
}

fn gmail(h: &Home, status: u16) -> (Share, Arc<FakeGmail>) {
    let g = Arc::new(FakeGmail { status, requests: Mutex::new(Vec::new()) });
    let sh = make(h, &FakeRunner::new(vec![ana()])).with_token(Some(token("tok"))).with_gmail(true).with_http(g.clone());
    (sh, g)
}

/// A small MIME reader for the test: (headers, body) of a message or a part, unfolded.
fn split_part(raw: &str) -> (Vec<(String, String)>, String) {
    let (head, body) = raw.split_once("\r\n\r\n").unwrap();
    let mut headers: Vec<(String, String)> = Vec::new();
    for line in head.split("\r\n") {
        if line.starts_with(' ') || line.starts_with('\t') {
            if let Some(last) = headers.last_mut() {
                last.1.push_str(line);
            }
        } else if let Some((k, v)) = line.split_once(':') {
            headers.push((k.to_string(), v.trim_start().to_string()));
        }
    }
    (headers, body.to_string())
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
}

#[tokio::test]
async fn test_an_email_share_is_a_gmail_attachment_of_the_approved_bytes() {
    let h = home();
    let (sh, g) = gmail(&h, 200);
    let out = sh.handle("share_file", &share_args("ana@example.com", "email", "~/Documents/report.pdf", Some("the report"))).await;
    assert!(s(&out, "question").contains("by email, attached, from your Gmail") && g.requests.lock().is_empty()); // asked, not sent
    let done = sh.confirm(&s(&out, "token")).await;
    assert!(done["ok"] == true && done["attached"] == true && g.requests.lock().len() == 1, "{done:?}");
    assert_eq!(done["id"], "m1");
    let r = g.requests.lock()[0].clone();
    assert!(r.url.starts_with(S::GMAIL_SEND_UPLOAD) && params(&r)["uploadType"] == "media");
    assert_eq!(r.header("content-type"), Some("message/rfc822"));
    assert_eq!(r.header("authorization"), Some("Bearer tok"));
    let raw = String::from_utf8(r.body.clone()).unwrap();
    let (headers, body) = split_part(&raw);
    assert_eq!(header(&headers, "To"), Some("ana@example.com"));
    assert_eq!(header(&headers, "Subject"), Some("report.pdf"));
    let content_type = header(&headers, "Content-Type").unwrap();
    let boundary = content_type.split("boundary=\"").nth(1).unwrap().trim_end_matches('"');
    let parts: Vec<&str> = body.split(&format!("--{boundary}")).collect();
    assert_eq!(parts.len(), 4); // "", text, attachment, "--\r\n"
    let (text_headers, text) = split_part(parts[1].trim_start_matches("\r\n"));
    assert_eq!(header(&text_headers, "Content-Type"), Some("text/plain; charset=\"utf-8\""));
    assert!(text.contains("the report"));
    let (file_headers, file) = split_part(parts[2].trim_start_matches("\r\n"));
    assert_eq!(header(&file_headers, "Content-Type"), Some("application/pdf"));
    assert_eq!(header(&file_headers, "Content-Disposition"), Some("attachment; filename=\"report.pdf\""));
    let bytes = base64::engine::general_purpose::STANDARD.decode(file.replace("\r\n", "")).unwrap();
    assert_eq!(bytes, std::fs::read(h.home.join("Documents/report.pdf")).unwrap()); // the exact bytes
}

#[tokio::test]
async fn test_a_file_changed_after_the_question_is_not_attached() {
    let h = home();
    let (sh, g) = gmail(&h, 200);
    let f = h.home.join("Documents/report.pdf");
    let out = sh.handle("share_file", &share_args("ana@example.com", "email", "~/Documents/report.pdf", Some("here it is"))).await;
    let before = std::fs::metadata(&f).unwrap();
    {
        let mut fh = std::fs::OpenOptions::new().write(true).open(&f).unwrap();
        fh.write_all(b"SECRET!!").unwrap();
    }
    same_stamp(&f, &before);
    let done = sh.confirm(&s(&out, "token")).await;
    assert_eq!(done["ok"], false);
    assert!(s(&done, "reason").contains("changed") && g.requests.lock().is_empty());
}

#[tokio::test]
async fn test_a_big_file_or_no_gmail_send_goes_as_a_drive_link() {
    let h = home();
    let (mut sh, _g) = gmail(&h, 200);
    sh.gmail_attach_max = 100; // report.pdf is ~2 KB
    let out = sh.handle("share_file", &share_args("ana@example.com", "email", "~/Documents/report.pdf", Some("here it is"))).await;
    assert!(s(&out, "question").contains("a Google Drive link"));
    assert!(!sh.pending(&s(&out, "token")).unwrap().attach);
    let plain = make(&h, &FakeRunner::new(vec![ana()])).with_token(Some(token("tok"))); // gmail off: a link, as before
    let out = plain.handle("share_file", &share_args("ana@example.com", "email", "~/Documents/report.pdf", Some("here it is"))).await;
    assert!(s(&out, "question").contains("a Google Drive link"));
    assert!(plain.tools()[0]["description"].as_str().unwrap().contains("Google Drive link"));
    assert!(!plain.instructions().contains("Gmail"));
    let (sh2, _) = gmail(&h, 200);
    assert!(sh2.tools()[0]["description"].as_str().unwrap().contains("attached, from the owner's Gmail"));
    assert!(sh2.instructions().contains("attachment from the owner's Gmail"));
}

#[tokio::test]
async fn test_a_gmail_refusal_says_nothing_was_sent() {
    let h = home();
    let (sh, _g) = gmail(&h, 403);
    let out = sh.handle("share_file", &share_args("ana@example.com", "email", "~/Documents/report.pdf", Some("here it is"))).await;
    let done = sh.confirm(&s(&out, "token")).await;
    assert_eq!(done["ok"], false);
    assert!(s(&done, "reason").contains("Gmail said HTTP 403") && s(&done, "reason").contains("nothing was sent"));
}

#[test]
fn test_gmail_can_send_needs_the_policy_and_the_granted_scope() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().to_path_buf();
    let env = |scope: &str, extra: &[(&str, &str)]| {
        std::fs::write(dir.join("google-token.json"), json!({"refresh_token": "r", "scope": scope}).to_string()).unwrap();
        let mut e: std::collections::HashMap<String, String> =
            [("EVE_APPS".to_string(), "1".to_string()), ("EVE_APPS_DIR".to_string(), dir.to_string_lossy().into_owned())]
                .into_iter()
                .collect();
        for (k, v) in extra {
            e.insert(k.to_string(), v.to_string());
        }
        e
    };
    let g = "https://www.googleapis.com/auth/";
    assert!(S::gmail_can_send(Some(&env(&format!("{g}gmail.readonly {g}gmail.send"), &[]))));
    assert!(!S::gmail_can_send(Some(&env(&format!("{g}gmail.readonly"), &[])))); // an older sign-in
    assert!(!S::gmail_can_send(Some(&env(&format!("{g}gmail.send"), &[("EVE_APPS_POLICY", "gmail=read")]))));
    assert!(!S::gmail_can_send(Some(&env(&format!("{g}gmail.send"), &[("EVE_APPS", "0")]))));
}

// ---- the email itself, byte for byte against Python's EmailMessage(policy=SMTP) ----------------------------------

/// The message with its random boundary swapped for "B".
fn mail_with_boundary(to: &str, filename: &str, note: &str, data: &[u8]) -> String {
    let raw = String::from_utf8(S::attachment_mail(to, filename, note, data).unwrap()).unwrap();
    let start = raw.find("boundary=\"").unwrap() + "boundary=\"".len();
    let end = start + raw[start..].find('"').unwrap();
    let boundary = raw[start..end].to_string();
    assert!(boundary.starts_with("===============") && boundary.ends_with("==") && boundary.len() == 36);
    raw.replace(&boundary, "B")
}

#[test]
fn test_attachment_mail_matches_pythons_bytes() {
    // python3: EmailMessage(policy=SMTP), To/Subject, set_content(note), add_attachment(...).as_bytes()
    assert_eq!(
        mail_with_boundary("ana@example.com", "report.pdf", "the report", &[b'x'; 100]),
        "To: ana@example.com\r\nSubject: report.pdf\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"B\"\r\n\r\n\
         --B\r\nContent-Type: text/plain; charset=\"utf-8\"\r\nContent-Transfer-Encoding: 7bit\r\n\r\nthe report\r\n\r\n\
         --B\r\nContent-Type: application/pdf\r\nContent-Transfer-Encoding: base64\r\nContent-Disposition: attachment; filename=\"report.pdf\"\r\nMIME-Version: 1.0\r\n\r\n\
         eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4\r\neHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eA==\r\n\r\n--B--\r\n"
    );
    assert_eq!(
        mail_with_boundary("ana@example.com", "report.pdf", "", b"x"),
        "To: ana@example.com\r\nSubject: report.pdf\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"B\"\r\n\r\n\
         --B\r\nContent-Type: text/plain; charset=\"utf-8\"\r\nContent-Transfer-Encoding: 7bit\r\n\r\n\r\n\r\n\
         --B\r\nContent-Type: application/pdf\r\nContent-Transfer-Encoding: base64\r\nContent-Disposition: attachment; filename=\"report.pdf\"\r\nMIME-Version: 1.0\r\n\r\n\
         eA==\r\n\r\n--B--\r\n"
    );
    // non-ASCII: an 8bit body, an RFC 2231 file name, an encoded subject word
    let m = mail_with_boundary("ana@example.com", "rapport é.pdf", "note é ü", b"x");
    assert!(m.contains("Subject: rapport =?utf-8?q?=C3=A9=2Epdf?=\r\n"), "{m}");
    assert!(m.contains("Content-Transfer-Encoding: 8bit\r\n\r\nnote é ü\r\n\r\n--B"));
    assert!(m.contains("Content-Disposition: attachment; filename*=utf-8''rapport%20%C3%A9.pdf\r\n"));
    // a long line: quoted-printable, as Python breaks it
    let m = mail_with_boundary("ana@example.com", &format!("{}.pdf", "a".repeat(90)), &"n ".repeat(60), b"x");
    assert!(m.contains(
        "Content-Transfer-Encoding: quoted-printable\r\n\r\nn n n n n n n n n n n n n n n n n n n n n n n n n n n n n n n n n n n n n n n=\r\n n n n n n n n n n n n n n n n n n n n n n=20\r\n\r\n--B"
    ), "{m}");
    assert!(m.contains(&format!(
        "Content-Disposition: attachment;\r\n filename*0*=us-ascii''{};\r\n filename*1*={}.pdf\r\n",
        "a".repeat(54),
        "a".repeat(36)
    )), "{m}");
    assert!(m.contains(&format!("Subject: =?utf-8?q?{}?=\r\n =?utf-8?q?{}=2Epdf?=\r\n", "a".repeat(57), "a".repeat(33))), "{m}");
    // quoting and folding of an ASCII name
    let m = mail_with_boundary("a@b.co", "a\"b.pdf", "x", b"x");
    assert!(m.contains("Content-Disposition: attachment; filename=\"a\\\"b.pdf\"\r\n"));
    let long = format!("{}.pdf", "x".repeat(60));
    let m = mail_with_boundary("a@b.co", &long, "x", b"x");
    assert!(m.contains(&format!("Subject: {long}\r\n")));
    assert!(m.contains(&format!("Content-Disposition: attachment;\r\n filename=\"{long}\"\r\n")));
    // a long subject folds at whitespace
    let m = mail_with_boundary("a@b.co", &"a b ".repeat(30), "x", b"x");
    assert!(m.contains(
        "Subject: a b a b a b a b a b a b a b a b a b a b a b a b a b a b a b a b a b a\r\n b a b a b a b a b a b a b a b a b a b a b a b a b \r\n"
    ), "{m}");
}

#[test]
fn test_attachment_mail_refuses_a_line_break_in_a_header() {
    assert!(S::attachment_mail("a@b.co", "a\nb.pdf", "x", b"x").is_err());
    assert!(S::attachment_mail("a@b.co", "a\rb.pdf", "x", b"x").is_err());
    assert!(S::attachment_mail("a@b.co", "a\tb.pdf", "x", b"x").is_ok());
}

#[test]
fn test_small_rules() {
    assert!(S::is_email(" ana@example.com ") && !S::is_email("ana@example") && !S::is_email("a b@x.co"));
    assert!(S::is_phone("+1 (555) 010-0199") && !S::is_phone("555-01") && !S::is_phone("call me"));
    assert_eq!(S::phone_handle("+1 (555) 010-0199"), "+15550100199");
    assert_eq!(S::phone_handle("555.010.0199"), "5550100199");
    assert_eq!(S::human_size(512), "512 bytes");
    assert_eq!(S::human_size(2009), "2 KB");
    assert_eq!(S::human_size(S::MAX_IMESSAGE_BYTES as i64), "100.0 MB");
    assert_eq!(S::human_size(S::MAX_DRIVE_BYTES as i64), "1024.0 MB");
}

#[tokio::test]
async fn test_the_real_runner_reports_a_missing_program_and_a_timeout() {
    let (rc, out, err) = S::osascript_runner(&["/no/such/program-eve".to_string()], b"", 5.0).await;
    assert_eq!((rc, out.as_str()), (S::RC_NO_START, ""));
    assert_eq!(err, "could not start (No such file or directory)");
    let (rc, _, _) = S::osascript_runner(&["/bin/sleep".into(), "5".into()], b"", 0.1).await;
    assert_eq!(rc, S::RC_TIMEOUT);
    let (rc, out, _) = S::osascript_runner(&["/bin/cat".into()], b"through stdin", 5.0).await;
    assert_eq!((rc, out.as_str()), (0, "through stdin"));
    let (rc, _, _) = S::osascript_runner(&["/bin/sh".into(), "-c".into(), "exit 3".into()], b"", 5.0).await;
    assert_eq!(rc, 3);
}

// ================================================================================================================
// test_notify.py
// ================================================================================================================

const TOKEN: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const OWNER: i64 = 42;

/// FakePost: answers `status`, or fails with `error` (kind, words).
#[derive(Clone)]
struct FakePost {
    status: i64,
    error: Option<(String, String)>,
    calls: Posts,
}

impl FakePost {
    fn new() -> Self {
        Self { status: 200, error: None, calls: Arc::new(Mutex::new(Vec::new())) }
    }

    fn post(&self) -> PostFn {
        let me = self.clone();
        Arc::new(move |url: &str, body: &[u8], headers: &[(String, String)]| {
            me.calls.lock().push((url.to_string(), String::from_utf8_lossy(body).into_owned(), headers.to_vec()));
            if let Some((kind, words)) = &me.error {
                return Err(anyhow::Error::new(PostError::new(kind, words.clone())));
            }
            Ok(me.status)
        })
    }

    fn bodies(&self) -> Vec<String> {
        self.calls.lock().iter().map(|c| c.1.clone()).collect()
    }
}

fn fixed(token: &'static str) -> Arc<dyn Fn() -> String + Send + Sync> {
    Arc::new(move || token.to_string())
}

fn quick(post: &FakePost, preview: bool) -> Arc<Notifier> {
    Notifier::new(fixed(TOKEN), notify::NOTIFY_URL, preview, post.post(), Arc::new(eve_brain::prelude::monotonic), 0.02, 0.15)
}

async fn sleep(secs: f64) {
    tokio::time::sleep(Duration::from_secs_f64(secs)).await;
}

// ---- the topic -----------------------------------------------------------------------------------------------

#[test]
fn test_the_topic_is_the_phones_and_differs_from_the_rendezvous() {
    // The same vector is asserted against Swift's NotifyTopic in ios/EveLink/Tests/main.swift.
    assert_eq!(notify::notify_topic(TOKEN), "eve-312f09f108bc0795afe2c8447ddfbd6a");
    assert_ne!(notify::notify_topic(TOKEN), eve_stick_link::rendezvous_topic(TOKEN));
    assert!(!notify::notify_topic(TOKEN).contains(TOKEN));
    assert_ne!(notify::notify_topic(&"b".repeat(40)), notify::notify_topic(TOKEN));
}

#[test]
fn test_the_switches() {
    let env = |pairs: &[(&str, &str)]| -> std::collections::HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    };
    assert_eq!(
        notify::configured(Some(&env(&[]))),
        notify::NotifyConfig { enabled: true, base: "https://ntfy.sh".into(), preview: true }
    );
    assert!(!notify::configured(Some(&env(&[("EVE_NOTIFY", "0")]))).enabled);
    assert!(!notify::configured(Some(&env(&[("EVE_NOTIFY_PREVIEW", "off")]))).preview);
    assert_eq!(notify::configured(Some(&env(&[("EVE_NOTIFY_URL", "https://ntfy.home/")]))).base, "https://ntfy.home");
    assert!(notify::make_notifier(Some(&env(&[("EVE_NOTIFY", "0")]))).is_none());
    // test_make_inlet_lends_the_notifier_in_app_mode_and_not_when_off, at this layer: make_api lends what this makes
    assert!(notify::make_notifier(Some(&env(&[]))).is_some());
}

// ---- the words -----------------------------------------------------------------------------------------------

#[test]
fn test_plain_text_strips_html_markdown_and_openui_and_caps() {
    let html = "<b>Watch</b>\n<i>flights</i>\n\nThe fare is <b>$412</b> &amp; falling, <a href=\"https://e.com\">see</a>";
    assert_eq!(notify::plain_text(html), "Watch\nflights\nThe fare is $412 & falling, see");
    assert_eq!(notify::plain_text("**done**: the [report](https://x.y) is in `out.md`"), "done: the report is in out.md");
    assert_eq!(
        notify::plain_text("Here:\n```openui-lang\nroot = Card()\n```"),
        "Here:\n(A table or chart is here: open Eve Link to see it.)"
    );
    let long = notify::plain_text(&"word ".repeat(200));
    assert!(long.chars().count() == notify::MAX_CHARS && long.ends_with('…'));
}

// ---- the hook: MutedBotApi's sends ---------------------------------------------------------------------------
//
// MutedBotApi hands every Bot API call it swallows to `offer_send(method, data)` (telegram_wire's tests cover that
// hand-over); these send the calls it makes, as the Bot API names and shapes them.

fn send(n: &Arc<Notifier>, method: &str, data: Value) {
    n.offer_send(method, &obj(data));
}

fn inline(rows: Value) -> Value {
    json!({"inline_keyboard": rows})
}

#[tokio::test(flavor = "multi_thread")]
async fn test_muted_sends_make_exactly_one_coalesced_post() {
    let post = FakePost::new();
    let n = quick(&post, true);
    send(&n, "sendMessage", json!({"chat_id": OWNER, "text": "first", "parse_mode": "HTML"}));
    send(&n, "sendMessage", json!({"chat_id": OWNER, "text": "the price <b>dropped</b>", "parse_mode": "HTML"}));
    send(&n, "sendMessage", json!({"chat_id": OWNER, "text": "Go on the chief's job?", "parse_mode": "HTML",
        "reply_markup": inline(json!([[{"text": "Go", "callback_data": "k1", "style": "success"},
                                        {"text": "No", "callback_data": "k2"}]]))}));
    sleep(0.1).await;
    let calls = post.calls.lock().clone();
    assert!(calls.len() == 1 && n.posts() == 1);
    let (url, body, headers) = &calls[0];
    assert_eq!(*url, format!("https://ntfy.sh/{}", notify::notify_topic(TOKEN)));
    assert_eq!(body, "Go on the chief's job? (+2 more)");
    assert_eq!(*headers, vec![("Title".to_string(), "eve".to_string()), ("Click".to_string(), "evelink://chat".to_string())]);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_edits_typing_drafts_reactions_commands_and_progress_cards_do_not_notify() {
    let post = FakePost::new();
    let n = quick(&post, true);
    send(&n, "editMessageText", json!({"chat_id": OWNER, "message_id": 5, "text": "edited"}));
    send(&n, "editMessageReplyMarkup", json!({"chat_id": OWNER, "message_id": 5, "reply_markup": inline(json!([]))}));
    send(&n, "sendChatAction", json!({"chat_id": OWNER, "action": "typing"}));
    send(&n, "sendMessageDraft", json!({"chat_id": OWNER, "draft_id": 1, "text": ""}));
    send(&n, "setMessageReaction", json!({"chat_id": OWNER, "message_id": 5, "reaction": []}));
    send(&n, "answerCallbackQuery", json!({"callback_query_id": "q", "text": "ok"}));
    send(&n, "setMyCommands", json!({"commands": [{"command": "jobs", "description": "your jobs"}]}));
    send(&n, "sendMessage", json!({"chat_id": OWNER, "text": "Working on it",
        "reply_markup": inline(json!([[{"text": STOP_BUTTON, "callback_data": "s1", "style": "danger"}]]))}));
    sleep(0.1).await;
    assert!(post.calls.lock().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn test_a_picture_and_a_file_notify() {
    let post = FakePost::new();
    let n = quick(&post, true);
    send(&n, "sendPhoto", json!({"chat_id": OWNER, "caption": ""}));
    sleep(0.1).await;
    send(&n, "sendDocument", json!({"chat_id": OWNER, "caption": "the notes"}));
    sleep(0.3).await;
    assert_eq!(post.bodies(), ["eve sent a picture", "the notes"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_at_most_one_post_per_gap_the_rest_folded() {
    let post = FakePost::new();
    let n = quick(&post, true);
    n.offer("one");
    sleep(0.05).await; // posted
    n.offer("two");
    sleep(0.05).await; // inside the gap: held
    assert_eq!(post.calls.lock().len(), 1);
    n.offer("three");
    sleep(0.2).await; // the gap is over: one post with both
    assert_eq!(post.bodies(), ["one", "three (+1 more)"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_preview_off_sends_no_words() {
    let post = FakePost::new();
    let n = quick(&post, false);
    n.offer("the secret code is 1234");
    n.offer("and another");
    sleep(0.1).await;
    assert_eq!(post.bodies(), ["eve has 2 new messages"]);
    assert!(!post.bodies()[0].contains("1234"));
}

#[tokio::test(flavor = "multi_thread")]
async fn test_no_token_posts_nothing() {
    let post = FakePost::new();
    let n = Notifier::new(fixed(""), notify::NOTIFY_URL, true, post.post(), Arc::new(eve_brain::prelude::monotonic), 0.01, notify::MIN_GAP_SECS);
    n.offer("hello");
    sleep(0.05).await;
    assert!(post.calls.lock().is_empty());
}

/// Every log line of this test binary, from every thread (the posts run on worker threads).
fn captured() -> Arc<Mutex<Vec<u8>>> {
    static LOGS: OnceLock<Arc<Mutex<Vec<u8>>>> = OnceLock::new();
    LOGS.get_or_init(|| {
        let logs = Arc::new(Mutex::new(Vec::new()));
        let sink = logs.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .with_ansi(false)
            .with_writer(move || Sink(sink.clone()))
            .finish();
        let _ = tracing::subscriber::set_global_default(subscriber);
        logs
    })
    .clone()
}

struct Sink(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

async fn failure_case(post: FakePost, warning: &str) {
    let logs = captured();
    let n = quick(&post, true);
    send(&n, "sendMessage", json!({"chat_id": OWNER, "text": "private words"})); // returns, never raises
    sleep(0.1).await;
    assert_eq!(n.posts(), 0);
    assert_eq!(post.calls.lock().len(), 1);
    // the notifier's lines (the other tests of this binary log beside it)
    let text: String = String::from_utf8_lossy(&logs.lock())
        .lines()
        .filter(|l| l.contains("eve_brain::notify") || l.contains("eve_brain::telegram"))
        .map(|l| format!("{l}\n"))
        .collect();
    // Python: `"eve-" not in caplog.text`; here make_notifier's own line (another test's) names "eve-bridge"
    let topics = text.replace("eve-bridge", "");
    assert!(!topics.contains("eve-") && !text.contains("private words"), "{text}");
    assert!(text.lines().any(|l| l.contains("WARN") && l.contains(warning)), "{text}");
}

#[tokio::test(flavor = "multi_thread")]
async fn test_failures_never_raise_and_never_log_the_topic_or_text_oserror() {
    let post = FakePost { error: Some(("OSError".into(), "https://ntfy.sh/eve-secret-topic down".into())), ..FakePost::new() };
    failure_case(post, "notify: could not post a notification (OSError)").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_failures_never_raise_and_never_log_the_topic_or_text_valueerror() {
    let post = FakePost { error: Some(("ValueError".into(), "bad".into())), ..FakePost::new() };
    failure_case(post, "notify: could not post a notification (ValueError)").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_failures_never_raise_and_never_log_the_topic_or_text_status_500() {
    let post = FakePost { status: 500, ..FakePost::new() };
    failure_case(post, "notify: ntfy answered 500").await;
}

#[test]
fn test_offer_never_blocks_without_a_loop_and_never_raises() {
    let post = FakePost::new();
    let n = quick(&post, true);
    n.offer("no loop here"); // a debug line, nothing else
    assert!(post.calls.lock().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn test_close_cancels_a_pending_post() {
    let post = FakePost::new();
    let n = Notifier::new(fixed(TOKEN), notify::NOTIFY_URL, true, post.post(), Arc::new(eve_brain::prelude::monotonic), 5.0, notify::MIN_GAP_SECS);
    send(&n, "sendMessage", json!({"chat_id": OWNER, "text": "later"}));
    n.close().await; // MutedBotApi.close closes its notifier
    assert!(post.calls.lock().is_empty());
}

// ---- the CLI's pieces ------------------------------------------------------------------------------------------

#[test]
fn test_cli_topic_and_test() {
    // `eve-bridge notify topic` prints setup_lines; `notify test` posts TEST_LINE through post_once
    let topic = notify::notify_topic(TOKEN);
    let out = notify::setup_lines(TOKEN, notify::NOTIFY_URL).join("\n");
    assert!(out.contains(&topic) && out.contains(&format!("https://ntfy.sh/{topic}")) && out.contains("App Store"));
    assert!(!out.contains(TOKEN));
    let post = FakePost::new();
    assert!(notify::post_once(TOKEN, notify::TEST_LINE, notify::NOTIFY_URL, &post.post()));
    assert_eq!(post.bodies(), [notify::TEST_LINE]);
    assert!(!notify::post_once("", notify::TEST_LINE, notify::NOTIFY_URL, &post.post()));
    assert!(!notify::post_once(TOKEN, notify::TEST_LINE, "", &post.post()));
}

#[test]
fn test_cli_without_a_pairing() {
    // the CLI says "not paired" when read_token is empty: a missing or short token reads as ""
    let tmp = tempfile::tempdir().unwrap();
    assert_eq!(notify::read_token(Some(&tmp.path().join("stick-link.json"))), "");
    std::fs::write(tmp.path().join("short.json"), r#"{"token": "abc"}"#).unwrap();
    assert_eq!(notify::read_token(Some(&tmp.path().join("short.json"))), "");
    std::fs::write(tmp.path().join("good.json"), json!({"token": TOKEN}).to_string()).unwrap();
    assert_eq!(notify::read_token(Some(&tmp.path().join("good.json"))), TOKEN);
}
