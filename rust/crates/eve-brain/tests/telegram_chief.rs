//! test_telegram_chief.py: the chief of staff wired into the Telegram door and the self-context block (design P4,
//! 2026-09-29), ported with its names and assertions. Its off-only tests (no Chief) are in inlet_tools.rs.
//!
//! The Rig is the Python one: test_telegram's fakes, and a real Chief on a temporary ledger built from the inlet's
//! own seams exactly as the daemon builds it (eve-daemon's `make_chief`: chief_start, chief_stop, chief_mac_free,
//! chief_ask, chief_notify, each through a weak reference to the inlet), its clock at 10:00 on 2026-09-29, outside
//! quiet hours. The agent's UI states are its `status()["ui_evidence"]` (the TaskAgent seam's carrier for Python's
//! `agent.ui_evidence`). Python's `history()` added the turns of a chat that closed meanwhile (`close_and_keep`):
//! these tests never advance the inlet's clock, so the one close is the poll loop's shutdown, and `run` keeps the
//! turns just before it. Where Python swapped
//! `inlet._agent_factory` or `chief._research` after building the rig, the rig is built with them.

mod more;
mod support;

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::TimeZone;
use futures::FutureExt;
use parking_lot::Mutex;
use serde_json::{json, Map, Value};

use eve_agent_contract::AgentEvent;
use eve_brain::prelude::{AgentFactory, AskUser, Flag, OnEvent, TaskAgent};
use eve_brain::seams::{in_ctx, Reader, TurnCtx};
use eve_brain::self_context::{self, Lent, Router};
use eve_brain::telegram::{
    Inbound, TelegramConfig, BOT_COMMANDS, CALL_ASK_AGAIN, CHIEF_TITLE, FAILED_LINE, JOBS_MENU, TASK_DONE_TITLE,
};
use eve_chief::{Chief, ChiefOptions, Raised, StartArgs};
use more::*;
use support::*;

// ---- test_chief.py's jobs ---------------------------------------------------------------------------------------

const LINKS: [&str; 4] = [
    "https://desks.example.com/a",
    "https://desks.example.com/b",
    "https://shop.example.org/c",
    "https://review.example.net/d",
];
const SAID: &str = "find me a standing desk under $600 and order it";
const READ_SAID: &str = "compare a few standing desks under $600 for me";
const ACT_SAID: &str = "order the desk in the open tab";
const CHANGE_DROP: &str = "Reply \"change c1\" or \"drop c1\".";

fn desk_job() -> Value {
    json!({
        "kind": "job", "title": "Standing desk", "purpose": "a desk to stand at",
        "end_state": "an order confirmation for one desk under $600",
        "done_checks": [{"kind": "read_links", "arg": "3"}, {"kind": "cited_pick", "arg": null},
                        {"kind": "ui_seen", "arg": "order confirmed"}],
        "never": ["over $600"],
        "phases": [{"do": "research", "goal": "standing desks under $600 with reviews", "door": null},
                   {"do": "assess", "goal": "pick one", "door": null},
                   {"do": "act", "goal": "order the picked desk", "door": null}],
        "deadline": null, "cue": null, "firm": null, "question": null})
}

/// `{**base, **over}`.
fn with(base: Value, over: Value) -> Value {
    let mut out = base;
    for (k, v) in over.as_object().unwrap() {
        out[k] = v.clone();
    }
    out
}

fn read_job() -> Value {
    let phases: Vec<Value> = desk_job()["phases"].as_array().unwrap()[..2].to_vec();
    with(
        desk_job(),
        json!({"title": "Desk shortlist", "end_state": "a pick with a page I read",
               "done_checks": [{"kind": "read_links", "arg": "3"}, {"kind": "cited_pick", "arg": null}],
               "never": [], "phases": phases}),
    )
}

/// One step, one-way, from the owner's own words: the act is the first queued step, so "go c<N>" can approve it.
fn act_job() -> Value {
    with(
        desk_job(),
        json!({"title": "Order the desk", "done_checks": [{"kind": "ui_seen", "arg": "order confirmed"}],
               "phases": [{"do": "act", "goal": "order the desk in the open tab", "door": null}]}),
    )
}

/// Research that the router sends to the Mac: a chief Mac step that is not an act.
fn mac_job() -> Value {
    let phases: Vec<Value> = read_job()["phases"].as_array().unwrap()[..1].to_vec();
    with(read_job(), json!({"done_checks": [{"kind": "guru_says_done", "arg": null}], "phases": phases}))
}

fn ordered() -> Vec<Value> {
    vec![json!({"call_id": "u", "state": "Window: Shop\nOrder confirmed, thank you"})]
}

/// `datetime(2026, 9, 29, hh).timestamp()`.
fn at(hh: u32) -> f64 {
    chrono::Local.with_ymd_and_hms(2026, 9, 29, hh, 0, 0).earliest().unwrap().timestamp() as f64
}

fn sheet_reply() -> Value {
    let body = json!({"options": [{"name": "Desk A", "price": 549, "why": "sturdy, quiet motor", "link": LINKS[0]},
                                  {"name": "Desk B", "price": 499, "why": "cheaper", "link": LINKS[1]}],
                      "pick": 0, "change": "a sale on Desk B"});
    json!({"output": [{"type": "message", "content": [{"type": "output_text", "text": body.to_string()}]}],
           "usage": {"input_tokens": 1000, "output_tokens": 200}})
}

// ---- test_telegram's model replies --------------------------------------------------------------------------------

fn say(text: &str) -> Value {
    json!({"id": "resp", "output": [{"type": "message", "role": "assistant",
                                     "content": [{"type": "output_text", "text": text}]}]})
}

fn pcall(name: &str, args: Value) -> Value {
    json!({"id": "resp", "output": [{"type": "reasoning", "id": "rs_1", "encrypted_content": "opaque"},
                                    {"type": "function_call", "name": name, "call_id": "call_1",
                                     "arguments": serde_json::to_string(&args).unwrap()}]})
}

fn inbound(text: &str, message_id: i64) -> Inbound {
    Inbound { message_id, ..Inbound::new(OWNER, OWNER, text) }
}

// ---- the agent --------------------------------------------------------------------------------------------------

/// A computer agent that finishes as soon as it is released (at once unless `hold`), with the UI states a real
/// Codex run would have captured. `failing`: Codex as it fails, an error event, then its failure sentence returned.
struct Agent {
    on_event: OnEvent,
    _ask_user: AskUser,
    running: AtomicBool,
    goal: Mutex<String>,
    cancel_reason: Mutex<Option<String>>,
    release: Flag,
    evidence: Vec<Value>,
    failing: bool,
    /// `agent.steer = ...`
    steer_with: Mutex<Option<Arc<dyn Fn(&str) -> bool + Send + Sync>>>,
}

#[async_trait]
impl TaskAgent for Agent {
    async fn run(&self, goal: &str, _note: &str) -> anyhow::Result<String> {
        self.running.store(true, Ordering::SeqCst);
        *self.goal.lock() = goal.to_string();
        (self.on_event)(&AgentEvent::with_text("started", goal));
        if self.failing {
            (self.on_event)(&AgentEvent::with_text("error", "Codex could not start or timed out."));
            self.running.store(false, Ordering::SeqCst);
            return Ok("Codex could not start or timed out. No alternate UI automation was used.".into());
        }
        (self.on_event)(&AgentEvent::with_text("progress", "Clicking Place order"));
        self.release.wait().await;
        self.running.store(false, Ordering::SeqCst);
        Ok("Done: the order went through.".into())
    }

    fn cancel(&self, reason: &str) {
        *self.cancel_reason.lock() = Some(reason.to_string());
        self.release.set();
    }

    fn steer(&self, text: &str) -> bool {
        let hook = self.steer_with.lock().clone();
        match hook {
            Some(hook) => hook(text),
            None => false,
        }
    }

    fn running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    fn status(&self) -> Value {
        json!({"ui_evidence": self.evidence})
    }
}

// ---- the rig ------------------------------------------------------------------------------------------------------

/// Rig's keyword arguments.
struct Kw {
    with_chief: bool,
    route: &'static str,
    hold: bool,
    evidence: Vec<Value>,
    busy: Option<Arc<AtomicBool>>,
    memory: Option<Arc<FakeMemory>>,
    app_only: bool,
    failing: bool,
    /// the research result's sources (Python's `rig.chief._research = ...`)
    sources: Option<Vec<Value>>,
}

impl Default for Kw {
    fn default() -> Self {
        Kw {
            with_chief: false,
            route: "web",
            hold: false,
            evidence: Vec::new(),
            busy: None,
            memory: None,
            app_only: false,
            failing: false,
            sources: None,
        }
    }
}

type Agents = Arc<Mutex<Vec<Arc<Agent>>>>;

struct ChiefRig {
    rig: Rig,
    api: Arc<FakeApi>,
    create: Arc<FakeCreate>,
    agents: Agents,
    floors: Arc<Mutex<Vec<Option<String>>>>,
    /// think_hard's questions (Python's `rig.thought`; no test here with the chief on reaches it)
    _thought: Arc<Mutex<Vec<String>>>,
    research: Arc<Mutex<Vec<String>>>,
    options: Arc<Mutex<Vec<Value>>>,
    chief: Option<Arc<Chief>>,
    /// the turns of the chat the loop's shutdown closed
    closed: Mutex<Vec<(String, String)>>,
    _dir: tempfile::TempDir,
}

impl ChiefRig {
    fn new(api: Arc<FakeApi>, create: Arc<FakeCreate>, kw: Kw) -> Self {
        let dir = tmp();
        let agents: Agents = Arc::default();
        let floors: Arc<Mutex<Vec<Option<String>>>> = Arc::default();
        let thought: Arc<Mutex<Vec<String>>> = Arc::default();
        let factory: AgentFactory = {
            let (agents, floors) = (agents.clone(), floors.clone());
            let (hold, evidence, failing) = (kw.hold, kw.evidence.clone(), kw.failing);
            Arc::new(move |on_event: OnEvent, ask_user: AskUser, floor: Option<String>| {
                floors.lock().push(floor);
                let agent = Arc::new(Agent {
                    on_event,
                    _ask_user: ask_user,
                    running: AtomicBool::new(false),
                    goal: Mutex::new(String::new()),
                    cancel_reason: Mutex::new(None),
                    release: Flag::new(),
                    evidence: evidence.clone(),
                    failing,
                    steer_with: Mutex::new(None),
                });
                if !hold {
                    agent.release.set();
                }
                agents.lock().push(agent.clone());
                agent as Arc<dyn TaskAgent>
            })
        };
        let thinker: eve_brain::seams::Thinker = {
            let t = thought.clone();
            Arc::new(move |question: String, _how: String| {
                t.lock().push(question);
                async { Ok(obj(json!({"ok": true, "answer": "thought"}))) }.boxed()
            })
        };
        let config = TelegramConfig { app_only: kw.app_only, ..config() };
        let busy = kw.busy.clone();
        let memory = kw.memory.clone();
        let rig = Rig::new(api.clone(), create.clone(), config, move |o| {
            o.agent_factory = Some(factory);
            o.thinker = Some(thinker);
            if let Some(busy) = busy {
                o.busy = Some(Arc::new(move || busy.load(Ordering::SeqCst)));
            }
            if let Some(memory) = memory {
                o.memory = Some(memory as Arc<dyn eve_brain::prelude::BrainMemory>);
            }
        });
        let research: Arc<Mutex<Vec<String>>> = Arc::default();
        let options: Arc<Mutex<Vec<Value>>> = Arc::default();
        let chief = kw.with_chief.then(|| make_chief(&rig, dir.path(), &kw, &research, &options));
        if let Some(chief) = &chief {
            *rig.inlet.chief.lock() = Some(chief.clone());
        }
        ChiefRig { rig, api, create, agents, floors, _thought: thought, research, options, chief, closed: Mutex::default(), _dir: dir }
    }

    fn chief(&self) -> &Arc<Chief> {
        self.chief.as_ref().expect("no chief")
    }

    fn card(&self, cid: &str) -> eve_chief::Card {
        self.chief().ledger.get(cid).expect("no such card")
    }

    fn history(&self) -> Vec<(String, String)> {
        let mut out = self.closed.lock().clone();
        out.extend(self.rig.inlet.turns());
        out
    }

    fn agent(&self, i: usize) -> Arc<Agent> {
        self.agents.lock()[i].clone()
    }

    fn agent_count(&self) -> usize {
        self.agents.lock().len()
    }

    async fn take(&self, args: Value, said: &str) -> String {
        let out = self.chief().handle("take_on", &args, said, "2026-09-29 10:00:00", "telegram").await.unwrap();
        assert_eq!(out["ok"], true, "{out}");
        out["id"].as_str().unwrap().to_string()
    }

    async fn tick(&self) {
        self.chief().tick().await.unwrap();
    }

    fn feed(&self, text: &str, update_id: i64) {
        self.api.feed(vec![said(text, update_id)]);
    }

    fn texts(&self) -> Vec<String> {
        self.api.texts()
    }

    fn has(&self, who: &str, text: &str) -> bool {
        self.history().iter().any(|(w, t)| w == who && t == text)
    }
}

/// The Chief as the daemon builds it (Daemon._make_chief), on a temporary ledger.
fn make_chief(rig: &Rig, tmp: &Path, kw: &Kw, research: &Arc<Mutex<Vec<String>>>, options: &Arc<Mutex<Vec<Value>>>) -> Arc<Chief> {
    let at10 = at(10);
    let clock = move || at10;
    let folder = tmp.join("chief");
    let me = Arc::downgrade(&rig.inlet);
    let gone = || Raised::new("RuntimeError", "the chat has stopped");
    let raised = |e: anyhow::Error| match e.downcast_ref::<Raised>() {
        Some(r) => r.clone(),
        None => Raised::new("Exception", e.to_string()),
    };
    let start_task: eve_chief::StartTask = {
        let me = me.clone();
        Arc::new(move |args: StartArgs| {
            let me = me.clone();
            async move {
                let inlet = me.upgrade().ok_or_else(gone)?;
                Ok(Value::Object(inlet.chief_start(&args.goal, &args.card_id, args.n as i64, args.floor.as_deref())))
            }
            .boxed()
        })
    };
    let stop_task: eve_chief::StopTask = {
        let me = me.clone();
        Arc::new(move |card_id: String, n: usize| {
            let me = me.clone();
            async move {
                let inlet = me.upgrade().ok_or_else(gone)?;
                inlet.chief_stop(&card_id, n as i64);
                Ok(())
            }
            .boxed()
        })
    };
    let mac_free: eve_chief::MacFree = {
        let me = me.clone();
        Arc::new(move || Ok(me.upgrade().ok_or_else(gone)?.chief_mac_free()))
    };
    let ask: eve_chief::Ask = {
        let me = me.clone();
        Arc::new(move |text: String| {
            let me = me.clone();
            async move {
                let inlet = me.upgrade().ok_or_else(gone)?;
                inlet.chief_ask(&text).await.map_err(raised)
            }
            .boxed()
        })
    };
    let notify: eve_chief::Notify = {
        let me = me.clone();
        Arc::new(move |text: String, about: String| {
            let me = me.clone();
            async move {
                let inlet = me.upgrade().ok_or_else(gone)?;
                Ok(inlet.chief_notify(&text, &about).await)
            }
            .boxed()
        })
    };
    // the options call: test_telegram's FakeCreate with three sheet replies
    let create: eve_chief::Create = {
        let options = options.clone();
        let replies = Arc::new(Mutex::new(VecDeque::from(vec![sheet_reply(), sheet_reply(), sheet_reply()])));
        Arc::new(move |body: Value| {
            options.lock().push(body);
            let next = replies.lock().pop_front();
            async move { next.ok_or_else(|| Raised::new("AssertionError", "a model call nobody scripted")) }.boxed()
        })
    };
    let research_fn: eve_chief::Research = {
        let research = research.clone();
        let sources = kw.sources.clone();
        Arc::new(move |query: String| {
            research.lock().push(query);
            let out = match &sources {
                Some(sources) => json!({"ok": true, "answer": "Desk A $549", "sources": sources}),
                None => json!({"ok": true, "answer": "Desk A is $549.", "sources": [], "cost_usd": 0.01}),
            };
            async move { Ok(out) }.boxed()
        })
    };
    let route = kw.route.to_string();
    let spend = eve_spend::Ledger::at(folder.join("spend"));
    let ledger = eve_chief_ledger::Ledger::with_clock(Some(&folder), Arc::new(clock));
    let desk = eve_chief_desk::Desk::new(
        Some(&folder),
        eve_chief_desk::DeskOptions {
            mode: Some("shadow".into()),
            clock: Some(Arc::new(clock)),
            environ: Some(HashMap::new()),
            ..eve_chief_desk::DeskOptions::default()
        },
    );
    let memory = eve_chief_reflect::Memory::new(Some(&folder), Some(Arc::new(clock)));
    let reflector = eve_chief_reflect::Reflector::new(
        Some(memory),
        eve_chief_reflect::ReflectorOptions {
            ask: Some(Arc::new(|_body: &Value| Ok(json!({})))),
            clock: Arc::new(clock),
            offload: false,
            spend: spend.clone(),
            ..eve_chief_reflect::ReflectorOptions::default()
        },
    );
    Chief::new(ChiefOptions {
        ledger: Some(ledger),
        desk: Some(desk),
        reflector: Some(Arc::new(reflector)),
        research: Some(research_fn),
        route_research: Some(Arc::new(move |_goal: &str| Ok(route.clone()))),
        start_task: Some(start_task),
        stop_task: Some(stop_task),
        mac_free: Some(mac_free),
        create: Some(create),
        ask: Some(ask),
        notify: Some(notify),
        clock: Some(Arc::new(clock)),
        offload: false,
        home: Some(tmp.to_path_buf()),
        // replanning: test_chief_replan.py
        environ: Some(HashMap::from([("EVE_CHIEF_REPLAN".to_string(), "off".to_string())])),
        spend,
        ..ChiefOptions::default()
    })
}

async fn run<F, Fut>(r: &ChiefRig, during: F)
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    run_rig(&r.rig, || async {
        during().await;
        settle(200).await;
        // the history, as it is when the loop's shutdown closes the chat (Python's close_and_keep)
        r.closed.lock().extend(r.rig.inlet.turns());
    })
    .await;
}

fn tool_names(body: &Value) -> Vec<String> {
    body["tools"].as_array().unwrap().iter().filter_map(|t| t.get("name").and_then(Value::as_str).map(str::to_string)).collect()
}

/// (text, buttons) of every message sent.
fn sent_with_buttons(api: &FakeApi) -> Vec<(String, Vec<String>)> {
    let log = api.log.lock();
    assert_eq!(log.sent.len(), log.buttons.len());
    log.sent.iter().zip(log.buttons.iter()).map(|((_, t), b)| (t.clone(), b.clone())).collect()
}

fn last_title(api: &FakeApi) -> Option<String> {
    api.log.lock().titled.last().and_then(|t| t.0.clone())
}

fn events(r: &ChiefRig, cid: &str) -> Vec<Map<String, Value>> {
    r.chief().ledger.events(Some(cid))
}

/// eve's lines as the transcript keeps them: what Eve Link's chat shows (link_chat.read_messages).
fn eve_rows(memory: &FakeMemory) -> Vec<String> {
    let (rows, _) = eve_link::link_chat::read_messages(&memory.transcripts, None).unwrap();
    rows.iter().filter(|r| r["role"] == "eve").map(|r| r["text"].as_str().unwrap_or_default().to_string()).collect()
}

fn tap(r: &ChiefRig, label: &str, update_id: i64) {
    let (message_id, key) = r.api.key(label, -1);
    r.api.feed(vec![tap_update(update_id, message_id, &key, OWNER, "q1")]);
}

// ---- on: take_on, the code words, the turn line -----------------------------------------------------------------

#[tokio::test]
async fn test_take_on_makes_no_second_model_call_and_its_backbrief_has_change_and_drop() {
    let _iso = isolated();
    let r = ChiefRig::new(
        FakeApi::new(vec![vec![said(SAID, 1)]]),
        FakeCreate::new(vec![pcall("take_on", desk_job())]),
        Kw { with_chief: true, ..Kw::default() },
    );
    run(&r, || async {}).await;
    assert_eq!(r.create.requests.lock().len(), 1); // the fake creator counts every call
    let body = r.create.requests.lock()[0].clone();
    let names = tool_names(&body);
    assert!(names.contains(&"take_on".to_string()) && names.contains(&"jobs_list".to_string()));
    assert!(body["instructions"].as_str().unwrap().contains(&eve_chief::instructions(&eve_chief::LIVE)));
    let card = r.card("c1");
    let phase = card.phase(3).unwrap();
    assert!(phase.door == "one_way" && phase.grounded); // "order": his words
    assert!(card.source.said_ref == "2026-09-29 10:00:00" || card.source.said_ref.starts_with("20"));
    let sent = sent_with_buttons(&r.api);
    assert_eq!(sent.len(), 1);
    let (text, buttons) = sent[0].clone();
    assert!(text.starts_with("On it: Standing desk.") && buttons == ["Change", "Drop"], "{text} {buttons:?}");
    assert_eq!(r.api.log.lock().titled[0].0.as_deref(), Some(CHIEF_TITLE));
    assert!(r.has("eve", &text)); // the history knows what eve took on
    assert!(r.research.lock().is_empty() && r.agent_count() == 0); // the steps run on the chief's loop, not the turn
}

#[tokio::test]
async fn test_an_app_turns_backbrief_reaches_the_app_with_change_and_drop_that_work() {
    // Gap 1 (2026-10-09): a take_on from an Eve Link app turn. Its backbrief goes to the app's stream with Change
    // and Drop as the words that answer them (as a picker's do), not to Telegram; those words, typed back from the
    // app, take the buttons' own path (the chief's code words); the card is recorded against the app turn.
    let _iso = isolated();
    let home = tmp();
    let memory = FakeMemory::new(home.path(), "");
    let r = ChiefRig::new(
        FakeApi::new(vec![]),
        FakeCreate::new(vec![pcall("take_on", desk_job())]),
        Kw { with_chief: true, memory: Some(memory.clone()), ..Kw::default() },
    );
    r.rig.inlet.tt_set_chat_id(Some(OWNER));
    let shown: Arc<Mutex<Vec<(String, String, String)>>> = Arc::default();
    let s = shown.clone();
    *r.rig.inlet.app_out.lock() = Some(Arc::new(move |turn: &str, text: &str, kind: &str| {
        s.lock().push((turn.to_string(), text.to_string(), kind.to_string()));
        true // the app is reading
    }));
    let app = TurnCtx { app: Some("app-turn-1".into()), ..TurnCtx::default() };
    let inlet = r.rig.inlet.clone();
    let shown2 = shown.clone();
    in_ctx(app, async move {
        inlet.tt_turn(&inbound(SAID, 1)).await;
        settle(200).await;
        {
            let shown = shown2.lock();
            assert_eq!(shown.len(), 1);
            let (turn, text, kind) = &shown[0];
            assert!(turn == "app-turn-1" && kind == "say");
            assert!(text.starts_with(&format!("**{CHIEF_TITLE}**\n\nOn it: Standing desk.")) && text.ends_with(CHANGE_DROP), "{text}");
        }
        assert_eq!(inlet.app_filed("app-turn-1").as_deref(), Some("c1"));
        assert_eq!(inlet.app_filed("app-turn-2"), None);
        inlet.tt_handle(&inbound("change c1", 2)); // typed in the app
        settle(200).await;
        assert!(shown2.lock().last().unwrap().1.contains("To change it: drop c1"));
        inlet.tt_handle(&inbound("drop c1", 3));
        settle(200).await;
        assert!(shown2.lock().last().unwrap().1.ends_with("Dropped c1."));
    })
    .await;
    assert!(r.card("c1").terminal()); // Drop worked
    assert!(r.api.log.lock().sent.is_empty() && r.api.log.lock().buttons.is_empty()); // nothing went to Telegram
    let plain = r
        .history()
        .into_iter()
        .find(|(who, t)| who == "eve" && t.starts_with("On it: Standing desk."))
        .map(|(_, t)| t)
        .expect("the backbrief is in the history");
    assert!(!plain.contains(CHANGE_DROP)); // the model's history is the backbrief alone
    assert!(eve_rows(&memory).contains(&format!("{plain}\n\n{CHANGE_DROP}"))); // the app's chat shows the words to reply
}

#[tokio::test]
async fn test_a_telegram_turns_backbrief_shows_change_and_drop_in_the_app_when_telegram_is_muted() {
    // Telegram app-only (the owner is on the app): a take_on from a turn that is not the app's (a call, a typed
    // Telegram line) still gets its buttons where Telegram would show them, and the transcript line, which the
    // app's chat shows, carries the words that answer them. With Telegram on, the line is as it always was.
    // Python's parametrize ids "app-only" and "telegram-on" are this loop's two rounds.
    for app_only in [true, false] {
        let _iso = isolated();
        let home = tmp();
        let memory = FakeMemory::new(home.path(), "");
        let r = ChiefRig::new(
            FakeApi::new(vec![]),
            FakeCreate::new(vec![pcall("take_on", desk_job())]),
            Kw { with_chief: true, memory: Some(memory.clone()), app_only, ..Kw::default() },
        );
        r.rig.inlet.tt_set_chat_id(Some(OWNER));
        // an app is linked, but this is not its turn
        *r.rig.inlet.app_out.lock() = Some(Arc::new(|_turn: &str, _text: &str, _kind: &str| true));
        r.rig.inlet.tt_turn(&inbound(SAID, 1)).await; // not an app turn
        settle(200).await;
        let sent = sent_with_buttons(&r.api);
        assert_eq!(sent.len(), 1, "app_only={app_only}");
        let (text, buttons) = sent[0].clone();
        assert!(text.starts_with("On it: Standing desk.") && buttons == ["Change", "Drop"]);
        assert!(r.has("eve", &text));
        let rows = eve_rows(&memory);
        assert_eq!(rows.contains(&format!("{text}\n\n{CHANGE_DROP}")), app_only, "app_only={app_only}");
        assert_eq!(rows.contains(&text), !app_only); // control: Telegram on keeps the line as it was
        assert!(r.rig.inlet.app_filed("anything").is_none() && r.rig.inlet.tt_app_filed_is_empty()); // not an app turn
    }
}

#[tokio::test]
async fn test_a_backbrief_telegram_refuses_to_button_is_still_sent() {
    let _iso = isolated();
    let api = FakeApi::new(vec![vec![said(SAID, 1)]]);
    *api.fail_inline.lock() = Some(api_error(400, "Bad Request: inline keyboard refused"));
    let r = ChiefRig::new(api, FakeCreate::new(vec![pcall("take_on", desk_job())]), Kw { with_chief: true, ..Kw::default() });
    run(&r, || async {}).await;
    assert_eq!(r.create.requests.lock().len(), 1);
    assert!(r.texts()[0].starts_with("On it: Standing desk."), "{:?}", r.texts());
}

#[tokio::test]
async fn test_jobs_and_the_id_words_make_no_model_call() {
    let _iso = isolated();
    // any model call fails: "nobody scripted"
    let r = ChiefRig::new(FakeApi::new(vec![]), FakeCreate::new(vec![]), Kw { with_chief: true, ..Kw::default() });
    run(&r, || async {
        r.take(desk_job(), SAID).await;
        r.take(read_job(), READ_SAID).await;
        let words = [
            "/jobs", "/jobs c1", "keep c2", "raise c2", "drop c2", "reopen c9", "change c1", "go c1", "Tomorrow c1",
            "happened c1", "didnt c1",
        ];
        for (n, text) in words.iter().enumerate() {
            r.feed(text, n as i64 + 1);
            settle(200).await;
        }
    })
    .await;
    assert!(r.create.requests.lock().is_empty() && r.options.lock().is_empty());
    let said = r.texts();
    assert!(said[0].starts_with("c1 ") && said[0].contains("c2 "), "{said:?}"); // /jobs: both cards, by code
    assert!(said[1].starts_with("c1 Standing desk") && said[1].contains("Check: "), "{said:?}");
    assert!(said.contains(&"Kept c2.".to_string()) && said.contains(&"Dropped c2.".to_string()), "{said:?}");
    assert!(said.iter().any(|s| s.starts_with("c2 may now spend $")), "{said:?}");
    assert!(said.contains(&"there is no card c9".to_string()), "{said:?}");
    assert!(said.iter().any(|s| s.contains("To change it: drop c1")), "{said:?}");
    // go before the pick: refused, by code
    assert!(said.contains(&"c1 is not at that step yet; I'll ask when it is".to_string()), "{said:?}");
    assert!(said.contains(&"c1 has no reminder to move".to_string()) && said.contains(&"c1 has no step in doubt".to_string()));
    assert_eq!(r.card("c2").status, "dropped");
    assert!(!said.contains(&FAILED_LINE.to_string()));
}

#[tokio::test]
async fn test_the_turns_note_carries_the_chiefs_line() {
    let _iso = isolated();
    let r = ChiefRig::new(FakeApi::new(vec![]), FakeCreate::new(vec![say("Nothing new.")]), Kw { with_chief: true, ..Kw::default() });
    run(&r, || async {
        r.take(desk_job(), SAID).await;
        r.feed("anything new?", 1);
    })
    .await;
    let line = r.chief().for_turn(None);
    assert!(line.starts_with("Open jobs: 1 (c1 ") && line.chars().count() <= 300, "{line}");
    let request = r.create.requests.lock()[0].clone();
    let notes: Vec<&Value> = request["input"].as_array().unwrap().iter().filter(|i| i.get("role") == Some(&json!("developer"))).collect();
    assert_eq!(notes.len(), 1);
    assert!(notes[0]["content"][0]["text"].as_str().unwrap().ends_with(&format!("\n\n{line}")));
    assert!(!request["instructions"].as_str().unwrap().contains(&line)); // the note, after the history: cache-free
}

#[tokio::test]
async fn test_the_menu_offers_jobs_only_while_the_chief_is_on() {
    let _iso = isolated();
    let on = ChiefRig::new(FakeApi::new(vec![]), FakeCreate::new(vec![]), Kw { with_chief: true, ..Kw::default() });
    run(&on, || async {}).await;
    let off = ChiefRig::new(FakeApi::new(vec![]), FakeCreate::new(vec![]), Kw::default());
    run(&off, || async {}).await;
    let on_commands = on.api.log.lock().commands.clone();
    let off_commands = off.api.log.lock().commands.clone();
    assert!(on_commands.len() == 1 && off_commands.len() == 1);
    let (on_menu, off_menu) = (&on_commands[0].0, &off_commands[0].0);
    let want: Vec<(String, String)> = BOT_COMMANDS.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
    let jobs = (JOBS_MENU[0].to_string(), JOBS_MENU[1].to_string());
    assert!(*off_menu == want && !off_menu.contains(&jobs));
    assert_eq!(on_menu[on_menu.len() - 2..], [jobs, want.last().unwrap().clone()]);
    assert_eq!(on_menu.len(), off_menu.len() + 1);
}

// ---- on: the Mac slot, the Go, the floor --------------------------------------------------------------------------

/// Yield until `done()` holds, for at most five seconds of wall time: what `settle` does, without a fixed count.
async fn until(done: impl Fn() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !done() && std::time::Instant::now() < deadline {
        settle(10).await;
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    }
}

#[tokio::test]
async fn test_a_chief_mac_step_queues_while_a_task_runs_or_the_desk_has_the_mac() {
    let _iso = isolated();
    let busy = Arc::new(AtomicBool::new(false));
    let r = ChiefRig::new(
        FakeApi::new(vec![vec![said("open the calculator", 1)]]),
        FakeCreate::new(vec![pcall("start_task", json!({"goal": "open the calculator"}))]),
        Kw { with_chief: true, route: "mac", hold: true, busy: Some(busy.clone()), ..Kw::default() },
    );
    run(&r, || async {
        r.take(mac_job(), READ_SAID).await;
        assert!(r.rig.inlet.task_running() && r.agent_count() == 1); // the owner's own task has the Mac
        r.tick().await;
        let card = r.card("c1");
        assert!(card.status == "waiting" && card.waiting_for.as_deref() == Some("mac_slot") && r.agent_count() == 1);
        r.agent(0).release.set();
        // the released run ends on another task: wait for it (a fixed number of yields lost the race under load)
        until(|| !r.rig.inlet.task_running()).await;
        assert!(!r.rig.inlet.task_running());
        busy.store(true, Ordering::SeqCst); // someone is talking at the desk
        r.tick().await;
        assert!(r.agent_count() == 1 && r.card("c1").waiting_for.as_deref() == Some("mac_slot"));
        // queued in the ledger, not tried and refused: nothing was dispatched while the Mac was taken
        assert!(!events(&r, "c1").iter().any(|e| e["e"] == "dispatched" || e["e"] == "result"));
        busy.store(false, Ordering::SeqCst);
        r.tick().await; // free: the step goes, read-only, on the floor
        // the second agent is made, then given its goal when its run starts
        until(|| r.agent_count() == 2 && !r.agent(1).goal.lock().is_empty()).await;
        settle(200).await;
        assert!(r.agent_count() == 2 && *r.floors.lock() == vec![None, None]);
        assert!(r.agent(1).goal.lock().starts_with(eve_chief::READ_ONLY));
        assert_eq!(r.rig.inlet.tt_chief_task(), Some(("c1".to_string(), 1)));
        r.agent(1).release.set();
        settle(200).await;
    })
    .await;
    let results: Vec<Map<String, Value>> = events(&r, "c1").into_iter().filter(|e| e["e"] == "result").collect();
    let last = results.last().expect("a result");
    assert!(last["phase"] == 1 && !last.get("stale").is_some_and(|s| s.as_bool().unwrap_or(false)));
    assert!(r.texts().contains(&"On it: c1, step 1.".to_string()), "{:?}", r.texts());
    // "Task result" once, for the owner's own task; the chief's step gets its receipt in place of one
    let titled = r.api.log.lock().titled.clone();
    assert_eq!(titled.iter().filter(|t| t.0.as_deref() == Some(TASK_DONE_TITLE)).count(), 1);
    assert_eq!(r.texts().iter().filter(|t| *t == "Done: the order went through.").count(), 1);
}

#[tokio::test]
async fn test_go_c12_approves_only_that_cards_pending_act_at_its_current_revision() {
    let _iso = isolated();
    let r = ChiefRig::new(FakeApi::new(vec![]), FakeCreate::new(vec![say("Sure.")]), Kw { with_chief: true, ..Kw::default() });
    run(&r, || async {
        r.take(act_job(), ACT_SAID).await;
        r.take(act_job(), ACT_SAID).await;
        r.feed("go c2", 1);
        settle(200).await;
        let (c1, c2) = (r.card("c1"), r.card("c2"));
        assert!(c1.phase(1).unwrap().approval.is_none());
        assert_eq!(c2.phase(1).unwrap().approval.as_ref().expect("approved").rev, c2.rev);
        r.feed("yes", 2); // a bare yes approves nothing that was not asked
        settle(200).await;
        assert!(r.card("c1").phase(1).unwrap().approval.is_none());
        let new = r.chief().change("c2", &obj(json!({"never": ["over $500"]})), "make it under $500").unwrap();
        assert_eq!(new["ok"], true);
        assert!(r.card("c2").phase(1).unwrap().approval.is_none()); // a new revision cancels it
        r.feed("go c2", 3);
        settle(200).await;
        let c2 = r.card("c2");
        let approval = c2.phase(1).unwrap().approval.clone().expect("approved");
        assert!(approval.rev == c2.rev && c2.rev == 2);
    })
    .await;
    assert_eq!(r.create.requests.lock().len(), 1); // only the bare "yes" was a model turn
    assert!(r.texts().contains(&"Go: c2, step 1, runs now.".to_string()), "{:?}", r.texts());
}

#[tokio::test]
async fn test_the_go_is_asked_with_yes_and_no_and_the_approved_act_runs_on_codex() {
    let _iso = isolated();
    let evidence = vec![json!({"call_id": "u1", "state": "Window: Shop\nOrder confirmed, thank you"})];
    let r = ChiefRig::new(FakeApi::new(vec![]), FakeCreate::new(vec![]), Kw { with_chief: true, evidence, ..Kw::default() });
    run(&r, || async {
        r.take(act_job(), ACT_SAID).await;
        r.tick().await; // the act needs a Go: asked, with Yes and No
        settle(200).await;
        let question = r.texts().last().cloned().unwrap();
        assert!(question.starts_with("c1, Order the desk: order the desk in the open tab.") && question.ends_with("Go?"), "{question}");
        assert_eq!(r.api.log.lock().buttons.last().unwrap(), &["Yes", "No"]);
        assert_eq!(last_title(&r.api).as_deref(), Some(CHIEF_TITLE));
        assert!(r.has("eve", "(I asked your Go for c1.)") && !r.has("eve", &question));
        tap(&r, "Yes", 1);
        settle(200).await;
        r.tick().await; // dispatched: on Codex, whatever the floor
        settle(200).await;
        r.tick().await; // nothing left to run: the card closes
        settle(200).await;
    })
    .await;
    assert_eq!(*r.floors.lock(), vec![Some("codex".to_string())]);
    let goal = r.agent(0).goal.lock().clone();
    assert!(goal.starts_with("order the desk in the open tab.") && goal.contains(eve_chief::ACT_GUARD), "{goal}");
    let card = r.card("c1");
    assert_eq!(card.status, "done", "{card:?}");
    let receipt = r.texts().last().cloned().unwrap();
    assert!(receipt.starts_with("Done: Order the desk"), "{receipt}");
    assert_eq!(last_title(&r.api).as_deref(), Some(CHIEF_TITLE));
    assert_eq!(r.api.log.lock().buttons.last().unwrap(), &["Reopen"]);
    assert!(r.create.requests.lock().is_empty()); // the Go, the act and the receipt: no chat turn
}

#[tokio::test]
async fn test_a_tap_on_drop_under_the_backbrief_drops_the_card_by_code() {
    let _iso = isolated();
    let r = ChiefRig::new(
        FakeApi::new(vec![vec![said(SAID, 1)]]),
        FakeCreate::new(vec![pcall("take_on", desk_job())]),
        Kw { with_chief: true, ..Kw::default() },
    );
    run(&r, || async {
        tap(&r, "Drop", 2);
        settle(200).await;
    })
    .await;
    assert!(r.card("c1").status == "dropped" && r.texts().contains(&"Dropped c1.".to_string()), "{:?}", r.texts());
    assert_eq!(r.create.requests.lock().len(), 1);
}

// ---- the self-context line --------------------------------------------------------------------------------------

/// test_self_context.py's `owner_env`: the owner's environment's shape, with placeholder keys.
fn owner_env(extra: &[(&str, &str)]) -> HashMap<String, String> {
    let mut e: HashMap<String, String> = [
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
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    for (k, v) in extra {
        e.insert(k.to_string(), v.to_string());
    }
    e
}

#[test]
fn test_the_self_context_block_stays_under_budget_with_everything_on() {
    let dir = tmp();
    let path: PathBuf = dir.path().join("lights.json");
    let lights = [
        eve_lights::Light { name: "a".into(), kind: "govee".into(), ip: "192.0.2.1".into(), ..Default::default() },
        eve_lights::Light { name: "b".into(), kind: "wiz".into(), ip: "192.0.2.2".into(), ..Default::default() },
        eve_lights::Light { name: "c".into(), kind: "triones".into(), address: "u".into(), ..Default::default() },
        eve_lights::Light { name: "d".into(), kind: "tuya".into(), id: "x".into(), ..Default::default() },
    ];
    eve_lights::save(lights.iter(), Some(&path)).unwrap();
    // fake_router(monkeypatch, ("perplexity", "tinyfish", "firecrawl"))
    let router = Router {
        available: ["perplexity", "tinyfish", "firecrawl"].iter().map(|p| p.to_string()).collect(),
        shipped: true,
    };
    let lent = Lent { lights_path: Some(path), commit: Some("abcdef123456".into()), router: Some(router), connect_file: None };
    let env = owner_env(&[
        ("EVE_VOICE", "1"),
        ("EVE_WEB_READER", "1"),
        ("TINYFISH_API_KEY", "test-tinyfish"),
        ("EVE_WATCH_TLS", "1"),
        ("EVE_WATCH_BROWSER", "1"),
        ("EVE_CHIEF", "on"),
    ]);
    let text = self_context::block_lent(Some(&env), &lent);
    assert!(text.contains("\n- Chief of staff: on.\n") && text.chars().count() < self_context::BUDGET, "{}", text.chars().count());
    assert!(text.contains("Voice:") && text.contains("Jev picks") && text.contains("Tasks on the Mac: Codex"), "{text}");
    // Python patches chief.SHIPPED to False: auto is then off, so the block has no line. Rust's SHIPPED is a
    // const, so the switch is read with SHIPPED as a parameter, and the block's own "off" stands for it.
    let mut auto = env.clone();
    auto.insert("EVE_CHIEF".into(), "auto".into());
    assert!(!eve_chief::enabled_shipped(Some(&auto), false));
    let mut off = env.clone();
    off.insert("EVE_CHIEF".into(), "off".into());
    assert!(!self_context::block_lent(Some(&off), &lent).contains("Chief of staff"));
}

// ---- the reviewer's attacks after P4 (2026-09-29): each was reproduced first, then fixed ----------------------------

#[tokio::test]
async fn test_attack_a_sentence_that_opens_with_ok_never_answers_the_go() {
    // With no relay on, the owner's next message answered any question, and the Go read only its first word:
    // "ok, also what's the weather tomorrow?" approved the order and ran it, and never reached the brain. The Go is
    // strict like a permission prompt, relay or not: a bare yes or no, or a button, answers it; any other words go
    // to the brain and the Go keeps waiting.
    let _iso = isolated();
    let r = ChiefRig::new(
        FakeApi::new(vec![]),
        FakeCreate::new(vec![say("Sunny tomorrow.")]),
        Kw { with_chief: true, evidence: ordered(), ..Kw::default() },
    );
    run(&r, || async {
        r.take(act_job(), ACT_SAID).await;
        r.tick().await;
        settle(200).await;
        assert!(r.texts().last().unwrap().ends_with("Go?"));
        r.feed("ok, also what's the weather tomorrow?", 1);
        settle(200).await;
        r.tick().await;
        settle(200).await;
        assert!(r.agent_count() == 0 && r.card("c1").phase(1).unwrap().approval.is_none());
        assert!(r.rig.inlet.tt_awaiting_answer()); // the Go still waits
        r.feed("yes", 2); // control: a bare yes answers it
        settle(200).await;
        r.tick().await;
        settle(200).await;
    })
    .await;
    assert_eq!(r.create.requests.lock().len(), 1);
    assert!(r.texts().contains(&"Sunny tomorrow.".to_string()));
    assert_eq!(*r.floors.lock(), vec![Some("codex".to_string())]);
    let kinds: Vec<String> = events(&r, "c1")
        .iter()
        .filter_map(|e| e["e"].as_str().map(str::to_string))
        .filter(|k| ["answered", "approved", "dispatched"].contains(&k.as_str()))
        .collect();
    assert_eq!(kinds, ["answered", "approved", "dispatched"]);
}

#[tokio::test]
async fn test_attack_a_codex_failure_never_closes_the_card_done() {
    let _iso = isolated();
    let job = with(desk_job(), json!({"done_checks": [{"kind": "read_links", "arg": "1"}, {"kind": "cited_pick", "arg": null}]}));
    let sources: Vec<Value> = LINKS[..2].iter().map(|u| json!({"url": u})).collect();
    // even with a screen seen
    let r = ChiefRig::new(
        FakeApi::new(vec![]),
        FakeCreate::new(vec![]),
        Kw { with_chief: true, failing: true, evidence: ordered(), sources: Some(sources), ..Kw::default() },
    );
    run(&r, || async {
        r.take(job, SAID).await;
        for _ in 0..6 {
            r.tick().await;
            settle(200).await;
            let c = r.card("c1");
            if c.status == "waiting" && c.waiting_for.as_deref() == Some("go") {
                break;
            }
        }
        r.feed("go c1", 9);
        settle(200).await;
        for _ in 0..4 {
            r.tick().await;
            settle(200).await;
        }
    })
    .await;
    let card = r.card("c1");
    assert!(r.agent_count() > 0 && *r.floors.lock() == vec![Some("codex".to_string())], "{:?}", r.floors.lock());
    assert!(card.phase(3).unwrap().status == "failed" && card.status == "unverified", "{card:?}");
    assert!(!r.texts().iter().any(|t| t.starts_with("Done: Standing desk")));
}

#[tokio::test]
async fn test_attack_on_a_call_only_a_plain_yes_approves_the_go() {
    // On a call, words that are not a choice were read by a chat model, and its pick approved the act. The Go is
    // answered on a call as a permission prompt is: the exact choice or a plain yes or no, else asked again.
    let _iso = isolated();
    let r = ChiefRig::new(
        FakeApi::new(vec![]),
        FakeCreate::new(vec![say(&json!({"choice": "yes"}).to_string())]),
        Kw { with_chief: true, ..Kw::default() },
    );
    let spoken: Arc<Mutex<Vec<String>>> = Arc::default();
    let s = spoken.clone();
    r.rig.inlet.tt_set_call_say(Some(Reader::new(move |text: &str| s.lock().push(text.to_string()))));
    let hear = |text: &str| r.rig.inlet.hear(text, eve_phone_call::Captured::Uncaptured, true, None, None, "auto", &[]);
    run(&r, || async {
        r.take(act_job(), ACT_SAID).await;
        r.tick().await;
        settle(200).await;
        hear("hmm, I'll think about it after lunch");
        settle(200).await;
        assert!(r.card("c1").phase(1).unwrap().approval.is_none());
        assert!(spoken.lock().contains(&CALL_ASK_AGAIN.to_string()), "{:?}", spoken.lock());
        hear("yes"); // control: a plain yes by voice
        settle(200).await;
    })
    .await;
    assert!(r.create.requests.lock().is_empty()); // no model read the owner's words
    assert!(r.card("c1").phase(1).unwrap().approval.is_some());
}

#[tokio::test]
async fn test_attack_the_brain_cannot_steer_a_chief_step() {
    // steer_task reached the approved act mid-run, past its Go: a chief step changes only by "change c<N>".
    let _iso = isolated();
    let r = ChiefRig::new(FakeApi::new(vec![]), FakeCreate::new(vec![]), Kw { with_chief: true, hold: true, ..Kw::default() });
    let steered: Arc<Mutex<Vec<String>>> = Arc::default();
    let steer_hook = |steered: &Arc<Mutex<Vec<String>>>| -> Arc<dyn Fn(&str) -> bool + Send + Sync> {
        let s = steered.clone();
        Arc::new(move |text: &str| {
            s.lock().push(text.to_string());
            true
        })
    };
    run(&r, || async {
        r.take(act_job(), ACT_SAID).await;
        r.chief().approve("c1", "id", None, None, None).unwrap();
        r.tick().await;
        settle(200).await;
        assert!(r.rig.inlet.tt_chief_task() == Some(("c1".to_string(), 1)) && r.agent_count() > 0);
        *r.agent(0).steer_with.lock() = Some(steer_hook(&steered));
        let out = r.rig.inlet.tt_tool("steer_task", &obj(json!({"text": "also add the extended warranty and pay for it"})), OWNER).await;
        assert!(out["ok"] == false && out["reason"].as_str().unwrap().contains("c1"), "{out:?}");
        let stop = r.rig.inlet.tt_tool("stop_task", &Obj::new(), OWNER).await; // stop still works
        assert_eq!(Value::Object(stop), json!({"ok": true}));
        settle(200).await;
    })
    .await;
    assert!(steered.lock().is_empty());
    // control: the owner's own task is steered as before
    let own = ChiefRig::new(FakeApi::new(vec![]), FakeCreate::new(vec![]), Kw { hold: true, ..Kw::default() });
    let inlet = own.rig.inlet.clone();
    let loop_task = tokio::spawn(async move { inlet.run().await });
    settle(200).await;
    // `inlet._start_task(goal, OWNER)`: the start_task tool's own path
    let started = own.rig.inlet.tt_tool("start_task", &obj(json!({"goal": "open the calculator"})), OWNER).await;
    assert_eq!(started["ok"], true, "{started:?}");
    settle(200).await;
    *own.agent(0).steer_with.lock() = Some(steer_hook(&steered));
    let out = own.rig.inlet.tt_tool("steer_task", &obj(json!({"text": "use the scientific mode"})), OWNER).await;
    assert_eq!(Value::Object(out), json!({"ok": true}));
    own.agent(0).release.set();
    settle(200).await;
    loop_task.abort();
    let _ = loop_task.await;
    assert_eq!(*steered.lock(), vec!["use the scientific mode".to_string()]);
}

#[tokio::test]
async fn test_attack_a_chief_steps_sentence_never_enters_the_history_as_eves() {
    let _iso = isolated();
    let r = ChiefRig::new(FakeApi::new(vec![]), FakeCreate::new(vec![]), Kw { with_chief: true, evidence: ordered(), ..Kw::default() });
    run(&r, || async {
        r.take(act_job(), ACT_SAID).await;
        r.chief().approve("c1", "id", None, None, None).unwrap();
        r.tick().await;
        settle(200).await;
        r.tick().await;
        settle(200).await;
    })
    .await;
    let history = r.history();
    assert!(!history.iter().any(|(_, text)| text.contains("the order went through")), "{history:?}");
    assert!(r.has("eve", "(Step 1 of c1 ended.)"), "{history:?}");
}
