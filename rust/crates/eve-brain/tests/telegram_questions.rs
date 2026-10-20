//! test_telegram.py's review fixes for questions, typing, bubbles, progress messages and receipts (4235-4815) that
//! the unit files did not carry, ported with their names and assertions.
//!
//! The private state and methods the Python reaches (`_typing`, `_paused_typing`, `_thinking`, `_launch`, `_jobs`,
//! `_open_progress`, `_progress_step`, `_task_progress`, `_image`, `_say(..., reply_to=)`) are the inlet's
//! doc-hidden `tt_*` hooks and its `say_relay_probe`.

mod more;
mod support;

use std::path::PathBuf;
use std::sync::Arc;

use futures::FutureExt;
use parking_lot::Mutex;
use serde_json::{json, Value};

use eve_brain::prelude::{BrainMemory, Clock, Flag, Sessions};
use eve_brain::telegram::{self as telegram, Inbound, TelegramApi, TelegramConfig, ON_IT_LINE};
use more::*;
use support::*;

const STRANGER: i64 = 666;
const REPO: &str = "/Users/g/repo";

async fn jobs(rig: &Rig) {
    settle(200).await;
    rig.inlet.say_relay_probe().jobs().await;
}

async fn dispatch(rig: &Rig, text: &str, update_id: i64) {
    rig.inlet.say_relay_probe().dispatch(&Value::Object(update(update_id, text, OWNER, NOW, update_id)));
    rig.inlet.say_relay_probe().jobs().await;
}

async fn tap_obj(rig: &Rig, tap: Obj) {
    rig.inlet.say_relay_probe().dispatch(&Value::Object(tap));
    jobs(rig).await;
}

async fn tap(rig: &Rig, key: &str, message_id: i64) {
    tap_obj(rig, tap_update(90, message_id, key, OWNER, "q1")).await;
}

async fn shutdown(rig: &Rig) {
    rig.inlet.say_relay_probe().shutdown().await;
}

fn sent(api: &FakeApi) -> Vec<(i64, String)> {
    api.log.lock().sent.clone()
}

fn reasons(rig: &Rig) -> Vec<String> {
    rig.inlet.tt_typing(OWNER).unwrap_or_default()
}

fn spawn_ask(rig: &Rig, question: &'static str) -> tokio::task::JoinHandle<String> {
    let inlet = rig.inlet.clone();
    tokio::spawn(async move { inlet.tt_ask_user(question, OWNER).await })
}

fn terminal_into(typed: &Arc<Mutex<Vec<String>>>) -> eve_brain::prelude::Terminal {
    let t = typed.clone();
    Arc::new(move |_cwd: String, text: String| {
        t.lock().push(text);
        async { Ok(String::new()) }.boxed()
    })
}

fn sessions(folders: &[&str]) -> eve_brain::prelude::ClaudeSessions {
    let folders: Vec<String> = folders.iter().map(|f| f.to_string()).collect();
    Arc::new(move || Ok(Sessions::Now(folders.clone())))
}

/// `relay_rig(api, typed, **kw)`: the relay joined to one session in the owner's chat, asking permissions.
fn relay_rig(api: Arc<dyn TelegramApi>, typed: &Arc<Mutex<Vec<String>>>, memory: Option<Arc<FakeMemory>>) -> Rig {
    let terminal = terminal_into(typed);
    let rig = rig_over_config(api, FakeCreate::new(vec![]), TelegramConfig { ask_permissions: true, ..config() }, move |o| {
        o.terminal = Some(terminal);
        o.claude_sessions = Some(sessions(&[REPO]));
        o.memory = memory.map(|m| m as Arc<dyn BrainMemory>);
    });
    rig.inlet.tt_relay_to(REPO);
    rig.inlet.tt_set_chat_id(Some(OWNER));
    rig
}

/// `_relay_rig(api)`: the relay joined to one session; a draft's 20 s refresh wait is held open.
fn draft_relay_rig(api: Arc<dyn TelegramApi>) -> Rig {
    rig_over_config(api, FakeCreate::new(vec![]), config(), |o| {
        o.terminal = Some(Arc::new(|_cwd: String, _text: String| async { Ok(String::new()) }.boxed()));
        o.claude_sessions = Some(sessions(&[REPO]));
        o.sleep = Some(Arc::new(|secs: f64| {
            async move {
                if secs == telegram::DRAFT_REFRESH_SECS {
                    futures::future::pending::<()>().await; // until the loop is cancelled
                }
                tokio::task::yield_now().await
            }
            .boxed()
        }));
    })
}

/// `_typing_relay_rig(api, typed, **kw)`: the relay joined with drafts off, so a relayed line shows "typing…"; the
/// 4 s tick wait is held open, so the reasons stay in view while the test looks at them.
fn typing_relay_rig(api: Arc<FakeApi>, typed: &Arc<Mutex<Vec<String>>>, folders: &[&str]) -> Rig {
    let terminal = terminal_into(typed);
    let sessions = sessions(folders);
    let rig = Rig::new(api, FakeCreate::new(vec![]), TelegramConfig { drafts: false, ..config() }, move |o| {
        o.terminal = Some(terminal);
        o.claude_sessions = Some(sessions);
        o.sleep = Some(Arc::new(|secs: f64| {
            async move {
                if secs == telegram::TYPING_EVERY_SECS {
                    futures::future::pending::<()>().await;
                }
                tokio::task::yield_now().await
            }
            .boxed()
        }));
    });
    rig.inlet.tt_relay_to(REPO);
    rig.inlet.tt_set_chat_id(Some(OWNER));
    rig
}

// ---- questions ------------------------------------------------------------------------------------------------

#[tokio::test]
async fn test_a_picker_tap_is_never_taken_as_the_answer_to_a_waiting_task_question() {
    // Review finding: a new-claude tree button tapped while a task's free question waited was that question's
    // answer, and the tree got nothing. A tap means its picker's choice only.
    let _iso = isolated();
    let dir = tmp();
    for area in ["personal", "work"] {
        std::fs::create_dir_all(dir.path().join(area).join("eve")).unwrap();
    }
    let api = FakeApi::new(vec![]);
    let root = dir.path().to_path_buf();
    let rig = Rig::new(api.clone(), FakeCreate::new(vec![]), config(), move |o| {
        o.launcher = Some(Arc::new(|_folder: PathBuf, _harness: String| async { Ok("Opened.".to_string()) }.boxed()));
        o.launch_root = Some(root);
        o.launch_recent = Some(Arc::new(Vec::new));
    });
    dispatch(&rig, "new claude", 1).await;
    let question = spawn_ask(&rig, "Which account?");
    jobs(&rig).await;
    let (message_id, key) = api.key("Cancel", 0);
    tap(&rig, &key, message_id).await;
    assert!(!rig.inlet.say_relay_probe().launch_open() && !question.is_finished()); // the tree took it; the question waits
    dispatch(&rig, "work", 7).await; // the control: typed words answer it
    assert_eq!(question.await.unwrap(), "work");
    shutdown(&rig).await;
}

#[tokio::test]
async fn test_a_typed_ok_answers_a_codex_command_prompt_as_its_allow_button_does() {
    // Review finding: Codex accepts only "yes"; a typed "ok" passed the yes/no check and then declined.
    let _iso = isolated();
    let rig = Rig::new(FakeApi::new(vec![]), FakeCreate::new(vec![]), config(), |_| {});
    let question = "Codex asks to run this command in /projects/eve:\nmake test\n\nReply yes or no.";
    let ask = spawn_ask(&rig, question);
    jobs(&rig).await;
    dispatch(&rig, "ok", 5).await;
    assert_eq!(ask.await.unwrap(), "yes");
    let ask = spawn_ask(&rig, question);
    jobs(&rig).await;
    dispatch(&rig, "nope", 6).await;
    assert_eq!(ask.await.unwrap(), "no");
    let ask = spawn_ask(&rig, "Which account?");
    jobs(&rig).await;
    dispatch(&rig, "ok", 7).await; // a free question keeps the words as typed
    assert_eq!(ask.await.unwrap(), "ok");
    shutdown(&rig).await;
}

#[tokio::test]
async fn test_a_task_question_asked_during_a_permission_prompt_gives_the_prompt_back_its_typed_yes() {
    // Review finding: a task's question replaced the waiting permission prompt for good, so a typed yes went to
    // Claude and the prompt waited out its timeout.
    let _iso = isolated();
    let typed = Arc::new(Mutex::new(Vec::new()));
    let rig = relay_rig(FakeApi::new(vec![]), &typed, None);
    let inlet = rig.inlet.clone();
    let permission = tokio::spawn(async move { inlet.decide_permission("Bash", "rm -rf build/", "", false).await });
    jobs(&rig).await;
    let question = spawn_ask(&rig, "Which account?");
    jobs(&rig).await;
    dispatch(&rig, "work", 5).await;
    assert_eq!(question.await.unwrap(), "work");
    dispatch(&rig, "yes", 6).await;
    assert!(permission.await.unwrap().as_deref() == Some("allow") && typed.lock().is_empty());
    shutdown(&rig).await;
}

// ---- no stale "typing…" or "Thinking…" (2026-09-23) -----------------------------------------------------------

#[tokio::test]
async fn test_a_relay_typing_that_ended_while_a_question_waited_does_not_come_back_after_the_answer() {
    // Review finding: _ask_user paused every reason and put them all back, so the relay's "typing…" came back after
    // the answer although Claude's turn had ended meanwhile.
    let _iso = isolated();
    let typed = Arc::new(Mutex::new(Vec::new()));
    let rig = typing_relay_rig(FakeApi::new(vec![]), &typed, &[REPO]);
    dispatch(&rig, "run the tests", 3).await;
    assert!(reasons(&rig).contains(&"relay".to_string()));
    rig.inlet.tt_keep_typing(OWNER, "turn", 60.0);
    let question = spawn_ask(&rig, "Which account?");
    jobs(&rig).await;
    assert!(rig.inlet.tt_typing(OWNER).is_none()); // nothing under the question
    rig.inlet.relay_turn_ended(REPO); // Claude finished while it waited
    dispatch(&rig, "work", 4).await;
    assert_eq!(question.await.unwrap(), "work");
    assert_eq!(reasons(&rig), vec!["turn"]); // eve's own turn resumes; the relay's does not
    shutdown(&rig).await;
}

#[tokio::test]
async fn test_a_question_asked_under_another_resumes_no_typing_and_the_outer_answer_resumes_only_live_reasons() {
    // Review finding: nested questions in one chat shared one paused set, and answering the inner one put back every
    // reason, the outer question's too, so "typing…" showed under a question still waiting.
    let _iso = isolated();
    let typed = Arc::new(Mutex::new(Vec::new()));
    let rig = typing_relay_rig(FakeApi::new(vec![]), &typed, &[REPO]);
    rig.inlet.tt_keep_typing(OWNER, "task", 60.0);
    let outer = spawn_ask(&rig, "Which account?");
    jobs(&rig).await;
    rig.inlet.tt_keep_typing(OWNER, "turn", 60.0);
    let inner = spawn_ask(&rig, "Which folder?");
    jobs(&rig).await;
    assert!(rig.inlet.tt_typing(OWNER).is_none());
    dispatch(&rig, "docs", 3).await;
    assert_eq!(inner.await.unwrap(), "docs");
    assert!(rig.inlet.tt_typing(OWNER).is_none()); // the outer question still waits
    rig.inlet.tt_stop_typing(OWNER, "task"); // the task ended while it waited
    dispatch(&rig, "work", 4).await;
    assert_eq!(outer.await.unwrap(), "work");
    assert_eq!(reasons(&rig), vec!["turn"]); // the ended reason does not come back
    assert!(!rig.inlet.tt_paused_typing(OWNER));
    shutdown(&rig).await;
}

#[tokio::test]
async fn test_a_stop_from_a_session_eve_does_not_know_leaves_the_joined_sessions_bubble() {
    // Review finding: the daemon passes no folder for a Stop hook whose session is not in its state, and that ended
    // the picked session's "Thinking…" early.
    let _iso = isolated();
    let rig = draft_relay_rig(FakeApi::new(vec![]));
    rig.inlet.tt_relay_to(REPO);
    rig.inlet.tt_set_chat_id(Some(OWNER));
    dispatch(&rig, "run the tests", 3).await;
    assert!(rig.inlet.tt_thinking());
    rig.inlet.relay_turn_ended("");
    assert!(rig.inlet.tt_thinking());
    rig.inlet.relay_turn_ended(REPO); // the control: its own Stop ends it
    assert!(!rig.inlet.tt_thinking());
    shutdown(&rig).await;
}

#[tokio::test]
async fn test_claude_on_opening_the_picker_ends_the_last_sessions_typing() {
    // Review finding: the picker turned the relay off, after which nothing ever ended its "typing…".
    let _iso = isolated();
    let api = FakeApi::new(vec![]);
    let typed = Arc::new(Mutex::new(Vec::new()));
    let rig = typing_relay_rig(api.clone(), &typed, &[REPO, "/Users/g/other"]);
    dispatch(&rig, "run the tests", 3).await;
    assert!(reasons(&rig).contains(&"relay".to_string()));
    dispatch(&rig, "claude on", 4).await;
    assert!(!rig.inlet.claude() && sent(&api).last().unwrap().1 == telegram::CLAUDE_PICK_LINE);
    assert!(!reasons(&rig).contains(&"relay".to_string()));
    shutdown(&rig).await;
}

#[tokio::test]
async fn test_a_quick_turn_that_ends_while_the_receipt_is_on_its_way_leaves_no_bubble() {
    // Review finding: "Thinking…" started only after the 👍 was awaited, so a Stop hook landing during that await
    // found nothing to end, and the bubble then ran to its five-minute cap.
    let _iso = isolated();
    let inner = FakeApi::new(vec![]);
    let (gate, reacting) = (Arc::new(Flag::new()), Arc::new(Flag::new()));
    let mut api = Hooked::new(inner.clone());
    let (g, r) = (gate.clone(), reacting.clone());
    api.before_react = Some(Box::new(move || {
        let (g, r) = (g.clone(), r.clone());
        async move {
            r.set();
            g.wait().await;
        }
        .boxed()
    }));
    let rig = draft_relay_rig(Arc::new(api));
    rig.inlet.tt_relay_to(REPO);
    rig.inlet.tt_set_chat_id(Some(OWNER));
    rig.inlet.say_relay_probe().dispatch(&Value::Object(update(3, "what is 2+2", OWNER, NOW, 3)));
    reacting.wait().await;
    rig.inlet.relay_turn_ended(REPO); // the answer came before the 👍 landed
    gate.set();
    jobs(&rig).await;
    assert!(inner.log.lock().reactions.last().unwrap().2 == telegram::TYPED_REACTION && !rig.inlet.tt_thinking());
    shutdown(&rig).await;
}

// ---- prompts and progress messages end saying the truth (2026-09-23) ------------------------------------------

#[tokio::test]
async fn test_a_question_whose_asker_went_away_does_not_claim_it_was_taken_as_a_no() {
    // Review finding: a cancelled ask (Chrome answered at the Mac, a stopped task) was edited to "No answer, so I
    // took that as a no.", which never happened.
    let _iso = isolated();
    let api = FakeApi::new(vec![]);
    let rig = Rig::new(api.clone(), FakeCreate::new(vec![]), config(), |_| {});
    let ask = spawn_ask(&rig, "Open Chrome? yes / no?");
    jobs(&rig).await;
    ask.abort();
    let _ = ask.await;
    jobs(&rig).await;
    let last = api.log.lock().edits.last().cloned().unwrap().1;
    assert!(last.ends_with(telegram::ASK_CLOSED_LINE), "{last}");
    assert!(!last.contains(telegram::UNANSWERED_LINE));
    // A restart during a question: nothing is left running after _shutdown
    let _left = spawn_ask(&rig, "Open Chrome? yes / no?");
    jobs(&rig).await;
    shutdown(&rig).await;
    assert_eq!(rig.inlet.tt_jobs_left(), 0);
}

#[tokio::test]
async fn test_a_prompt_longer_than_one_message_keeps_its_text_and_says_the_outcome_below() {
    // Review finding: the settling edit wrote the prompt's first piece over its last one and cut the outcome line. A
    // long prompt loses its buttons and the outcome goes as its own message.
    let _iso = isolated();
    let api = FakeApi::new(vec![]);
    let rig = Rig::new(api.clone(), FakeCreate::new(vec![]), config(), |_| {});
    let question: &'static str = Box::leak(format!("{}\n\nyes / no?", vec!["word ".repeat(700); 2].join("\n\n")).into_boxed_str());
    let ask = spawn_ask(&rig, question);
    jobs(&rig).await;
    let (message_id, key) = api.key("Allow", -1);
    tap(&rig, &key, message_id).await;
    assert_eq!(ask.await.unwrap(), "yes");
    jobs(&rig).await;
    let log = api.log.lock();
    assert!(log.edits.is_empty() && log.dropped.contains(&message_id) && log.sent.last().unwrap() == &(OWNER, "Allowed.".to_string()));
    drop(log);
    shutdown(&rig).await;
}

#[tokio::test]
async fn test_the_owners_own_tap_outside_their_chat_is_answered_and_a_strangers_is_not() {
    let _iso = isolated();
    let api = FakeApi::new(vec![]);
    let rig = Rig::new(api.clone(), FakeCreate::new(vec![]), config(), |_| {});
    tap_obj(&rig, tap_update(90, 5, "k.1", STRANGER, "q1")).await;
    assert!(api.log.lock().answered.is_empty());
    let mut group = tap_update(90, 5, "k.1", OWNER, "q1");
    group["callback_query"]["message"]["chat"] = json!({"id": -100, "type": "group"});
    tap_obj(&rig, group).await;
    assert!(api.log.lock().answered == vec![("q1".to_string(), String::new())] && api.log.lock().sent.is_empty());
    shutdown(&rig).await;
}

#[tokio::test]
async fn test_a_restart_closes_an_open_progress_message_and_its_stop_button() {
    // Review finding: after a restart the progress message still said "On it" above a Stop button.
    let _iso = isolated();
    let api = FakeApi::new(vec![]);
    let rig = Rig::new(api.clone(), FakeCreate::new(vec![]), config(), |_| {});
    let progress = rig.inlet.tt_open_progress(OWNER, ON_IT_LINE);
    rig.inlet.tt_set_task_progress(Some(progress));
    jobs(&rig).await;
    shutdown(&rig).await;
    let log = api.log.lock();
    assert_eq!(log.edits.last().cloned().unwrap(), (log.message_id, format!("{ON_IT_LINE}\n\n{}", telegram::PROGRESS_STOPPED_LINE)));
    assert!(log.edit_keyboards.last().unwrap().1.is_none());
}

#[tokio::test]
async fn test_a_step_during_the_first_send_waits_out_its_second_before_the_edit() {
    // Review finding: the pacing wait was read before the lock; a step arriving while the first send held it saw no
    // send yet and edited the moment the send landed.
    let _iso = isolated();
    let now = Arc::new(Mutex::new(100.0_f64));
    let stamps: Arc<Mutex<Vec<(String, f64)>>> = Arc::new(Mutex::new(Vec::new()));
    let gate = Arc::new(Flag::new());
    let inner = FakeApi::new(vec![]);
    let mut api = Hooked::new(inner);
    let (g, s, n) = (gate.clone(), stamps.clone(), now.clone());
    api.before_inline = Some(Box::new(move || {
        let (g, s, n) = (g.clone(), s.clone(), n.clone());
        async move {
            g.wait().await;
            s.lock().push(("send".into(), *n.lock()));
        }
        .boxed()
    }));
    let (s, n) = (stamps.clone(), now.clone());
    api.before_edit = Some(Box::new(move || s.lock().push(("edit".into(), *n.lock()))));
    let (n1, n2) = (now.clone(), now.clone());
    let clock: Clock = Arc::new(move || *n1.lock());
    let rig = rig_over(Arc::new(api), FakeCreate::new(vec![]), move |o| {
        o.clock = Some(clock);
        o.sleep = Some(Arc::new(move |secs: f64| {
            *n2.lock() += secs;
            async { tokio::task::yield_now().await }.boxed()
        }));
    });
    let progress = rig.inlet.tt_open_progress(OWNER, ON_IT_LINE);
    settle(200).await;
    rig.inlet.tt_progress_step(&progress, "Opening the calculator");
    settle(200).await;
    gate.set();
    jobs(&rig).await;
    let stamps = stamps.lock().clone();
    let ((_, sent_at), (_, edited_at)) = (stamps[0].clone(), stamps[1].clone());
    assert!(edited_at - sent_at >= telegram::PROGRESS_EDIT_SECS, "{stamps:?}");
    shutdown(&rig).await;
}

// ---- receipts, labels and the chat's record (2026-09-23) ------------------------------------------------------

#[tokio::test]
async fn test_a_delivered_image_whose_thumbs_up_fails_does_not_keep_its_eyes() {
    // Review finding: the 👀 ("on its way") stayed on a delivered image when the 👍 could not be set.
    let _iso = isolated();
    let inner = FakeApi::new(vec![]);
    *inner.image.lock() = Some(Ok(eve_telegram_images::ReceivedImage::new(vec![0xFF, 0xD8, 0xFF, 0xD9], "image/jpeg", "photo.jpg")));
    let mut api = Hooked::new(inner.clone());
    let i = inner.clone();
    api.react = Some(Box::new(move |chat, message, emoji| {
        if emoji == telegram::DELIVERED_REACTION {
            return Err(api_error(400, "Bad Request: REACTION_INVALID"));
        }
        i.log.lock().reactions.push((chat, message, emoji.to_string()));
        Ok(())
    }));
    let typed = Arc::new(Mutex::new(Vec::new()));
    let rig = relay_rig(Arc::new(api), &typed, None);
    // `media.save` is the real one here: it writes to the temp folder telegram_images keeps for the relay
    let inbound = Inbound { image: Some(eve_telegram_images::Attachment::new("f")), message_id: 5, ..Inbound::new(OWNER, OWNER, "look") };
    rig.inlet.tt_image(&inbound, "claude", 0).await;
    let emojis: Vec<String> = inner.log.lock().reactions.iter().map(|r| r.2.clone()).collect();
    assert!(!typed.lock().is_empty() && emojis == vec![telegram::SEEN_REACTION.to_string(), String::new()], "{emojis:?}");
    assert_eq!(sent(&inner).last().cloned().unwrap(), (OWNER, "Typed.".to_string())); // the line stands for the 👍
    shutdown(&rig).await;
}

#[tokio::test]
async fn test_a_reply_link_refused_for_another_reason_is_not_resent_whole() {
    // Review finding: any error on a message with a reply link resent it whole; after a 429 on a later piece the first
    // pieces arrived twice. Only a 400 (the link or box refused) is resent plain.
    let _iso = isolated();
    let attempts = Arc::new(Mutex::new(0));
    let mut api = Hooked::new(FakeApi::new(vec![]));
    let a = attempts.clone();
    api.before_message = Some(Box::new(move || {
        *a.lock() += 1;
        Some(api_error(429, "Too Many Requests: retry after 3"))
    }));
    let rig = rig_over(Arc::new(api), FakeCreate::new(vec![]), |_| {});
    rig.inlet.tt_say_reply(OWNER, "the result", 7).await;
    assert_eq!(*attempts.lock(), 1);
    let api = FakeApi::new(vec![]);
    api.refuse_links.store(true, std::sync::atomic::Ordering::SeqCst); // the control: a refused link still goes plain
    let rig = Rig::new(api.clone(), FakeCreate::new(vec![]), config(), |_| {});
    rig.inlet.tt_say_reply(OWNER, "the result", 7).await;
    assert_eq!(sent(&api), vec![(OWNER, "the result".to_string())]);
}

#[tokio::test]
async fn test_a_tapped_question_option_is_kept_in_the_chats_record() {
    let _iso = isolated();
    let dir = tmp();
    let memory = FakeMemory::new(dir.path(), "");
    let api = FakeApi::new(vec![]);
    let typed = Arc::new(Mutex::new(Vec::new()));
    let rig = relay_rig(api.clone(), &typed, Some(memory.clone()));
    rig.inlet.relay_tool_call("AskUserQuestion", "Which one? (1. era-maker / 2. eve)");
    jobs(&rig).await;
    let (message_id, key) = api.key("2. eve", -1);
    tap(&rig, &key, message_id).await;
    // the record is the transcript; a relay line never enters the text brain's history
    assert!(*typed.lock() == vec!["2"] && !rig.inlet.turns().contains(&("user".to_string(), "2. eve".to_string())));
    shutdown(&rig).await;
    let rows: Vec<(String, String, String)> = memory
        .said()
        .iter()
        .filter(|l| l.get("kind").and_then(Value::as_str) != Some("close"))
        .map(|l| {
            let s = |k: &str| l.get(k).and_then(Value::as_str).unwrap_or("").to_string();
            (s("who"), s("kind"), s("text"))
        })
        .collect();
    assert!(rows.contains(&("owner".to_string(), "relay".to_string(), "2. eve".to_string())), "{rows:?}");
}
