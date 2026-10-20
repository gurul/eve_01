//! A port of `telegram.py`, lines 7340-7593: media, the screen, the robot body, files, photos, think_hard.

#[allow(unused_imports)]
use crate::prelude::*;
#[allow(unused_imports)]
use crate::telegram::*;

use futures::future::BoxFuture;

/// scene.LOOK_FRESH_SECS: `scene.look()`'s default freshness (the scene crate is not a dependency here).
const LOOK_FRESH_SECS: f64 = 4.0;

/// `{"ok": False, "reason": reason}`.
fn refused(reason: &str) -> Obj {
    let mut r = Obj::new();
    r.insert("ok".into(), Value::Bool(false));
    r.insert("reason".into(), Value::String(reason.to_string()));
    r
}

/// A JSON result as the object a tool returns (a non-object is `{"ok": True, "result": value}`).
fn as_obj(value: Value) -> Obj {
    match value {
        Value::Object(o) => o,
        other => {
            let mut r = Obj::new();
            r.insert("ok".into(), Value::Bool(true));
            r.insert("result".into(), other);
            r
        }
    }
}

/// `self.__dict__.setdefault("_sent_files", {})`: per inlet, (chat, path, size, mtime_ns) → when it was sent.
type SentKey = (i64, String, u64, i128);
static SENT_FILES: LazyLock<Mutex<HashMap<usize, HashMap<SentKey, f64>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// `time.time()`.
fn now_secs() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

/// `text[:n]`, in characters.
fn head_chars(text: &str, n: usize) -> String {
    text.chars().take(n).collect()
}

/// Why a picture or file did not go: the Bot API refused it, or a file could not be read or written.
#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    #[error(transparent)]
    Api(#[from] BotApiError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl TelegramInlet {
    /// py: telegram.py:7340 `async def _send_media(self, chat_id: int, path: Optional[Path] = None, caption: str = "", *, document: bool = False, data: Optional[bytes] = None, name: str = "", buttons: Sequence[tuple[str, str]] = ()) -> None:`
    ///
    /// Every picture or file eve sends the owner goes through here (owner, 2026-10-09, in Eve Link: "It can't
    /// send attachments" / "Like images"; with Telegram app-only the photos went nowhere the owner could see). First a
    /// copy for the Eve Link app (``_keep_for_app``), then the Bot API exactly as before: Telegram when it is on,
    /// the notifier when it is muted (MutedBotApi). ``document``: sendDocument of ``path``; ``data`` (JPEG bytes):
    /// a picture with Web App ``buttons`` under it (a built app's, send_photo_web_apps); otherwise sendPhoto of
    /// ``path``. A Bot API failure is raised as before, unless the app has the copy: then the owner has the picture,
    /// and the failure is one warning line.
    ///
    /// Rust: the raise is the `Err`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn send_media(self: &Arc<Self>, chat_id: i64, path: Option<&Path>, caption: &str, document: bool, data: Option<&[u8]>, name: &str, buttons: &[(String, String)]) -> Result<(), BotApiError> {
        let name = if name.is_empty() {
            path.and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
        } else {
            name.to_string()
        };
        let kept = self.keep_for_app(path, data, &name, caption).await;
        if kept && self.api.muted() && ctx().app.is_some() {
            // Asked for from the app, which shows it now: no ntfy push for what is already on the screen.
            return Ok(());
        }
        let sent = if let Some(data) = data {
            self.api.send_photo_web_apps(chat_id, data, caption, buttons).await
        } else if let Some(path) = path {
            if document {
                self.api.send_document(chat_id, path, caption).await
            } else {
                self.api.send_photo(chat_id, path, caption).await
            }
        } else {
            Err(BotApiError { message: "no picture or file to send".into(), code: 0, description: "no picture or file to send".into() })
        };
        match sent {
            Ok(()) => Ok(()),
            Err(e) if !kept => Err(e),
            // re-raised unless the app has it
            Err(_) => {
                tracing::warn!("telegram: a picture or file reached the app only (BotApiError)");
                Ok(())
            }
        }
    }

    /// py: telegram.py:7366 `async def _send_app_photo(self, chat_id: int, jpeg: bytes, caption: str, buttons: Sequence[tuple[str, str]]) -> None:`
    ///
    /// apps_maker.ChatMaker's ``send_photo``: a finished app's picture and its Open button (_send_media).
    pub(crate) async fn send_app_photo(self: &Arc<Self>, chat_id: i64, jpeg: &[u8], caption: &str, buttons: &[(String, String)]) -> Result<(), BotApiError> {
        self.send_media(chat_id, None, caption, false, Some(jpeg), "app.jpg", buttons).await
    }

    /// py: telegram.py:7371 `async def _keep_for_app(self, path: Optional[Path], data: Optional[bytes], name: str, caption: str) -> bool:`
    ///
    /// The app's copy of one picture or file → whether the app has it: the bytes into the link door's upload store
    /// (link_uploads.UploadStore.keep) and eve's ``media`` transcript line with ``att``, the caption as its text,
    /// which /api/link/messages returns as eve's message with the attachment. Over ``link_uploads.MAX_BYTES``, the
    /// line names the file instead (False). Nothing without the store or a transcript (no app to show it). The line
    /// is the transcript's only: never the model's chat history, since the model had the picture as its tool's
    /// result, and no prompt block reads the kind; the transcript ``read`` and ``search`` tools do show it, as one
    /// line of what was said that day. Never raises; logs carry sizes and kinds, never a name.
    pub(crate) async fn keep_for_app(self: &Arc<Self>, path: Option<&Path>, data: Option<&[u8]>, name: &str, caption: &str) -> bool {
        let Some(store) = self.link_uploads.lock().clone() else {
            return false;
        };
        if self.memory.is_none() {
            return false;
        }
        let caption = head_chars(caption.py_trim(), MAX_CAPTION_CHARS);
        let bytes: Vec<u8> = match data {
            Some(d) => d.to_vec(),
            None => {
                let Some(path) = path else {
                    return false;
                };
                let size = match std::fs::metadata(path) {
                    Ok(m) => m.len(),
                    Err(_) => {
                        tracing::warn!("telegram: no app copy of a picture or file (OSError)");
                        return false;
                    }
                };
                if size as usize > eve_link_uploads::MAX_BYTES {
                    let mb = std::cmp::max(1, py_round(size as f64 / (1024.0 * 1024.0)));
                    let line = format!("{name} ({mb} MB) is too large to show here.");
                    let text = if caption.is_empty() { line } else { format!("{caption}\n{line}") };
                    self.record("eve", "media", &text, "", None, false, None);
                    return false;
                }
                match std::fs::read(path) {
                    Ok(b) => b,
                    Err(_) => {
                        tracing::warn!("telegram: no app copy of a picture or file (OSError)");
                        return false;
                    }
                }
            }
        };
        let name_owned = name.to_string();
        let kept = tokio::task::spawn_blocking(move || store.keep(&bytes, &name_owned)).await;
        let upload = match kept {
            Ok(Ok(upload)) => upload,
            Ok(Err(_)) => {
                tracing::warn!("telegram: no app copy of a picture or file (UploadError)");
                return false;
            }
            Err(_) => {
                tracing::warn!("telegram: no app copy of a picture or file (OSError)");
                return false;
            }
        };
        let att: Vec<HashMap<String, String>> = eve_link::link_chat::att_record(&[upload])
            .into_iter()
            .filter_map(|row| row.as_object().cloned())
            .map(|row| row.into_iter().map(|(k, v)| (k, v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string()))).collect())
            .collect();
        self.record("eve", "media", &caption, "", Some(&att), false, None)
    }

    /// py: telegram.py:7403 `async def _send_screen(self, chat_id: int, caption: str, *, agent: Any = None) -> dict[str, Any]:`
    ///
    /// Rust: ``agent`` given (any value) is the Codex chat, the one caller that names one (``agent=self._codex``):
    /// CodexChat has neither ``browser_shot`` nor ``provider``, so its screen is the desktop's, as in Python. With
    /// none, the Codex chat when this is its chat, else the running task's agent.
    pub(crate) async fn send_screen(self: &Arc<Self>, chat_id: i64, caption: &str, agent: Option<&Value>) -> Obj {
        let task_agent: Option<Arc<dyn TaskAgent>> = if agent.is_some() {
            None
        } else {
            let st = self.st.lock();
            if st.codex_chat == Some(chat_id) { None } else { st.agent.clone() }
        };
        if let Some(agent) = &task_agent {
            // The Chrome lane (chrome_lane.py): the page in the owner's Chrome, full size — never the desktop.
            if let Some(shot) = agent.browser_shot().await {
                let label = head_chars(if caption.is_empty() { &shot.label } else { caption }, MAX_CAPTION_CHARS);
                let sent = self.send_temp(chat_id, &shot.data, &format!("browser{}", shot.suffix), &label).await;
                if sent.is_err() {
                    return refused("The browser picture could not be delivered.");
                }
                let mut r = Obj::new();
                r.insert("ok".into(), Value::Bool(true));
                r.insert("sent".into(), Value::Bool(true));
                r.insert("source".into(), json!("chrome_lane"));
                return r;
            }
            if agent.provider() == "codex" && (agent.browser_used() || agent.running()) {
                let Some((data, suffix)) = agent.browser_screenshot() else {
                    return refused("No verified picture from the task browser tab is available yet.");
                };
                // Reuse validated bytes from this task's own cua result, never the foreground desktop.
                let label = format!("Task browser tab {} (last captured view)", agent.browser_tab_id());
                if self.send_temp(chat_id, &data, &format!("browser{suffix}"), &label).await.is_err() {
                    return refused("The browser picture could not be delivered.");
                }
                let mut r = Obj::new();
                r.insert("ok".into(), Value::Bool(true));
                r.insert("sent".into(), Value::Bool(true));
                r.insert("source".into(), json!("task_browser"));
                return r;
            }
        }
        let screen = self.screen.clone();
        let Some(path) = tokio::task::spawn_blocking(move || screen()).await.ok().flatten() else {
            return refused("could not capture the screen (is Screen Recording allowed for eve?)");
        };
        let suffix = path.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
        let sent = self.send_media(chat_id, Some(&path), caption, false, None, &format!("Screen{suffix}"), &[]).await;
        let _ = std::fs::remove_file(&path);
        if let Err(e) = sent {
            // Python raises out of the handler; _tool reports it as the tool failing
            tracing::error!("telegram: screenshot failed ({e})");
            return refused("screenshot failed");
        }
        let mut r = Obj::new();
        r.insert("ok".into(), Value::Bool(true));
        r.insert("sent".into(), Value::Bool(true));
        r
    }

    /// The bytes into a fresh temporary folder as `file`, sent as a picture, the folder gone after.
    async fn send_temp(self: &Arc<Self>, chat_id: i64, data: &[u8], file: &str, caption: &str) -> Result<(), MediaError> {
        let folder = tempfile::Builder::new().prefix("eve-browser-").tempdir()?;
        let path = folder.path().join(file);
        std::fs::write(&path, data)?;
        self.send_media(chat_id, Some(&path), caption, false, None, "", &[]).await?;
        Ok(())
    }

    /// py: telegram.py:7446 `async def _robot_tool(self, name: str, args: dict[str, Any]) -> dict[str, Any]:`
    ///
    /// The robot's own tools, as the voice has them (voice_agent._slow_tool / _tool). In stealth the
    /// robot stays asleep: the camera may still look (it moves nothing), the head and the explorer may not.
    pub(crate) async fn robot_tool(self: &Arc<Self>, name: &str, args: &Obj) -> Obj {
        if name == "look" {
            let Some(scene) = &self.scene else {
                return refused("vision is not set up on this computer");
            };
            return as_obj(scene.look(LOOK_FRESH_SECS, None).await);
        }
        if matches!(name, "look_around" | "find" | "move_head" | "go_explore") && self.stealth() {
            return refused("stealth mode: the robot is playing asleep and will not move");
        }
        if name == "look_around" || name == "find" {
            let (Some(head), Some(scene)) = (&self.head, &self.scene) else {
                return refused("head control or vision is not set up on this computer");
            };
            let scene: &dyn eve_head::Scene = scene.as_ref();
            if name == "look_around" {
                return as_obj(eve_head::look_around(head, scene, eve_head::SWEEP, eve_head::SETTLE_SECS).await);
            }
            let target = match args.get("target") {
                Some(Value::String(s)) => s.clone(),
                Some(v) if crate::telegram::inlet::turn::truthy(Some(v)) => v.to_string(),
                _ => String::new(),
            };
            return as_obj(
                eve_head::find(head, scene, &target, eve_head::SWEEP, eve_head::SETTLE_SECS, eve_head::DEFAULT_HOLD_SECS).await,
            );
        }
        if name == "move_head" {
            let Some(head) = &self.head else {
                return refused("head control is not set up on this computer");
            };
            let num = |key: &str| -> Option<f64> {
                match args.get(key) {
                    Some(Value::Number(n)) => n.as_f64(),
                    _ => None,
                }
            };
            let hold = num("hold_secs").unwrap_or(eve_head::DEFAULT_HOLD_SECS);
            let relative = args.get("relative") == Some(&Value::Bool(true));
            return as_obj(head.move_to(num("yaw"), num("pitch"), relative, hold).await);
        }
        if name == "go_explore" {
            let Some(on_explore) = &self.on_explore else {
                return refused("exploring is not set up on this computer");
            };
            if self.task_running() {
                return refused("a task is running; stop it first");
            }
            // ExploreRefused and friends carry a reason
            return match on_explore().await {
                Ok(()) => {
                    let mut r = Obj::new();
                    r.insert("ok".into(), Value::Bool(true));
                    r
                }
                Err(e) => {
                    let reason = e.to_string();
                    refused(if reason.is_empty() { "Exception" } else { &reason })
                }
            };
        }
        if name == "take_notes" {
            let action = match args.get("action") {
                Some(Value::String(s)) if !s.is_empty() => s.clone(),
                Some(v) if crate::telegram::inlet::turn::truthy(Some(v)) => v.to_string(),
                _ => "status".to_string(),
            };
            let phone_notes = self.phone_notes.lock().clone();
            if let Some(phone_notes) = phone_notes {
                // The phone's when it was said on its Eve Link call, or while its notes are on; None otherwise.
                let me = self.me.clone();
                let later: Later = Arc::new(move |done: Box<dyn FnOnce() + Send>| {
                    if let Some(me) = me.upgrade() {
                        me.st.lock().after_turn.push(done);
                    }
                });
                if let Some(phone) = phone_notes(action.clone(), self.call_reader(), later).await {
                    return phone;
                }
            }
            let Some(notes) = &self.notes else {
                return refused("note taking is not set up on this computer");
            };
            let taker = notes();
            if action == "start" {
                return as_obj(taker.start());
            }
            if action == "stop" {
                return as_obj(taker.stop("asked from Telegram").await);
            }
            let mut r = Obj::new();
            r.insert("ok".into(), Value::Bool(true));
            r.extend(taker.status());
            return r;
        }
        if name == "set_sound" {
            let Some(Value::Bool(on)) = args.get("on") else {
                return refused("on must be true or false");
            };
            let Some(on_sound) = &self.on_sound else {
                return refused("sound control is not set up on this computer");
            };
            on_sound(*on);
            let mut r = Obj::new();
            r.insert("ok".into(), Value::Bool(true));
            r.insert("sound".into(), json!(if *on { "on" } else { "off" }));
            return r;
        }
        let claim_raw = match args.get("claim") {
            Some(Value::String(s)) => s.clone(),
            Some(v) if crate::telegram::inlet::turn::truthy(Some(v)) => v.to_string(),
            _ => String::new(),
        };
        let claim = claim_raw.split(eve_envfile::is_py_space).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ");
        if claim.is_empty() {
            return refused("nothing to remember");
        }
        let Some(memory) = self.memory.clone() else {
            return refused("permanent memory is not set up on this computer");
        };
        // records.star: starred.md, for good
        let kept = tokio::task::spawn_blocking(move || memory.star(&claim)).await.ok().flatten();
        match kept {
            Some(kept) if !kept.is_empty() => {
                let mut r = Obj::new();
                r.insert("ok".into(), Value::Bool(true));
                r.insert("kept".into(), Value::String(kept));
                r
            }
            _ => refused("could not write it down"),
        }
    }

    /// py: telegram.py:7517 `async def _send_file(self, chat_id: int, raw: str, caption: str) -> dict[str, Any]:`
    pub(crate) async fn send_file(self: &Arc<Self>, chat_id: i64, raw: &str, caption: &str) -> Obj {
        let (real, why) = resolve_owner_path(raw, None);
        let Some(real) = real else {
            return refused(&why);
        };
        if real.is_dir() {
            return refused("that is a folder; name a file in it (list_files shows them)");
        }
        if !real.is_file() {
            return refused("not a regular file");
        }
        let Ok(stat) = std::fs::metadata(&real) else {
            tracing::error!("telegram: send_file failed");
            return refused("send_file failed");
        };
        let size = stat.len();
        if size as usize > MAX_DOCUMENT_BYTES {
            return refused(&format!("too big for Telegram ({} MB; the limit is 50 MB)", size / (1024 * 1024)));
        }
        // The same file to the same chat once (owner, 2026-10-10: "why did it send two pictures"): a task hands over
        // the files it wrote, and the chat turn that waited on it then sent the same photo again.
        let mtime_ns: i128 = stat
            .modified()
            .ok()
            .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos() as i128)
            .unwrap_or(0);
        let key: SentKey = (chat_id, real.to_string_lossy().into_owned(), size, mtime_ns);
        let now = now_secs();
        let file_name = real.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        {
            let mut all = SENT_FILES.lock();
            let sent_files = all.entry(Arc::as_ptr(self) as usize).or_default();
            sent_files.retain(|_, at| now - *at <= RESENT_FILE_SECS);
            if sent_files.contains_key(&key) {
                tracing::info!("telegram: that file was just sent; not again ({} bytes)", size);
                let mut r = Obj::new();
                r.insert("ok".into(), Value::Bool(true));
                r.insert("sent".into(), Value::String(file_name));
                r.insert("bytes".into(), json!(size));
                r.insert("note".into(), json!("already sent a moment ago; not sent twice"));
                return r;
            }
            sent_files.insert(key, now);
        }
        if self.send_media(chat_id, Some(&real), caption, true, None, "", &[]).await.is_err() {
            // Python raises out of the handler; _tool reports it as the tool failing
            tracing::error!("telegram: send_file failed");
            return refused("send_file failed");
        }
        tracing::info!("telegram: sent a file ({} bytes)", size); // the size, never the name
        let mut r = Obj::new();
        r.insert("ok".into(), Value::Bool(true));
        r.insert("sent".into(), Value::String(file_name));
        r.insert("bytes".into(), json!(size));
        r
    }

    /// py: telegram.py:7544 `async def _take_photo(self, note: str, chat_id: int) -> dict[str, Any]:`
    pub(crate) async fn take_photo(self: &Arc<Self>, note: &str, chat_id: i64) -> Obj {
        let Some(on_photo) = &self.on_photo else {
            return refused("no camera to take a photo with right now");
        };
        let shot = on_photo(note.to_string()).await;
        let path = shot.get("path").filter(|p| crate::telegram::inlet::turn::truthy(Some(p)));
        if !crate::telegram::inlet::turn::truthy(shot.get("ok")) || path.is_none() {
            let reason = match shot.get("reason") {
                Some(Value::String(s)) if !s.is_empty() => s.clone(),
                Some(v) if crate::telegram::inlet::turn::truthy(Some(v)) => v.to_string(),
                _ => "the camera gave no picture just now".to_string(),
            };
            return refused(&reason);
        }
        let path = match path {
            Some(Value::String(s)) => PathBuf::from(s),
            Some(v) => PathBuf::from(v.to_string()),
            None => PathBuf::new(),
        };
        let caption = match shot.get("caption") {
            Some(Value::String(s)) => s.clone(),
            Some(v) if crate::telegram::inlet::turn::truthy(Some(v)) => v.to_string(),
            _ => String::new(),
        };
        if self.send_media(chat_id, Some(&path), &caption, false, None, "", &[]).await.is_err() {
            // Python raises out of the handler; _tool reports it as the tool failing
            tracing::error!("telegram: take_photo failed");
            return refused("take_photo failed");
        }
        let mut r = Obj::new();
        r.insert("ok".into(), Value::Bool(true));
        r.insert("sent".into(), Value::Bool(true));
        r.insert("shows".into(), Value::String(caption));
        r
    }

    /// py: telegram.py:7554 `async def _think_hard(self, question: str, chat_id: Optional[int] = None) -> dict[str, Any]:`
    ///
    /// Rust: the thinker raising is ``{"ok": False, "reason": "think_hard failed"}``, the result _tool makes of it.
    pub(crate) async fn think_hard(self: &Arc<Self>, question: &str, chat_id: Option<i64>) -> Obj {
        if question.is_empty() {
            return refused("empty question");
        }
        let Some(thinker) = self.thinker.clone() else {
            return refused("deep reasoning is off on this computer; answer as best you can");
        };
        // The slow brain can outlast a turn's "typing…": it keeps its own for as long as it may take.
        self.keep_typing(chat_id, "think", TYPING_THINK_SECS);
        struct Stop(Arc<TelegramInlet>, Option<i64>);
        impl Drop for Stop {
            fn drop(&mut self) {
                self.0.stop_typing(self.1, "think");
            }
        }
        let _stop = Stop(self.clone(), chat_id);
        // The slow brain gets who the owner is and what was said today (owner, 2026-09-23): "given what I
        // told you this morning" needs it. With no memory it is asked exactly as before.
        let context = if self.memory.is_some() {
            let me = self.clone();
            tokio::task::spawn_blocking(move || me.think_context()).await.unwrap_or_default()
        } else {
            String::new()
        };
        match thinker(question.to_string(), context).await {
            Ok(answer) => answer,
            Err(_) => {
                tracing::error!("telegram: think_hard failed");
                refused("think_hard failed")
            }
        }
    }
}

/// Python's `round(x)` for a non-negative float: half to even.
fn py_round(x: f64) -> u64 {
    let r = x.round_ties_even();
    if r < 0.0 { 0 } else { r as u64 }
}

pub const ROBOT_TOOLS: &[&str] = &["look", "look_around", "find", "move_head", "go_explore", "set_sound", "remember", "take_notes"];

/// One fixed tool's handler, called as `handler(inlet, name, args, chat_id)`.
pub(crate) type ToolHandler = for<'a> fn(&'a Arc<TelegramInlet>, &'a str, &'a Obj, i64) -> BoxFuture<'a, Obj>;

macro_rules! handler {
    ($method:ident) => {{
        fn call<'a>(inlet: &'a Arc<TelegramInlet>, name: &'a str, args: &'a Obj, chat_id: i64) -> BoxFuture<'a, Obj> {
            Box::pin(inlet.$method(name, args, chat_id))
        }
        call as ToolHandler
    }};
}

/// py: telegram.py:7575 `TOOL_HANDLERS`
///
/// TelegramInlet._tool's fixed tools: name -> handler, called as handler(inlet, name, args, chat_id). Checked
/// before the owner's apps and the second brain, exactly as the if-chain it replaced (2026-09-23).
pub(crate) static TOOL_HANDLERS: LazyLock<indexmap::IndexMap<&'static str, ToolHandler>> = LazyLock::new(|| {
    let mut m: indexmap::IndexMap<&'static str, ToolHandler> = indexmap::IndexMap::new();
    m.insert("start_task", handler!(tool_start_task));
    m.insert("opentable_search", handler!(tool_opentable_search));
    m.insert("steer_task", handler!(tool_steer_task));
    m.insert("stop_task", handler!(tool_stop_task));
    m.insert("recent_messages", handler!(tool_recent_messages));
    m.insert("start_coding_session", handler!(tool_start_coding_session));
    m.insert("take_photo", handler!(tool_take_photo));
    m.insert("screenshot", handler!(tool_screenshot));
    m.insert("send_file", handler!(tool_send_file));
    m.insert("list_files", handler!(tool_list_files));
    m.insert(eve_menu_tools::READ_FILE, handler!(tool_read_file));
    for robot in ROBOT_TOOLS {
        m.insert(robot, handler!(tool_robot));
    }
    m.insert(eve_web::websearch::TOOL_NAME, handler!(tool_web_search));
    for menu in eve_menu_tools::TOOL_NAMES {
        m.insert(menu, handler!(tool_menu));
    }
    m.insert(eve_events::TOOL_NAME, handler!(tool_events));
    m
});
