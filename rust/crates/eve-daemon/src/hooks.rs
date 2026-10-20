//! Claude Code's PreToolUse and PermissionRequest hooks: the matchers, the audit log, the command-risk gate (Jev,
//! shadow or enforced), the owner's answer over the text door, and Read's in-cwd policy.

use std::sync::Arc;

use serde_json::{json, Map, Value};

use crate::daemon::Daemon;
use crate::ipc::{get_truthy, py_str, raised, round3, str_or};
use crate::seams::{CommandAsker, Obj};

fn ok() -> Obj {
    let mut m = Map::new();
    m.insert("ok".into(), Value::Bool(true));
    m
}

fn ok_decision(decision: &str) -> Obj {
    let mut m = ok();
    m.insert("decision".into(), Value::String(decision.to_string()));
    m
}

fn chars_prefix(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// `{"verdict", "why", **{k: round(v, 3) for k, v in nouls}, "ms": round(ms)}`: the audit's jev field.
fn jev_fields(verdict: &eve_typed_ask::CommandVerdict) -> Obj {
    let mut m = Map::new();
    m.insert("verdict".into(), Value::String(verdict.decision.clone()));
    m.insert("why".into(), Value::String(verdict.why.clone()));
    for (k, v) in verdict.answer.nouls() {
        m.insert(k.to_string(), serde_json::Number::from_f64(round3(v)).map(Value::Number).unwrap_or(Value::Null));
    }
    // round(ms): an int, ties to even
    let ms = verdict.answer.ms.round_ties_even();
    m.insert("ms".into(), if ms.is_finite() { json!(ms as i64) } else { Value::Null });
    m
}

/// The audit fields every decision of one call shares (`audit_kwargs`).
#[derive(Clone)]
struct AuditKw {
    session_id: String,
    tool_name: String,
    hint: String,
    matcher: String,
}

impl AuditKw {
    fn record<'a>(&'a self, decision: Option<&'a str>, source: &'a str, jev: Option<&'a Obj>) -> eve_audit::Record<'a> {
        eve_audit::Record {
            session_id: &self.session_id,
            tool_name: &self.tool_name,
            hint: &self.hint,
            matcher: &self.matcher,
            decision,
            source,
            elapsed_s: None,
            jev,
        }
    }
}

/// The env lookups `jev.route_config(os.environ)` and `jev.timeout_from_env(os.environ)` make, from the daemon's.
struct DaemonEnv<'a>(Option<&'a crate::seams::Environ>);

impl eve_jev::Env for DaemonEnv<'_> {
    fn var(&self, name: &str) -> Option<String> {
        crate::seams::env_get(self.0, name)
    }
}

impl Daemon {
    /// `_handle_permission_request(req)`: the PermissionRequest hook's decision.
    ///
    /// A permission dialog Claude Code is about to show (hooks/permission_request.py). With the relay on,
    /// the owner is on the phone and nobody is at the dialog: ask there, yes/no. No answer, relay off, or a
    /// question dialog (AskUserQuestion is answered by typing the option, never by a hook): no decision.
    pub async fn handle_permission_request(self: &Arc<Self>, req: Obj) -> anyhow::Result<Obj> {
        let tool_name = str_or(&req, "tool_name", "tool");
        let Some(inlet) = self.relay().filter(|_| tool_name != "AskUserQuestion") else {
            return Ok(ok());
        };
        let hint = str_or(&req, "hint", "");
        let decision = inlet.decide_permission(&tool_name, &hint, &str_or(&req, "cwd", ""), true).await;
        let answered = decision.as_deref().filter(|d| matches!(*d, "allow" | "deny"));
        tracing::info!(
            "permissionrequest for {} ({}): {}",
            tool_name,
            chars_prefix(&hint, 60),
            match answered {
                Some(d) => format!("answered from Telegram → {d}"),
                None => "no answer → the dialog".to_string(),
            }
        );
        if let Some(d) = answered {
            let session_id = str_or(&req, "session_id", "unknown");
            self.audit.record(eve_audit::Record {
                session_id: &session_id,
                tool_name: &tool_name,
                hint: &hint,
                matcher: "permission_request",
                decision: Some(d),
                source: "telegram",
                elapsed_s: None,
                jev: None,
            });
            return Ok(ok_decision(d));
        }
        Ok(ok())
    }

    /// `_handle_pretooluse(req)`: the PreToolUse hook's decision: classify, audit, ask the owner when it asks.
    pub async fn handle_pretooluse(self: &Arc<Self>, req: Obj) -> anyhow::Result<Obj> {
        let Some(tool_use_id) = req.get("tool_use_id").and_then(Value::as_str).filter(|t| !t.is_empty()) else {
            let mut m = Map::new();
            m.insert("ok".into(), Value::Bool(false));
            m.insert("error".into(), Value::String("missing tool_use_id".into()));
            return Ok(m);
        };
        let tool_use_id = tool_use_id.to_string();
        let session_id = str_or(&req, "session_id", "unknown");
        let tool_name = str_or(&req, "tool_name", "tool");
        let hint = str_or(&req, "hint", "");
        self.ensure_session(&req);
        self.state.lock().note_tool(&session_id, &tool_name);

        // A question for the owner: relayed with its numbered options, and nothing else. It is answered by
        // typing the option into the dialog, never by a decision here — and its text must not meet the
        // command matchers below (a question that mentions "rm" is not an rm).
        if tool_name == "AskUserQuestion" {
            if let Some(relay) = self.relay() {
                relay.relay_tool_call(&tool_name, &hint);
            }
            return Ok(ok());
        }

        // Read tool takes its own path: in-cwd reads never prompt; out-of-cwd reads
        // are allowed by the relay, else Claude Code's own flow. See read_policy.py.
        if tool_name == "Read" {
            return self.handle_read_pretooluse(req, &tool_use_id, &session_id, &hint).await;
        }

        // Smart matcher: classify trivial / risky commands first.
        // auto_allow → approve immediately, no prompt anywhere (keeps ls/cat fast).
        // always_ask → asked on the phone when the relay is on, else Claude Code's flow.
        // default    → no decision, let Claude Code's native permission flow run.
        let decision_class = eve_matchers::classify_command(&hint, &self.matchers).as_str();
        let kw = AuditKw {
            session_id: session_id.clone(),
            tool_name: tool_name.clone(),
            hint: hint.clone(),
            matcher: decision_class.to_string(),
        };
        let relay = self.relay();
        if let Some(relay) = &relay {
            relay.relay_tool_call(&tool_name, &hint); // only a question for the owner leaves; calls stay gray
        }
        if decision_class == "allow" {
            tracing::info!("pretooluse for {} ({}): auto_allow match → allow", tool_name, chars_prefix(&hint, 60));
            self.audit.record(kw.record(Some("allow"), "auto_allow", None));
            return Ok(ok_decision("allow"));
        }

        // "claude on" (telegram.py): the owner is on the phone, so a prompt on the Mac has nobody at it.
        // The relay is bypass (owner, 2026-09-21): a call is allowed here without asking, as
        // bypassPermissions would. The owner's own always_ask list (rm, sudo: matchers.py) is the one
        // exception, asked in the chat as a yes/no that only the owner's next message answers; silence
        // there defers to Claude Code's own flow, never denies. EVE_TELEGRAM_ASK=1 asks every call.
        // Decided before the robot check: the phone is for when the owner is away.
        let inlet = relay;
        let mode = str_or(&req, "permission_mode", "");
        let cwd = str_or(&req, "cwd", "");
        let bypass = matches!(mode.as_str(), "bypassPermissions" | "dontAsk");
        // The Auto Mode gate (typed_ask.py, the Jev Engineering article): a Bash command the regex list did
        // not stop is judged by Jev before it runs, in EVERY permission mode, because bypass is exactly when
        // nothing else would stop it. "shadow" only logs the verdict; "ask" turns a risky one into the
        // phone's yes/no; "off" is the relay of 2026-09-21. A safe verdict or an error changes nothing.
        let risk = if inlet.is_some() && tool_name == "Bash" && !hint.is_empty() { self.command_risk() } else { None };
        if let (Some((risk_mode, ask_cmd)), Some(inlet)) = (risk.filter(|_| decision_class != "ask"), &inlet) {
            if risk_mode == "shadow" {
                let me = self.clone();
                let (tool, h, c, kw) = (tool_name.clone(), hint.clone(), cwd.clone(), kw.clone());
                self.spawn("command-risk-shadow", async move {
                    let audit = kw.record(None, "", None);
                    me.shadow_command_risk(ask_cmd, &tool, &h, &c, audit).await;
                });
            } else if risk_mode == "ask" {
                let (t, h, c) = (tool_name.clone(), hint.clone(), cwd.clone());
                let verdict = tokio::task::spawn_blocking(move || ask_cmd(&t, &h, &c))
                    .await
                    .map_err(|e| raised("RuntimeError", e.to_string()))?;
                let jev = jev_fields(&verdict);
                if verdict.decision == "risky" {
                    tracing::info!(
                        "pretooluse for {} ({}): Jev says risky ({}) → asking the phone",
                        tool_name,
                        chars_prefix(&hint, 60),
                        verdict.why
                    );
                    let asked = format!("{hint} [Jev: {}]", verdict.why);
                    let decision = inlet.decide_permission(&tool_name, &asked, &cwd, true).await;
                    if let Some(d) = decision.as_deref().filter(|d| matches!(*d, "allow" | "deny")) {
                        self.audit.record(kw.record(Some(d), "telegram", Some(&jev)));
                        return Ok(ok_decision(d));
                    }
                    tracing::info!(
                        "pretooluse for {} ({}): no answer from Telegram → defer",
                        tool_name,
                        chars_prefix(&hint, 60)
                    );
                    self.audit.record(kw.record(None, "jev_risky_deferred", Some(&jev)));
                    return Ok(ok());
                }
                let source = if verdict.decision == "safe" { "jev_safe" } else { "jev_error" };
                if verdict.decision != "safe" {
                    tracing::warn!(
                        "pretooluse for {}: the risk model failed ({}); allowed as before",
                        tool_name,
                        verdict.why
                    );
                }
                if bypass {
                    self.audit.record(kw.record(None, source, Some(&jev)));
                    return Ok(ok()); // bypass: Claude Code allows on its own
                }
                self.audit.record(kw.record(Some("allow"), source, Some(&jev)));
                return Ok(ok_decision("allow"));
            }
        }
        if let Some(inlet) = inlet.filter(|_| !bypass) {
            let asks_all = inlet.ask_permissions();
            if decision_class == "ask" || asks_all {
                let decision = inlet.decide_permission(&tool_name, &hint, &cwd, true).await;
                if let Some(d) = decision.as_deref().filter(|d| matches!(*d, "allow" | "deny")) {
                    tracing::info!(
                        "pretooluse for {} ({}): answered from Telegram → {}",
                        tool_name,
                        chars_prefix(&hint, 60),
                        d
                    );
                    self.audit.record(kw.record(Some(d), "telegram", None));
                    return Ok(ok_decision(d));
                }
                tracing::info!("pretooluse for {} ({}): no answer from Telegram → defer", tool_name, chars_prefix(&hint, 60));
            } else {
                tracing::info!("pretooluse for {} ({}): claude relay on → allow", tool_name, chars_prefix(&hint, 60));
                self.audit.record(kw.record(Some("allow"), "telegram_relay", None));
                return Ok(ok_decision("allow"));
            }
        }

        // If BLE isn't connected, skip the round-trip and return no decision so
        // Claude Code's normal flow runs (respects user's auto/allow settings).
        if !self.ble.connected() {
            // "stick", not "ble": self.ble duck-types over serial too, and the
            // 2026-08-06 log read as a BLE problem when the board was simply
            // off the USB bus.
            tracing::info!("pretooluse for {}: stick not connected, deferring to default flow", tool_name);
            self.audit.record(kw.record(None, "ble_disconnected", None));
            return Ok(ok());
        }

        // The board has no decision surface any more — the swipe card is
        // gone, so nothing can answer a prompt. Always defer to Claude Code's
        // own permission flow rather than block the tool call waiting on a
        // button that no longer exists. Placed after auto_allow so the matcher
        // fast paths keep working.
        tracing::info!("pretooluse for {} ({}): no card → defer to default", tool_name, chars_prefix(&hint, 60));
        self.audit.record(kw.record(None, "defer", None));
        Ok(ok())
    }

    /// `_command_risk()`: (mode, asker) for the command-risk gate, made once; None when off.
    ///
    /// EVE_COMMAND_RISK is off | shadow | ask (typed_ask.COMMAND_RISK_DEFAULT when unset); a route
    /// without a key is "off" with one log line, never an error on the hook path.
    pub fn command_risk(&self) -> Option<(String, CommandAsker)> {
        if let Some(cached) = self.st.lock().command_risk.clone() {
            return cached;
        }
        let raw = self
            .env("EVE_COMMAND_RISK")
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| eve_typed_ask::COMMAND_RISK_DEFAULT.to_string());
        let mut raw = raw.trim().to_lowercase();
        if !eve_typed_ask::COMMAND_RISK_MODES.contains(&raw.as_str()) {
            let modes = eve_typed_ask::COMMAND_RISK_MODES
                .iter()
                .map(|m| eve_motion::py_repr_str(m))
                .collect::<Vec<_>>()
                .join(", ");
            tracing::warn!(
                "command risk: EVE_COMMAND_RISK={} is not one of ({}); off",
                eve_motion::py_repr_str(&raw),
                modes
            );
            raw = "off".to_string();
        }
        let mut built: Option<(String, CommandAsker)> = None;
        if raw != "off" {
            let env = DaemonEnv(self.environ());
            match eve_jev::route_config(&env, "") {
                Ok((url, key, model)) => {
                    let seconds = eve_jev::timeout_from_env(&env);
                    let predict = eve_jev::make_predict(
                        &url,
                        &key,
                        &model,
                        eve_jev::PredictOptions { timeout_s: seconds, ..Default::default() },
                    )
                    .anyhow_fn();
                    let asker = eve_typed_ask::make_jev_command_asker(predict, eve_typed_ask::perf_counter, None);
                    let asker: CommandAsker = Arc::new(asker);
                    built = Some((raw.clone(), asker));
                    tracing::info!(
                        "command risk: {} (Jev {} judges a relayed Bash command: destroys, escapes, publishes, \
                         secrets; obvious secrets redacted first)",
                        raw,
                        model
                    );
                }
                Err(e) => tracing::warn!("command risk: off (JevError: {})", e),
            }
        } else {
            tracing::info!("command risk: off (the relay allows what the regex list does not stop)");
        }
        self.st.lock().command_risk = Some(built.clone());
        built
    }

    /// `_shadow_command_risk(ask_cmd, tool_name, hint, cwd, audit_kwargs)`: the verdict logged beside the regex
    /// class and never acted on.
    pub async fn shadow_command_risk(
        self: &Arc<Self>,
        ask_cmd: CommandAsker,
        tool_name: &str,
        hint: &str,
        cwd: &str,
        audit: eve_audit::Record<'_>,
    ) {
        let (t, h, c) = (tool_name.to_string(), hint.to_string(), cwd.to_string());
        let verdict = match tokio::task::spawn_blocking(move || ask_cmd(&t, &h, &c)).await {
            Ok(v) => v,
            Err(e) => {
                // shadow: nothing may reach the hook path
                tracing::warn!("command risk (shadow): {}", if e.is_panic() { "PanicException" } else { "CancelledError" });
                return;
            }
        };
        let jev = jev_fields(&verdict);
        self.audit.record(eve_audit::Record {
            decision: None,
            source: "jev_shadow",
            jev: Some(&jev),
            ..audit
        });
    }

    /// `_ensure_session(req)`: a hook from a session the daemon has not seen starts it.
    ///
    /// SessionStart is the only hook that normally creates a session, so a
    /// daemon restart leaves every *already-running* session invisible until
    /// the human restarts it too — the monitor then shows an empty agent list
    /// while agents are plainly working. Tool hooks carry both ids we need,
    /// and session_start is create-if-absent, so adopting the session here is
    /// free and makes the list self-heal within one tool call.
    pub fn ensure_session(&self, req: &Obj) {
        let Some(session_id) = req.get("session_id").and_then(Value::as_str).filter(|s| !s.is_empty()) else {
            return;
        };
        let cwd = get_truthy(req, "cwd").map(py_str);
        let mut state = self.state.lock();
        state.session_start(session_id, None, cwd.as_deref());
        // session_start is create-if-absent, so it will NOT fill in a cwd it
        // missed. posttooluse doesn't carry one, so whichever hook happens to
        // fire first decides the row's name forever — and losing the race
        // leaves the agent labelled with a session-id prefix instead of its
        // repo. Backfill the first cwd we actually see.
        if let (Some(sess), Some(cwd)) = (state.sessions.get_mut(session_id), cwd) {
            if sess.cwd.as_deref().unwrap_or("").is_empty() {
                sess.cwd = Some(cwd);
            }
        }
    }

    /// `_handle_read_pretooluse(req, tool_use_id, session_id, hint)`: Read prompts: out-of-cwd reads are carded;
    /// approval grants the whole folder.
    ///
    /// Read-tool prompts: card out-of-cwd reads; approval grants the whole
    /// enclosing scope (git repo or parent dir) for the daemon's lifetime.
    /// See read_policy.py for why scope-not-file is the point.
    pub async fn handle_read_pretooluse(
        self: &Arc<Self>,
        req: Obj,
        _tool_use_id: &str,
        session_id: &str,
        hint: &str,
    ) -> anyhow::Result<Obj> {
        let path = hint;
        let cwd = str_or(&req, "cwd", "");
        let kw = AuditKw {
            session_id: session_id.to_string(),
            tool_name: "Read".into(),
            hint: path.to_string(),
            matcher: "read".into(),
        };
        // In-cwd reads never prompt anywhere; defer without noise.
        if path.is_empty() || eve_read_policy::is_within(path, &cwd) {
            return Ok(ok());
        }
        let mode = str_or(&req, "permission_mode", "");
        if self.relay().is_some() && !matches!(mode.as_str(), "bypassPermissions" | "dontAsk") {
            // The Claude relay is bypass (see _handle_pretooluse): a read is allowed, not carded.
            tracing::info!("read {}: claude relay on → allow", path);
            self.audit.record(kw.record(Some("allow"), "telegram_relay", None));
            return Ok(ok_decision("allow"));
        }
        if !self.ble.connected() {
            self.audit.record(kw.record(None, "ble_disconnected", None));
            return Ok(ok());
        }
        // No card to show it on — defer to Claude Code's own flow.
        self.audit.record(kw.record(None, "defer", None));
        Ok(ok())
    }
}
