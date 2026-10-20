//! test_phone_call.py: "Call eve", push to talk into the chat's brain. A loopback server that answers the upgrade
//! as the Mini App server does, a real WebSocket client as the phone's browser, a fake chat that answers through the
//! call's reader, and a fake voice (no model). Plus the pure parts: the accept key, frames, the speech helpers,
//! `make_voice`, the transcription request, and test_telegram_app_only.py's `foreign` cases.

#![allow(clippy::type_complexity)]

use std::io::{Read as _, Write as _};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;

use super::test_support::*;
use super::*;

fn calls(brain: Option<Arc<FakeBrain>>, voice: Arc<FakeVoice>) -> Arc<PhoneCalls> {
    Arc::new(PhoneCalls::new(check_owner(), brain.map(|b| b as Arc<dyn Brain>), Some(voice as Arc<dyn Voice>)))
}

#[test]
fn test_the_accept_key_is_rfc_6455s() {
    assert_eq!(accept_key("dGhlIHNhbXBsZSBub25jZQ=="), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
}

#[tokio::test(flavor = "multi_thread")]
async fn test_a_press_is_heard_handed_to_the_chat_and_the_reply_read_back() {
    let (brain, voice) = (FakeBrain::default_reply(), FakeVoice::calendar());
    let base = serve(calls(Some(brain.clone()), voice.clone())).await;
    let ws = Phone::connect(&format!("{base}/api/call")).await;
    ws.hello_owner().await;
    ws.until(is_state("connected")).await;
    ws.send_bytes(vec![0u8; 4800]).await; // before a press: never recorded
    ws.press(1.0).await;
    let got = ws.until(is_state("listening")).await;
    ws.send_json(json!({"type": "end"})).await;
    let ended = ws.until(is_kind("ended")).await;
    ws.close().await;

    assert!(got.contains(&Msg::Json(json!({"type": "heard", "text": "what's on my calendar"}))));
    assert_eq!(brain.heard(), vec!["what's on my calendar"]);
    assert_eq!(voice.transcribed(), vec![48000]); // the press alone: 1 s at 24 kHz
    assert_eq!(voice.spoken(), vec!["You have two meetings: standup and lunch."]); // Markdown and links as words
    assert_eq!(audio_bytes(&got), 4800);
    assert_eq!(states(&got)[..2], ["thinking", "speaking"]);
    // the sentence's words, for voice mode's captions, just before its sound (2026-10-09)
    let said: Vec<usize> = got.iter().enumerate().filter(|(_, m)| m.is("said")).map(|(i, _)| i).collect();
    let sound = got.iter().position(|m| matches!(m, Msg::Bin(_))).unwrap();
    assert_eq!(texts_of(&got, "said"), vec!["You have two meetings: standup and lunch."]);
    assert!(said[0] < sound);
    assert_eq!(ended.last().unwrap(), &Msg::Json(json!({"type": "ended", "reason": "you hung up"})));
    assert!(brain.say().is_none()); // the chat stops reading to a gone call
}

#[tokio::test(flavor = "multi_thread")]
async fn test_a_tap_is_not_words_and_nothing_is_asked() {
    let brain = FakeBrain::default_reply();
    let base = serve(calls(Some(brain.clone()), FakeVoice::calendar())).await;
    let ws = Phone::connect(&format!("{base}/api/call")).await;
    ws.hello_owner().await;
    ws.press(0.1).await;
    let got = ws.until(|m| m.json().and_then(|v| v.get("note")).is_some_and(eve_records::py::truthy)).await;
    ws.close().await;
    let note = got.last().unwrap().json().unwrap()["note"].as_str().unwrap().to_string();
    assert!(note.contains("Hold the button") && brain.heard().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn test_a_press_while_eve_talks_interrupts_it() {
    let brain = FakeBrain::new(&"One. ".repeat(60)); // many sentences, slowly made
    let voice = Arc::new(FakeVoice { delay: Duration::from_millis(200), ..FakeVoice::plain("what's on my calendar") });
    let base = serve(calls(Some(brain), voice)).await;
    let ws = Phone::connect(&format!("{base}/api/call")).await;
    ws.hello_owner().await;
    ws.press(1.0).await;
    ws.until(is_state("speaking")).await;
    ws.send_json(json!({"type": "talk"})).await;
    let got = ws.until(is_kind("flush")).await;
    ws.close().await;
    assert_eq!(got.last().unwrap(), &Msg::Json(json!({"type": "flush"})));
}

#[tokio::test(flavor = "multi_thread")]
async fn test_nobody_but_the_owner_can_call() {
    for hello in [r#"{"initData": ""}"#, r#"{"initData": "garbage"}"#, "not json", r#"{"x": 1}"#] {
        let brain = FakeBrain::default_reply();
        let base = serve(calls(Some(brain.clone()), FakeVoice::calendar())).await;
        let ws = Phone::connect(&format!("{base}/api/call")).await;
        ws.send_text(hello).await;
        let got = ws.until(is_kind("ended")).await;
        assert_eq!(got.last().unwrap().json().unwrap()["reason"], "Only eve's owner can call.", "{hello}");
        ws.close().await;
        // signed by the bot, but not for an owner
        let ws = Phone::connect(&format!("{base}/api/call")).await;
        ws.send_json(json!({"initData": "query_id=AAH&user=%7B%22id%22%3A999%7D&hash=signed-by-the-bot"})).await;
        let got = ws.until(is_kind("ended")).await;
        assert_eq!(got.last().unwrap().json().unwrap()["reason"], "Only eve's owner can call.");
        ws.close().await;
        assert!(brain.say().is_none()); // nothing was started
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn test_calling_again_replaces_a_call_left_open_and_calls_are_off_without_a_chat() {
    let c = calls(Some(FakeBrain::default_reply()), FakeVoice::calendar());
    let base = serve(c.clone()).await;
    let first = Phone::connect(&format!("{base}/api/call")).await;
    first.hello_owner().await;
    first.until(is_state("connected")).await;
    let second = Phone::connect(&format!("{base}/api/call")).await; // the page reopened; the old socket lingers
    second.hello_owner().await;
    let got = second.until(is_state("connected")).await;
    assert_eq!(got.last().unwrap().state(), Some("connected"));
    let active = c.active().expect("the new call");
    assert!(!active.ws.closed());
    second.close().await;
    first.close().await;

    let base = serve(calls(None, FakeVoice::calendar())).await;
    let ws = Phone::connect(&format!("{base}/api/call")).await;
    ws.hello_owner().await;
    let got = ws.until(is_kind("ended")).await;
    assert!(got.last().unwrap().json().unwrap()["reason"].as_str().unwrap().contains("aren't set up"));
    ws.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_a_phone_that_goes_quiet_ends_the_call() {
    let brain = FakeBrain::default_reply();
    let c = calls(Some(brain.clone()), FakeVoice::calendar());
    c.limits.set_quiet_secs(0.3);
    let base = serve(c.clone()).await;
    let ws = Phone::connect(&format!("{base}/api/call")).await;
    ws.hello_owner().await;
    let got = ws.until(is_kind("ended")).await;
    assert_eq!(got.last().unwrap().json().unwrap()["reason"], "the phone went quiet");
    until("the call to end", || c.active().is_none()).await;
    assert!(brain.say().is_none());
    ws.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_a_silent_socket_is_closed_after_the_auth_window() {
    let c = calls(Some(FakeBrain::default_reply()), FakeVoice::calendar());
    c.limits.set_auth_secs(0.2);
    let base = serve(c).await;
    let ws = Phone::connect(&format!("{base}/api/call")).await;
    let got = ws.until(is_kind("ended")).await;
    assert_eq!(got.last().unwrap().json().unwrap()["reason"], "Only eve's owner can call.");
    ws.close().await;
}

#[test]
fn test_speakable_reads_a_chat_message_as_words() {
    assert_eq!(speakable("**Done.** See [the doc](https://d.example/x) or https://y.example/z"), "Done. See the doc or a link");
    assert_eq!(speakable("- one\n- two"), "one two");
    let long = "A sentence here. ".repeat(200);
    let out = speakable(&long);
    assert!(out.chars().count() <= MAX_SPOKEN_CHARS + 40 && out.ends_with("The rest is in the chat."));
    let text = "Hi. Yes. ".to_string() + &"This one is long enough to stand alone as a sentence of its own. ".repeat(2);
    assert!(sentences(&text, 220)[0].starts_with("Hi. Yes."));
}

#[test]
fn speakable_and_sentences_match_python_on_the_edges() {
    // python3 -c 'from eve_bridge.phone_call import *; print(speakable(...))'
    assert_eq!(speakable("[](https://x) `code` #tag > quote | ~"), "code tag quote");
    assert_eq!(speakable("• a\n  - b\nc - d"), "a b c - d");
    assert_eq!(speakable(""), "");
    // a cut past the 200th character, at the last sentence end inside the limit
    let long = format!("{}. {}", "a".repeat(1000), "b".repeat(1000));
    assert_eq!(speakable(&long), format!("{}. The rest is in the chat.", "a".repeat(1000)));
    // no sentence end past 200: cut at the limit
    let flat = "w".repeat(1600);
    assert_eq!(speakable(&flat), format!("{} The rest is in the chat.", "w".repeat(1500)));
    assert_eq!(sentences("One!  Two?\nThree. ", 220), vec!["One! Two? Three."]);
    let a = "x".repeat(100);
    assert_eq!(sentences(&format!("{a}. {a}. ok"), 220), vec![format!("{a}."), format!("{a}."), "ok".to_string()]);
    assert_eq!(sentences(&format!("Hi. Yes. ok. {}.", "z".repeat(95)), 220), vec!["Hi. Yes. ok.".to_string(), format!("{}.", "z".repeat(95))]);
    assert_eq!(sentences(&"y".repeat(1000), 220), vec!["y".repeat(880)]);
    assert_eq!(sentences("e.g.x and 3.5", 220), vec!["e.g.x and 3.5"]);
    // Python's `\s` is str.isspace, which takes U+001C..U+001F too
    assert_eq!(sentences("One.\u{1f}Two.", 220), vec!["One. Two."]);
    assert_eq!(speakable("see http://x.y\u{1f}z"), "see a link z");
    assert_eq!(speakable("\u{1f}- one\n\u{1c}-\u{1d}two"), "one two");
}

#[tokio::test]
async fn test_frames_round_trip_and_bad_frames_close() {
    let (mut client, server) = tokio::io::duplex(1 << 20);
    let payload = vec![b'x'; 70000]; // the 64-bit length form
    let mask = [1u8, 2, 3, 4];
    let masked: Vec<u8> = payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]).collect();
    let mut wire = vec![0x82, 0x80 | 127];
    wire.extend((payload.len() as u64).to_be_bytes());
    wire.extend(mask);
    wire.extend(&masked);
    client.write_all(&wire).await.unwrap();
    let ws = WebSocket::new(server, tokio::io::sink());
    assert_eq!(ws.recv().await.unwrap(), (OP_BIN, payload));
    client.write_all(&[0x82, 0x05]).await.unwrap(); // unmasked: a client must mask
    client.write_all(b"hello").await.unwrap();
    assert!(ws.recv().await.is_err());

    let big = (MAX_FRAME_BYTES + 1) as u64;
    let (mut client2, server2) = tokio::io::duplex(1 << 10);
    let mut wire = vec![0x82, 0x80 | 127];
    wire.extend(big.to_be_bytes());
    client2.write_all(&wire).await.unwrap();
    let err = WebSocket::new(server2, tokio::io::sink()).recv().await.unwrap_err();
    assert_eq!(err.0, "frame too large");
    assert_eq!(frame(OP_TEXT, b"hi"), b"\x81\x02hi");
}

#[tokio::test]
async fn frames_join_fragments_answer_pings_and_close() {
    fn masked(b1: u8, payload: &[u8]) -> Vec<u8> {
        let mask = [9u8, 8, 7, 6];
        let mut out = vec![b1, 0x80 | payload.len() as u8];
        out.extend(mask);
        out.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
        out
    }
    let (mut client, server) = tokio::io::duplex(1 << 16);
    let (sent_w, mut sent_r) = tokio::io::duplex(1 << 16);
    let ws = WebSocket::new(server, sent_w);
    let mut wire = masked(0x01, b"he"); // a text frame, not final
    wire.extend(masked(0x89, b"p")); // a ping in between
    wire.extend(masked(0x80, b"llo")); // the final continuation
    wire.extend(masked(0x88, &[0x03, 0xe8])); // close 1000
    client.write_all(&wire).await.unwrap();
    assert_eq!(ws.recv().await.unwrap(), (OP_TEXT, b"hello".to_vec()));
    let err = ws.recv().await.unwrap_err();
    assert_eq!(err.0, "closed by the phone");
    assert!(ws.closed());
    let mut back = [0u8; 7];
    tokio::io::AsyncReadExt::read_exact(&mut sent_r, &mut back).await.unwrap();
    assert_eq!(&back, b"\x8a\x01p\x88\x02\x03\xe8"); // the pong, then the close echoed
    assert_eq!(ws.send_json(&json!({"type": "x"})).await.unwrap_err().0, "already closed");
    let mut f = frame(OP_BIN, &[0u8; 300]);
    assert_eq!(&f[..4], &[0x82, 126, 0x01, 0x2c]);
    f = frame(OP_BIN, &vec![0u8; 70000]);
    assert_eq!(&f[..10], &[0x82, 127, 0, 0, 0, 0, 0, 1, 0x11, 0x70]);
}

#[tokio::test]
async fn the_handshake_answers_with_the_accept_key_and_json_is_pythons() {
    let (_client, server) = tokio::io::duplex(64);
    let (w, mut r) = tokio::io::duplex(1 << 12);
    let headers: HashMap<String, String> = [
        ("upgrade", "websocket"),
        ("connection", "keep-alive, Upgrade"),
        ("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ=="),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    assert!(is_upgrade(&headers));
    let ws = WebSocket::accept(server, w, &headers).await.unwrap();
    ws.send_json(&json!({"type": "heard", "text": "café"})).await.unwrap();
    drop(ws);
    let mut out = Vec::new();
    tokio::io::AsyncReadExt::read_to_end(&mut r, &mut out).await.unwrap();
    let head = "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
                Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n\r\n";
    assert!(out.starts_with(head.as_bytes()));
    let body = b"{\"type\": \"heard\", \"text\": \"caf\\u00e9\"}";
    assert_eq!(&out[head.len()..], &frame(OP_TEXT, body)[..]);
    let mut no_key = headers.clone();
    no_key.remove("sec-websocket-key");
    assert!(!is_upgrade(&no_key));
}

// ---- latency: live ears, the fast voice and their fallbacks ---------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn test_a_press_is_transcribed_live_and_falls_back_to_the_upload() {
    for (fail, expect) in [(false, "live words"), (true, "uploaded words")] {
        let brain = FakeBrain::new("");
        let ears = FakeEars::new(fail);
        let voice = FakeVoice::live(Some(ears.clone() as Arc<dyn Ears>));
        let base = serve(calls(Some(brain.clone()), voice.clone())).await;
        let ws = Phone::connect(&format!("{base}/api/call")).await;
        ws.hello_owner().await;
        ws.until(is_state("connected")).await;
        tokio::time::sleep(Duration::from_millis(50)).await; // the live session opens meanwhile
        ws.press(1.0).await;
        let got = ws.until(is_kind("heard")).await;
        ws.close().await;
        assert_eq!(got.last().unwrap().json().unwrap()["text"], expect);
        assert_eq!(brain.heard(), vec![expect]);
        assert_eq!(ears.fed.load(Ordering::SeqCst), 48000);
        assert_eq!(ears.begun.load(Ordering::SeqCst), 1);
        // uploaded only when the live one failed
        assert_eq!(voice.transcribed(), if fail { vec![48000] } else { vec![] });
    }
}

/// A one-request-at-a-time HTTP server on a thread: answers each request with `status` and `body`, and records the
/// request line, the Authorization header and the JSON body.
fn speech_server(status: Arc<std::sync::atomic::AtomicU16>) -> (String, Arc<Mutex<Vec<(String, String, Value)>>>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://127.0.0.1:{}/api/v1/audio/speech", listener.local_addr().unwrap().port());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            let (head, body_start) = loop {
                let n = stream.read(&mut chunk).unwrap();
                buf.extend_from_slice(&chunk[..n]);
                if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    break (String::from_utf8_lossy(&buf[..i]).to_string(), i + 4);
                }
            };
            let length: usize = head
                .lines()
                .find_map(|l| l.to_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse().unwrap()))
                .unwrap_or(0);
            while buf.len() < body_start + length {
                let n = stream.read(&mut chunk).unwrap();
                buf.extend_from_slice(&chunk[..n]);
            }
            let body: Value = serde_json::from_slice(&buf[body_start..body_start + length]).unwrap();
            let auth = head
                .lines()
                .find_map(|l| l.strip_prefix("authorization: ").or_else(|| l.strip_prefix("Authorization: ")))
                .unwrap_or("")
                .to_string();
            log.lock().push((head.lines().next().unwrap().to_string(), auth, body));
            let code = status.load(Ordering::SeqCst);
            let payload = b"\x01\x00".repeat(10);
            let reply = format!("HTTP/1.1 {code} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", payload.len());
            stream.write_all(reply.as_bytes()).unwrap();
            stream.write_all(&payload).unwrap();
        }
    });
    (url, seen)
}

#[test]
fn test_the_voice_is_openrouters_alone_and_a_failed_sentence_raises() {
    // until 2026-10-08 OpenAI's gpt-4o-mini-tts was the fallback; with no OpenAI key there is no second voice
    let status = Arc::new(std::sync::atomic::AtomicU16::new(503));
    let (url, seen) = speech_server(status.clone());
    let spend = tempfile::tempdir().unwrap();
    let env: HashMap<String, String> = [("OPENROUTER_API_KEY".to_string(), "or-key".to_string())].into();
    let mut v = make_voice(Some(&env)).unwrap();
    assert_eq!(v.speech_url, OPENROUTER_SPEECH_URL);
    v.speech_url = url;
    v.ledger = Ledger::at(spend.path());
    let err = v.speak_stream("hello").collect::<anyhow::Result<Vec<Vec<u8>>>>().unwrap_err();
    assert!(err.to_string().contains("503"), "{err}");
    status.store(200, Ordering::SeqCst);
    let out: Vec<u8> = v.speak_stream("hello").collect::<anyhow::Result<Vec<_>>>().unwrap().concat();
    assert_eq!(out, b"\x01\x00".repeat(10));
    let seen = seen.lock().clone();
    assert_eq!(seen.len(), 2);
    for (line, auth, body) in &seen {
        assert_eq!(line, "POST /api/v1/audio/speech HTTP/1.1");
        assert_eq!(auth, "Bearer or-key");
        assert_eq!(body, &json!({"model": "deepgram/aura-2", "voice": "aura-2-thalia-en", "input": "hello",
                                 "response_format": "pcm"}));
    }
    // the sentence that was heard is metered at aura-2's price; the failed one is not
    let rows: Vec<_> = ledger_days(spend.path()).iter().flat_map(|d| Ledger::at(spend.path()).day_rows(d)).collect();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["m"], "deepgram/aura-2");
    assert_eq!(rows[0]["f"], eve_spend::CALLS);
}

/// The days a ledger folder holds lines for (one file per local day, named by it).
fn ledger_days(dir: &std::path::Path) -> Vec<String> {
    let mut days: Vec<String> = std::fs::read_dir(dir)
        .map(|rd| rd.flatten().filter_map(|e| e.path().file_stem().map(|s| s.to_string_lossy().to_string())).collect())
        .unwrap_or_default();
    days.sort();
    days.dedup();
    days
}

#[test]
fn test_make_voice_needs_only_the_openrouter_key_and_the_live_ears_are_opt_in() {
    let env = |pairs: &[(&str, &str)]| -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    };
    let v = make_voice(Some(&env(&[("OPENROUTER_API_KEY", "or-test")]))).unwrap();
    assert!(v.or_key == "or-test" && v.or_model == "deepgram/aura-2" && !v.live && v.live_ears().is_none());
    assert!(eve_openrouter::BASE_URL.starts_with("https://openrouter.ai/api/v1"));
    assert!(make_voice(Some(&env(&[("OPENAI_API_KEY", "sk-test")]))).is_none()); // an OpenAI key alone: off
    assert!(make_voice(Some(&env(&[]))).is_none());
    // EVE_CALL_LIVE_STT=1 without an OpenAI key: uploads, no live ears
    assert!(!make_voice(Some(&env(&[("OPENROUTER_API_KEY", "or-test"), ("EVE_CALL_LIVE_STT", "1")]))).unwrap().live);
    let v = make_voice(Some(&env(&[
        ("OPENROUTER_API_KEY", "or-test"),
        ("EVE_CALL_LIVE_STT", "1"),
        ("OPENAI_API_KEY", "sk-test"),
    ])))
    .unwrap();
    assert!(v.live && v.live_ears().is_some() && v.live_key == "sk-test");
    // the tuning knobs, stripped; a blank one keeps the default
    let v = make_voice(Some(&env(&[
        ("OPENROUTER_API_KEY", " or-test "),
        ("EVE_CALL_STT_MODEL", " whisper "),
        ("EVE_CALL_OR_TTS_MODEL", ""),
        ("EVE_CALL_OR_VOICE", "aura-2-luna-en"),
        ("EVE_CALL_LIVE_STT", " On "),
    ])))
    .unwrap();
    assert_eq!((v.or_key.as_str(), v.stt_model.as_str()), ("or-test", "whisper"));
    assert_eq!((v.or_model.as_str(), v.or_voice.as_str()), ("deepgram/aura-2", "aura-2-luna-en"));
    assert!(!v.live);
}

struct FakeTranscriptions {
    asked: Mutex<Vec<(Map<String, Value>, String, String, usize)>>,
}

impl NotesSdk for FakeTranscriptions {
    fn transcriptions_create(&self, fields: Map<String, Value>, file: Part) -> anyhow::Result<Value> {
        let Part::File { filename, content_type, bytes, .. } = file else { anyhow::bail!("no file") };
        self.asked.lock().push((fields, filename, content_type, bytes.len()));
        Ok(json!({"text": " hi ", "usage": null}))
    }
    fn responses_create(&self, _body: Value) -> anyhow::Result<Value> {
        anyhow::bail!("the call never asks for a response")
    }
}

#[tokio::test]
async fn test_calls_are_transcribed_in_english_unless_told_otherwise() {
    // a short Voice PE press, left to guess, was heard as Chinese and answered in Chinese (2026-09-27)
    let env = |pairs: &[(&str, &str)]| -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    };
    assert_eq!(make_voice(Some(&env(&[("OPENROUTER_API_KEY", "or-test")]))).unwrap().language, "en");
    let es = make_voice(Some(&env(&[("OPENROUTER_API_KEY", "or-test"), ("EVE_CALL_LANGUAGE", "ES")]))).unwrap();
    assert_eq!(es.language, "es");
    let guess = make_voice(Some(&env(&[("OPENROUTER_API_KEY", "or-test"), ("EVE_CALL_LANGUAGE", "")]))).unwrap();
    assert_eq!(guess.language, "");

    let spend = tempfile::tempdir().unwrap();
    let fake = Arc::new(FakeTranscriptions { asked: Mutex::new(vec![]) });
    let mut v = OpenAIVoice::new("or-test", "m");
    v.client = fake.clone();
    v.ledger = Ledger::at(spend.path());
    v.language = "en".into();
    let v = Arc::new(v);
    let pcm = b"\0\0".repeat(2400);
    let vv = v.clone();
    let p = pcm.clone();
    assert_eq!(tokio::task::spawn_blocking(move || vv.transcribe(&p)).await.unwrap().unwrap(), "hi");
    {
        let asked = fake.asked.lock();
        let (fields, filename, ctype, n) = asked.last().unwrap();
        assert_eq!(Value::Object(fields.clone()), json!({"model": "m", "language": "en"}));
        assert_eq!((filename.as_str(), ctype.as_str(), *n), ("call.wav", "audio/wav", 44 + pcm.len()));
    }
    let mut guessing = OpenAIVoice::new("or-test", "m");
    guessing.client = fake.clone();
    guessing.ledger = Ledger::at(spend.path());
    guessing.language = String::new();
    tokio::task::spawn_blocking(move || guessing.transcribe(&pcm)).await.unwrap().unwrap();
    assert!(!fake.asked.lock().last().unwrap().0.contains_key("language"));

    let server = super::tests_turns::RealtimeServer::new(&[]);
    let ears = LiveEars::with_connector("k", "m", "en", server.clone());
    ears.open().await.unwrap();
    ears.close().await;
    assert_eq!(server.sessions()[0]["audio"]["input"]["transcription"], json!({"model": "m", "language": "en"}));
    let ears = LiveEars::with_connector("k", "m", "", server.clone());
    ears.open().await.unwrap();
    ears.close().await;
    assert_eq!(server.sessions()[1]["audio"]["input"]["transcription"], json!({"model": "m"}));
}

#[tokio::test]
async fn test_the_calls_ears_keep_the_full_press_context_with_near_field_noise_reduction() {
    // A physical press supplies the boundary; pauses within it stay in the same transcription turn.
    assert_eq!(DEFAULT_STT_MODEL, "gpt-4o-transcribe");
    let server = super::tests_turns::RealtimeServer::new(&[]);
    let ears = LiveEars::with_connector("k", DEFAULT_STT_MODEL, DEFAULT_LANGUAGE, server.clone());
    ears.open().await.unwrap();
    let sent = server.sessions();
    let audio_in = &sent[0]["audio"]["input"];
    assert_eq!(audio_in["transcription"]["model"], "gpt-4o-transcribe");
    assert_eq!(audio_in["noise_reduction"], json!({"type": "near_field"}));
    assert_eq!(audio_in["turn_detection"], Value::Null);
    assert_eq!(audio_in["format"], json!({"type": "audio/pcm", "rate": 24000}));
    ears.close().await;
    // and the meter can price it
    let usd = eve_pricing::estimate_transcribe_cost("gpt-4o-transcribe", Some(&json!({})), 60.0).unwrap();
    assert!((usd - 0.006).abs() < 1e-9);
}

// ---- a call holds the chat from its first press, and an idle one gives it to the desk (2026-09-30) ----------------

#[tokio::test(flavor = "multi_thread")]
async fn test_an_open_unused_call_holds_nothing_and_an_idle_one_gives_the_chat_to_the_desk() {
    // An open call that was never pressed. On 2026-09-30 one kept the Voice PE's button waiting.
    let now = Arc::new(Mutex::new(0.0f64));
    let brain = FakeBrain::ready("You have **two** meetings: [standup](https://x.example) and lunch.");
    let t = now.clone();
    let c = Arc::new(PhoneCalls::with_clock(
        check_owner(),
        Some(brain.clone() as Arc<dyn Brain>),
        Some(FakeVoice::calendar() as Arc<dyn Voice>),
        Arc::new(move || *t.lock()),
    ));
    let base = serve(c.clone()).await;
    let ws = Phone::connect(&format!("{base}/api/call")).await;
    ws.hello_owner().await;
    ws.until(is_state("connected")).await;
    // open, never pressed: no listener, and the desk is free
    assert!(brain.say().is_none() && !c.holding_chat());
    // the first press takes the chat; the reply is read out
    ws.press(1.0).await;
    ws.until(is_state("listening")).await;
    let call = c.active().unwrap();
    assert!(call.listening() && brain.say() == Some(call.reader()));
    // just answered: still the phone's
    *now.lock() += IDLE_YIELD_SECS - 1.0;
    assert!(c.holding_chat() && brain.say() == Some(call.reader()));
    // quiet long enough: the desk's button takes it; the call stays open
    *now.lock() += 2.0;
    assert!(!c.holding_chat() && brain.say().is_none() && Arc::ptr_eq(&c.active().unwrap(), &call));
    // the phone's next press takes it back
    ws.press(1.0).await;
    ws.until(is_state("listening")).await;
    assert_eq!(brain.say(), Some(call.reader()));
    ws.close().await;
}

#[tokio::test]
async fn test_a_call_that_is_speaking_or_answering_never_gives_the_chat_up() {
    let now = Arc::new(Mutex::new(0.0f64));
    let brain = FakeBrain::ready("");
    let t = now.clone();
    let call = Call::with_clock(MemorySocket::fixed(false), brain.clone(), FakeVoice::calendar(), Arc::new(move || *t.lock()));
    call.take_chat();
    *now.lock() = 100.0;
    call.set_talking(true);
    assert!(!call.release_if_idle() && brain.say() == Some(call.reader()));
    call.set_talking(false);
    call.set_speaking(true);
    assert!(!call.release_if_idle() && brain.say() == Some(call.reader()));
    call.set_speaking(false);
    call.queue_line("still to say");
    assert!(!call.release_if_idle());
    call.take_line();
    assert!(call.release_if_idle() && brain.say().is_none()); // control: idle, it lets go
}

// ---- Eve Link's door (test_stick_link.py's call tests) -------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn test_the_app_calls_with_its_token_and_a_press_is_heard() {
    let brain = FakeBrain::default_reply();
    let base = serve(calls(Some(brain.clone()), FakeVoice::calendar())).await;
    let ws = Phone::connect(&format!("{base}/api/stick")).await; // no Origin, as the app
    ws.send_json(json!({"token": LINK})).await;
    ws.until(is_state("connected")).await;
    ws.press(1.0).await;
    let got = ws.until(is_state("listening")).await;
    ws.close().await;
    assert!(got.contains(&Msg::Json(json!({"type": "heard", "text": "what's on my calendar"}))));
    assert_eq!(brain.heard(), vec!["what's on my calendar"]);
    assert!(got.iter().any(|m| matches!(m, Msg::Bin(_)))); // the reply's audio came back
}

#[tokio::test(flavor = "multi_thread")]
async fn test_a_wrong_or_missing_token_ends_the_call_before_it_starts() {
    for hello in [r#"{"token": "wrong"}"#, r#"{"token": ""}"#, "{}", r#"{"initData": "x"}"#, "not json"] {
        let brain = FakeBrain::default_reply();
        let base = serve(calls(Some(brain.clone()), FakeVoice::calendar())).await;
        let ws = Phone::connect(&format!("{base}/api/stick")).await;
        ws.send_text(hello).await;
        let got = ws.until(is_kind("ended")).await;
        assert_eq!(got.last().unwrap().json().unwrap()["reason"], "Only eve's owner can call.");
        ws.close().await;
        assert!(brain.say().is_none() && brain.heard().is_empty());
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn test_telegrams_initdata_does_not_open_the_apps_door_nor_the_token_the_mini_apps() {
    let base = serve(calls(Some(FakeBrain::default_reply()), FakeVoice::calendar())).await;
    let ws = Phone::connect(&format!("{base}/api/stick")).await;
    ws.hello_owner().await;
    assert_eq!(ws.until(is_kind("ended")).await.last().unwrap().json().unwrap()["reason"], "Only eve's owner can call.");
    ws.close().await;
    let ws = Phone::connect(&format!("{base}/api/call")).await;
    ws.send_json(json!({"token": LINK})).await;
    assert_eq!(ws.until(is_kind("ended")).await.last().unwrap().json().unwrap()["reason"], "Only eve's owner can call.");
    ws.close().await;
    let ws = Phone::connect(&format!("{base}/api/call")).await; // control: the Mini App still gets in
    ws.hello_owner().await;
    ws.until(is_state("connected")).await;
    ws.close().await;
}

// ---- English only (test_telegram_app_only.py) ----------------------------------------------------------------------

#[test]
fn test_english_calls_drop_words_in_another_script() {
    for text in ["あ。", "ご視聴ありがとうございました", "Продолжение следует", "谢谢"] {
        assert!(foreign(text, "en"), "{text}");
    }
}

#[test]
fn test_english_and_punctuation_are_kept() {
    for text in ["Who is the president?", "Play Beyoncé", "café at 5", "ok", "...", "42", ""] {
        assert!(!foreign(text, "en"), "{text}");
    }
}

#[test]
fn test_no_language_or_another_language_keeps_everything() {
    assert!(!foreign("あ。", ""));
    assert!(!foreign("あ。", "ja"));
}

#[test]
fn the_switch_reads_truthy_strings_as_python_does() {
    for (raw, want) in [(Some("1"), true), (Some(" TRUE "), true), (Some("yes"), true), (Some("on"), true),
                        (Some("0"), false), (Some(""), false), (None, false), (Some("enabled"), false)] {
        assert_eq!(on(raw), want, "{raw:?}");
    }
}
