//! A port of `telegram.py`, lines 7594-7699: diagnose, MutedBotApi, make_inlet.

#[allow(unused_imports)]
use crate::prelude::*;
#[allow(unused_imports)]
use crate::telegram::*;

use std::sync::atomic::Ordering;

use crate::telegram::bot_api::wire;

/// py: telegram.py:7594 `def diagnose(environ: Any = None, api: Any = None) -> int:`
///
/// `eve-bridge telegram-check`: is the token good, and what is my numeric id?
///
/// Reads without acknowledging (no offset), so the daemon still sees every message afterwards. Prints
/// ids and first names only — never what anyone wrote. Run it with the daemon's inlet off: Telegram
/// allows one poller per bot and answers the second with 409.
///
/// Rust: `asyncio.run(check())` is the caller's runtime; this is `check()` itself.
pub async fn diagnose(environ: Option<&Environ>, api: Option<Arc<dyn TelegramApi>>) -> i64 {
    let token = env_get(environ, "EVE_TELEGRAM_TOKEN").unwrap_or_default().py_trim().to_string();
    if token.is_empty() {
        println!(
            "EVE_TELEGRAM_TOKEN is not set. Create a bot with @BotFather, then add the token to ~/.config/eve-bridge/env."
        );
        return 1;
    }
    let owners = owner_ids(&env_get(environ, "EVE_TELEGRAM_OWNER").unwrap_or_default());
    let lent = api.is_some();
    let bot: Arc<dyn TelegramApi> = match api {
        Some(api) => api,
        None => Arc::new(BotApi::new(&token, None)),
    };
    let polled = async {
        let me = bot.get_me().await?;
        let username = match me.get("username") {
            Some(name) if wire_truthy(Some(name)) => wire_str(name),
            _ => "?".to_string(),
        };
        println!("token ok: the bot is @{username}");
        bot.get_updates(None, 0).await
    }
    .await;
    if !lent {
        bot.close().await;
    }
    let updates = match polled {
        Ok(updates) => updates,
        Err(e) => {
            let hint = match e.code {
                401 => "the token is wrong",
                404 => "the token is malformed",
                409 => "another program is polling this bot (the daemon?): turn EVE_TELEGRAM off and restart it, then run this again",
                _ => "",
            };
            let tail = if hint.is_empty() { String::new() } else { format!(" — {hint}") };
            println!("the Bot API answered {e}{tail}");
            return 1;
        }
    };
    let mut seen: indexmap::IndexMap<i64, String> = indexmap::IndexMap::new();
    for update in &updates {
        let sender = update.get("message").and_then(Value::as_object).and_then(|m| m.get("from")).and_then(Value::as_object);
        if let Some(sender) = sender {
            if let Some(uid) = wire_int(sender.get("id")) {
                let name = match sender.get("first_name") {
                    Some(n) if wire_truthy(Some(n)) => wire_str(n),
                    _ => String::new(),
                };
                seen.insert(uid, name);
            }
        }
    }
    if seen.is_empty() {
        println!("no waiting messages. Send your bot any message from your phone, then run this again.");
    }
    for (uid, name) in &seen {
        let mark = if owners.contains(uid) { "  <- already an owner" } else { "" };
        println!("user id {uid} ({name}){mark}");
    }
    if !seen.is_empty() && owners.is_empty() {
        println!("put yours in ~/.config/eve-bridge/env as EVE_TELEGRAM_OWNER=<id>");
    }
    0
}

/// py: telegram.py:7641 `class MutedBotApi(BotApi):`
///
/// EVE_TELEGRAM=app: a BotApi that never reaches Telegram. Every send "works" (message id 0) and nothing
/// leaves the Mac; polling is never asked of it (TelegramInlet.run waits instead). The inlet then behaves as it does
/// with Telegram on, so app turns, calls, the watcher and the chief keep their one path; their lines reach the owner
/// through the transcript, which the Eve Link app reads.
///
/// Its sends are exactly what Telegram would have pushed to the phone, so they go to ``notifier`` (notify.py,
/// ntfy.sh) when there is one: sendMessage, sendPhoto and sendDocument only, never an edit, "typing…", a draft, a
/// reaction or a tap's answer. A reply to an Eve Link app turn goes to the app (TelegramInlet._to_app) and never
/// gets here.
/// (Python bases: BotApi.)
///
/// Rust: implements [`TelegramApi`] with BotApi's own method bodies, every request answered by
/// [`MutedBotApi::call`]. It has no HTTP client: nothing can leave the Mac.
pub struct MutedBotApi {
    pub(crate) token: String,
    /// Bot API calls not made, for the log and tests
    pub dropped: std::sync::atomic::AtomicI64,
    pub notifier: Option<Arc<crate::notify::Notifier>>,
}

impl MutedBotApi {
    /// py: telegram.py:7654 `def __init__(self, token: str = "", notifier: Any = None) -> None:   # no HTTP client: nothing to reach self._token = "" self.dropped = 0                   # Bot API calls not made, for the log and tests self.notifier = notifier hide_token(token)`
    pub fn new(token: &str, notifier: Option<Arc<crate::notify::Notifier>>) -> Self {
        hide_token(token); // other code (the Mini App's pin) may still quote the token in a log line
        Self { token: String::new(), dropped: std::sync::atomic::AtomicI64::new(0), notifier }
    }

    /// py: telegram.py:7660 `async def _call(self, method: str, data: dict[str, Any], files: Any = None) -> Any:`
    pub(crate) async fn call(&self, method: &str, data: &Obj, files: Option<&BotFiles>) -> Value {
        let _ = files;
        self.dropped.fetch_add(1, Ordering::SeqCst);
        if let Some(notifier) = &self.notifier {
            notifier.offer_send(method, data); // never blocks, never raises
        }
        match method {
            "getUpdates" => json!([]),
            "getMe" => json!({"id": 0, "is_bot": true, "username": "muted"}),
            m if m.starts_with("send") => json!({"message_id": 0}),
            _ => Value::Bool(true),
        }
    }

    /// py: telegram.py:7672 `async def close(self) -> None:`
    pub async fn close(&self) {
        if let Some(notifier) = &self.notifier {
            notifier.close().await;
        }
    }

    /// `api.dropped`.
    pub fn dropped(&self) -> i64 {
        self.dropped.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl TelegramApi for MutedBotApi {
    async fn get_me(&self) -> Result<Obj, BotApiError> {
        wire::get_me(self).await
    }

    async fn get_updates(&self, offset: Option<i64>, timeout: i64) -> Result<Vec<Obj>, BotApiError> {
        wire::get_updates(self, offset, timeout).await
    }

    async fn receive_image(&self, item: &eve_telegram_images::Attachment) -> Result<eve_telegram_images::ReceivedImage, ApiImageError> {
        wire::receive_image(self, item).await
    }

    async fn send_message(&self, chat_id: i64, text: &str, title: Option<&str>, subtitle: Option<&str>, buttons: &[String], reply_to: i64, force_reply: Option<&str>) -> Result<(), BotApiError> {
        wire::send_message(self, chat_id, text, title, subtitle, buttons, reply_to, force_reply).await
    }

    async fn send_inline(&self, chat_id: i64, text: &str, keyboard: &[Vec<(String, String, String)>], title: Option<&str>, subtitle: Option<&str>) -> Result<i64, BotApiError> {
        wire::send_inline(self, chat_id, text, keyboard, title, subtitle).await
    }

    async fn edit_message(&self, chat_id: i64, message_id: i64, text: &str, title: Option<&str>, subtitle: Option<&str>, keyboard: Option<&[Vec<(String, String, String)>]>) -> Result<(), BotApiError> {
        wire::edit_message(self, chat_id, message_id, text, title, subtitle, keyboard).await
    }

    async fn drop_buttons(&self, chat_id: i64, message_id: i64) -> Result<(), BotApiError> {
        wire::drop_buttons(self, chat_id, message_id).await
    }

    async fn answer_callback(&self, query_id: &str, text: &str) -> Result<(), BotApiError> {
        wire::answer_callback(self, query_id, text).await
    }

    async fn react(&self, chat_id: i64, message_id: i64, emoji: &str) -> Result<(), BotApiError> {
        wire::react(self, chat_id, message_id, emoji).await
    }

    async fn send_photo(&self, chat_id: i64, path: &Path, caption: &str) -> Result<(), BotApiError> {
        wire::send_photo(self, chat_id, path, caption).await
    }

    async fn send_document(&self, chat_id: i64, path: &Path, caption: &str) -> Result<(), BotApiError> {
        wire::send_document(self, chat_id, path, caption).await
    }

    async fn typing(&self, chat_id: i64) -> Result<(), BotApiError> {
        wire::typing(self, chat_id).await
    }

    async fn send_draft(&self, chat_id: i64, draft_id: i64, text: &str) -> Result<(), BotApiError> {
        wire::send_draft(self, chat_id, draft_id, text).await
    }

    async fn send_web_apps(&self, chat_id: i64, text: &str, buttons: &[(String, String)]) -> Result<(), BotApiError> {
        wire::send_web_apps(self, chat_id, text, buttons).await
    }

    async fn send_url_buttons(&self, chat_id: i64, text: &str, buttons: &[(String, String)]) -> Result<(), BotApiError> {
        wire::send_url_buttons(self, chat_id, text, buttons).await
    }

    async fn send_photo_web_apps(&self, chat_id: i64, jpeg: &[u8], caption: &str, buttons: &[(String, String)]) -> Result<(), BotApiError> {
        wire::send_photo_web_apps(self, chat_id, jpeg, caption, buttons).await
    }

    async fn set_commands(&self, commands: &[(String, String)], chat_id: i64) -> Result<(), BotApiError> {
        wire::set_commands(self, commands, chat_id).await
    }

    async fn close(&self) {
        MutedBotApi::close(self).await
    }

    fn muted(&self) -> bool {
        true
    }

    fn notifier(&self) -> Option<Arc<crate::notify::Notifier>> {
        self.notifier.clone()
    }
}

/// The Bot API `make_inlet` lends the inlet: muted in app-only mode (EVE_TELEGRAM=app), with the phone's notifier
/// when notify.py makes one; Telegram's own otherwise. `environ` is what the notifier reads (None: the process's).
pub fn make_api(config: &TelegramConfig, environ: Option<&Environ>) -> Arc<dyn TelegramApi> {
    if config.app_only {
        let notifier: Option<Arc<crate::notify::Notifier>> = crate::notify::make_notifier(environ).map(Into::into);
        Arc::new(MutedBotApi::new(&config.token, notifier))
    } else {
        Arc::new(BotApi::new(&config.token, None))
    }
}

/// py: telegram.py:7677 `def make_inlet(config: TelegramConfig, **lent: Any) -> Optional[TelegramInlet]:`
///
/// The real inlet, or None (with one log line) when it cannot run.
/// `**lent` is `lent`; `create` and `stream_create` are made here (make_response_creator, make_stream_creator).
/// The OpenRouter key and the notifier are read from `lent.environ` (None: the process's environment).
pub fn make_inlet(config: TelegramConfig, lent: InletOptions) -> Option<Arc<TelegramInlet>> {
    if !config.enabled {
        return None;
    }
    if !eve_openrouter::available(lent.environ.as_ref()) {
        tracing::warn!("telegram: OPENROUTER_API_KEY not set — the text door stays shut");
        return None;
    }
    use eve_computer_agent::responses::{make_response_creator, make_stream_creator, CreatorOptions};
    let made = make_response_creator(CreatorOptions::default())
        .and_then(|create| make_stream_creator(CreatorOptions::default()).map(|stream| (create, stream)));
    let (create, stream_create) = match made {
        Ok(made) => made,
        Err(e) => {
            tracing::warn!("telegram: not importable ({e}) — the text door stays shut");
            return None;
        }
    };
    let api = make_api(&config, lent.environ.as_ref());
    let mut lent = lent;
    lent.stream_create = Some(stream_create);
    Some(TelegramInlet::new(config, api, create, lent))
}
