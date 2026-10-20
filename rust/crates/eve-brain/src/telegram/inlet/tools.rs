//! A port of `telegram.py`, lines 5853-6307: tool dispatch and the chat's own tool handlers.
//!
//! TOOL_HANDLERS (telegram.py:7575, below the class) is the first `match` of [`TelegramInlet::tool`]: the fixed
//! tools by name, checked before the owner's apps and the second brain, exactly as Python's table is.
//!
//! A Python handler that raised is reported by `_tool`, never raised: here a handler's `Err` (an upstream crate's
//! `anyhow::Error`) and a handler's panic are that exception, caught in the same place and answered the same way.

use std::panic::AssertUnwindSafe;

use futures::FutureExt;

#[allow(unused_imports)]
use crate::prelude::*;
#[allow(unused_imports)]
use crate::telegram::*;

// ---- Python's own small semantics, shared by this unit's three files (prefixed: the inlet's modules are globbed) ----

/// `str(args.get(key) or "")`: a falsy value is "", a string is itself, anything else as Python prints it.
pub(crate) fn itl_str(args: &Obj, key: &str) -> String {
    itl_str_of(args.get(key))
}

/// `str(value or "")` for one value.
pub(crate) fn itl_str_of(value: Option<&Value>) -> String {
    if !itl_truthy_of(value) {
        return String::new();
    }
    itl_py_str(value)
}

/// `str(d.get(key))`: "None" for a missing key.
pub(crate) fn itl_py_str(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => s.clone(),
        Some(other) => itl_repr(other),
        None => "None".into(),
    }
}

/// `repr(value)` for a JSON value, as Python prints the dict it was parsed into.
pub(crate) fn itl_repr(value: &Value) -> String {
    match value {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) => match (n.as_i64(), n.as_u64()) {
            (Some(i), _) => i.to_string(),
            (None, Some(u)) => u.to_string(),
            _ => itl_float_repr(n.as_f64().unwrap_or(0.0)),
        },
        Value::String(s) => {
            let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
            let mut out = String::new();
            out.push(quote);
            for c in s.chars() {
                match c {
                    '\\' => out.push_str("\\\\"),
                    '\n' => out.push_str("\\n"),
                    '\r' => out.push_str("\\r"),
                    '\t' => out.push_str("\\t"),
                    c if c == quote => {
                        out.push('\\');
                        out.push(c);
                    }
                    c if !itl_printable(c) => {
                        let n = c as u32;
                        out.push_str(&if n < 0x100 {
                            format!("\\x{n:02x}")
                        } else if n < 0x10000 {
                            format!("\\u{n:04x}")
                        } else {
                            format!("\\U{n:08x}")
                        });
                    }
                    c => out.push(c),
                }
            }
            out.push(quote);
            out
        }
        Value::Array(a) => format!("[{}]", a.iter().map(itl_repr).collect::<Vec<_>>().join(", ")),
        Value::Object(o) => format!(
            "{{{}}}",
            o.iter().map(|(k, v)| format!("{}: {}", itl_repr(&Value::String(k.clone())), itl_repr(v))).collect::<Vec<_>>().join(", ")
        ),
    }
}

/// `str.isprintable()` for one character, as `repr` escapes what is not: the controls (Cc), the format characters
/// (Cf), the separators other than the space (Zs, Zl, Zp) and the private use areas (Co). Unassigned code points
/// (Cn), which Python also escapes, are kept as they are: this table does not know them.
fn itl_printable(c: char) -> bool {
    let n = c as u32;
    !matches!(n,
        0x00..=0x1f | 0x7f..=0xa0 | 0xad | 0x600..=0x605 | 0x61c | 0x6dd | 0x70f | 0x1680 | 0x180e
        | 0x2000..=0x200f | 0x2028..=0x202f | 0x205f..=0x2064 | 0x2066..=0x206f | 0x3000 | 0xe000..=0xf8ff
        | 0xfeff | 0xfff9..=0xfffb | 0x110bd | 0x1bca0..=0x1bca3 | 0x1d173..=0x1d17a | 0xe0001 | 0xe0020..=0xe007f
        | 0xf0000..=0xffffd | 0x100000..=0x10fffd)
}

/// `repr(float)`: the shortest round-trip digits, with Python's ".0" and exponent forms.
pub(crate) fn itl_float_repr(x: f64) -> String {
    if x.is_nan() {
        return "nan".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf".into() } else { "-inf".into() };
    }
    let abs = x.abs();
    if abs != 0.0 && !(1e-4..1e16).contains(&abs) {
        let s = format!("{x:e}");
        let (mantissa, exp) = s.split_once('e').unwrap_or((&s, "0"));
        let exp: i32 = exp.parse().unwrap_or(0);
        return format!("{mantissa}e{}{:02}", if exp < 0 { '-' } else { '+' }, exp.abs());
    }
    let s = format!("{x}");
    if s.contains('.') { s } else { format!("{s}.0") }
}

/// Python's truthiness of one value.
pub(crate) fn itl_truthy_of(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// Python's truthiness of `d.get(key)`.
pub(crate) fn itl_truthy(d: &Obj, key: &str) -> bool {
    itl_truthy_of(d.get(key))
}

/// `round(x, ndigits)`, half to even on the decimal digits as Python's is for the values a ledger holds.
pub(crate) fn itl_round(x: f64, ndigits: i32) -> f64 {
    let text = format!("{x:.prec$}", prec = ndigits.max(0) as usize);
    text.parse().unwrap_or(x)
}

/// `isinstance(raw, int) and not isinstance(raw, bool)`: the value as an int.
pub(crate) fn itl_int(raw: Option<&Value>) -> Option<i64> {
    match raw {
        Some(Value::Number(n)) => n.as_i64(),
        _ => None,
    }
}

/// `str.strip()` / `str.rstrip()` as Python strips: Unicode whitespace and the ASCII separators 0x1c-0x1f.
pub(crate) fn itl_is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

pub(crate) fn itl_strip(s: &str) -> &str {
    s.trim_matches(itl_is_space)
}

pub(crate) fn itl_rstrip(s: &str) -> &str {
    s.trim_end_matches(itl_is_space)
}

/// `s.rstrip(chars)`.
pub(crate) fn itl_rstrip_chars<'a>(s: &'a str, chars: &str) -> &'a str {
    s.trim_end_matches(|c: char| chars.contains(c))
}

/// Python's `str.lower()`, close enough for the code words it is used on.
pub(crate) fn itl_lower(s: &str) -> String {
    s.to_lowercase()
}

/// A handler's dict: a JSON object as it is, anything else a failure of `name`.
pub(crate) fn itl_obj(value: Value, name: &str) -> Obj {
    match value {
        Value::Object(map) => map,
        _ => itl_failed(name),
    }
}

/// `{"ok": False, "reason": f"{name} failed"}`.
pub(crate) fn itl_failed(name: &str) -> Obj {
    let mut out = Obj::new();
    out.insert("ok".into(), Value::Bool(false));
    out.insert("reason".into(), Value::String(format!("{name} failed")));
    out
}

/// `{"ok": False, "reason": reason}`.
pub(crate) fn itl_refused(reason: &str) -> Obj {
    let mut out = Obj::new();
    out.insert("ok".into(), Value::Bool(false));
    out.insert("reason".into(), Value::String(reason.to_string()));
    out
}

/// A `json!` object as an [`Obj`].
pub(crate) fn itl_o(value: Value) -> Obj {
    match value {
        Value::Object(map) => map,
        _ => Obj::new(),
    }
}

/// `datetime.fromtimestamp(t).astimezone().strftime(fmt)`.
pub(crate) fn itl_local_fmt(t: f64, fmt: &str) -> String {
    use chrono::TimeZone;
    let secs = t.floor();
    let nanos = ((t - secs) * 1e9).round().clamp(0.0, 999_999_999.0) as u32;
    match chrono::Local.timestamp_opt(secs as i64, nanos) {
        chrono::LocalResult::Single(at) | chrono::LocalResult::Ambiguous(at, _) => at.format(fmt).to_string(),
        chrono::LocalResult::None => String::new(),
    }
}

/// A panic's message, as `str(exc)` would name it in a log line.
pub(crate) fn itl_panic_name(panic: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = panic.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = panic.downcast_ref::<String>() {
        s.clone()
    } else {
        "panic".into()
    }
}

/// `a is b` for two task agents (None is None).
pub(crate) fn itl_same_agent(a: &Option<Arc<dyn TaskAgent>>, b: &Option<Arc<dyn TaskAgent>>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => std::ptr::eq(Arc::as_ptr(a) as *const (), Arc::as_ptr(b) as *const ()),
        _ => false,
    }
}

/// A chat log line as the inlet's other code reads one (`ln.seq`, `ln.t`, `ln.who`, `ln.text`, ...): every field
/// of [`eve_chat_log::Line`], by its Python name. Python's `id(ln)` is its `seq`.
pub(crate) fn itl_line_value(ln: &eve_chat_log::Line) -> Value {
    let mut m = Obj::new();
    m.insert("seq".into(), json!(ln.seq));
    m.insert("t".into(), json!(ln.t));
    m.insert("chat".into(), json!(ln.chat));
    m.insert("who".into(), json!(ln.who));
    m.insert("text".into(), json!(ln.text));
    m.insert("kind".into(), json!(ln.kind));
    m.insert("conv".into(), json!(ln.conv));
    m.insert("boot".into(), json!(ln.boot));
    m.insert("key".into(), json!(ln.key));
    m.insert("quotes".into(), json!(ln.quotes));
    m.insert("outside".into(), json!(ln.outside));
    m.insert("extra".into(), Value::Object(ln.extra.clone()));
    Value::Object(m)
}

fn line_str<'a>(ln: &'a Value, key: &str) -> &'a str {
    ln.get(key).and_then(Value::as_str).unwrap_or("")
}

fn line_t(ln: &Value) -> f64 {
    ln.get("t").and_then(Value::as_f64).unwrap_or(0.0)
}

fn line_seq(ln: &Value) -> i64 {
    ln.get("seq").and_then(Value::as_i64).unwrap_or(0)
}

/// `text if len(text) <= RECENT_MESSAGES_LINE_CHARS else text[:RECENT_MESSAGES_LINE_CHARS - 1].rstrip() + "…"`.
fn clip_line(text: &str) -> String {
    if text.chars().count() <= RECENT_MESSAGES_LINE_CHARS {
        return text.to_string();
    }
    let head: String = text.chars().take(RECENT_MESSAGES_LINE_CHARS - 1).collect();
    format!("{}…", itl_rstrip(&head))
}

/// `{"user": "owner", "eve": "you (eve)", "event": "note"}[who]`.
fn who_word(who: &str) -> &'static str {
    match who {
        "user" => "owner",
        "eve" => "you (eve)",
        _ => "note",
    }
}

const WHEN_FMT: &str = "%a %d %b %H:%M";

impl TelegramInlet {
    /// py: telegram.py:5853 `async def _tool(self, name: str, args: dict[str, Any], chat_id: int) -> dict[str, Any]:`
    ///
    /// One tool call from the text brain. The fixed tools are a table (TOOL_HANDLERS, below the class);
    /// then, in this order, the owner's apps (known only at run time), the second brain, and think_hard for
    /// anything else. A handler that raises is reported, never raised.
    ///
    /// eve lite refuses every tool it does not have before any handler runs (LITE_TOOL_OFF): parse_response takes
    /// any of eve's own names, offered this turn or not, and the screen, the camera, the Mac's files and its
    /// tasks are not eve lite's.
    pub(crate) async fn tool(self: &Arc<Self>, name: &str, args: &Obj, chat_id: i64) -> Obj {
        if lite(self.environ.as_ref()) && !lite_has(name) {
            tracing::info!("telegram: lite: refused a tool eve lite does not have");
            return itl_refused(LITE_TOOL_OFF);
        }
        let run = AssertUnwindSafe(self.tool_body(name, args, chat_id)).catch_unwind().await;
        let why = match run {
            Ok(Ok(result)) => return result,
            Ok(Err(e)) => format!("{e:#}"),
            Err(panic) => itl_panic_name(panic.as_ref()),
        };
        if eve_opentable::TOOL_NAMES.contains(&name) {
            tracing::warn!("telegram: OpenTable search could not start ({why})");
            return itl_o(json!({"ok": false, "status": "unavailable", "reservation_submitted": false,
                                "reason": "The OpenTable browser task could not start. Please try again or use OpenTable directly.",
                                "opentable_url": eve_opentable::OPENTABLE_URL}));
        }
        if eve_america::TOOL_NAMES.contains(&name) {
            tracing::warn!("telegram: /america failed ({why})");
            return itl_o(json!({"ok": false, "status": "upstream_unavailable",
                                "reason": "The /america lookup failed. Please try official-source research or the source website.",
                                "america_url": eve_america::AMERICA_URL}));
        }
        tracing::error!("telegram: {name} failed: {why}");
        itl_failed(name)
    }

    /// `_tool`'s `try:` body: TOOL_HANDLERS, then the run-time owners in Python's order.
    async fn tool_body(self: &Arc<Self>, name: &str, args: &Obj, chat_id: i64) -> anyhow::Result<Obj> {
        // TOOL_HANDLERS (telegram.py:7575)
        if let Some(result) = self.tool_handler(name, args, chat_id).await {
            return Ok(result);
        }
        let as_value = Value::Object(args.clone());
        let maker = self.maker.lock().clone();
        if let Some(maker) = maker {
            if crate::apps_maker::MAKER_TOOL_NAMES.contains(&name) {
                let api = self.api.clone();
                let send: SendButtons = Arc::new(move |chat, text, buttons| {
                    let api = api.clone();
                    async move { api.send_web_apps(chat, &text, &buttons).await.map_err(anyhow::Error::from) }.boxed()
                });
                let me = self.clone();
                let say: SayFn = Arc::new(move |chat, text| {
                    let me = me.clone();
                    async move { me.say(chat, &text, None, None, &[], 0, None).await }.boxed()
                });
                let me = self.clone();
                let photo: SendPhoto = Arc::new(move |chat, jpeg, caption, buttons| {
                    let me = me.clone();
                    async move { me.send_app_photo(chat, &jpeg, &caption, &buttons).await.map_err(anyhow::Error::from) }.boxed()
                });
                return Ok(maker.handle(name, args, chat_id, send, say, Some(photo)).await);
            }
        }
        if self.memory.is_some()
            && self.memory_tools().iter().any(|t| t.get("name").and_then(Value::as_str) == Some(name))
        {
            return Ok(self.memory_tool(name, args).await);
        }
        if self.st.lock().routed && name == eve_apps_mcp::OPEN_TOOL {
            return Ok(self.open_toolsets(args));
        }
        if let Some(direct) = &self.direct {
            if direct.names().contains(name) || direct.owns(name) {
                return Ok(self.direct_tool(name, args, chat_id).await);
            }
        }
        if let Some(apps) = &self.apps {
            if apps.names().contains(name) {
                self.lend(eve_toolset_router::WORKSPACE);
                return Ok(self.app_tool(name, args, chat_id).await);
            }
        }
        if eve_meet::TOOL_NAMES.contains(&name) {
            let Some(meeter) = &self.meeter else {
                return Ok(itl_refused(eve_meet::OFF_REASON));
            };
            return Ok(itl_obj(meeter.handle(name, &as_value).await?, name));
        }
        if name == "firetv_screenshot" {
            let firetv = self.hubs.lock().get("firetv");
            if let Some(firetv) = firetv {
                // the picture goes to the owner's chat; the model can't see it, so it is told in words what
                // the TV reports at the same moment ("shows"), never the path. The file is only needed until
                // it is sent, so it is deleted then: shots would otherwise pile up in the cache
                let shot = firetv.screenshot_for_model().await.and_then(|v| v.as_object().cloned()).unwrap_or_default();
                if !itl_truthy(&shot, "ok") || !itl_truthy(&shot, "path") {
                    let reason = match itl_str(&shot, "reason") {
                        r if r.is_empty() => "the TV sent no picture".to_string(),
                        r => r,
                    };
                    return Ok(itl_refused(&reason));
                }
                let path = PathBuf::from(itl_str(&shot, "path"));
                // the app keeps its own copy
                let sent = AssertUnwindSafe(self.send_media(chat_id, Some(&path), "The TV right now", false, None, "", &[]))
                    .catch_unwind()
                    .await;
                let _ = std::fs::remove_file(&path);
                if let Err(panic) = sent {
                    std::panic::resume_unwind(panic);
                }
                return Ok(itl_o(json!({"ok": true, "sent": "photo", "shows": itl_str(&shot, "shows")})));
            }
        }
        if let Some(app) = eve_app_registry::owner(name, false) {
            // eve's apps (app_registry.APPS): the hub's own handle, or the app's off reason
            let hub = self.hubs.lock().get(&app.slot);
            let Some(hub) = hub else {
                let reason = app.module_info().and_then(|m| m.off_reason.clone()).unwrap_or_default();
                return Ok(itl_refused(&reason));
            };
            return Ok(itl_obj(hub.handle(name, &as_value).await, name));
        }
        let environ = self.environ.as_ref();
        if eve_places::phone_location::TOOL_NAMES.contains(&name) {
            // the phone's latest fix (phone_location.py): a file read, no device, nothing sent anywhere
            return Ok(eve_places::phone_location::handle(name, args, environ).await);
        }
        if eve_find_image::TOOL_NAMES.contains(&name) {
            // a picture's URL and its Wikipedia page title (find_image.py): outside words, like web_search's
            return Ok(itl_obj(eve_find_image::handle(name, &as_value, environ, None).await, name));
        }
        if eve_science_search::TOOL_NAMES.contains(&name) {
            // papers' titles and abstracts (science_search.py): outside words, so the result taints the turn
            // as web_search's does (tool_gate); off, the reason and no request
            let opts = eve_science_search::SearchOptions::default();
            return Ok(itl_obj(eve_science_search::handle(name, &as_value, environ, &opts).await, name));
        }
        if eve_nyt::TOOL_NAMES.contains(&name) {
            // The New York Times (nyt.py): outside words, so the result taints the turn as web_search's does;
            // off, or a key tool without NYT_API_KEY, the reason and no request
            let opts = eve_nyt::Opts::default();
            return Ok(itl_obj(eve_nyt::handle(name, &as_value, environ, &opts).await, name));
        }
        if eve_canvas::TOOL_NAMES.contains(&name) {
            let Some(canvas) = &self.canvas else {
                return Ok(itl_refused(eve_canvas::OFF_REASON));
            };
            return Ok(itl_obj(canvas.handle(name, &as_value).await, name));
        }
        if crate::share::TOOL_NAMES.contains(&name) {
            self.lend(eve_toolset_router::SHARE);
            return Ok(self.share_tool(name, args, chat_id).await);
        }
        if eve_america::TOOL_NAMES.contains(&name) {
            let Some(america) = &self.america else {
                return Ok(itl_o(json!({"ok": false, "status": "unavailable", "reason": eve_america::OFF_REASON,
                                       "america_url": eve_america::AMERICA_URL})));
            };
            if !self.config.owner_ids.contains(&chat_id) {
                return Ok(itl_o(json!({"ok": false, "status": "unavailable",
                                       "reason": "Only a configured owner can use /america.",
                                       "america_url": eve_america::AMERICA_URL})));
            }
            return Ok(itl_obj(america.handle(name, &as_value).await?, name));
        }
        if eve_agentcard::TOOL_NAMES.contains(&name) {
            let Some(purchases) = &self.purchases else {
                return Ok(itl_refused(eve_agentcard::OFF_REASON));
            };
            if !self.config.owner_ids.contains(&chat_id) {
                return Ok(itl_refused("Only a configured owner can use AgentCard."));
            }
            let me = self.clone();
            // The gateway asks from its own worker task; asyncio's task copies the context, so the question is
            // still read on the call the work began on (`_FROM_CALL`).
            let turn_ctx = crate::seams::ctx();
            let ask: PurchaseAsk = Box::new(move |question: String, about: String| {
                crate::seams::in_ctx(turn_ctx, async move {
                    Ok(me
                        .ask_user(&question, chat_id, "Confirm purchase", Some(&super::chief::itl_yes_no()), Some(&about), true, true, None, false)
                        .await)
                })
                .boxed()
            });
            return Ok(itl_obj(purchases.handle(name, &as_value, Some(chat_id), ask).await, name));
        }
        if eve_web::watch::tools::TOOL_NAMES.contains(&name) {
            let Some(watcher) = &self.watcher else {
                return Ok(itl_refused(eve_web::watch::OFF_REASON));
            };
            return Ok(itl_obj(watcher.handle(name, args).await, name));
        }
        if eve_chief::TOOL_NAMES.contains(&name) {
            // a stray call never falls through to think_hard
            if self.chief.lock().is_none() {
                return Ok(itl_refused(CHIEF_OFF_REASON));
            }
            return Ok(self.chief_tool(name, args).await);
        }
        if eve_second_brain::SECOND_BRAIN_TOOL_NAMES.contains(&name) {
            let Some(vault) = &self.vault else {
                return Ok(itl_refused("the second brain is off on this computer (EVE_SECOND_BRAIN)"));
            };
            let (root, name_owned) = (vault.root.clone(), name.to_string());
            let out = tokio::task::spawn_blocking(move || eve_second_brain::dispatch(&root, &name_owned, &as_value))
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            return Ok(itl_obj(out, name));
        }
        let question = itl_str(args, "question");
        Ok(self.think_hard(itl_strip(&question), Some(chat_id)).await)
    }

    /// TOOL_HANDLERS (telegram.py:7575): `handler(inlet, name, args, chat_id)` for a fixed tool, None for any other.
    /// Later entries of Python's dict win over earlier ones; none of its names repeat.
    async fn tool_handler(self: &Arc<Self>, name: &str, args: &Obj, chat_id: i64) -> Option<Obj> {
        let out = match name {
            "start_task" => self.tool_start_task(name, args, chat_id).await,
            "opentable_search" => self.tool_opentable_search(name, args, chat_id).await,
            "steer_task" => self.tool_steer_task(name, args, chat_id).await,
            "stop_task" => self.tool_stop_task(name, args, chat_id).await,
            "recent_messages" => self.tool_recent_messages(name, args, chat_id).await,
            "start_coding_session" => self.tool_start_coding_session(name, args, chat_id).await,
            "take_photo" => self.tool_take_photo(name, args, chat_id).await,
            "screenshot" => self.tool_screenshot(name, args, chat_id).await,
            "send_file" => self.tool_send_file(name, args, chat_id).await,
            "list_files" => self.tool_list_files(name, args, chat_id).await,
            n if n == eve_menu_tools::READ_FILE => self.tool_read_file(name, args, chat_id).await,
            n if ROBOT_TOOLS.contains(&n) => self.tool_robot(name, args, chat_id).await,
            n if n == eve_web::websearch::TOOL_NAME => self.tool_web_search(name, args, chat_id).await,
            n if eve_menu_tools::TOOL_NAMES.contains(&n) => self.tool_menu(name, args, chat_id).await,
            n if n == eve_events::TOOL_NAME => self.tool_events(name, args, chat_id).await,
            _ => return None,
        };
        Some(out)
    }

    /// py: telegram.py:5974 `def _mail_task(self, args: dict[str, Any]) -> bool:`
    ///
    /// Whether a start_task goal sends email while Gmail is up and may send (MAIL_SEND).
    pub(crate) fn mail_task(self: &Arc<Self>, args: &Obj) -> bool {
        let Some(d) = &self.direct else {
            return false;
        };
        if !d.started() || !d.apps().iter().any(|a| a == "gmail") {
            return false;
        }
        if d.policy.get("gmail").map(String::as_str).unwrap_or("ask") == "read" {
            return false;
        }
        MAIL_SEND.is_match(&itl_str(args, "goal"))
    }

    /// py: telegram.py:5983 `async def _tool_start_task(self, name: str, args: dict[str, Any], chat_id: int) -> dict[str, Any]:`
    pub(crate) async fn tool_start_task(self: &Arc<Self>, _name: &str, args: &Obj, chat_id: i64) -> Obj {
        let goal = itl_str(args, "goal");
        self.start_task(itl_strip(&goal), chat_id, None, None, None)
    }

    /// py: telegram.py:5986 `async def _tool_opentable_search(self, name: str, args: dict[str, Any], chat_id: int) -> dict[str, Any]:`
    pub(crate) async fn tool_opentable_search(self: &Arc<Self>, _name: &str, args: &Obj, chat_id: i64) -> Obj {
        if !self.config.owner_ids.contains(&chat_id) {
            return itl_o(json!({"ok": false, "status": "unavailable", "reason": "Only a configured owner can use /opentable."}));
        }
        if !self.opentable_enabled {
            return itl_o(json!({"ok": false, "status": "unavailable", "reason": eve_opentable::OFF_REASON,
                                "opentable_url": eve_opentable::OPENTABLE_URL}));
        }
        let goal = if args.len() != 1 || !args.contains_key("request") {
            Err("OpenTable search expects only a request.".to_string())
        } else {
            eve_opentable::build_goal(&args["request"]).map_err(|e| e.0)
        };
        let goal = match goal {
            Ok(goal) => goal,
            Err(reason) => return itl_o(json!({"ok": false, "status": "invalid_request", "reason": reason})),
        };
        let request = args["request"].as_str().unwrap_or("");
        let display = format!("OpenTable: {}", itl_strip(request));
        let started = std::panic::catch_unwind(AssertUnwindSafe(|| {
            self.start_task(&goal, chat_id, None, Some("codex"), Some(&display))
        }));
        let result = match started {
            Ok(result) => result,
            Err(panic) => {
                let progress = self.st.lock().task_progress.take();
                self.close_progress(progress.as_ref(), PROGRESS_STOPPED_LINE).await;
                std::panic::resume_unwind(panic);
            }
        };
        if itl_truthy(&result, "ok") {
            {
                let mut st = self.st.lock();
                st.opentable_agent = st.agent.clone();
            }
            return itl_o(json!({"ok": true, "status": "started", "provider": "opentable_browser",
                                "reservation_submitted": false,
                                "note": "Search started; observed availability and links will be sent when it finishes. No reservation has been made."}));
        }
        let mut out = result;
        out.insert("status".into(), json!("unavailable"));
        out.insert("reservation_submitted".into(), json!(false));
        out
    }

    /// py: telegram.py:6011 `async def _opentable_command(self, inbound: Inbound, rest: str) -> None:`
    pub(crate) async fn opentable_command(self: &Arc<Self>, inbound: &Inbound, rest: &str) {
        let chat = inbound.chat_id;
        if !self.config.owner_ids.contains(&chat) || !self.config.owner_ids.contains(&inbound.user_id) {
            self.say(chat, "Only a configured owner can use /opentable.", None, None, &[], 0, None).await;
            return;
        }
        if !self.opentable_enabled {
            let line = format!("{}\n{}", eve_opentable::OFF_REASON, eve_opentable::OPENTABLE_URL);
            self.say(chat, &line, None, None, &[], 0, None).await;
            return;
        }
        if inbound.image.is_some() {
            self.say(chat, "Please send the /opentable request in text without attachments.", None, None, &[], 0, None)
                .await;
            return;
        }
        if STOP_WORDS.contains(&itl_rstrip_chars(&itl_lower(rest), ".! ")) {
            let (same, task_chat) = {
                let st = self.st.lock();
                (itl_same_agent(&st.opentable_agent, &st.agent), st.task_chat)
            };
            if same && self.task_running() && task_chat == Some(chat) {
                // Only this search; unrelated purchase consent stays open.
                self.stop(chat, Some(&Obj::new())).await;
            } else {
                self.say(chat, "No /opentable search is running.", None, None, &[], 0, None).await;
            }
            return;
        }
        if rest.is_empty() {
            self.say(chat, eve_opentable::HELP_LINE, None, None, &[], 0, None).await;
            return;
        }
        self.st.lock().chat_id = Some(chat);
        self.note("user", &inbound.text, "command", None, None, None, false, None);
        let mut args = Obj::new();
        args.insert("request".into(), json!(rest));
        let result = self.tool("opentable_search", &args, chat).await;
        if !itl_truthy(&result, "ok") {
            let reason = match itl_str(&result, "reason") {
                r if r.is_empty() => "OpenTable search could not start.".to_string(),
                r => r,
            };
            self.say(chat, &reason, None, None, &[], 0, None).await;
        }
    }

    /// py: telegram.py:6037 `async def _tool_steer_task(self, name: str, args: dict[str, Any], chat_id: int) -> dict[str, Any]:`
    pub(crate) async fn tool_steer_task(self: &Arc<Self>, _name: &str, args: &Obj, _chat_id: i64) -> Obj {
        let chief_task = self.st.lock().chief_task.clone();
        if let Some((cid, n)) = chief_task {
            if self.task_running() {
                // a chief's step runs what its card says (and an act, what its Go showed): the turn may hold web text
                // from its own searches, so it never steers one; the owner changes the card (reviewer after P4,
                // 2026-09-29: steer_task reached an approved act mid-run). Stop still works.
                return itl_refused(&format!(
                    "that is step {n} of {cid}; it cannot be steered. The owner can say change {cid}, or stop it"
                ));
            }
        }
        let text = itl_str(args, "text");
        let agent = self.st.lock().agent.clone();
        let ok = match agent {
            Some(agent) => self.task_running() && agent.steer(&text),
            None => false,
        };
        if ok {
            let line = format!(
                "You told the running computer task: {}",
                eve_chat_log::note_text(&text, eve_chat_log::NOTE_CHARS)
            );
            self.event(&line, "", false);
            return itl_o(json!({"ok": true}));
        }
        itl_refused("no task is running")
    }

    /// py: telegram.py:6050 `async def _tool_stop_task(self, name: str, args: dict[str, Any], chat_id: int) -> dict[str, Any]:`
    pub(crate) async fn tool_stop_task(self: &Arc<Self>, _name: &str, _args: &Obj, chat_id: i64) -> Obj {
        let purchase_stop = self.purchases.as_ref().map(|p| p.stop(Some(chat_id)));
        let stopped_purchase = purchase_stop.as_ref().is_some_and(|s| s.stopped > 0 || s.in_flight);
        let (agent, returned) = {
            let st = self.st.lock();
            (st.agent.clone(), st.task_returned)
        };
        if let Some(agent) = agent {
            if self.task_running() && !returned {
                agent.cancel("stopped from Telegram");
                self.event("You stopped the running computer task", "", false);
                return match (&purchase_stop, stopped_purchase) {
                    (Some(stop), true) => itl_o(json!({"ok": true, "purchase": stop.to_value()})),
                    _ => itl_o(json!({"ok": true})),
                };
            }
        }
        if let (Some(stop), true) = (&purchase_stop, stopped_purchase) {
            return itl_o(json!({"ok": true, "purchase": stop.to_value()}));
        }
        itl_refused("no task is running")
    }

    /// py: telegram.py:6061 `async def _tool_recent_messages(self, name: str, args: dict[str, Any], chat_id: int) -> dict[str, Any]:`
    ///
    /// This chat's earlier lines from the chat log, word for word, oldest first, the message being answered
    /// (and anything this turn wrote after it) left out: "before that" is a lookup, never a guess.
    pub(crate) async fn tool_recent_messages(self: &Arc<Self>, _name: &str, args: &Obj, _chat_id: i64) -> Obj {
        let chat_log = self.st.lock().chat_log.clone();
        let Some(chat_log) = chat_log else {
            return itl_refused("no chat log is kept on this computer");
        };
        self.seed_log();

        let number = |key: &str, default: i64, low: i64, high: i64| -> i64 {
            let value = itl_int(args.get(key)).unwrap_or(default);
            low.max(high.min(value))
        };

        let mut lines: Vec<Value> = chat_log.lines(self.log_chat()).iter().map(itl_line_value).collect();
        let now = self.st.lock().turn_said.clone();
        if !now.is_empty() {
            for i in (0..lines.len()).rev() {
                if line_str(&lines[i], "who") == "user" {
                    let text = line_str(&lines[i], "text");
                    if text == now || text.starts_with(&format!("{now} [")) {
                        lines.truncate(i);
                    }
                    break;
                }
            }
        }
        // "back": how far back each line is, 1 the newest before the message being answered; "owner_back" counts the
        // owner's own messages only, 1 their last one before this. "The one before the one I quoted" and "the message
        // I sent before my last one" are then numbers to read, not lines to count (eval, 2026-10-08: the brain
        // counted eve's lines as the owner's)
        let owners: Vec<Value> = lines.iter().rev().filter(|ln| line_str(ln, "who") == "user").cloned().collect();
        let owner_back: HashMap<i64, i64> =
            owners.iter().enumerate().map(|(i, ln)| (line_seq(ln), i as i64 + 1)).collect();
        let since = local_time(args.get("since").unwrap_or(&Value::Null));
        let until = local_time(args.get("until").unwrap_or(&Value::Null));
        for (key, value) in [("since", since), ("until", until)] {
            if !itl_strip(&itl_str(args, key)).is_empty() && value.is_none() {
                // a time that cannot be read is said, never quietly the newest lines (verification review, 2026-10-09)
                return itl_refused(&format!(
                    "{key} must be a time like 2026-10-08T18:00 (the owner's local time)"
                ));
            }
        }
        if let Some(since) = since {
            if until.is_some_and(|until| until <= since) {
                return itl_refused("until must be after since");
            }
            return self.recent_range(&lines, &owner_back, since, until);
        }
        let mut top = lines.len() as i64;
        let raw_from = itl_int(args.get("before_owner_back")).filter(|n| *n >= 1);
        if let Some(raw_from) = raw_from {
            // read back from a given owner message: the lines before it (live, 2026-10-08: "before that" twice)
            let anchor = if raw_from as usize <= owners.len() { Some(line_seq(&owners[raw_from as usize - 1])) } else { None };
            top = match anchor {
                Some(seq) => lines.iter().position(|ln| line_seq(ln) == seq).unwrap_or(0) as i64,
                None => 0,
            };
        }
        let count = number("count", RECENT_MESSAGES_DEFAULT, 1, RECENT_MESSAGES_MAX as i64);
        let before = number("before", 0, 0, top);
        let end = top - before;
        let mut picked: Vec<Value> = lines[(end - count).max(0) as usize..end as usize].to_vec();
        // A character budget, newest first, as the shown history has one: 20 lines of 4000 characters would be a
        // 20k-token tool result, repeated on every page back (review, 2026-10-08). The newest line always fits.
        let (mut budget, mut keep) = (RECENT_MESSAGES_CHARS as i64, 0usize);
        for ln in picked.iter().rev() {
            let size = line_str(ln, "text").chars().count().min(RECENT_MESSAGES_LINE_CHARS) as i64;
            if keep > 0 && size > budget {
                break;
            }
            budget -= size;
            keep += 1;
        }
        picked = picked.split_off(picked.len() - keep);

        let when = |ln: &Value| itl_local_fmt(line_t(ln), WHEN_FMT);
        // eve's blocks as the history shows them to this turn
        let app = ctx().app.is_some();
        let mut messages = Vec::new();
        for (i, ln) in picked.iter().enumerate() {
            let who = line_str(ln, "who");
            let mut row = Obj::new();
            row.insert("back".into(), json!(lines.len() as i64 - (end - picked.len() as i64 + i as i64)));
            row.insert("who".into(), json!(who_word(who)));
            if who == "user" {
                row.insert("owner_back".into(), json!(owner_back.get(&line_seq(ln)).copied().unwrap_or(0)));
            }
            row.insert("when".into(), json!(when(ln)));
            let text = line_str(ln, "text");
            let shown = if who != "eve" { text.to_string() } else { history_text(text, app) };
            row.insert("text".into(), json!(clip_line(&shown)));
            messages.push(Value::Object(row));
        }
        let seen: Vec<Value> = picked.iter().filter(|ln| line_str(ln, "who") == "user").cloned().collect();
        self.st.lock().recall_seen.extend(seen);
        let earlier = end - picked.len() as i64;
        let is_empty = messages.is_empty();
        let mut out = Obj::new();
        out.insert("ok".into(), json!(true));
        out.insert("messages".into(), Value::Array(messages));
        out.insert("earlier".into(), json!(earlier));
        let quoted = last_quoted(&lines);
        if let Some(quoted) = quoted.filter(|q| owner_back.contains_key(&line_seq(q))) {
            // The screenshot's failure, and the live one (2026-10-08): eve had just quoted a message, and "before
            // that" was answered with the same message again. Which one eve's last reply quoted is a fact (tagged
            // on its log line when written); the one before it is given whole, so nothing is left to count.
            let n = owner_back[&line_seq(&quoted)];
            out.insert(
                "your_last_reply_quoted".into(),
                json!({"owner_back": n, "text": clip_line(line_str(&quoted, "text"))}),
            );
            if (n as usize) < owners.len() {
                let prior = owners[n as usize].clone();
                out.insert(
                    "before_that".into(),
                    json!({"owner_back": n + 1, "when": when(&prior), "text": clip_line(line_str(&prior, "text"))}),
                );
                self.st.lock().recall_seen.push(prior);
            } else {
                let mut line = "nothing before it is kept in this chat's log".to_string();
                if self.memory.is_some() {
                    line.push_str("; memory_search reads older talk");
                }
                out.insert("before_that".into(), json!(line));
            }
        }
        let note = if is_empty {
            "nothing earlier is kept in this chat's log".to_string()
        } else if earlier == 0 {
            "these are the oldest messages kept; nothing before them is kept".to_string()
        } else {
            // only an int >= 1 moves `top`, so the raw value printed is that int
            let from = match raw_from {
                Some(raw) if top < lines.len() as i64 => format!("and before_owner_back={raw} "),
                _ => String::new(),
            };
            format!(
                "{earlier} earlier messages are kept too: call again with before={} {from}to read further back",
                before + picked.len() as i64
            )
        };
        out.insert("note".into(), json!(note));
        out
    }

    /// py: telegram.py:6160 `def _recent_range(self, lines: list[Any], owner_back: dict[int, int], since: float, until: Optional[float]) -> dict[str, Any]:`
    ///
    /// recent_messages over a stretch of time: every line from ``since`` to ``until`` (now when None), oldest
    /// first, up to RECENT_RANGE_MAX lines and RECENT_RANGE_CHARS characters. What does not fit is said, with the
    /// time to read on from, never dropped in silence.
    ///
    /// Rust: `lines` are chat log lines as values ([`itl_line_value`]); `owner_back` is keyed by a line's `seq`
    /// (Python's `id(ln)`).
    pub(crate) fn recent_range(self: &Arc<Self>, lines: &[Value], owner_back: &HashMap<i64, i64>, since: f64, until: Option<f64>) -> Obj {
        let end = until.unwrap_or(f64::INFINITY);
        let inside: Vec<&Value> = lines.iter().filter(|ln| since <= line_t(ln) && line_t(ln) < end).collect();
        let app = ctx().app.is_some();
        let mut budget = RECENT_RANGE_CHARS as i64;
        let mut picked: Vec<(&Value, Value)> = Vec::new();
        for ln in inside.iter().take(RECENT_RANGE_MAX) {
            let who = line_str(ln, "who");
            let raw = line_str(ln, "text");
            let text = if who != "eve" { raw.to_string() } else { history_text(raw, app) };
            let text = clip_line(&text);
            let size = text.chars().count() as i64;
            if !picked.is_empty() && size > budget {
                break;
            }
            budget -= size;
            let mut row = Obj::new();
            row.insert("who".into(), json!(who_word(who)));
            row.insert("when".into(), json!(itl_local_fmt(line_t(ln), WHEN_FMT)));
            row.insert("text".into(), json!(text));
            if who == "user" {
                row.insert("owner_back".into(), json!(owner_back.get(&line_seq(ln)).copied().unwrap_or(0)));
            }
            picked.push((ln, Value::Object(row)));
        }
        let seen: Vec<Value> =
            picked.iter().filter(|(ln, _)| line_str(ln, "who") == "user").map(|(ln, _)| (*ln).clone()).collect();
        self.st.lock().recall_seen.extend(seen);
        let mut out = Obj::new();
        out.insert("ok".into(), json!(true));
        out.insert("messages".into(), Value::Array(picked.iter().map(|(_, row)| row.clone()).collect()));
        let left = inside.len() - picked.len();
        let note = if inside.is_empty() {
            let mut line = "nothing was said in this chat in that time".to_string();
            if self.memory.is_some() {
                line.push_str("; memory_search reads older talk");
            }
            line
        } else if left > 0 {
            let next = itl_local_fmt(line_t(inside[picked.len()]), "%Y-%m-%dT%H:%M");
            format!(
                "{left} more messages in that time: call again with since={next} to read them, and read them before you sum the time up"
            )
        } else {
            format!("all {} messages in that time", picked.len())
        };
        out.insert("note".into(), json!(note));
        out
    }

    /// py: telegram.py:6196 `async def _tool_start_coding_session(self, name: str, args: dict[str, Any], chat_id: int) -> dict[str, Any]:`
    pub(crate) async fn tool_start_coding_session(self: &Arc<Self>, _name: &str, args: &Obj, chat_id: i64) -> Obj {
        let mut flow = self.new_launch();
        let step = flow.request(&itl_str(args, "area"), &itl_str(args, "folder"), &itl_str(args, "harness"));
        self.st.lock().launch = if step.done { None } else { Some(flow) };
        let me = self.clone();
        self.spawn(async move { me.launch_step(chat_id, &step).await }, "telegram-launch");
        itl_o(json!({"ok": true, "sent": "the question or the result is already in the chat; add nothing more than a word or two"}))
    }

    /// py: telegram.py:6205 `async def _tool_menu(self, name: str, args: dict[str, Any], chat_id: int) -> dict[str, Any]:`
    ///
    /// A / menu command from plain words (menu_tools.py): the command's own handler, as _handle runs it for the
    /// typed word. A hand-off's handler sends its own message, so its result ends the turn (``end_turn``).
    pub(crate) async fn tool_menu(self: &Arc<Self>, name: &str, args: &Obj, chat_id: i64) -> Obj {
        let done = || {
            itl_o(json!({"ok": true, "end_turn": true,
                         "sent": "the command's own message is already in the chat; add nothing more"}))
        };
        if name == eve_menu_tools::SPEND {
            return match self.spend_text().await {
                None => itl_refused("the spend ledger could not be read just now"),
                Some(text) => itl_o(json!({"ok": true, "report": text})),
            };
        }
        if name == eve_menu_tools::RUNDOWN {
            self.rundown_handoff(chat_id);
            return done();
        }
        if name == eve_menu_tools::CLAUDE {
            let action = match itl_str(args, "action") {
                a if a.is_empty() => "on".to_string(),
                a => a,
            };
            let on = itl_lower(itl_strip(&action)) != "off";
            let inbound = Inbound {
                chat_id,
                user_id: 0,
                text: if on { "claude on" } else { "claude off" }.into(),
                message_id: self.st.lock().turn_request,
                ..Inbound::default()
            };
            self.st.lock().chat_id = Some(chat_id);
            // tell=False: the reason goes back as the result, said once by the brain, not also as a line here.
            // ask_to_start: a session that is not running is the owner's tap to start, never a silent launch.
            let session = itl_str(args, "session");
            if !self.claude_switch(&inbound, on, itl_strip(&session), false, false, true) {
                return itl_refused(CODEX_IN_USE_LINE);
            }
            return done();
        }
        if name == eve_menu_tools::CODEX {
            let raw = match itl_str(args, "action") {
                a if a.is_empty() => "on".to_string(),
                a => a,
            };
            let mut action = itl_lower(itl_strip(&raw));
            let folder_raw = itl_str(args, "folder");
            let folder = Some(itl_strip(&folder_raw)).filter(|f| !f.is_empty());
            if !["on", "off", "status"].contains(&action.as_str()) {
                action = "on".into();
            }
            if action == "on" && folder.is_some() {
                // as "codex <folder>" typed in the chat
                action = "use".into();
            }
            let selection = if action == "use" { folder } else { None };
            if !self.codex_switch(chat_id, &action, selection) {
                return itl_refused(CODEX_IN_USE_LINE);
            }
            return done();
        }
        if name == eve_menu_tools::STEALTH {
            self.set_stealth(chat_id, itl_truthy(args, "on"));
            return done();
        }
        if name == eve_menu_tools::APPS {
            self.open_apps(chat_id).await;
            return done();
        }
        if name == eve_menu_tools::LINK {
            self.stick_link(chat_id, itl_truthy(args, "new_link")).await;
            return done();
        }
        itl_refused("not a menu tool")
    }

    /// py: telegram.py:6250 `def _rundown_handoff(self, chat_id: int) -> None:`
    ///
    /// daily_rundown: the owner's message goes to a rundown turn of its own, as the typed /rundown does
    /// (_answer_turn, daily): its own instructions and sources, and only its read tools (rundown.allows), so no
    /// word of the mail or Slack it reads can reach an acting tool. It runs once this turn ends (the turn lock);
    /// this turn ends with the call (menu_tools.HANDS_OFF).
    pub(crate) fn rundown_handoff(self: &Arc<Self>, chat_id: i64) {
        let (said, request) = {
            let st = self.st.lock();
            (st.turn_said.clone(), st.turn_request)
        };
        let said = if said.is_empty() { "rundown".to_string() } else { said };
        tracing::info!("telegram: daily_rundown hands the message to a rundown turn");
        let inbound = Inbound { chat_id, user_id: 0, text: said, message_id: request, ..Inbound::default() };
        let me = self.clone();
        // boxed: the rundown turn's own tools come back here, and a boxed future keeps the types finite
        let turn: futures::future::BoxFuture<'static, ()> =
            Box::pin(async move { me.turn(&inbound, None, &[], true, false).await });
        self.spawn(turn, "telegram-rundown");
    }

    /// py: telegram.py:6260 `async def _tool_events(self, name: str, args: dict[str, Any], chat_id: int) -> dict[str, Any]:`
    pub(crate) async fn tool_events(self: &Arc<Self>, name: &str, args: &Obj, _chat_id: i64) -> Obj {
        itl_obj(self.events.handle(name, args).await, name)
    }

    /// py: telegram.py:6263 `async def _tool_take_photo(self, name: str, args: dict[str, Any], chat_id: int) -> dict[str, Any]:`
    pub(crate) async fn tool_take_photo(self: &Arc<Self>, _name: &str, args: &Obj, chat_id: i64) -> Obj {
        self.take_photo(&itl_str(args, "note"), chat_id).await
    }

    /// py: telegram.py:6266 `async def _tool_screenshot(self, name: str, args: dict[str, Any], chat_id: int) -> dict[str, Any]:`
    pub(crate) async fn tool_screenshot(self: &Arc<Self>, _name: &str, args: &Obj, chat_id: i64) -> Obj {
        self.send_screen(chat_id, &itl_str(args, "caption"), None).await
    }

    /// py: telegram.py:6269 `async def _tool_send_file(self, name: str, args: dict[str, Any], chat_id: int) -> dict[str, Any]:`
    pub(crate) async fn tool_send_file(self: &Arc<Self>, _name: &str, args: &Obj, chat_id: i64) -> Obj {
        self.send_file(chat_id, &itl_str(args, "path"), &itl_str(args, "caption")).await
    }

    /// py: telegram.py:6272 `async def _tool_list_files(self, name: str, args: dict[str, Any], chat_id: int) -> dict[str, Any]:`
    pub(crate) async fn tool_list_files(self: &Arc<Self>, name: &str, args: &Obj, _chat_id: i64) -> Obj {
        let raw = itl_str(args, "path");
        let home = self.owner_home();
        match tokio::task::spawn_blocking(move || list_files(&raw, home.as_deref())).await {
            Ok(out) => out,
            Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
            Err(_) => itl_failed(name),
        }
    }

    /// py: telegram.py:6275 `async def _tool_read_file(self, name: str, args: dict[str, Any], chat_id: int) -> dict[str, Any]:`
    pub(crate) async fn tool_read_file(self: &Arc<Self>, name: &str, args: &Obj, _chat_id: i64) -> Obj {
        let raw = itl_str(args, "path");
        match tokio::task::spawn_blocking(move || read_file(&raw, None)).await {
            Ok(out) => out,
            Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
            Err(_) => itl_failed(name),
        }
    }

    /// py: telegram.py:6278 `async def _tool_robot(self, name: str, args: dict[str, Any], chat_id: int) -> dict[str, Any]:`
    pub(crate) async fn tool_robot(self: &Arc<Self>, name: &str, args: &Obj, _chat_id: i64) -> Obj {
        self.robot_tool(name, args).await
    }

    /// py: telegram.py:6281 `async def _memory_tool(self, name: str, args: dict[str, Any]) -> dict[str, Any]:`
    ///
    /// A memory tool (memory.tools()), run by the Memory off the loop. One log line: the tool, how many
    /// hits and how long; never the query, never what came back.
    pub(crate) async fn memory_tool(self: &Arc<Self>, name: &str, args: &Obj) -> Obj {
        let Some(memory) = self.memory.clone() else {
            return itl_failed(name);
        };
        let t0 = monotonic();
        let turn = ctx();
        if MEMORY_READS.contains(&name) && memory.retry() {
            if let Some(spent) = &turn.memory_calls {
                let over = {
                    let mut n = spent.lock();
                    *n += 1;
                    *n > MAX_MEMORY_CALLS as i64
                };
                if over {
                    tracing::info!("telegram: {name} refused: {MAX_MEMORY_CALLS} memory reads this turn");
                    return itl_refused(MEMORY_CALLS_SPENT);
                }
            }
        }
        let (tool, input) = (name.to_string(), Value::Object(args.clone()));
        let result = match tokio::task::spawn_blocking(move || memory.handle_tool(&tool, &input, None, CHANNEL)).await {
            Ok(result) => result,
            Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
            Err(_) => return itl_failed(name),
        };
        let hits: usize = match &result {
            Value::Object(map) => map.values().filter_map(Value::as_array).map(Vec::len).sum(),
            _ => 0,
        };
        tracing::info!(
            "telegram: {name}: {hits} hit{} in {} ms",
            if hits == 1 { "" } else { "s" },
            ((monotonic() - t0) * 1000.0).round() as i64
        );
        if let Some(kept) = &turn.sources {
            if MEMORY_READS.contains(&name) {
                // read from the result, never written to it: the model is handed exactly what the Memory returned
                let found = std::panic::catch_unwind(AssertUnwindSafe(|| eve_memory_sources::from_result(name, &result)));
                match found {
                    Ok(found) => {
                        let mut list = kept.lock();
                        let mut sources =
                            eve_memory_sources::sanitize(&Value::Array(list.iter().cloned().map(Value::Object).collect()));
                        eve_memory_sources::add(&mut sources, &found);
                        *list = sources.iter().map(|s| itl_o(s.to_value())).collect();
                    }
                    // Sources never cost the turn
                    Err(_) => tracing::warn!("telegram: memory sources not kept (panic)"),
                }
            }
        }
        match result {
            Value::Object(map) => map,
            _ => itl_failed(name),
        }
    }

    /// py: telegram.py:6304 `async def _tool_web_search(self, name: str, args: dict[str, Any], chat_id: int) -> dict[str, Any]:`
    pub(crate) async fn tool_web_search(self: &Arc<Self>, name: &str, args: &Obj, _chat_id: i64) -> Obj {
        // websearch.search (the engine, or routed by Jev), off the loop: a second or two of network
        let query = itl_str(args, "query");
        let config = self.config.search.clone();
        let lent = self.st.lock().web_search.clone();
        let found = tokio::task::spawn_blocking(move || match lent {
            Some(search) => search(&query, &config),
            None => eve_web::websearch::search(eve_web::Ctx::global(), &query, Some(&config), &eve_web::websearch::EngineOpts::default()),
        })
        .await;
        match found {
            Ok(out) => itl_obj(out, name),
            Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
            Err(_) => itl_failed(name),
        }
    }
}

// ---- test hooks: what the Python tests reach by attribute (`rig.inlet._chat_id = OWNER`, `rig.inlet._tool(...)`) ----

/// The private attributes and methods the ported tests of this unit set and call, as Python's tests do. Not part of
/// the inlet's surface: nothing outside `tests/` calls them.
#[doc(hidden)]
impl TelegramInlet {
    /// `inlet._chat_id = chat`.
    pub fn tt_set_chat_id(&self, chat: Option<i64>) {
        self.st.lock().chat_id = chat;
    }
    /// `inlet.claude = on`.
    pub fn tt_set_claude(&self, on: bool) {
        self.st.lock().claude = on;
    }
    /// `inlet._call_say = say`.
    pub fn tt_set_call_say(&self, say: Option<Reader>) {
        self.st.lock().call_say = say;
    }
    /// `inlet._permission_future = future`.
    pub fn tt_set_permission_future(&self, future: Option<Pending>) {
        self.st.lock().permission_future = future;
    }
    /// `inlet._pending_answer = future`.
    pub fn tt_set_pending_answer(&self, future: Option<Pending>) {
        self.st.lock().pending_answer = future;
    }
    /// `inlet._held`: messages held while "claude on" lists the sessions.
    pub fn tt_held(&self) -> usize {
        self.st.lock().held.len()
    }
    /// `inlet._chief_task`.
    pub fn tt_chief_task(&self) -> Option<(String, i64)> {
        self.st.lock().chief_task.clone()
    }
    /// `inlet._handle(inbound)`.
    pub fn tt_handle(self: &Arc<Self>, inbound: &Inbound) {
        self.handle(inbound);
    }
    /// `await inlet._turn(inbound)`.
    pub async fn tt_turn(self: &Arc<Self>, inbound: &Inbound) {
        self.turn(inbound, None, &[], false, false).await;
    }
    /// `await inlet._tool(name, args, chat_id)`.
    pub async fn tt_tool(self: &Arc<Self>, name: &str, args: &Obj, chat_id: i64) -> Obj {
        self.tool(name, args, chat_id).await
    }
    /// `await inlet._say(chat_id, text, title=title)`.
    pub async fn tt_say(self: &Arc<Self>, chat_id: i64, text: &str, title: Option<&str>) {
        self.say(chat_id, text, title, None, &[], 0, None).await;
    }
    /// `await inlet._ask_user(question, chat_id)`.
    pub async fn tt_ask_user(self: &Arc<Self>, question: &str, chat_id: i64) -> String {
        self.ask_user(question, chat_id, TASK_ASKS_TITLE, None, None, false, false, None, false).await
    }
    /// `inlet._open_question(future, chat_id, strict=strict, words=words, firm=firm)`.
    pub fn tt_open_question(self: &Arc<Self>, future: &Pending, chat_id: Option<i64>, strict: bool, words: &[&str], firm: bool) -> Arc<_Question> {
        let words: std::collections::HashSet<String> = words.iter().map(|w| w.to_string()).collect();
        self.open_question(future, chat_id, strict, &words, firm)
    }
    /// `inlet._live_question()`.
    pub fn tt_live_question(self: &Arc<Self>) -> Option<Arc<_Question>> {
        self.live_question()
    }
    /// `inlet._relay_to(cwd)`.
    pub fn tt_relay_to(self: &Arc<Self>, cwd: &str) {
        self.relay_to(cwd);
    }
    /// `inlet._chief_word(text)`: the chief's code word and card, or None.
    pub fn tt_chief_word(self: &Arc<Self>, text: &str) -> Option<(String, String)> {
        self.chief_word(text)
    }
    /// `await inlet._think(items, chat_id, taint=taint)`.
    pub async fn tt_think(self: &Arc<Self>, items: &[Obj], chat_id: i64, taint: &str) -> anyhow::Result<(String, i64)> {
        self.think(items, chat_id, false, 0, None, taint).await
    }
    /// `inlet._answers_pending(text)`.
    pub fn tt_answers_pending(self: &Arc<Self>, text: &str) -> bool {
        self.answers_pending(Some(text))
    }
    /// `inlet._claude_on_job = loop.create_future()` (on) and its `.cancel()` (off): a slow "claude on" listing.
    pub fn tt_hold_claude_on(self: &Arc<Self>, on: bool) {
        let mut st = self.st.lock();
        if let Some(job) = st.claude_on_job.take() {
            job.cancel();
        }
        if on {
            st.claude_on_job = Some(crate::seams::Job::spawn("claude-on", futures::future::pending::<()>()));
        }
    }
    /// `len(inlet._jobs)`: the jobs not yet done.
    pub fn tt_jobs_left(&self) -> usize {
        self.st.lock().jobs.iter().filter(|j| !j.done()).count()
    }
    /// `inlet._thinking`.
    pub fn tt_thinking(self: &Arc<Self>) -> bool {
        self.thinking()
    }
    /// `chat_id in inlet._paused_typing`.
    pub fn tt_paused_typing(&self, chat_id: i64) -> bool {
        self.st.lock().paused_typing.contains_key(&chat_id)
    }
    /// `inlet._open_progress(chat_id, head, lambda cid: None)`.
    pub fn tt_open_progress(self: &Arc<Self>, chat_id: i64, head: &str) -> Arc<_Progress> {
        self.open_progress(chat_id, head, Arc::new(|_| {}), None, None, 0)
    }
    /// `inlet._task_progress = progress`.
    pub fn tt_set_task_progress(&self, progress: Option<Arc<_Progress>>) {
        self.st.lock().task_progress = progress;
    }
    /// `inlet._progress_step(progress, line)`.
    pub fn tt_progress_step(self: &Arc<Self>, progress: &Arc<_Progress>, line: &str) {
        self.progress_step(Some(progress), line);
    }
    /// `await inlet._image(inbound, target, epoch)`.
    pub async fn tt_image(self: &Arc<Self>, inbound: &Inbound, target: &str, epoch: i64) {
        self.image(inbound, target, epoch, None).await;
    }
    /// `await inlet._say(chat_id, text, reply_to=reply_to)`.
    pub async fn tt_say_reply(self: &Arc<Self>, chat_id: i64, text: &str, reply_to: i64) {
        self.say(chat_id, text, None, None, &[], reply_to, None).await;
    }
    /// `await inlet._ask_user(question, chat_id, choices=choices)`.
    pub async fn tt_ask_user_choices(self: &Arc<Self>, question: &str, chat_id: i64, choices: &[Choice]) -> String {
        self.ask_user(question, chat_id, TASK_ASKS_TITLE, Some(choices), None, false, false, None, false).await
    }
    /// `await inlet._app_tool(name, args, chat_id)`.
    pub async fn tt_app_tool(self: &Arc<Self>, name: &str, args: &Obj, chat_id: i64) -> Obj {
        self.app_tool(name, args, chat_id).await
    }
    /// `inlet._keep_typing(chat_id, reason, secs)`.
    pub fn tt_keep_typing(self: &Arc<Self>, chat_id: i64, reason: &str, secs: f64) {
        self.keep_typing(Some(chat_id), reason, secs);
    }
    /// `inlet._stop_typing(chat_id, reason)`.
    pub fn tt_stop_typing(self: &Arc<Self>, chat_id: i64, reason: &str) {
        self.stop_typing(Some(chat_id), reason);
    }
    /// `inlet._typing.get(chat_id)`: its reasons, None when the chat is not in it.
    pub fn tt_typing(&self, chat_id: i64) -> Option<Vec<String>> {
        self.st.lock().typing.get(&chat_id).map(|reasons| {
            let mut out: Vec<String> = reasons.keys().cloned().collect();
            out.sort();
            out
        })
    }
    /// `chat_id in inlet._typing_tasks`.
    pub fn tt_typing_task(&self, chat_id: i64) -> bool {
        self.st.lock().typing_tasks.contains_key(&chat_id)
    }
    /// `inlet._start_thinking(chat_id)`.
    pub fn tt_start_thinking(self: &Arc<Self>, chat_id: i64) {
        self.start_thinking(Some(chat_id));
    }
    /// `inlet._draft_task is not None`.
    pub fn tt_draft_task(&self) -> bool {
        self.st.lock().draft_task.is_some()
    }
    /// `inlet._drafts_ok`.
    pub fn tt_drafts_ok(&self) -> bool {
        self.st.lock().drafts_ok
    }
    /// `await inlet._think_hard(question, chat_id)`.
    pub async fn tt_think_hard(self: &Arc<Self>, question: &str, chat_id: i64) -> Obj {
        self.think_hard(question, Some(chat_id)).await
    }
    /// `inlet._pending_answer`.
    pub fn tt_pending_answer(&self) -> Option<Pending> {
        self.st.lock().pending_answer.clone()
    }
    /// `inlet._pending_answer_chat`.
    pub fn tt_pending_answer_chat(&self) -> Option<i64> {
        self.st.lock().pending_answer_chat
    }
    /// `monkeypatch.setattr(websearch, "search", search)` for this inlet.
    pub fn tt_set_web_search(&self, search: Option<WebSearchFn>) {
        self.st.lock().web_search = search;
    }
    /// `inlet._agent = agent`.
    pub fn tt_set_agent(&self, agent: Option<Arc<dyn TaskAgent>>) {
        self.st.lock().agent = agent;
    }
    /// `await inlet._run_agent(goal, chat_id)`.
    pub async fn tt_run_agent(self: &Arc<Self>, goal: &str, chat_id: i64) {
        self.run_agent(goal, chat_id, None, None, &[], &[], None).await;
    }
    /// `await inlet._send_screen(chat_id, caption)`.
    pub async fn tt_send_screen(self: &Arc<Self>, chat_id: i64, caption: &str) -> Obj {
        self.send_screen(chat_id, caption, None).await
    }
    /// `await inlet._robot_tool(name, args)`.
    pub async fn tt_robot_tool(self: &Arc<Self>, name: &str, args: &Obj) -> Obj {
        self.robot_tool(name, args).await
    }
    /// `await inlet._judge_answer(question, choices, said)`.
    pub async fn tt_judge_answer(self: &Arc<Self>, question: &str, choices: &[&str], said: &str) -> Option<String> {
        let choices: Vec<String> = choices.iter().map(|c| c.to_string()).collect();
        self.judge_answer(question, &choices, said).await
    }
}

// ---- this unit's pure helpers, against what CPython 3 prints (python3 -c, 2026-10-20) ------------------------------

#[cfg(test)]
mod itl_tests {
    use super::*;

    #[test]
    fn repr_is_pythons_for_a_parsed_dict() {
        let v = json!({"a": "it's", "b": [1, 2.5, null, true], "c": "q\"", "d": "x\ny\u{1}\u{7f}\u{a0}\u{ad}\u{2028}"});
        // serde_json keeps insertion order (preserve_order) or sorts; either way each pair is Python's
        let got = itl_repr(&v);
        let d_pair = ["'d': 'x", r"\ny\x01\x7f\xa0\xad\u", "2028'"].concat();
        for pair in ["'a': \"it's\"", "'b': [1, 2.5, None, True]", "'c': 'q\"'", d_pair.as_str()] {
            assert!(got.contains(pair), "{got} lacks {pair}");
        }
        assert!(got.starts_with('{') && got.ends_with('}'));
        assert_eq!(itl_repr(&json!("a'b\"c")), r#"'a\'b"c'"#);
        assert_eq!(itl_repr(&json!("tab\there")), r"'tab\there'");
        assert_eq!(itl_repr(&json!({})), "{}");
    }

    #[test]
    fn float_repr_is_pythons() {
        let cases = [
            (1e16, "1e+16"),
            (1e-5, "1e-05"),
            (0.0001, "0.0001"),
            (123.0, "123.0"),
            (-0.0, "-0.0"),
            (1.5e300, "1.5e+300"),
            (2.5e-7, "2.5e-07"),
            (0.1, "0.1"),
            (f64::INFINITY, "inf"),
        ];
        for (x, want) in cases {
            assert_eq!(itl_float_repr(x), want, "{x}");
        }
        assert_eq!(itl_float_repr(f64::NAN), "nan");
    }

    #[test]
    fn round_is_pythons_on_the_binary_value() {
        assert_eq!(itl_round(0.125, 2), 0.12);
        assert_eq!(itl_round(2.675, 2), 2.67);
        assert_eq!(itl_round(1.05, 1), 1.1);
        assert_eq!(itl_round(0.00005, 4), 0.0001);
    }

    #[test]
    fn str_of_and_truthiness_are_pythons() {
        assert_eq!(itl_str_of(Some(&json!(0))), ""); // str(0 or "")
        assert_eq!(itl_str_of(Some(&json!([]))), "");
        assert_eq!(itl_str_of(Some(&json!(7))), "7");
        assert_eq!(itl_str_of(Some(&json!(true))), "True");
        assert_eq!(itl_str_of(None), "");
        assert_eq!(itl_py_str(None), "None"); // str(d.get(key))
        assert_eq!(itl_py_str(Some(&json!(null))), "None");
        assert!(itl_truthy_of(Some(&json!(0.5))) && !itl_truthy_of(Some(&json!(0.0))) && !itl_truthy_of(Some(&json!(""))));
        assert_eq!(itl_int(Some(&json!(3))), Some(3));
        assert_eq!(itl_int(Some(&json!(3.0))), None); // a float is not an int
        assert_eq!(itl_int(Some(&json!(true))), None); // nor is a bool
    }

    #[test]
    fn strip_is_pythons() {
        // ' \x1c a \x85\xa0'.strip() == 'a'
        assert_eq!(itl_strip(" \u{1c} a \u{85}\u{a0}"), "a");
        assert_eq!(itl_rstrip("a \n"), "a");
        assert_eq!(itl_rstrip_chars("yes!?.", ".!?"), "yes");
        assert_eq!(itl_strip("\u{200b}a"), "\u{200b}a"); // a zero-width space is no whitespace to Python either
    }

    #[test]
    fn a_recent_line_is_clipped_with_an_ellipsis_and_who_is_named() {
        let short = "a".repeat(RECENT_MESSAGES_LINE_CHARS);
        assert_eq!(clip_line(&short), short);
        let long = format!("{} {}", "b".repeat(RECENT_MESSAGES_LINE_CHARS - 2), "tail");
        let clipped = clip_line(&long);
        assert!(clipped.ends_with('…') && clipped.chars().count() <= RECENT_MESSAGES_LINE_CHARS);
        assert!(!clipped.trim_end_matches('…').ends_with(' ')); // rstripped before the ellipsis
        assert_eq!((who_word("user"), who_word("eve"), who_word("event")), ("owner", "you (eve)", "note"));
    }

    #[test]
    fn failures_and_refusals_are_the_dicts_python_returns() {
        assert_eq!(Value::Object(itl_failed("x")), json!({"ok": false, "reason": "x failed"}));
        assert_eq!(Value::Object(itl_refused("off")), json!({"ok": false, "reason": "off"}));
        assert_eq!(Value::Object(itl_obj(json!([1]), "y")), json!({"ok": false, "reason": "y failed"}));
        assert_eq!(itl_panic_name(&"boom"), "boom");
    }
}
