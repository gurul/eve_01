//! test_telegram_agentcard.py: AgentCard in the chat (question routing, owner checks, the optional service seam).
//!
//! Python's `Rig(..., agentcard=service)` lends a duck-typed client; here it is a [`CardService`] wrapped in
//! [`CardPurchases`], the inlet's `agentcard` and `_purchases` both. `telegram._FROM_CALL.set(spoken.append)` is a
//! [`TurnCtx`] with that reader as its call, scoped around the work. The client's calls run on blocking workers, so the
//! waits below are real-time (`until`, 3 s), as the Python ones are.
//!
//! Not ported here: `test_daemon_prerequisites_factory_and_safe_self_context`'s daemon half (`Daemon._make_telegram`,
//! eve-daemon's) — its `self_context.lines` half is below; `test_spoken_answer_cannot_move_to_a_new_question`'s
//! `delayed_judge=True` case, which replaces the bound method `_judge_answer` on the instance (Rust has no such seam).

mod support;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde_json::{json, Map, Value};

use eve_agentcard_tools::Prepared;
use eve_brain::prelude::BrainMemory;
use eve_brain::seams::{in_ctx, CardPurchases, CardService, Pending, Purchases, Reader, TurnCtx};
use eve_brain::telegram::{message_item, spoken_question, Inbound, TelegramConfig, GATE_TITLE, YES_NO};
use support::*;

const STRANGER: i64 = 666;
const PREVIEW: &str = "PRIVATE_PREVIEW: Paper, quantity 1. Total $5.00. Purchase this cart?";
const SNAPSHOT: u64 = 7;

/// `ARGS`.
fn args() -> Obj {
    obj(json!({"conversation_id": "conversation-1", "cart_hashes": ["0123456789abcdef"]}))
}

fn obj(v: Value) -> Obj {
    v.as_object().cloned().unwrap_or_default()
}

/// The Python file's `Service`.
struct Service {
    reads: Mutex<Vec<(String, Value)>>,
    confirmed: Mutex<Vec<u64>>,
    question: Mutex<String>,
}

impl Service {
    fn new() -> Arc<Self> {
        Arc::new(Self { reads: Mutex::new(Vec::new()), confirmed: Mutex::new(Vec::new()), question: Mutex::new(PREVIEW.into()) })
    }
    fn confirmed(&self) -> usize {
        self.confirmed.lock().len()
    }
}

impl eve_agentcard_tools::Service for Service {
    type Snapshot = u64;

    fn handle(&self, name: &str, args: Value) -> anyhow::Result<Value> {
        self.reads.lock().push((name.to_string(), args));
        Ok(json!({"ok": true, "status": "needs_input", "reply": "SAFE_CART_REPLY"}))
    }

    fn prepare_confirm(&self, _conversation_id: &str, _cart_hashes: &[String]) -> anyhow::Result<Prepared<u64>> {
        Ok(Prepared {
            result: json!({"ok": true, "question": self.question.lock().clone(),
                           "about": "AgentCard: asked for approval of a cart totalling $5.00."}),
            snapshot: Some(SNAPSHOT),
        })
    }

    fn confirm(&self, snapshot: u64, cancelled: &Arc<AtomicBool>) -> anyhow::Result<Value> {
        assert_eq!(snapshot, SNAPSHOT);
        assert!(!cancelled.load(Ordering::SeqCst));
        self.confirmed.lock().push(snapshot);
        Ok(json!({"ok": true, "status": "order_placed", "order_id": "safe-order"}))
    }
}

impl CardService for Service {
    fn tools(&self) -> Vec<Value> {
        eve_agentcard::TOOL_NAMES
            .iter()
            .map(|name| json!({"type": "function", "name": name, "strict": true,
                               "parameters": {"type": "object", "properties": {}, "required": [],
                                              "additionalProperties": false}}))
            .collect()
    }
    fn instructions(&self) -> String {
        "AgentCard: use agentcard_confirm for the exact cart; the owner approves it.".into()
    }
}

fn card<S: CardService>(service: &Arc<S>) -> Arc<dyn Purchases> {
    CardPurchases::new(service.clone())
}

/// `Rig(api, create, config, agentcard=service, memory=memory)`.
fn rig_with(api: Arc<FakeApi>, create: Arc<FakeCreate>, config: TelegramConfig, purchases: Option<Arc<dyn Purchases>>,
            memory: Option<Arc<dyn BrainMemory>>) -> Rig {
    Rig::new(api, create, config, move |o| {
        o.agentcard = purchases;
        o.memory = memory;
    })
}

fn rig(service: &Arc<Service>, create: Arc<FakeCreate>) -> Rig {
    rig_with(FakeApi::new(vec![]), create, config(), Some(card(service)), None)
}

/// test_telegram.py's `say(text)`.
fn say(text: &str) -> Value {
    reply(text)
}

/// test_telegram.py's `call(name, args)`.
fn pcall(name: &str, args: &Obj) -> Value {
    call(name, Value::Object(args.clone()), "call_1")
}

/// `until(predicate)`: real time, 3 s at most.
async fn until(mut predicate: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(3);
    while !predicate() {
        assert!(Instant::now() < end, "integration did not reach expected state");
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}

fn keyboards(rig: &Rig) -> usize {
    rig.api.log.lock().keyboards.len()
}

fn hear(rig: &Rig, text: &str) {
    rig.inlet.hear(text, eve_phone_call::Captured::Uncaptured, true, None, None, "auto", &[]);
}

fn dispatch(rig: &Rig, update: Obj) {
    rig.inlet.say_relay_probe().dispatch(&Value::Object(update));
}

/// A tap on the last keyboard's "Yes".
fn tap_yes(rig: &Rig, update_id: i64) {
    let (mid, key) = rig.api.key("Yes", -1);
    dispatch(rig, tap_update(update_id, mid, &key, OWNER, &format!("q{update_id}")));
}

fn inbound(text: &str, message_id: i64) -> Inbound {
    Inbound { chat_id: OWNER, user_id: OWNER, text: text.into(), message_id, ..Inbound::default() }
}

/// `asyncio.create_task(rig.inlet._tool(name, args, OWNER))`, inside `ctx`.
fn tool_job(rig: &Rig, name: &'static str, args: Obj, ctx: TurnCtx) -> tokio::task::JoinHandle<Obj> {
    let inlet = rig.inlet.clone();
    tokio::spawn(in_ctx(ctx, async move { inlet.tt_tool(name, &args, OWNER).await }))
}

fn ok(result: &Obj) -> bool {
    result.get("ok").and_then(Value::as_bool).unwrap_or(false)
}

async fn joined(job: tokio::task::JoinHandle<Obj>) -> Obj {
    tokio::time::timeout(Duration::from_secs(3), job).await.expect("in 3 s").expect("the job")
}

/// `await clean(inlet)`.
async fn clean(rig: &Rig) {
    rig.inlet.say_relay_probe().shutdown().await;
    tokio::task::yield_now().await;
}

/// `spoken: list[str] = []` and `spoken.append`, as a call's reader.
fn spoken() -> (Reader, Arc<Mutex<Vec<String>>>) {
    let read = Arc::new(Mutex::new(Vec::new()));
    let r = read.clone();
    (Reader::new(move |text: &str| r.lock().push(text.to_string())), read)
}

fn on_call(reader: &Reader) -> TurnCtx {
    TurnCtx { call: Some(reader.clone()), ..TurnCtx::default() }
}

#[tokio::test]
async fn test_long_purchase_preview_is_read_completely_on_the_call() {
    let service = Service::new();
    let mut preview: Vec<String> = Vec::new();
    for merchant in 0..8 {
        for item in 0..12 {
            preview.push(format!("Merchant {merchant}: Item {item}, quantity 2, API price 521 cents."));
        }
    }
    let preview = preview.join("\n") + "\nFinal total 50016 API cents. Purchase these carts?";
    *service.question.lock() = preview.clone();
    let rig = rig(&service, FakeCreate::new(vec![]));
    let (reader, read) = spoken();
    rig.inlet.tt_set_call_say(Some(reader.clone()));
    let task = tool_job(&rig, "agentcard_confirm", args(), on_call(&reader));
    until(|| keyboards(&rig) > 0).await;
    let rendered: Vec<String> = read
        .lock()
        .iter()
        .flat_map(|piece| eve_phone_call::sentences(&eve_phone_call::speakable(piece), 220))
        .collect();
    let rendered = rendered.join(" ");
    let want = spoken_question(&preview, &YES_NO).split_whitespace().collect::<Vec<_>>().join(" ");
    assert_eq!(rendered, want);
    assert!(!rendered.contains("The rest is in the chat"));
    assert_eq!(service.confirmed(), 0);
    hear(&rig, "no");
    assert!(!ok(&joined(task).await));
    clean(&rig).await;
}

#[tokio::test]
async fn test_optional_registration_dispatch_and_no_executor_fallback() {
    let off = Rig::new(FakeApi::new(vec![]), FakeCreate::new(vec![]), config(), |_| {});
    let (parts, allowed, _) = off.inlet.turn_parts(0, false).await;
    let names = |parts: &Obj| -> Vec<String> {
        parts.get("watch_tools").and_then(Value::as_array).into_iter().flatten()
            .filter_map(|t| t.get("name").and_then(Value::as_str).map(str::to_string)).collect()
    };
    let block = |parts: &Obj| parts.get("watch_block").and_then(Value::as_str).unwrap_or("").to_string();
    for name in eve_agentcard::TOOL_NAMES {
        assert!(!allowed.iter().any(|a| a == name));
        assert!(!names(&parts).iter().any(|a| a == name));
    }
    assert!(!block(&parts).contains("AgentCard"));
    for name in eve_agentcard::TOOL_NAMES {
        let result = off.inlet.tt_tool(name, &Map::new(), OWNER).await;
        assert_eq!(Value::Object(result), json!({"ok": false, "reason": eve_agentcard::OFF_REASON}));
    }
    assert!(off.create.requests.lock().is_empty());
    let service = Service::new();
    let on = rig(&service, FakeCreate::new(vec![]));
    let (parts, allowed, _) = on.inlet.turn_parts(0, false).await;
    for name in eve_agentcard::TOOL_NAMES {
        assert!(allowed.iter().any(|a| a == name), "{name} allowed");
        assert!(names(&parts).iter().any(|a| a == name), "{name} lent");
    }
    assert!(block(&parts).contains("agentcard_confirm"));
    let result = on.inlet.tt_tool("agentcard_buy", &obj(json!({"ask": "paper", "conversation_id": null})), OWNER).await;
    assert_eq!(result.get("reply"), Some(&json!("SAFE_CART_REPLY")));
    assert_eq!(service.reads.lock().len(), 1);
    assert!(!ok(&on.inlet.tt_tool("agentcard_buy", &Map::new(), STRANGER).await));
    assert_eq!(service.reads.lock().len(), 1);
    clean(&off).await;
    clean(&on).await;
}

// ---- the memory the purchase is written to: memory.Memory around a real Transcripts --------------------------

struct FakeMemory {
    transcripts: Arc<eve_transcripts::Transcripts>,
}

impl FakeMemory {
    fn new(root: &std::path::Path) -> Arc<Self> {
        std::fs::create_dir_all(root).unwrap();
        let config = eve_transcripts::TranscriptConfig {
            enabled: true,
            root: root.join("transcripts"),
            home: Some(root.to_path_buf()),
        };
        Arc::new(Self { transcripts: Arc::new(eve_transcripts::Transcripts::new(config, None, None)) })
    }
}

impl BrainMemory for FakeMemory {
    fn transcripts(&self) -> Arc<eve_transcripts::Transcripts> {
        self.transcripts.clone()
    }
    fn profile(&self) -> String {
        String::new()
    }
    fn profile_for_voice(&self) -> String {
        String::new()
    }
    fn today(&self, max_chars: i64, options: &eve_transcripts::Today) -> String {
        self.transcripts.today_block_with(max_chars, options)
    }
    fn star(&self, _text: &str) -> Option<String> {
        None
    }
    fn tools(&self, _voice: bool) -> Vec<Value> {
        Vec::new()
    }
    fn handle_tool(&self, _name: &str, _args: &Value, _since: Option<eve_transcripts::Dt>, _channel: &str) -> Value {
        json!({"ok": true})
    }
}

/// Every file under `root` whose name has `.json` in it, read and joined (`rglob("*.json*")`).
fn disk(root: &std::path::Path) -> String {
    fn walk(dir: &std::path::Path, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.contains(".json")) {
                out.push(std::fs::read_to_string(&path).unwrap_or_default());
            }
        }
    }
    let mut out = Vec::new();
    walk(root, &mut out);
    out.join("\n")
}

#[tokio::test]
async fn test_purchase_preview_is_firm_spoken_and_only_safe_about_enters_model_history() {
    let dir = tmp();
    let service = Service::new();
    let api = FakeApi::new(vec![]);
    let create = FakeCreate::new(vec![pcall("agentcard_confirm", &args()), say("Placed safe-order.")]);
    let memory: Arc<dyn BrainMemory> = FakeMemory::new(dir.path());
    let rig = rig_with(api.clone(), create.clone(), config(), Some(card(&service)), Some(memory));
    let (reader, read) = spoken();
    rig.inlet.tt_set_call_say(Some(reader.clone()));
    let inlet = rig.inlet.clone();
    let turn = tokio::spawn(in_ctx(on_call(&reader), async move { inlet.tt_turn(&inbound("buy paper", 1)).await }));
    until(|| keyboards(&rig) > 0).await;
    let asked = rig.inlet.tt_live_question().expect("a live question");
    assert!(asked.firm);
    assert_eq!(asked.words, ["yes", "no"].iter().map(|w| w.to_string()).collect::<std::collections::HashSet<_>>());
    assert!(read.lock().join(" ").contains("PRIVATE_PREVIEW"));
    assert_eq!(service.confirmed(), 0);
    dispatch(&rig, update(1, "yes", STRANGER, NOW, 5));
    hear(&rig, "yes, but buy the cheaper one");
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert_eq!(service.confirmed(), 0);
    assert!(!asked.future.done());
    assert_eq!(create.requests.lock().len(), 1, "firm approval never calls the model to judge an answer");
    hear(&rig, "yes");
    tokio::time::timeout(Duration::from_secs(3), turn).await.expect("in 3 s").unwrap();
    assert_eq!(service.confirmed(), 1);
    tap_yes(&rig, 2);
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert_eq!(service.confirmed(), 1);
    let model_input = serde_json::to_string(&*create.requests.lock()).unwrap();
    let history = serde_json::to_string(&rig.inlet.turns()).unwrap();
    assert!(!(model_input.clone() + &history).contains("PRIVATE_PREVIEW"));
    assert!(model_input.contains("safe-order") && history.contains("$5.00"));
    let saved = disk(&dir.path().join("transcripts"));
    assert!(!saved.contains("PRIVATE_PREVIEW"));
    assert!(saved.contains("safe-order") && saved.contains("$5.00"));
    clean(&rig).await;
}

async fn denial_or_stop_withdraws_purchase(answer: &str, said: bool) {
    let service = Service::new();
    let rig = rig(&service, FakeCreate::new(vec![]));
    let job = tool_job(&rig, "agentcard_confirm", args(), TurnCtx::default());
    until(|| keyboards(&rig) > 0).await;
    if said {
        hear(&rig, answer);
    } else {
        rig.inlet.tt_handle(&Inbound { chat_id: OWNER, user_id: OWNER, text: answer.into(), ..Inbound::default() });
    }
    let result = joined(job).await;
    assert!(!ok(&result) && service.confirmed() == 0);
    assert!(rig.inlet.tt_live_question().is_none());
    clean(&rig).await;
}

#[tokio::test]
async fn test_denial_or_stop_withdraws_purchase_no_typed() {
    denial_or_stop_withdraws_purchase("no", false).await;
}

#[tokio::test]
async fn test_denial_or_stop_withdraws_purchase_stop_typed() {
    denial_or_stop_withdraws_purchase("stop", false).await;
}

#[tokio::test]
async fn test_denial_or_stop_withdraws_purchase_stop_spoken() {
    denial_or_stop_withdraws_purchase("stop", true).await;
}

async fn stop_after_yes_before_dispatch_also_withdraws_queued_confirmations(said: bool, relay_listing: bool) {
    let service = Service::new();
    let rig = rig(&service, FakeCreate::new(vec![]));
    let first = tool_job(&rig, "agentcard_confirm", args(), TurnCtx::default());
    until(|| keyboards(&rig) > 0).await;
    let second = tool_job(&rig, "agentcard_confirm", args(), TurnCtx::default());
    tokio::task::yield_now().await;
    if relay_listing {
        rig.inlet.tt_hold_claude_on(true);
    }
    // A tap resolves its exact future even while a slow relay listing holds typed messages.
    tap_yes(&rig, 1);
    if said {
        hear(&rig, "stop");
    } else {
        rig.inlet.tt_handle(&Inbound { chat_id: OWNER, user_id: OWNER, text: "stop".into(), ..Inbound::default() });
    }
    assert!(!ok(&joined(first).await));
    assert!(!ok(&joined(second).await));
    assert_eq!(service.confirmed(), 0);
    assert_eq!(keyboards(&rig), 1);
    assert_eq!(rig.inlet.tt_held(), 0);
    if relay_listing {
        rig.inlet.tt_hold_claude_on(false);
    }
    clean(&rig).await;
}

#[tokio::test]
async fn test_stop_after_yes_before_dispatch_also_withdraws_queued_confirmations_typed() {
    stop_after_yes_before_dispatch_also_withdraws_queued_confirmations(false, false).await;
}

#[tokio::test]
async fn test_stop_after_yes_before_dispatch_also_withdraws_queued_confirmations_spoken() {
    stop_after_yes_before_dispatch_also_withdraws_queued_confirmations(true, false).await;
}

#[tokio::test]
async fn test_stop_after_yes_before_dispatch_also_withdraws_queued_confirmations_typed_while_relay_lists() {
    stop_after_yes_before_dispatch_also_withdraws_queued_confirmations(false, true).await;
}

#[tokio::test]
async fn test_stop_after_yes_before_dispatch_also_withdraws_queued_confirmations_spoken_while_relay_lists() {
    stop_after_yes_before_dispatch_also_withdraws_queued_confirmations(true, true).await;
}

#[tokio::test]
async fn test_button_send_failure_stays_firm_and_shutdown_withdraws_approval() {
    let service = Service::new();
    let api = FakeApi::new(vec![]);
    *api.fail_inline.lock() = Some(api_error(400, "mock button refusal"));
    let rig = rig_with(api.clone(), FakeCreate::new(vec![]), config(), Some(card(&service)), None);
    let job = tool_job(&rig, "agentcard_confirm", args(), TurnCtx::default());
    until(|| !api.log.lock().sent.is_empty()).await;
    assert!(rig.inlet.tt_live_question().expect("a live question").firm);
    assert!(!rig.inlet.tt_answers_pending("yes, also change the total"));
    clean(&rig).await;
    let result = joined(job).await;
    assert!(!ok(&result) && service.confirmed() == 0);
}

#[tokio::test]
async fn test_other_owner_chat_cannot_answer_and_keyboard_is_single_use() {
    let service = Service::new();
    let config = TelegramConfig { owner_ids: vec![OWNER, 7], ..config() };
    let rig = rig_with(FakeApi::new(vec![]), FakeCreate::new(vec![]), config, Some(card(&service)), None);
    let job = tool_job(&rig, "agentcard_confirm", args(), TurnCtx::default());
    until(|| keyboards(&rig) > 0).await;
    dispatch(&rig, update(1, "yes", 7, NOW, 5));
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert_eq!(service.confirmed(), 0);
    tap_yes(&rig, 2);
    tap_yes(&rig, 3);
    let result = joined(job).await;
    assert_eq!(result.get("status"), Some(&json!("order_placed")));
    assert_eq!(service.confirmed(), 1);
    clean(&rig).await;
}

/// The `delayed_judge=False` case; the other replaces `_judge_answer` on the instance (see the header).
#[tokio::test]
async fn test_spoken_answer_cannot_move_to_a_new_question() {
    let rig = Rig::new(FakeApi::new(vec![]), FakeCreate::new(vec![]), config(), |_| {});
    let old = rig.inlet.tt_open_question(&Pending::new(), Some(OWNER), true, &["yes", "no"], true);
    hear(&rig, "yes");
    let new = rig.inlet.tt_open_question(&Pending::new(), Some(OWNER), true, &["yes", "no"], true);
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert!(!old.future.done() && !new.future.done());
    hear(&rig, "yes"); // control: a new answer to the current question still works
    until(|| new.future.done()).await;
    assert_eq!(new.future.wait().await.ok().as_deref(), Some("yes"));
    assert!(!old.future.done());
    clean(&rig).await;
}

#[tokio::test]
async fn test_cache_warm_and_rundown_never_execute_a_purchase() {
    let service = Service::new();
    let rig = rig(&service, FakeCreate::new(vec![pcall("agentcard_confirm", &args()), say("Your day.")]));
    rig.inlet.warm().await;
    assert!(service.confirmed() == 0 && service.reads.lock().is_empty());
    rig.inlet.tt_turn(&inbound("rundown", 2)).await;
    let requests = rig.create.requests.lock().clone();
    let last = requests.last().expect("a request");
    let lent: Vec<&str> = last.get("tools").and_then(Value::as_array).into_iter().flatten()
        .filter_map(|t| t.get("name").and_then(Value::as_str)).collect();
    assert!(!eve_agentcard::TOOL_NAMES.iter().any(|n| lent.contains(n)));
    assert!(service.confirmed() == 0 && service.reads.lock().is_empty());
    clean(&rig).await;
}

#[tokio::test]
async fn test_real_client_through_chat_waits_for_owner_then_sends_exact_vault_request() {
    let dir = tmp();
    let token = "PRIVATE_AGENTCARD_ACCESS_TOKEN_SENTINEL";
    let address = json!({"street": "123 PrivateAddressSentinel Road", "city": "PrivateCitySentinel",
                         "state": "CA", "zip": "94107", "country": "US", "phone": "+15555550123"});
    let (cid, cart_hash) = ("conv_cafe", "cafecafecafecafe");
    let cart = json!({"merchant": "retail", "merchant_name": "Retail", "hash": cart_hash,
                      "items": [{"name": "Printer paper", "qty": 1, "priceCents": 500, "product_id": "paper-ca"}],
                      "serviceFeesCents": 20, "tipCents": 0, "totalCents": 520});
    let requests: Arc<Mutex<Vec<(String, Option<Value>)>>> = Arc::new(Mutex::new(Vec::new()));
    let (seen, cart2, address2) = (requests.clone(), cart.clone(), address.clone());
    let request = move |request: eve_agentcard::HttpRequest| -> anyhow::Result<eve_agentcard::HttpResponse> {
        assert!(request.url.starts_with(&format!("{}/buy", eve_agentcard::API_ORIGIN)));
        let auth = request.headers.iter().find(|(k, _)| k == "Authorization").map(|(_, v)| v.clone());
        assert_eq!(auth.as_deref(), Some(format!("Bearer {token}").as_str()));
        seen.lock().push((request.method.clone(), request.json.clone()));
        let payload = request.json.clone().unwrap_or(Value::Null);
        let street = address2["street"].as_str().unwrap();
        let phone = address2["phone"].as_str().unwrap();
        let body = if request.method == "GET" {
            json!({"conversation_id": cid, "status": "open", "turn_in_progress": false,
                   "orders": [], "last_checkout": null, "carts": [cart2], "unmatched": []})
        } else if payload.get("ask").is_some() {
            json!({"conversation_id": cid, "status": "needs_input", "cart": cart2,
                   "carts": [cart2], "unmatched": [], "catalog": null,
                   "reply": format!("Paper is ready for {street}; {token}")})
        } else {
            json!({"conversation_id": cid, "status": "order_placed", "order_id": "order-safe",
                   "charge_status": "settled", "carts": [], "cart": null, "unmatched": [],
                   "placements": null, "payment_source": {"brand": "visa", "last4": "4832"},
                   "reply": format!("Charged Visa ending 4832; {phone}; {token}")})
        };
        Ok(eve_agentcard::HttpResponse::json(200, &body))
    };
    let config = eve_agentcard::AgentCardConfig {
        address_json: Some(address.to_string()),
        ..eve_agentcard::AgentCardConfig::new(true, token)
    };
    let client = Arc::new(
        eve_agentcard::AgentCard::new(config, Some(Arc::new(request)), Some(dir.path().join("state"))).expect("a client"),
    );
    let api = FakeApi::new(vec![]);
    let memory: Arc<dyn BrainMemory> = FakeMemory::new(&dir.path().join("memory"));
    let rig = rig_with(api.clone(), FakeCreate::new(vec![]), support::config(), Some(card(&client)), Some(memory));
    let built = rig.inlet.tt_tool("agentcard_buy", &obj(json!({"ask": "printer paper", "conversation_id": null})), OWNER).await;
    assert!(ok(&built), "{built:?}");
    assert_eq!(built.get("conversation_id"), Some(&json!(cid)));
    assert_eq!(built["carts"][0]["hash"], json!(cart_hash));
    let task = tool_job(&rig, "agentcard_confirm", obj(json!({"conversation_id": cid, "cart_hashes": [cart_hash]})),
                        TurnCtx::default());
    until(|| keyboards(&rig) > 0).await;
    let confirms = || -> Vec<Value> {
        requests.lock().iter().filter_map(|(_, p)| p.clone()).filter(|p| p.get("confirm").is_some()).collect()
    };
    assert!(confirms().is_empty());
    let preview = api.log.lock().sent.last().unwrap().1.clone();
    assert!(preview.contains("Printer paper") && preview.contains("520"), "{preview}");
    tap_yes(&rig, 1);
    let result = joined(task).await;
    assert_eq!(result.get("status"), Some(&json!("order_placed")));
    assert_eq!(result.get("order_id"), Some(&json!("order-safe")));
    assert_eq!(confirms(), vec![json!({"conversation_id": cid, "confirm": cart_hash, "payment_source": "vault",
                                       "delivery_address": address})]);
    assert_eq!(requests.lock()[0].1.as_ref().and_then(|p| p.get("delivery_address")), Some(&address));
    let projection = serde_json::to_string(&built).unwrap()
        + &serde_json::to_string(&result).unwrap()
        + &serde_json::to_string(&rig.inlet.turns()).unwrap();
    let on_disk = disk(dir.path());
    for secret in [token, address["street"].as_str().unwrap(), address["phone"].as_str().unwrap(), "4832"] {
        assert!(!(projection.clone() + &on_disk).contains(secret), "{secret} leaked");
    }
    assert!(projection.contains("order-safe") && on_disk.contains("order-safe"));
    clean(&rig).await;
    client.close();
}

/// The `self_context.lines` half of `test_daemon_prerequisites_factory_and_safe_self_context`.
#[test]
fn test_daemon_prerequisites_factory_and_safe_self_context_lines_half() {
    let env: Vec<(&str, String)> = vec![
        ("EVE_TELEGRAM", "1".into()), ("EVE_TELEGRAM_TOKEN", "mock-telegram-token".into()),
        ("EVE_TELEGRAM_OWNER", OWNER.to_string()), ("EVE_WATCH", "0".into()), ("EVE_COMPOSIO", "0".into()),
        ("EVE_APPS", "0".into()), ("EVE_CHIEF", "off".into()), ("EVE_AGENTCARD", "1".into()),
        ("AGENTCARD_ACCESS_TOKEN", "SECRET_TOKEN".into()), ("OPENROUTER_API_KEY", "mock-openrouter".into()),
    ];
    let with = |extra: &[(&str, &str)]| -> Vec<String> {
        let mut map: std::collections::HashMap<String, String> =
            env.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        for (k, v) in extra {
            map.insert(k.to_string(), v.to_string());
        }
        eve_brain::self_context::lines(Some(&map), None, Some("")).expect("lines")
    };
    let base = with(&[("EVE_AGENTCARD", "0")]);
    let on = with(&[]);
    assert!(!base.iter().any(|l| l.contains("AgentCard")));
    assert!(on.iter().any(|l| l.contains("AgentCard")));
    assert!(!on.join("\n").contains("SECRET_TOKEN"));
    let invalid = with(&[("EVE_AGENTCARD_TIMEOUT_SECS", "bad")]);
    assert!(!invalid.iter().any(|l| l.contains("AgentCard")));
    assert!(invalid.iter().any(|l| l.contains("Telegram brain")));
    let invalid_address = with(&[("EVE_AGENTCARD_ADDRESS_FILE", "/nonexistent/agentcard-address.json")]);
    assert!(!invalid_address.iter().any(|l| l.contains("AgentCard")));
    assert!(invalid_address.iter().any(|l| l.contains("Telegram brain")));
}

async fn tainted_chat_purchase_has_only_the_final_cart_approval(approve: bool) {
    let service = Service::new();
    let rig = rig(&service, FakeCreate::new(vec![pcall("agentcard_confirm", &args()), say("Handled.")]));
    let inlet = rig.inlet.clone();
    let task = tokio::spawn(in_ctx(TurnCtx::default(), async move {
        inlet.tt_think(&[message_item("user", "buy paper")], OWNER, "a web search").await
    }));
    until(|| rig.inlet.tt_live_question().is_some()).await;
    let asked = rig.inlet.tt_live_question().unwrap();
    assert!(asked.firm && asked.text.lock().contains("PRIVATE_PREVIEW"));
    assert_eq!(service.confirmed(), 0);
    hear(&rig, if approve { "yes" } else { "no" });
    tokio::time::timeout(Duration::from_secs(3), task).await.expect("in 3 s").unwrap().unwrap();
    assert_eq!(service.confirmed(), usize::from(approve));
    let titled = rig.api.log.lock().titled.clone();
    assert_eq!(titled.iter().filter(|t| t.2.contains("PRIVATE_PREVIEW")).count(), 1);
    assert!(!titled.iter().any(|t| t.0.as_deref() == Some(GATE_TITLE)));
    clean(&rig).await;
}

#[tokio::test]
async fn test_tainted_chat_purchase_has_only_the_final_cart_approval_denied() {
    tainted_chat_purchase_has_only_the_final_cart_approval(false).await;
}

#[tokio::test]
async fn test_tainted_chat_purchase_has_only_the_final_cart_approval_approved() {
    tainted_chat_purchase_has_only_the_final_cart_approval(true).await;
}
