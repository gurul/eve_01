//! A port of `telegram.py`, lines 6611-7077: tell_owner, the chief of staff, spend, stick, apps, direct/share/gate.

use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};

use futures::FutureExt;

#[allow(unused_imports)]
use crate::prelude::*;
#[allow(unused_imports)]
use crate::telegram::*;

use super::tools::{itl_failed, itl_local_fmt, itl_o, itl_py_str, itl_refused, itl_str, itl_truthy};

/// `Choice(label, value, style, done)`.
fn choice(label: &str, value: &str, style: &str, done: &str) -> Choice {
    Choice { label: label.into(), value: value.into(), style: style.into(), done: done.into() }
}

/// telegram.py:1632 `ALLOW_DENY`.
pub(crate) fn itl_allow_deny() -> Vec<Choice> {
    vec![choice("Allow", "yes", "success", "Allowed."), choice("Deny", "no", "danger", "Denied.")]
}

/// telegram.py:1633 `YES_NO`.
pub(crate) fn itl_yes_no() -> Vec<Choice> {
    vec![choice("Yes", "yes", "success", "Yes."), choice("No", "no", "danger", "No.")]
}

/// `re.match(r"(c\d+),", text)`: the card a Go names first.
static GO_CARD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(c\d+),").expect("GO_CARD"));

/// `re.search(r"\bc\d+\b", about)`: the card an alert's line names.
static ABOUT_CARD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\bc\d+\b").expect("ABOUT_CARD"));

/// `_write_question`'s `describer`: the module whose `describe_whole` (and `asks_whole`, when it has one) shows a call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ItlDescriber {
    /// apps_mcp: the direct apps (Gmail's whole-email allowance is its `asks_whole`)
    Apps,
    /// composio_tools: no `asks_whole`
    Composio,
}

impl TelegramInlet {
    /// py: telegram.py:6611 `async def tell_owner(self, text: str, about: str = "", title: str = WATCH_TITLE, buttons: Sequence[Choice] = ()) -> bool:`
    ///
    /// A watch alert (watch.Watcher.notify): a new message in the owner's chat, unasked. It goes into the
    /// chat's history and transcript as eve's, so "stop watching that" in reply has something to point at.
    /// Stealth does not hold it back: stealth is the desk's, and this is the phone. Returns whether it was sent:
    /// the watcher keeps an alert Telegram refused and sends it again (``_say`` would swallow the refusal).
    /// The history and the transcript get ``about``, a line the watcher builds from the watch alone: the alert
    /// itself can carry words from the web, and the brain reading them as its own words was a stored prompt
    /// injection (re-verification, 2026-09-25).
    ///
    /// ``buttons`` (the chief's, 2026-09-29): SAY buttons under the message, whose tap types their words, so a
    /// tap and the typed code word take one path (``_on_tap``). A keyboard Telegram refuses leaves the message
    /// plain, never unsent.
    ///
    /// While the owner is in Eve Link (an app turn's work, ``_FROM_APP``, or Telegram app-only), the buttons
    /// reach the app the way a picker's do (``_send_choices``): as the words that answer them (``reply_words``),
    /// which the owner types or taps back and which take the tap's own path (a code word, ``_chief_command``).
    /// An app turn's message goes to the app's open stream, not to Telegram, when its words are eve's own
    /// (no separate ``about``); the transcript line, which the app's chat shows, carries the reply words.
    pub async fn tell_owner(self: &Arc<Self>, text: &str, about: &str, title: &str, buttons: &[Choice]) -> bool {
        let chat = self.st.lock().chat_id;
        let Some(chat) = chat else {
            return false;
        };
        self.to_call(chat, text, Some(title));
        let choices: Vec<Choice> = buttons.iter().take(MAX_INLINE_BUTTONS).cloned().collect();
        let on_app = !choices.is_empty() && (ctx().app.is_some() || self.config.app_only);
        let noted = if about.is_empty() { text } else { about };
        // the transcript's words
        let shown = if on_app { Some(format!("{noted}\n\n{}", reply_words(&choices))) } else { None };
        let title_opt = Some(title).filter(|t| !t.is_empty());
        if on_app && (about.is_empty() || about == text) {
            let head = if title.is_empty() { String::new() } else { format!("**{title}**\n\n") };
            let line = format!("{head}{text}\n\n{}", reply_words(&choices));
            if self.to_app(Some(chat), &line, "say") {
                self.note("eve", noted, "say", shown.as_deref(), None, None, false, None);
                return true;
            }
        }
        if !buttons.is_empty() {
            let board = Arc::new(_Keyboard {
                chat_id: Some(chat),
                kind: SAY.to_string(),
                choices: choices.clone(),
                future: None,
                valid: None,
                on_stop: None,
                keys: Mutex::new(Vec::new()),
                message_id: Mutex::new(0),
            });
            self.register(&board);
            let keys = board.keys.lock().clone();
            let rows = vec![board
                .choices
                .iter()
                .zip(keys.iter())
                .map(|(c, key)| (c.label.clone(), key.clone(), c.style.clone()))
                .collect::<Vec<_>>()];
            let sent = AssertUnwindSafe(self.api.send_inline(chat, text, &rows, title_opt, None)).catch_unwind().await;
            match sent {
                Ok(Ok(id)) => {
                    *board.message_id.lock() = id;
                    self.note("eve", noted, "say", shown.as_deref(), None, None, false, None);
                    return true;
                }
                Ok(Err(e)) => {
                    // no buttons is a plain message, never no message
                    self.retire(Some(&board), false);
                    tracing::warn!("telegram: buttons not sent ({e}); sent as a plain message");
                }
                Err(_) => {
                    self.retire(Some(&board), false);
                    tracing::warn!("telegram: buttons not sent (panic); sent as a plain message");
                }
            }
        }
        if let Err(e) = self.api.send_message(chat, text, title_opt, None, &[], 0, None).await {
            // BotApiError, a network error: not sent, tried again later
            tracing::warn!("telegram: a watch alert was not sent ({e})");
            return false;
        }
        self.note("eve", noted, "say", shown.as_deref(), None, None, false, None);
        true
    }

    /// py: telegram.py:6669 `def _chief_word(self, text: str) -> Optional[tuple[str, str]]:`
    ///
    /// ("jobs", "" | card id) or (word, card id) for a chief code word, or None: always None while the chief is
    /// off, so the words go where they always went.
    pub(crate) fn chief_word(self: &Arc<Self>, text: &str) -> Option<(String, String)> {
        self.chief.lock().as_ref()?;
        let said = super::tools::itl_rstrip_chars(super::tools::itl_strip(text), ".! ");
        if let Some(jobs) = JOBS_COMMAND.captures(said) {
            return Some(("jobs".into(), jobs.get(1).map(|m| m.as_str().to_lowercase()).unwrap_or_default()));
        }
        if let Some(word) = CHIEF_WORD.captures(said) {
            return Some((word[1].to_lowercase().replace('\'', ""), word[2].to_lowercase()));
        }
        None
    }

    /// py: telegram.py:6683 `async def _chief_command(self, inbound: Inbound, verb: str, cid: str) -> None:`
    ///
    /// /jobs, "/jobs c12" and "<word> c12", by code (chief.py's own methods), never a model call. A word that
    /// sends its own message (done, happened: the receipt) is answered only when it could not act.
    pub(crate) async fn chief_command(self: &Arc<Self>, inbound: &Inbound, verb: &str, cid: &str) {
        let chief = self.chief.lock().clone();
        let Some(chief) = chief else {
            return;
        };
        let chat = inbound.chat_id;
        let ran = AssertUnwindSafe(self.chief_word_line(&chief, inbound, verb, cid)).catch_unwind().await;
        let line = match ran {
            Ok(Ok(line)) => line,
            Ok(Err(e)) => {
                // a card word that breaks is a line, never a silence
                tracing::warn!("telegram: the chief's {verb} failed ({})", eve_chief::err_name(&e));
                "I couldn't do that just now.".to_string()
            }
            Err(_) => {
                tracing::warn!("telegram: the chief's {verb} failed (panic)");
                "I couldn't do that just now.".to_string()
            }
        };
        if !line.is_empty() {
            self.say(chat, &line, Some(CHIEF_TITLE), None, &[], 0, None).await;
        }
    }

    /// `_chief_command`'s `try:` body: the line to say ("" for none).
    async fn chief_word_line(
        self: &Arc<Self>,
        chief: &Arc<eve_chief::Chief>,
        inbound: &Inbound,
        verb: &str,
        cid: &str,
    ) -> anyhow::Result<String> {
        let reason = |r: &Value| itl_py_str(r.get("reason"));
        let ok = |r: &Value| super::tools::itl_truthy_of(r.get("ok"));
        let line = match verb {
            "jobs" => {
                if cid.is_empty() {
                    chief.listing()
                } else {
                    chief.details(cid)
                }
            }
            "go" => {
                let r = chief.approve(cid, "id", None, None, None)?;
                if ok(&r) {
                    format!("Go: {cid}, step {}, runs now.", itl_py_str(r.get("phase")))
                } else {
                    reason(&r)
                }
            }
            "drop" => {
                let r = eve_chief::Chief::drop(chief, cid)?;
                if ok(&r) { format!("Dropped {cid}.") } else { reason(&r) }
            }
            "keep" => {
                let r = chief.keep(cid)?;
                if ok(&r) { format!("Kept {cid}.") } else { reason(&r) }
            }
            "done" => {
                let r = chief.done(cid).await?;
                if ok(&r) { String::new() } else { reason(&r) }
            }
            "raise" => {
                let r = chief.raise_budget(cid)?;
                if ok(&r) {
                    let usd = r.get("usd").and_then(Value::as_f64).ok_or_else(|| anyhow::anyhow!("KeyError: 'usd'"))?;
                    format!(
                        "{cid} may now spend ${} and {} min.",
                        format!("{usd:.2}"),
                        itl_py_str(r.get("minutes"))
                    )
                } else {
                    reason(&r)
                }
            }
            "reopen" => {
                let r = chief.reopen(cid)?;
                if ok(&r) { format!("Reopened {cid}.") } else { reason(&r) }
            }
            "happened" => {
                let r = chief.happened(cid).await?;
                let card = chief.ledger.get(cid);
                if !ok(&r) {
                    reason(&r)
                } else if card.as_ref().is_none_or(|c| c.terminal()) {
                    String::new()
                } else {
                    format!("Noted. {cid} goes on.")
                }
            }
            "didnt" => {
                let r = chief.didnt(cid)?;
                if ok(&r) { super::tools::itl_str_of(r.get("say")) } else { reason(&r) }
            }
            "tomorrow" => {
                let day = Self::tomorrow_of((self.wall)());
                let r = chief.snooze(cid, &format!("{day} {CHIEF_SNOOZE_AT}"), &inbound.text)?;
                if ok(&r) { format!("Moved {cid} to {}.", itl_py_str(r.get("at"))) } else { reason(&r) }
            }
            "change" => {
                // A correction in the owner's words needs a model to become a card; the chat's tools are take_on,
                // jobs_list and close_card, and a changed card is a new one: the card as it stands, and how.
                match chief.ledger.get(cid) {
                    Some(card) if !card.terminal() => format!(
                        "{}\nTo change it: drop {cid}, then tell me the job again the way you want it.",
                        eve_chief_card::backbrief(&card)
                    ),
                    _ => format!("There is no open card {cid}."),
                }
            }
            _ => String::new(),
        };
        Ok(line)
    }

    /// `(datetime.fromtimestamp(wall) + timedelta(days=1)).date().isoformat()`.
    fn tomorrow_of(wall: f64) -> String {
        use chrono::TimeZone;
        let day = match chrono::Local.timestamp_opt(wall.floor() as i64, 0) {
            chrono::LocalResult::Single(at) | chrono::LocalResult::Ambiguous(at, _) => at.date_naive(),
            chrono::LocalResult::None => chrono::Local::now().date_naive(),
        };
        (day + chrono::Duration::days(1)).format("%Y-%m-%d").to_string()
    }

    /// py: telegram.py:6739 `async def _chief_tool(self, name: str, args: dict[str, Any]) -> dict[str, Any]:`
    ///
    /// take_on, jobs_list or close_card. take_on and close_card get this turn's own words (``_turn_said``, taken by code from the
    /// owner's message, never from the model) for its door floor, and a reference to them (the transcript day
    /// and time). Its backbrief, composed by code, goes out here with Change and Drop; the turn then ends
    /// (``_think``). A card filed on an Eve Link app turn is recorded against that turn (``app_filed``).
    ///
    /// Rust: the chief raising is `_tool`'s caught exception, answered here as `_tool` answers it.
    pub(crate) async fn chief_tool(self: &Arc<Self>, name: &str, args: &Obj) -> Obj {
        let chief = self.chief.lock().clone();
        let Some(chief) = chief else {
            return itl_refused(CHIEF_OFF_REASON);
        };
        let reference = itl_local_fmt((self.wall)(), "%Y-%m-%d %H:%M:%S");
        let said = self.st.lock().turn_said.clone();
        let handled = AssertUnwindSafe(chief.handle(name, &Value::Object(args.clone()), &said, &reference, "telegram"))
            .catch_unwind()
            .await;
        let result = match handled {
            Ok(Ok(Value::Object(result))) => result,
            Ok(Ok(_)) => return itl_failed(name),
            Ok(Err(e)) => {
                tracing::error!("telegram: {name} failed ({})", eve_chief::err_name(&e));
                return itl_failed(name);
            }
            Err(_) => {
                tracing::error!("telegram: {name} failed (panic)");
                return itl_failed(name);
            }
        };
        let filed = name == "take_on" && itl_truthy(&result, "ok");
        if filed && itl_truthy(&result, "id") {
            if let Some(turn) = ctx().app {
                // the card this app turn filed (link_chat)
                let mut st = self.st.lock();
                st.app_filed.entry(turn).or_insert_with(|| itl_str(&result, "id"));
                while st.app_filed.len() > MAX_APP_FILED {
                    st.app_filed.shift_remove_index(0);
                }
            }
        }
        if filed && itl_truthy(&result, "backbrief") {
            let cid = itl_py_str(result.get("id"));
            let text = itl_py_str(result.get("backbrief"));
            let buttons =
                [choice("Change", &format!("change {cid}"), "primary", ""), choice("Drop", &format!("drop {cid}"), "danger", "")];
            let sent = self.tell_owner(&text, &text, CHIEF_TITLE, &buttons).await;
            let mut out = result;
            out.insert("sent".into(), json!(sent));
            return out;
        }
        result
    }

    /// py: telegram.py:6758 `def _chief_line(self) -> str:`
    ///
    /// The chief's line for the turn's note (open cards, what waits on the owner), or "".
    pub(crate) fn chief_line(self: &Arc<Self>) -> String {
        let chief = self.chief.lock().clone();
        let Some(chief) = chief else {
            return String::new();
        };
        match std::panic::catch_unwind(AssertUnwindSafe(|| chief.for_turn(None))) {
            Ok(line) => line,
            Err(_) => {
                // a ledger that cannot be read costs the line, never the turn
                tracing::warn!("telegram: the chief's turn line is unavailable (panic)");
                String::new()
            }
        }
    }

    /// py: telegram.py:6768 `def _chief_spoke(self, text: str) -> None:`
    ///
    /// A message the owner sent through the Claude relay: the chief's breakpoint clock and their own actions
    /// on its cards, without the day's brief (that rides eve's own reply, _chief_heard).
    pub(crate) fn chief_spoke(self: &Arc<Self>, text: &str) {
        let chief = self.chief.lock().clone();
        let Some(chief) = chief else {
            return;
        };
        if std::panic::catch_unwind(AssertUnwindSafe(|| chief.owner_spoke(text, None))).is_err() {
            tracing::warn!("telegram: the chief could not hear a relayed message (panic)");
        }
    }

    /// py: telegram.py:6778 `async def _chief_heard(self, chat_id: int, text: str) -> None:`
    ///
    /// The owner wrote: the chief hears it (their action cancels a queued nudge, 90 s later is a moment), and
    /// the day's first message gets the brief after the reply, with any problem the ledger had.
    pub(crate) async fn chief_heard(self: &Arc<Self>, chat_id: i64, text: &str) {
        let chief = self.chief.lock().clone();
        let Some(chief) = chief else {
            return;
        };
        let extra = match std::panic::catch_unwind(AssertUnwindSafe(|| chief.heard(text, None))) {
            Ok(extra) => extra,
            Err(_) => {
                tracing::warn!("telegram: the chief could not hear the turn (panic)");
                return;
            }
        };
        if !extra.is_empty() {
            self.note("eve", &extra, "say", None, None, None, false, None);
            self.say(chat_id, &extra, Some(CHIEF_TITLE), None, &[], 0, None).await;
        }
    }

    /// py: telegram.py:6792 `async def _chief_result(self, card: tuple[str, int], final: str, *, ok: bool, secs: float, steps: Sequence[str]) -> None:`
    ///
    /// A chief's Mac step ended: its closing sentence and the agent's UI states go to the chief, which reads
    /// the evidence (chief_receipt) and sends the receipt. The wrappers hand ``ui_evidence`` on from whoever
    /// ran (app_reflex, chrome_lane, web_reader).
    ///
    /// Rust: the agent's `ui_evidence` is read off its `status()` (`{"ui_evidence": [...]}`); the TaskAgent seam has
    /// no attribute of its own for it.
    pub(crate) async fn chief_result(self: &Arc<Self>, card: (String, i64), final_: &str, ok: bool, secs: f64, steps: &[String]) {
        let chief = self.chief.lock().clone();
        let Some(chief) = chief else {
            return;
        };
        let agent = self.st.lock().agent.clone();
        let evidence: Vec<Value> = agent
            .map(|a| a.status())
            .and_then(|s| s.get("ui_evidence").and_then(Value::as_array).cloned())
            .unwrap_or_default();
        let end = eve_chief::AgentEnd { ok, usd: 0.0, secs: secs.max(0.0), steps: steps.to_vec() };
        let n = card.1.max(0) as usize;
        let ran = AssertUnwindSafe(chief.on_agent_result(&card.0, n, final_, &evidence, end)).catch_unwind().await;
        let why = match ran {
            Ok(Ok(())) => return,
            Ok(Err(e)) => eve_chief::err_name(&e),
            Err(_) => "panic".to_string(),
        };
        // a result the chief cannot take leaves the step for its ceiling
        tracing::warn!("telegram: the chief could not take {} step {}'s result ({why})", card.0, card.1);
    }

    /// py: telegram.py:6810 `def chief_start(self, goal: str, *, card_id: str, n: int, floor: Optional[str] = None) -> dict[str, Any]:`
    ///
    /// The chief's Mac step (chief.StartTask): the same slot, the same agent and the same Stop as a texted task.
    pub fn chief_start(self: &Arc<Self>, goal: &str, card_id: &str, n: i64, floor: Option<&str>) -> Obj {
        let chat = self.st.lock().chat_id;
        let Some(chat) = chat else {
            return itl_refused("there is no owner chat");
        };
        self.start_task(goal, chat, Some((card_id.to_string(), n)), floor, None)
    }

    /// py: telegram.py:6816 `def chief_stop(self, card_id: str, n: int) -> None:`
    ///
    /// Stop the chief's step at its ceiling (chief.StopTask): only that step, never another task.
    pub fn chief_stop(self: &Arc<Self>, card_id: &str, n: i64) {
        let (mine, agent, returned) = {
            let st = self.st.lock();
            let mine = st.chief_task.as_ref().is_some_and(|(c, k)| c == card_id && *k == n);
            (mine, st.agent.clone(), st.task_returned)
        };
        if let Some(agent) = agent {
            if mine && self.task_running() && !returned {
                agent.cancel("the job's step reached its time limit");
            }
        }
    }

    /// py: telegram.py:6822 `def chief_mac_free(self) -> bool:`
    ///
    /// The Mac can take a chief's step: tasks are on, no task runs, and the desk does not have it.
    pub fn chief_mac_free(self: &Arc<Self>) -> bool {
        // a slot that cannot be read is busy
        let busy = std::panic::catch_unwind(AssertUnwindSafe(|| (self.busy)())).unwrap_or(true);
        self.agent_enabled
            && self.agent_factory.is_some()
            && self.st.lock().chat_id.is_some()
            && !self.task_running()
            && !busy
    }

    /// py: telegram.py:6831 `async def chief_ask(self, text: str) -> str:`
    ///
    /// The chief's Go (chief.Ask), with Yes and No. Never while another question waits: its typed yes would
    /// answer this one. The history gets a line naming the card, not the Go's quoted pick. Firm: only a bare yes
    /// or no or a tap answers it, typed or spoken, relay or not; other words go on as a new message.
    ///
    /// Rust: Python's `RuntimeError` is the `Err`, an [`eve_chief::Raised`] named "RuntimeError" (chief.Ask raises).
    pub async fn chief_ask(self: &Arc<Self>, text: &str) -> anyhow::Result<String> {
        let chat = self.st.lock().chat_id;
        let Some(chat) = chat else {
            return Err(anyhow::Error::new(eve_chief::Raised::new("RuntimeError", "there is no owner chat to ask in")));
        };
        if self.awaiting_answer() {
            return Err(anyhow::Error::new(eve_chief::Raised::new(
                "RuntimeError",
                "another question is waiting for the owner",
            )));
        }
        let about = match GO_CARD.captures(text) {
            Some(cid) => format!("(I asked your Go for {}.)", &cid[1]),
            None => "(I asked your Go.)".to_string(),
        };
        Ok(self.ask_user(text, chat, CHIEF_TITLE, Some(&itl_yes_no()), Some(&about), true, false, None, false).await)
    }

    /// py: telegram.py:6844 `async def chief_notify(self, text: str, about: str = "") -> bool:`
    ///
    /// The chief's messages (chief.Notify), through ``tell_owner``, with the buttons the card has now
    /// (chief.choices). The card is the first id in ``about``, a line the chief composes from the card.
    pub async fn chief_notify(self: &Arc<Self>, text: &str, about: &str) -> bool {
        let mut buttons: Vec<Choice> = Vec::new();
        let chief = self.chief.lock().clone();
        if let (Some(cid), Some(chief)) = (ABOUT_CARD.find(about), chief) {
            match std::panic::catch_unwind(AssertUnwindSafe(|| chief.choices(cid.as_str()))) {
                Ok(pairs) => {
                    buttons = pairs
                        .iter()
                        .map(|(label, word)| choice(label, word, if word.starts_with("drop") { "danger" } else { "" }, ""))
                        .collect();
                }
                // no buttons is a plain message
                Err(_) => tracing::warn!("telegram: the chief's buttons are unavailable (panic)"),
            }
        }
        self.tell_owner(text, about, CHIEF_TITLE, &buttons).await
    }

    /// py: telegram.py:6857 `async def _spend_text(self) -> Optional[str]:`
    ///
    /// The spend brief (spend.py, spend_sync.py): today, yesterday, this month, the top features and OpenRouter's
    /// own figure, read off the loop; None when a ledger cannot be read. /spend and spend_report share it.
    pub(crate) async fn spend_text(self: &Arc<Self>) -> Option<String> {
        let read = tokio::task::spawn_blocking(|| -> anyhow::Result<String> {
            let summary = serde_json::to_value(eve_spend::summary(None, 30, None))?;
            let providers = Value::Object(eve_spend_sync::reported(None, None, None));
            Ok(eve_spend::brief(&summary, Some(&providers)))
        })
        .await;
        match read {
            Ok(Ok(text)) => Some(text),
            Ok(Err(e)) => {
                // a ledger that cannot be read is said, never a silence
                tracing::warn!("telegram: the spend brief failed ({e})");
                None
            }
            Err(_) => {
                tracing::warn!("telegram: the spend brief failed (panic)");
                None
            }
        }
    }

    /// py: telegram.py:6868 `async def _spend_now(self, chat_id: int) -> None:`
    ///
    /// /spend: the spend brief as a line. No model call.
    pub(crate) async fn spend_now(self: &Arc<Self>, chat_id: i64) {
        let text = self.spend_text().await;
        let line = text.unwrap_or_else(|| "I couldn't read the spend ledger just now.".to_string());
        self.say(chat_id, &line, None, None, &[], 0, None).await;
    }

    /// py: telegram.py:6873 `async def _stick_link(self, chat_id: int, rotate: bool) -> None:`
    ///
    /// /stick: Eve Link's pairing link, a URL button (it opens a page that hands off to the app).
    pub(crate) async fn stick_link(self: &Arc<Self>, chat_id: i64, rotate: bool) {
        let maker = self.maker.lock().clone();
        let url = match maker {
            Some(maker) => match AssertUnwindSafe(maker.stick_link(rotate)).catch_unwind().await {
                Ok(url) => url,
                Err(_) => {
                    // a token that cannot be written is a line, not a silence
                    tracing::warn!("telegram: /stick failed (panic)");
                    String::new()
                }
            },
            None => String::new(),
        };
        if url.is_empty() {
            self.say(chat_id, eve_stick_link::OFF_LINE, None, None, &[], 0, None).await;
            return;
        }
        let line = if rotate { eve_stick_link::NEW_LINE } else { eve_stick_link::PAIR_LINE };
        let buttons = [(eve_stick_link::UPDATE_TEXT.to_string(), url)];
        if let Err(e) = self.api.send_url_buttons(chat_id, line, &buttons).await {
            tracing::error!("telegram: /stick's button was not sent ({e})");
        }
    }

    /// py: telegram.py:6887 `async def _open_apps(self, chat_id: int) -> None:`
    ///
    /// /apps: the Mini App's Open button, at its address right now.
    pub(crate) async fn open_apps(self: &Arc<Self>, chat_id: i64) {
        let maker = self.maker.lock().clone();
        let url = maker.map(|m| m.home_url()).unwrap_or_default();
        if url.is_empty() {
            self.say(chat_id, APPS_OFF_LINE, None, None, &[], 0, None).await;
            return;
        }
        let buttons = [("Open eve".to_string(), url)];
        if let Err(e) = self.api.send_web_apps(chat_id, APPS_TEXT, &buttons).await {
            tracing::error!("telegram: /apps's button was not sent ({e})");
        }
    }

    /// py: telegram.py:6895 `def _app_tools(self, opened: Optional[set[str]] = None, lent: Optional[set[str]] = None, routed: Optional[bool] = None) -> list[dict[str, Any]]:`
    ///
    /// The apps' tools for this turn: eve's app maker while the Mini App is live, then the direct apps'
    /// (open_app and the opened apps' tools), then Composio's once its session is up (both start on a thread).
    ///
    /// Routed (the toolset router on, 2026-10-05): the opened direct apps' tools, then the open lent toolsets'
    /// (the workspace's Composio tools, Share's), then open_app over every toolset still closed, last:
    /// what changes from turn to turn, or mid-turn, sits at the end of the list so the cached prefix is longest.
    pub(crate) fn app_tools(self: &Arc<Self>, opened: Option<&[String]>, lent: Option<&[String]>, routed: Option<bool>) -> Vec<Obj> {
        let (opened, lent, routed) = {
            let st = self.st.lock();
            let opened: HashSet<String> = match opened {
                Some(o) => o.iter().cloned().collect(),
                None => st.direct_open.clone(),
            };
            let lent: HashSet<String> = match lent {
                Some(l) => l.iter().cloned().collect(),
                None => st.lent.clone(),
            };
            (opened, lent, routed.unwrap_or(st.routed))
        };
        let maker = self.maker.lock().clone();
        let mut out: Vec<Obj> = maker.map(|m| m.tools()).unwrap_or_default();
        if routed {
            let up = self.direct_up();
            if !up.is_empty() {
                if let Some(direct) = &self.direct {
                    out.extend(
                        direct
                            .tools()
                            .into_iter()
                            .filter(|t| {
                                let name = t.get("name").and_then(Value::as_str).unwrap_or("");
                                opened.contains(eve_apps_mcp::split_name(name).0)
                            })
                            .map(|t| itl_o(t)),
                    );
                }
            }
            let lendable = self.lendable();
            for name in [eve_toolset_router::WORKSPACE, eve_toolset_router::SHARE] {
                if lent.contains(name) && lendable.iter().any(|l| l == name) {
                    out.extend(self.lent_tools(name));
                }
            }
            let mut closed: Vec<(&str, &str)> = Vec::new();
            for a in up.iter().filter(|a| !opened.contains(*a)) {
                let label = eve_apps_mcp::SERVERS.get(a.as_str()).map(|s| s.label).unwrap_or("");
                closed.push((a.as_str(), label));
            }
            for n in lendable.iter().filter(|n| !lent.contains(*n)) {
                let label = LENT_LABELS.iter().find(|(k, _)| k == n).map(|(_, v)| *v).unwrap_or("");
                closed.push((n.as_str(), label));
            }
            if let Some(head) = eve_apps_mcp::open_tool_spec(&closed) {
                out.push(itl_o(head));
            }
            return out;
        }
        if let Some(direct) = &self.direct {
            if direct.started() {
                out.extend(direct.tools_for(&opened).into_iter().map(itl_o));
            }
        }
        let Some(apps) = &self.apps else {
            return out;
        };
        if !apps.started() {
            return out;
        }
        match std::panic::catch_unwind(AssertUnwindSafe(|| apps.tools())) {
            Ok(tools) => {
                out.extend(tools);
                out
            }
            Err(_) => {
                // a session that cannot list its tools offers none this turn
                tracing::warn!("telegram: the apps' tools are unavailable (panic)");
                out
            }
        }
    }

    /// py: telegram.py:6929 `def _read_now(self) -> str:`
    ///
    /// What the work running now has read from outside eve: a Work session's tool call counts as having read
    /// what the session wrote (SESSION_READ, ``_SESSION``); anything else is the chat turn's own ``_taint``.
    pub(crate) fn read_now(self: &Arc<Self>) -> String {
        if ctx().session.is_some() {
            return SESSION_READ.to_string();
        }
        self.st.lock().taint.clone()
    }

    /// py: telegram.py:6934 `def _direct_up(self) -> list[str]:`
    ///
    /// The direct apps that are up now (apps_mcp.AppsBridge.apps); none before the bridge has started.
    pub(crate) fn direct_up(self: &Arc<Self>) -> Vec<String> {
        match &self.direct {
            Some(direct) if direct.started() => direct.apps(),
            _ => Vec::new(),
        }
    }

    /// py: telegram.py:6940 `def _composio_up(self) -> bool:`
    pub(crate) fn composio_up(self: &Arc<Self>) -> bool {
        self.apps.as_ref().is_some_and(|a| a.started())
    }

    /// py: telegram.py:6943 `async def _direct_tool(self, name: str, args: dict[str, Any], chat_id: int) -> dict[str, Any]:`
    ///
    /// One direct app call under the owner's policy (apps_mcp.AppsBridge.decide): open_app loads an app for
    /// the turn; reads run, explicit read policies refuse writes, and args-aware policy permits routine
    /// private edits while consequential or unknown writes require a whole-action approval.
    pub(crate) async fn direct_tool(self: &Arc<Self>, name: &str, args: &Obj, chat_id: i64) -> Obj {
        let Some(direct) = self.direct.clone() else {
            return itl_failed(name);
        };
        if name == eve_apps_mcp::OPEN_TOOL {
            let mut st = self.st.lock();
            return direct.open(args, &mut st.direct_open);
        }
        // The model named an up app's tool without opening it: the app opens now, so its real tools are offered
        // from the next round (_think re-offers when _direct_open changes). A tool the app lacks is refused below
        // with the app's real tool names, as a result the next round repairs from, never a failed turn.
        {
            let mut st = self.st.lock();
            direct.ensure_open(name, &mut st.direct_open);
        }
        // Code may verify a private calendar target and pin guest delivery off. Judge and execute
        // exactly those prepared arguments; an unsuccessful read leaves the approval requirement intact.
        let args = direct.prepare(name, args).await;
        let decision = direct.decide(name, Some(&Value::Object(args.clone())));
        if decision.action == eve_apps_mcp::Action::Refuse {
            tracing::info!("telegram: apps: refused {name}");
            return itl_refused(&decision.why);
        }
        // The action policy distinguishes routine changes from consequential ones, with or without outside text.
        if decision.action == eve_apps_mcp::Action::Ask {
            // Firm, and only a bare allow runs it (review, 2026-10-05): a strict ask took "ok, and what's the
            // weather tomorrow?" as its answer with no relay on, and consent.approves read the "ok" as a yes.
            let Some(question) = self.write_question(ItlDescriber::Apps, name, &args, false) else {
                tracing::info!("telegram: apps: {name} refused unasked (too long to show whole)");
                return itl_o(eve_tool_gate::TOO_LONG.clone());
            };
            let outside = !self.read_now().is_empty();
            let answer = self
                .ask_user(&question, chat_id, APP_ASKS_TITLE, Some(&itl_allow_deny()), None, true, false, None, outside)
                .await;
            if eve_consent::bare_decision(&answer) != "allow" {
                return itl_refused("the owner said no; do not retry it");
            }
        }
        direct.execute(name, &args).await
    }

    /// py: telegram.py:6976 `def _write_question(self, describer: Any, name: str, args: dict[str, Any], *, mail: bool = False) -> Optional[str]:`
    ///
    /// Show every argument for a consequential app approval, or refuse it unasked.
    ///
    /// Gmail keeps its larger whole-email allowance: a describer's own (``asks_whole``), or ``mail`` when the
    /// caller knows the call sends email (eve lite's Gmail through Composio). Clean turns use the same full preview
    /// as tainted turns: an approval must never hide an argument behind a clipped label.
    ///
    /// Rust: `describer` is which module describes the call ([`ItlDescriber`]); only this file asks.
    pub(crate) fn write_question(self: &Arc<Self>, describer: ItlDescriber, name: &str, args: &Obj, mail: bool) -> Option<String> {
        let whole_ask = mail || (describer == ItlDescriber::Apps && eve_apps_mcp::asks_whole(name));
        let limit = if whole_ask {
            eve_tool_gate::SHOW_CHARS.max(eve_apps_mcp::WHOLE_ASK_CHARS)
        } else {
            eve_tool_gate::SHOW_CHARS
        };
        let whole = match describer {
            ItlDescriber::Apps => eve_apps_mcp::describe_whole(name, args, limit),
            ItlDescriber::Composio => eve_composio_tools::describe_whole(name, &Value::Object(args.clone()), limit),
        };
        whole.map(|w| format!("{w}\n\nyes / no?"))
    }

    /// py: telegram.py:6989 `async def _share_tool(self, name: str, args: dict[str, Any], chat_id: int) -> dict[str, Any]:`
    ///
    /// share_file / find_contact (share.py): owner only. share_file sends nothing until the owner taps Allow on
    /// the exact file and recipient; the approval token never reaches the model (Share.ask_and_share).
    ///
    /// Rust: Share's `ask` cannot raise, so `_ask_alone`'s `_QuestionWaiting` is a flag the ask sets (answering ""
    /// meanwhile, which Share takes as no answer); the flag set is the `except _QuestionWaiting:` branch.
    pub(crate) async fn share_tool(self: &Arc<Self>, name: &str, args: &Obj, chat_id: i64) -> Obj {
        let Some(share) = self.share.clone() else {
            return itl_refused(crate::share::OFF_REASON);
        };
        if !self.config.owner_ids.contains(&chat_id) {
            return itl_refused("Only a configured owner can share files.");
        }
        if name == "share_file" {
            // any live question, not the slot: the slot names a settled future until its done callback runs, while
            // an older question still waits (Lean ShareGate P3, 2026-10-05)
            if self.live_question().is_some() {
                return itl_refused(QUESTION_WAITING_REASON);
            }
            let waiting = Arc::new(AtomicBool::new(false));
            let (me, flag) = (self.clone(), waiting.clone());
            let ask: crate::share::Ask = Arc::new(move |question: String| {
                let (me, flag) = (me.clone(), flag.clone());
                async move {
                    match me.ask_alone(&question, chat_id, "Share a file", None, false).await {
                        Ok(answer) => answer,
                        Err(_) => {
                            flag.store(true, Ordering::SeqCst);
                            String::new()
                        }
                    }
                }
                .boxed()
            });
            let out = share.ask_and_share(args, ask).await;
            if waiting.load(Ordering::SeqCst) {
                // ask_and_share dropped the pending share on the way out
                return itl_refused(QUESTION_WAITING_REASON);
            }
            return out;
        }
        share.handle(name, args).await
    }

    /// py: telegram.py:7008 `async def _gate(self, name: str, args: dict[str, Any], chat_id: int, read: Optional[str] = None) -> Optional[dict[str, Any]]:`
    ///
    /// The taint gate's question (tool_gate.py): None when the owner said yes and the call may run, else the
    /// call's result. Firm (only a bare yes or a tap answers it, relay or not), asked only while no other question
    /// waits (as _ask_alone: a yes meant for another question must not approve this one). The history gets a
    /// neutral line in place of the question, so no words from the read become eve's own. ``read``: what was read
    /// before the call (a Work session passes SESSION_READ); by default the turn's own taint (``_read_now``).
    pub(crate) async fn gate(self: &Arc<Self>, name: &str, args: &Obj, chat_id: i64, read: Option<&str>) -> Option<Obj> {
        if self.live_question().is_some() {
            return Some(itl_refused(QUESTION_WAITING_REASON));
        }
        let read = match read.filter(|r| !r.is_empty()) {
            Some(read) => read.to_string(),
            None => match self.read_now() {
                now if now.is_empty() => "your request".to_string(),
                now => now,
            },
        };
        let Some(asked) = eve_tool_gate::question(name, args, &read) else {
            // too long to show whole: the owner is never asked to approve words they cannot read (tool_gate.py)
            tracing::info!("telegram: gate: {name} refused unasked (too long to show)");
            return Some(itl_o(eve_tool_gate::TOO_LONG.clone()));
        };
        tracing::info!("telegram: gate: {name} needs confirmation; asking the owner first");
        let about = eve_tool_gate::about(name, &read);
        let answer = self.ask_user(&asked, chat_id, GATE_TITLE, Some(&itl_yes_no()), Some(&about), true, false, None, false).await;
        if eve_consent::bare_decision(&answer) != "allow" {
            tracing::info!("telegram: gate: {name} not approved");
            return Some(itl_o(eve_tool_gate::REFUSED.clone()));
        }
        None
    }

    /// py: telegram.py:7031 `async def _ask_alone(self, question: str, chat_id: int, *, title: str, about: Optional[str] = None, read_all: bool = False) -> str:`
    ///
    /// A firm Allow/Deny for an outward send, asked only while no other question waits (as chief_ask).
    /// Review, 2026-10-05: a task's "Reply yes or no." was open when an Allow question opened; the
    /// newest question takes a typed yes, so the owner's yes for the task approved the other one. Checked again
    /// here, after the preview's awaits, as the caller's check may be stale by now. Any live question counts, not
    /// only the slot ``_awaiting_answer`` reads, which lags a loop turn behind a settled question (Lean ShareGate
    /// P3, 2026-10-05).
    ///
    /// Rust: Python's `raise _QuestionWaiting()` is the `Err`.
    pub(crate) async fn ask_alone(self: &Arc<Self>, question: &str, chat_id: i64, title: &str, about: Option<&str>, read_all: bool) -> Result<String, _QuestionWaiting> {
        if self.live_question().is_some() {
            return Err(_QuestionWaiting { message: String::new() });
        }
        Ok(self.ask_user(question, chat_id, title, Some(&itl_allow_deny()), about, true, read_all, None, false).await)
    }

    /// py: telegram.py:7044 `async def _app_tool(self, name: str, args: dict[str, Any], chat_id: int) -> dict[str, Any]:`
    ///
    /// One Composio call under args-aware policy: reads and permitted routine edits run; explicit
    /// read policy refuses writes, and consequential or unknown writes need the owner's whole-action yes.
    pub(crate) async fn app_tool(self: &Arc<Self>, name: &str, args: &Obj, chat_id: i64) -> Obj {
        let Some(apps) = self.apps.clone() else {
            return itl_failed(name);
        };
        let as_value = Value::Object(args.clone());
        let web = eve_composio_tools::web_toolkit_slugs(name, &as_value);
        if !web.is_empty() {
            // The public web is eve's own web_search, one round; through Composio it cost the office-hours
            // turns of 2026-09-30 every round they had (composio_tools.TOOLKITS, WEB_TOOLKITS).
            tracing::info!("telegram: apps: refused web toolkit {}; web_search instead", web.join(", "));
            return itl_refused(&format!(
                "{} is not used here: look the web up with {}, which answers in one step.",
                web.join(", "),
                eve_web::websearch::TOOL_NAME
            ));
        }
        let lite_on = lite(self.environ.as_ref());
        let decision = if lite_on {
            // eve lite: the owner's own Gmail and Calendar are reached here, and a connection change asks first
            eve_composio_tools::decide_lite(name, &as_value, Some(&self.app_policy))
        } else {
            eve_composio_tools::decide(name, &as_value, Some(&self.app_policy), true)
        };
        if decision.action.as_str() == "refuse" {
            tracing::info!("telegram: apps: refused {name} ({})", decision.slugs.join(", "));
            return itl_refused(&decision.why);
        }
        // The action policy owns confirmation; outside text does not add another question for routine edits.
        if decision.action.as_str() == "ask" {
            // Firm, and only a bare allow runs it, as the direct apps' ask (review, 2026-10-05). eve lite's email
            // goes only through here, so a send is shown whole with Gmail's own allowance.
            let mail = lite_on && eve_composio_tools::sends_mail(name, &as_value);
            let Some(question) = self.write_question(ItlDescriber::Composio, name, args, mail) else {
                tracing::info!("telegram: apps: {name} refused unasked (too long to show whole)");
                return itl_o(eve_tool_gate::TOO_LONG.clone());
            };
            let outside = !self.read_now().is_empty();
            let answer = self
                .ask_user(&question, chat_id, APP_ASKS_TITLE, Some(&itl_allow_deny()), None, true, false, None, outside)
                .await;
            if eve_consent::bare_decision(&answer) != "allow" {
                return itl_refused("the owner said no; do not retry it");
            }
        }
        apps.execute(name, args).await
    }
}

#[cfg(test)]
mod itl_chief_tests {
    use super::*;

    #[test]
    fn allow_deny_and_yes_no_are_pythons() {
        let words = |cs: Vec<Choice>| cs.into_iter().map(|c| (c.label, c.value, c.style, c.done)).collect::<Vec<_>>();
        let s = |a: &str, b: &str, c: &str, d: &str| (a.to_string(), b.to_string(), c.to_string(), d.to_string());
        assert_eq!(words(itl_allow_deny()), vec![s("Allow", "yes", "success", "Allowed."), s("Deny", "no", "danger", "Denied.")]);
        assert_eq!(words(itl_yes_no()), vec![s("Yes", "yes", "success", "Yes."), s("No", "no", "danger", "No.")]);
    }

    #[test]
    fn the_go_names_its_card_only_at_the_start_and_an_alert_names_its_first() {
        assert_eq!(&GO_CARD.captures("c12, Order the desk: order it.\nGo?").unwrap()[1], "c12");
        assert!(GO_CARD.captures("Order c12, the desk").is_none()); // re.match: at the start only
        assert!(GO_CARD.captures("c12 Order").is_none());
        assert_eq!(ABOUT_CARD.find("Done: c3 and c4").unwrap().as_str(), "c3");
        assert!(ABOUT_CARD.find("abc3d").is_none());
    }

    #[test]
    fn tomorrow_is_the_local_day_after() {
        use chrono::TimeZone;
        let noon = chrono::Local.with_ymd_and_hms(2026, 9, 30, 12, 0, 0).earliest().unwrap().timestamp() as f64;
        assert_eq!(TelegramInlet::tomorrow_of(noon), "2026-10-01");
        let late = chrono::Local.with_ymd_and_hms(2026, 12, 31, 23, 59, 0).earliest().unwrap().timestamp() as f64;
        assert_eq!(TelegramInlet::tomorrow_of(late), "2027-01-01");
    }
}
