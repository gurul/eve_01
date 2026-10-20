//! Port of `bridge/tests/test_desk_call_lean.py`: replays of the counterexamples in verification/Eve/DeskCall.lean
//! against the real code. Each test is one trace from that file's `current_violates` theorem, driven through the
//! real types with fakes (no board, no microphone, no OpenAI).
//!
//!   P2  A phone call starts during a desk call. When the desk call ends, its Call.run cleared the chat's one
//!       listener, which was the phone's: the rest of the phone call was texted, not spoken.
//!   P1  A tap on the board while the "listening" ring is still being sent: the release ran first, and the press
//!       then opened the Mac microphone with the button up. Nothing closed it until the call ended.
//!
//! The daemon's traces (P1 during a conversation, P3 the wake word mid-hold) and TelegramInlet's own guards are
//! the daemon's and telegram's tests.

use std::sync::Arc;

use eve_phone_call::{PhoneCalls, Reader, OP_TEXT};
use parking_lot::Mutex;
use serde_json::{json, Value};

use crate::test_support::*;
use crate::*;
use eve_phone_call::CallSocket;

const OWNER: i64 = 4242;

/// TelegramInlet's listener, as far as a call sees it: `listen(None, owner=say)` clears only `say`, and the chat is
/// ready without taking the listener.
#[derive(Default)]
struct Inlet {
    call_say: Mutex<Option<Reader>>,
}

impl eve_phone_call::Brain for Inlet {
    fn listen(&self, say: Option<Reader>, owner: Option<&Reader>) -> bool {
        let mut cur = self.call_say.lock();
        if say.is_some() || owner.is_none() || cur.as_ref() == owner {
            *cur = say;
        }
        true
    }

    fn hear(&self, _text: &str, _call: Option<&Reader>, _answer_context: eve_phone_call::Captured) {
        let say = self.call_say.lock().clone();
        if let Some(say) = say {
            say.say("ok");
        }
    }

    fn chat_ready(&self) -> Option<bool> {
        Some(true)
    }
}

impl Inlet {
    fn say(&self) -> Option<Reader> {
        self.call_say.lock().clone()
    }
}

/// DeskCall.lean `p2Trace`: desk press, phone call, desk call ends. The phone call is still on, so the chat must
/// still read out to it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_a_desk_call_that_ends_leaves_the_phone_calls_listener() {
    let inlet = Arc::new(Inlet::default());
    let (send, _sent) = recorder();
    let (mic_factory, _made) = mic_factory();
    let (speaker_factory, _speakers) = speaker_factory();
    let phones =
        Arc::new(PhoneCalls::new(Arc::new(|_init: &str| Some(OWNER)), Some(inlet.clone()), Some(Voice::new())));
    let p = phones.clone();
    let desk = DeskCalls::new(
        Some(inlet.clone()),
        Some(Voice::new()),
        send.clone(),
        DeskOptions {
            busy: Arc::new(move || p.active().is_some()),
            mic_factory,
            speaker_factory,
            ..DeskOptions::default()
        },
    );

    desk.press(true).await; // the desk call takes the chat's listener
    settle().await;
    assert!(desk.active() && inlet.say().is_some());

    // The owner calls from the phone. A DeskLink stands in for the phone's WebSocket: it is exactly what Call
    // needs, and the hello is the first message, as the Mini App sends it.
    let ws = DeskLink::new(send, Arc::new(FakeSpeaker::default()), None);
    ws.push(OP_TEXT, json!({"initData": "signed"}).to_string().into_bytes());
    let (ph, w) = (phones.clone(), ws.clone());
    let serving = tokio::spawn(async move { ph.serve_socket(w, None).await });
    settle().await;
    let phone = phones.active().expect("the phone call is on");
    // An open phone call does not take the chat until its first press; the model's `phoneStart` is that press.
    assert!(inlet.say().is_some() && inlet.say() != Some(phone.reader()));
    ws.push(OP_TEXT, br#"{"type": "talk"}"#.to_vec());
    settle().await;
    assert!(inlet.say() == Some(phone.reader()));

    desk.press(false).await;
    desk.stop().await; // the desk call ends (quiet, or stopped)
    settle().await;
    assert!(!desk.active() && phones.active().is_some_and(|a| Arc::ptr_eq(&a, &phone)));
    assert!(inlet.say() == Some(phone.reader()), "the desk call's end took the phone call's listener");

    ws.close(1000).await;
    let _ = serving.await;
    settle().await;
    assert!(inlet.say().is_none()); // the phone call's own end clears it
}

/// DeskCall.lean `p1TapTrace`: the press sets the ring to listening, the send waits on the serial link, the
/// release arrives, then the press resumes. The microphone must not be open with the button up.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_a_tap_during_the_ring_send_leaves_the_microphone_closed() {
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let g = gate.clone();
    let send: BoardSend = Arc::new(move |obj: Value| {
        let g = g.clone();
        Box::pin(async move {
            if obj == json!({"cmd": "agent", "state": "listening"}) {
                let _ = g.acquire().await; // the serial link is busy
            }
            Ok(())
        })
    });
    let (mic_factory, made) = mic_factory();
    let (speaker_factory, _speakers) = speaker_factory();
    let desk = DeskCalls::new(
        Some(Brain::new("")),
        Some(Voice::new()),
        send,
        DeskOptions { mic_factory, speaker_factory, ..DeskOptions::default() },
    );
    let d = desk.clone();
    let down = tokio::spawn(async move { d.press(true).await });
    settle().await;
    desk.press(false).await; // the release: its own message, its own task
    gate.add_permits(1000);
    down.await.unwrap();
    settle().await;
    assert!(!desk.held());
    assert_eq!(open_mics(&made), 0, "the microphone is open with the button up");
    desk.stop().await;
}
