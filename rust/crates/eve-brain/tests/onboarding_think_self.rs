//! Unit `onboarding_think_self`: onboarding.py 1-571, think.py 1-225, self_context.py 1-412; tests: test_onboarding.py, test_think.py, test_self_context.py, test_self_context_lite.py.
//!
//! The shared fakes are in `support` (architect-owned); this unit's own go here.
//!
//! Every HTTP exchange of the sign-ins goes through a fake transport (Python's httpx.MockTransport), so the real
//! request code runs and nothing leaves the Mac. The Composio shapes are the ones spike S1 recorded
//! (bridge/tests/fixtures/composio_connect). Canaries are not token-shaped, and each absence check has a planted
//! positive control. Python's `monkeypatch` of `_hide`, `system_context.context`, `search_router.available` /
//! `SHIPPED`, `subprocess.run` and HOME are the seams the ports lend instead (`with_hide`, `with_clock`,
//! `self_context::Lent`, `commit_of`).

mod support;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine as _;
use parking_lot::Mutex;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use eve_brain::onboarding::{
    self, credit_with, save_openrouter_key_with, ComposioConnectLogin, Hide, OnboardingError, OpenRouterLogin,
    SignInError,
};
use eve_brain::prelude::{Clock, Http, HttpError, HttpRequest, HttpResponse, HttpTransport};
use eve_brain::self_context::{self, Lent, Router};
use eve_brain::think::{
    self, configured, make_openai_thinker, parse_answer, question_items, request_with_clock, OpenAIThinker, ThinkConfig,
    ANSWER_MAX_CHARS, CONTEXT_HEADER, CONTEXT_MAX_CHARS, INSTRUCTIONS,
};
use eve_web::websearch::{self, SearchConfig};

type Obj = Map<String, Value>;

// ==== onboarding: the fakes ========================================================================================

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../bridge/tests/fixtures/composio_connect");

fn fixture(name: &str) -> Value {
    let text = std::fs::read_to_string(Path::new(FIXTURES).join(name)).expect("fixture");
    serde_json::from_str(&text).expect("fixture json")
}

const KEY: &str = "CANARY-NOT-A-KEY-7f3c";
const ACCESS: &str = "CANARY-NOT-AN-ACCESS-TOKEN-7f3c";
const REFRESH: &str = "CANARY-NOT-A-REFRESH-TOKEN-7f3c";
const ID_TOKEN: &str = "CANARY-NOT-AN-ID-TOKEN-7f3c";
const OR_CALLBACK: &str = "http://localhost:48780/onboard/openrouter/callback";
const CP_CALLBACK: &str = "http://localhost:48780/onboard/composio/callback";
const CP_127: &str = "http://127.0.0.1:48780/onboard/composio/callback";
const T0: f64 = 1_800_000_000.0;

fn client_id() -> String {
    fixture("registration.json")["response"]["client_id"].as_str().unwrap().to_string()
}

fn exchange() -> String {
    format!("POST {}", onboarding::OPENROUTER_KEYS_URL)
}
fn key_info_route() -> String {
    format!("GET {}", eve_spend_sync::OPENROUTER_KEY_URL)
}
fn credits_route() -> String {
    format!("GET {}", eve_spend_sync::OPENROUTER_CREDITS_URL)
}
fn register_route() -> String {
    format!("POST {}", onboarding::CONNECT_REGISTER_URL)
}
fn token_route() -> String {
    format!("POST {}", onboarding::CONNECT_TOKEN_URL)
}

fn management_only() -> Value {
    json!({"error": {"code": 403, "message": "Only management keys can perform this operation."}})
}

/// The fake clock (Python's `Clock`): `now` moves when a test moves it.
#[derive(Clone)]
struct FakeClock(Arc<Mutex<f64>>);

impl FakeClock {
    fn new() -> Self {
        FakeClock(Arc::new(Mutex::new(T0)))
    }
    fn clock(&self) -> Clock {
        let now = self.0.clone();
        Arc::new(move || *now.lock())
    }
    fn now(&self) -> f64 {
        *self.0.lock()
    }
    fn advance(&self, secs: f64) {
        *self.0.lock() += secs;
    }
}

type Answer = Arc<dyn Fn(&HttpRequest) -> HttpResponse + Send + Sync>;

/// One route of the fake: an answer, an answer after a pause, or a transport error of this kind.
#[derive(Clone)]
enum Route {
    Answer(Answer),
    Slow(f64, Answer),
    Raise(&'static str),
}

/// A fake OpenRouter and Composio (Python's `Provider`): routes are "METHOD url" without the query; every request
/// is kept.
struct Provider {
    routes: Mutex<HashMap<String, Route>>,
    seen: Mutex<Vec<HttpRequest>>,
    closed: AtomicBool,
}

impl Provider {
    fn new(routes: Vec<(String, Route)>) -> Arc<Self> {
        Arc::new(Provider {
            routes: Mutex::new(routes.into_iter().collect()),
            seen: Mutex::new(Vec::new()),
            closed: AtomicBool::new(false),
        })
    }

    fn http(self: &Arc<Self>) -> Http {
        self.clone()
    }

    fn set(&self, route: String, answer: Route) {
        self.routes.lock().insert(route, answer);
    }

    fn seen(&self) -> Vec<HttpRequest> {
        self.seen.lock().clone()
    }

    fn to(&self, route: &str) -> Vec<HttpRequest> {
        let (method, url) = route.split_once(' ').unwrap();
        self.seen().into_iter().filter(|r| r.method == method && without_query(&r.url) == url).collect()
    }
}

fn without_query(url: &str) -> &str {
    url.split_once('?').map_or(url, |(u, _)| u)
}

#[async_trait]
impl HttpTransport for Provider {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, HttpError> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(HttpError {
                kind: "RuntimeError".into(),
                message: "Cannot send a request, as the client has been closed.".into(),
            });
        }
        self.seen.lock().push(request.clone());
        let key = format!("{} {}", request.method, without_query(&request.url));
        let route = self.routes.lock().get(&key).cloned();
        match route {
            None => Ok(json_response(404, &json!({"error": "no such route in the fake"}))),
            Some(Route::Raise(kind)) => Err(HttpError { kind: kind.into(), message: "the fake refused".into() }),
            Some(Route::Answer(f)) => Ok(f(&request)),
            Some(Route::Slow(secs, f)) => {
                tokio::time::sleep(std::time::Duration::from_secs_f64(secs)).await;
                Ok(f(&request))
            }
        }
    }
}

fn json_response(status: u16, body: &Value) -> HttpResponse {
    HttpResponse {
        status,
        headers: vec![("content-type".into(), "application/json".into())],
        body: serde_json::to_vec(body).unwrap(),
    }
}

fn reply(status: u16, body: Value) -> Route {
    Route::Answer(Arc::new(move |_r| json_response(status, &body)))
}

fn raw(status: u16, text: &str) -> Route {
    let text = text.to_string();
    Route::Answer(Arc::new(move |_r| HttpResponse {
        status,
        headers: vec![("content-type".into(), "application/json".into())],
        body: text.clone().into_bytes(),
    }))
}

/// Composio's 201 (registration.json): the request echoed, plus the client id and two defaults.
fn registered_answer(request: &HttpRequest) -> HttpResponse {
    let mut sent: Obj = serde_json::from_slice(&request.body).unwrap();
    let fixture = fixture("registration.json");
    for k in ["client_id", "client_id_issued_at", "application_type"] {
        sent.insert(k.into(), fixture["response"][k].clone());
    }
    json_response(201, &Value::Object(sent))
}

fn registered() -> Route {
    Route::Answer(Arc::new(registered_answer))
}

/// The token endpoint's 200, written as Python's own encoder would: each change is a key and its raw JSON text
/// (`Infinity`, `NaN` and integers no float holds go out as the bare words a careless server sends), or None to
/// leave the key out.
fn granted(changes: &[(&str, Option<String>)]) -> Route {
    let mut answer: Vec<(String, String)> = vec![
        ("access_token".into(), format!("\"{ACCESS}\"")),
        ("refresh_token".into(), format!("\"{REFRESH}\"")),
        ("token_type".into(), "\"Bearer\"".into()),
        ("expires_in".into(), "300".into()),
        ("scope".into(), format!("\"{}\"", onboarding::CONNECT_SCOPE)),
        ("id_token".into(), format!("\"{ID_TOKEN}\"")),
    ];
    for (k, v) in changes {
        match answer.iter().position(|(key, _)| key == k) {
            Some(i) => match v {
                Some(v) => answer[i].1 = v.clone(),
                None => {
                    answer.remove(i);
                }
            },
            None => {
                if let Some(v) = v {
                    answer.push((k.to_string(), v.clone()));
                }
            }
        }
    }
    let text = format!(
        "{{{}}}",
        answer.iter().map(|(k, v)| format!("\"{k}\": {v}")).collect::<Vec<_>>().join(", ")
    );
    raw(200, &text)
}

fn query(url: &str) -> HashMap<String, Vec<String>> {
    let q = url.split_once('?').map_or("", |(_, q)| q);
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    for (k, v) in url::form_urlencoded::parse(q.as_bytes()) {
        out.entry(k.into_owned()).or_default().push(v.into_owned());
    }
    out
}

fn query_part(url: &str) -> String {
    url.split_once('?').map_or("", |(_, q)| q).to_string()
}

fn state_of(url: &str) -> String {
    query(url)["state"][0].clone()
}

fn s256(verifier: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn unb64(text: &str) -> Vec<u8> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(text.trim_end_matches('=')).unwrap()
}

fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// A JWT-shaped string built here from the claims' raw JSON text, so the source holds nothing token-shaped.
fn jwt(claims: &str) -> String {
    let part = |data: &str| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(data.as_bytes());
    format!("\"{}.{}.unsigned\"", part("{\"alg\": \"none\"}"), part(claims))
}

fn recorder() -> (Hide, Arc<Mutex<Vec<String>>>) {
    let hidden = Arc::new(Mutex::new(Vec::new()));
    let h = hidden.clone();
    let hide: Hide = Arc::new(move |values: &[&str]| h.lock().extend(values.iter().map(|v| v.to_string())));
    (hide, hidden)
}

fn openrouter(routes: Vec<(String, Route)>) -> (OpenRouterLogin, Arc<Provider>, FakeClock, Arc<Mutex<Vec<String>>>) {
    let mut all = vec![(exchange(), reply(200, json!({"key": KEY, "user_id": "user-not-real"})))];
    all.extend(routes);
    let fake = Provider::new(all);
    let clock = FakeClock::new();
    let (hide, hidden) = recorder();
    let login = OpenRouterLogin::new(Some(fake.http()), Some(clock.clock())).with_hide(hide);
    (login, fake, clock, hidden)
}

fn composio(dir: &Path, routes: Vec<(String, Route)>) -> (ComposioConnectLogin, Arc<Provider>, FakeClock, Arc<Mutex<Vec<String>>>) {
    let mut all = vec![(register_route(), registered()), (token_route(), granted(&[]))];
    all.extend(routes);
    let fake = Provider::new(all);
    let clock = FakeClock::new();
    let (hide, hidden) = recorder();
    let login = ComposioConnectLogin::new(
        Some(&dir.join("composio-connect.json")),
        "eve lite on test-mac",
        Some(fake.http()),
        Some(clock.clock()),
    )
    .with_hide(hide);
    (login, fake, clock, hidden)
}

fn tmp() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

fn is_root() -> bool {
    // SAFETY: geteuid has no preconditions.
    unsafe { libc::geteuid() == 0 }
}

// ==== OpenRouter: the URL and PKCE ================================================================================

#[test]
fn test_the_start_url_carries_the_callback_challenge_state_and_label() {
    let url = OpenRouterLogin::new(None, None).start(OR_CALLBACK, "eve lite on test-mac").unwrap();
    assert!(url.starts_with("https://openrouter.ai/auth?"));
    let q = query(&url);
    let mut keys: Vec<&String> = q.keys().collect();
    keys.sort();
    assert_eq!(keys, ["callback_url", "code_challenge", "code_challenge_method", "key_label", "state"]);
    assert_eq!(q["callback_url"], [OR_CALLBACK]);
    assert_eq!(q["code_challenge_method"], ["S256"]);
    assert_eq!(q["key_label"], ["eve lite on test-mac"]);
    assert!(query_part(&url).contains("key_label=eve%20lite%20on%20test-mac")); // %20, as the spec's URL writes it
    assert_eq!(unb64(&q["state"][0]).len(), 16); // 128 bits
    let challenge = &q["code_challenge"][0];
    assert_eq!(challenge.len(), 43);
    assert!(challenge.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'));
}

#[tokio::test]
async fn test_the_challenge_is_the_unpadded_base64url_sha256_of_the_verifier_sent() {
    let (login, fake, _, _) = openrouter(vec![]);
    let url = login.start(OR_CALLBACK, "x").unwrap();
    assert_eq!(login.finish("code-1", state_of(&url), "").await.unwrap(), KEY);
    let body: Value = serde_json::from_slice(&fake.seen()[0].body).unwrap();
    let verifier = body["code_verifier"].as_str().unwrap().to_string();
    assert!((43..=128).contains(&verifier.len())); // RFC 7636 4.1
    assert!(verifier.chars().all(|c| c.is_ascii_alphanumeric() || "._~-".contains(c)));
    let challenge = query(&url)["code_challenge"][0].clone();
    assert!(!challenge.contains('=') && challenge == s256(&verifier));
    assert_eq!(unb64(&challenge), Sha256::digest(verifier.as_bytes()).to_vec()); // decoded, the digest
}

#[test]
fn test_each_start_gets_its_own_state_and_verifier() {
    let (login, _, _, _) = openrouter(vec![]);
    let urls: Vec<String> = (0..3).map(|_| login.start(OR_CALLBACK, "x").unwrap()).collect();
    let states: std::collections::HashSet<String> = urls.iter().map(|u| state_of(u)).collect();
    let challenges: std::collections::HashSet<String> = urls.iter().map(|u| query(u)["code_challenge"][0].clone()).collect();
    assert_eq!(states.len(), 3);
    assert_eq!(challenges.len(), 3);
}

#[tokio::test]
async fn test_the_exchange_body_is_exactly_code_verifier_and_method() {
    let (login, fake, _, _) = openrouter(vec![]);
    let state = state_of(&login.start(OR_CALLBACK, "x").unwrap());
    login.finish("code-1", state, "").await.unwrap();
    let seen = fake.seen();
    assert_eq!(seen.len(), 1);
    let request = &seen[0];
    assert_eq!(request.method, "POST");
    assert_eq!(request.url, "https://openrouter.ai/api/v1/auth/keys");
    assert_eq!(request.header("content-type"), Some("application/json"));
    assert_eq!(request.header("authorization"), None);
    let body: Obj = serde_json::from_slice(&request.body).unwrap();
    let mut keys: Vec<&String> = body.keys().collect();
    keys.sort();
    assert_eq!(keys, ["code", "code_challenge_method", "code_verifier"]);
    assert_eq!(body["code"], "code-1");
    assert_eq!(body["code_challenge_method"], "S256");
}

// ==== OpenRouter: the state =======================================================================================

#[tokio::test]
async fn test_a_wrong_or_missing_state_is_400_and_nothing_is_sent() {
    for state in [json!(""), Value::Null, json!("not-the-state"), json!(["a", "list"])] {
        let (login, fake, _, _) = openrouter(vec![]);
        let good = state_of(&login.start(OR_CALLBACK, "x").unwrap());
        let e = login.finish("code-1", state.clone(), "").await.unwrap_err();
        assert!(e.is_state(), "{state}");
        assert_eq!(e.status, 400);
        assert!([onboarding::STATE_MISSING, onboarding::STATE_UNKNOWN].contains(&e.to_string().as_str()));
        assert!(fake.seen().is_empty());
        assert_eq!(login.finish("code-1", good, "").await.unwrap(), KEY); // control: the real state still works
    }
}

#[tokio::test]
async fn test_a_reused_state_is_400_and_the_second_exchange_never_happens() {
    let (login, fake, _, _) = openrouter(vec![]);
    let state = state_of(&login.start(OR_CALLBACK, "x").unwrap());
    assert_eq!(login.finish("code-1", state.clone(), "").await.unwrap(), KEY);
    let e = login.finish("code-1", state, "").await.unwrap_err();
    assert!(e.is_state() && e.status == 400 && e.to_string() == onboarding::STATE_UNKNOWN);
    assert_eq!(fake.seen().len(), 1);
}

#[tokio::test]
async fn test_a_failed_exchange_uses_the_state_up_too() {
    let (login, fake, _, _) = openrouter(vec![(exchange(), reply(500, json!({"error": {"code": 500, "message": "oops"}})))]);
    let state = state_of(&login.start(OR_CALLBACK, "x").unwrap());
    assert!(!login.finish("code-1", state.clone(), "").await.unwrap_err().is_state());
    assert!(login.finish("code-1", state, "").await.unwrap_err().is_state());
    assert_eq!(fake.seen().len(), 1);
}

#[tokio::test]
async fn test_a_state_older_than_ten_minutes_is_400_and_nothing_is_sent() {
    let (login, fake, clock, _) = openrouter(vec![]);
    let first = state_of(&login.start(OR_CALLBACK, "x").unwrap());
    let second = state_of(&login.start(OR_CALLBACK, "x").unwrap());
    clock.advance(onboarding::STATE_TTL_SECS - 1.0);
    assert_eq!(login.finish("code-1", first, "").await.unwrap(), KEY); // control: inside the window it works
    clock.advance(2.0);
    let e = login.finish("code-1", second, "").await.unwrap_err();
    assert!(e.is_state() && e.status == 400 && e.to_string() == onboarding::STATE_EXPIRED);
    assert_eq!(fake.seen().len(), 1);
}

#[tokio::test]
async fn test_at_most_max_pending_sign_ins_wait_and_the_oldest_goes_first() {
    let (login, fake, _, _) = openrouter(vec![]);
    let states: Vec<String> =
        (0..onboarding::MAX_PENDING + 1).map(|_| state_of(&login.start(OR_CALLBACK, "x").unwrap())).collect();
    assert!(login.finish("code-1", states[0].clone(), "").await.unwrap_err().is_state());
    assert!(fake.seen().is_empty());
    assert_eq!(login.finish("code-1", states[states.len() - 1].clone(), "").await.unwrap(), KEY);
    assert_eq!(login.finish("code-1", states[1].clone(), "").await.unwrap(), KEY);
}

#[tokio::test]
async fn test_one_sign_ins_state_is_no_good_for_the_other() {
    let dir = tmp();
    let (login, fake, _, _) = openrouter(vec![]);
    let (connect, cfake, _, _) = composio(dir.path(), vec![]);
    let e = connect.finish("code-1", state_of(&login.start(OR_CALLBACK, "x").unwrap()), "").await.unwrap_err();
    assert!(e.is_state());
    let url = connect.start(CP_CALLBACK).await.unwrap();
    assert!(login.finish("code-1", state_of(&url), "").await.unwrap_err().is_state());
    assert!(fake.seen().is_empty() && cfake.to(&token_route()).is_empty());
}

// ==== OpenRouter: failures are plain lines ========================================================================

#[tokio::test]
async fn test_a_403_and_an_expired_code_each_give_their_own_plain_line() {
    for (message, line) in [
        ("Invalid code or code_verifier", onboarding::OPENROUTER_REFUSED),
        ("Authorization code expired", onboarding::OPENROUTER_CODE_EXPIRED),
    ] {
        let (login, _, _, _) =
            openrouter(vec![(exchange(), reply(403, json!({"error": {"code": 403, "message": message}})))]);
        let state = state_of(&login.start(OR_CALLBACK, "x").unwrap());
        let e = login.finish("code-1", state, "").await.unwrap_err();
        assert_eq!(e.to_string(), line);
        assert!(!e.is_state());
        assert!(!line.contains('\n') && !line.contains(message) && !line.contains('{'));
    }
    assert_ne!(onboarding::OPENROUTER_REFUSED, onboarding::OPENROUTER_CODE_EXPIRED);
}

#[tokio::test]
async fn test_every_other_failure_is_a_plain_line() {
    let cases = vec![
        (
            reply(500, json!({"error": {"code": 500, "message": "Internal Server Error"}})),
            onboarding::OPENROUTER_FAILED.replace("{status}", "500"),
        ),
        (
            reply(400, json!({"error": {"code": 400, "message": "Invalid code_challenge_method"}})),
            onboarding::OPENROUTER_REFUSED.to_string(),
        ),
        (reply(200, json!({"user_id": "user-not-real"})), onboarding::OPENROUTER_NO_KEY.to_string()),
        (reply(200, json!({"key": "two words"})), onboarding::OPENROUTER_NO_KEY.to_string()),
        (raw(200, "<html>not json</html>"), onboarding::OPENROUTER_NO_KEY.to_string()),
        (Route::Raise("ConnectError"), onboarding::OPENROUTER_UNREACHABLE.to_string()),
        (Route::Raise("ReadTimeout"), onboarding::OPENROUTER_UNREACHABLE.to_string()),
    ];
    for (route, line) in cases {
        let (login, _, _, _) = openrouter(vec![(exchange(), route)]);
        let state = state_of(&login.start(OR_CALLBACK, "x").unwrap());
        let e = login.finish("code-1", state, "").await.unwrap_err();
        assert_eq!(e.to_string(), line);
        assert_eq!(e.status, 502);
    }
}

#[tokio::test]
async fn test_a_key_with_stray_whitespace_around_it_is_trimmed() {
    let (login, _, _, _) = openrouter(vec![(exchange(), reply(200, json!({"key": format!("  {KEY}\n")})))]);
    let state = state_of(&login.start(OR_CALLBACK, "x").unwrap());
    assert_eq!(login.finish("code-1", state, "").await.unwrap(), KEY);
}

#[tokio::test]
async fn test_a_denied_or_codeless_callback_sends_nothing() {
    let long = "x".repeat(onboarding::MAX_CODE_CHARS + 1);
    for (code, error) in [(json!(""), ""), (Value::Null, ""), (json!("code-1"), "access_denied"), (json!(long), "")] {
        let (login, fake, _, _) = openrouter(vec![]);
        let state = state_of(&login.start(OR_CALLBACK, "x").unwrap());
        let e = login.finish(code, state, error).await.unwrap_err();
        assert_eq!(e.status, 400);
        assert_eq!(e.to_string(), onboarding::OPENROUTER_NOT_AUTHORIZED);
        assert!(fake.seen().is_empty());
    }
}

// ==== OpenRouter: keeping the key =================================================================================

#[tokio::test]
async fn test_success_writes_the_key_0600_and_keeps_the_other_lines() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tmp();
    let f = dir.path().join("env");
    std::fs::write(&f, "# mine\nEVE_PROFILE=lite\nOTHER=1\n").unwrap();
    std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o644)).unwrap();
    let (login, _, _, _) = openrouter(vec![]);
    let state = state_of(&login.start(OR_CALLBACK, "x").unwrap());
    let key = login.finish("code-1", state, "").await.unwrap();
    let mut env: HashMap<String, String> = HashMap::new();
    let (hide, _) = recorder();
    save_openrouter_key_with(&key, Some(&f), &mut env, &hide).unwrap();
    assert_eq!(std::fs::read_to_string(&f).unwrap(), format!("# mine\nEVE_PROFILE=lite\nOTHER=1\nOPENROUTER_API_KEY={KEY}\n"));
    assert_eq!(mode(&f), 0o600);
    assert_eq!(env, HashMap::from([("OPENROUTER_API_KEY".to_string(), KEY.to_string())]));
}

#[test]
fn test_a_new_key_replaces_the_old_one() {
    let dir = tmp();
    let f = dir.path().join("env");
    std::fs::write(&f, "EVE_PROFILE=lite\nOPENROUTER_API_KEY=old-value-here\n").unwrap();
    let mut env = HashMap::from([("OPENROUTER_API_KEY".to_string(), "old-value-here".to_string())]);
    let (hide, _) = recorder();
    save_openrouter_key_with(KEY, Some(&f), &mut env, &hide).unwrap();
    assert_eq!(std::fs::read_to_string(&f).unwrap(), format!("EVE_PROFILE=lite\nOPENROUTER_API_KEY={KEY}\n"));
    assert_eq!(env["OPENROUTER_API_KEY"], KEY);
}

#[test]
fn test_without_a_path_the_key_goes_to_the_env_file_the_daemon_reads() {
    // EVE_ENV_FILE wins, as for the loader (the HOME half of the Python test sets the process's HOME, which
    // parallel tests must not do; envfile::env_path's own tests cover the default)
    let dir = tmp();
    let shared = dir.path().join("shared-env");
    let mut env = HashMap::from([("EVE_ENV_FILE".to_string(), shared.display().to_string())]);
    let (hide, _) = recorder();
    save_openrouter_key_with(KEY, None, &mut env, &hide).unwrap();
    assert_eq!(std::fs::read_to_string(&shared).unwrap(), format!("OPENROUTER_API_KEY={KEY}\n"));
    assert_eq!(mode(&shared), 0o600);
    let mut loaded: HashMap<String, String> = HashMap::new();
    eve_envfile::load_env_file(Some(&shared), &mut loaded).unwrap();
    assert_eq!(loaded, HashMap::from([("OPENROUTER_API_KEY".to_string(), KEY.to_string())]));
    assert_eq!(env["OPENROUTER_API_KEY"], KEY);
}

#[test]
fn test_save_refuses_what_is_not_a_key_and_writes_nothing() {
    let bad_line = format!("{KEY}\nEVE_PROFILE=full");
    for bad in ["", "short", "two words here", bad_line.as_str()] {
        let dir = tmp();
        let f = dir.path().join("env");
        let mut env: HashMap<String, String> = HashMap::new();
        let (hide, hidden) = recorder();
        let e = save_openrouter_key_with(bad, Some(&f), &mut env, &hide).unwrap_err();
        assert!(matches!(e, SignInError::Value(_)));
        assert!(!f.exists() && env.is_empty() && hidden.lock().is_empty());
        assert!(!e.to_string().contains(KEY));
    }
}

#[test]
fn test_a_save_that_cannot_write_is_a_plain_line() {
    use std::os::unix::fs::PermissionsExt;
    if is_root() {
        return; // root ignores folder permissions
    }
    let dir = tmp();
    let folder = dir.path().join("locked");
    std::fs::create_dir(&folder).unwrap();
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o500)).unwrap();
    let mut env: HashMap<String, String> = HashMap::new();
    let (hide, _) = recorder();
    let result = save_openrouter_key_with(KEY, Some(&folder.join("env")), &mut env, &hide);
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700)).unwrap();
    let e = match result.unwrap_err() {
        SignInError::Onboarding(e) => e,
        other => panic!("not an OnboardingError: {other:?}"),
    };
    assert_eq!(e.status, 500);
    assert!(!e.to_string().contains(KEY) && env.is_empty());
    assert_eq!(std::fs::read_dir(&folder).unwrap().count(), 0);
}

/// Rust: the log redaction is the lent `hide` (telegram::hide_token in eve); what is handed to it is hidden from
/// every log line from then on (telegram_wire's redaction tests).
#[tokio::test]
async fn test_the_key_is_hidden_from_every_log_record_the_moment_it_arrives() {
    let (login, _, _, hidden) = openrouter(vec![]);
    assert!(hidden.lock().is_empty()); // control: nothing before the key arrives
    let state = state_of(&login.start(OR_CALLBACK, "x").unwrap());
    login.finish("code-1", state, "").await.unwrap();
    assert_eq!(*hidden.lock(), vec![KEY.to_string()]);
}

// ==== OpenRouter: credit ==========================================================================================

fn key_info(changes: Value) -> Value {
    let mut info = json!({"label": "eve lite on test-mac", "limit": null, "limit_remaining": null, "limit_reset": null,
                          "is_free_tier": false, "usage": 0.5, "usage_daily": 0.5});
    for (k, v) in changes.as_object().unwrap() {
        info[k] = v.clone();
    }
    json!({"data": info})
}

fn account(bought: f64, used: f64) -> Value {
    json!({"data": {"total_credits": bought, "total_usage": used}})
}

async fn credit(key: &str, fake: &Arc<Provider>) -> Value {
    let (hide, _) = recorder();
    Value::Object(credit_with(key, Some(fake.http()), &hide).await)
}

#[tokio::test]
async fn test_credit_is_the_account_balance_read_with_the_key_as_a_bearer() {
    let fake = Provider::new(vec![
        (key_info_route(), reply(200, key_info(json!({})))),
        (credits_route(), reply(200, account(10.0, 2.5))),
    ]);
    assert_eq!(credit(KEY, &fake).await, json!({"ok": true, "usd": 7.5, "funded": true}));
    let mut urls: Vec<String> = fake.seen().iter().map(|r| r.url.clone()).collect();
    urls.sort();
    let mut want = vec![eve_spend_sync::OPENROUTER_KEY_URL.to_string(), eve_spend_sync::OPENROUTER_CREDITS_URL.to_string()];
    want.sort();
    assert_eq!(urls, want);
    for request in fake.seen() {
        assert_eq!(request.method, "GET");
        assert_eq!(request.header("authorization"), Some(format!("Bearer {KEY}").as_str()));
        assert!(request.body.is_empty() && !request.url.contains(KEY));
    }
}

#[tokio::test]
async fn test_credit_says_what_the_key_can_spend() {
    let cases = vec![
        (key_info(json!({})), (200, account(5.0, 5.0)), json!({"ok": true, "usd": 0.0, "funded": false})),
        (key_info(json!({})), (200, account(5.0, 5.5)), json!({"ok": true, "usd": 0.0, "funded": false})), // owing reads $0
        (key_info(json!({})), (200, account(1.0, 0.996)), json!({"ok": true, "usd": 0.0, "funded": false})), // under a cent
        (
            key_info(json!({"limit": 2.0, "limit_remaining": 1.25})),
            (200, account(10.0, 2.5)),
            json!({"ok": true, "usd": 1.25, "funded": true}),
        ),
        // /credits needs a management key (OpenRouter's docs), which an OAuth key is not:
        (key_info(json!({"is_free_tier": true})), (403, management_only()), json!({"ok": true, "usd": null, "funded": false})),
        (key_info(json!({})), (403, management_only()), json!({"ok": true, "usd": null, "funded": null})),
        (
            key_info(json!({"limit": 3.0, "limit_remaining": 3.0})),
            (403, management_only()),
            json!({"ok": true, "usd": 3.0, "funded": true}),
        ),
        (key_info(json!({})), (200, json!({"data": {"total_credits": "lots"}})), json!({"ok": true, "usd": null, "funded": null})),
    ];
    for (info, (status, balance), shown) in cases {
        let fake = Provider::new(vec![(key_info_route(), reply(200, info)), (credits_route(), reply(status, balance))]);
        assert_eq!(credit(KEY, &fake).await, shown);
    }
}

#[tokio::test]
async fn test_credit_never_raises_and_says_why() {
    let cases = vec![
        (reply(401, json!({"error": {"code": 401, "message": "User not found."}})), "HTTP 401"),
        (raw(200, "<html>"), "unreadable reply"),
        (reply(200, json!({"data": "nope"})), "unreadable reply"),
        (Route::Raise("ConnectError"), "unreachable"),
        (Route::Raise("ConnectTimeout"), "timed out"),
    ];
    for (route, why) in cases {
        let fake = Provider::new(vec![(key_info_route(), route), (credits_route(), reply(200, account(10.0, 2.5)))]);
        let mut expected = json!({"ok": false, "usd": null, "funded": null, "error": why});
        if why == "HTTP 401" {
            expected["rejected"] = json!(true); // a 401 is the one error that means "sign the key in again"
        }
        assert_eq!(credit(KEY, &fake).await, expected);
    }
}

#[tokio::test]
async fn test_credit_reads_a_figure_that_is_not_a_finite_number_as_unknown() {
    let huge = format!("1{}", "0".repeat(400)); // a JSON integer no float can hold
    let cases = vec![
        (
            "{\"data\": {\"limit_remaining\": null, \"is_free_tier\": false}}".to_string(),
            "{\"data\": {\"total_credits\": Infinity, \"total_usage\": 1.0}}".to_string(),
            json!({"ok": true, "usd": null, "funded": null}),
        ),
        (
            "{\"data\": {\"limit_remaining\": NaN, \"is_free_tier\": true}}".to_string(),
            "{\"data\": {\"total_credits\": 5.0, \"total_usage\": NaN}}".to_string(),
            json!({"ok": true, "usd": null, "funded": false}),
        ),
        (
            format!("{{\"data\": {{\"limit_remaining\": {huge}, \"is_free_tier\": false}}}}"),
            format!("{{\"data\": {{\"total_credits\": {huge}, \"total_usage\": 1.0}}}}"),
            json!({"ok": true, "usd": null, "funded": null}),
        ),
        (
            "{\"data\": {\"limit_remaining\": 2.0, \"is_free_tier\": false}}".to_string(),
            "{\"data\": {\"total_credits\": Infinity, \"total_usage\": 1.0}}".to_string(),
            json!({"ok": true, "usd": 2.0, "funded": true}),
        ),
    ];
    for (info, balance, shown) in cases {
        let fake = Provider::new(vec![(key_info_route(), raw(200, &info)), (credits_route(), raw(200, &balance))]);
        let result = credit(KEY, &fake).await;
        assert_eq!(result, shown);
        serde_json::to_string(&result).unwrap(); // the page's JSON stays strict
    }
}

#[tokio::test]
async fn test_credit_never_raises_even_on_a_closed_client() {
    let fake = Provider::new(vec![
        (key_info_route(), reply(200, key_info(json!({})))),
        (credits_route(), reply(200, account(10.0, 2.5))),
    ]);
    assert_eq!(credit(KEY, &fake).await["ok"], json!(true)); // control: the client works
    fake.closed.store(true, Ordering::SeqCst);
    assert_eq!(credit(KEY, &fake).await, json!({"ok": false, "usd": null, "funded": null, "error": "unreachable"}));
}

#[tokio::test]
async fn test_credit_hides_the_key_it_is_given() {
    let fake = Provider::new(vec![
        (key_info_route(), reply(200, key_info(json!({})))),
        (credits_route(), reply(200, account(10.0, 2.5))),
    ]);
    let (hide, hidden) = recorder();
    credit_with(&format!(" {KEY} "), Some(fake.http()), &hide).await; // a fresh process: the key came from the env file
    assert_eq!(*hidden.lock(), vec![KEY.to_string()]);
}

#[tokio::test]
async fn test_credit_without_a_key_sends_nothing() {
    let fake = Provider::new(vec![]);
    assert_eq!(credit("", &fake).await, json!({"ok": false, "usd": null, "funded": null, "error": "no key"}));
    assert_eq!(credit("   ", &fake).await, json!({"ok": false, "usd": null, "funded": null, "error": "no key"}));
    assert!(fake.seen().is_empty());
}

// ==== Composio Connect: registration and the authorize URL ========================================================

#[test]
fn test_the_pinned_endpoints_are_the_ones_composio_publishes() {
    let metadata = fixture("oauth_metadata.json");
    let server = &metadata["documents"]["https://connect.composio.dev/.well-known/oauth-authorization-server"]["body"];
    let resource = &metadata["documents"]["https://connect.composio.dev/.well-known/oauth-protected-resource"]["body"];
    assert_eq!(onboarding::CONNECT_ISSUER, server["issuer"]);
    assert_eq!(onboarding::CONNECT_AUTHORIZE_URL, server["authorization_endpoint"]);
    assert_eq!(onboarding::CONNECT_TOKEN_URL, server["token_endpoint"]);
    assert_eq!(onboarding::CONNECT_REGISTER_URL, server["registration_endpoint"]);
    assert_eq!(onboarding::CONNECT_RESOURCE, resource["resource"]);
    let scopes: Vec<Value> = onboarding::CONNECT_SCOPE.split(' ').map(|s| json!(s)).collect();
    assert_eq!(Value::Array(scopes), server["scopes_supported"]);
    assert_eq!(server["code_challenge_methods_supported"], json!(["S256"]));
    assert_eq!(server["token_endpoint_auth_methods_supported"], json!(["none"]));
}

#[tokio::test]
async fn test_the_registration_body_is_the_one_s1_verified() {
    let dir = tmp();
    let (login, fake, _, _) = composio(dir.path(), vec![]);
    login.start(CP_CALLBACK).await.unwrap();
    let sent = fake.to(&register_route());
    assert_eq!(sent.len(), 1);
    let request = &sent[0];
    assert_eq!(request.header("content-type"), Some("application/json"));
    assert_eq!(request.header("authorization"), None);
    let body: Value = serde_json::from_slice(&request.body).unwrap();
    let mut want = fixture("registration.json")["request"].clone();
    want["client_name"] = json!("eve lite on test-mac");
    assert_eq!(body, want);
    assert_eq!(body["token_endpoint_auth_method"], "none");
    assert_eq!(body["redirect_uris"], json!([CP_CALLBACK, CP_127]));
}

#[tokio::test]
async fn test_the_authorize_url_has_the_resource_the_scope_and_s256() {
    let dir = tmp();
    let (login, _, _, _) = composio(dir.path(), vec![]);
    let url = login.start(CP_CALLBACK).await.unwrap();
    assert_eq!(without_query(&url), "https://connect.composio.dev/oauth/authorize");
    let q = query(&url);
    let mut keys: Vec<String> = q.keys().cloned().collect();
    keys.sort();
    let mut want: Vec<String> = fixture("authorize_probe.json")["parameters"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    want.sort();
    assert_eq!(keys, want); // exactly the keys S1 sent
    assert_eq!(q["resource"], ["https://connect.composio.dev/mcp"]);
    assert_eq!(q["scope"], ["openid profile email offline_access"]);
    assert_eq!(q["code_challenge_method"], ["S256"]);
    assert_eq!(q["response_type"], ["code"]);
    assert_eq!(q["client_id"], [client_id()]);
    assert_eq!(q["redirect_uri"], [CP_CALLBACK]);
    assert_eq!(q["code_challenge"][0].len(), 43);
    assert_eq!(unb64(&q["state"][0]).len(), 16);
    assert!(query_part(&url).contains("scope=openid%20profile%20email%20offline_access")); // S1's URL
    assert!(query_part(&url).contains("resource=https%3A%2F%2Fconnect.composio.dev%2Fmcp"));
    assert!(!login.token_path.exists()); // nothing is written before a token
}

#[tokio::test]
async fn test_composio_is_told_the_pages_own_loopback_form() {
    for (callback, told) in [
        ("http://127.0.0.1:48783/onboard/composio/callback", "http://127.0.0.1:48783/onboard/composio/callback"),
        ("http://[::1]:48783/onboard/composio/callback", "http://localhost:48783/onboard/composio/callback"),
        ("http://LOCALHOST:48783/onboard/composio/callback", "http://localhost:48783/onboard/composio/callback"),
    ] {
        let dir = tmp();
        let (login, fake, _, _) = composio(dir.path(), vec![]);
        assert_eq!(query(&login.start(callback).await.unwrap())["redirect_uri"], [told]);
        let body: Value = serde_json::from_slice(&fake.to(&register_route())[0].body).unwrap();
        assert_eq!(
            body["redirect_uris"],
            json!(["http://localhost:48783/onboard/composio/callback", "http://127.0.0.1:48783/onboard/composio/callback"])
        );
    }
}

#[test]
fn test_openrouter_is_always_told_the_localhost_form_it_documents() {
    for host in ["localhost", "127.0.0.1", "[::1]"] {
        let url = OpenRouterLogin::new(None, None).start(&format!("http://{host}:48783/onboard/openrouter/callback"), "x").unwrap();
        assert_eq!(query(&url)["callback_url"], ["http://localhost:48783/onboard/openrouter/callback"]);
    }
}

#[tokio::test]
async fn test_a_callback_off_this_mac_is_refused_before_any_request() {
    for bad in [
        "http://192.168.1.5:48781/onboard/composio/callback",
        "https://localhost:48780/onboard/composio/callback",
        "http://localhost/onboard/composio/callback",
        "http://eve.example:48780/onboard/composio/callback",
        "http://localhost:48780/onboard/composio/callback?next=x",
        "http://someone@localhost:48780/cb",
        "http://localhost:48780/cb#frag",
        "http://localhost:99999/cb",
        "localhost:48780/cb",
        "",
        // what Python 3.13's urlsplit refuses around a bracketed host, though its hostname would read as ::1
        "http://x[::1]:48780/cb",
        "http://[::1]x:48780/cb",
        "http://[127.0.0.1]:48780/cb",
        "http://[localhost]:48780/cb",
        "http://[::1:48780/cb",
    ] {
        let dir = tmp();
        let (login, fake, _, _) = composio(dir.path(), vec![]);
        assert!(matches!(login.start(bad).await, Err(SignInError::Value(_))), "{bad}");
        assert!(matches!(OpenRouterLogin::new(Some(fake.http()), None).start(bad, "x"), Err(SignInError::Value(_))));
        assert!(fake.seen().is_empty());
    }
}

#[tokio::test]
async fn test_a_second_start_reuses_the_registration() {
    let dir = tmp();
    let (login, fake, _, _) = composio(dir.path(), vec![]);
    let first = login.start(CP_CALLBACK).await.unwrap();
    let second = login.start(CP_127).await.unwrap();
    assert_eq!(fake.to(&register_route()).len(), 1);
    assert_eq!(query(&first)["client_id"], [client_id()]);
    assert_eq!(query(&second)["client_id"], [client_id()]);
    assert_ne!(state_of(&first), state_of(&second));
}

#[tokio::test]
async fn test_two_quick_clicks_register_once() {
    for locked in [true, false] {
        let dir = tmp();
        let slow = Route::Slow(0.01, Arc::new(registered_answer));
        let (login, fake, _, _) = composio(dir.path(), vec![(register_route(), slow)]);
        // control: without the lock both clicks register
        let login = if locked { login } else { login.without_lock() };
        let (a, b) = futures::join!(login.start(CP_CALLBACK), login.start(CP_CALLBACK));
        a.unwrap();
        b.unwrap();
        assert_eq!(fake.to(&register_route()).len(), if locked { 1 } else { 2 });
    }
}

#[tokio::test]
async fn test_a_new_process_reuses_the_client_kept_in_the_token_file() {
    let dir = tmp();
    let path = dir.path().join("composio-connect.json");
    let kept = json!({"version": 1, "issuer": onboarding::CONNECT_ISSUER, "client_id": "client-kept-in-file",
                      "redirect_uris": [CP_CALLBACK, CP_127], "access_token": "", "refresh_token": "",
                      "expires_at": 0, "scope": onboarding::CONNECT_SCOPE});
    std::fs::write(&path, kept.to_string()).unwrap();
    let (login, fake, _, _) = composio(dir.path(), vec![]);
    assert_eq!(query(&login.start(CP_CALLBACK).await.unwrap())["client_id"], ["client-kept-in-file"]);
    assert!(fake.to(&register_route()).is_empty());
    let other_port = "http://localhost:48784/onboard/composio/callback"; // control: not listed there
    assert_eq!(query(&login.start(other_port).await.unwrap())["client_id"], [client_id()]);
    assert_eq!(fake.to(&register_route()).len(), 1);
}

#[tokio::test]
async fn test_a_token_file_eve_cannot_vouch_for_is_not_reused() {
    let forms = json!([CP_CALLBACK, CP_127]);
    for stored in [
        json!({"version": 2, "client_id": "client-kept-in-file", "redirect_uris": forms}), // a format eve does not know
        json!({"client_id": "client-kept-in-file", "redirect_uris": forms}),               // no version at all
        json!({"version": 1, "client_id": "client kept in file", "redirect_uris": forms}), // not an id
        json!({"version": 1, "client_id": "client-kept-in-file", "redirect_uris": CP_CALLBACK}), // not a list
        json!(["not", "an", "object"]),
    ] {
        let dir = tmp();
        std::fs::write(dir.path().join("composio-connect.json"), stored.to_string()).unwrap();
        let (login, fake, _, _) = composio(dir.path(), vec![]);
        assert_eq!(query(&login.start(CP_CALLBACK).await.unwrap())["client_id"], [client_id()]);
        assert_eq!(fake.to(&register_route()).len(), 1);
    }
}

#[tokio::test]
async fn test_a_registration_that_fails_is_a_plain_line_and_starts_nothing() {
    let elsewhere = Route::Answer(Arc::new(|r: &HttpRequest| {
        let mut sent: Obj = serde_json::from_slice(&r.body).unwrap();
        sent.insert("client_id".into(), json!(client_id()));
        sent.insert("redirect_uris".into(), json!(["http://localhost:1/elsewhere"]));
        json_response(201, &Value::Object(sent))
    }));
    let cases = vec![
        (reply(400, json!({"error": "invalid_redirect_uri"})), onboarding::COMPOSIO_NOT_REGISTERED.replace("{status}", "400")),
        (reply(201, json!({"redirect_uris": [CP_CALLBACK, CP_127]})), onboarding::COMPOSIO_NOT_REGISTERED.replace("{status}", "201")),
        (elsewhere, onboarding::COMPOSIO_NO_CALLBACK.to_string()),
        (Route::Raise("ConnectError"), onboarding::COMPOSIO_UNREACHABLE.to_string()),
    ];
    for (route, line) in cases {
        let dir = tmp();
        let (login, _, _, _) = composio(dir.path(), vec![(register_route(), route)]);
        let e = match login.start(CP_CALLBACK).await.unwrap_err() {
            SignInError::Onboarding(e) => e,
            other => panic!("{other:?}"),
        };
        assert_eq!(e.to_string(), line);
        assert_eq!(e.status, 502);
        assert_eq!(login.waiting(), 0);
        assert!(!login.token_path.exists());
    }
}

// ==== Composio Connect: the callback ==============================================================================

#[tokio::test]
async fn test_a_wrong_or_missing_composio_state_is_400_and_nothing_is_sent() {
    for state in [json!(""), Value::Null, json!("not-the-state")] {
        let dir = tmp();
        let (login, fake, _, _) = composio(dir.path(), vec![]);
        login.start(CP_CALLBACK).await.unwrap();
        let e = login.finish("code-1", state, "").await.unwrap_err();
        assert!(e.is_state() && e.status == 400);
        assert!(fake.to(&token_route()).is_empty() && !login.token_path.exists());
    }
}

#[tokio::test]
async fn test_a_reused_or_expired_composio_state_is_400_and_nothing_is_sent() {
    let dir = tmp();
    let (login, fake, clock, _) = composio(dir.path(), vec![]);
    let first = login.start(CP_CALLBACK).await.unwrap();
    let second = login.start(CP_CALLBACK).await.unwrap();
    login.finish("code-1", state_of(&first), "").await.unwrap();
    assert!(login.finish("code-1", state_of(&first), "").await.unwrap_err().is_state());
    clock.advance(onboarding::STATE_TTL_SECS + 1.0);
    let e = login.finish("code-1", state_of(&second), "").await.unwrap_err();
    assert!(e.is_state());
    assert_eq!(e.to_string(), onboarding::STATE_EXPIRED);
    assert_eq!(fake.to(&token_route()).len(), 1);
}

#[tokio::test]
async fn test_the_token_exchange_is_the_public_client_form_s1_verified() {
    let dir = tmp();
    let (login, fake, _, _) = composio(dir.path(), vec![]);
    let url = login.start(CP_CALLBACK).await.unwrap();
    login.finish("code-1", state_of(&url), "").await.unwrap();
    let sent = fake.to(&token_route());
    assert_eq!(sent.len(), 1);
    let request = &sent[0];
    assert_eq!(request.header("content-type"), Some("application/x-www-form-urlencoded"));
    assert_eq!(request.header("authorization"), None); // no secret, no Basic header
    let form = query(&format!("?{}", String::from_utf8(request.body.clone()).unwrap()));
    let mut keys: Vec<String> = form.keys().cloned().collect();
    keys.sort();
    let mut want: Vec<String> = fixture("token_probe.json")["cases"][0]["request_keys"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    want.sort();
    assert_eq!(keys, want);
    assert_eq!(form["grant_type"], ["authorization_code"]);
    assert_eq!(form["code"], ["code-1"]);
    assert_eq!(form["redirect_uri"], [CP_CALLBACK]);
    assert_eq!(form["client_id"], [client_id()]);
    assert_eq!(form["resource"], ["https://connect.composio.dev/mcp"]);
    assert_eq!(query(&url)["code_challenge"], [s256(&form["code_verifier"][0])]);
}

#[tokio::test]
async fn test_success_writes_a_0600_token_file_with_exactly_the_contract_keys() {
    let dir = tmp();
    let (login, _, clock, _) = composio(dir.path(), vec![]);
    let url = login.start(CP_CALLBACK).await.unwrap();
    login.finish("code-1", state_of(&url), "").await.unwrap();
    assert_eq!(mode(&login.token_path), 0o600);
    let text = std::fs::read_to_string(&login.token_path).unwrap();
    let data: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        data,
        json!({"version": 1, "issuer": "https://connect.composio.dev", "client_id": client_id(),
               "redirect_uris": [CP_CALLBACK, CP_127], "access_token": ACCESS, "refresh_token": REFRESH,
               "expires_at": clock.now() + 300.0, "scope": "openid profile email offline_access"})
    );
    assert!(!text.contains(ID_TOKEN)); // the id token (an email) is not kept
}

#[tokio::test]
async fn test_the_expiry_comes_from_expires_in_then_the_tokens_own_exp() {
    let d = onboarding::DEFAULT_TOKEN_SECS;
    let huge = format!("1{}", "0".repeat(400));
    let s = |v: &str| Some(v.to_string());
    #[allow(clippy::type_complexity)]
    let cases: Vec<(Vec<(&str, Option<String>)>, f64)> = vec![
        (vec![("expires_in", s("\"600\""))], 600.0),
        (vec![("expires_in", s("299.5"))], 299.5),
        (vec![("expires_in", None), ("access_token", Some(jwt(&format!("{{\"exp\": {}, \"sub\": \"someone\"}}", T0 + 1234.0))))], 1234.0),
        (vec![("expires_in", None), ("access_token", Some(jwt(&format!("{{\"exp\": {}}}", T0 - 5.0))))], d),
        (vec![("expires_in", None), ("access_token", Some(jwt("{\"exp\": Infinity}")))], d),
        (vec![("expires_in", None), ("access_token", Some(jwt(&format!("{{\"exp\": {huge}}}"))))], d), // no float holds it
        (vec![("expires_in", None), ("access_token", Some(jwt(&format!("{{\"exp\": {}}}", T0 + 1e12))))], d), // 31,000 years
        (vec![("expires_in", Some(huge.clone()))], d),
        (vec![("expires_in", None), ("access_token", Some(jwt("{\"exp\": \"soon\"}")))], d),
        (vec![("expires_in", None)], d),
        (vec![("expires_in", s("0"))], d),
        (vec![("expires_in", s("-60"))], d),
        (vec![("expires_in", s("true"))], d),
        (vec![("expires_in", s("\"\u{b2}\""))], d), // isdigit() says yes, int() says no
        (vec![("expires_in", s("\"soon\""))], d),
        (vec![("expires_in", s("Infinity"))], d),
        (vec![("expires_in", s("NaN"))], d),
        (vec![("expires_in", s("1000000000000"))], d),
    ];
    for (changes, expires_in) in cases {
        let dir = tmp();
        let (login, _, clock, _) = composio(dir.path(), vec![(token_route(), granted(&changes))]);
        let url = login.start(CP_CALLBACK).await.unwrap();
        login.finish("code-1", state_of(&url), "").await.unwrap_or_else(|e| panic!("{changes:?}: {e}"));
        // serde_json refuses NaN and Infinity, as Python's parse_constant hook does here
        let data: Value = serde_json::from_str(&std::fs::read_to_string(&login.token_path).unwrap()).unwrap();
        assert_eq!(data["expires_at"].as_f64(), Some(clock.now() + expires_in), "{changes:?}");
    }
}

/// The log lines written while `f` runs, rendered.
struct Captured(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn capture_logs() -> (tracing::subscriber::DefaultGuard, Arc<Mutex<Vec<u8>>>) {
    let buf = Arc::new(Mutex::new(Vec::new()));
    let b = buf.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(move || Captured(b.clone()))
        .finish();
    (tracing::subscriber::set_default(subscriber), buf)
}

fn text_of(buf: &Arc<Mutex<Vec<u8>>>) -> String {
    String::from_utf8_lossy(&buf.lock()).into_owned()
}

#[tokio::test]
async fn test_an_answer_without_a_refresh_token_still_signs_in() {
    let dir = tmp();
    let (_guard, logs) = capture_logs();
    let (login, _, _, _) = composio(dir.path(), vec![(token_route(), granted(&[("refresh_token", None), ("scope", None)]))]);
    let url = login.start(CP_CALLBACK).await.unwrap();
    login.finish("code-1", state_of(&url), "").await.unwrap();
    let data: Value = serde_json::from_str(&std::fs::read_to_string(&login.token_path).unwrap()).unwrap();
    assert_eq!(data["refresh_token"], "");
    assert_eq!(data["access_token"], ACCESS);
    assert_eq!(data["scope"], onboarding::CONNECT_SCOPE); // the server may leave an unchanged scope out
    assert!(text_of(&logs).contains("no refresh token"));
}

#[tokio::test]
async fn test_the_token_endpoints_two_refusals_are_plain_lines() {
    let probe = fixture("token_probe.json");
    for (case, line) in [(0, onboarding::COMPOSIO_GRANT_EXPIRED), (2, onboarding::COMPOSIO_CLIENT_GONE)] {
        let p = &probe["cases"][case]; // invalid_grant, then invalid_client
        let dir = tmp();
        let (login, fake, _, _) =
            composio(dir.path(), vec![(token_route(), reply(p["status"].as_u64().unwrap() as u16, p["answer"].clone()))]);
        let url = login.start(CP_CALLBACK).await.unwrap();
        let e = login.finish("code-1", state_of(&url), "").await.unwrap_err();
        assert_eq!(e.to_string(), line);
        assert!(!login.token_path.exists());
        login.start(CP_CALLBACK).await.unwrap();
        let gone = p["answer"]["error"] == "invalid_client";
        assert_eq!(fake.to(&register_route()).len(), if gone { 2 } else { 1 }); // gone: again
    }
}

#[tokio::test]
async fn test_a_client_gone_from_the_token_file_is_registered_again() {
    let dir = tmp();
    let path = dir.path().join("composio-connect.json");
    std::fs::write(&path, json!({"version": 1, "client_id": "client-kept-in-file", "redirect_uris": [CP_CALLBACK, CP_127]}).to_string())
        .unwrap();
    let gone = fixture("token_probe.json")["cases"][2].clone();
    let (login, fake, _, _) =
        composio(dir.path(), vec![(token_route(), reply(gone["status"].as_u64().unwrap() as u16, gone["answer"].clone()))]);
    let url = login.start(CP_CALLBACK).await.unwrap();
    assert!(login.finish("code-1", state_of(&url), "").await.is_err());
    assert_eq!(query(&login.start(CP_CALLBACK).await.unwrap())["client_id"], [client_id()]);
    assert_eq!(fake.to(&register_route()).len(), 1);
}

#[tokio::test]
async fn test_a_refused_or_codeless_callback_sends_nothing() {
    let long = "x".repeat(onboarding::MAX_CODE_CHARS + 1);
    let cases = vec![
        ("", "access_denied", onboarding::COMPOSIO_CANCELLED),
        ("", "invalid_request", onboarding::COMPOSIO_NOT_FINISHED), // S1: PKCE "plain" comes back so
        ("code-1", "server_error", onboarding::COMPOSIO_NOT_FINISHED),
        ("", "", onboarding::COMPOSIO_NOT_FINISHED),
        (long.as_str(), "", onboarding::COMPOSIO_NOT_FINISHED),
    ];
    for (code, error, line) in cases {
        let dir = tmp();
        let (login, fake, _, _) = composio(dir.path(), vec![]);
        let url = login.start(CP_CALLBACK).await.unwrap();
        let e = login.finish(code, state_of(&url), error).await.unwrap_err();
        assert_eq!(e.to_string(), line);
        assert_eq!(e.status, 400);
        assert!(fake.to(&token_route()).is_empty() && !login.token_path.exists());
    }
}

#[tokio::test]
async fn test_every_other_composio_failure_is_a_plain_line() {
    let cases = vec![
        (reply(500, json!({"error": "server_error"})), onboarding::COMPOSIO_FAILED.replace("{status}", "500")),
        (reply(200, json!({"token_type": "Bearer"})), onboarding::COMPOSIO_NO_TOKEN.to_string()),
        (reply(200, json!({"access_token": "two words"})), onboarding::COMPOSIO_NO_TOKEN.to_string()),
        (Route::Raise("ConnectError"), onboarding::COMPOSIO_UNREACHABLE.to_string()),
    ];
    for (route, line) in cases {
        let dir = tmp();
        let (login, _, _, _) = composio(dir.path(), vec![(token_route(), route)]);
        let url = login.start(CP_CALLBACK).await.unwrap();
        let e = login.finish("code-1", state_of(&url), "").await.unwrap_err();
        assert_eq!(e.to_string(), line);
        assert_eq!(e.status, 502);
        assert!(!login.token_path.exists());
    }
}

#[tokio::test]
async fn test_a_token_file_that_cannot_be_written_is_a_plain_line() {
    use std::os::unix::fs::PermissionsExt;
    if is_root() {
        return; // root ignores folder permissions
    }
    let dir = tmp();
    let folder = dir.path().join("locked");
    std::fs::create_dir(&folder).unwrap();
    let (login, _, _, _) = composio(&folder, vec![]);
    let url = login.start(CP_CALLBACK).await.unwrap();
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o500)).unwrap();
    let result = login.finish("code-1", state_of(&url), "").await;
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700)).unwrap();
    let e: OnboardingError = result.unwrap_err();
    assert_eq!(e.status, 500);
    assert!(!e.to_string().contains(ACCESS));
    assert_eq!(std::fs::read_dir(&folder).unwrap().count(), 0);
}

/// Rust: the log redaction is the lent `hide` (telegram::hide_token in eve); what is handed to it is hidden from
/// every log line from then on (telegram_wire's redaction tests).
#[tokio::test]
async fn test_the_tokens_are_hidden_from_every_log_record_once_they_arrive() {
    let dir = tmp();
    let (login, _, _, hidden) = composio(dir.path(), vec![]);
    let url = login.start(CP_CALLBACK).await.unwrap();
    assert!(hidden.lock().is_empty());
    login.finish("code-1", state_of(&url), "").await.unwrap();
    assert_eq!(*hidden.lock(), vec![ACCESS.to_string(), REFRESH.to_string()]);
}

// ==== canaries: a key or token reaches nothing but its own file ===================================================

/// The files under `root` that hold `canary`.
fn files_with(canary: &str, root: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .filter(|e| std::fs::read(e.path()).map(|b| String::from_utf8_lossy(&b).contains(canary)).unwrap_or(false))
        .map(|e| e.path().to_path_buf())
        .collect();
    out.sort();
    out
}

#[tokio::test]
async fn test_a_canary_key_shows_nowhere_but_the_env_file() {
    let dir = tmp();
    let (_guard, logs) = capture_logs();
    tracing::debug!(target: "eve_brain::onboarding", "control {}", KEY); // positive control: logs
    std::fs::write(dir.path().join("control"), KEY).unwrap(); // positive control: files
    assert!(text_of(&logs).contains(&format!("control {KEY}")));
    assert_eq!(files_with(KEY, dir.path()), vec![dir.path().join("control")]);
    logs.lock().clear();
    std::fs::remove_file(dir.path().join("control")).unwrap();

    let (login, fake, _, hidden) = openrouter(vec![
        (key_info_route(), reply(200, key_info(json!({})))),
        (credits_route(), reply(403, management_only())),
    ]);
    let url = login.start(OR_CALLBACK, "").unwrap();
    let key = login.finish("code-1", state_of(&url), "").await.unwrap();
    let env_file = dir.path().join("home").join(".config").join("eve-bridge").join("env");
    let mut env = HashMap::from([("EVE_ENV_FILE".to_string(), env_file.display().to_string())]);
    let (save_hide, saved) = recorder();
    save_openrouter_key_with(&key, None, &mut env, &save_hide).unwrap();
    let mut shown = vec![credit(&key, &fake).await];
    fake.set(key_info_route(), reply(401, json!({"error": {"code": 401, "message": format!("bad key {KEY}")}})));
    shown.push(credit(&key, &fake).await);
    fake.set(exchange(), reply(403, json!({"error": {"code": 403, "message": format!("Invalid code for {KEY}")}})));
    let e = login.finish("code-2", state_of(&login.start(OR_CALLBACK, "").unwrap()), "").await.unwrap_err();

    // handed to the redaction on arrival and on save
    assert_eq!(*hidden.lock(), vec![KEY.to_string()]);
    assert_eq!(*saved.lock(), vec![KEY.to_string()]);
    assert!(!text_of(&logs).contains(KEY));
    assert_eq!(files_with(KEY, dir.path()), vec![env_file]);
    assert!(!serde_json::to_string(&shown).unwrap().contains(KEY) && !e.to_string().contains(KEY) && !url.contains(KEY));
    for request in fake.seen() {
        // sent only as the bearer of the two GETs
        assert!(!String::from_utf8_lossy(&request.body).contains(KEY) && !request.url.contains(KEY));
        if request.header("authorization").unwrap_or("").contains(KEY) {
            assert_eq!(request.method, "GET");
            assert!([eve_spend_sync::OPENROUTER_KEY_URL, eve_spend_sync::OPENROUTER_CREDITS_URL].contains(&request.url.as_str()));
        }
    }
    assert!(fake.seen().iter().any(|r| r.header("authorization").unwrap_or("").contains(KEY)));
}

#[tokio::test]
async fn test_canary_tokens_show_nowhere_but_the_token_file() {
    let dir = tmp();
    let (_guard, logs) = capture_logs();
    let id = client_id();
    tracing::debug!(target: "eve_brain::onboarding", "control {} {}", ACCESS, id);
    assert!(text_of(&logs).contains(ACCESS) && text_of(&logs).contains(&id)); // positive control
    logs.lock().clear();

    let (login, fake, _, hidden) = composio(dir.path(), vec![]);
    let url = login.start(CP_CALLBACK).await.unwrap();
    login.finish("code-1", state_of(&url), "").await.unwrap();
    fake.set(token_route(), reply(400, json!({"error": "invalid_grant", "error_description": format!("bad {REFRESH}")})));
    let url = login.start(CP_CALLBACK).await.unwrap();
    let e = login.finish("code-2", state_of(&url), "").await.unwrap_err();

    assert_eq!(*hidden.lock(), vec![ACCESS.to_string(), REFRESH.to_string()]);
    let token_file = vec![dir.path().join("composio-connect.json")];
    let logged = text_of(&logs);
    for canary in [ACCESS, REFRESH, ID_TOKEN] {
        assert!(!logged.contains(canary));
        assert_eq!(files_with(canary, dir.path()), if canary != ID_TOKEN { token_file.clone() } else { vec![] });
    }
    assert!(!logged.contains(&id)); // the client id is not logged either
    assert!(!e.to_string().contains(REFRESH));
    for request in fake.seen() {
        // eve never sends a token anywhere
        let body = String::from_utf8_lossy(&request.body).into_owned();
        let headers = format!("{:?}", request.headers);
        for c in [ACCESS, REFRESH, ID_TOKEN] {
            assert!(!body.contains(c) && !headers.contains(c));
        }
    }
}

// ==== the label ===================================================================================================

#[test]
fn urlsplit_refuses_a_bracketed_host_with_pythons_own_words() {
    // the text urllib.parse's ValueError carries (Python 3.13, the daemon's interpreter)
    for (url, said) in [
        ("http://x[::1]:8080/cb", "Invalid IPv6 URL"),
        ("http://[::1]x:8080/cb", "Invalid IPv6 URL"),
        ("http://[::1:8080/cb", "Invalid IPv6 URL"),
        ("http://[foo]:8080/cb", "'foo' does not appear to be an IPv4 or IPv6 address"),
        ("http://[127.0.0.1]:8080/cb", "An IPv4 address cannot be in brackets"),
        ("http://[vz]:80/", "IPvFuture address is invalid"),
    ] {
        assert_eq!(onboarding::SplitUrl::parse(url), Err(said.to_string()), "{url}");
    }
    for url in ["http://[::1]:8080/cb", "http://[v1.x]:80/", "http://[fe80::1%25en0]:80/"] {
        assert!(onboarding::SplitUrl::parse(url).is_ok(), "{url}");
    }
    let parts = onboarding::SplitUrl::parse("http://[::1]:48783/onboard/openrouter/callback").unwrap();
    assert_eq!((parts.hostname().as_deref(), parts.port()), (Some("::1"), Ok(Some(48783))));
    // _checknetloc: a netloc whose NFKC form gains a delimiter is refused, with the netloc in the words
    for (url, said) in [
        ("http://local\u{ff0f}host:1/cb", "netloc 'local\u{ff0f}host:1' contains invalid characters under NFKC normalization"),
        ("http://\u{2100}:1/", "netloc '\u{2100}:1' contains invalid characters under NFKC normalization"),
    ] {
        assert_eq!(onboarding::SplitUrl::parse(url), Err(said.to_string()), "{url}");
        let e = OpenRouterLogin::new(None, None).start(url, "").unwrap_err();
        assert_eq!(e, SignInError::Value(said.to_string()));
    }
    // a non-ASCII netloc NFKC leaves without a delimiter splits, and is then refused as off this Mac
    for url in ["http://loc\u{e9}lhost:1/cb", "http://\u{ff4c}ocalhost:1/cb"] {
        assert!(onboarding::SplitUrl::parse(url).is_ok(), "{url}");
    }
}

#[test]
fn a_jwt_exp_is_read_as_pythons_lenient_base64_reads_it() {
    // what Python 3.13's urlsafe_b64decode (padding added by length) makes of each payload, measured
    let b = |s: &str| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(s);
    let now = 1.9e9;
    let fallback = now + onboarding::DEFAULT_TOKEN_SECS;
    let p = b(r#"{"exp":1950000000}"#);
    for (payload, want) in [
        (b(r#"{"exp": 2000000000}"#), 2e9),
        (format!("{}==", b(r#"{"exp": 2000000000}"#)), 2e9),
        (format!("{}=={}", &p[..3], &p[3..]), 1.95e9),           // a `=` mid-way does not end the input
        ("e!yJleHAiOjIwMDAwMDAwMDB9".to_string(), 2e9),          // a character outside the alphabet is skipped
        ("eyJleHAiOjIwMDAwMDAwMDB9!!".to_string(), 2e9),
        ("eyJleHAi=OjIwMDAwMDAwMDB9".to_string(), 2e9),
        ("eyJl=eHAiOjIwMDAwMDAwMDB9".to_string(), 2e9),
        (b("\u{feff}{\"exp\": 2000000000}"), 2e9),               // json.loads of bytes drops a UTF-8 BOM
        (b(r#"{"exp": 1.95e9, "x": NaN}"#), 1.95e9),
        (b(r#"{"exp": "2000000000"}"#), fallback),
        (b(r#"{"exp": Infinity}"#), fallback),
        (b("[1]"), fallback),
        ("\u{e9}".to_string(), fallback),
        (String::new(), fallback),
    ] {
        let got = onboarding::expires_at(&Obj::new(), &format!("a.{payload}.c"), now);
        assert_eq!(got, want, "{payload}");
    }
    // padding by length cannot complete a payload whose skipped characters hide a partial quad: "Incorrect padding"
    assert_eq!(onboarding::expires_at(&Obj::new(), "a.eyJ!.c", now), fallback);
    assert_eq!(onboarding::expires_at(&Obj::new(), "a.b", now), fallback);
}

#[test]
fn test_mac_label_comes_from_the_host_name_without_starting_a_program() {
    assert_eq!(onboarding::mac_label_from(Ok("Test-Mac.local".into())), "eve lite on Test-Mac");
    assert_eq!(onboarding::mac_label_from(Ok(String::new())), "eve lite on this Mac");
    assert_eq!(onboarding::mac_label_from(Err(std::io::Error::other("no name"))), "eve lite on this Mac");
    assert!(onboarding::mac_label().starts_with("eve lite on "));
}

#[tokio::test]
async fn test_a_label_is_one_short_printable_line() {
    let label = format!("eve lite\non\u{0} a Mac {}", "x".repeat(200));
    let label = query(&OpenRouterLogin::new(None, None).start(OR_CALLBACK, &label).unwrap())["key_label"][0].clone();
    assert!(label.starts_with("eve lite on a Mac x") && label.chars().count() <= onboarding::LABEL_MAX);
    assert!(label.chars().all(eve_lights::py::is_printable));
    assert_eq!(query(&OpenRouterLogin::new(None, None).start(OR_CALLBACK, "").unwrap())["key_label"], [onboarding::mac_label()]);
    let dir = tmp();
    let fake = Provider::new(vec![(register_route(), registered())]);
    let login = ComposioConnectLogin::new(Some(&dir.path().join("composio-connect.json")), "", Some(fake.http()), None);
    login.start(CP_CALLBACK).await.unwrap();
    assert_eq!(login.client_name(), onboarding::mac_label());
}

// ==== think =======================================================================================================

const CLOCK: &str = "\n\nSystem context:\nLocal clock when this context was generated: fixed.";

fn think_config(model: &str, engine: &str) -> ThinkConfig {
    ThinkConfig {
        model: model.into(),
        search: SearchConfig { engine: engine.into(), ..SearchConfig::default() },
        ..ThinkConfig::default()
    }
}

fn obj(v: Value) -> Obj {
    v.as_object().unwrap().clone()
}

/// A `create` that answers from `replies` in turn and keeps every request.
fn scripted(replies: Vec<Value>) -> (eve_brain::prelude::Create, Arc<Mutex<Vec<Value>>>) {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let queue = Arc::new(Mutex::new(std::collections::VecDeque::from(replies)));
    let r = requests.clone();
    let create: eve_brain::prelude::Create = Arc::new(move |req: Value| {
        r.lock().push(req);
        let next = queue.lock().pop_front();
        Box::pin(async move { next.ok_or_else(|| anyhow::anyhow!("no more replies")) })
    });
    (create, requests)
}

fn search_call(query: &str, id: &str) -> Value {
    json!({"status": "completed", "output": [
        {"type": "function_call", "name": "web_search", "call_id": id, "arguments": format!("{{\"query\": \"{query}\"}}")}]})
}

fn said(text: &str) -> Value {
    json!({"status": "completed", "output": [
        {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": text}]}]})
}

#[test]
fn test_request_reasons_hard_searches_the_web_and_keeps_nothing() {
    let cfg = think_config("gpt-6-astra", "openrouter-exa");
    let body = request_with_clock(&cfg, &question_items("  what is\n the  answer "), "", false, CLOCK);
    assert_eq!(body["model"], "gpt-6-astra");
    assert_eq!(body["reasoning"], json!({"effort": "high"}));
    assert_eq!(body["tools"], json!([*websearch::WEB_SEARCH_TOOL])); // Exa through OpenRouter
    assert_eq!(body["tool_choice"], "auto");
    assert_eq!(body["store"], json!(false));
    assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
    assert_eq!(
        body["input"],
        json!([{"type": "message", "role": "user", "content": [{"type": "input_text", "text": "what is the answer"}]}])
    );
    assert!(body["instructions"].as_str().unwrap().starts_with(INSTRUCTIONS) && INSTRUCTIONS.contains("spoken"));
    // without an OpenRouter key the hosted search is offered, so nothing goes dark
    let hosted = request_with_clock(&think_config("m", "openai"), &question_items("q"), "", false, CLOCK);
    assert_eq!(hosted["tools"], json!([{"type": "web_search"}]));
    // the real clock block goes last
    let real = think::request(&think_config("m", "openai"), &question_items("q"), "", false);
    assert!(real["instructions"].as_str().unwrap().contains("System context:"));
}

#[tokio::test]
async fn test_a_think_that_searches_gets_the_sources_and_answers_in_a_later_round() {
    let (create, requests) = scripted(vec![
        json!({"status": "completed", "output": [
            {"type": "reasoning", "id": "rs1", "encrypted_content": "abc"},
            {"type": "function_call", "name": "web_search", "call_id": "c1", "arguments": "{\"query\": \"lakers score\"}"}]}),
        said("Lakers lost 104 to 112."),
    ]);
    let searched = Arc::new(Mutex::new(Vec::new()));
    let s = searched.clone();
    let search: think::SearchFn = Arc::new(move |q: &str| {
        s.lock().push(q.to_string());
        obj(json!({"ok": true, "answer": "Warriors 112, Lakers 104", "sources": [{"url": "https://espn.com"}]}))
    });
    let cfg = think_config("m", "openrouter-exa");
    let thinker = OpenAIThinker::new(&cfg, None, Some(create), Some(search.clone())).unwrap();
    let out = thinker.call("who won the lakers game", "").await.unwrap();
    assert_eq!(Value::Object(out), json!({"ok": true, "answer": "Lakers lost 104 to 112."}));
    assert_eq!(*searched.lock(), vec!["lakers score".to_string()]);
    let second = requests.lock()[1]["input"].clone();
    let kinds: Vec<&str> = second.as_array().unwrap().iter().map(|i| i["type"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["message", "reasoning", "function_call", "function_call_output"]);
    let last = &second.as_array().unwrap()[3];
    let output: Value = serde_json::from_str(last["output"].as_str().unwrap()).unwrap();
    assert_eq!(output["sources"], json!([{"url": "https://espn.com"}]));
    assert_eq!(last["call_id"], "c1");
    // a model that only ever searches is cut off after MAX_SEARCH_ROUNDS and the empty answer is an error
    let forever: eve_brain::prelude::Create = Arc::new(|_req: Value| Box::pin(async { Ok(search_call("x", "c")) }));
    let thinker = OpenAIThinker::new(&cfg, None, Some(forever), Some(search)).unwrap();
    let e = thinker.call("q", "").await.unwrap_err();
    assert!(e.to_string().starts_with("empty answer (status=completed, incomplete=None)"), "{e}");
}

#[tokio::test]
async fn test_the_last_round_may_not_search_again() {
    // With tool_choice auto on the last round the model could ask for one more search, and the think ended in
    // "empty answer". The last round must answer from what it already found.
    let mut replies: Vec<Value> = (0..think::MAX_SEARCH_ROUNDS).map(|_| search_call("x", "c")).collect();
    replies.push(said("done"));
    let (create, requests) = scripted(replies);
    let cfg = think_config("m", "openrouter-exa");
    let search: think::SearchFn = Arc::new(|_q: &str| obj(json!({"ok": true})));
    let thinker = OpenAIThinker::new(&cfg, None, Some(create), Some(search)).unwrap();
    let out = thinker.call("q", "").await.unwrap();
    assert_eq!(out["answer"], "done");
    assert_eq!(requests.lock().len(), think::MAX_SEARCH_ROUNDS + 1);
    let choices: Vec<Value> = requests.lock().iter().map(|r| r["tool_choice"].clone()).collect();
    let mut want = vec![json!("auto"); think::MAX_SEARCH_ROUNDS];
    want.push(json!("none"));
    assert_eq!(choices, want);
}

fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

#[test]
fn test_configured_defaults_to_astra_at_high_effort() {
    let cfg = configured(Some(&env(&[])), "gpt-5-mini"); // thinking is astra, whatever voice uses
    assert_eq!(
        cfg,
        ThinkConfig {
            enabled: true,
            model: "gpt-6-astra".into(),
            effort: "high".into(),
            timeout_secs: 90.0,
            search: websearch::configured(&eve_web::Ctx::global().clone().with_env(env(&[]))), // no key: engine off
            about: String::new(),
        }
    );
    assert_eq!(cfg.search.engine, "off");
    assert_eq!(configured(Some(&env(&[("OPENROUTER_API_KEY", "r")])), "m").search.engine, "openrouter-perplexity");
    let cfg = configured(
        Some(&env(&[("EVE_THINK_MODEL", "gpt-5.5"), ("EVE_THINK_EFFORT", "xhigh"), ("EVE_THINK_TIMEOUT_SECS", "120")])),
        "gpt-6-astra",
    );
    assert_eq!((cfg.model.as_str(), cfg.effort.as_str(), cfg.timeout_secs), ("gpt-5.5", "xhigh", 120.0));
    // nonsense falls back, and is clamped
    let cfg = configured(Some(&env(&[("EVE_THINK_EFFORT", "max"), ("EVE_THINK_TIMEOUT_SECS", "1")])), "m");
    assert_eq!((cfg.effort.as_str(), cfg.timeout_secs), ("high", 10.0));
    let cfg = configured(Some(&env(&[("EVE_THINK_TIMEOUT_SECS", "soon")])), "m");
    assert_eq!(cfg.timeout_secs, 90.0);
    assert!(!configured(Some(&env(&[("EVE_THINK", "0")])), "m").enabled);
}

#[test]
fn test_make_thinker_is_none_without_a_key_or_when_off() {
    assert!(make_openai_thinker(&think_config("m", "off"), Some(&env(&[]))).is_none()); // no key
    let off = ThinkConfig { enabled: false, ..think_config("m", "off") };
    assert!(make_openai_thinker(&off, Some(&env(&[("OPENROUTER_API_KEY", "k")]))).is_none());
    let thinker = make_openai_thinker(&think_config("m", "off"), Some(&env(&[("OPENROUTER_API_KEY", "k")])));
    assert_eq!(thinker.expect("a thinker").config.model, "m");
    assert!(think::make_thinker(&think_config("m", "off"), Some(&env(&[("OPENROUTER_API_KEY", "k")]))).is_some());
}

#[test]
fn test_parse_answer_flattens_and_clips() {
    assert_eq!(parse_answer("  Forty-two.\n\nBecause.  ").unwrap(), "Forty-two. Because.");
    let long = parse_answer(&"x ".repeat(ANSWER_MAX_CHARS)).unwrap();
    assert_eq!(long.chars().count(), ANSWER_MAX_CHARS);
    assert!(long.ends_with('…'));
    assert!(parse_answer("   ").is_err());
}

#[test]
fn test_no_context_keeps_the_body_byte_identical() {
    let cfg = think_config("m", "openai");
    let items = question_items("q");
    let plain = request_with_clock(&cfg, &items, "", false, CLOCK);
    assert_eq!(plain["instructions"], format!("{INSTRUCTIONS}{CLOCK}")); // the context-blind body, as before
    for empty in ["", "   \n  "] {
        assert_eq!(
            serde_json::to_string(&request_with_clock(&cfg, &items, empty, false, CLOCK)).unwrap(),
            serde_json::to_string(&plain).unwrap()
        );
    }
    assert_eq!(plain["store"], json!(false));
}

#[test]
fn test_context_goes_after_the_instructions_before_the_clock_and_is_capped() {
    let cfg = think_config("m", "openai");
    let body = request_with_clock(&cfg, &question_items("plan my week"), "Owner works late on weekdays.", false, CLOCK);
    let instr = body["instructions"].as_str().unwrap().to_string();
    assert_eq!(instr, format!("{INSTRUCTIONS}{CONTEXT_HEADER}Owner works late on weekdays.{CLOCK}"));
    assert!(instr.find(INSTRUCTIONS).unwrap() < instr.find("works late").unwrap());
    assert!(instr.find("works late").unwrap() < instr.find("System context:").unwrap());
    assert_eq!(body["store"], json!(false));
    assert_eq!(body["input"], Value::Array(question_items("plan my week").into_iter().map(Value::Object).collect()));
    // a huge context is capped at CONTEXT_MAX_CHARS; the head is kept
    let huge = request_with_clock(&cfg, &question_items("q"), &format!("A{}TAIL", "x".repeat(20000)), false, CLOCK);
    let huge = huge["instructions"].as_str().unwrap().to_string();
    let ctx = &huge[INSTRUCTIONS.len() + CONTEXT_HEADER.len()..huge.len() - CLOCK.len()];
    assert_eq!(ctx.chars().count(), CONTEXT_MAX_CHARS);
    assert!(ctx.starts_with("Ax") && ctx.ends_with('…') && !huge.contains("TAIL"));
}

#[tokio::test]
async fn test_the_thinker_sends_the_context_every_round() {
    let (create, requests) = scripted(vec![search_call("x", "c1"), said("Tuesday."), said("Yes.")]);
    let cfg = think_config("m", "openrouter-exa");
    let search: think::SearchFn = Arc::new(|_q: &str| obj(json!({"ok": true})));
    let thinker = OpenAIThinker::new(&cfg, None, Some(create), Some(search))
        .unwrap()
        .with_clock(Arc::new(|| CLOCK.to_string()));
    let out = thinker.call("which day suits me", "Owner is free on Tuesdays.").await.unwrap();
    assert_eq!(Value::Object(out), json!({"ok": true, "answer": "Tuesday."}));
    let seen: Vec<String> = requests.lock().iter().map(|r| r["instructions"].as_str().unwrap().to_string()).collect();
    assert_eq!(seen.len(), 2);
    assert!(seen.iter().all(|s| s.contains("free on Tuesdays")));
    assert!(requests.lock().iter().all(|r| r["store"] == json!(false)));
    // and a caller that passes no context still works, context-blind
    requests.lock().clear();
    assert_eq!(thinker.call("q", "").await.unwrap()["answer"], "Yes.");
    let seen: Vec<Value> = requests.lock().iter().map(|r| r["instructions"].clone()).collect();
    assert_eq!(seen, vec![json!(format!("{INSTRUCTIONS}{CLOCK}"))]);
}

#[test]
fn test_parse_calls_reads_the_searches_the_text_and_what_to_carry() {
    let (calls, text, carry) = think::parse_calls(&json!([
        {"type": "reasoning", "id": "rs1"},
        {"type": "function_call", "name": "web_search", "call_id": "c1", "arguments": "not json"},
        {"type": "function_call", "name": "other_tool", "call_id": "c2", "arguments": "{}"},
        {"type": "message", "content": [{"type": "output_text", "text": "a"}, {"type": "output_text", "text": ""},
                                        {"type": "refusal", "text": "no"}, {"type": "output_text", "text": "b"}]},
        "not an item",
    ]));
    assert_eq!(calls, vec![obj(json!({"call_id": "c1", "query": ""}))]);
    assert_eq!(text, "a\nb");
    assert_eq!(carry.len(), 2);
    assert_eq!(think::parse_calls(&json!("nothing")), (vec![], String::new(), vec![]));
}

// ==== self_context ================================================================================================

const SHA: &str = "abc1234";

/// The owner's environment's shape (switches and models as in their env file), with placeholder keys.
fn owner_env(extra: &[(&str, &str)]) -> HashMap<String, String> {
    let mut e = env(&[
        ("OPENAI_API_KEY", "test-openai"),
        ("OPENROUTER_API_KEY", "test-openrouter"),
        ("ANTHROPIC_API_KEY", "test-anthropic"),
        ("EVE_TELEGRAM", "1"),
        ("EVE_TELEGRAM_TOKEN", "test-token"),
        ("EVE_TELEGRAM_OWNER", "1"),
        ("EVE_TELEGRAM_MODEL", "gpt-6-luna"),
        ("EVE_THINK_MODEL", "gpt-6-astra"),
        ("EVE_WEB_SEARCH", "openrouter-perplexity"),
        ("EVE_MEMORY", "1"),
        ("EVE_MEM0", "1"),
        ("EVE_SECOND_BRAIN", "1"),
        ("EVE_MINIAPP", "1"),
        ("EVE_VOICE", "0"),
        ("EVE_WATCH_TLS", "0"),
        ("EVE_WATCH_BROWSER", "0"),
    ]);
    for (k, v) in extra {
        e.insert(k.to_string(), v.to_string());
    }
    e
}

/// A Govee and a HappyLighting light, in a temporary lights file.
fn lights_file(dir: &Path) -> PathBuf {
    let path = dir.join("lights.json");
    let lamp = eve_lights::Light {
        name: "floor lamp".into(),
        kind: "govee".into(),
        ip: "192.0.2.10".into(),
        device: "AA:BB".into(),
        sku: "H6008".into(),
        ..eve_lights::Light::default()
    };
    let strip = eve_lights::Light { name: "desk strip".into(), kind: "triones".into(), address: "uuid-1".into(), ..Default::default() };
    eve_lights::save([&lamp, &strip], Some(&path)).unwrap();
    path
}

/// The search router as the Python fixture `no_router` leaves it: available() lists nothing.
fn no_router() -> Option<Router> {
    Some(Router { available: vec![], shipped: eve_web::search_router::SHIPPED })
}

fn fake_router(providers: &[&str], shipped: bool) -> Option<Router> {
    Some(Router { available: providers.iter().map(|p| p.to_string()).collect(), shipped })
}

fn lent(lights: Option<&Path>, commit: &str, router: Option<Router>) -> Lent {
    Lent { lights_path: lights.map(Path::to_path_buf), commit: Some(commit.to_string()), router, connect_file: None }
}

fn lines(env: &HashMap<String, String>, lent: &Lent) -> Vec<String> {
    self_context::lines_lent(Some(env), lent).unwrap()
}

#[test]
fn test_the_owners_setup_is_described_line_by_line() {
    let dir = tmp();
    let lights = lights_file(dir.path());
    let text = self_context::block_lent(Some(&owner_env(&[])), &lent(Some(&lights), SHA, no_router()));
    assert!(text.starts_with(self_context::HEADER) && text.ends_with(self_context::RULE), "{text}");
    let body: Vec<&str> = text.lines().collect();
    assert!(body.contains(&"- Telegram brain (texts, calls, desk push-to-talk): gpt-6-luna, low effort."));
    assert!(body.contains(&"- think_hard: gpt-6-astra, high effort."));
    let search = format!("- Web search: Perplexity through OpenRouter; {} writes the answer.", websearch::DEFAULT_MODEL);
    assert!(body.contains(&search.as_str()));
    assert!(body.contains(&"- Lights: Govee and HappyLighting."));
    let commit = format!("- Running code: git commit {SHA}.");
    assert!(body.contains(&commit.as_str()));
    assert!(body.iter().any(|ln| ln.starts_with("- Memory: day transcripts, records and profile")));
    assert!(body.contains(&"- Doors on: Telegram, the Mini App and the Meet notetaker."));
    assert!(!body.iter().any(|ln| ln.starts_with("- Voice:"))); // EVE_VOICE=0: no voice line, no door
}

#[test]
fn test_a_different_environment_gives_different_lines() {
    let dir = tmp();
    let lights = lights_file(dir.path());
    let base = lines(&owner_env(&[]), &lent(Some(&lights), SHA, no_router()));
    let changed_env = owner_env(&[
        ("EVE_TELEGRAM_MODEL", "gpt-6-astra"),
        ("EVE_TELEGRAM_EFFORT", "medium"),
        ("EVE_WEB_SEARCH", "openai"),
        ("EVE_WATCH", "0"),
        ("EVE_MEMORY", "0"),
        ("EVE_SECOND_BRAIN", "0"),
        ("EVE_VOICE", "1"),
        ("EVE_THINK", "0"),
    ]);
    let none = dir.path().join("none.json");
    let changed = lines(&changed_env, &lent(Some(&none), "", no_router()));
    assert_ne!(base, changed);
    let has = |ls: &[String], l: &str| ls.iter().any(|x| x == l);
    assert!(has(&changed, "Telegram brain (texts, calls, desk push-to-talk): gpt-6-astra, medium effort."));
    assert!(has(&changed, "Web search: OpenAI's hosted web search."));
    assert!(has(&changed, "Watcher: off.") && has(&changed, "Memory: off.") && has(&changed, "Lights: none set up."));
    assert!(!changed.iter().any(|ln| ln.starts_with("think_hard") || ln.starts_with("Running code")));
    let voice: Vec<&String> = changed.iter().filter(|ln| ln.starts_with("Voice:")).collect();
    assert_eq!(
        voice,
        [&format!(
            "Voice: {} talks; {} ({} effort) answers behind it.",
            eve_brain::voice_agent::DEFAULT_MODEL,
            eve_brain::voice_agent::DEFAULT_BACKEND_MODEL,
            eve_brain::voice_agent::DEFAULT_BACKEND_EFFORT
        )]
    );
    assert!(changed.iter().find(|ln| ln.starts_with("Doors on:")).unwrap().contains("voice (wake word)"));
    // every door off: the block still says so, in plain words
    let bare = lines(&env(&[("EVE_LIGHTS", "0"), ("EVE_VOICE", "0")]), &lent(None, "", no_router()));
    assert!(has(&bare, "Lights: off.") && has(&bare, "Doors on: none.") && has(&bare, "Web search: off.")); // no key: no search
}

#[test]
fn test_tasks_on_the_mac_name_the_agent_the_daemon_builds() {
    // The Python test also builds the daemon's agent (Daemon._make_agent) to check it is Codex; the daemon is
    // another crate, so this checks the line alone.
    let dir = tmp();
    let lights = lights_file(dir.path());
    for computer in ["codex", ""] {
        let extra: Vec<(&str, &str)> = if computer.is_empty() { vec![] } else { vec![("EVE_COMPUTER", computer)] };
        let body = lines(&owner_env(&extra), &lent(Some(&lights), "", no_router()));
        let line = body.iter().find(|ln| ln.starts_with("Tasks on the Mac:")).unwrap();
        assert_eq!(line, "Tasks on the Mac: Codex.");
        let mut off_extra = extra.clone();
        off_extra.push(("EVE_COMPUTER_CONTROL", "0"));
        let off = lines(&owner_env(&off_extra), &lent(Some(&lights), "", no_router()));
        assert!(off.contains(&"Tasks on the Mac: off.".to_string()));
    }
}

#[test]
fn test_the_reader_and_the_watcher_ladder_follow_the_tinyfish_key() {
    let dir = tmp();
    let lights = lights_file(dir.path());
    let without = lines(&owner_env(&[("EVE_WEB_READER", "1")]), &lent(Some(&lights), "", no_router()));
    assert!(without.contains(&"Tasks on the Mac: Codex.".to_string())); // no reader without its key
    assert!(without.contains(&"Watcher ladder: plain read, Perplexity search.".to_string()));
    let with_key = lines(
        &owner_env(&[("EVE_WEB_READER", "1"), ("TINYFISH_API_KEY", "test-tinyfish")]),
        &lent(Some(&lights), "", no_router()),
    );
    assert!(with_key.iter().any(|ln| ln.starts_with("Tasks on the Mac: Codex; web reads try TinyFish (")));
    assert!(with_key.contains(&"Watcher ladder: plain read, TinyFish, Perplexity search.".to_string()));
}

#[test]
fn test_web_search_says_what_jev_routes_across_when_the_router_is_on() {
    let dir = tmp();
    let lights = lights_file(dir.path());
    let all = ["perplexity", "tinyfish", "firecrawl"];
    let body = lines(
        &owner_env(&[("EVE_WEB_READER", "1"), ("TINYFISH_API_KEY", "test-tinyfish")]),
        &lent(Some(&lights), "", fake_router(&all, true)),
    );
    assert!(body.contains(&"Web search: Jev picks per query among Perplexity through OpenRouter, TinyFish and Firecrawl.".to_string()));
    assert!(body.contains(&"Tasks on the Mac: Codex; web reads try the routed search first.".to_string()));
    // only the providers usable here are named: a router that can only reach two says two, in its own order
    let two = lines(&owner_env(&[]), &lent(Some(&lights), "", fake_router(&["firecrawl", "perplexity"], true)));
    assert!(two.contains(&"Web search: Jev picks per query among Perplexity through OpenRouter and Firecrawl.".to_string()));
    // mode off, or auto before the router SHIPPED, routes nothing, whatever available() lists
    let off_mode = lines(&owner_env(&[("EVE_SEARCH_ROUTER", "off")]), &lent(Some(&lights), "", fake_router(&all, true)));
    assert!(off_mode.iter().any(|ln| ln.starts_with("Web search: Perplexity through OpenRouter;")));
    let unshipped = lines(&owner_env(&[]), &lent(Some(&lights), "", fake_router(&all, false)));
    assert!(unshipped.iter().any(|ln| ln.starts_with("Web search: Perplexity through OpenRouter;")));
    let on = lines(&owner_env(&[("EVE_SEARCH_ROUTER", "on")]), &lent(Some(&lights), "", fake_router(&all, false)));
    assert!(on.join(" ").contains("Jev picks"));
    // the real router, turned on: it lists what this environment's keys allow, and the block follows it
    let real = owner_env(&[("EVE_SEARCH_ROUTER", "on"), ("EVE_JEV_ROUTE", "openrouter"), ("FIRECRAWL_API_KEY", "test-firecrawl")]);
    assert_eq!(
        websearch::configured(&eve_web::Ctx::global().clone().with_env(real.clone())).providers,
        vec!["perplexity".to_string(), "firecrawl".to_string()]
    );
    let routed = lines(&real, &lent(Some(&lights), "", None));
    assert!(routed.contains(&"Web search: Jev picks per query among Perplexity through OpenRouter and Firecrawl.".to_string()));
    let off = lines(
        &owner_env(&[("EVE_SEARCH_ROUTER", "off"), ("FIRECRAWL_API_KEY", "test-firecrawl")]),
        &lent(Some(&lights), "", None),
    );
    assert!(off.iter().any(|ln| ln.starts_with("Web search: Perplexity through OpenRouter;")));
}

#[test]
fn test_the_rule_scopes_itself_to_what_eve_runs_on_and_leaves_the_tools_to_the_tool_list() {
    // 2026-09-29: "if it does not cover something, say you do not know" made the brain answer "I do not know"
    // about its own calendar and camera. The rule now covers models, services and providers only.
    let rule = self_context::RULE;
    assert!(rule.contains("source of truth for the models, services and providers you run on"));
    assert!(rule.contains("never guess") && rule.contains("What you can do is your tools."));
    assert!(!rule.split("never guess").next().unwrap().contains("tools")); // "do not know" is not about the tools
}

#[test]
fn test_a_part_that_fails_leaves_no_block_and_no_error() {
    // a part that raises (here: an EVE_PROFILE that is neither profile, which profile.is_lite refuses)
    let broken = owner_env(&[("EVE_PROFILE", "bogus")]);
    assert!(self_context::lines(Some(&broken), None, Some("")).is_err()); // control: the part does fail
    assert_eq!(self_context::block(Some(&broken), None, Some("")), "");
}

#[test]
fn test_building_it_does_not_log_the_doors_warnings_again() {
    let (_guard, logs) = capture_logs();
    eve_brain::telegram::configured(Some(&owner_env(&[("EVE_TELEGRAM_EFFORT", "bogus")])));
    assert_eq!(text_of(&logs).matches("EVE_TELEGRAM_EFFORT").count(), 1);
}

#[test]
fn test_the_quiet_filter_drops_only_the_quiet_modules_records() {
    let (_guard, logs) = capture_logs();
    self_context::quiet(|| {
        tracing::warn!(target: "eve_web::websearch", "quiet-one");
        tracing::warn!(target: "eve_brain::telegram::config", "quiet-two");
        tracing::warn!(target: "eve_profile", "loud-one");
    });
    tracing::warn!(target: "eve_web::websearch", "loud-two"); // control: outside the build it is logged
    let text = text_of(&logs);
    assert!(!text.contains("quiet-one") && !text.contains("quiet-two"), "{text}");
    assert!(text.contains("loud-one") && text.contains("loud-two"), "{text}");
}

/// The owner's environment with every app-shaped part on: the direct apps signed in to Google (a token file in a
/// temporary folder) and Maps (a key), Composio with a key, file sharing.
fn apps_env(dir: &Path, extra: &[(&str, &str)]) -> HashMap<String, String> {
    let folder = dir.join("apps");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("google-token.json"), json!({"refresh_token": "test-refresh"}).to_string()).unwrap();
    let folder_s = folder.display().to_string();
    let mut on: Vec<(&str, &str)> = vec![
        ("EVE_APPS", "1"),
        ("EVE_APPS_DIR", &folder_s),
        ("EVE_GOOGLE_MAPS_KEY", "test-maps"),
        ("EVE_COMPOSIO", "1"),
        ("COMPOSIO_API_KEY", "test-composio"),
        ("EVE_SHARE", "1"),
    ];
    on.extend_from_slice(extra);
    owner_env(&on)
}

#[test]
fn test_the_owners_apps_and_sharing_each_get_one_line_while_on() {
    let dir = tmp();
    let lights = lights_file(dir.path());
    let body = lines(&apps_env(dir.path(), &[]), &lent(Some(&lights), "", no_router()));
    // signed in to Google and Maps, not GitHub: GitHub is not named, and each app carries its own write rule
    assert!(body.contains(
        &"Own apps: Google Contacts and Google Maps read only; Gmail, Google Calendar and Google Drive routine changes run, sends and deletes asked first."
            .to_string()
    ), "{body:?}");
    assert!(body.contains(&"Composio apps: Slack, Notion and Linear.".to_string()));
    let sharing: Vec<&String> = body.iter().filter(|ln| ln.starts_with("File sharing:")).collect();
    let mut vias: Vec<String> = Vec::new();
    if eve_brain::share::configured(Some(&env(&[("EVE_SHARE", "1")])), if cfg!(target_os = "macos") { "darwin" } else { "linux" }).imessage {
        vias.push("iMessage".into());
    }
    vias.push("email as a Drive link".into());
    let joined = match vias.as_slice() {
        [one] => one.clone(),
        [a, b] => format!("{a} and {b}"),
        _ => unreachable!(),
    };
    assert_eq!(sharing, [&format!("File sharing: {joined}, on the owner's Allow.")]);
    // off (or Telegram, their only door, off): none of the three lines, and the block is what it was
    for e in [
        owner_env(&[]),
        apps_env(dir.path(), &[("EVE_TELEGRAM", "0")]),
        apps_env(dir.path(), &[("EVE_APPS", "0"), ("EVE_COMPOSIO", "0"), ("EVE_SHARE", "0")]),
    ] {
        let ls = lines(&e, &lent(Some(&lights), "", no_router()));
        assert!(!ls.iter().any(|ln| ln.starts_with("Own apps") || ln.starts_with("Composio apps") || ln.starts_with("File sharing")));
    }
}

#[test]
fn test_every_app_policy_has_its_words() {
    // every signed-in app is named under some rule: a policy missing from APP_RULES would drop it silently
    let mut rules: Vec<&str> = self_context::APP_RULES.iter().map(|(r, _)| *r).collect();
    rules.sort();
    let mut policies: Vec<&str> = eve_apps_mcp::POLICIES.to_vec();
    policies.sort();
    assert_eq!(rules, policies);
}

#[test]
fn test_the_block_stays_under_its_budget_with_everything_on() {
    let dir = tmp();
    let path = dir.path().join("lights.json");
    let lights = [
        eve_lights::Light { name: "a".into(), kind: "govee".into(), ip: "192.0.2.1".into(), ..Default::default() },
        eve_lights::Light { name: "b".into(), kind: "wiz".into(), ip: "192.0.2.2".into(), ..Default::default() },
        eve_lights::Light { name: "c".into(), kind: "triones".into(), address: "u".into(), ..Default::default() },
        eve_lights::Light { name: "d".into(), kind: "tuya".into(), id: "x".into(), ..Default::default() },
    ];
    eve_lights::save(lights.iter(), Some(&path)).unwrap();
    let mut e = owner_env(&[
        ("EVE_VOICE", "1"),
        ("EVE_WEB_READER", "1"),
        ("TINYFISH_API_KEY", "test-tinyfish"),
        ("EVE_WATCH_TLS", "1"),
        ("EVE_WATCH_BROWSER", "1"),
        ("EVE_CHIEF", "on"),
        ("CANVAS_BASE_URL", "https://canvas.example.edu"),
        ("CANVAS_API_TOKEN", "test-canvas"),
    ]);
    for (k, v) in apps_env(dir.path(), &[]) {
        e.entry(k).or_insert(v);
    }
    let text = self_context::block_lent(
        Some(&e),
        &lent(Some(&path), "abcdef123456", fake_router(&["perplexity", "tinyfish", "firecrawl"], true)),
    );
    assert!(text.chars().count() < self_context::BUDGET, "{}", text.chars().count());
    for part in ["Voice:", "Jev picks", "Sylvania (Tuya)", "Tasks on the Mac: Codex", "Canvas (", "Chief of staff: on."] {
        assert!(text.contains(part), "{part}: {text}");
    }
    for part in ["Own apps:", "Composio apps:", "File sharing:"] {
        assert!(text.contains(part), "{part}: {text}");
    }
}

#[tokio::test]
async fn test_think_carries_it_before_the_background_in_every_round() {
    let e = owner_env(&[]);
    let cfg = configured(Some(&e), "");
    assert_eq!(cfg.about, self_context::block(Some(&e), None, None));
    assert!(!cfg.about.is_empty());
    let (create, requests) = scripted(vec![search_call("q", "c1"), said("Perplexity.")]);
    let search: think::SearchFn = Arc::new(|_q: &str| obj(json!({"ok": true})));
    let thinker = OpenAIThinker::new(&cfg, None, Some(create), Some(search))
        .unwrap()
        .with_clock(Arc::new(|| "\n\nSystem context:\nCLOCK.".to_string()));
    assert_eq!(thinker.call("what do you search with", "BG").await.unwrap()["answer"], "Perplexity.");
    let want = json!(format!("{INSTRUCTIONS}{}{CONTEXT_HEADER}BG\n\nSystem context:\nCLOCK.", cfg.about));
    let seen: Vec<Value> = requests.lock().iter().map(|r| r["instructions"].clone()).collect();
    assert_eq!(seen, vec![want.clone(), want]);
}

#[test]
fn test_no_prompt_names_the_retired_engine() {
    // The positive control is the same check on a setup that really does run the retired engine: then the block
    // names it, because there it would be true (and the check is shown to work).
    let retired = regex::Regex::new(&format!(r"(?i)\b{}\b", "ex".to_string() + "a")).unwrap();
    let e = owner_env(&[("EVE_VOICE", "1")]);
    let body = think::request(&configured(Some(&e), ""), &question_items("q"), "BG", false);
    let text = serde_json::to_string(&body).unwrap();
    assert!(!retired.is_match(&text), "{text}");
    assert!(text.contains(&serde_json::to_string(self_context::RULE).unwrap()[1..self_context::RULE.len()]));
    let engine = format!("openrouter-{}", "ex".to_string() + "a");
    let control = self_context::block_lent(Some(&owner_env(&[("EVE_WEB_SEARCH", &engine)])), &lent(None, "", no_router()));
    assert!(retired.is_match(&control));
}

// ---- eve lite ----------------------------------------------------------------------------------------------------

const PLACEHOLDER: &str = "placeholder-not-a-key";

/// eve lite's environment as profile.apply leaves it, for the switches these lines read.
fn lite_env() -> HashMap<String, String> {
    env(&[
        ("EVE_PROFILE", "lite"),
        ("OPENROUTER_API_KEY", PLACEHOLDER),
        ("EVE_TELEGRAM", "app"),
        ("EVE_MEMORY", "1"),
        ("EVE_MEM0_EMBEDDER", "openrouter"),
        ("EVE_SECOND_BRAIN", "0"),
        ("EVE_MINIAPP", "0"),
        ("EVE_VOICE", "0"),
        ("EVE_COMPUTER_CONTROL", "0"),
        ("EVE_LIGHTS", "0"),
        ("EVE_CANVAS", "0"),
        ("EVE_CHIEF", "off"),
        ("EVE_SHARE", "0"),
        ("EVE_AGENTCARD", "0"),
        ("EVE_SEARCH_ROUTER", "off"),
        ("EVE_WEB_READER", "0"),
        ("EVE_WATCH_TLS", "0"),
        ("EVE_WATCH_BROWSER", "0"),
        ("EVE_WATCH_HOSTED", "0"),
        ("EVE_APPS", "0"),
        ("EVE_COMPOSIO", "0"),
    ])
}

fn full_env() -> HashMap<String, String> {
    env(&[
        ("OPENROUTER_API_KEY", PLACEHOLDER),
        ("EVE_TELEGRAM", "1"),
        ("EVE_TELEGRAM_TOKEN", PLACEHOLDER),
        ("EVE_TELEGRAM_OWNER", "4242"),
        ("EVE_MEMORY", "1"),
        ("EVE_VOICE", "0"),
        ("EVE_WATCH_TLS", "0"),
        ("EVE_WATCH_BROWSER", "0"),
        ("EVE_APPS", "0"),
    ])
}

/// What a lite block never says: the doors, devices and parts eve lite does not have (no "off" lines either).
const NOT_LITE: &[&str] = &[
    "Telegram", "iPhone", "Mini App", "Eve Link", "Tasks on the Mac", "Lights", "Canvas", "File sharing",
    "Chief of staff", "AgentCard", "Voice:", "Own apps", "Composio apps", "Running code",
];

/// A HOME of the test's own: the Connect sign-in file is looked for under it.
fn connect_lent(home: &Path, commit: Option<&str>) -> Lent {
    Lent {
        lights_path: None,
        commit: commit.map(str::to_string),
        router: None,
        connect_file: Some(home.join(".config").join("eve-bridge").join("composio-connect.json")),
    }
}

fn connect_file(home: &Path) -> PathBuf {
    let path = home.join(".config").join("eve-bridge").join("composio-connect.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, json!({"version": 1, "access_token": "CANARY-NOT-A-KEY-7f3c"}).to_string()).unwrap();
    path
}

#[test]
fn test_the_lite_block_is_eve_lites_parts_in_order() {
    let home = tmp();
    let e = lite_env();
    let got = lines(&e, &connect_lent(home.path(), None));
    let tg = eve_brain::telegram::configured(Some(&e));
    let tc = configured(Some(&e), "");
    assert_eq!(got.len(), 6, "{got:?}");
    assert_eq!(got[0], format!("Chat brain (eve's web app: texts and push-to-talk): {}, {} effort.", tg.model, tg.effort));
    assert_eq!(got[1], format!("think_hard: {}, {} effort.", tc.model, tc.effort));
    assert!(got[2].starts_with("Web search: ") && got[3].starts_with("Watcher ladder: ") && got[4].starts_with("Memory: "));
    assert_eq!(got[5], "Doors on: eve's web app.");
    let text = got.join("\n");
    let leaked: Vec<&&str> = NOT_LITE.iter().filter(|w| text.contains(**w)).collect();
    assert!(leaked.is_empty(), "{text}");
}

#[test]
fn test_the_apps_line_is_there_only_while_the_owner_is_signed_in_to_composio_connect() {
    let home = tmp();
    let e = lite_env();
    assert!(!lines(&e, &connect_lent(home.path(), None)).iter().any(|ln| ln.starts_with("Apps:")));
    let path = connect_file(home.path());
    let got = lines(&e, &connect_lent(home.path(), None));
    assert_eq!(got.last().unwrap(), self_context::LITE_APPS);
    assert_eq!(self_context::LITE_APPS, "Apps: the owner's apps connected in Composio, through the COMPOSIO tools.");
    assert!(!got.join("\n").contains("CANARY")); // the file's presence is read, never its tokens
    std::fs::remove_file(path).unwrap();
    assert!(!lines(&e, &connect_lent(home.path(), None)).contains(&self_context::LITE_APPS.to_string()));
    // control: full eve has no such line, signed in or not
    connect_file(home.path());
    assert!(!lines(&full_env(), &connect_lent(home.path(), Some(""))).contains(&self_context::LITE_APPS.to_string()));
}

#[test]
fn test_the_block_rides_the_lite_brains_config_inside_its_budget() {
    let home = tmp();
    connect_file(home.path());
    let block = self_context::block_lent(Some(&lite_env()), &connect_lent(home.path(), None));
    assert!(block.starts_with(self_context::HEADER) && block.ends_with(self_context::RULE), "{block}");
    assert!(block.chars().count() <= self_context::BUDGET);
    assert!(block.contains("- Chat brain (eve's web app") && block.contains("- Doors on: eve's web app."));
    // the brain's own block reads the real HOME's sign-in file: equal to a block built the same way
    assert_eq!(eve_brain::telegram::configured(Some(&lite_env())).about, self_context::block(Some(&lite_env()), None, None));
}

#[test]
fn test_the_commit_line_starts_no_git_in_lite_and_does_in_full_eve() {
    let ran: Arc<Mutex<Vec<Vec<String>>>> = Arc::new(Mutex::new(Vec::new()));
    let r = ran.clone();
    let spy = move |argv: &[String]| {
        r.lock().push(argv.to_vec());
        Some((true, "abc1234\n".to_string()))
    };
    assert_eq!(self_context::commit_of(true, &spy), "");
    assert!(ran.lock().is_empty());
    let home = tmp();
    let got = lines(&lite_env(), &connect_lent(home.path(), None));
    assert!(!got.iter().any(|ln| ln.contains("git commit")));
    let lent_commit = lines(&lite_env(), &connect_lent(home.path(), Some("abc1234")));
    assert_eq!(lent_commit.last().unwrap(), "Running code: git commit abc1234.");
    // control: full eve reads its commit with git
    assert_eq!(self_context::commit_of(false, &spy), "abc1234");
    assert_eq!(ran.lock().len(), 1);
    assert_eq!(ran.lock()[0][0], "git");
    assert_eq!(ran.lock()[0][3..], ["rev-parse".to_string(), "--short".to_string(), "HEAD".to_string()]);
    // what git says is kept only when it is a short sha
    assert_eq!(self_context::commit_of(false, &|_: &[String]| Some((false, "abc1234".into()))), "");
    assert_eq!(self_context::commit_of(false, &|_: &[String]| Some((true, "not a sha".into()))), "");
    assert_eq!(self_context::commit_of(false, &|_: &[String]| None), "");
}

#[test]
fn test_each_block_follows_the_environment_it_is_given() {
    // profile.is_lite is scoped to the mapping passed in: a block built for another environment is that one's
    let home = tmp();
    let full = lines(&full_env(), &connect_lent(home.path(), Some("")));
    assert!(full[0].starts_with("Telegram brain (texts, calls, desk push-to-talk)"));
    assert!(full.iter().any(|ln| ln.starts_with("Doors on: Telegram")));
    let lite = lines(&lite_env(), &connect_lent(home.path(), None));
    assert!(lite[0].starts_with("Chat brain (eve's web app") && !lite.join("\n").contains("Telegram"));
}

/// Python patches `system_context.context` to a fixed CLOCK; here the real clock is used and the test checks that its
/// header never reaches the cached instructions. The lights file is the default one on both sides of the equality.
#[test]
fn test_the_telegram_brain_carries_it_right_after_its_instructions() {
    use eve_brain::telegram;
    // conftest's `_menu_tools_pinned_off`: the process environment the request reads has the menu tools off
    let process = env(&[("EVE_MENU_TOOLS", "off")]);
    let env = owner_env(&[]);
    let cfg = telegram::configured(Some(&env));
    assert_eq!(cfg.about, self_context::block(Some(&env), None, None));
    assert!(!cfg.about.is_empty());
    let items = vec![telegram::message_item("user", "what do you use for search")];
    let request = |items: &[Obj], today: &str| {
        telegram::request_in(Some(&process), &cfg, items, "", "PROFILE", &[], false, today, &[], None, &[], "", &[], "",
                             "", "", &[])
    };
    let body = request(&items, "TODAY");
    let instructions = body["instructions"].as_str().unwrap().to_string();
    assert!(instructions.starts_with(&format!("{}{}{}", telegram::INSTRUCTIONS, cfg.about, telegram::PROFILE_HEADER)));
    // stable from turn to turn: the block is in the cached prefix, the clock is not
    let mut more = items.clone();
    more.push(telegram::message_item("user", "and for reading pages?"));
    let again = request(&more, "TODAY more");
    let prefix = format!("{}{}{}\nPROFILE", telegram::INSTRUCTIONS, cfg.about, telegram::PROFILE_HEADER);
    assert!(instructions.starts_with(&prefix));
    assert!(again["instructions"].as_str().unwrap().starts_with(&prefix));
    assert!(!instructions.contains("Local clock"));
    assert_eq!(body["prompt_cache_key"], json!(telegram::PROMPT_CACHE_KEY));
    // a hand-built config (the other tests) has none, and the instructions are as before
    let plain = telegram::request_in(Some(&process), &telegram::TelegramConfig::default(), &items, "", "", &[], false, "",
                                     &[], None, &[], "", &[], "", "", "", &[]);
    assert_eq!(plain["instructions"], json!(telegram::INSTRUCTIONS));
}

#[test]
fn test_the_voice_carries_it_in_both_halves() {
    use eve_brain::voice_agent;
    let env = owner_env(&[("EVE_VOICE", "1")]);
    let cfg = voice_agent::configured(Some(&env));
    assert_eq!(cfg.about, self_context::block(Some(&env), None, None));
    assert!(cfg.about.contains("Voice:"));
    let memory_tools = vec![json!({"type": "function", "name": "memory_search"}).as_object().unwrap().clone()];
    let got = voice_agent::session_config(&cfg, "", None, "VOICE-PAGE", "FULL-PAGE", "", "", Some(&memory_tools), None);
    let front = got["instructions"].as_str().unwrap();
    assert!(front.starts_with(&format!("{}{}{}", voice_agent::INSTRUCTIONS, cfg.about, voice_agent::PROFILE_HEADER)));
    let back = got["delegation"]["responses"]["instructions"].as_str().unwrap();
    assert!(back.starts_with(&format!("{}{}{}{}", voice_agent::BACKEND_INSTRUCTIONS, voice_agent::BACKEND_MEMORY_RULES,
                                      cfg.about, voice_agent::BACKEND_PROFILE_HEADER)));
    let plain = voice_agent::session_config(&voice_agent::VoiceConfig::default(), "", None, "", "", "", "", None, None);
    assert!(!serde_json::to_string(&plain).unwrap().contains(self_context::HEADER));
}
