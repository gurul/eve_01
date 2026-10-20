//! Unit `inlet_say_relay`: telegram.py 3715-4773 and 7078-7339 (inlet/say, relay, tasks); tests: test_telegram.py the terminal relay (1210-1456), inline buttons (3013-3635), task progress (3636-4214), the Codex chat (2221-2395 and its review fixes).
//!
//! The shared fakes are in `support` (architect-owned); this unit's own go here. What Python's tests read or set as
//! private attributes is reached through `TelegramInlet::say_relay_probe`. Python's FakeCodex is [`FakeCodex`], lent
//! to the inlet through the `CodexDoor` seam.

mod support;

use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use futures::FutureExt;
use parking_lot::Mutex;
use serde_json::{json, Value};

use eve_agent_contract::AgentEvent;
use eve_brain::prelude::{CodexDoor, Flag, Sessions, Sleep};
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
    /// setMessageReaction refused with this
    refuse_react: Mutex<Option<BotApiError>>,
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
            refuse_react: Mutex::new(None),
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
        if let Some(e) = self.refuse_react.lock().clone() {
            return Err(e);
        }
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
        // the shipped default: the phone never asks (owner, 2026-09-21); EVE_TELEGRAM_ASK=1 turns it on
        let quiet = TRig::plain(FakeApi::new(vec![]), FakeCreate::new(vec![]), config(), |_| {});
        quiet.probe().set_claude(true);
        assert_eq!(quiet.inlet.decide_permission("Bash", "rm -rf /", "", false).await, None);
        assert!(quiet.api.log.lock().sent.is_empty());
        assert!(!configured(Some(&std::collections::HashMap::new())).ask_permissions);
        let asked_on: std::collections::HashMap<String, String> =
            [("EVE_TELEGRAM_ASK".to_string(), "1".to_string())].into_iter().collect();
        assert!(configured(Some(&asked_on)).ask_permissions);
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

// ---- the Codex chat (FakeCodex) ------------------------------------------------------------------------------------

/// FakeCodex's attributes.
#[derive(Default)]
struct CodexState {
    connected: bool,
    selected: Vec<std::path::PathBuf>,
    sent: Vec<String>,
    stops: usize,
    closed: usize,
    emit: Option<eve_codex_chat::Emit>,
    /// the turn's result (CodexChat hands it here, not to emit, when given)
    done: Option<eve_codex_chat::Emit>,
    /// a turn is going: a message sent now steers it
    running: bool,
    fail: bool,
}

/// How a FakeCodex subclass in Python's tests changes it.
#[derive(Default, Clone, Copy, PartialEq)]
enum CodexKind {
    #[default]
    Plain,
    /// `Refuses`: interrupt raises CodexUnavailable
    Refuses,
    /// `SlowFail`: the wire takes a moment, then sends (or fails)
    SlowFail,
    /// `Chatty`: a step is emitted while the send is awaited
    Chatty,
}

#[derive(Default)]
struct FakeCodex {
    st: Mutex<CodexState>,
    attach_wait: Mutex<Option<Arc<Flag>>>,
    kind: CodexKind,
}

const CODEX_FOLDER: &str = "/projects/eve";

impl FakeCodex {
    fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn of(kind: CodexKind) -> Arc<Self> {
        Arc::new(Self { kind, ..Self::default() })
    }

    /// `await codex.emit(text)`.
    async fn emit(&self, text: &str) {
        let emit = self.st.lock().emit.clone().expect("no chat started");
        let _ = emit(text.to_string()).await;
    }

    /// `await codex.done(text)`.
    async fn done(&self, text: &str) {
        let done = self.st.lock().done.clone().expect("no chat started");
        let _ = done(text.to_string()).await;
    }

    fn sent(&self) -> Vec<String> {
        self.st.lock().sent.clone()
    }

    fn selected(&self) -> Vec<std::path::PathBuf> {
        self.st.lock().selected.clone()
    }
}

#[async_trait]
impl CodexDoor for FakeCodex {
    fn connected(&self) -> bool {
        self.st.lock().connected
    }

    fn running(&self) -> bool {
        self.st.lock().running
    }

    async fn start(
        &self,
        folder: &std::path::Path,
        emit: eve_codex_chat::Emit,
        _picture: Option<eve_codex_chat::Picture>,
        done: Option<eve_codex_chat::Emit>,
    ) -> Result<(), eve_codex_computer::CodexError> {
        {
            let mut st = self.st.lock();
            st.selected.push(folder.to_path_buf());
            st.emit = Some(emit);
            st.done = done;
        }
        let wait = self.attach_wait.lock().clone();
        if let Some(wait) = wait {
            wait.wait().await;
        }
        let mut st = self.st.lock();
        if st.fail {
            return Err(eve_codex_computer::CodexError::Unavailable("Codex unavailable. Eve is still available.".into()));
        }
        st.connected = true;
        Ok(())
    }

    async fn send(&self, text: &str) -> Result<(), eve_codex_computer::CodexError> {
        if self.kind == CodexKind::SlowFail {
            settle(5).await;
        }
        {
            let mut st = self.st.lock();
            if st.fail {
                st.connected = false;
                return Err(eve_codex_computer::CodexError::Unavailable("No confirmation; work was not retried.".into()));
            }
            st.sent.push(text.to_string());
        }
        if self.kind == CodexKind::Chatty {
            self.emit("Reading the tests").await;
        }
        Ok(())
    }

    async fn interrupt(&self) -> Result<(), eve_codex_computer::CodexError> {
        if self.kind == CodexKind::Refuses {
            return Err(eve_codex_computer::CodexError::Unavailable(
                "Codex is still starting your last message. Please wait.".into(),
            ));
        }
        self.st.lock().stops += 1;
        Ok(())
    }

    async fn close(&self) {
        let mut st = self.st.lock();
        st.connected = false;
        st.closed += 1;
    }
}

fn folders(paths: &[&str]) -> eve_brain::prelude::CodexFolders {
    let paths: Vec<std::path::PathBuf> = paths.iter().map(std::path::PathBuf::from).collect();
    Arc::new(move || paths.clone())
}

/// `Rig(api, create, codex=codex, codex_folders=lambda: [...])`.
fn codex_rig(hooked: Arc<Hooked>, create: Arc<FakeCreate>, codex: &Arc<FakeCodex>, paths: &[&str], config: TelegramConfig) -> TRig {
    let (c, f) = (codex.clone(), folders(paths));
    TRig::new(hooked, create, config, move |o| {
        o.codex = Some(c as Arc<dyn CodexDoor>);
        o.codex_folders = Some(f);
    })
}

/// `await dispatch(rig, text, uid=uid)`: from `uid`, in their own private chat.
async fn dispatch_from(rig: &TRig, text: &str, update_id: i64, uid: i64) {
    rig.probe().dispatch(&Value::Object(update(update_id, text, uid, NOW, update_id)));
    rig.probe().jobs().await;
}

fn last_edit(rig: &TRig) -> (i64, String) {
    rig.api.log.lock().edits.last().cloned().expect("no edit")
}

fn edit_of(rig: &TRig, message_id: i64) -> String {
    // `dict(api.edits)[id]`: the last edit of that message
    rig.api.log.lock().edits.iter().rev().find(|(id, _)| *id == message_id).map(|(_, t)| t.clone()).expect("no edit")
}

fn titled3(t: &(Option<String>, Option<String>, String)) -> (String, String, String) {
    (t.0.clone().unwrap_or_default(), t.1.clone().unwrap_or_default(), t.2.clone())
}

#[tokio::test]
async fn test_codex_selection_relay_escape_stop_and_off() {
    let codex = FakeCodex::new();
    let create = FakeCreate::new(vec![reply("Eve here"), reply("back to Eve")]);
    let rig = codex_rig(Hooked::new(FakeApi::new(vec![])), create.clone(), &codex, &[CODEX_FOLDER], config());
    dispatch(&rig, "codex on", 1).await;
    assert_eq!(sent(&rig), owner_says(&["eve"]));
    assert!(rig.probe().codex_chat().is_none() && codex.selected().is_empty());
    dispatch(&rig, "codex eve", 1).await;
    assert_eq!(codex.selected(), vec![std::path::PathBuf::from(CODEX_FOLDER)]);
    dispatch(&rig, "implement it", 7).await;
    assert!(codex.sent() == ["implement it"] && create.requests.lock().is_empty());
    // the turn's one progress message, with a Stop button; a step edits it, the result is a new reply
    assert_eq!(titled3(&titled_last(&rig)), ("Codex".into(), "eve".into(), "Sent to Codex.".into()));
    assert_eq!(rig.api.log.lock().buttons.last().unwrap(), &vec!["Stop".to_string()]);
    let progress_id = rig.api.log.lock().message_id;
    codex.emit("Reading the tests").await;
    settle(200).await;
    assert_eq!(last_edit(&rig), (progress_id, "Sent to Codex.\n\n- Reading the tests".to_string()));
    codex.done("Here is the answer.").await;
    settle(200).await;
    assert_eq!(titled3(&titled_last(&rig)), ("Codex".into(), "eve".into(), "Here is the answer.".into()));
    assert_eq!(rig.api.log.lock().replies.last().unwrap(), &("Here is the answer.".to_string(), 7));
    assert!(last_edit(&rig).1.ends_with(PROGRESS_DONE_LINE));
    dispatch(&rig, "eve: how are you?", 1).await;
    assert!(create.requests.lock().len() == 1 && codex.sent() == ["implement it"]);
    dispatch(&rig, "stop", 1).await;
    assert_eq!(codex.st.lock().stops, 1);
    dispatch(&rig, "codex off", 1).await;
    let count = sent(&rig).len();
    codex.emit("late output must not leak").await;
    codex.done("late result must not leak").await;
    assert!(sent(&rig).len() == count && !codex.connected());
    dispatch(&rig, "hello eve", 1).await;
    assert_eq!(create.requests.lock().len(), 2);
    rig.probe().shutdown().await;
}

#[tokio::test]
async fn test_codex_connects_by_folder_without_a_number() {
    for command in ["codex eve", "codex use eve", "codex on eve", "/codex eve"] {
        let codex = FakeCodex::new();
        let rig = codex_rig(Hooked::new(FakeApi::new(vec![])), FakeCreate::new(vec![]), &codex, &[CODEX_FOLDER], config());
        dispatch(&rig, command, 1).await;
        dispatch(&rig, command, 1).await;
        let eve = std::path::PathBuf::from(CODEX_FOLDER);
        assert_eq!(codex.selected(), vec![eve.clone(), eve], "{command}"); // each selection starts fresh
        assert_eq!(sent(&rig).last().unwrap(), &(OWNER, "New Codex chat in eve. Send your message.".to_string()));
        assert!(rig.create.requests.lock().is_empty());
        rig.probe().shutdown().await;
    }
}

#[tokio::test]
async fn test_codex_failure_returns_to_eve_without_replaying_failed_work() {
    let logs = Caplog::capture();
    let codex = FakeCodex::new();
    codex.st.lock().fail = true;
    let rig = codex_rig(Hooked::new(FakeApi::new(vec![])), FakeCreate::new(vec![reply("Hello")]), &codex, &[CODEX_FOLDER], config());
    dispatch(&rig, "codex eve", 1).await;
    assert!(!codex.connected() && rig.probe().codex_chat().is_none());
    assert!(rig.create.requests.lock().is_empty());
    assert!(logs.text().contains("codex startup failed error=CodexUnavailable"), "{}", logs.text());
    assert!(!logs.text().contains("codex eve"));
    dispatch(&rig, "Hi", 1).await;
    assert_eq!(rig.create.requests.lock().len(), 1);
    dispatch(&rig, "codex: explicit work", 1).await;
    assert!(codex.sent().is_empty() && rig.create.requests.lock().len() == 1);
    codex.st.lock().fail = false;
    dispatch(&rig, "codex eve", 1).await;
    codex.st.lock().fail = true;
    dispatch(&rig, "private work", 1).await;
    assert!(codex.sent().is_empty() && rig.probe().codex_chat().is_none());
    assert_eq!(rig.create.requests.lock().len(), 1); // no replay via Eve on send failure
    rig.probe().shutdown().await;
}

/// `accessible_folders` raising CodexUnavailable (a missing or broken saved-folders file) is answered in its own
/// words, and the Codex chat is closed, as a failed start is.
#[tokio::test]
async fn test_unreadable_saved_codex_folders_are_said_and_close_codex() {
    let codex = FakeCodex::new();
    let c = codex.clone();
    let rig = TRig::new(Hooked::new(FakeApi::new(vec![])), FakeCreate::new(vec![]), config(), move |o| {
        o.codex = Some(c as Arc<dyn CodexDoor>);
        // a lent infallible lister is overridden by the raising one
        o.codex_folders = Some(folders(&[CODEX_FOLDER]));
        o.codex_folders_result = Some(Arc::new(|| {
            Err(eve_codex_computer::CodexError::Unavailable("Could not read saved Codex folders.".into()))
        }));
    });
    for command in ["codex on", "codex eve"] {
        let closed = codex.st.lock().closed;
        dispatch(&rig, command, 1).await;
        assert_eq!(sent(&rig).last().unwrap(), &(OWNER, "Could not read saved Codex folders.".to_string()), "{command}");
        assert_eq!(codex.st.lock().closed, closed + 1, "{command}");
        assert!(codex.selected().is_empty() && rig.probe().codex_chat().is_none(), "{command}");
    }
    rig.probe().shutdown().await;
}

#[tokio::test]
async fn test_codex_owner_binding_and_claude_switch() {
    let codex = FakeCodex::new();
    let config = TelegramConfig { owner_ids: [OWNER, STRANGER].into_iter().collect(), ..config() };
    let (c, f) = (codex.clone(), folders(&[CODEX_FOLDER]));
    let rig = TRig::plain(FakeApi::new(vec![]), FakeCreate::new(vec![]), config, move |o| {
        o.codex = Some(c as Arc<dyn CodexDoor>);
        o.codex_folders = Some(f);
        o.claude_sessions = Some(sessions(&["/Users/g/repo"]));
    });
    dispatch(&rig, "codex eve", 1).await;
    dispatch_from(&rig, "codex: cannot steer", 1, STRANGER).await;
    dispatch_from(&rig, "codex off", 1, STRANGER).await;
    assert!(codex.connected() && codex.sent().is_empty());
    codex.emit("only for the original owner").await;
    assert_eq!(sent(&rig).last().unwrap(), &(OWNER, "only for the original owner".to_string()));
    dispatch(&rig, "claude on", 1).await;
    assert!(rig.inlet.claude() && !codex.connected());
    let count = sent(&rig).len();
    codex.emit("late").await;
    assert_eq!(sent(&rig).len(), count);
    rig.probe().shutdown().await;
}

#[tokio::test]
async fn test_codex_off_during_start_prevents_late_enable() {
    let codex = FakeCodex::new();
    let wait = Arc::new(Flag::new());
    *codex.attach_wait.lock() = Some(wait.clone());
    let rig = codex_rig(Hooked::new(FakeApi::new(vec![])), FakeCreate::new(vec![]), &codex, &[CODEX_FOLDER], config());
    rig.probe().dispatch(&Value::Object(msg("codex eve", 1)));
    settle(200).await;
    rig.probe().dispatch(&Value::Object(msg("codex off", 1)));
    wait.set();
    rig.probe().jobs().await;
    assert!(!codex.connected());
    assert!(!sent(&rig).iter().any(|(_, t)| t.contains("New Codex chat")));
    rig.probe().shutdown().await;
}

#[tokio::test]
async fn test_codex_invalid_selection_status_and_explicit_prefix_off() {
    let codex = FakeCodex::new();
    let rig = codex_rig(Hooked::new(FakeApi::new(vec![])), FakeCreate::new(vec![]), &codex, &[CODEX_FOLDER], config());
    dispatch(&rig, "codex use 99", 1).await;
    assert!(codex.selected().is_empty() && rig.probe().codex_chat().is_none());
    assert!(sent(&rig).iter().any(|(_, t)| t.contains("not available")));
    dispatch(&rig, "codex status", 1).await;
    assert!(sent(&rig).iter().any(|(_, t)| t.contains("No Codex chat")));
    dispatch(&rig, "codex off", 1).await;
    dispatch(&rig, "codex: no accidental Eve action", 1).await;
    assert!(rig.create.requests.lock().is_empty() && codex.sent().is_empty());
    rig.probe().shutdown().await;
}

/// `codex_relay_rig(api)`: a connected Codex chat in the owner's chat.
fn codex_relay_rig(hooked: Arc<Hooked>) -> (TRig, Arc<FakeCodex>) {
    let codex = FakeCodex::new();
    codex.st.lock().connected = true;
    let c = codex.clone();
    let rig = TRig::new(hooked, FakeCreate::new(vec![]), config(), move |o| o.codex = Some(c as Arc<dyn CodexDoor>));
    rig.probe().set_codex_chat(Some(OWNER));
    rig.probe().set_chat(Some(OWNER));
    (rig, codex)
}

#[tokio::test]
async fn test_while_a_codex_yes_no_waits_codex_relay_text_goes_to_codex() {
    // The same finding on the Codex relay: a command approval ("Reply yes or no.") ate the owner's next message
    // for Codex, and a sentence opening with "ok" approved the command.
    let (rig, codex) = codex_relay_rig(Hooked::new(FakeApi::new(vec![])));
    let asked = ask_owner(&rig, "Codex asks to run this command in /r:\nrm -rf build\n\nReply yes or no.", None);
    jobs(&rig).await;
    assert_eq!(rig.api.log.lock().buttons.last().unwrap(), &vec!["Allow".to_string(), "Deny".to_string()]);
    dispatch(&rig, "ok, also update the README", 5).await;
    assert!(codex.sent() == ["ok, also update the README"] && !asked.is_finished());
    dispatch(&rig, "no", 6).await;
    assert_eq!(asked.await.unwrap(), "no");
    assert_eq!(codex.sent(), ["ok, also update the README"]);
    rig.probe().shutdown().await;
}

#[tokio::test]
async fn test_a_codex_app_access_prompt_takes_its_button_words_typed_and_sends_other_text_to_codex() {
    let (rig, codex) = codex_relay_rig(Hooked::new(FakeApi::new(vec![])));
    let question = "Codex: Allow Calendar?\nReply: yes to allow once; \"allow for task\"; \"always allow\" to remember \
                    this app for future tasks; no to deny.";
    let asked = ask_owner(&rig, question, None);
    jobs(&rig).await;
    dispatch(&rig, "what calendar is it using?", 5).await;
    assert!(codex.sent() == ["what calendar is it using?"] && !asked.is_finished());
    dispatch(&rig, "Allow for task.", 6).await;
    assert_eq!(asked.await.unwrap(), "Allow for task.");
    jobs(&rig).await;
    assert!(last_edit(&rig).1.ends_with("Allowed for this task."));
    let (strict, words) = rig.probe().pending();
    assert!(!strict && words.is_empty());
    rig.probe().shutdown().await;
}

#[tokio::test]
async fn test_the_codex_folder_menu_is_buttons_and_a_tap_starts_that_folder() {
    let codex = FakeCodex::new();
    let rig = codex_rig(Hooked::new(FakeApi::new(vec![])), FakeCreate::new(vec![]), &codex, &[CODEX_FOLDER, "/projects/car"], config());
    dispatch(&rig, "codex on", 1).await;
    assert_eq!(sent(&rig).last().unwrap(), &(OWNER, "eve\ncar".to_string()));
    assert_eq!(rig.api.log.lock().buttons.last().unwrap(), &vec!["eve".to_string(), "car".to_string()]);
    let (_, key) = rig.api.key("car", -1);
    let board = last_keyboard_id(&rig);
    tap(&rig, &key, board).await;
    assert_eq!(codex.selected(), vec![std::path::PathBuf::from("/projects/car")]);
    assert_eq!(rig.probe().codex_chat(), Some(OWNER));
    assert!(rig.create.requests.lock().is_empty());
    rig.probe().shutdown().await;
}

#[tokio::test]
async fn test_a_codex_commentary_is_sent_when_the_turn_ends_inside_the_second_in_degraded_mode() {
    let codex = FakeCodex::new();
    let rig = codex_rig(Hooked::refusing(FakeApi::new(vec![])), FakeCreate::new(vec![]), &codex, &[CODEX_FOLDER], config());
    rig.naps.set(Nap::Hold); // held_second
    dispatch(&rig, "codex eve", 1).await;
    dispatch(&rig, "fix the tests", 3).await;
    settle(200).await;
    assert_eq!(sent(&rig).last().unwrap(), &(OWNER, "Sent to Codex.".to_string()));
    codex.emit("I fixed the flaky test.").await;
    codex.done("All tests pass.").await; // ends at once: no settle between
    settle(200).await;
    let texts = rig.api.texts();
    assert_eq!(&texts[texts.len() - 2..], &["I fixed the flaky test.".to_string(), "All tests pass.".to_string()]);
    dispatch(&rig, "codex off", 1).await;
    rig.probe().shutdown().await;
}

#[tokio::test]
async fn test_the_codex_stop_button_interrupts_and_a_steer_keeps_the_progress_message() {
    let codex = FakeCodex::new();
    let rig = codex_rig(Hooked::new(FakeApi::new(vec![])), FakeCreate::new(vec![]), &codex, &[CODEX_FOLDER], config());
    dispatch(&rig, "codex eve", 1).await;
    dispatch(&rig, "fix the tests", 3).await;
    let (progress_id, key) = rig.api.key("Stop", -1);
    codex.st.lock().running = true;
    let opened = sent(&rig).len();
    dispatch(&rig, "and the docs", 4).await; // steers the running turn
    assert!(codex.sent() == ["fix the tests", "and the docs"] && sent(&rig).len() == opened); // no line
    assert_eq!(rig.api.log.lock().reactions.last().unwrap(), &(OWNER, 4, DELIVERED_REACTION.to_string())); // a 👍
    assert_eq!(rig.api.key("Stop", -1), (progress_id, key.clone())); // no second progress message
    tap(&rig, &key, progress_id).await;
    assert!(codex.st.lock().stops == 1 && sent(&rig).last().unwrap() == &(OWNER, "Stop requested in Codex.".to_string()));
    codex.done("Codex stopped.").await;
    settle(200).await;
    assert_eq!(rig.api.log.lock().replies.last().unwrap(), &("Codex stopped.".to_string(), 3));
    // the progress message closes as stopped, not finished
    assert_eq!(last_edit(&rig), (progress_id, format!("Sent to Codex.\n\n{PROGRESS_STOPPED_LINE}")));
    dispatch(&rig, "codex off", 1).await;
    rig.probe().shutdown().await;
}

#[tokio::test]
async fn test_a_codex_steer_whose_thumb_is_refused_says_sent_to_codex() {
    let codex = FakeCodex::new();
    let hooked = Hooked::new(FakeApi::new(vec![]));
    *hooked.refuse_react.lock() = Some(api_error(400, "Bad Request: REACTION_INVALID"));
    let rig = codex_rig(hooked, FakeCreate::new(vec![]), &codex, &[CODEX_FOLDER], config());
    dispatch(&rig, "codex eve", 1).await;
    dispatch(&rig, "fix the tests", 3).await;
    let opened = sent(&rig).len(); // the progress message, not a line
    codex.st.lock().running = true;
    dispatch(&rig, "and the docs", 4).await;
    assert_eq!(codex.sent(), ["fix the tests", "and the docs"]);
    assert_eq!(sent(&rig)[opened..].to_vec(), owner_says(&[CODEX_SENT_LINE]));
    dispatch(&rig, "codex off", 1).await;
    rig.probe().shutdown().await;
}

#[tokio::test]
async fn test_codex_off_closes_an_open_progress_message() {
    let codex = FakeCodex::new();
    let rig = codex_rig(Hooked::new(FakeApi::new(vec![])), FakeCreate::new(vec![]), &codex, &[CODEX_FOLDER], config());
    dispatch(&rig, "codex eve", 1).await;
    dispatch(&rig, "fix the tests", 1).await;
    let (progress_id, key) = rig.api.key("Stop", -1);
    dispatch(&rig, "codex off", 1).await;
    assert_eq!(last_edit(&rig), (progress_id, format!("Sent to Codex.\n\n{PROGRESS_CLOSED_LINE}")));
    tap(&rig, &key, progress_id).await;
    assert!(rig.api.log.lock().answered.last().unwrap().1 == TAP_EXPIRED_LINE && codex.st.lock().stops == 0);
    rig.probe().shutdown().await;
}

#[tokio::test]
async fn test_a_tasks_stop_button_stops_the_task_even_inside_a_codex_chat() {
    // Review of the progress batch (owner, 2026-09-23): the button used to replay the word "stop", which a Codex chat
    // takes first, so the Codex turn was interrupted and the task kept running.
    let codex = FakeCodex::new();
    let rig = codex_rig(Hooked::new(FakeApi::new(vec![])), task_create(), &codex, &[CODEX_FOLDER], config());
    dispatch(&rig, "codex eve", 1).await;
    dispatch(&rig, "eve: open the calculator", 3).await;
    settle(200).await;
    assert!(rig.inlet.task_running());
    let (progress_id, key) = rig.api.key("Stop", -1);
    tap(&rig, &key, progress_id).await;
    assert_eq!(codex.st.lock().stops, 0); // the Codex chat is left alone
    let agent = rig.agents.last();
    assert_eq!(agent.cancel_reason.lock().as_deref(), Some("stopped from Telegram"));
    assert_eq!(sent(&rig).last().unwrap(), &(OWNER, STOPPED_LINE.to_string()));
    agent.release.set();
    for _ in 0..2000 {
        if !rig.inlet.task_running() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    }
    settle(200).await;
    assert!(!rig.inlet.task_running());
    assert_eq!(last_edit(&rig), (progress_id, format!("{ON_IT_LINE}\n\n{PROGRESS_STOPPED_LINE}")));
    dispatch(&rig, "codex off", 1).await;
    rig.probe().shutdown().await;
}

#[tokio::test]
async fn test_a_long_codex_commentary_is_kept_whole_in_the_progress_message() {
    let codex = FakeCodex::new();
    let rig = codex_rig(Hooked::new(FakeApi::new(vec![])), FakeCreate::new(vec![]), &codex, &[CODEX_FOLDER], config());
    dispatch(&rig, "codex eve", 1).await;
    dispatch(&rig, "fix the tests", 3).await;
    let (progress_id, _) = rig.api.key("Stop", -1);
    let long_step = format!("{}Then I fixed it.", "I read the failing test first. ".repeat(16)); // about 500 characters
    codex.emit(&long_step).await;
    jobs(&rig).await;
    assert_eq!(last_edit(&rig), (progress_id, format!("Sent to Codex.\n\n- {}", long_step.trim())));
    dispatch(&rig, "codex off", 1).await;
    rig.probe().shutdown().await;
}

#[tokio::test]
async fn test_claude_off_during_a_codex_chat_leaves_its_steps_and_result_flowing() {
    // Review finding: "claude off" bumped the Codex epoch, so the running Codex turn's steps and result were dropped
    // and its progress message kept a dead Stop button.
    let codex = FakeCodex::new();
    let rig = codex_rig(Hooked::new(FakeApi::new(vec![])), FakeCreate::new(vec![]), &codex, &[CODEX_FOLDER], config());
    dispatch(&rig, "codex eve", 1).await;
    dispatch(&rig, "implement it", 7).await;
    dispatch(&rig, "claude off", 8).await;
    codex.emit("Reading the tests").await;
    codex.done("Here is the answer.").await;
    settle(200).await;
    assert_eq!(titled3(&titled_last(&rig)), ("Codex".into(), "eve".into(), "Here is the answer.".into()));
    let (_, edited) = last_edit(&rig);
    assert!(edited.ends_with(PROGRESS_DONE_LINE) && edited.contains("Reading the tests"));
    rig.probe().shutdown().await;
}

#[tokio::test]
async fn test_a_codex_stop_that_fails_puts_the_stop_button_back() {
    // Review finding: a Stop tap retired its key before the interrupt ran; when Codex refused ("still starting"), the
    // work went on with no button left to stop it.
    let codex = FakeCodex::of(CodexKind::Refuses);
    let rig = codex_rig(Hooked::new(FakeApi::new(vec![])), FakeCreate::new(vec![]), &codex, &[CODEX_FOLDER], config());
    dispatch(&rig, "codex eve", 1).await;
    dispatch(&rig, "implement it", 7).await;
    let (progress_id, key) = rig.api.key("Stop", -1);
    tap(&rig, &key, progress_id).await;
    settle(200).await;
    assert_eq!(last_edit(&rig).0, progress_id);
    let rows = rig.api.log.lock().edit_keyboards.last().cloned().unwrap().1.expect("buttons back");
    assert!(rows[0][0].0 == "Stop" && rows[0][0].1 != key); // back, under a fresh key
    rig.probe().shutdown().await;
}

#[tokio::test]
async fn test_a_codex_send_that_fails_closes_its_progress_message() {
    // Review finding: a disconnect left the turn's progress message open with a live Stop button.
    let codex = FakeCodex::of(CodexKind::SlowFail);
    let rig = codex_rig(Hooked::new(FakeApi::new(vec![])), FakeCreate::new(vec![]), &codex, &[CODEX_FOLDER], config());
    dispatch(&rig, "codex eve", 1).await;
    dispatch(&rig, "implement it", 7).await;
    let first = rig.api.log.lock().message_id;
    codex.st.lock().fail = true;
    dispatch(&rig, "and the docs", 8).await;
    let second = rig.api.log.lock().message_id;
    assert!(edit_of(&rig, first).ends_with(PROGRESS_CLOSED_LINE));
    assert!(edit_of(&rig, second).ends_with(CODEX_NOT_SENT_LINE));
    assert!(rig.probe().codex_chat().is_none() && rig.probe().codex_progress_shown().is_none());
    rig.probe().shutdown().await;
}

#[tokio::test]
async fn test_a_codex_disconnect_noticed_at_the_next_message_closes_the_open_progress() {
    let codex = FakeCodex::new();
    let rig = codex_rig(Hooked::new(FakeApi::new(vec![])), FakeCreate::new(vec![]), &codex, &[CODEX_FOLDER], config());
    dispatch(&rig, "codex eve", 1).await;
    dispatch(&rig, "implement it", 7).await;
    let progress_id = rig.api.log.lock().message_id;
    codex.st.lock().connected = false; // the app-server went away
    dispatch(&rig, "and the docs", 8).await;
    assert!(edit_of(&rig, progress_id).ends_with(PROGRESS_CLOSED_LINE));
    rig.probe().shutdown().await;
}

#[tokio::test]
async fn test_codex_commentary_emitted_while_the_send_is_awaited_is_a_step_of_its_turn() {
    // Review finding: the progress message was opened only after send returned, so a step Codex emitted meanwhile
    // went as a message of its own.
    let codex = FakeCodex::of(CodexKind::Chatty);
    let rig = codex_rig(Hooked::new(FakeApi::new(vec![])), FakeCreate::new(vec![]), &codex, &[CODEX_FOLDER], config());
    dispatch(&rig, "codex eve", 1).await;
    dispatch(&rig, "implement it", 7).await;
    settle(200).await;
    let own = (some("Codex"), some("eve"), "Reading the tests".to_string());
    assert!(!rig.api.log.lock().titled.contains(&own)); // not a message of its own
    assert_eq!(rig.probe().codex_progress_shown().as_deref(), Some("Sent to Codex.\n\n- Reading the tests"));
    rig.probe().shutdown().await;
}

#[tokio::test]
async fn test_two_codex_folders_with_one_name_get_buttons_that_can_be_told_apart() {
    let codex = FakeCodex::new();
    let home = dirs::home_dir().expect("a home");
    let (work, personal) = (home.join("work").join("eve"), home.join("personal").join("eve"));
    let paths = [work.to_string_lossy().into_owned(), personal.to_string_lossy().into_owned(), "/projects/status".into()];
    let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
    let rig = codex_rig(Hooked::new(FakeApi::new(vec![])), FakeCreate::new(vec![]), &codex, &refs, config());
    dispatch(&rig, "codex on", 1).await;
    assert_eq!(
        rig.api.log.lock().buttons.last().unwrap(),
        &vec!["~/work/eve".to_string(), "~/personal/eve".to_string(), "status".to_string()]
    );
    let (_, key) = rig.api.key("status", -1);
    let board = last_keyboard_id(&rig);
    tap(&rig, &key, board).await; // a folder, not the "status" verb
    assert_eq!(codex.selected(), vec![std::path::PathBuf::from("/projects/status")]);
    rig.probe().shutdown().await;
}

/// `_opening_rig(tmp_path, **kw)`: a launcher that says when the terminal is opening and waits to be released.
struct Opening {
    rig: TRig,
    folder: std::path::PathBuf,
    opened: Arc<Flag>,
    release: Arc<Flag>,
    _dir: tempfile::TempDir,
}

fn opening_rig(codex: Option<Arc<FakeCodex>>) -> Opening {
    let dir = tmp();
    let folder = dir.path().join("repo");
    std::fs::create_dir(&folder).unwrap();
    let (opened, release) = (Arc::new(Flag::new()), Arc::new(Flag::new()));
    let (o2, r2) = (opened.clone(), release.clone());
    let launcher: eve_brain::prelude::Launcher = Arc::new(move |_where, _harness| {
        let (o, r) = (o2.clone(), r2.clone());
        async move {
            o.set();
            r.wait().await;
            Ok("Opened.".to_string())
        }
        .boxed()
    });
    let repo = folder.to_string_lossy().into_owned();
    let rig = TRig::plain(FakeApi::new(vec![]), FakeCreate::new(vec![]), config(), move |o| {
        o.launcher = Some(launcher);
        if let Some(codex) = codex {
            o.codex = Some(codex as Arc<dyn CodexDoor>);
            o.codex_folders = Some(folders(&[&repo]));
        }
    });
    Opening { rig, folder, opened, release, _dir: dir }
}

fn opening_step(folder: &std::path::Path) -> eve_claude_launch::Step {
    eve_claude_launch::Step { folder: Some(folder.to_path_buf()), harness: Some("claude".into()), done: true, ..eve_claude_launch::Step::new("Opening.") }
}

#[tokio::test]
async fn test_a_codex_folder_while_a_new_claude_opens_cancels_its_late_join() {
    // Found 2026-10-06: `_launch_step` took the "join what opens" flag, awaited the terminal for seconds, then joined
    // without looking again. `codex <folder>` in that window left Claude and Codex both on.
    let codex = FakeCodex::new();
    let o = opening_rig(Some(codex.clone()));
    o.rig.probe().set_join_after_launch(true);
    let (probe, step) = (o.rig.probe(), opening_step(&o.folder));
    let launching = tokio::spawn(async move { probe.launch_step(OWNER, &step).await });
    o.opened.wait().await;
    dispatch(&o.rig, &format!("codex use {}", o.folder.display()), 1).await;
    o.release.set();
    launching.await.unwrap();
    o.rig.probe().jobs().await;
    assert!(o.rig.probe().codex_chat() == Some(OWNER) && !o.rig.inlet.claude());
}

#[tokio::test]
async fn test_claude_off_while_a_new_claude_opens_cancels_its_late_join() {
    // The same window with "claude off": the relay stayed off only until the terminal opened.
    let o = opening_rig(None);
    o.rig.probe().set_join_after_launch(true);
    let (probe, step) = (o.rig.probe(), opening_step(&o.folder));
    let launching = tokio::spawn(async move { probe.launch_step(OWNER, &step).await });
    o.opened.wait().await;
    dispatch(&o.rig, "claude off", 1).await;
    o.release.set();
    launching.await.unwrap();
    assert!(!o.rig.inlet.claude() && sent(&o.rig).last().unwrap() == &(OWNER, "Opened.".to_string()));
}

#[tokio::test]
async fn test_with_no_switch_the_late_join_still_happens() {
    // The control for the two above.
    let o = opening_rig(None);
    o.release.set();
    o.rig.probe().set_join_after_launch(true);
    o.rig.probe().launch_step(OWNER, &opening_step(&o.folder)).await;
    assert!(o.rig.inlet.claude() && titled_last(&o.rig).0 == some(CLAUDE_ON_TITLE));
}

#[tokio::test]
async fn test_the_first_message_to_a_new_claude_waits_until_the_session_has_started() {
    // 2026-10-10: "new claude" joined as soon as the terminal opened, and the first message was typed a second before
    // Claude Code's SessionStart, so it was lost. Typing now waits for that hook from the session's folder.
    let dir = tmp();
    let folder = dir.path().join("repo");
    std::fs::create_dir(&folder).unwrap();
    let typed: Typed = Arc::new(Mutex::new(Vec::new()));
    let launcher: eve_brain::prelude::Launcher = Arc::new(|_where, _harness| async { Ok("Opened.".to_string()) }.boxed());
    let t = typed.clone();
    let rig = TRig::plain(FakeApi::new(vec![]), FakeCreate::new(vec![]), config(), move |o| {
        o.launcher = Some(launcher);
        o.terminal = Some(terminal(&t));
    });
    rig.probe().set_join_after_launch(true);
    rig.probe().launch_step(OWNER, &opening_step(&folder)).await;
    assert!(rig.inlet.claude());
    let probe = rig.probe();
    let typing = tokio::spawn(async move { probe.type_to_claude(OWNER, "hello claude", 7).await });
    settle(200).await;
    assert!(typed.lock().is_empty()); // still starting: nothing typed yet
    rig.inlet.relay_session_started(&dir.path().join("elsewhere").to_string_lossy()); // another folder's session
    settle(200).await;
    assert!(typed.lock().is_empty());
    rig.inlet.relay_session_started(&folder.to_string_lossy());
    assert!(typing.await.unwrap());
    assert_eq!(texts_typed(&typed), ["hello claude"]);
    assert!(rig.probe().type_to_claude(OWNER, "and again", 8).await); // started: no wait after the first
    assert_eq!(texts_typed(&typed), ["hello claude", "and again"]);
}

#[tokio::test]
async fn test_a_session_already_running_is_typed_into_at_once() {
    // The control: "claude on" joining a running session never waits for a SessionStart that already happened.
    let typed: Typed = Arc::new(Mutex::new(Vec::new()));
    let t = typed.clone();
    let rig = TRig::plain(FakeApi::new(vec![]), FakeCreate::new(vec![]), config(), move |o| o.terminal = Some(terminal(&t)));
    rig.probe().relay_to("/Users/g/repo");
    let probe = rig.probe();
    let typing = tokio::time::timeout(std::time::Duration::from_secs(1), probe.type_to_claude(OWNER, "run it", 3));
    assert!(typing.await.expect("no wait"));
    assert_eq!(texts_typed(&typed), ["run it"]);
}

// ---- test_relay_questions.py: a restart during a texted task ------------------------------------------------------

#[tokio::test]
async fn test_a_restart_during_a_texted_task_tells_the_owner() {
    // the goal is the one the model's start_task names (task_create): "open the calculator"
    let rig = task_rig(Hooked::new(calculator()), Nap::Yield, |_| {});
    let inlet = rig.inlet.clone();
    let looping = tokio::spawn(async move { inlet.run().await });
    settle(200).await;
    assert!(rig.inlet.task_running()); // never released: still running when the daemon stops
    let before = sent(&rig).len();
    rig.probe().shutdown().await;
    assert_eq!(rig.agents.last().cancel_reason.lock().as_deref(), Some("the daemon is stopping"));
    let after: Vec<String> = sent(&rig)[before..].iter().map(|(_, t)| t.clone()).collect();
    assert_eq!(after, vec![RESTART_LINE.replace("{goal}", "open the calculator")]);
    assert_eq!(sent(&rig).last().unwrap().0, OWNER);
    looping.abort();
    let _ = looping.await;
}
