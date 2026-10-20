//! Port of `bridge/tests/test_agentcard.py`: offline purchase safety tests with
//! an injected transport, never a live merchant or charge.

use std::io::{Read, Write as _};
use std::net::TcpListener;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Condvar};
use std::thread;

use parking_lot::Mutex;
use serde_json::{json, Value};

use super::*;

const TOKEN: &str = "PRIVATE_AGENTCARD_TOKEN_SENTINEL_93281";
const CID: &str = "conv_cafecafe";
const H1: &str = "cacacacacacacaca";
const H2: &str = "dededededededede";

fn address_fixture() -> Value {
    json!({"street": "91 Secret Sentinel Street", "city": "Testville", "state": "CA", "zip": "94123",
           "phone": "+14155551234", "name": "Ada Sentinel", "country": "US"})
}

fn addr(key: &str) -> String {
    address_fixture()[key].as_str().unwrap().to_string()
}

fn cart_of(h: &str, merchant: &str, name: &str) -> Value {
    json!({"merchant": merchant, "merchant_name": name,
           "items": [{"name": "Coffee CAFE", "qty": 2, "priceCents": 100,
                      "product_id": "https://example.com/products/CAFE"}],
           "serviceFeesCents": 5, "tipCents": 10, "totalCents": 215, "hash": h})
}

fn cart() -> Value {
    cart_of(H1, "retail", "Amazon")
}

fn shopping(carts: Option<Vec<Value>>) -> Value {
    let carts = carts.unwrap_or_else(|| vec![cart()]);
    let last = carts.last().cloned().unwrap_or(Value::Null);
    json!({"conversation_id": CID, "status": "needs_input", "reply": "Which coffee?", "carts": carts,
           "cart": last, "unmatched": [], "catalog": null})
}

fn merge(mut base: Value, updates: Value) -> Value {
    if let (Some(b), Some(u)) = (base.as_object_mut(), updates.as_object()) {
        for (k, v) in u {
            b.insert(k.clone(), v.clone());
        }
    }
    base
}

fn ledger_with(carts: Option<Vec<Value>>, updates: Value) -> Value {
    let carts = carts.unwrap_or_else(|| vec![cart()]);
    merge(
        json!({"conversation_id": CID, "status": "open", "turn_in_progress": false, "last_checkout": null,
               "orders": [], "carts": carts, "unmatched": []}),
        updates,
    )
}

fn ledger() -> Value {
    ledger_with(None, json!({}))
}

fn receipt(merchant: &str, order_id: Value, updates: Value) -> Value {
    merge(
        json!({"merchant": merchant, "merchant_name": "Amazon", "status": "order_placed", "order_id": order_id,
               "charge_status": "settled", "decline_code": null}),
        updates,
    )
}

fn placed(updates: Value) -> Value {
    merge(
        json!({"conversation_id": CID, "status": "order_placed", "order_id": "order_1", "charge_status": "settled",
               "carts": [], "unmatched": []}),
        updates,
    )
}

enum Checkout {
    Reply(u16, Value),
    Fail(String),
}

type Hook = Arc<dyn Fn(&Value) + Send + Sync>;
type FailWhen = Arc<dyn Fn(&HttpRequest) -> bool + Send + Sync>;

struct Server {
    calls: Mutex<Vec<HttpRequest>>,
    ask_body: Mutex<Value>,
    ledger: Mutex<Value>,
    checkout: Mutex<Checkout>,
    before_checkout: Mutex<Option<Hook>>,
    merchants: Mutex<Value>,
    fail_when: Mutex<Option<FailWhen>>,
}

impl Server {
    fn new() -> Arc<Server> {
        Arc::new(Server {
            calls: Mutex::new(Vec::new()),
            ask_body: Mutex::new(shopping(None)),
            ledger: Mutex::new(ledger()),
            checkout: Mutex::new(Checkout::Reply(200, placed(json!({})))),
            before_checkout: Mutex::new(None),
            merchants: Mutex::new(
                json!({"merchants": [{"slug": "retail", "name": "Retail", "link_status": "ready"}]}),
            ),
            fail_when: Mutex::new(None),
        })
    }

    fn confirms(&self) -> Vec<Value> {
        self.calls
            .lock()
            .iter()
            .filter(|c| c.method == "POST")
            .filter_map(|c| c.json.clone())
            .filter(|j| j.get("confirm").is_some())
            .collect()
    }

    fn gets(&self) -> usize {
        self.calls
            .lock()
            .iter()
            .filter(|c| c.method == "GET")
            .count()
    }

    fn posts(&self) -> usize {
        self.calls
            .lock()
            .iter()
            .filter(|c| c.method == "POST")
            .count()
    }

    fn set_checkout(&self, code: u16, body: Value) {
        *self.checkout.lock() = Checkout::Reply(code, body);
    }

    fn set_ledger(&self, body: Value) {
        *self.ledger.lock() = body;
    }
}

impl Transport for Server {
    fn request(&self, request: HttpRequest) -> anyhow::Result<HttpResponse> {
        assert!(request.url.starts_with(&format!("{API_ORIGIN}/buy")));
        let auth = request
            .headers
            .iter()
            .find(|(k, _)| k == "Authorization")
            .map(|(_, v)| v.clone())
            .unwrap();
        assert!(auth.starts_with("Bearer "));
        assert!(request.timeout >= 120.0);
        let fail = self.fail_when.lock().clone();
        if fail.is_some_and(|f| f(&request)) {
            anyhow::bail!("{TOKEN}");
        }
        self.calls.lock().push(request.clone());
        if request.method == "GET" {
            let body = if request.url.ends_with("/merchants") {
                self.merchants.lock().clone()
            } else {
                self.ledger.lock().clone()
            };
            return Ok(HttpResponse::json(200, &body));
        }
        let payload = request.json.clone().unwrap_or(Value::Null);
        if payload.get("confirm").is_none() {
            return Ok(HttpResponse::json(200, &self.ask_body.lock().clone()));
        }
        let hook = self.before_checkout.lock().clone();
        if let Some(hook) = hook {
            hook(&payload);
        }
        match &*self.checkout.lock() {
            Checkout::Fail(text) => anyhow::bail!("{text}"),
            Checkout::Reply(code, body) => Ok(HttpResponse::json(*code, body)),
        }
    }
}

fn service_with(
    tmp: &Path,
    server: Option<Arc<Server>>,
    token: &str,
    address: Option<Value>,
) -> (Arc<AgentCard>, Arc<Server>) {
    let server = server.unwrap_or_else(Server::new);
    let config = AgentCardConfig {
        enabled: true,
        token: token.into(),
        timeout_secs: 180.0,
        address_json: address.map(|a| a.to_string()),
    };
    let transport: Arc<dyn Transport> = server.clone();
    let client = AgentCard::new(config, Some(transport), Some(tmp.join("state"))).unwrap();
    (Arc::new(client), server)
}

fn service(tmp: &Path) -> (Arc<AgentCard>, Arc<Server>) {
    service_with(tmp, None, TOKEN, Some(address_fixture()))
}

fn start(client: &AgentCard) -> Value {
    let result = client.handle(
        "agentcard_buy",
        &json!({"ask": "Find coffee", "conversation_id": null}),
    );
    assert_eq!(result["ok"], json!(true), "{result}");
    result
}

fn prepare_hashes(client: &AgentCard, hashes: &[&str]) -> Prepared {
    let prepared = client.prepare_confirm(&json!(CID), &json!(hashes));
    assert!(prepared.ok(), "{}", prepared.result);
    prepared
}

fn prepare(client: &AgentCard) -> Prepared {
    prepare_hashes(client, &[H1])
}

fn try_prepare(client: &AgentCard, hashes: &[&str]) -> Prepared {
    client.prepare_confirm(&json!(CID), &json!(hashes))
}

fn snapshot(prepared: Prepared) -> Snapshot {
    prepared.snapshot.expect("snapshot")
}

fn journal(client: &AgentCard) -> Value {
    serde_json::from_str(&fs::read_to_string(client.state_path()).unwrap()).unwrap()
}

fn state_text(client: &AgentCard) -> String {
    fs::read_to_string(client.state_path()).unwrap()
}

fn not_ok(v: &Value) -> bool {
    v["ok"] != json!(true)
}

fn tmp() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// `threading.Event`.
#[derive(Default)]
struct Event {
    set: std::sync::Mutex<bool>,
    cv: Condvar,
}

impl Event {
    fn set(&self) {
        *self.set.lock().unwrap() = true;
        self.cv.notify_all();
    }

    fn wait(&self, secs: u64) -> bool {
        let guard = self.set.lock().unwrap();
        let (guard, _) = self
            .cv
            .wait_timeout_while(guard, Duration::from_secs(secs), |s| !*s)
            .unwrap();
        *guard
    }
}

#[test]
fn test_disabled_configuration_has_no_service_file_read_or_network() {
    let dir = tmp();
    let missing = dir.path().join("missing").to_string_lossy().into_owned();
    for e in [
        env(&[]),
        env(&[("AGENTCARD_ACCESS_TOKEN", TOKEN)]),
        env(&[("EVE_AGENTCARD", "1")]),
        env(&[
            ("EVE_AGENTCARD", "0"),
            ("AGENTCARD_ACCESS_TOKEN", TOKEN),
            ("EVE_AGENTCARD_ADDRESS_FILE", &missing),
        ]),
    ] {
        assert!(!configured(Some(&e)).unwrap().enabled);
        assert!(make_agentcard(Some(&e)).is_none());
    }
}

#[test]
fn test_configuration_refuses_unsafe_timeout_without_secret_logging() {
    for timeout in ["119", "nan", "inf", "bad", "-1"] {
        let e = env(&[
            ("EVE_AGENTCARD", "1"),
            ("AGENTCARD_ACCESS_TOKEN", TOKEN),
            ("EVE_AGENTCARD_TIMEOUT_SECS", timeout),
        ]);
        let err = configured(Some(&e)).unwrap_err();
        assert!(!err.to_string().contains(TOKEN));
        assert!(make_agentcard(Some(&e)).is_none());
    }
}

#[test]
fn test_configuration_private_address_and_token_repr() {
    let dir = tmp();
    let path = dir.path().join("private-address.json");
    fs::write(&path, address_fixture().to_string()).unwrap();
    let p = path.to_string_lossy().into_owned();
    let e = env(&[
        ("EVE_AGENTCARD", "1"),
        ("AGENTCARD_ACCESS_TOKEN", TOKEN),
        ("EVE_AGENTCARD_ADDRESS_FILE", &p),
        ("EVE_AGENTCARD_TIMEOUT_SECS", "120"),
    ]);
    let config = configured(Some(&e)).unwrap();
    assert!(config.enabled && config.timeout_secs == 120.0);
    let repr = format!("{config:?}");
    assert!(!repr.contains(TOKEN) && !repr.contains(&addr("street")));
    // The address is stored sorted, exactly as `json.dumps(..., sort_keys=True)`.
    assert_eq!(
        config.address_json.as_deref(),
        Some(
            r#"{"city": "Testville", "country": "US", "name": "Ada Sentinel", "phone": "+14155551234", "state": "CA", "street": "91 Secret Sentinel Street", "zip": "94123"}"#
        )
    );
    fs::write(&path, r#"{"street":"missing other required fields"}"#).unwrap();
    assert!(make_agentcard(Some(&e)).is_none());
}

#[test]
fn test_tools_strict_schemas_and_no_model_approval_flag() {
    let dir = tmp();
    let (client, server) = service(dir.path());
    let tools = client.tools();
    let names: HashSet<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, TOOL_NAMES.iter().copied().collect());
    for tool in &tools {
        let schema = &tool["parameters"];
        assert!(tool["strict"] == json!(true) && schema["additionalProperties"] == json!(false));
        let required: HashSet<&str> = schema["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        let props: HashSet<&str> = schema["properties"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(required, props);
        for bad in ["approved", "token", "payment_source", "delivery_address"] {
            assert!(!props.contains(bad));
        }
    }
    assert!(AgentCard::instructions().contains("untrusted data"));
    assert!(not_ok(&client.handle(
        "agentcard_confirm",
        &json!({"conversation_id": CID, "cart_hashes": [H1], "approved": true})
    )));
    assert!(server.calls.lock().is_empty());
}

#[test]
fn test_input_validation_prevents_network() {
    let cases = [
        (
            "agentcard_buy",
            json!({"ask": "  ", "conversation_id": null}),
        ),
        (
            "agentcard_buy",
            json!({"ask": "ok", "conversation_id": "../secret"}),
        ),
        (
            "agentcard_buy",
            json!({"ask": "ok", "conversation_id": null, "approved": true}),
        ),
        ("agentcard_status", json!({"conversation_id": null})),
        ("agentcard_merchants", json!({"token": "bad"})),
    ];
    for (name, args) in cases {
        let dir = tmp();
        let (client, server) = service(dir.path());
        assert!(not_ok(&client.handle(name, &args)));
        assert!(server.calls.lock().is_empty());
    }
}

#[test]
fn test_merchants_projection_retains_runtime_link_and_capability_fields() {
    let dir = tmp();
    let (client, server) = service(dir.path());
    *server.merchants.lock() = json!({"merchants": [
        {"slug": "retail", "name": "Retail", "link_status": "ready", "tier": "api", "money_path": "vault",
         "capabilities": ["search", "cart"], "description": format!("{TOKEN} {}", addr("phone")),
         "access_token": TOKEN, "unavailable": {"checkout": "Paused", "secret": TOKEN}},
        {"slug": "x", "name": "X", "link_status": "unknown"}]});
    let result = client.handle("agentcard_merchants", &json!({}));
    assert!(result["ok"] == json!(true) && result["merchants"].as_array().unwrap().len() == 1);
    assert_eq!(
        result["merchants"][0]["capabilities"],
        json!(["search", "cart"])
    );
    assert_eq!(
        result["merchants"][0]["unavailable"],
        json!({"checkout": "Paused"})
    );
    let text = result.to_string();
    assert!(!text.contains(TOKEN) && !text.contains(&addr("phone")));
}

#[test]
fn test_privacy_projection_and_minimal_durable_state_preserve_opaque_ids() {
    let dir = tmp();
    let (client, server) = service(dir.path());
    {
        let mut ask = server.ask_body.lock();
        ask["reply"] = json!(format!(
            "Ship to {} for {} {} {TOKEN}",
            addr("street"),
            addr("name"),
            addr("phone")
        ));
        ask["secret"] = json!({"token": TOKEN});
        ask["catalog"] = json!({"merchant": "retail", "items": [{"id": "https://example.com/CAFE", "name": "Coffee",
                                "priceCents": 100, "image_url": "https://example.com/photo.jpg"}]});
    }
    let result = start(&client);
    assert!(result["conversation_id"] == json!(CID) && result["carts"][0]["hash"] == json!(H1));
    assert!(result["carts"][0]["items"][0]["product_id"]
        .as_str()
        .unwrap()
        .ends_with("CAFE"));
    assert!(result["catalog"]["items"][0]["image_url"]
        .as_str()
        .unwrap()
        .ends_with("photo.jpg"));
    let serialized = result.to_string();
    for value in [
        TOKEN.to_string(),
        addr("street"),
        addr("phone"),
        addr("name"),
    ] {
        assert!(!serialized.contains(&value));
        assert!(!state_text(&client).contains(&value));
    }
    let state = journal(&client);
    let conv = state["conversations"][CID].as_object().unwrap();
    let keys: HashSet<&str> = conv.keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        HashSet::from([
            "version",
            "address_digest",
            "carts",
            "attempts",
            "shopping_pending"
        ])
    );
    assert_eq!(conv["carts"][H1].as_str().unwrap().len(), 64);
    assert!(!state_text(&client).contains("Coffee") && !state_text(&client).contains("photo.jpg"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(client.state_dir())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(client.state_path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    let prepared = prepare(&client);
    let question = prepared.result["question"].as_str().unwrap().to_string();
    assert!(prepared.result["about"].is_string());
    assert!(question.contains(H1) && !question.contains(&addr("street")));
    assert_eq!(client.confirm(&snapshot(prepared), None)["ok"], json!(true));
    assert_eq!(
        server.confirms(),
        vec![
            json!({"conversation_id": CID, "confirm": H1, "payment_source": "vault",
                    "delivery_address": address_fixture()})
        ]
    );
}

#[test]
fn test_preview_covers_all_selected_carts_totals_fees_tips_and_omissions() {
    let dir = tmp();
    let (client, server) = service(dir.path());
    let carts = vec![cart(), cart_of(H2, "restaurant", "Soup Shop")];
    *server.ask_body.lock() = shopping(Some(carts.clone()));
    server.set_ledger(ledger_with(
        Some(carts),
        json!({"unmatched": [{"merchant": "retail", "requested": "blue mug", "reason": "not_found",
                              "detail": "Not stocked", "at": "2026-10-03"}]}),
    ));
    start(&client);
    let prepared = prepare_hashes(&client, &[H1, H2]);
    let question = prepared.result["question"].as_str().unwrap();
    for text in [
        "Amazon",
        "Soup Shop",
        H1,
        H2,
        "2 ×",
        "100 cents",
        "Fees: 5",
        "tip: 10",
        "215 cents",
        "blue mug",
        "not_found",
    ] {
        assert!(question.contains(text), "{text} missing from {question}");
    }
    assert!(server.confirms().is_empty());
}

#[test]
fn test_preview_matches_python_text_exactly() {
    let dir = tmp();
    let (client, server) = service(dir.path());
    server.set_ledger(ledger_with(
        None,
        json!({"unmatched": [{"merchant": "retail", "requested": "blue mug", "reason": "not_found",
                              "detail": "Not stocked", "at": "2026-10-03"}]}),
    ));
    start(&client);
    let prepared = prepare(&client);
    assert_eq!(
        prepared.result["question"].as_str().unwrap(),
        "Place these exact AgentCard carts using your Vault? Amounts below are the API's cents.\n\
Merchant: \"Amazon\" (\"retail\"); hash cacacacacacacaca\n  \
2 × \"Coffee CAFE\"; API price: 100 cents; product: \"https://example.com/products/CAFE\"\n  \
Fees: 5 cents; tip: 10 cents; all-in total: 215 cents (2.15).\n\
Delivery: the private address frozen when this conversation began.\n\
Requested lines omitted from the carts:\n  \
{\"merchant\": \"retail\", \"requested\": \"blue mug\", \"reason\": \"not_found\", \"detail\": \"Not stocked\"}\n\
A partial or declined checkout is possible. Confirm only if every selected cart and omission is acceptable."
    );
}

#[test]
fn test_confirm_hash_validation_refuses_before_get() {
    for hashes in [
        json!([]),
        json!(["bad"]),
        json!([H1, H1]),
        json!(vec![H1; 9]),
        json!([123]),
    ] {
        let dir = tmp();
        let (client, server) = service(dir.path());
        assert!(!client.prepare_confirm(&json!(CID), &hashes).ok());
        assert!(server.calls.lock().is_empty());
    }
}

#[test]
fn test_snapshot_is_single_use_and_cannot_be_forged() {
    let dir = tmp();
    let (client, server) = service(dir.path());
    start(&client);
    let snap = snapshot(prepare(&client));
    let forged = Snapshot(Arc::new((*snap.0).clone()));
    assert!(not_ok(&client.confirm(&forged, None)));
    assert!(not_ok(&client.confirm(&snap, None)));
    let snap = snapshot(prepare(&client));
    assert_eq!(client.confirm(&snap, None)["ok"], json!(true));
    assert!(not_ok(&client.confirm(&snap, None)));
    assert_eq!(server.confirms().len(), 1);
}

#[test]
fn test_snapshot_stale_after_new_preview_or_status() {
    let dir = tmp();
    let (client, server) = service(dir.path());
    start(&client);
    let old = snapshot(prepare(&client));
    prepare(&client);
    assert!(not_ok(&client.confirm(&old, None)));
    let old = snapshot(prepare(&client));
    assert_eq!(
        client.handle("agentcard_status", &json!({"conversation_id": CID}))["ok"],
        json!(true)
    );
    assert!(not_ok(&client.confirm(&old, None)));
    assert!(server.confirms().is_empty());
}

#[test]
fn test_cancelled_confirm_and_closed_client_do_not_dispatch() {
    let dir = tmp();
    let (client, server) = service(dir.path());
    start(&client);
    let snap = snapshot(prepare(&client));
    let cancelled = AtomicBool::new(true);
    assert!(not_ok(&client.confirm(&snap, Some(&cancelled))));
    client.close();
    assert!(not_ok(&client.confirm(&snap, None)));
    assert!(not_ok(&client.handle("agentcard_merchants", &json!({}))));
    assert!(server.confirms().is_empty());
}

#[test]
fn test_durable_claim_exists_before_post_and_blocks_second_instance() {
    let dir = tmp();
    let (client, server) = service(dir.path());
    start(&client);
    let snap = snapshot(prepare(&client));
    let checked = Arc::new(AtomicBool::new(false));
    {
        let (client, server_ref, root, checked) = (
            client.clone(),
            Arc::downgrade(&server),
            dir.path().to_path_buf(),
            checked.clone(),
        );
        *server.before_checkout.lock() = Some(Arc::new(move |_payload: &Value| {
            let recorded = &journal(&client)["conversations"][CID]["attempts"][H1];
            assert_eq!(recorded["stage"], json!("claimed"));
            // A separate client can acquire the file lock while network is running, but cannot re-charge.
            let (other, _) =
                service_with(&root, server_ref.upgrade(), TOKEN, Some(address_fixture()));
            let result = try_prepare(&other, &[H1]).result;
            assert!(not_ok(&result) && result["reason"] == json!("cart_already_attempted"));
            checked.store(true, Ordering::SeqCst);
        }));
    }
    assert_eq!(client.confirm(&snap, None)["ok"], json!(true));
    assert!(checked.load(Ordering::SeqCst));
    assert_eq!(server.confirms().len(), 1);
    let (restarted, _) = service_with(
        dir.path(),
        Some(server.clone()),
        TOKEN,
        Some(address_fixture()),
    );
    assert!(!try_prepare(&restarted, &[H1]).ok());
    assert_eq!(server.confirms().len(), 1);
}

#[test]
fn test_account_namespace_and_restart_address_binding() {
    let dir = tmp();
    let (client, server) = service(dir.path());
    start(&client);
    let (other, other_server) = service_with(
        dir.path(),
        None,
        &format!("{TOKEN}_OTHER"),
        Some(address_fixture()),
    );
    assert!(!try_prepare(&other, &[H1]).ok());
    assert!(other_server.calls.lock().is_empty() && other.state_path() != client.state_path());
    let changed_address = merge(
        address_fixture(),
        json!({"street": "Different private street"}),
    );
    let (changed, changed_server) = service_with(dir.path(), None, TOKEN, Some(changed_address));
    assert!(!try_prepare(&changed, &[H1]).ok());
    assert!(changed_server.confirms().is_empty());
    let (restarted, _) = service_with(dir.path(), Some(server), TOKEN, Some(address_fixture()));
    assert!(prepare(&restarted).ok());
}

#[test]
fn test_state_corruption_blocks_confirmation() {
    for bad_state in [
        "null",
        "[]",
        "broken",
        r#"{"schema":1,"account":"wrong","conversations":{}}"#,
    ] {
        let dir = tmp();
        let (client, server) = service(dir.path());
        start(&client);
        let snap = snapshot(prepare(&client));
        fs::write(client.state_path(), bad_state).unwrap();
        assert!(not_ok(&client.confirm(&snap, None)));
        assert!(server.confirms().is_empty());
    }
}

#[test]
fn test_unwritable_durable_claim_blocks_post() {
    let dir = tmp();
    let (client, server) = service(dir.path());
    start(&client);
    let snap = snapshot(prepare(&client));
    client.hooks.fail.store(true, Ordering::SeqCst);
    let result = client.confirm(&snap, None);
    assert!(not_ok(&result) && !result.to_string().contains(TOKEN));
    assert!(server.confirms().is_empty());
}

#[test]
fn test_uncertain_confirmation_never_retries_even_after_fresh_empty_ledger() {
    let failures = [
        Checkout::Fail(TOKEN.to_string()),
        Checkout::Reply(500, json!({"conversation_id": CID, "error": TOKEN})),
        Checkout::Reply(200, json!([])),
        Checkout::Reply(
            200,
            json!({"conversation_id": CID, "status": "new_unknown_status"}),
        ),
    ];
    for failure in failures {
        let dir = tmp();
        let (client, server) = service(dir.path());
        start(&client);
        let snap = snapshot(prepare(&client));
        *server.checkout.lock() = failure;
        let result = client.confirm(&snap, None);
        assert!(
            not_ok(&result) && result["status"] == json!("uncertain"),
            "{result}"
        );
        assert_eq!(result["reconciliation_required"], json!(true));
        assert!(!result.to_string().contains(TOKEN));
        assert!(!try_prepare(&client, &[H1]).ok());
        let (restarted, _) = service_with(
            dir.path(),
            Some(server.clone()),
            TOKEN,
            Some(address_fixture()),
        );
        assert!(!try_prepare(&restarted, &[H1]).ok());
        assert_eq!(server.confirms().len(), 1);
    }
}

#[test]
fn test_vault_pause_requires_fresh_get_new_consent_and_never_automatically_retries() {
    let dir = tmp();
    let (client, server) = service(dir.path());
    start(&client);
    server.set_checkout(
        200,
        json!({"conversation_id": CID, "status": "declined", "decline_code": "vault_approval_required",
               "charge_status": "none", "approval_url": "https://vault.agentcard.sh/authorize?id=safe"}),
    );
    let result = client.confirm(&snapshot(prepare(&client)), None);
    assert!(
        not_ok(&result)
            && result["approval_url"]
                .as_str()
                .unwrap()
                .starts_with("https://vault.agentcard.sh/")
    );
    // Empty ledger is not evidence for a retryable approval pause.
    assert!(!try_prepare(&client, &[H1]).ok());
    server.set_ledger(ledger_with(
        None,
        json!({"last_checkout": {"status": "needs_approval", "charge_status": "none",
                                 "decline_code": "vault_approval_required", "order_id": null}}),
    ));
    let retry = prepare(&client);
    assert_eq!(server.confirms().len(), 1);
    server.set_checkout(200, placed(json!({})));
    assert_eq!(client.confirm(&snapshot(retry), None)["ok"], json!(true));
    assert_eq!(server.confirms().len(), 2);
}

#[test]
fn test_vault_asynchronous_placement_and_unknown_ledger_block_retry() {
    for state in ["in_progress", "auto_placed", "unknown_charge"] {
        let dir = tmp();
        let (client, server) = service(dir.path());
        start(&client);
        server.set_checkout(
            200,
            json!({"conversation_id": CID, "status": "declined", "decline_code": "vault_approval_required",
                   "charge_status": "none"}),
        );
        client.confirm(&snapshot(prepare(&client)), None);
        server.set_ledger(match state {
            "in_progress" => ledger_with(None, json!({"turn_in_progress": true})),
            "auto_placed" => ledger_with(
                None,
                json!({"orders": [{"order_id": "order_1", "merchant": "retail", "merchant_name": "Amazon",
                                   "status": "settled", "total_cents": 215}]}),
            ),
            _ => ledger_with(None, json!({"last_checkout": {"status": "error", "charge_status": "unknown"}})),
        });
        let result = try_prepare(&client, &[H1]);
        assert!(!result.ok() && server.confirms().len() == 1, "{state}");
    }
}

#[test]
fn test_projection_rejects_unapproved_vault_links() {
    for url in [
        "http://vault.agentcard.sh/authorize?id=x",
        "https://vault.agentcard.sh.evil.test/authorize?id=x",
        "https://user:pass@vault.agentcard.sh/authorize?id=x",
        "https://vault.agentcard.sh:444/authorize?id=x",
    ] {
        let dir = tmp();
        let (client, server) = service(dir.path());
        start(&client);
        server.set_checkout(
            200,
            json!({"conversation_id": CID, "status": "declined", "decline_code": "vault_approval_required",
                   "charge_status": "none", "approval_url": url}),
        );
        let result = client.confirm(&snapshot(prepare(&client)), None);
        assert!(result.get("approval_url").is_none(), "{url}");
    }
}

#[test]
fn test_checkout_privacy_omits_card_prose_but_preserves_structured_receipt() {
    let dir = tmp();
    let (client, server) = service(dir.path());
    start(&client);
    server.set_checkout(
        200,
        placed(
            json!({"reply": format!("Charged to Visa ending 4832 {TOKEN} {}", addr("street")),
                      "messages": ["Visa 4832"],
                      "payment_source": {"source": "vault", "brand": "visa", "last4": "4832"},
                      "card_number": "4111111111111111", "delivery_address": address_fixture()}),
        ),
    );
    let result = client.confirm(&snapshot(prepare(&client)), None);
    assert!(
        result["ok"] == json!(true)
            && result["order_id"] == json!("order_1")
            && result["charge_status"] == json!("settled")
    );
    let text = result.to_string();
    for bad in [
        TOKEN.to_string(),
        addr("street"),
        "4832".into(),
        "4111111111111111".into(),
        "payment_source".into(),
        "messages".into(),
        "reply".into(),
    ] {
        assert!(
            !text.contains(&bad) && !state_text(&client).contains(&bad),
            "{bad}"
        );
    }
}

#[test]
fn test_multi_confirm_full_success_vs_partial_and_unaccounted_cart() {
    for kind in ["full", "mixed", "server_partial", "missing_cart"] {
        let dir = tmp();
        let (client, server) = service(dir.path());
        let carts = vec![cart(), cart_of(H2, "restaurant", "Soup Shop")];
        *server.ask_body.lock() = shopping(Some(carts.clone()));
        server.set_ledger(ledger_with(Some(carts), json!({})));
        start(&client);
        let mut p = vec![
            receipt("retail", json!("order_1"), json!({})),
            receipt("restaurant", json!("order_2"), json!({})),
        ];
        let (mut code, mut status) = (200, "order_placed");
        if kind == "mixed" || kind == "server_partial" {
            p[1] = receipt(
                "restaurant",
                Value::Null,
                json!({"status": "declined", "charge_status": "none", "decline_code": "items_unavailable"}),
            );
            status = "partially_placed";
            code = if kind == "server_partial" { 502 } else { 200 };
        } else if kind == "missing_cart" {
            p.pop();
        }
        server.set_checkout(
            code,
            json!({"conversation_id": CID, "status": status, "order_id": null, "charge_status": null, "placements": p}),
        );
        let result = client.confirm(&snapshot(prepare_hashes(&client, &[H1, H2])), None);
        assert_eq!(
            result["ok"] == json!(true),
            kind == "full",
            "{kind}: {result}"
        );
        assert_eq!(result["placements"][0]["order_id"], json!("order_1"));
        if kind != "full" {
            assert!(
                result["status"] == json!("partially_placed")
                    && result["reconciliation_required"] == json!(true)
            );
        }
        let confirms = server.confirms();
        assert_eq!(confirms[0]["confirm"], json!([H1, H2]));
        assert!(confirms[0].get("ask").is_none());
        assert!(!try_prepare(&client, &[H1, H2]).ok());
    }
}

#[test]
fn test_malformed_ledger_or_authorization_ceiling_blocks_confirmation() {
    for bad in [
        "ceiling",
        "invalid_hash",
        "negative_total",
        "bad_order",
        "bad_checkout",
    ] {
        let dir = tmp();
        let (client, server) = service(dir.path());
        start(&client);
        {
            let mut l = server.ledger.lock();
            match bad {
                "ceiling" => l["carts"][0]["approvedCeilingCents"] = json!(99999),
                "invalid_hash" => l["carts"][0]["hash"] = json!(1234567890123456_i64),
                "negative_total" => l["carts"][0]["totalCents"] = json!(-1),
                "bad_order" => l["orders"] = json!([{"status": "future_unknown_status"}]),
                _ => {
                    l["last_checkout"] =
                        json!({"status": "future_unknown_status", "charge_status": "none"})
                }
            }
        }
        assert!(!try_prepare(&client, &[H1]).ok(), "{bad}");
        assert!(server.confirms().is_empty());
    }
}

#[test]
fn test_shopping_followup_after_placed_order_requires_new_conversation() {
    let dir = tmp();
    let (client, server) = service(dir.path());
    start(&client);
    assert_eq!(
        client.confirm(&snapshot(prepare(&client)), None)["ok"],
        json!(true)
    );
    let result = client.handle(
        "agentcard_buy",
        &json!({"ask": "Another bag", "conversation_id": CID}),
    );
    assert!(not_ok(&result) && result["reason"] == json!("start_new_conversation"));
    assert_eq!(server.posts(), 2);
}

#[test]
fn test_concurrent_instances_cannot_submit_a_second_claim_while_http_in_flight() {
    let dir = tmp();
    let (client, server) = service(dir.path());
    start(&client);
    let snap = Arc::new(snapshot(prepare(&client)));
    let entered = Arc::new(Event::default());
    let release = Arc::new(Event::default());
    {
        let (entered, release) = (entered.clone(), release.clone());
        *server.before_checkout.lock() = Some(Arc::new(move |_payload: &Value| {
            entered.set();
            assert!(release.wait(3));
        }));
    }
    let worker = {
        let (client, snap) = (client.clone(), snap.clone());
        thread::spawn(move || client.confirm(&snap, None))
    };
    assert!(entered.wait(2));
    let (other, _) = service_with(
        dir.path(),
        Some(server.clone()),
        TOKEN,
        Some(address_fixture()),
    );
    let checks = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        assert!(!try_prepare(&other, &[H1]).ok());
        assert!(not_ok(&other.confirm(&snap, None)));
        assert_eq!(server.confirms().len(), 1);
    }));
    release.set();
    assert_eq!(worker.join().unwrap()["ok"], json!(true));
    if let Err(panic) = checks {
        std::panic::resume_unwind(panic);
    }
}

#[test]
fn test_snapshot_cache_bounded_after_denied_previews() {
    let dir = tmp();
    let (client, _) = service(dir.path());
    start(&client);
    for _ in 0..20 {
        prepare(&client);
        assert_eq!(client.mutex.lock().snapshots.len(), 1);
    }
}

#[test]
fn test_vault_retry_rechecks_ledger_after_owner_consent_before_post() {
    let dir = tmp();
    let (client, server) = service(dir.path());
    start(&client);
    server.set_checkout(
        200,
        json!({"conversation_id": CID, "status": "declined", "decline_code": "vault_approval_required",
               "charge_status": "none"}),
    );
    client.confirm(&snapshot(prepare(&client)), None);
    server.set_ledger(ledger_with(
        None,
        json!({"last_checkout": {"status": "needs_approval", "charge_status": "none",
                                 "decline_code": "vault_approval_required"}}),
    ));
    let snap = snapshot(prepare(&client));
    server.set_ledger(ledger_with(
        None,
        json!({"orders": [{"order_id": "order_auto", "merchant": "retail", "merchant_name": "Amazon",
                           "status": "settled", "total_cents": 215}]}),
    ));
    let result = client.confirm(&snap, None);
    assert!(
        not_ok(&result) && result["orders"][0]["order_id"] == json!("order_auto"),
        "{result}"
    );
    assert_eq!(server.confirms().len(), 1);
}

#[test]
fn test_confirm_preserves_known_or_unknown_outcome_when_state_reacquire_fails() {
    for known_receipt in [true, false] {
        let dir = tmp();
        let (client, server) = service(dir.path());
        start(&client);
        let snap = snapshot(prepare(&client));
        if !known_receipt {
            server.set_checkout(200, json!([]));
        }
        {
            let path = client.state_path().to_path_buf();
            *server.before_checkout.lock() = Some(Arc::new(move |_payload: &Value| {
                fs::write(&path, "broken").unwrap();
            }));
        }
        let result = client.confirm(&snap, None);
        assert!(not_ok(&result) && result["reconciliation_required"] == json!(true));
        if known_receipt {
            assert!(
                result["order_id"] == json!("order_1")
                    && result["status"] == json!("order_placed")
                    && result["charge_status"] == json!("settled")
            );
        } else {
            assert!(
                result["status"] == json!("uncertain")
                    && result["charge_status"] == json!("unknown")
            );
        }
        assert_eq!(server.confirms().len(), 1);
    }
}

#[test]
fn test_oversized_omissions_or_selected_identity_blocks_preview_without_truncation() {
    let dir = tmp();
    let (client, server) = service(dir.path());
    start(&client);
    let omitted = json!({"merchant": "retail", "requested": "blue mug", "reason": "not_found"});
    server.set_ledger(ledger_with(
        None,
        json!({"unmatched": vec![omitted.clone(); 201]}),
    ));
    assert!(!try_prepare(&client, &[H1]).ok());
    server.set_ledger(ledger_with(
        None,
        json!({"unmatched": [merge(omitted.clone(), json!({"requested": "oversized identity ".repeat(300)}))]}),
    ));
    assert!(!try_prepare(&client, &[H1]).ok());
    server.set_ledger(ledger_with(
        None,
        json!({"unmatched": [merge(omitted, json!({"detail": "oversized meaning ".repeat(300)}))]}),
    ));
    assert!(!try_prepare(&client, &[H1]).ok());
    let mut l = ledger();
    l["carts"][0]["items"][0]["name"] = json!("important identity ".repeat(300));
    server.set_ledger(l);
    assert!(!try_prepare(&client, &[H1]).ok());
    assert!(server.confirms().is_empty());
}

#[test]
fn test_timed_out_shopping_ask_recovers_from_idle_ledger_without_clearing_charge_barrier() {
    let dir = tmp();
    let (client, server) = service(dir.path());
    start(&client);
    *server.fail_when.lock() = Some(Arc::new(|r: &HttpRequest| {
        r.method == "POST"
            && r.json.as_ref().is_some_and(|j| {
                j.get("conversation_id") == Some(&json!(CID)) && j.get("ask").is_some()
            })
    }));
    assert!(not_ok(&client.handle(
        "agentcard_buy",
        &json!({"ask": "Make it two", "conversation_id": CID})
    )));
    assert!(journal(&client)["conversations"][CID]["shopping_pending"].is_string());
    assert!(prepare(&client).ok());
    assert!(journal(&client)["conversations"][CID]["shopping_pending"].is_null());
    *server.checkout.lock() = Checkout::Fail(TOKEN.into());
    assert_eq!(
        client.confirm(&snapshot(prepare(&client)), None)["status"],
        json!("uncertain")
    );
    assert!(!try_prepare(&client, &[H1]).ok());
    assert_eq!(server.confirms().len(), 1);
}

#[test]
fn test_409_cart_changed_requires_new_hash_preview_and_new_owner_snapshot() {
    let dir = tmp();
    let (client, server) = service(dir.path());
    start(&client);
    let old = snapshot(prepare(&client));
    let mut new_cart = cart_of(H2, "retail", "Amazon");
    new_cart["items"][0]["priceCents"] = json!(125);
    new_cart["totalCents"] = json!(265);
    server.set_checkout(
        409,
        json!({"conversation_id": CID, "carts": [new_cart], "cart": new_cart}),
    );
    let result = client.confirm(&old, None);
    assert!(
        not_ok(&result)
            && result["carts"][0]["hash"] == json!(H2)
            && result["charge_status"] == json!("none")
    );
    assert!(not_ok(&client.confirm(&old, None)));
    server.set_ledger(ledger_with(Some(vec![new_cart]), json!({})));
    assert!(!try_prepare(&client, &[H1]).ok());
    let fresh = prepare_hashes(&client, &[H2]);
    assert!(fresh.result["question"]
        .as_str()
        .unwrap()
        .contains("265 cents"));
    assert_eq!(server.confirms().len(), 1);
    server.set_checkout(200, placed(json!({"order_id": "order_new"})));
    assert_eq!(client.confirm(&snapshot(fresh), None)["ok"], json!(true));
    let sent: Vec<Value> = server
        .confirms()
        .iter()
        .map(|p| p["confirm"].clone())
        .collect();
    assert_eq!(sent, vec![json!(H1), json!(H2)]);
}

#[test]
fn test_default_transport_fixed_origin_no_redirect_no_environment_proxy_and_closes() {
    // The client always addresses the fixed origin with the bearer token and the configured timeout.
    let dir = tmp();
    let seen: Arc<Mutex<Vec<HttpRequest>>> = Arc::default();
    let transport: Arc<dyn Transport> = {
        let seen = seen.clone();
        Arc::new(move |r: HttpRequest| {
            seen.lock().push(r);
            Ok(HttpResponse::json(
                200,
                &json!({"merchants": [{"slug": "retail", "name": "Retail", "link_status": "ready"}]}),
            ))
        })
    };
    let client = AgentCard::new(
        AgentCardConfig::new(true, TOKEN),
        Some(transport),
        Some(dir.path().to_path_buf()),
    )
    .unwrap();
    assert_eq!(
        client.handle("agentcard_merchants", &json!({}))["ok"],
        json!(true)
    );
    let calls = seen.lock();
    assert_eq!(
        (calls[0].method.as_str(), calls[0].url.as_str()),
        ("GET", "https://api.agentcard.sh/buy/merchants")
    );
    assert_eq!(calls[0].timeout, 180.0);
    assert!(calls[0]
        .headers
        .contains(&("Authorization".into(), format!("Bearer {TOKEN}"))));

    // The default transport does not follow a redirect and closes its connection.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = [0u8; 4096];
        let n = stream.read(&mut buf).unwrap();
        let request = String::from_utf8_lossy(&buf[..n]).to_string();
        stream
            .write_all(b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:9/elsewhere\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
            .unwrap();
        request
    });
    let response = HttpTransport
        .request(HttpRequest {
            method: "GET".into(),
            url: format!("http://127.0.0.1:{port}/buy/merchants"),
            headers: vec![("Authorization".into(), format!("Bearer {TOKEN}"))],
            json: None,
            timeout: 5.0,
        })
        .unwrap();
    assert_eq!(response.status, 302);
    let request = server.join().unwrap();
    assert!(request.starts_with("GET /buy/merchants HTTP/1.1"));
    assert!(request
        .to_lowercase()
        .contains(&format!("authorization: bearer {}", TOKEN.to_lowercase())));
}

#[test]
fn test_conflicting_same_hash_cart_envelope_is_not_approved() {
    let dir = tmp();
    let (client, server) = service(dir.path());
    start(&client);
    {
        let mut l = server.ledger.lock();
        let mut c = cart();
        c["items"][0]["name"] = json!("Different purchase with same claimed hash");
        l["cart"] = c;
    }
    assert!(!try_prepare(&client, &[H1]).ok());
    assert!(server.confirms().is_empty());
}

#[test]
fn test_malformed_confirm_charge_fields_are_uncertain_and_require_get() {
    for (status, charge) in [
        ("needs_input", None),
        ("declined", None),
        ("declined", Some(json!("future_invalid_charge"))),
        ("declined", Some(Value::Null)),
    ] {
        let dir = tmp();
        let (client, server) = service(dir.path());
        start(&client);
        let mut body = json!({"conversation_id": CID, "status": status});
        if let Some(charge) = charge {
            body["charge_status"] = charge;
        }
        server.set_checkout(200, body);
        let result = client.confirm(&snapshot(prepare(&client)), None);
        assert!(
            not_ok(&result)
                && result["status"] == json!("uncertain")
                && result["charge_status"] == json!("unknown")
        );
        assert_eq!(result["reconciliation_required"], json!(true));
        let reads_before = server.gets();
        assert!(!try_prepare(&client, &[H1]).ok());
        assert!(server.gets() > reads_before);
        assert_eq!(server.confirms().len(), 1);
    }
}

#[test]
fn test_inflight_cancel_and_close_preserve_actual_order_outcome() {
    let dir = tmp();
    let (client, server) = service(dir.path());
    start(&client);
    let snap = snapshot(prepare(&client));
    let cancelled = Arc::new(AtomicBool::new(false));
    {
        let (client, cancelled) = (client.clone(), cancelled.clone());
        *server.before_checkout.lock() = Some(Arc::new(move |_payload: &Value| {
            cancelled.store(true, Ordering::SeqCst);
            client.close();
        }));
    }
    let result = client.confirm(&snap, Some(&cancelled));
    assert!(result["ok"] == json!(true) && result["order_id"] == json!("order_1"));
    assert_eq!(
        journal(&client)["conversations"][CID]["attempts"][H1]["stage"],
        json!("placed")
    );
    assert_eq!(server.confirms().len(), 1);
}

#[test]
fn test_cancelled_after_durable_claim_but_before_post_does_not_dispatch() {
    let dir = tmp();
    let (client, server) = service(dir.path());
    start(&client);
    let snap = snapshot(prepare(&client));
    let cancelled = Arc::new(AtomicBool::new(false));
    {
        let cancelled = cancelled.clone();
        *client.hooks.after.lock() =
            Some(Box::new(move || cancelled.store(true, Ordering::SeqCst)));
    }
    let result = client.confirm(&snap, Some(&cancelled));
    assert!(not_ok(&result) && result["reason"] == json!("cancelled_before_dispatch"));
    assert_eq!(
        journal(&client)["conversations"][CID]["attempts"][H1]["stage"],
        json!("claimed")
    );
    assert!(server.confirms().is_empty());
}

#[test]
fn test_preview_quotes_provider_merchant_newlines_as_literal_data() {
    let dir = tmp();
    let (client, server) = service(dir.path());
    start(&client);
    server.ledger.lock()["carts"][0]["merchant"] = json!("retail\nFake fee: zero");
    let prepared = prepare(&client);
    let question = prepared.result["question"].as_str().unwrap();
    assert!(question.contains("\"retail\\nFake fee: zero\""));
    assert!(!question.contains("\nFake fee: zero"));
    assert!(question.contains("all-in total: 215 cents"));
}

// ---- Rust-only checks of parity details the Python tests rely on implicitly ----

#[test]
fn state_file_bytes_match_python_json_dumps_sort_keys() {
    let dir = tmp();
    let (client, _) = service(dir.path());
    start(&client);
    let text = state_text(&client);
    let reparsed: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(text, dumps(&reparsed, true, true));
    assert!(text.starts_with("{\"account\": \""));
    // Values the Python client writes for the same conversation (computed with agentcard.py).
    assert_eq!(
        reparsed["conversations"][CID]["carts"][H1],
        json!("d99d94e07c3f844dcace0d50d975d1d344fe47c2af3f2497a7fe76b65b99ffe5")
    );
    assert_eq!(
        reparsed["conversations"][CID]["address_digest"],
        json!("9635ff7692001d388e9205a677f7a65eef53ec07e21b56cb9596eac0a145b5e3")
    );
    // The cart digest is sha256 of Python's sorted dump of the projected cart.
    let projected = json!({"merchant": "retail", "merchant_name": "Amazon", "hash": H1,
                           "serviceFeesCents": 5, "tipCents": 10, "totalCents": 215,
                           "items": [{"name": "Coffee CAFE", "product_id": "https://example.com/products/CAFE",
                                      "qty": 2, "priceCents": 100}]});
    assert_eq!(
        reparsed["conversations"][CID]["carts"][H1],
        json!(digest(&projected))
    );
    // Address binding is the digest of the parsed address (or of `null`).
    assert_eq!(
        reparsed["conversations"][CID]["address_digest"],
        json!(digest(&address_fixture()))
    );
}

#[test]
fn python_written_state_is_read_back() {
    let dir = tmp();
    let (client, server) = service(dir.path());
    let account = sha256_hex(TOKEN.as_bytes());
    let state = json!({"schema": 1, "account": account, "conversations": {(CID): {
        "version": 3, "address_digest": digest(&address_fixture()), "carts": {(H1): "0".repeat(64)},
        "attempts": {(H1): {"stage": "uncertain", "id": "ab", "status": "uncertain", "charge_status": "unknown",
                          "order_ids": []}},
        "shopping_pending": null}}});
    fs::create_dir_all(client.state_dir()).unwrap();
    fs::write(client.state_path(), dumps(&state, true, true)).unwrap();
    let status = client.handle("agentcard_status", &json!({"conversation_id": CID}));
    assert_eq!(status["ok"], json!(true), "{status}");
    let result = try_prepare(&client, &[H1]).result;
    assert_eq!(result["reason"], json!("cart_already_attempted"));
    assert!(server.confirms().is_empty());
}

#[test]
fn text_redacts_token_address_card_and_bearer() {
    let dir = tmp();
    let (client, _) = service(dir.path());
    let address = address_fixture();
    let payment = json!({"brand": "visa", "last4": "4832"});
    let text = client
        .text(
            Some(&json!(format!(
                "{} at 91 SECRET sentinel street, Visa 4832 bearer abc;\u{1}x ca",
                TOKEN.to_lowercase()
            ))),
            address.as_object(),
            Some(&payment),
            4000,
        )
        .unwrap();
    assert_eq!(
        text,
        "[redacted] at [redacted], [redacted] [redacted] Bearer [redacted];x ca"
    );
}

#[test]
fn address_validation_matches_python() {
    assert!(address(&address_fixture()).is_ok());
    assert!(
        address(&json!({"street": "1 A St", "city": "B", "state": "BC", "zip": "V5K 0A1"})).is_ok()
    );
    assert!(
        address(&json!({"street": "1 A St", "city": "B", "state": "C", "zip": "94123"})).is_err()
    );
    assert!(
        address(&json!({"street": "1 A St", "city": "B", "state": "CA", "zip": "9412"})).is_err()
    );
    assert!(address(&json!({"street": "1 A St", "city": "B", "state": "CA", "zip": "94123", "can_leave_at_door": 1})).is_err());
    assert!(address(&json!({"street": "1 A St", "city": "B", "state": "CA", "zip": "94123", "can_leave_at_door": true})).is_ok());
    assert!(address(
        &json!({"street": "1 A St", "city": "B", "state": "CA", "zip": "94123", "extra": "x"})
    )
    .is_err());
    assert!(
        address(&json!({"street": "1\nA St", "city": "B", "state": "CA", "zip": "94123"})).is_err()
    );
}

/// The client half of `test_telegram_agentcard.py::test_real_client_through_chat_waits_for_owner_then_sends_exact_vault_request`
/// (the owner question and button live in the Telegram inlet's port).
#[test]
fn test_real_client_through_chat_waits_for_owner_then_sends_exact_vault_request() {
    let token = "PRIVATE_AGENTCARD_ACCESS_TOKEN_SENTINEL";
    let address = json!({"street": "123 PrivateAddressSentinel Road", "city": "PrivateCitySentinel",
                         "state": "CA", "zip": "94107", "country": "US", "phone": "+15555550123"});
    let (cid, cart_hash) = ("conv_cafe", "cafecafecafecafe");
    let cart = json!({"merchant": "retail", "merchant_name": "Retail", "hash": cart_hash,
                      "items": [{"name": "Printer paper", "qty": 1, "priceCents": 500, "product_id": "paper-ca"}],
                      "serviceFeesCents": 20, "tipCents": 0, "totalCents": 520});
    let requests: Arc<Mutex<Vec<(String, Option<Value>)>>> = Arc::default();
    let transport: Arc<dyn Transport> = {
        let (requests, address, cart) = (requests.clone(), address.clone(), cart.clone());
        Arc::new(move |r: HttpRequest| {
            assert!(r.url.starts_with(&format!("{API_ORIGIN}/buy")));
            assert!(r.headers.contains(&("Authorization".into(), format!("Bearer {token}"))));
            requests.lock().push((r.method.clone(), r.json.clone()));
            let body = if r.method == "GET" {
                json!({"conversation_id": cid, "status": "open", "turn_in_progress": false, "orders": [],
                       "last_checkout": null, "carts": [cart], "unmatched": []})
            } else if r.json.as_ref().is_some_and(|j| j.get("ask").is_some()) {
                json!({"conversation_id": cid, "status": "needs_input", "cart": cart, "carts": [cart],
                       "unmatched": [], "catalog": null,
                       "reply": format!("Paper is ready for {}; {token}", address["street"].as_str().unwrap())})
            } else {
                json!({"conversation_id": cid, "status": "order_placed", "order_id": "order-safe",
                       "charge_status": "settled", "carts": [], "cart": null, "unmatched": [],
                       "placements": null, "payment_source": {"brand": "visa", "last4": "4832"},
                       "reply": format!("Charged Visa ending 4832; {}; {token}", address["phone"].as_str().unwrap())})
            };
            Ok(HttpResponse::json(200, &body))
        })
    };
    let dir = tmp();
    let config = AgentCardConfig { address_json: Some(address.to_string()), ..AgentCardConfig::new(true, token) };
    let client = AgentCard::new(config, Some(transport), Some(dir.path().join("state"))).unwrap();
    let built = client.handle("agentcard_buy", &json!({"ask": "printer paper", "conversation_id": null}));
    assert!(built["ok"] == json!(true) && built["conversation_id"] == json!(cid));
    assert_eq!(built["carts"][0]["hash"], json!(cart_hash));
    let prepared = client.prepare_confirm(&json!(cid), &json!([cart_hash]));
    assert!(!requests.lock().iter().any(|(_, p)| p.as_ref().is_some_and(|p| p.get("confirm").is_some())));
    let preview = prepared.result["question"].as_str().unwrap().to_string();
    assert!(preview.contains("Printer paper") && preview.contains("520"));
    let result = client.confirm(prepared.snapshot.as_ref().unwrap(), Some(&AtomicBool::new(false)));
    assert!(result["status"] == json!("order_placed") && result["order_id"] == json!("order-safe"));
    let confirms: Vec<Value> = requests
        .lock()
        .iter()
        .filter_map(|(_, p)| p.clone())
        .filter(|p| p.get("confirm").is_some())
        .collect();
    assert_eq!(
        confirms,
        vec![json!({"conversation_id": cid, "confirm": cart_hash, "payment_source": "vault", "delivery_address": address})]
    );
    assert_eq!(requests.lock()[0].1.as_ref().unwrap()["delivery_address"], address);
    let projection = format!("{built}{result}");
    let disk = fs::read_to_string(client.state_path()).unwrap();
    for secret in [token, address["street"].as_str().unwrap(), address["phone"].as_str().unwrap(), "4832"] {
        assert!(!projection.contains(secret) && !disk.contains(secret), "{secret}");
    }
    assert!(projection.contains("order-safe") && disk.contains("order-safe"));
    client.close();
}

/// Replays cases recorded from `agentcard.py` (projection, checkout stage
/// ladder, ledger envelope, Vault pause, URL acceptance, redaction) and
/// requires byte-identical output, key order included.
fn replay_differential(cases: &str) -> usize {
    let dir = tmp();
    let (client, _) = service(dir.path());
    let mut count = 0;
    for (line_no, line) in cases.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
        let row: Value = serde_json::from_str(line).unwrap();
        let address = row["address"].as_object();
        let got = match row["kind"].as_str().unwrap() {
            "project" => Value::Object(client.project(row["body"].as_object().unwrap(), address)),
            "checkout" => {
                let merchants: Vec<String> = row["merchants"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|m| m.as_str().unwrap().to_string())
                    .collect();
                let code = row["code"].as_i64();
                let (result, stage) =
                    client.checkout_result(code, row["body"].as_object(), CID, &merchants, address);
                json!([result, stage])
            }
            "ledger" => {
                let body = row["body"].as_object().unwrap();
                json!([AgentCard::ledger_safe(body, CID), AgentCard::vault_pause(body)])
            }
            "url" => json!([
                client.url(Some(&row["value"]), address, false),
                client.url(Some(&row["value"]), address, true)
            ]),
            "text" => json!(client.text(Some(&row["value"]), address, row.get("payment"), 4000)),
            "config" => {
                let env: HashMap<String, String> = row["env"]
                    .as_object()
                    .unwrap()
                    .iter()
                    .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string()))
                    .collect();
                match configured(Some(&env)) {
                    Ok(c) if row["out"].get("enabled").is_some() => {
                        json!({"enabled": c.enabled, "token": c.token, "timeout": c.timeout_secs})
                    }
                    Ok(c) => json!({"timeout": c.timeout_secs}),
                    Err(e) => json!({"error": e.to_string()}),
                }
            }
            "address" => match super::address(&row["value"]) {
                Ok(a) => json!({"ok": a}),
                Err(e) => json!({"error": e.to_string()}),
            },
            other => panic!("unknown case kind {other}"),
        };
        assert_eq!(
            dumps(&got, false, false),
            dumps(&row["out"], false, false),
            "case {} ({}): {}",
            line_no + 1,
            row["kind"],
            line
        );
        count += 1;
    }
    count
}

#[test]
fn differential_cases_recorded_from_python_match() {
    let cases = include_str!("../tests/data/differential.jsonl");
    assert!(replay_differential(cases) >= 200);
}

/// A larger recorded set: `AGENTCARD_DIFF_CASES=/path/cases.jsonl cargo test -p eve-agentcard -- --ignored`.
#[test]
#[ignore = "needs AGENTCARD_DIFF_CASES pointing at a recorded case file"]
fn differential_cases_from_file_match() {
    let path = std::env::var("AGENTCARD_DIFF_CASES").expect("AGENTCARD_DIFF_CASES");
    let cases = fs::read_to_string(path).unwrap();
    assert!(replay_differential(&cases) > 0);
}

/// Python raises `TypeError` (two `reason`s) when the Vault retry's fresh
/// ledger read fails; nothing is claimed or sent.
#[test]
fn vault_retry_with_unavailable_ledger_raises_before_dispatch() {
    let dir = tmp();
    let (client, server) = service(dir.path());
    start(&client);
    server.set_checkout(
        200,
        json!({"conversation_id": CID, "status": "declined", "decline_code": "vault_approval_required",
               "charge_status": "none"}),
    );
    client.confirm(&snapshot(prepare(&client)), None);
    server.set_ledger(ledger_with(
        None,
        json!({"last_checkout": {"status": "needs_approval", "charge_status": "none",
                                 "decline_code": "vault_approval_required"}}),
    ));
    let snap = snapshot(prepare(&client));
    *server.fail_when.lock() = Some(Arc::new(|r: &HttpRequest| r.method == "GET"));
    assert_eq!(client.try_confirm(&snap, None), Err(ConfirmRaised));
    assert_eq!(server.confirms().len(), 1);
    assert_eq!(
        journal(&client)["conversations"][CID]["attempts"][H1]["stage"],
        json!("vault_pause")
    );
    // The Value-returning form folds the exception into a refusal.
    let snap = {
        *server.fail_when.lock() = None;
        snapshot(prepare(&client))
    };
    *server.fail_when.lock() = Some(Arc::new(|r: &HttpRequest| r.method == "GET"));
    let result = client.confirm(&snap, None);
    assert!(not_ok(&result) && result["reason"] == json!("vault_retry_requires_reconciliation"));
    assert_eq!(server.confirms().len(), 1);
}

#[test]
fn state_directory_parents_keep_the_default_mode() {
    let dir = tmp();
    let server = Server::new();
    let config = AgentCardConfig::new(true, TOKEN);
    let transport: Arc<dyn Transport> = server.clone();
    let client =
        AgentCard::new(config, Some(transport), Some(dir.path().join("parent/agentcard"))).unwrap();
    assert_eq!(client.handle("agentcard_status", &json!({"conversation_id": CID}))["ok"], json!(true));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir.path().join("parent/agentcard")), 0o700);
        assert_ne!(mode(&dir.path().join("parent")), 0o700);
    }
}

#[tokio::test]
async fn default_transport_called_inside_a_runtime_blocks_instead_of_panicking() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = [0u8; 4096];
        let _ = stream.read(&mut buf).unwrap();
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
            .unwrap();
    });
    let response = HttpTransport
        .request(HttpRequest {
            method: "GET".into(),
            url: format!("http://127.0.0.1:{port}/buy/merchants"),
            headers: vec![],
            json: None,
            timeout: 5.0,
        })
        .unwrap();
    assert_eq!((response.status, response.body.as_slice()), (200, &b"{}"[..]));
    server.join().unwrap();
}

/// Python's `str.strip()` also removes U+001C..U+001F; Rust's `trim` keeps them.
#[test]
fn python_strip_removes_ascii_separators() {
    let config = configured(Some(&env(&[
        ("EVE_AGENTCARD", "1\u{1c}"),
        ("AGENTCARD_ACCESS_TOKEN", "\u{1d}tok\u{1f}"),
    ])))
    .unwrap();
    assert!(config.enabled);
    assert_eq!(config.token, "tok");

    let dir = tmp();
    let (client, server) = service(dir.path());
    let result = client.handle(
        "agentcard_buy",
        &json!({"ask": "\u{1c}\u{1f}", "conversation_id": null}),
    );
    assert_eq!(result["reason"], json!("invalid_ask"));
    assert!(server.calls.lock().is_empty());

    let mut bad = cart();
    bad["merchant_name"] = json!("\u{1e}");
    let body = shopping(Some(vec![bad]));
    let projected = client.project(body.as_object().unwrap(), None);
    assert_eq!(projected["carts"], json!([]));
    assert_eq!(projected["cart_validation_error"], json!(true));
}
