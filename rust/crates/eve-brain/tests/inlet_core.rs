//! Unit `inlet_core`: telegram.py 2448-3714 (inlet/mod, run, handle); tests: test_telegram.py G6-G8 (542-780), the robot and stealth (1057-1209), where the owner's message goes (4215-4381), review findings and the relay join (6341-6675); test_telegram_agentcard.py.
//!
//! The shared fakes are in `support` (architect-owned); this unit's own go here. Python's tests that drive private
//! methods (`_ask_user`, `_relay_to`, `_launch_step`, `_tool`, `_turn_parts`) are driven here through the poll loop
//! (`run`) and the public API where the same behaviour can be reached that way; PARITY.md lists the rest.

mod support;

use std::future::Future;
use std::sync::Arc;

use async_trait::async_trait;
use futures::FutureExt;
use parking_lot::Mutex;
use serde_json::{json, Value};

use eve_agent_contract::AgentEvent;
use eve_brain::prelude::{BrainMemory, Flag, Lent, RoomNotes, Sessions, Sleep};
use eve_brain::telegram::{
    recall_line, own_tools, BotApiError, InletOptions, TelegramApi, TelegramConfig, TelegramInlet, BRIEF_RULES,
    FAILED_LINE, INSTRUCTIONS, NOTHING_TO_STOP_LINE, ON_IT_LINE, STAR_REACTION, STEALTH_OFF_LINE, STEALTH_ON_LINE,
    STOPPED_LINE, TASK_FILE_HINT,
};
use support::*;

const STRANGER: i64 = 666;

// ---- this unit's own helpers ---------------------------------------------------------------------------------------

/// `run_rig(rig, during)`: the poll loop runs, `during` runs while it does, and the loop must still be alive
/// before it is cancelled (its shutdown then runs, as Python's `finally` does).
async fn run_rig(rig: &Rig, during: impl Future<Output = ()>) {
    let inlet = rig.inlet.clone();
    let looping = tokio::spawn(async move { inlet.run().await });
    settle(200).await;
    during.await;
    settle(200).await;
    assert!(!looping.is_finished(), "the poll loop died");
    looping.abort();
    let _ = looping.await;
    settle(200).await;
}

/// A message `update` with Python's defaults: from the owner, in their private chat, message id = update id.
fn msg(text: &str, update_id: i64) -> Obj {
    update(update_id, text, OWNER, NOW, update_id)
}

/// `update(text, chat_id=..., chat_type=..., **extra)`: the same message with its chat or extra fields changed.
fn msg_with(text: Option<&str>, update_id: i64, uid: i64, chat: (i64, &str), extra: Value) -> Obj {
    let mut message = json!({"message_id": update_id, "date": NOW as i64,
                             "from": {"id": uid, "is_bot": false, "first_name": "Someone"},
                             "chat": {"id": chat.0, "type": chat.1}});
    if let Some(text) = text {
        message["text"] = json!(text);
    }
    if let (Some(m), Some(extra)) = (message.as_object_mut(), extra.as_object()) {
        for (k, v) in extra {
            m.insert(k.clone(), v.clone());
        }
    }
    json!({"update_id": update_id, "message": message}).as_object().unwrap().clone()
}

/// The JSON output of the last tool call in request `i`.
fn last_output(rig: &Rig, i: usize) -> Value {
    let requests = rig.create.requests.lock();
    let input = requests[i]["input"].as_array().expect("input");
    serde_json::from_str(input.last().unwrap()["output"].as_str().expect("an output")).expect("json")
}

fn sent(rig: &Rig) -> Vec<(i64, String)> {
    rig.api.log.lock().sent.clone()
}

fn owner_says(texts: &[&str]) -> Vec<(i64, String)> {
    texts.iter().map(|t| (OWNER, t.to_string())).collect()
}

fn feed(rig: &Rig, update: Obj) {
    rig.api.feed(vec![update]);
}

// ---- G6: a text turn -----------------------------------------------------------------------------------------------

#[tokio::test]
async fn test_a_text_turn_is_answered_with_the_brief_and_history() {
    let api = FakeApi::new(vec![vec![msg("what's your favourite colour?", 1)], vec![msg("why?", 2)]]);
    let rig = Rig::new(api, FakeCreate::new(vec![reply("Orange, like my LEDs."), reply("It is warm.")]), config(), |o| {
        o.brief = Some(Arc::new(|| "Yesterday you talked about the robot's new eyes.".to_string()));
    });
    run_rig(&rig, async {}).await;
    assert_eq!(sent(&rig), owner_says(&["Orange, like my LEDs.", "It is warm."]));
    let requests = rig.create.requests.lock().clone();
    let (first, second) = (&requests[0], &requests[1]);
    // the brief is in the turn's developer note, worded for a chat, not in the (cached) instructions
    assert_eq!(first["instructions"], json!(format!("{INSTRUCTIONS}{}", recall_line(false, false))));
    assert!(!first["instructions"].as_str().unwrap().contains("new eyes"));
    let note = first["input"][0]["content"][0]["text"].as_str().unwrap();
    assert!(note.contains("new eyes") && note.contains(BRIEF_RULES.trim()) && !note.contains("greeting"));
    assert_eq!(first["store"], json!(false));
    assert_eq!(first["include"], json!(["reasoning.encrypted_content"]));
    assert!(first.get("previous_response_id").is_none());
    assert_eq!(first["model"], json!("gpt-6-luna"));
    assert_eq!(first["reasoning"], json!({"effort": "low"}));
    let mut tools: Vec<Value> = own_tools().into_iter().map(Value::Object).collect();
    tools.push(json!({"type": "web_search"})); // no OpenRouter key in the config: the hosted search
    assert_eq!(first["tools"], Value::Array(tools));
    let roles = |r: &Value| -> Vec<String> {
        r["input"].as_array().unwrap().iter().map(|i| i["role"].as_str().unwrap_or("").to_string()).collect()
    };
    assert_eq!(roles(first), vec!["developer", "user"]);
    // the second turn sees the first: the owner's words as input_text, eve's as output_text; the note sits right
    // before the newest user message, never in the history
    let shown: Vec<(String, String, String)> = second["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["role"] != json!("developer"))
        .map(|i| {
            let c = &i["content"][0];
            (i["role"].as_str().unwrap().into(), c["type"].as_str().unwrap().into(), c["text"].as_str().unwrap().into())
        })
        .collect();
    assert_eq!(
        shown,
        vec![
            ("user".into(), "input_text".into(), "what's your favourite colour?".into()),
            ("assistant".into(), "output_text".into(), "Orange, like my LEDs.".into()),
            ("user".into(), "input_text".into(), "why?".into()),
        ]
    );
    assert_eq!(roles(second), vec!["user", "assistant", "developer", "user"]);
    assert!(rig.inlet.turns().iter().all(|(_, text)| !text.contains("new eyes")));
}

#[tokio::test]
async fn test_a_model_failure_is_answered_not_swallowed() {
    let create = FakeCreate::new(vec![]);
    create.push(Scripted::Fail("503".into()));
    let rig = Rig::new(FakeApi::new(vec![vec![msg("hello?", 1)]]), create, config(), |_| {});
    run_rig(&rig, async {}).await;
    assert_eq!(sent(&rig), owner_says(&[FAILED_LINE]));
    let rogue = json!({"id": "r", "output": [{"type": "function_call", "name": "rm_rf", "call_id": "c", "arguments": "{}"}]});
    let rig = Rig::new(FakeApi::new(vec![vec![msg("hello?", 1)]]), FakeCreate::new(vec![rogue]), config(), |_| {});
    run_rig(&rig, async {}).await; // a tool nobody defined is a failure, not a call
    assert_eq!(sent(&rig), owner_says(&[FAILED_LINE]));
    assert!(rig.agents.made.lock().is_empty());
}

// ---- G7: tasks -----------------------------------------------------------------------------------------------------

#[tokio::test]
async fn test_a_texted_task_runs_the_agent_and_texts_the_result() {
    let rig = Rig::new(
        FakeApi::new(vec![vec![msg("open the calculator", 1)]]),
        FakeCreate::new(vec![call("start_task", json!({"goal": "open the calculator"}), "call_1")]),
        config(),
        |_| {},
    );
    run_rig(&rig, async {
        let agent = rig.agents.last();
        assert_eq!(agent.goal.lock().as_deref(), Some("open the calculator"));
        assert!(rig.inlet.task_running());
        assert_eq!(*agent.note.lock(), TASK_FILE_HINT); // beside the goal: the router never reads it
        assert_eq!(sent(&rig), owner_says(&[ON_IT_LINE])); // started, not finished, and no second model call
        assert_eq!(rig.create.requests.lock().len(), 1);
        agent.release.set();
        settle(200).await;
        assert!(rig.inlet.turns().contains(&("eve".to_string(), "Calculator is open.".to_string())));
    })
    .await;
    assert_eq!(sent(&rig), owner_says(&[ON_IT_LINE, "Calculator is open."]));
    let states = rig.states.lock().clone();
    assert_eq!(&states[..2], &["working".to_string(), "done".to_string()]);
    assert_eq!(states.last().map(String::as_str), Some("idle"));
}

#[tokio::test]
async fn test_a_second_task_is_refused_while_one_runs() {
    let api = FakeApi::new(vec![vec![msg("open the calculator", 1)], vec![msg("and open mail", 2)]]);
    let rig = Rig::new(
        api,
        FakeCreate::new(vec![
            call("start_task", json!({"goal": "open the calculator"}), "call_1"),
            call("start_task", json!({"goal": "open mail"}), "call_2"),
            reply("One thing at a time: the calculator is still going."),
        ]),
        config(),
        |_| {},
    );
    run_rig(&rig, async {
        assert_eq!(rig.agents.made.lock().len(), 1);
        let refusal = last_output(&rig, 2);
        assert_eq!(refusal["ok"], json!(false));
        assert!(refusal["reason"].as_str().unwrap().contains("already running"));
        // the round after a refusal carries the model's own call back, reasoning included: stateless
        let kinds: Vec<String> = rig.create.requests.lock()[2]["input"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["type"].as_str().unwrap_or("").to_string())
            .collect();
        assert_eq!(&kinds[kinds.len() - 2..], &["function_call".to_string(), "function_call_output".to_string()]);
        rig.agents.last().release.set();
    })
    .await;
    assert!(sent(&rig).contains(&(OWNER, "One thing at a time: the calculator is still going.".to_string())));
}

#[tokio::test]
async fn test_the_desk_and_the_chat_never_share_the_mouse() {
    let rig = Rig::new(
        FakeApi::new(vec![vec![msg("open mail", 1)]]),
        FakeCreate::new(vec![call("start_task", json!({"goal": "open mail"}), "call_1"), reply("Later!")]),
        config(),
        |o| o.busy = Some(Arc::new(|| true)),
    );
    run_rig(&rig, async {}).await;
    assert!(rig.agents.made.lock().is_empty());
    assert!(last_output(&rig, 1)["reason"].as_str().unwrap().contains("at the desk"));
}

#[tokio::test]
async fn test_stop_cancels_the_running_task() {
    // "stop" costs no model call
    let rig = Rig::new(
        FakeApi::new(vec![vec![msg("open the calculator", 1)]]),
        FakeCreate::new(vec![call("start_task", json!({"goal": "open the calculator"}), "call_1")]),
        config(),
        |_| {},
    );
    run_rig(&rig, async {
        assert!(rig.inlet.task_running());
        feed(&rig, msg("Stop!", 2));
        settle(200).await;
        assert!(!rig.inlet.task_running());
        // said once, by code: the agent's own "Okay, I stopped." is kept for memory but not sent as well
        assert!(rig.inlet.turns().contains(&("eve".to_string(), "Okay, I stopped.".to_string())));
        feed(&rig, msg("stop", 3));
    })
    .await;
    assert_eq!(rig.agents.last().cancel_reason.lock().as_deref(), Some("stopped from Telegram"));
    assert_eq!(sent(&rig), owner_says(&[ON_IT_LINE, STOPPED_LINE, NOTHING_TO_STOP_LINE]));
    assert_eq!(rig.create.requests.lock().len(), 1);
    assert_eq!(rig.states.lock().last().map(String::as_str), Some("idle"));
}

#[tokio::test]
async fn test_no_task_starts_when_computer_control_is_off() {
    let rig = Rig::new(
        FakeApi::new(vec![vec![msg("open mail", 1)]]),
        FakeCreate::new(vec![call("start_task", json!({"goal": "open mail"}), "call_1"), reply("I can't.")]),
        config(),
        |o| o.agent_enabled = false,
    );
    run_rig(&rig, async {}).await;
    assert!(rig.agents.made.lock().is_empty());
    assert_eq!(sent(&rig), owner_says(&["I can't."]));
    assert!(last_output(&rig, 1)["reason"].as_str().unwrap().contains("disabled"));
}

// ---- G8: only the human approves -------------------------------------------------------------------------------------

#[tokio::test]
async fn test_a_tasks_question_is_answered_by_the_owners_next_message() {
    let rig = Rig::new(
        FakeApi::new(vec![vec![msg("email the report to Sam", 1)]]),
        FakeCreate::new(vec![call("start_task", json!({"goal": "email the report to Sam"}), "call_1")]),
        config(),
        |_| {},
    );
    *rig.agents.ask.lock() = Some("Send the email to Sam now?".into());
    *rig.agents.final_text.lock() = Some("Sent.".into());
    run_rig(&rig, async {
        assert_eq!(sent(&rig).last().cloned(), Some((OWNER, "Send the email to Sam now?".to_string())));
        feed(&rig, msg("yes send it", 2));
        settle(200).await;
        rig.agents.last().release.set();
    })
    .await;
    assert_eq!(
        sent(&rig),
        owner_says(&[ON_IT_LINE, "Send the email to Sam now?", "Sent. (answer=yes send it)"])
    );
    assert_eq!(rig.create.requests.lock().len(), 1); // the answer was not also run as a new turn
    assert!(rig.states.lock().iter().any(|s| s == "asking"));
}

#[tokio::test]
async fn test_a_stranger_cannot_answer_a_tasks_question() {
    let rig = Rig::new(
        FakeApi::new(vec![vec![msg("email the report to Sam", 1)]]),
        FakeCreate::new(vec![call("start_task", json!({"goal": "email the report to Sam"}), "call_1")]),
        config(),
        |_| {},
    );
    *rig.agents.ask.lock() = Some("Send the email to Sam now?".into());
    run_rig(&rig, async {
        rig.api.feed(vec![
            msg_with(Some("yes"), 2, STRANGER, (STRANGER, "private"), json!({})),
            msg_with(Some("yes"), 3, OWNER, (-100123, "group"), json!({})),
            msg_with(Some("yes"), 4, OWNER, (OWNER, "private"), json!({"forward_origin": {"type": "user"}})),
        ]);
        settle(200).await;
        assert!(rig.inlet.waiting_on_owner()); // still waiting
        feed(&rig, msg("no", 5)); // the owner, in their own words: the control
        settle(200).await;
        rig.agents.last().release.set();
    })
    .await;
    assert_eq!(sent(&rig).last().cloned(), Some((OWNER, "Calculator is open. (answer=no)".to_string())));
    assert_eq!(rig.create.requests.lock().len(), 1);
}

#[tokio::test]
async fn test_no_answer_in_time_reads_as_no() {
    let cfg = TelegramConfig { ask_timeout_secs: 0.01, ..config() };
    let rig = Rig::new(
        FakeApi::new(vec![vec![msg("email the report to Sam", 1)]]),
        FakeCreate::new(vec![call("start_task", json!({"goal": "email the report to Sam"}), "call_1")]),
        cfg,
        |_| {},
    );
    *rig.agents.ask.lock() = Some("Send the email to Sam now?".into());
    run_rig(&rig, async {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        settle(200).await;
        rig.agents.last().release.set();
    })
    .await;
    let last = sent(&rig).last().cloned().unwrap().1;
    assert!(last.starts_with("Calculator is open. (answer=no (no answer within"), "{last}");
    assert!(!rig.inlet.waiting_on_owner());
}

// ---- the robot over text, and stealth ---------------------------------------------------------------------------------

/// The room note-taker, recording what it was asked.
#[derive(Default)]
struct FakeNotes {
    calls: Mutex<Vec<String>>,
}

#[async_trait]
impl RoomNotes for FakeNotes {
    fn start(&self) -> Value {
        self.calls.lock().push("start".into());
        json!({"ok": true, "started": true})
    }
    async fn stop(&self, reason: &str) -> Value {
        self.calls.lock().push(format!("stop:{reason}"));
        json!({"ok": true, "minutes": 3})
    }
    fn status(&self) -> Obj {
        json!({"active": false}).as_object().unwrap().clone()
    }
}

/// memory.Memory around a REAL transcript store in a tmp folder; the profile, tools and star are stubs.
struct FakeMemory {
    transcripts: Arc<eve_transcripts::Transcripts>,
    stars: Mutex<Vec<String>>,
}

impl FakeMemory {
    fn new(root: &std::path::Path) -> Arc<Self> {
        let config = eve_transcripts::TranscriptConfig {
            enabled: true,
            root: root.join("transcripts"),
            home: Some(root.to_path_buf()),
        };
        let transcripts = Arc::new(eve_transcripts::Transcripts::new(config, None, None));
        assert!(transcripts.enabled, "the tmp folder must pass the transcript guards");
        Arc::new(Self { transcripts, stars: Mutex::new(Vec::new()) })
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
    fn star(&self, text: &str) -> Option<String> {
        self.stars.lock().push(text.to_string());
        Some(format!("- {text} (2026-09-23)"))
    }
    fn tools(&self, _voice: bool) -> Vec<Value> {
        ["memory_search", "memory_read", "forget_preview", "forget_apply"]
            .iter()
            .map(|name| {
                json!({"type": "function", "name": name, "strict": true, "description": "Memory.",
                       "parameters": {"type": "object", "additionalProperties": false, "required": ["query"],
                                      "properties": {"query": {"type": "string"}}}})
            })
            .collect()
    }
    fn handle_tool(&self, name: &str, args: &Value, _since: Option<eve_transcripts::Dt>, _channel: &str) -> Value {
        if name == "memory_search" {
            return self.transcripts.search(args["query"].as_str().unwrap_or(""));
        }
        json!({"ok": true})
    }
}

/// The robot's head, with every command it was sent recorded.
fn recording_head() -> (Arc<eve_head::Head>, Arc<Mutex<Vec<Value>>>) {
    let sends = Arc::new(Mutex::new(Vec::new()));
    let s = sends.clone();
    let send: eve_head::Sender = Arc::new(move |cmd: Value| {
        s.lock().push(cmd);
        async { true }.boxed()
    });
    let sleep: eve_head::Sleep = Arc::new(|_secs: f64| async { tokio::task::yield_now().await }.boxed());
    (Arc::new(eve_head::Head::with(send, Arc::new(|| true), Arc::new(|| 0.0), sleep)), sends)
}

/// scene.SceneWatcher's look, answering a desk with a mug.
struct FakeScene;

#[async_trait]
impl eve_head::Scene for FakeScene {
    async fn look(&self, _fresh_secs: f64, _newer_than: Option<f64>) -> Value {
        json!({"ok": true, "view": "a desk with a mug"})
    }
    async fn locate(&self, _target: &str, _newer_than: Option<f64>) -> Value {
        json!({"ok": false})
    }
}

#[async_trait]
impl eve_brain::prelude::SceneWatch for FakeScene {}

#[tokio::test]
async fn test_a_text_is_a_word_said_to_the_robot() {
    let dir = tmp();
    let (head, moves) = recording_head();
    let notes = Arc::new(FakeNotes::default());
    let sounds = Arc::new(Mutex::new(Vec::new()));
    let memory = FakeMemory::new(dir.path());
    let api = FakeApi::new(vec![
        vec![msg("look left", 1)],
        vec![msg("start taking notes", 2)],
        vec![msg("mute yourself", 3)],
        vec![msg("remember that I like ramen", 4)],
        vec![msg("what do you see?", 5)],
    ]);
    let create = FakeCreate::new(vec![
        call("move_head", json!({"yaw": -60, "pitch": null, "relative": false, "hold_secs": null}), "call_1"),
        reply("Looking left."),
        call("take_notes", json!({"action": "start"}), "c2"),
        reply("Taking notes."),
        call("set_sound", json!({"on": false}), "c3"),
        reply("Muted."),
        call("remember", json!({"claim": "I like ramen"}), "c4"), // 🏆 is the answer
        call("look", json!({}), "c5"),
        reply("A desk with a mug."),
    ]);
    let (n, s, m) = (notes.clone(), sounds.clone(), memory.clone());
    let rig = Rig::new(api, create, config(), move |o| {
        o.head = Some(head);
        o.scene = Some(Arc::new(FakeScene));
        o.notes = Some(Arc::new(move || n.clone() as Arc<dyn RoomNotes>));
        o.on_sound = Some(Arc::new(move |on: bool| s.lock().push(on)));
        o.memory = Some(m as Arc<dyn BrainMemory>);
    });
    run_rig(&rig, async {}).await;
    assert_eq!(moves.lock().len(), 1);
    assert_eq!(*notes.calls.lock(), vec!["start".to_string()]);
    assert_eq!(*sounds.lock(), vec![false]);
    assert_eq!(*memory.stars.lock(), vec!["I like ramen".to_string()]); // "remember that" stars via memory.star
    let texts: Vec<String> = sent(&rig).into_iter().map(|(_, t)| t).collect();
    assert_eq!(texts, vec!["Looking left.", "Taking notes.", "Muted.", "A desk with a mug."]);
    assert_eq!(rig.api.log.lock().reactions, vec![(OWNER, 4, STAR_REACTION.to_string())]); // starred: a receipt
    assert_eq!(last_output(&rig, 8)["view"], json!("a desk with a mug"));
}

#[tokio::test]
async fn test_take_notes_started_on_a_call_says_its_reply_and_lets_the_chat_go_after_the_turn() {
    async fn run(defer: bool) -> (Vec<String>, Vec<String>) {
        let api = FakeApi::new(vec![]);
        let create = FakeCreate::new(vec![
            reply("ok"),
            call("take_notes", json!({"action": "start"}), "call_1"),
            reply("Taking notes."),
        ]);
        let rig = Rig::new(api, create, config(), |_| {});
        let read = Arc::new(Mutex::new(Vec::<String>::new()));
        let r = read.clone();
        let reader = eve_phone_call::Reader::new(move |text: &str| r.lock().push(text.to_string()));
        let inlet = Arc::downgrade(&rig.inlet);
        let owner = reader.clone();
        let phone: eve_brain::prelude::PhoneNotes =
            Arc::new(move |_action: String, call_say: Option<eve_phone_call::Reader>, later: eve_brain::prelude::Later| {
                assert!(call_say.is_some()); // said on the call
                let (inlet, owner) = (inlet.clone(), owner.clone());
                let release = move || {
                    if let Some(inlet) = inlet.upgrade() {
                        inlet.listen(None, Some(&owner)); // Call.release_for_notes
                    }
                };
                if defer {
                    later(Box::new(release));
                } else {
                    release(); // the old way: inside the turn
                }
                async { Some(json!({"ok": true, "started": true, "on": "your phone"}).as_object().unwrap().clone()) }
                    .boxed()
            });
        *rig.inlet.phone_notes.lock() = Some(phone);
        assert!(rig.inlet.listen(Some(reader), None));
        settle(200).await; // the cache warm-up
        rig.inlet.hear("start taking notes", Default::default(), true, None, None, "auto", &[]);
        settle(400).await;
        let texts = sent(&rig).into_iter().map(|(_, t)| t).collect();
        let read = read.lock().clone();
        (read, texts)
    }
    let (read, texts) = run(true).await;
    assert!(read.iter().any(|t| t == "Taking notes.") && !texts.iter().any(|t| t == "Taking notes."));
    // positive control: let go inside the turn, the same reply is texted and never said
    let (read, texts) = run(false).await;
    assert!(texts.iter().any(|t| t == "Taking notes.") && !read.iter().any(|t| t == "Taking notes."));
}

#[tokio::test]
async fn test_the_robot_shows_what_the_chat_does_unless_stealth() {
    let captions = Arc::new(Mutex::new(Vec::<Value>::new()));
    let (head, moves) = recording_head();
    let c = captions.clone();
    let rig = Rig::new(
        FakeApi::new(vec![vec![msg("open the calculator", 1)]]),
        FakeCreate::new(vec![
            call("start_task", json!({"goal": "open the calculator"}), "call_1"),
            call("move_head", json!({"yaw": -60, "pitch": null, "relative": false, "hold_secs": null}), "c2"),
            reply("Can't, I'm asleep."),
        ]),
        config(),
        move |o| {
            o.on_caption = Some(Arc::new(move |message: Value| c.lock().push(message)));
            o.head = Some(head);
        },
    );
    run_rig(&rig, async {
        let agent = rig.agents.last();
        (agent.on_event)(&AgentEvent::with_turn("progress", "opened Calculator", 1));
        settle(200).await;
        assert_eq!(*rig.states.lock(), vec!["working".to_string()]);
        {
            let captions = captions.lock();
            let last = captions.last().expect("a caption");
            assert_eq!(last["lines"], json!(["opened Calculator"]));
            assert_eq!(last["chirp"], json!(false));
        }
        agent.release.set();
        settle(200).await;
        assert_eq!(captions.lock().last().unwrap()["lines"], json!(["Calculator is", "open."]));
        assert!(rig.states.lock().iter().any(|s| s == "done"));
        // stealth: a code word, no model call; the face goes idle and stays there
        feed(&rig, msg("stealth mode", 2));
        settle(200).await;
        assert!(rig.inlet.stealth());
        assert_eq!(rig.states.lock().last().map(String::as_str), Some("idle"));
        assert_eq!(sent(&rig).last().cloned(), Some((OWNER, STEALTH_ON_LINE.to_string())));
        let before = (captions.lock().len(), rig.states.lock().len());
        feed(&rig, msg("look left", 3));
        settle(200).await;
        assert!(moves.lock().is_empty()); // asleep robots do not move
        let n = rig.create.requests.lock().len();
        let refusal = last_output(&rig, n - 1);
        assert_eq!(refusal["ok"], json!(false));
        assert!(refusal["reason"].as_str().unwrap().contains("stealth"));
        assert_eq!((captions.lock().len(), rig.states.lock().len()), before); // and show nothing
        feed(&rig, msg("wake up", 4));
        settle(200).await;
        assert!(!rig.inlet.stealth());
        assert_eq!(sent(&rig).last().cloned(), Some((OWNER, STEALTH_OFF_LINE.to_string())));
    })
    .await;
    assert_eq!(rig.create.requests.lock().len(), 3); // task, then the refused look-left turn
}

// ---- review fixes: the owner's message goes where it was meant (2026-09-23) --------------------------------------------

/// `relay_rig`: permissions asked, the terminal typed into, one session running; "claude on" joins it.
fn relay_rig(typed: Arc<Mutex<Vec<String>>>, create: Arc<FakeCreate>) -> Rig {
    let cfg = TelegramConfig { ask_permissions: true, ..config() };
    Rig::new(FakeApi::new(vec![vec![msg("claude on", 1)]]), create, cfg, move |o| {
        let t = typed.clone();
        o.terminal = Some(Arc::new(move |_cwd: String, text: String| {
            t.lock().push(text);
            async { Ok(String::new()) }.boxed()
        }));
        o.claude_sessions = Some(Arc::new(|| Ok(Sessions::Now(vec!["/Users/g/repo".to_string()]))));
    })
}

#[tokio::test]
async fn test_eve_yes_while_allow_deny_waits_is_for_eve_and_allows_nothing() {
    let typed = Arc::new(Mutex::new(Vec::new()));
    let rig = relay_rig(typed.clone(), FakeCreate::new(vec![reply("Hi there.")]));
    run_rig(&rig, async {
        assert!(rig.inlet.claude());
        let inlet = rig.inlet.clone();
        let ask = tokio::spawn(async move { inlet.decide_permission("Bash", "rm -rf build/", "", false).await });
        settle(200).await;
        feed(&rig, msg("eve: yes", 5));
        settle(200).await;
        assert!(!ask.is_finished() && rig.create.requests.lock().len() == 1 && typed.lock().is_empty());
        feed(&rig, msg("yes", 6)); // the control: a plain yes still answers
        settle(200).await;
        assert_eq!(ask.await.unwrap().as_deref(), Some("allow"));
    })
    .await;
}

#[tokio::test]
async fn test_an_image_is_refused_while_a_question_with_buttons_waits_and_no_relay_is_on() {
    let rig = Rig::new(
        FakeApi::new(vec![vec![msg("delete the draft", 1)]]),
        FakeCreate::new(vec![call("start_task", json!({"goal": "delete the draft"}), "call_1")]),
        config(),
        |_| {},
    );
    *rig.agents.ask.lock() = Some("Delete the draft? yes / no?".into());
    run_rig(&rig, async {
        assert!(rig.inlet.waiting_on_owner());
        let photo = msg_with(None, 9, OWNER, (OWNER, "private"), json!({"photo": [{"file_id": "f", "file_size": 10}]}));
        feed(&rig, photo);
        settle(200).await;
        assert!(sent(&rig).last().unwrap().1.contains("answer the pending question"));
        assert_eq!(rig.create.requests.lock().len(), 1);
        rig.agents.last().release.set();
    })
    .await;
}

#[tokio::test]
async fn test_claude_on_awaits_a_session_list_off_the_loop_and_holds_messages_until_it_decides() {
    let typed = Arc::new(Mutex::new(Vec::<String>::new()));
    let gate = Arc::new(Flag::new());
    let (t, g) = (typed.clone(), gate.clone());
    let rig = Rig::new(
        FakeApi::new(vec![vec![msg("claude on", 1), msg("run the tests", 2)]]),
        FakeCreate::new(vec![]),
        config(),
        move |o| {
            o.terminal = Some(Arc::new(move |_cwd: String, text: String| {
                t.lock().push(text);
                async { Ok(String::new()) }.boxed()
            }));
            o.claude_sessions = Some(Arc::new(move || {
                let g = g.clone();
                Ok(Sessions::Later(
                    async move {
                        g.wait().await;
                        Ok(vec!["/Users/g/repo".to_string()])
                    }
                    .boxed(),
                ))
            }));
        },
    );
    run_rig(&rig, async {
        assert!(!rig.inlet.claude() && typed.lock().is_empty());
        gate.set();
        settle(200).await;
        assert!(rig.inlet.claude());
        assert_eq!(*typed.lock(), vec!["run the tests".to_string()]);
        assert!(rig.create.requests.lock().is_empty());
    })
    .await;
}

/// FakeApi, except that the / menu of chat 7 is refused ("chat not found").
struct OneRefused(Arc<FakeApi>);

#[async_trait]
impl TelegramApi for OneRefused {
    async fn get_me(&self) -> Result<Obj, BotApiError> {
        self.0.get_me().await
    }
    async fn get_updates(&self, offset: Option<i64>, timeout: i64) -> Result<Vec<Obj>, BotApiError> {
        self.0.get_updates(offset, timeout).await
    }
    async fn receive_image(
        &self,
        item: &eve_telegram_images::Attachment,
    ) -> Result<eve_telegram_images::ReceivedImage, eve_brain::telegram::ApiImageError> {
        self.0.receive_image(item).await
    }
    async fn send_message(&self, chat_id: i64, text: &str, title: Option<&str>, subtitle: Option<&str>, buttons: &[String], reply_to: i64, force_reply: Option<&str>) -> Result<(), BotApiError> {
        self.0.send_message(chat_id, text, title, subtitle, buttons, reply_to, force_reply).await
    }
    async fn send_inline(&self, chat_id: i64, text: &str, keyboard: &[Vec<(String, String, String)>], title: Option<&str>, subtitle: Option<&str>) -> Result<i64, BotApiError> {
        self.0.send_inline(chat_id, text, keyboard, title, subtitle).await
    }
    async fn edit_message(&self, chat_id: i64, message_id: i64, text: &str, title: Option<&str>, subtitle: Option<&str>, keyboard: Option<&[Vec<(String, String, String)>]>) -> Result<(), BotApiError> {
        self.0.edit_message(chat_id, message_id, text, title, subtitle, keyboard).await
    }
    async fn drop_buttons(&self, chat_id: i64, message_id: i64) -> Result<(), BotApiError> {
        self.0.drop_buttons(chat_id, message_id).await
    }
    async fn answer_callback(&self, query_id: &str, text: &str) -> Result<(), BotApiError> {
        self.0.answer_callback(query_id, text).await
    }
    async fn react(&self, chat_id: i64, message_id: i64, emoji: &str) -> Result<(), BotApiError> {
        self.0.react(chat_id, message_id, emoji).await
    }
    async fn send_photo(&self, chat_id: i64, path: &std::path::Path, caption: &str) -> Result<(), BotApiError> {
        self.0.send_photo(chat_id, path, caption).await
    }
    async fn send_document(&self, chat_id: i64, path: &std::path::Path, caption: &str) -> Result<(), BotApiError> {
        self.0.send_document(chat_id, path, caption).await
    }
    async fn typing(&self, chat_id: i64) -> Result<(), BotApiError> {
        self.0.typing(chat_id).await
    }
    async fn send_draft(&self, chat_id: i64, draft_id: i64, text: &str) -> Result<(), BotApiError> {
        self.0.send_draft(chat_id, draft_id, text).await
    }
    async fn send_web_apps(&self, chat_id: i64, text: &str, buttons: &[(String, String)]) -> Result<(), BotApiError> {
        self.0.send_web_apps(chat_id, text, buttons).await
    }
    async fn send_url_buttons(&self, chat_id: i64, text: &str, buttons: &[(String, String)]) -> Result<(), BotApiError> {
        self.0.send_url_buttons(chat_id, text, buttons).await
    }
    async fn send_photo_web_apps(&self, chat_id: i64, jpeg: &[u8], caption: &str, buttons: &[(String, String)]) -> Result<(), BotApiError> {
        self.0.send_photo_web_apps(chat_id, jpeg, caption, buttons).await
    }
    async fn set_commands(&self, commands: &[(String, String)], chat_id: i64) -> Result<(), BotApiError> {
        if chat_id == 7 {
            return Err(api_error(400, "Bad Request: chat not found"));
        }
        self.0.set_commands(commands, chat_id).await
    }
}

/// Rig's options, for an inlet built around another API.
fn rig_options() -> InletOptions {
    let sleep: Sleep = no_sleep();
    InletOptions {
        brief: Some(Arc::new(String::new)),
        clock: Some(Arc::new(|| 0.0)),
        wall: Some(Arc::new(|| NOW)),
        sleep: Some(sleep),
        screen: Some(Arc::new(|| None)),
        toolset_route: Lent::Given(None),
        grader: Lent::Given(None),
        small_talk_ask: Lent::Given(None),
        environ: Some(Default::default()),
        ..InletOptions::default()
    }
}

#[tokio::test]
async fn test_one_owner_whose_menu_is_refused_does_not_cost_the_others_theirs() {
    let fake = FakeApi::new(vec![]);
    let cfg = TelegramConfig { owner_ids: vec![7, OWNER], ..config() };
    let inlet = TelegramInlet::new(cfg, Arc::new(OneRefused(fake.clone())), FakeCreate::new(vec![]).create(), rig_options());
    let looping = {
        let inlet = inlet.clone();
        tokio::spawn(async move { inlet.run().await })
    };
    settle(200).await;
    looping.abort();
    let _ = looping.await;
    let chats: Vec<i64> = fake.log.lock().commands.iter().map(|(_, chat)| *chat).collect();
    assert_eq!(chats, vec![OWNER]);
}

// ---- the / menu, as this inlet runs ---------------------------------------------------------------------------------

#[tokio::test]
async fn test_the_menu_has_jobs_america_and_opentable_only_while_they_are_on() {
    let rig = Rig::new(FakeApi::new(vec![]), FakeCreate::new(vec![]), config(), |_| {});
    let names: Vec<String> = rig.inlet.menu_commands().into_iter().map(|(c, _)| c).collect();
    let base: Vec<String> = eve_brain::telegram::BOT_COMMANDS.iter().map(|(c, _)| c.to_string()).collect();
    assert_eq!(names, base);
    assert_eq!(names.last().map(String::as_str), Some("stop"));
    let rig = Rig::new(FakeApi::new(vec![]), FakeCreate::new(vec![]), config(), |o| o.opentable_enabled = true);
    let names: Vec<String> = rig.inlet.menu_commands().into_iter().map(|(c, _)| c).collect();
    assert_eq!(&names[names.len() - 2..], &["opentable".to_string(), "stop".to_string()]);
    // OpenTable needs a computer task to book with: off without one
    let rig = Rig::new(FakeApi::new(vec![]), FakeCreate::new(vec![]), config(), |o| {
        o.opentable_enabled = true;
        o.agent_factory = None;
    });
    assert!(!rig.inlet.menu_commands().iter().any(|(c, _)| c == "opentable"));
}

#[tokio::test]
async fn test_a_forwarded_message_and_a_sticker_are_answered_by_code() {
    let rig = Rig::new(
        FakeApi::new(vec![vec![
            msg_with(Some("do it"), 1, OWNER, (OWNER, "private"), json!({"forward_origin": {"type": "user"}})),
            msg_with(None, 2, OWNER, (OWNER, "private"), json!({"sticker": {"file_id": "s"}})),
        ]]),
        FakeCreate::new(vec![]),
        config(),
        |_| {},
    );
    run_rig(&rig, async {}).await;
    assert_eq!(
        sent(&rig),
        owner_says(&[eve_brain::telegram::FORWARDED_LINE, eve_brain::telegram::NOT_TEXT_LINE])
    );
    assert!(rig.create.requests.lock().is_empty());
}

#[tokio::test]
async fn test_a_poll_failure_backs_off_and_a_fatal_one_stops_the_loop() {
    let api = FakeApi::new(vec![]);
    api.feed_error(api_error(502, "Bad Gateway"));
    api.feed(vec![msg("/start", 1)]);
    api.feed_error(api_error(401, "Unauthorized"));
    let rig = Rig::new(api, FakeCreate::new(vec![]), config(), |_| {});
    let inlet = rig.inlet.clone();
    let looping = tokio::spawn(async move { inlet.run().await });
    settle(400).await;
    assert!(looping.is_finished(), "a 401 ends the loop");
    let log = rig.api.log.lock();
    assert_eq!(log.offsets, vec![None, None, Some(2)]); // the update is acknowledged on the next poll
    assert_eq!(log.sent, vec![(OWNER, eve_brain::telegram::HELLO_LINE.to_string())]);
}

#[tokio::test]
async fn test_stealth_is_a_code_word_and_never_a_model_call() {
    let rig = Rig::new(
        FakeApi::new(vec![vec![msg("stealth mode", 1)], vec![msg("wake up", 2)]]),
        FakeCreate::new(vec![]),
        config(),
        |_| {},
    );
    run_rig(&rig, async {}).await;
    assert!(!rig.inlet.stealth());
    assert_eq!(sent(&rig), owner_says(&[STEALTH_ON_LINE, STEALTH_OFF_LINE]));
    assert_eq!(rig.states.lock().clone(), vec!["idle".to_string()]);
    assert!(rig.create.requests.lock().is_empty());
}

#[tokio::test]
async fn test_muse_is_refused_when_it_is_not_set_up_here() {
    let rig = Rig::new(FakeApi::new(vec![]), FakeCreate::new(vec![]), config(), |_| {});
    if eve_muse_pi::available() {
        return; // this Mac has Muse: the refusal cannot be seen here
    }
    assert!(!rig.inlet.set_muse(true, Some(OWNER), true));
    settle(50).await;
    assert!(!rig.inlet.muse());
    assert_eq!(sent(&rig), owner_says(&[eve_brain::telegram::MUSE_UNAVAILABLE_LINE]));
    assert!(!rig.inlet.set_muse(false, Some(OWNER), true)); // already off: nothing more is said
    settle(50).await;
    assert_eq!(sent(&rig).len(), 1);
}

