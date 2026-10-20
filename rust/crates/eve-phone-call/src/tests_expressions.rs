//! test_phone_expressions.py, the call's half: eve's expressions in the phone's eyes. The loopback server and the
//! Eve Link door, a real WebSocket client as the phone, the fake chat and voice. The real picker
//! (live_expressions.LiveExpressions) is another crate's; where Python ran it, a fake picker here hands out ids and
//! sends its picks through `PhoneCalls::send_expression` as the daemon's wiring does.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde_json::{json, Map, Value};

use super::test_support::*;
use super::*;

type Offered = Arc<Mutex<Vec<(String, String, Instant)>>>;

fn recorder(offered: &Offered) -> OnText {
    let offered = offered.clone();
    Arc::new(move |who: &str, text: &str| {
        offered.lock().push((who.to_string(), text.to_string(), Instant::now()));
        Ok(None)
    })
}

fn pairs(offered: &Offered) -> Vec<(String, String)> {
    offered.lock().iter().map(|(w, t, _)| (w.clone(), t.clone())).collect()
}

async fn setup(brain: Arc<FakeBrain>, voice: Arc<FakeVoice>) -> (Arc<PhoneCalls>, String) {
    let calls = Arc::new(PhoneCalls::new(check_owner(), Some(brain), Some(voice)));
    let base = serve(calls.clone()).await;
    (calls, format!("{base}/api/stick"))
}

async fn hello(ws: &Phone, can: &[&str]) {
    ws.send_json(json!({"token": LINK, "can": can})).await;
    ws.until(is_state("connected")).await;
}

async fn eventually(what: &str, pred: impl Fn() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !pred() {
        assert!(tokio::time::Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn saids(got: &[Msg]) -> Vec<String> {
    texts_of(got, "said")
}

fn expressions(got: &[Msg]) -> Vec<Value> {
    got.iter().filter_map(|m| m.json()).filter(|v| v["type"] == "expression").cloned().collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn test_both_sides_of_a_turn_are_offered_in_order() {
    let (calls, url) = setup(FakeBrain::new("Lovely. That went well."), FakeVoice::calendar()).await;
    let offered: Offered = Arc::default();
    calls.set_on_text(Some(recorder(&offered)));
    let ws = Phone::connect(&url).await;
    hello(&ws, &["notes", "eyes"]).await;
    ws.press(1.0).await;
    let got = ws.until(is_state("listening")).await;
    let said = saids(&got);
    assert_eq!(pairs(&offered)[0], ("user".to_string(), "what's on my calendar".to_string()));
    // every sentence read out, as it was captioned, in order (each when the phone starts to play it)
    eventually("every sentence offered", || offered.lock().len() == 1 + said.len()).await;
    let rest: Vec<(String, String)> = said.iter().map(|s| ("assistant".to_string(), s.clone())).collect();
    assert!(!said.is_empty() && pairs(&offered)[1..] == rest[..]);
    ws.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_a_hello_without_eyes_gets_no_offers_and_no_expressions() {
    let (calls, url) = setup(FakeBrain::new("Lovely. That went well."), FakeVoice::calendar()).await;
    let offered: Offered = Arc::default();
    calls.set_on_text(Some(recorder(&offered)));
    let ws = Phone::connect(&url).await;
    hello(&ws, &["notes"]).await; // an older Eve Link build
    ws.press(1.0).await;
    let got = ws.until(is_state("listening")).await;
    assert!(offered.lock().is_empty() && !calls.eyes_live());
    assert!(!calls.send_expression(&json!({"cmd": "expression", "id": 5, "label": "happy", "ttl_ms": 4000})).await);
    assert!(expressions(&got).is_empty());
    ws.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_a_picker_fault_never_breaks_the_call() {
    let (calls, url) = setup(FakeBrain::new("Lovely. That went well."), FakeVoice::calendar()).await;
    calls.set_on_text(Some(Arc::new(|_: &str, _: &str| anyhow::bail!("picker down"))));
    let ws = Phone::connect(&url).await;
    hello(&ws, &["notes", "eyes"]).await;
    ws.press(1.0).await;
    let got = ws.until(is_state("listening")).await;
    assert!(got.iter().any(|m| matches!(m, Msg::Bin(_)))); // eve still spoke
    ws.close().await;
}

/// The daemon's wiring with a fake model: every offer gets the next id, and its pick ("happy") is sent at once.
fn picker(calls: &Arc<PhoneCalls>, states: Arc<Mutex<Vec<String>>>) {
    let next = Arc::new(AtomicI64::new(0));
    let weak: Weak<PhoneCalls> = Arc::downgrade(calls);
    calls.set_on_text(Some(Arc::new(move |who: &str, text: &str| {
        let id = next.fetch_add(1, Ordering::SeqCst) + 1;
        states.lock().push(format!("{}: {text}", if who == "user" { "User" } else { "Eve" }));
        let weak = weak.clone();
        tokio::spawn(async move {
            if let Some(calls) = weak.upgrade() {
                calls.send_expression(&json!({"cmd": "expression", "id": id, "label": "happy", "ttl_ms": 3000})).await;
            }
        });
        Ok(Some(id))
    })));
}

/// One press; then its reply (back to listening) and at least one pick, in whichever order they come.
async fn turn_with_pick(ws: &Phone) -> Vec<Value> {
    ws.press(1.0).await;
    let mut seen = vec![];
    let mut listening = false;
    while !listening || seen.is_empty() {
        let got = ws.until(|m| m.is("expression") || m.state() == Some("listening")).await;
        let m = got.last().unwrap().json().unwrap().clone();
        if m["type"] == "expression" {
            seen.push(m);
        } else {
            listening = true;
        }
    }
    seen
}

#[tokio::test(flavor = "multi_thread")]
async fn test_picks_reach_the_phone_in_the_contract_shape_with_increasing_ids() {
    let (calls, url) = setup(FakeBrain::new("Lovely. That went well."), FakeVoice::calendar()).await;
    let states = Arc::new(Mutex::new(vec![]));
    picker(&calls, states.clone());
    let ws = Phone::connect(&url).await;
    hello(&ws, &["notes", "eyes"]).await;
    let mut seen = turn_with_pick(&ws).await;
    seen.extend(turn_with_pick(&ws).await);
    for m in &seen {
        // an assistant pick names the sentence it was made for, so the phone starts it with that sound
        let keys: Vec<&str> = m.as_object().unwrap().keys().map(String::as_str).collect();
        assert!(keys == ["type", "id", "label", "ttl_ms"] || keys == ["type", "id", "label", "ttl_ms", "said"], "{m}");
        assert!(m["label"] == "happy" && m["id"].as_i64().unwrap() > 0);
        assert!((500..=4000).contains(&m["ttl_ms"].as_i64().unwrap()));
    }
    let ids: Vec<i64> = seen.iter().map(|m| m["id"].as_i64().unwrap()).collect();
    let mut sorted = ids.clone();
    sorted.sort();
    sorted.dedup();
    assert!(ids.len() >= 2 && ids == sorted);
    assert!(states.lock().iter().any(|s| s.contains("User: what's on my calendar")));
    assert!(states.lock().iter().any(|s| s.contains("Eve: ")));
    assert!(seen.iter().any(|m| m.get("said").is_some()));
    ws.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_sentences_are_offered_when_the_phone_plays_them_not_when_they_are_made() {
    // Speech is made faster than it plays: the second sentence's offer waits for the first's sound.
    let brain = FakeBrain::new(
        "Bad news, the build failed on the release branch this morning because a test timed out. \
         But I fixed the slow test, reran everything twice, and it is all green now!",
    );
    let voice = Arc::new(FakeVoice { speech: Speech::HalfSecond, ..FakeVoice::plain("what's on my calendar") });
    let (calls, url) = setup(brain, voice).await;
    let offered: Offered = Arc::default();
    calls.set_on_text(Some(recorder(&offered)));
    let ws = Phone::connect(&url).await;
    hello(&ws, &["notes", "eyes"]).await;
    ws.press(1.0).await;
    let got = ws.until(is_state("listening")).await;
    let said = saids(&got);
    assert_eq!(said.len(), 2);
    eventually("three offers", || offered.lock().len() == 3).await;
    let o = offered.lock().clone();
    assert_eq!((o[1].1.clone(), o[2].1.clone()), (said[0].clone(), said[1].clone()));
    assert!(o[2].2.duration_since(o[1].2) >= Duration::from_millis(400)); // the first sentence's half second
    ws.close().await;
}

fn map(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        _ => unreachable!(),
    }
}

#[tokio::test]
async fn test_an_assistant_pick_names_its_sentence_and_others_do_not() {
    let ws = MemorySocket::fixed(false);
    let call = Call::new(ws.clone(), FakeBrain::default_reply(), FakeVoice::calendar());
    let ids = Arc::new(Mutex::new(vec![7i64, 8].into_iter()));
    call.set_on_text(Some(Arc::new(move |_: &str, _: &str| Ok(ids.lock().next()))));
    call.text("assistant", "But it's green now!");
    call.text("user", "did it build");
    assert!(call.send_expression(map(json!({"type": "expression", "id": 7, "label": "happy", "ttl_ms": 3000}))).await);
    assert!(call.send_expression(map(json!({"type": "expression", "id": 8, "label": "curious", "ttl_ms": 3000}))).await);
    assert!(call.send_expression(map(json!({"type": "expression", "id": 7, "clear": true}))).await);
    assert_eq!(
        ws.sent_json(),
        vec![
            json!({"type": "expression", "id": 7, "label": "happy", "ttl_ms": 3000, "said": "But it's green now!"}),
            json!({"type": "expression", "id": 8, "label": "curious", "ttl_ms": 3000}),
            json!({"type": "expression", "id": 7, "clear": true}),
        ]
    );
}

#[tokio::test]
async fn test_a_barge_in_drops_the_offers_still_waiting_for_their_sound() {
    let ws = MemorySocket::fixed(false);
    let call = Call::new(ws.clone(), FakeBrain::default_reply(), FakeVoice::calendar());
    let offered = Arc::new(Mutex::new(Vec::<String>::new()));
    let o = offered.clone();
    call.set_on_text(Some(Arc::new(move |_: &str, text: &str| {
        o.lock().push(text.to_string());
        Ok(None)
    })));
    call.sent_audio(BYTES_PER_SEC / 5); // 0.2 s of eve's sound queued on the phone
    call.offer_when_heard("two");
    assert!(offered.lock().is_empty() && call.pending_offers() == 1);
    call.interrupt().await; // the owner pressed: the phone flushed it
    tokio::time::sleep(Duration::from_millis(350)).await;
    assert!(offered.lock().is_empty() && call.pending_offers() == 0 && call.play_end() == 0.0);
    call.offer_when_heard("three"); // nothing queued: offered at once
    assert_eq!(*offered.lock(), vec!["three"]);
    call.stop_tasks().await;
}

#[tokio::test]
async fn test_no_live_call_sends_nothing() {
    let calls = PhoneCalls::new(Arc::new(|_: &str| None), None, None);
    assert!(calls.eyes_call().is_none() && !calls.eyes_live());
    assert!(!calls.send_expression(&json!({"cmd": "expression", "id": 1, "label": "happy", "ttl_ms": 4000})).await);
}

#[tokio::test]
async fn test_no_target_while_talking_closed_or_unable() {
    for (can_eyes, talking, closed) in [(true, true, false), (true, false, true), (false, false, false)] {
        let calls = PhoneCalls::new(Arc::new(|_: &str| None), None, None);
        let ws = MemorySocket::fixed(closed);
        let call = Call::new(ws.clone(), FakeBrain::default_reply(), FakeVoice::calendar());
        call.set_can_eyes(can_eyes);
        call.set_talking(talking);
        calls.set_active(Some(call));
        assert!(calls.eyes_call().is_none() && !calls.eyes_live());
        assert!(!calls.send_expression(&json!({"cmd": "expression", "id": 9, "label": "sad", "ttl_ms": 4000})).await);
        assert!(ws.sent().is_empty());
    }
    // positive control: able, not talking, open
    let calls = PhoneCalls::new(Arc::new(|_: &str| None), None, None);
    let call = Call::new(MemorySocket::fixed(false), FakeBrain::default_reply(), FakeVoice::calendar());
    call.set_can_eyes(true);
    calls.set_active(Some(call));
    assert!(calls.eyes_live());
}

#[tokio::test]
async fn test_a_malformed_command_is_not_sent() {
    let calls = PhoneCalls::new(Arc::new(|_: &str| None), None, None);
    let ws = MemorySocket::fixed(false);
    let call = Call::new(ws.clone(), FakeBrain::default_reply(), FakeVoice::calendar());
    call.set_can_eyes(true);
    calls.set_active(Some(call));
    assert!(!calls.send_expression(&json!({"cmd": "expression", "label": "sad", "ttl_ms": 4000})).await);
    assert!(!calls.send_expression(&json!({"cmd": "expression", "id": 3, "ttl_ms": 4000})).await);
    assert!(!calls.send_expression(&json!({"cmd": "expression", "id": "3", "label": "sad", "ttl_ms": 4000})).await);
    assert!(
        calls.send_expression(&json!({"cmd": "expression", "id": 3, "label": "sad", "ttl_ms": 4000, "chirp": false})).await
    );
    assert_eq!(ws.sent_json(), vec![json!({"type": "expression", "id": 3, "label": "sad", "ttl_ms": 4000})]);
}
