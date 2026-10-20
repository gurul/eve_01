//! Unit `inlet_say_relay`: telegram.py 3715-4773 and 7078-7339 (inlet/say, relay, tasks); tests: test_telegram.py the terminal relay (1210-1456), inline buttons (3013-3635), task progress (3636-4214).
//!
//! The shared fakes are in `support` (architect-owned); this unit's own go here. What Python's tests read or set as
//! private attributes is reached through `TelegramInlet::say_relay_probe`. Python's tests that need FakeCodex (the
//! Codex relay's progress, steer and folder tests) have no Rust counterpart: `CodexChat` is concrete and its fake
//! transport lives in a crate this one does not depend on. The Codex progress tests whose subject is the progress
//! message itself (a long step kept whole, a step too long, steps scrolling off) run here on a task's progress
//! message, which is the same `_Progress`.

mod support;

use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use futures::FutureExt;
use parking_lot::Mutex;
use serde_json::{json, Value};

use eve_agent_contract::AgentEvent;
use eve_brain::prelude::{Flag, Sessions, Sleep};
use eve_brain::telegram::*;
use support::*;

const STRANGER: i64 = 666;

// ---- this unit's own helpers ---------------------------------------------------------------------------------------

/// FakeApi with the overrides Python's tests make by subclassing it or by assigning its methods: inline keyboards
/// refused (or held, then sent or refused), taps not answerable, a plain send held on one text, and the first edit
/// running code around itself.
struct Hooked {
    inner: Arc<FakeApi>,
    refuse_inline: Mutex<Option<BotApiError>>,
    /// set when an inline send begins; the send waits for `inline_gate`
    inline_sending: Flag,
    inline_gate: Mutex<Option<Arc<Flag>>>,
    fail_answers: Mutex<Option<BotApiError>>,
    /// a plain send of exactly this text waits for the flag
    send_gate: Mutex<Option<(String, Arc<Flag>)>>,
    edits: AtomicUsize,
    /// (before, after) the first edit
    first_edit: Mutex<Option<(Box<dyn FnOnce() + Send>, Box<dyn FnOnce() + Send>)>>,
}

impl Hooked {
    fn new(inner: Arc<FakeApi>) -> Arc<Self> {
        Arc::new(Self {
            inner,
            refuse_inline: Mutex::new(None),
            inline_sending: Flag::new(),
            inline_gate: Mutex::new(None),
            fail_answers: Mutex::new(None),
            send_gate: Mutex::new(None),
            edits: AtomicUsize::new(0),
            first_edit: Mutex::new(None),
        })
    }

    fn refusing(inner: Arc<FakeApi>) -> Arc<Self> {
        let me = Self::new(inner);
        *me.refuse_inline.lock() = Some(api_error(400, "Bad Request: inline keyboards are not allowed"));
        me
    }
}

#[async_trait]
impl TelegramApi for Hooked {
    async fn get_me(&self) -> Result<Obj, BotApiError> {
        self.inner.get_me().await
    }

    async fn get_updates(&self, offset: Option<i64>, timeout: i64) -> Result<Vec<Obj>, BotApiError> {
        self.inner.get_updates(offset, timeout).await
    }

    async fn receive_image(&self, item: &eve_telegram_images::Attachment) -> Result<eve_telegram_images::ReceivedImage, ApiImageError> {
        self.inner.receive_image(item).await
    }

    async fn send_message(&self, chat_id: i64, text: &str, title: Option<&str>, subtitle: Option<&str>, buttons: &[String], reply_to: i64, force_reply: Option<&str>) -> Result<(), BotApiError> {
        let gate = self.send_gate.lock().clone();
        if let Some((held, gate)) = gate {
            if held == text {
                gate.wait().await; // the step is on its way when work ends
            }
        }
        self.inner.send_message(chat_id, text, title, subtitle, buttons, reply_to, force_reply).await
    }

    async fn send_inline(&self, chat_id: i64, text: &str, keyboard: &[Vec<(String, String, String)>], title: Option<&str>, subtitle: Option<&str>) -> Result<i64, BotApiError> {
        self.inline_sending.set();
        let gate = self.inline_gate.lock().clone();
        if let Some(gate) = gate {
            gate.wait().await;
        }
        if let Some(e) = self.refuse_inline.lock().clone() {
            return Err(e);
        }
        self.inner.send_inline(chat_id, text, keyboard, title, subtitle).await
    }

    async fn edit_message(&self, chat_id: i64, message_id: i64, text: &str, title: Option<&str>, subtitle: Option<&str>, keyboard: Option<&[Vec<(String, String, String)>]>) -> Result<(), BotApiError> {
        let n = self.edits.fetch_add(1, Ordering::SeqCst) + 1;
        let hook = if n == 1 { self.first_edit.lock().take() } else { None };
        match hook {
            Some((before, after)) => {
                before();
                tokio::task::yield_now().await;
                let edited = self.inner.edit_message(chat_id, message_id, text, title, subtitle, keyboard).await;
                after();
                edited
            }
            None => self.inner.edit_message(chat_id, message_id, text, title, subtitle, keyboard).await,
        }
    }

    async fn drop_buttons(&self, chat_id: i64, message_id: i64) -> Result<(), BotApiError> {
        self.inner.drop_buttons(chat_id, message_id).await
    }

    async fn answer_callback(&self, query_id: &str, text: &str) -> Result<(), BotApiError> {
        if let Some(e) = self.fail_answers.lock().clone() {
            return Err(e);
        }
        self.inner.answer_callback(query_id, text).await
    }

    async fn react(&self, chat_id: i64, message_id: i64, emoji: &str) -> Result<(), BotApiError> {
        self.inner.react(chat_id, message_id, emoji).await
    }

    async fn send_photo(&self, chat_id: i64, path: &std::path::Path, caption: &str) -> Result<(), BotApiError> {
        self.inner.send_photo(chat_id, path, caption).await
    }

    async fn send_document(&self, chat_id: i64, path: &std::path::Path, caption: &str) -> Result<(), BotApiError> {
        self.inner.send_document(chat_id, path, caption).await
    }

    async fn typing(&self, chat_id: i64) -> Result<(), BotApiError> {
        self.inner.typing(chat_id).await
    }

    async fn send_draft(&self, chat_id: i64, draft_id: i64, text: &str) -> Result<(), BotApiError> {
        self.inner.send_draft(chat_id, draft_id, text).await
    }

    async fn send_web_apps(&self, chat_id: i64, text: &str, buttons: &[(String, String)]) -> Result<(), BotApiError> {
        self.inner.send_web_apps(chat_id, text, buttons).await
    }

    async fn send_url_buttons(&self, chat_id: i64, text: &str, buttons: &[(String, String)]) -> Result<(), BotApiError> {
        self.inner.send_url_buttons(chat_id, text, buttons).await
    }

    async fn send_photo_web_apps(&self, chat_id: i64, jpeg: &[u8], caption: &str, buttons: &[(String, String)]) -> Result<(), BotApiError> {
        self.inner.send_photo_web_apps(chat_id, jpeg, caption, buttons).await
    }

    async fn set_commands(&self, commands: &[(String, String)], chat_id: i64) -> Result<(), BotApiError> {
        self.inner.set_commands(commands, chat_id).await
    }
}

/// How the lent `sleep` behaves, switchable mid-test (Python's tests assign `rig.inlet._sleep`).
#[derive(Clone, Copy, PartialEq)]
enum Nap {
    /// `await asyncio.sleep(0)`
    Yield,
    /// `held_second`: a second that never ends
    Hold,
    /// `paced`: the clock moves on by the wait, which is recorded
    Paced,
}

struct Naps {
    mode: Mutex<Nap>,
    waits: Mutex<Vec<f64>>,
    clock: OnceLock<FakeClock>,
}

impl Naps {
    fn new(mode: Nap) -> Arc<Self> {
        Arc::new(Self { mode: Mutex::new(mode), waits: Mutex::new(Vec::new()), clock: OnceLock::new() })
    }

    fn set(&self, mode: Nap) {
        *self.mode.lock() = mode;
    }

    fn sleep(self: &Arc<Self>) -> Sleep {
        let me = self.clone();
        Arc::new(move |secs: f64| {
            let me = me.clone();
            async move {
                let mode = *me.mode.lock();
                match mode {
                    Nap::Yield => tokio::task::yield_now().await,
                    Nap::Hold => futures::future::pending::<()>().await,
                    Nap::Paced => {
                        me.waits.lock().push(secs);
                        if let Some(clock) = me.clock.get() {
                            clock.advance(secs);
                        }
                        tokio::task::yield_now().await;
                    }
                }
            }
            .boxed()
        })
    }
}

/// `Rig(api, create, config, **kw)` over a [`Hooked`] api, with a switchable sleep.
struct TRig {
    api: Arc<FakeApi>,
    hooked: Arc<Hooked>,
    create: Arc<FakeCreate>,
    agents: Arc<Agents>,
    clock: FakeClock,
    naps: Arc<Naps>,
    inlet: Arc<TelegramInlet>,
}

impl TRig {
    fn new(hooked: Arc<Hooked>, create: Arc<FakeCreate>, config: TelegramConfig, tweak: impl FnOnce(&mut InletOptions)) -> Self {
        let agents = Arc::new(Agents::default());
        let clock = FakeClock::new(0.0);
        let naps = Naps::new(Nap::Yield);
        let _ = naps.clock.set(clock.clone());
        let mut options = InletOptions {
            agent_factory: Some(agents.factory()),
            brief: Some(Arc::new(String::new)),
            on_state: Some(Arc::new(|_state: &str| {})),
            clock: Some(clock.clock()),
            wall: Some(Arc::new(|| NOW)),
            sleep: Some(naps.sleep()),
            screen: Some(Arc::new(|| None)),
            toolset_route: eve_brain::prelude::Lent::Given(None),
            grader: eve_brain::prelude::Lent::Given(None),
            small_talk_ask: eve_brain::prelude::Lent::Given(None),
            environ: Some(Default::default()),
            ..InletOptions::default()
        };
        tweak(&mut options);
        let inlet = TelegramInlet::new(config, hooked.clone(), create.create(), options);
        Self { api: hooked.inner.clone(), hooked, create, agents, clock, naps, inlet }
    }

    fn plain(api: Arc<FakeApi>, create: Arc<FakeCreate>, config: TelegramConfig, tweak: impl FnOnce(&mut InletOptions)) -> Self {
        Self::new(Hooked::new(api), create, config, tweak)
    }

    fn probe(&self) -> SayRelayProbe {
        self.inlet.say_relay_probe()
    }
}

/// `run_rig(rig, during)`: the poll loop runs, `during` runs while it does, and the loop must still be alive
/// before it is cancelled.
async fn run_rig(rig: &TRig, during: impl Future<Output = ()>) {
    let inlet = rig.inlet.clone();
    let looping = tokio::spawn(async move { inlet.run().await });
    settle(200).await;
    during.await;
    settle(200).await;
    // A released task's end reads its files on a worker thread (`asyncio.to_thread(task_files, ...)`), which yields
    // alone may not wait out on a loaded machine: give it real time, so the shutdown below does not find it running.
    for _ in 0..2000 {
        if !rig.inlet.task_running() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    }
    settle(200).await;
    assert!(!looping.is_finished(), "the poll loop died");
    looping.abort();
    let _ = looping.await;
    settle(200).await;
}

/// `update(text, update_id=...)`: from the owner, in their private chat, message id = update id.
fn msg(text: &str, update_id: i64) -> Obj {
    update(update_id, text, OWNER, NOW, update_id)
}

/// `await jobs(rig)`: settle, then wait for every job the inlet spawned.
async fn jobs(rig: &TRig) {
    settle(200).await;
    rig.probe().jobs().await;
}

/// `await dispatch(rig, text, update_id=...)`.
async fn dispatch(rig: &TRig, text: &str, update_id: i64) {
    rig.probe().dispatch(&Value::Object(msg(text, update_id)));
    rig.probe().jobs().await;
}

/// `await tap(rig, key, message_id, uid=..., query_id=...)`.
async fn tap_as(rig: &TRig, key: &str, message_id: i64, user: i64, query_id: &str) {
    rig.probe().dispatch(&Value::Object(tap_update(90, message_id, key, user, query_id)));
    jobs(rig).await;
}

async fn tap(rig: &TRig, key: &str, message_id: i64) {
    tap_as(rig, key, message_id, OWNER, "q1").await;
}

fn sent(rig: &TRig) -> Vec<(i64, String)> {
    rig.api.log.lock().sent.clone()
}

fn owner_says(texts: &[&str]) -> Vec<(i64, String)> {
    texts.iter().map(|t| (OWNER, t.to_string())).collect()
}

fn titled_last(rig: &TRig) -> (Option<String>, Option<String>, String) {
    rig.api.log.lock().titled.last().cloned().expect("nothing sent")
}

fn last_keyboard_id(rig: &TRig) -> i64 {
    rig.api.log.lock().keyboards.last().expect("no keyboard").0
}

fn some(s: &str) -> Option<String> {
    Some(s.to_string())
}

type Typed = Arc<Mutex<Vec<(String, String)>>>;

fn terminal(typed: &Typed) -> eve_brain::prelude::Terminal {
    let typed = typed.clone();
    Arc::new(move |cwd: String, text: String| {
        typed.lock().push((cwd, text));
        async { Ok(String::new()) }.boxed() // went in: a reaction, no line
    })
}

fn sessions(cwds: &[&str]) -> eve_brain::prelude::ClaudeSessions {
    let cwds: Vec<String> = cwds.iter().map(|c| c.to_string()).collect();
    Arc::new(move || Ok(Sessions::Now(cwds.clone())))
}

fn texts_typed(typed: &Typed) -> Vec<String> {
    typed.lock().iter().map(|(_, t)| t.clone()).collect()
}

/// `relay_rig(api, typed)`: the relay on in /Users/g/repo, permission prompts asked.
fn relay_rig(hooked: Arc<Hooked>, typed: &Typed, create: Arc<FakeCreate>, permission_timeout: f64) -> TRig {
    let config = TelegramConfig { ask_permissions: true, ..config() };
    let rig = TRig::new(hooked, create, config, |o| {
        o.terminal = Some(terminal(typed));
        o.claude_sessions = Some(sessions(&["/Users/g/repo"]));
        o.permission_timeout_secs = permission_timeout;
    });
    rig.probe().relay_to("/Users/g/repo");
    rig.probe().set_chat(Some(OWNER));
    rig
}

/// `asyncio.ensure_future(inlet.decide_permission(tool, hint, cwd))`.
fn ask_permission(rig: &TRig, tool: &str, hint: &str, cwd: &str) -> tokio::task::JoinHandle<Option<String>> {
    let (inlet, tool, hint, cwd) = (rig.inlet.clone(), tool.to_string(), hint.to_string(), cwd.to_string());
    tokio::spawn(async move { inlet.decide_permission(&tool, &hint, &cwd, false).await })
}

/// `asyncio.ensure_future(inlet._ask_user(question, OWNER, title=...))`.
fn ask_owner(rig: &TRig, question: &str, title: Option<&str>) -> tokio::task::JoinHandle<String> {
    let (probe, question, title) = (rig.probe(), question.to_string(), title.map(str::to_string));
    tokio::spawn(async move { probe.ask_user(&question, OWNER, title.as_deref()).await })
}

fn progress(agent: &FakeAgent, text: &str) {
    (agent.on_event)(&AgentEvent::with_text("progress", text));
}

fn task_create() -> Arc<FakeCreate> {
    FakeCreate::new(vec![call("start_task", json!({"goal": "open the calculator"}), "call_1")])
}

/// `task_rig(api, **kw)`: "open the calculator" starts a task.
fn task_rig(hooked: Arc<Hooked>, mode: Nap, tweak: impl FnOnce(&mut InletOptions)) -> TRig {
    let rig = TRig::new(hooked, task_create(), config(), tweak);
    rig.naps.set(mode);
    rig
}

fn calculator() -> Arc<FakeApi> {
    FakeApi::new(vec![vec![msg("open the calculator", 1)]])
}

fn cant_edit() -> BotApiError {
    api_error(400, "Bad Request: message can't be edited")
}

// ---- "claude on" / "claude off": the terminal relay -----------------------------------------------------------------

#[tokio::test]
async fn test_the_claude_relay_is_off_until_said_and_forwards_only_while_on() {
    // Python's eve turn calls move_head on a FakeHead first (two model calls); the head is not this unit's, so eve's
    // turns here are one reply each and the counts below are Python's minus that call.
    let typed: Typed = Arc::new(Mutex::new(Vec::new()));
    let api = FakeApi::new(vec![vec![msg("claude: run the tests", 1)]]);
    let rig = TRig::plain(api, FakeCreate::new(vec![reply("Looking left."), reply("On it.")]), config(), |o| {
        o.terminal = Some(terminal(&typed));
        o.claude_sessions = Some(sessions(&["/Users/g/repo"]));
    });
    run_rig(&rig, async {
        let inlet = &rig.inlet;
        assert!(!inlet.claude() && sent(&rig) == owner_says(&[CLAUDE_NOT_ON_LINE]) && typed.lock().is_empty());
        inlet.relay_text("I fixed the bug.", "/Users/g/repo").await; // off: nothing forwarded
        inlet.relay_notification("permission_prompt", "Bash needs approval", true).await;
        assert_eq!(sent(&rig).len(), 1);
        rig.api.feed(vec![msg("claude on", 2)]);
        settle(200).await;
        assert!(inlet.claude() && sent(&rig).last().unwrap().1 == CLAUDE_ON_LINE);
        assert_eq!(titled_last(&rig).0, some(CLAUDE_ON_TITLE));
        let long = format!("  I fixed\n the bug. {}", "x".repeat(3000));
        inlet.relay_text(&long, "/Users/g/repo").await;
        settle(200).await;
        // Claude's words under a "Claude" title with the repo under it; its line breaks kept; a long reply arrives
        // whole, never "the rest is in the terminal" (owner, 2026-09-24)
        assert_eq!(titled_last(&rig), (some(CLAUDE_TITLE), some("repo"), format!("I fixed\n the bug. {}", "x".repeat(3000))));
        // only a runaway past the cap is cut at a line or a space, and says the rest is on the Mac
        let runaway = format!("  I fixed\n the bug. {}", "x ".repeat(MAX_RELAY_CHARS));
        inlet.relay_text(&runaway, "/Users/g/repo").await;
        settle(200).await;
        let body = titled_last(&rig).2;
        assert!(body.starts_with("I fixed\n the bug. x x") && body.ends_with(&format!("…\n\n{RELAY_CUT_LINE}")));
        assert!(body.chars().count() <= MAX_RELAY_CHARS + RELAY_CUT_LINE.chars().count() + 4);
        // the terminal's gray lines, a tool call and its result tail, never leave the Mac (owner, 2026-09-21): only
        // what it prints in white does, plus a question for the owner
        let before = sent(&rig).len();
        inlet.relay_tool_call("Bash", "pytest -q");
        inlet.relay_tool_call("Edit", "src/app.py");
        settle(200).await;
        assert_eq!(sent(&rig).len(), before);
        inlet.relay_text("All green.", "/Users/g/repo").await;
        inlet.relay_text("Two tests\nfixed.", "/Users/g/repo").await;
        inlet.relay_tool_call("AskUserQuestion", "Ship it now? (yes / no)");
        settle(200).await;
        // one batch: what Claude said under one title, as paragraphs; its question as its own message
        let titled = rig.api.log.lock().titled.clone();
        assert_eq!(
            titled[titled.len() - 2..].to_vec(),
            vec![
                (some(CLAUDE_TITLE), some("repo"), "All green.\n\nTwo tests\nfixed.".to_string()),
                (some(CLAUDE_ASKS_TITLE), None, "Ship it now? (yes / no)".to_string()),
            ]
        );
        let before = sent(&rig).len();
        inlet.relay_notification("idle_reminder", "still here", false).await; // not waiting: not news
        settle(200).await;
        assert_eq!(sent(&rig).len(), before);
        inlet.relay_notification("permission_prompt", "Bash needs approval", true).await;
        assert_eq!(sent(&rig).len(), before); // right after the question itself: an echo, dropped
        rig.clock.advance(ASKED_RECENTLY_SECS + 1.0);
        inlet.relay_notification("permission_prompt", "Bash needs approval", true).await;
        assert_eq!(titled_last(&rig), (some(CLAUDE_WAITS_TITLE), None, "Bash needs approval".to_string()));
        // relay on: plain text is for Claude
        rig.api.feed(vec![msg("> git status", 3), msg("claude: make it green", 4), msg("now run the tests please", 5)]);
        settle(200).await;
        let repo = |t: &str| ("/Users/g/repo".to_string(), t.to_string());
        assert_eq!(*typed.lock(), vec![repo("git status"), repo("make it green"), repo("now run the tests please")]);
        // each typed line is acknowledged by a reaction on the owner's message, never a "typed" line
        assert_eq!(
            rig.api.log.lock().reactions,
            vec![(OWNER, 3, TYPED_REACTION.to_string()), (OWNER, 4, TYPED_REACTION.to_string()), (OWNER, 5, TYPED_REACTION.to_string())]
        );
        assert!(!sent(&rig).iter().any(|(_, t)| t.to_lowercase().starts_with("typed")));
        rig.api.feed(vec![msg("eve: look left", 6)]); // for eve, by prefix
        settle(200).await;
        assert!(typed.lock().last() == Some(&repo("now run the tests please")) && rig.create.requests.lock().len() == 1);
        rig.api.feed(vec![msg("stealth mode", 7), msg("screenshot", 8)]); // eve's code words still
        settle(200).await;
        assert!(inlet.stealth() && typed.lock().last().unwrap().1 == "now run the tests please" && rig.create.requests.lock().len() == 1);
        rig.api.feed(vec![msg("claude off", 9)]);
        settle(200).await;
        assert!(!inlet.claude() && sent(&rig).last().unwrap().1 == CLAUDE_OFF_LINE);
        rig.api.feed(vec![msg("run the tests", 10)]); // off: a chat turn again
        settle(200).await;
        assert!(typed.lock().last().unwrap().1 == "now run the tests please" && rig.create.requests.lock().len() == 2);
    })
    .await;
}

#[tokio::test]
async fn test_a_permission_prompt_is_answered_from_the_phone_and_silence_defers() {
    let api = FakeApi::new(vec![vec![msg("claude on", 1)]]);
    let asking = TelegramConfig { ask_permissions: true, ..config() };
    let rig = TRig::plain(api, FakeCreate::new(vec![]), asking, |o| {
        // Python's 0.05 s, longer: the suite runs its tests in parallel, and the answers below must land inside it
        o.permission_timeout_secs = 0.5;
        o.claude_sessions = Some(sessions(&["/Users/g/repo"]));
    });
    run_rig(&rig, async {
        let inlet = &rig.inlet;
        let ask = ask_permission(&rig, "Bash", "rm -rf build/", "/Users/g/repo");
        settle(200).await;
        // the command as a code block under a title that names the tool, the repo under it
        assert_eq!(titled_last(&rig), (some("Claude asks to run Bash"), some("repo"), "```\nrm -rf build/\n```\n\nyes / no?".to_string()));
        let text = sent(&rig).last().unwrap().1.clone();
        assert!(eve_telegram_format::compose(&text, Some("Claude asks to run Bash"), Some("repo"))
            .starts_with("<b>Claude asks to run Bash</b>\n<i>repo</i>\n\n<pre>rm -rf build/</pre>\n\nyes / no?"));
        rig.api.feed(vec![update(2, "yes", STRANGER, NOW, 2)]); // a stranger cannot approve
        settle(200).await;
        assert!(!ask.is_finished());
        rig.api.feed(vec![msg("no, leave it", 3)]);
        assert_eq!(ask.await.unwrap().as_deref(), Some("deny"));
        let ask = ask_permission(&rig, "Bash", "pytest -q", "");
        settle(200).await;
        rig.api.feed(vec![msg("yes go", 4)]);
        assert_eq!(ask.await.unwrap().as_deref(), Some("allow"));
        assert_eq!(inlet.decide_permission("Bash", "sleep 1", "", false).await, None); // silence: defer
        assert!(rig.create.requests.lock().is_empty()); // yes/no never became a chat turn
        assert!(inlet.turns().is_empty()); // the prompt and its answers were Claude's: never the history
        // the shipped default: the phone never asks (owner, 2026-09-21)
        let quiet = TRig::plain(FakeApi::new(vec![]), FakeCreate::new(vec![]), config(), |_| {});
        quiet.probe().set_claude(true);
        assert_eq!(quiet.inlet.decide_permission("Bash", "rm -rf /", "", false).await, None);
        assert!(quiet.api.log.lock().sent.is_empty());
    })
    .await;
}

#[tokio::test]
async fn test_a_question_for_the_owner_is_streamed_as_a_question() {
    let asked = "Which database? (1. Postgres / 2. SQLite) | Ship it now?";
    let rig = TRig::plain(FakeApi::new(vec![]), FakeCreate::new(vec![]), config(), |_| {});
    run_rig(&rig, async {
        rig.probe().set_claude(true);
        rig.probe().set_chat(Some(OWNER));
        rig.inlet.relay_tool_call("AskUserQuestion", asked);
        rig.inlet.relay_tool_call("AskUserQuestion", "");
        settle(200).await;
        assert_eq!(
            titled_last(&rig),
            (
                some(CLAUDE_ASKS_TITLE),
                None,
                "Which database? (1. Postgres / 2. SQLite) | Ship it now?\n\nReply with the option's number.\n\n(see the terminal)".to_string()
            )
        );
    })
    .await;
}

// ---- inline buttons: callbacks, Allow/Deny, options, pickers (owner, 2026-09-23) ------------------------------------

#[tokio::test]
async fn test_a_button_key_fits_callback_data_and_is_new_at_every_start() {
    let a = TRig::plain(FakeApi::new(vec![]), FakeCreate::new(vec![]), config(), |_| {});
    let b = TRig::plain(FakeApi::new(vec![]), FakeCreate::new(vec![]), config(), |_| {});
    let keys = a.probe().register_allow_deny(OWNER);
    assert!(keys.iter().all(|k| (1..=64).contains(&k.len())));
    assert_eq!(keys.iter().collect::<std::collections::HashSet<_>>().len(), 2);
    assert_ne!(a.probe().tap_gen(), b.probe().tap_gen()); // a restart's keys are unknown to the next
}

#[tokio::test]
async fn test_allow_and_deny_buttons_answer_a_permission_and_the_prompt_says_what_was_decided() {
    // Python sets `_permission_timeout = 0.01` before the silent prompt; here the timeout is lent at construction,
    // short enough for the silence and long enough for the taps before it.
    let typed: Typed = Arc::new(Mutex::new(Vec::new()));
    let rig = relay_rig(Hooked::new(FakeApi::new(vec![])), &typed, FakeCreate::new(vec![]), 0.5);
    let ask = ask_permission(&rig, "Bash", "rm -rf build/", "/Users/g/repo");
    jobs(&rig).await;
    assert_eq!(rig.api.log.lock().buttons.last().unwrap(), &vec!["Allow".to_string(), "Deny".to_string()]);
    let styles: Vec<String> = rig.api.log.lock().keyboards.last().unwrap().1[0].iter().map(|(_, _, s)| s.clone()).collect();
    assert_eq!(styles, vec!["success", "danger"]); // green, red
    let (message_id, key) = rig.api.key("Allow", -1);
    tap_as(&rig, &key, message_id, STRANGER, "q1").await; // a stranger's tap does nothing at all
    assert!(!ask.is_finished() && rig.api.log.lock().answered.is_empty());
    tap(&rig, &key, message_id).await;
    assert_eq!(ask.await.unwrap().as_deref(), Some("allow"));
    jobs(&rig).await;
    assert_eq!(rig.api.log.lock().answered.last().unwrap(), &("q1".to_string(), "Allowed.".to_string()));
    assert_eq!(rig.api.log.lock().edits.last().unwrap(), &(message_id, "```\nrm -rf build/\n```\n\nAllowed.".to_string()));
    tap(&rig, &key, message_id).await; // the same button again: spent
    assert_eq!(rig.api.log.lock().answered.last().unwrap().1, TAP_EXPIRED_LINE);
    assert_eq!(*rig.api.log.lock().dropped.last().unwrap(), message_id);
    // Deny, then silence: each prompt ends saying so
    let ask = ask_permission(&rig, "Bash", "sudo reboot", "");
    jobs(&rig).await;
    let deny = rig.api.key("Deny", -1).1;
    tap(&rig, &deny, last_keyboard_id(&rig)).await;
    assert_eq!(ask.await.unwrap().as_deref(), Some("deny"));
    assert_eq!(rig.inlet.decide_permission("Bash", "sleep 1", "", false).await, None);
    jobs(&rig).await;
    assert!(rig.api.log.lock().edits.last().unwrap().1.ends_with(PERMISSION_DEFERRED_LINE));
    assert!(rig.create.requests.lock().is_empty() && typed.lock().is_empty());
    rig.probe().shutdown().await;
}

#[tokio::test]
async fn test_while_allow_deny_waits_other_text_goes_to_claude_and_a_typed_yes_still_answers() {
    // Relay weakness 3: the owner's next message used to be eaten as the answer, and lost.
    let typed: Typed = Arc::new(Mutex::new(Vec::new()));
    let rig = relay_rig(Hooked::new(FakeApi::new(vec![])), &typed, FakeCreate::new(vec![]), 120.0);
    let ask = ask_permission(&rig, "Bash", "rm -rf build/", "");
    jobs(&rig).await;
    dispatch(&rig, "also run the linter after", 5).await;
    assert!(texts_typed(&typed) == ["also run the linter after"] && !ask.is_finished()); // to Claude; the prompt waits
    dispatch(&rig, "no, leave it", 6).await;
    assert_eq!(ask.await.unwrap().as_deref(), Some("deny"));
    assert_eq!(texts_typed(&typed), ["also run the linter after"]);
    let ask = ask_permission(&rig, "Bash", "pytest -q", "");
    jobs(&rig).await;
    dispatch(&rig, "yes go", 7).await;
    assert_eq!(ask.await.unwrap().as_deref(), Some("allow"));
    jobs(&rig).await;
    assert!(rig.api.log.lock().edits.last().unwrap().1.ends_with("Allowed."));
    rig.probe().shutdown().await;
}

#[tokio::test]
async fn test_while_allow_deny_waits_a_sentence_opening_with_yes_or_no_goes_to_claude() {
    // Review finding: a sentence whose first word is a yes-word used to allow an always_ask rm.
    for text in ["ok, also update the README when you're done", "no worries, keep going with X", "go check the logs"] {
        let typed: Typed = Arc::new(Mutex::new(Vec::new()));
        let rig = relay_rig(Hooked::new(FakeApi::new(vec![])), &typed, FakeCreate::new(vec![]), 120.0);
        let mut ask = ask_permission(&rig, "Bash", "rm -rf build/", "");
        jobs(&rig).await;
        dispatch(&rig, text, 5).await;
        assert!(texts_typed(&typed) == [text] && !ask.is_finished(), "{text}"); // to Claude; nothing decided
        for (answer, verdict) in [("go ahead", "allow"), ("no", "deny"), ("yes", "allow")] {
            dispatch(&rig, answer, 6).await;
            assert_eq!(ask.await.unwrap().as_deref(), Some(verdict), "{text} / {answer}");
            ask = ask_permission(&rig, "Bash", "rm -rf build/", "");
            jobs(&rig).await;
        }
        assert_eq!(texts_typed(&typed), [text]);
        ask.abort();
        rig.probe().shutdown().await;
    }
}

#[tokio::test]
async fn test_turning_the_relay_off_settles_a_waiting_permission_prompt_for_the_mac_dialog() {
    // Review finding: after "claude off" the Allow/Deny prompt stayed pending and stopped being strict, so the owner's
    // next message to eve was eaten as its answer, and a yes-opening sentence allowed the rm. (Python's second case,
    // "codex on eve", needs FakeCodex.)
    let typed: Typed = Arc::new(Mutex::new(Vec::new()));
    let rig = relay_rig(Hooked::new(FakeApi::new(vec![])), &typed, FakeCreate::new(vec![reply("Nothing on it.")]), 120.0);
    let ask = ask_permission(&rig, "Bash", "rm -rf ~/work", "");
    jobs(&rig).await;
    let (message_id, key) = rig.api.key("Allow", -1);
    dispatch(&rig, "claude off", 5).await;
    assert_eq!(ask.await.unwrap(), None); // no answer here: the Mac dialog decides
    jobs(&rig).await;
    assert_eq!(
        rig.api.log.lock().edits.last().unwrap(),
        &(message_id, format!("```\nrm -rf ~/work\n```\n\n{PERMISSION_DEFERRED_LINE}"))
    );
    assert!(!rig.probe().awaiting_answer() && !rig.probe().permission_pending());
    tap(&rig, &key, message_id).await; // a late Allow tap is spent
    assert_eq!(rig.api.log.lock().answered.last().unwrap().1, TAP_EXPIRED_LINE);
    dispatch(&rig, "ok, what's on my calendar today", 6).await;
    assert!(rig.create.requests.lock().len() == 1 && typed.lock().is_empty()); // eve's turn: the message is not lost
    rig.probe().shutdown().await;
}

#[tokio::test]
async fn test_a_permission_prompt_stays_strict_even_with_no_relay_on() {
    // Were a prompt ever left waiting with no relay, a sentence that opens with a yes-word goes to eve and allows
    // nothing; only a clear yes or no answers.
    let typed: Typed = Arc::new(Mutex::new(Vec::new()));
    let rig = relay_rig(Hooked::new(FakeApi::new(vec![])), &typed, FakeCreate::new(vec![reply("Nothing on it.")]), 120.0);
    let ask = ask_permission(&rig, "Bash", "rm -rf ~/work", "");
    jobs(&rig).await;
    rig.probe().set_claude(false); // the relay gone, the prompt not settled
    dispatch(&rig, "ok, what's on my calendar today", 6).await;
    assert!(!ask.is_finished() && rig.create.requests.lock().len() == 1);
    dispatch(&rig, "no", 7).await;
    assert_eq!(ask.await.unwrap().as_deref(), Some("deny"));
    rig.probe().shutdown().await;
}

#[tokio::test]
async fn test_moving_the_relay_to_another_session_settles_its_permission_prompt() {
    let typed: Typed = Arc::new(Mutex::new(Vec::new()));
    let rig = relay_rig(Hooked::new(FakeApi::new(vec![])), &typed, FakeCreate::new(vec![]), 120.0);
    let ask = ask_permission(&rig, "Bash", "rm -rf build/", "/Users/g/repo");
    jobs(&rig).await;
    rig.probe().relay_to("/Users/g/repo"); // the same session again: still waiting
    jobs(&rig).await;
    assert!(!ask.is_finished());
    rig.probe().relay_to("/Users/g/other"); // another session: the Mac decides
    assert_eq!(ask.await.unwrap(), None);
    rig.probe().shutdown().await;
}

#[tokio::test]
async fn test_text_sent_while_the_allow_deny_prompt_is_still_on_its_way_goes_to_claude() {
    // Review finding: the prompt was non-strict until sendMessage returned, so text typed then was lost.
    let typed: Typed = Arc::new(Mutex::new(Vec::new()));
    let hooked = Hooked::new(FakeApi::new(vec![]));
    let gate = Arc::new(Flag::new());
    *hooked.inline_gate.lock() = Some(gate.clone());
    let rig = relay_rig(hooked.clone(), &typed, FakeCreate::new(vec![]), 120.0);
    let ask = ask_permission(&rig, "Bash", "rm -rf build/", "");
    hooked.inline_sending.wait().await;
    dispatch(&rig, "also run the linter after", 5).await;
    assert!(texts_typed(&typed) == ["also run the linter after"] && !ask.is_finished());
    gate.set();
    jobs(&rig).await;
    let deny = rig.api.key("Deny", -1).1;
    tap(&rig, &deny, last_keyboard_id(&rig)).await;
    assert_eq!(ask.await.unwrap().as_deref(), Some("deny"));
    assert_eq!(texts_typed(&typed), ["also run the linter after"]);
    rig.probe().shutdown().await;
}

#[tokio::test]
async fn test_when_buttons_cannot_be_sent_the_prompt_is_plain_text_and_the_next_message_answers() {
    let typed: Typed = Arc::new(Mutex::new(Vec::new()));
    let hooked = Hooked::new(FakeApi::new(vec![]));
    *hooked.refuse_inline.lock() = Some(api_error(400, "Bad Request: BUTTON_DATA_INVALID"));
    let rig = relay_rig(hooked, &typed, FakeCreate::new(vec![]), 120.0);
    let ask = ask_permission(&rig, "Bash", "rm -rf build/", "");
    jobs(&rig).await;
    assert_eq!(titled_last(&rig).2, "```\nrm -rf build/\n```\n\nyes / no?");
    assert!(rig.api.log.lock().buttons.last().unwrap().is_empty());
    assert!(rig.probe().taps_empty());
    dispatch(&rig, "maybe later", 5).await; // today's rule: the next message answers
    assert_eq!(ask.await.unwrap(), None);
    assert!(typed.lock().is_empty());
    rig.probe().shutdown().await;
}

#[tokio::test]
async fn test_a_button_from_before_a_restart_or_unknown_is_answered_expired_and_does_nothing() {
    let typed: Typed = Arc::new(Mutex::new(Vec::new()));
    let rig = relay_rig(Hooked::new(FakeApi::new(vec![])), &typed, FakeCreate::new(vec![]), 120.0);
    let ask = ask_permission(&rig, "Bash", "rm -rf build/", "");
    jobs(&rig).await;
    tap(&rig, "0ld0ld.1", 555).await;
    tap(&rig, "", 556).await;
    assert!(!ask.is_finished());
    let expired = ("q1".to_string(), TAP_EXPIRED_LINE.to_string());
    assert_eq!(rig.api.log.lock().answered, vec![expired.clone(), expired]);
    assert_eq!(rig.api.log.lock().dropped, vec![555, 556]);
    ask.abort();
    rig.probe().shutdown().await;
}

#[tokio::test]
async fn test_a_tap_that_breaks_nothing_in_the_poll_loop() {
    let api = FakeApi::new(vec![vec![
        tap_update(1, 1001, "x.1", OWNER, "q1"),
        json!({"update_id": 2, "callback_query": "junk"}).as_object().unwrap().clone(),
        tap_update(3, 1001, "x.2", STRANGER, "q1"),
        msg("hi", 4),
    ]]);
    let hooked = Hooked::new(api);
    *hooked.fail_answers.lock() = Some(api_error(400, "query is too old"));
    let rig = TRig::new(hooked, FakeCreate::new(vec![reply("Hello!")]), config(), |_| {});
    run_rig(&rig, async {}).await;
    assert_eq!(sent(&rig), owner_says(&["Hello!"]));
}

#[tokio::test]
async fn test_a_yes_no_task_question_gets_buttons_and_a_free_one_is_answered_in_words() {
    let rig = TRig::plain(FakeApi::new(vec![]), FakeCreate::new(vec![]), config(), |_| {});
    let asked = ask_owner(&rig, "Create the event Lunch?\n\nyes / no?", Some(APP_ASKS_TITLE));
    jobs(&rig).await;
    assert_eq!(rig.api.log.lock().buttons.last().unwrap(), &vec!["Allow".to_string(), "Deny".to_string()]);
    assert_eq!(titled_last(&rig).0, some(APP_ASKS_TITLE));
    let deny = rig.api.key("Deny", -1).1;
    tap(&rig, &deny, last_keyboard_id(&rig)).await;
    assert_eq!(asked.await.unwrap(), "no");
    jobs(&rig).await;
    assert!(rig.api.log.lock().edits.last().unwrap().1.ends_with("Denied."));
    assert_eq!(rig.api.log.lock().answered.last().unwrap(), &("q1".to_string(), "Denied.".to_string()));
    // a task's question keeps its rule: the next message is the answer, whatever it says
    let asked = ask_owner(&rig, "Should I go ahead: press Buy now?", None);
    jobs(&rig).await;
    dispatch(&rig, "only if it is under 20 dollars", 8).await;
    assert_eq!(asked.await.unwrap(), "only if it is under 20 dollars");
    jobs(&rig).await;
    assert!(rig.api.log.lock().edits.last().unwrap().1.ends_with(ANSWERED_LINE));
    assert!(rig.create.requests.lock().is_empty()); // and that answer was not a new turn too
    let asked = ask_owner(&rig, "Which account?", None);
    jobs(&rig).await;
    assert!(rig.api.log.lock().buttons.last().unwrap().is_empty());
    assert_eq!(titled_last(&rig).2, "Which account?");
    asked.abort();
    rig.probe().shutdown().await;
}

#[tokio::test]
async fn test_an_unanswered_question_with_buttons_says_so_and_loses_them() {
    let quick = TelegramConfig { ask_timeout_secs: 0.01, ..config() };
    let rig = TRig::plain(FakeApi::new(vec![]), FakeCreate::new(vec![]), quick, |_| {});
    let answer = rig.probe().ask_user("Reply yes or no.", OWNER, None).await;
    jobs(&rig).await;
    assert!(answer.starts_with("no (no answer"));
    assert_eq!(sent(&rig).last().unwrap(), &(OWNER, UNANSWERED_LINE.to_string()));
    assert!(rig.api.log.lock().edits.last().unwrap().1.ends_with(UNANSWERED_LINE));
    assert!(rig.probe().taps_empty());
    rig.probe().shutdown().await;
}

#[tokio::test]
async fn test_while_a_yes_no_question_waits_relay_text_goes_to_claude() {
    // Python drives this through a Composio consent (`_app_tool`) and a Codex command approval; both reach the same
    // strict question, asked here directly: with the relay on, a sentence for Claude goes to Claude, and only a
    // bare no or yes answers.
    let typed: Typed = Arc::new(Mutex::new(Vec::new()));
    let rig = relay_rig(Hooked::new(FakeApi::new(vec![])), &typed, FakeCreate::new(vec![]), 120.0);
    let asked = ask_owner(&rig, "Delete the file x?\n\nyes / no?", Some(APP_ASKS_TITLE));
    jobs(&rig).await;
    assert_eq!(rig.api.log.lock().buttons.last().unwrap(), &vec!["Allow".to_string(), "Deny".to_string()]);
    dispatch(&rig, "ok, also update the README", 5).await;
    assert!(texts_typed(&typed) == ["ok, also update the README"] && !asked.is_finished());
    dispatch(&rig, "no", 6).await; // a bare no still answers by typing
    assert_eq!(asked.await.unwrap(), "no");
    let asked = ask_owner(&rig, "Delete the file x?\n\nyes / no?", Some(APP_ASKS_TITLE));
    jobs(&rig).await;
    dispatch(&rig, "yes", 7).await;
    assert_eq!(asked.await.unwrap(), "yes");
    assert_eq!(texts_typed(&typed), ["ok, also update the README"]);
    rig.probe().shutdown().await;
}

#[tokio::test]
async fn test_a_question_whose_buttons_fail_is_answered_by_the_next_message_as_before() {
    // Python's Composio consent whose buttons fail: the question goes plain and stops being strict.
    let typed: Typed = Arc::new(Mutex::new(Vec::new()));
    let hooked = Hooked::new(FakeApi::new(vec![]));
    *hooked.refuse_inline.lock() = Some(api_error(400, "Bad Request: BUTTON_DATA_INVALID"));
    let rig = relay_rig(hooked, &typed, FakeCreate::new(vec![]), 120.0);
    let asked = ask_owner(&rig, "Delete the file x?\n\nyes / no?", Some(APP_ASKS_TITLE));
    jobs(&rig).await;
    assert!(rig.api.log.lock().buttons.last().unwrap().is_empty());
    assert!(!rig.probe().pending().0);
    asked.abort();
    rig.probe().shutdown().await;
}

#[tokio::test]
async fn test_a_relayed_questions_options_are_buttons_that_type_the_number() {
    let typed: Typed = Arc::new(Mutex::new(Vec::new()));
    let rig = relay_rig(Hooked::new(FakeApi::new(vec![])), &typed, FakeCreate::new(vec![]), 120.0);
    rig.inlet.relay_tool_call("AskUserQuestion", "Which database? (1. Postgres / 2. SQLite)");
    jobs(&rig).await;
    assert_eq!(rig.api.log.lock().buttons.last().unwrap(), &vec!["1. Postgres".to_string(), "2. SQLite".to_string()]);
    assert!(sent(&rig).last().unwrap().1.contains("Reply with the option's number."));
    let (message_id, key) = rig.api.key("2. SQLite", -1);
    tap(&rig, &key, message_id).await;
    assert!(texts_typed(&typed) == ["2"] && rig.api.log.lock().reactions.is_empty()); // a tap has no message to react to
    assert_eq!(rig.api.log.lock().answered.last().unwrap(), &("q1".to_string(), "Typed 2".to_string()));
    assert_eq!(*rig.api.log.lock().dropped.last().unwrap(), message_id);
    // a typed answer spends the buttons: a late tap types nothing
    rig.inlet.relay_tool_call("AskUserQuestion", "Ship it? (1. Yes / 2. Not yet)");
    jobs(&rig).await;
    let (message_id, key) = rig.api.key("1. Yes", -1);
    dispatch(&rig, "2", 11).await;
    jobs(&rig).await;
    assert!(texts_typed(&typed) == ["2", "2"] && rig.api.log.lock().dropped.contains(&message_id));
    tap(&rig, &key, message_id).await;
    assert!(texts_typed(&typed) == ["2", "2"] && rig.api.log.lock().answered.last().unwrap().1 == TAP_EXPIRED_LINE);
    // several questions at once are answered in turn in the terminal: no buttons
    rig.inlet.relay_tool_call("AskUserQuestion", "A? (1. x / 2. y) | B? (1. z / 2. w)");
    jobs(&rig).await;
    assert!(rig.api.log.lock().buttons.last().unwrap().is_empty());
    // the relay switched off: the option buttons mean nothing now
    rig.inlet.relay_tool_call("AskUserQuestion", "Go? (1. Yes / 2. No)");
    jobs(&rig).await;
    let (message_id, key) = rig.api.key("1. Yes", -1);
    dispatch(&rig, "claude off", 12).await;
    tap(&rig, &key, message_id).await;
    assert!(texts_typed(&typed) == ["2", "2"] && rig.api.log.lock().answered.last().unwrap().1 == TAP_EXPIRED_LINE);
    rig.probe().shutdown().await;
}

#[tokio::test]
async fn test_the_claude_on_picker_is_inline_and_a_tap_joins_exactly_as_typing_does() {
    let rig = TRig::plain(FakeApi::new(vec![]), FakeCreate::new(vec![]), config(), |o| {
        o.claude_sessions = Some(sessions(&["/r/eve", "/r/era-maker"]));
    });
    dispatch(&rig, "claude on", 1).await;
    assert_eq!(
        rig.api.log.lock().buttons.last().unwrap(),
        &vec!["eve".to_string(), "era-maker".to_string(), "New claude".to_string()]
    );
    assert!(!rig.inlet.claude());
    let (message_id, key) = rig.api.key("era-maker", -1);
    tap(&rig, &key, message_id).await;
    assert!(rig.inlet.claude() && rig.probe().relay_pin() == "/r/era-maker");
    assert_eq!(sent(&rig).last().unwrap(), &(OWNER, CLAUDE_ON_LINE.to_string()));
    assert_eq!(*rig.api.log.lock().dropped.last().unwrap(), message_id);
    assert_eq!(rig.api.log.lock().answered.last().unwrap(), &("q1".to_string(), "era-maker".to_string()));
    rig.probe().shutdown().await;
}

#[tokio::test]
async fn test_the_new_claude_tree_is_inline_and_an_old_steps_button_has_expired() {
    let root = tmp();
    for area in ["personal", "work"] {
        std::fs::create_dir_all(root.path().join(area).join("eve")).unwrap();
    }
    let opened: Arc<Mutex<Vec<(std::path::PathBuf, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let o2 = opened.clone();
    let rig = TRig::plain(FakeApi::new(vec![]), FakeCreate::new(vec![]), config(), |o| {
        o.launcher = Some(Arc::new(move |folder: std::path::PathBuf, harness: String| {
            o2.lock().push((folder, harness));
            async { Ok("Opened.".to_string()) }.boxed()
        }));
        o.launch_root = Some(root.path().to_path_buf());
        o.launch_recent = Some(Arc::new(Vec::new));
    });
    dispatch(&rig, "new claude", 1).await;
    let first = last_keyboard_id(&rig);
    let work = rig.api.key("Work", -1).1;
    let personal = rig.api.key("Personal", -1).1;
    tap(&rig, &personal, first).await;
    assert!(rig.api.log.lock().keyboards.len() == 2 && rig.probe().launch_open());
    tap(&rig, &work, first).await; // the first step's button: spent
    assert_eq!(rig.api.log.lock().answered.last().unwrap().1, TAP_EXPIRED_LINE);
    let cancel = rig.api.key("Cancel", -1).1;
    tap(&rig, &cancel, last_keyboard_id(&rig)).await; // the typed word's own path
    assert!(!rig.probe().launch_open() && opened.lock().is_empty());
    rig.probe().shutdown().await;
}

// ---- task progress in one message, with a Stop button (owner, 2026-09-23) -------------------------------------------

#[tokio::test]
async fn test_a_tasks_steps_edit_one_progress_message_and_the_result_replies_to_the_request() {
    let captions: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let c2 = captions.clone();
    let rig = task_rig(Hooked::new(FakeApi::new(vec![vec![msg("open the calculator", 31)]])), Nap::Yield, |o| {
        o.on_caption = Some(Arc::new(move |page: Value| c2.lock().push(page)));
    });
    run_rig(&rig, async {
        let progress_id = rig.api.log.lock().message_id;
        assert_eq!(sent(&rig), owner_says(&[ON_IT_LINE]));
        assert_eq!(rig.api.log.lock().buttons, vec![vec!["Stop".to_string()]]);
        let agent = rig.agents.last();
        progress(&agent, "Opening Calculator");
        progress(&agent, "Opening Calculator"); // the same step again: shown once
        progress(&agent, "Typing 2+2");
        settle(200).await;
        assert_eq!(sent(&rig).len(), 1); // still one message: edits, not sends
        assert_eq!(
            rig.api.log.lock().edits.last().unwrap(),
            &(progress_id, format!("{ON_IT_LINE}\n\n- Opening Calculator\n- Typing 2+2"))
        );
        let stop = rig.api.log.lock().edit_keyboards.last().unwrap().1.clone().expect("the Stop button");
        assert_eq!(stop[0][0].0, "Stop"); // the Stop button stays under it
        let edits = rig.api.log.lock().edits.len();
        rig.probe().task_step("Typing 2+2");
        settle(200).await;
        assert_eq!(rig.api.log.lock().edits.len(), edits); // a no-op edit is never sent
        agent.release.set();
    })
    .await;
    assert_eq!(sent(&rig), owner_says(&[ON_IT_LINE, "Calculator is open."]));
    assert_eq!(titled_last(&rig).0, some(TASK_DONE_TITLE));
    assert_eq!(rig.api.log.lock().replies, vec![("Calculator is open.".to_string(), 31)]); // a reply to the request
    let edits = rig.api.log.lock().edits.clone();
    let keyboards = rig.api.log.lock().edit_keyboards.clone();
    assert!(edits.last().unwrap().1.ends_with(PROGRESS_DONE_LINE) && keyboards.last().unwrap().1.is_none());
    let shown = captions.lock().iter().any(|c| {
        c["lines"].as_array().map(|l| l.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" ")).unwrap_or_default().contains("Opening Calculator")
    });
    assert!(shown); // the robot still acts it out
}

#[tokio::test]
async fn test_progress_edits_are_paced_to_one_a_second() {
    let rig = task_rig(Hooked::new(calculator()), Nap::Paced, |_| {});
    run_rig(&rig, async {
        rig.clock.set(0.25); // a step a quarter second after "On it"
        rig.naps.waits.lock().clear();
        let agent = rig.agents.last();
        progress(&agent, "one");
        progress(&agent, "two");
        settle(200).await;
        let waits = rig.naps.waits.lock().clone();
        assert!(!waits.is_empty() && (waits[0] - 0.75).abs() < 1e-9, "{waits:?}"); // waited out the rest of the second
        let edits: Vec<String> = rig.api.log.lock().edits.iter().map(|(_, t)| t.clone()).collect();
        assert_eq!(edits, vec![format!("{ON_IT_LINE}\n\n- one\n- two")]); // both steps, one edit
        agent.release.set();
    })
    .await;
}

#[tokio::test]
async fn test_the_stop_button_does_what_texting_stop_does() {
    let rig = task_rig(Hooked::new(calculator()), Nap::Yield, |_| {});
    run_rig(&rig, async {
        let (progress_id, key) = rig.api.key("Stop", -1);
        rig.probe().dispatch(&Value::Object(tap_update(90, progress_id, &key, STRANGER, "q1"))); // only an owner may act
        settle(200).await;
        assert!(rig.inlet.task_running() && rig.agents.last().cancel_reason.lock().is_none());
        rig.probe().dispatch(&Value::Object(tap_update(91, progress_id, &key, OWNER, "q1")));
        settle(200).await;
        assert!(!rig.inlet.task_running());
        assert_eq!(rig.agents.last().cancel_reason.lock().as_deref(), Some("stopped from Telegram"));
        assert_eq!(rig.api.log.lock().answered.last().unwrap(), &("q1".to_string(), "Stop".to_string()));
        assert!(rig.api.log.lock().dropped.contains(&progress_id));
        assert_eq!(rig.api.log.lock().edits.last().unwrap(), &(progress_id, format!("{ON_IT_LINE}\n\n{PROGRESS_STOPPED_LINE}")));
        rig.probe().dispatch(&Value::Object(tap_update(92, progress_id, &key, OWNER, "q2"))); // a second tap: spent
        settle(200).await;
        assert_eq!(rig.api.log.lock().answered.last().unwrap(), &("q2".to_string(), TAP_EXPIRED_LINE.to_string()));
    })
    .await;
    assert_eq!(sent(&rig), owner_says(&[ON_IT_LINE, STOPPED_LINE])); // as "stop" typed: no result line
}

#[tokio::test]
async fn test_a_failed_edit_falls_back_to_a_message_per_step_and_the_result_still_arrives() {
    let api = FakeApi::new(vec![vec![msg("open the calculator", 5)]]);
    *api.fail_edits.lock() = Some(cant_edit());
    api.refuse_links.store(true, Ordering::SeqCst); // and the reply link is refused too
    let rig = task_rig(Hooked::new(api), Nap::Yield, |_| {});
    run_rig(&rig, async {
        let agent = rig.agents.last();
        progress(&agent, "Opening Calculator");
        settle(200).await;
        progress(&agent, "Typing 2+2");
        settle(200).await;
        assert_eq!(sent(&rig), owner_says(&[ON_IT_LINE, "Opening Calculator", "Typing 2+2"]));
        agent.release.set();
    })
    .await;
    assert_eq!(sent(&rig).last().unwrap(), &(OWNER, "Calculator is open.".to_string())); // sent plain, never lost
    assert!(rig.api.log.lock().replies.is_empty());
    assert!(!rig.api.log.lock().dropped.is_empty()); // the Stop button still went
}

#[tokio::test]
async fn test_progress_falls_back_to_plain_messages_when_buttons_are_refused() {
    let rig = task_rig(Hooked::refusing(calculator()), Nap::Yield, |_| {});
    run_rig(&rig, async {
        let agent = rig.agents.last();
        progress(&agent, "Opening Calculator");
        settle(200).await;
        agent.release.set();
    })
    .await;
    assert_eq!(sent(&rig), owner_says(&[ON_IT_LINE, "Opening Calculator", "Calculator is open."]));
    assert!(rig.api.log.lock().edits.is_empty());
}

// Review of the progress batch (owner, 2026-09-23): when editing does not work, the steps wait for the paced flush,
// and work that ended inside that second used to drop them. A sleep that never returns holds the flush open like a
// real second, so nothing is settled between the last step and the end.

#[tokio::test]
async fn test_a_last_step_is_sent_when_the_task_ends_inside_the_second_and_buttons_were_refused() {
    let rig = task_rig(Hooked::refusing(calculator()), Nap::Hold, |_| {});
    run_rig(&rig, async {
        let agent = rig.agents.last();
        progress(&agent, "Clicked Pay");
        agent.release.set(); // ends at once: no settle between
    })
    .await;
    assert_eq!(sent(&rig), owner_says(&[ON_IT_LINE, "Clicked Pay", "Calculator is open."]));
}

#[tokio::test]
async fn test_a_last_step_is_sent_after_earlier_ones_in_degraded_mode() {
    let rig = task_rig(Hooked::refusing(calculator()), Nap::Yield, |_| {});
    run_rig(&rig, async {
        let agent = rig.agents.last();
        progress(&agent, "first");
        settle(200).await;
        rig.naps.set(Nap::Hold); // the next step waits out its second
        progress(&agent, "LAST STEP");
        agent.release.set();
    })
    .await;
    assert_eq!(sent(&rig), owner_says(&[ON_IT_LINE, "first", "LAST STEP", "Calculator is open."]));
}

#[tokio::test]
async fn test_a_last_step_is_sent_when_the_closing_edit_fails() {
    let api = calculator();
    *api.fail_edits.lock() = Some(cant_edit());
    let rig = task_rig(Hooked::new(api), Nap::Hold, |_| {});
    run_rig(&rig, async {
        let progress_id = rig.api.log.lock().message_id;
        let agent = rig.agents.last();
        progress(&agent, "Clicked Pay");
        agent.release.set();
        settle(200).await;
        assert!(rig.api.log.lock().dropped.contains(&progress_id)); // the Stop button still went
    })
    .await;
    assert_eq!(sent(&rig), owner_says(&[ON_IT_LINE, "Clicked Pay", "Calculator is open."]));
}

#[tokio::test]
async fn test_a_working_closing_edit_shows_the_last_step_and_sends_nothing_extra() {
    let rig = task_rig(Hooked::new(calculator()), Nap::Hold, |_| {});
    run_rig(&rig, async {
        let agent = rig.agents.last();
        progress(&agent, "Clicked Pay");
        agent.release.set();
    })
    .await;
    assert_eq!(sent(&rig), owner_says(&[ON_IT_LINE, "Calculator is open."]));
    assert_eq!(rig.api.log.lock().edits.last().unwrap().1, format!("{ON_IT_LINE}\n\n- Clicked Pay\n\n{PROGRESS_DONE_LINE}"));
}

#[tokio::test]
async fn test_a_flush_already_sending_steps_is_waited_for_not_cancelled() {
    let hooked = Hooked::refusing(calculator());
    let gate = Arc::new(Flag::new());
    *hooked.send_gate.lock() = Some(("Clicked Pay".to_string(), gate.clone()));
    let rig = task_rig(hooked, Nap::Yield, |_| {});
    run_rig(&rig, async {
        let agent = rig.agents.last();
        progress(&agent, "Clicked Pay");
        settle(200).await;
        agent.release.set();
        settle(200).await;
        assert_eq!(sent(&rig), owner_says(&[ON_IT_LINE])); // the close waits for the step
        gate.set();
    })
    .await;
    assert_eq!(sent(&rig), owner_says(&[ON_IT_LINE, "Clicked Pay", "Calculator is open."]));
}

#[tokio::test]
async fn test_a_step_during_a_refused_first_send_is_not_lost() {
    let hooked = Hooked::refusing(calculator());
    let gate = Arc::new(Flag::new());
    *hooked.inline_gate.lock() = Some(gate.clone());
    let rig = task_rig(hooked, Nap::Yield, |_| {});
    run_rig(&rig, async {
        let agent = rig.agents.last();
        progress(&agent, "Opening Calculator"); // while "On it" is being sent
        settle(200).await;
        gate.set();
        settle(200).await;
        agent.release.set();
    })
    .await;
    assert_eq!(sent(&rig), owner_says(&[ON_IT_LINE, "Opening Calculator", "Calculator is open."]));
}

#[tokio::test]
async fn test_stealth_hides_the_robot_not_the_progress_message() {
    let api = FakeApi::new(vec![vec![msg("stealth mode", 1), msg("open the calculator", 2)]]);
    let captions: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let c2 = captions.clone();
    let rig = task_rig(Hooked::new(api), Nap::Yield, |o| {
        o.on_caption = Some(Arc::new(move |page: Value| c2.lock().push(page)));
    });
    run_rig(&rig, async {
        assert!(rig.inlet.stealth());
        let agent = rig.agents.last();
        progress(&agent, "Opening Calculator");
        settle(200).await;
        assert!(rig.api.log.lock().edits.last().unwrap().1.ends_with("- Opening Calculator"));
        agent.release.set();
    })
    .await;
    assert!(captions.lock().is_empty());
}

#[tokio::test]
async fn test_a_free_task_question_opens_the_reply_box() {
    let email = || FakeCreate::new(vec![call("start_task", json!({"goal": "email the report to Sam"}), "call_1")]);
    let rig = TRig::plain(FakeApi::new(vec![vec![msg("email the report to Sam", 1)]]), email(), config(), |_| {});
    *rig.agents.ask.lock() = Some("Which address should I use?".into());
    *rig.agents.final_text.lock() = Some("Sent.".into());
    run_rig(&rig, async {
        assert_eq!(
            rig.api.log.lock().force_replies,
            vec![("Which address should I use?".to_string(), ASK_PLACEHOLDER.to_string())]
        );
        rig.api.feed(vec![msg("sam@example.com", 2)]);
        settle(200).await;
        rig.agents.last().release.set();
    })
    .await;
    assert_eq!(sent(&rig).last().unwrap(), &(OWNER, "Sent. (answer=sam@example.com)".to_string()));
    // a refused reply box still asks the question, plainly
    let api = FakeApi::new(vec![vec![msg("email the report to Sam", 1)]]);
    api.refuse_links.store(true, Ordering::SeqCst);
    let rig = TRig::plain(api, email(), config(), |_| {});
    *rig.agents.ask.lock() = Some("Which address should I use?".into());
    run_rig(&rig, async {
        assert!(sent(&rig).contains(&(OWNER, "Which address should I use?".to_string())));
        rig.agents.last().release.set();
    })
    .await;
}

// Python's three Codex commentary tests, on a task's progress message (the same `_Progress`).

#[tokio::test]
async fn test_a_long_step_is_kept_whole_in_the_progress_message() {
    let rig = task_rig(Hooked::new(calculator()), Nap::Yield, |_| {});
    run_rig(&rig, async {
        let (progress_id, _) = rig.api.key("Stop", -1);
        let long_step = format!("{}Then I fixed it.", "I read the failing test first. ".repeat(16)); // about 500 characters
        rig.probe().task_step(&long_step);
        settle(200).await; // the task's own job runs until it is released: no jobs() here
        assert_eq!(rig.api.log.lock().edits.last().unwrap(), &(progress_id, format!("{ON_IT_LINE}\n\n- {}", long_step.trim())));
        rig.agents.last().release.set();
    })
    .await;
}

#[tokio::test]
async fn test_a_step_too_long_for_the_message_goes_whole_as_its_own_message() {
    let rig = task_rig(Hooked::new(calculator()), Nap::Yield, |_| {});
    run_rig(&rig, async {
        let (progress_id, _) = rig.api.key("Stop", -1);
        let huge = "word ".repeat(1200); // 6000 characters: past the limit
        rig.probe().task_step(&huge);
        settle(200).await; // the task's own job runs until it is released: no jobs() here
        assert!(sent(&rig).contains(&(OWNER, huge.trim().to_string()))); // whole, as a message of its own
        assert_eq!(rig.api.log.lock().edits.last().unwrap(), &(progress_id, format!("{ON_IT_LINE}\n\n- {PROGRESS_LONG_STEP_LINE}")));
        rig.agents.last().release.set();
    })
    .await;
}

#[tokio::test]
async fn test_steps_scroll_off_near_the_limit_and_none_is_lost() {
    let rig = task_rig(Hooked::new(calculator()), Nap::Yield, |_| {});
    run_rig(&rig, async {
        let (progress_id, _) = rig.api.key("Stop", -1);
        let steps: Vec<String> = (0..20).map(|i| format!("step {i}: {}", "x".repeat(400))).collect(); // 8000 characters
        for step in &steps {
            rig.probe().task_step(step);
        }
        settle(200).await; // the task's own job runs until it is released: no jobs() here
        let (id, shown) = rig.api.log.lock().edits.last().unwrap().clone();
        assert_eq!(id, progress_id);
        assert!(eve_telegram_format::compose(&shown, None, None).chars().count() <= MAX_PROGRESS_CHARS);
        assert!(!shown.contains("step 0:") && shown.contains(&format!("- {}", steps[19]))); // the oldest scrolled off
        let said: Vec<String> = sent(&rig).into_iter().map(|(_, t)| t).collect();
        for step in &steps {
            // every step reached the phone
            assert!(said.contains(step) || shown.contains(&format!("- {step}")), "{step}");
        }
        rig.agents.last().release.set();
    })
    .await;
}

// Review of the progress batch, second pass (owner, 2026-09-23): a step that arrives while an edit is in flight is not
// in that edit's text, so a successful edit must not count it as shown. Only the steps the edit took leave `unsent`;
// the late step goes out by the next edit or, when editing fails, as a message.

/// `edit_then_step`: the first edit lets `step` arrive while it is in flight, then succeeds; later edits fail. With
/// `then_hold` the next paced edit waits out a second that never ends, so the close comes first.
fn edit_then_step(rig: &TRig, steps: Vec<String>, then_fail: bool, then_hold: bool) {
    let (probe, api, naps) = (rig.probe(), rig.api.clone(), rig.naps.clone());
    let before: Box<dyn FnOnce() + Send> = Box::new(move || {
        for step in &steps {
            probe.task_step(step);
        }
    });
    let after: Box<dyn FnOnce() + Send> = Box::new(move || {
        if then_fail {
            *api.fail_edits.lock() = Some(cant_edit());
        }
        if then_hold {
            naps.set(Nap::Hold);
        }
    });
    *rig.hooked.first_edit.lock() = Some((before, after));
}

#[tokio::test]
async fn test_a_step_during_an_in_flight_edit_is_sent_when_the_next_edit_fails() {
    let rig = task_rig(Hooked::new(calculator()), Nap::Yield, |_| {});
    edit_then_step(&rig, vec!["B CLICKED PAY".into()], true, false);
    run_rig(&rig, async {
        let agent = rig.agents.last();
        progress(&agent, "A");
        settle(200).await;
        assert_eq!(rig.api.log.lock().edits[0].1, format!("{ON_IT_LINE}\n\n- A")); // the edit in flight showed only A
        assert!(sent(&rig).contains(&(OWNER, "B CLICKED PAY".to_string()))); // the next edit failed: B went plain
        agent.release.set();
    })
    .await;
    assert_eq!(sent(&rig), owner_says(&[ON_IT_LINE, "B CLICKED PAY", "Calculator is open."]));
}

#[tokio::test]
async fn test_a_step_during_an_in_flight_edit_is_sent_when_the_closing_edit_fails() {
    let rig = task_rig(Hooked::new(calculator()), Nap::Yield, |_| {});
    edit_then_step(&rig, vec!["B CLICKED PAY".into()], true, true);
    run_rig(&rig, async {
        let agent = rig.agents.last();
        progress(&agent, "A");
        settle(200).await;
        assert_eq!(sent(&rig), owner_says(&[ON_IT_LINE])); // B waits for the paced edit
        agent.release.set(); // the close comes first, and fails
    })
    .await;
    assert_eq!(sent(&rig), owner_says(&[ON_IT_LINE, "B CLICKED PAY", "Calculator is open."]));
}

#[tokio::test]
async fn test_a_step_during_an_in_flight_edit_is_shown_by_the_next_edit_when_edits_work() {
    let rig = task_rig(Hooked::new(calculator()), Nap::Yield, |_| {});
    edit_then_step(&rig, vec!["B CLICKED PAY".into()], false, false);
    run_rig(&rig, async {
        let agent = rig.agents.last();
        progress(&agent, "A");
        settle(200).await;
        assert_eq!(rig.api.log.lock().edits.last().unwrap().1, format!("{ON_IT_LINE}\n\n- A\n- B CLICKED PAY"));
        agent.release.set();
    })
    .await;
    assert_eq!(sent(&rig), owner_says(&[ON_IT_LINE, "Calculator is open."])); // nothing extra: B was shown
}

#[tokio::test]
async fn test_a_step_scrolled_off_during_an_in_flight_edit_does_not_lose_a_later_one() {
    // Long steps arriving while the edit of A is in flight scroll A off (sent on its own). The edit's share of
    // `unsent` shrinks with it, so its success does not wipe the long step that took A's place.
    let rig = task_rig(Hooked::new(calculator()), Nap::Yield, |_| {});
    let (long1, long2) = (format!("one {}", "x".repeat(2000)), format!("two {}", "y".repeat(2000)));
    edit_then_step(&rig, vec![long1.clone(), long2.clone()], true, false);
    run_rig(&rig, async {
        let agent = rig.agents.last();
        progress(&agent, "A");
        settle(200).await;
        agent.release.set();
    })
    .await;
    let said: String = sent(&rig).into_iter().map(|(_, t)| t).collect::<Vec<_>>().join("\n");
    assert!(said.contains(&long1) && said.contains(&long2)); // both long steps reached the phone
    assert_eq!(sent(&rig).last().unwrap(), &(OWNER, "Calculator is open.".to_string()));
}
