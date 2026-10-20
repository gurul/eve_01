//! A port of `telegram.py`, lines 2194-2447: the Bot API client and token redaction.
//!
//! Skeleton: constants and prompts are verbatim (generated from the Python values); bodies are
//! `todo!()` until this file's unit fills them.

#[allow(unused_imports)]
use crate::prelude::*;
#[allow(unused_imports)]
use crate::telegram::*;

/// py: telegram.py:2196 `class BotApiError(Exception):`
///
/// A Bot API failure. Carries a code and Telegram's description — never a URL, because the URL is
/// the token.
/// (Python bases: Exception.)
#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct BotApiError {
    /// `str(error)`.
    pub message: String,
    pub code: i64,
    pub description: String,
}

impl BotApiError {
    /// py: telegram.py:2200 `def __init__(self, code: int, description: str) -> None:`
    pub fn new(code: i64, description: &str) -> Self {
        todo!("telegram.py:2200 BotApiError.__init__")
    }
}

// py: telegram.py:2206 `_hidden_tokens` (set): ported by this file's unit.

// py: telegram.py:2207 `_plain_record_factory` (NoneType): ported by this file's unit.

/// py: telegram.py:2210 `def _redacting_record_factory(*args: Any, **kwargs: Any) -> logging.LogRecord:`
/// (Python also takes *args, **kwargs.)
///
/// Rust: the redaction a log line goes through (every hidden token replaced), applied by the crate's log writes.
pub(crate) fn redacting_record_factory(message: &str) -> String {
    todo!("telegram.py:2210 _redacting_record_factory")
}

/// py: telegram.py:2224 `def hide_token(token: str) -> None:`
///
/// Keep the token out of every log record in the process, whoever writes it.
///
/// httpx logs each request line at INFO, URL included, and the Bot API URL *is* the token. A filter on the
/// "httpx" logger would miss httpcore's child loggers and anything else that quotes a URL (logger filters
/// are not inherited), so the record factory is wrapped instead: every record is checked as it is made.
/// Idempotent; ``unhide_tokens`` undoes it (tests).
pub fn hide_token(token: &str) {
    todo!("telegram.py:2224 hide_token")
}

/// py: telegram.py:2240 `def unhide_tokens() -> None:`
pub fn unhide_tokens() {
    todo!("telegram.py:2240 unhide_tokens")
}

/// py: telegram.py:2248 `class BotApi:`
///
/// The Bot API methods eve uses. ``client`` is an ``httpx.AsyncClient`` (tests hand it one
/// with a mock transport, so the real request and logging paths run).
pub struct BotApi {
    pub(crate) token: String,
    pub(crate) client: reqwest::Client,
}

impl BotApi {
    /// py: telegram.py:2252 `def __init__(self, token: str, client: Any = None) -> None:`
    pub fn new(token: &str, client: Option<reqwest::Client>) -> Self {
        todo!("telegram.py:2252 BotApi.__init__")
    }

    /// py: telegram.py:2261 `async def _call(self, method: str, data: dict[str, Any], files: Any = None) -> Any:`
    pub(crate) async fn call(&self, method: &str, data: &Obj, files: Option<&BotFiles>) -> Result<Value, BotApiError> {
        todo!("telegram.py:2261 BotApi._call")
    }

    /// py: telegram.py:2331 `async def _send_pieces(self, chat_id: int, text: str, title: Optional[str], subtitle: Optional[str], markup: Optional[dict[str, Any]], *, reply_to: int = 0) -> int:`
    pub(crate) async fn send_pieces(&self, chat_id: i64, text: &str, title: Option<&str>, subtitle: Option<&str>, markup: Option<&Obj>, reply_to: i64) -> Result<i64, BotApiError> {
        todo!("telegram.py:2331 BotApi._send_pieces")
    }

    /// py: telegram.py:2444 `async def close(self) -> None:`
    pub async fn close(&self) {
        todo!("telegram.py:2444 BotApi.close")
    }
}

/// A multipart upload's files: (form field, file name, bytes, content type), as httpx's `files=`.
pub type BotFiles = Vec<(String, String, Vec<u8>, String)>;

/// What the inlet needs of the Bot API: [`BotApi`] over HTTPS, [`MutedBotApi`] when eve runs without Telegram
/// (EVE_TELEGRAM=app), or a test's fake. Python reached these by duck typing on `inlet.api`.
#[async_trait::async_trait]
pub trait TelegramApi: Send + Sync {
    /// py: telegram.py:2283 `async def get_me(self) -> dict[str, Any]:`
    async fn get_me(&self) -> Result<Obj, BotApiError>;

    /// py: telegram.py:2287 `async def get_updates(self, offset: Optional[int], timeout: int = POLL_TIMEOUT_SECS) -> list[dict[str, Any]]:`
    async fn get_updates(&self, offset: Option<i64>, timeout: i64) -> Result<Vec<Obj>, BotApiError>;

    /// py: telegram.py:2295 `async def receive_image(self, item: telegram_images.Attachment) -> telegram_images.ReceivedImage:`
    async fn receive_image(&self, item: &eve_telegram_images::Attachment) -> Result<eve_telegram_images::ReceivedImage, ApiImageError>;

    /// py: telegram.py:2298 `async def send_message(self, chat_id: int, text: str, title: Optional[str] = None, subtitle: Optional[str] = None, buttons: Sequence[str] = (), *, reply_to: int = 0, force_reply: Optional[str] = None) -> None:`
    ///
    /// One message, composed by telegram_format (a bold ``title`` when the voice is not eve's, an
    /// italic ``subtitle`` when an identifier helps, the body as HTML paragraphs) and sent in pieces
    /// under the Bot API limit. A piece Telegram will not parse goes again as plain text: a message is
    /// never lost to markup. ``buttons`` become a one-time reply keyboard under the last piece: a tap
    /// sends that button's text as the owner's own message, so no callback path is needed. It is what
    /// a picker falls back to when its inline keyboard cannot be sent (TelegramInlet._send_choices).
    ///
    /// ``reply_to`` makes the first piece a reply to that message of the owner's (reply_parameters, with
    /// allow_sending_without_reply, so a request the owner has since deleted never stops the send).
    /// ``force_reply`` opens the owner's reply box on the last piece (ForceReply), with that text as the
    /// placeholder, or none when it is empty.
    async fn send_message(&self, chat_id: i64, text: &str, title: Option<&str>, subtitle: Option<&str>, buttons: &[String], reply_to: i64, force_reply: Option<&str>) -> Result<(), BotApiError>;

    /// py: telegram.py:2320 `async def send_inline(self, chat_id: int, text: str, keyboard: Sequence[Sequence[tuple[str, str, str]]], title: Optional[str] = None, subtitle: Optional[str] = None) -> int:`
    ///
    /// A message with an inline keyboard under its last piece → that piece's message id, which is what a
    /// later edit names. ``keyboard`` is rows of (label, callback_data, style); callback_data is a short key
    /// the inlet maps to what the tap means (1-64 bytes, InlineKeyboardButton), and style is "success",
    /// "danger", "primary" or "" for the app's own look. A tap sends nothing to the chat: it arrives as a
    /// callback_query (get_updates).
    async fn send_inline(&self, chat_id: i64, text: &str, keyboard: &[Vec<(String, String, String)>], title: Option<&str>, subtitle: Option<&str>) -> Result<i64, BotApiError>;

    /// py: telegram.py:2353 `async def edit_message(self, chat_id: int, message_id: int, text: str, title: Optional[str] = None, subtitle: Optional[str] = None, keyboard: Optional[Sequence[Sequence[tuple[str, str, str]]]] = None) -> None:`
    ///
    /// Rewrite one of the bot's own messages (editMessageText) and take its inline keyboard away: the
    /// empty ``inline_keyboard`` says so explicitly. ``keyboard`` (rows as in send_inline) keeps buttons on
    /// it instead: a progress message is edited with its Stop button still under it. Only the first piece:
    /// a prompt or a progress message is short. Unparseable markup goes again as plain text, as in
    /// send_message.
    async fn edit_message(&self, chat_id: i64, message_id: i64, text: &str, title: Option<&str>, subtitle: Option<&str>, keyboard: Option<&[Vec<(String, String, String)>]>) -> Result<(), BotApiError>;

    /// py: telegram.py:2372 `async def drop_buttons(self, chat_id: int, message_id: int) -> None:`
    ///
    /// Take a message's inline keyboard away and leave its text (editMessageReplyMarkup).
    async fn drop_buttons(&self, chat_id: i64, message_id: i64) -> Result<(), BotApiError>;

    /// py: telegram.py:2377 `async def answer_callback(self, query_id: str, text: str = "") -> None:`
    ///
    /// The answer every tap needs (answerCallbackQuery): until it comes, the owner's phone shows a
    /// progress bar on the button. ``text`` is a toast of 0-200 characters, or nothing.
    async fn answer_callback(&self, query_id: &str, text: &str) -> Result<(), BotApiError>;

    /// py: telegram.py:2385 `async def react(self, chat_id: int, message_id: int, emoji: str) -> None:`
    ///
    /// A reaction on one of the owner's messages: the receipt, instead of a line saying it arrived. An
    /// empty ``emoji`` takes the bot's reaction off again (setMessageReaction with an empty list).
    async fn react(&self, chat_id: i64, message_id: i64, emoji: &str) -> Result<(), BotApiError>;

    /// py: telegram.py:2391 `async def send_photo(self, chat_id: int, path: Path, caption: str = "") -> None:`
    async fn send_photo(&self, chat_id: i64, path: &Path, caption: &str) -> Result<(), BotApiError>;

    /// py: telegram.py:2396 `async def send_document(self, chat_id: int, path: Path, caption: str = "") -> None:`
    async fn send_document(&self, chat_id: i64, path: &Path, caption: &str) -> Result<(), BotApiError>;

    /// py: telegram.py:2401 `async def typing(self, chat_id: int) -> None:`
    ///
    /// "typing…" under the bot's name for 5 s at most, or until its next message (sendChatAction). The
    /// inlet repeats it while work goes on (TelegramInlet._keep_typing).
    async fn typing(&self, chat_id: i64) -> Result<(), BotApiError>;

    /// py: telegram.py:2406 `async def send_draft(self, chat_id: int, draft_id: int, text: str = "") -> None:`
    ///
    /// A draft in a private chat (sendMessageDraft): an empty ``text`` shows Telegram's "Thinking…"
    /// placeholder. ``draft_id`` is non-zero; the same id animates a change, a new one replaces the draft.
    /// A draft is not a message: it lasts 30 s, and the bot's next message clears it. The inlet sends only
    /// the empty placeholder (TelegramInlet._start_thinking).
    async fn send_draft(&self, chat_id: i64, draft_id: i64, text: &str) -> Result<(), BotApiError>;

    /// py: telegram.py:2416 `async def send_web_apps(self, chat_id: int, text: str, buttons: Sequence[tuple[str, str]]) -> None:`
    ///
    /// A message with inline ``web_app`` buttons, one per row: each opens its URL as a Mini App (with the
    /// owner's signed initData), not in a browser. Private chats only, which is all the door serves.
    async fn send_web_apps(&self, chat_id: i64, text: &str, buttons: &[(String, String)]) -> Result<(), BotApiError>;

    /// py: telegram.py:2422 `async def send_url_buttons(self, chat_id: int, text: str, buttons: Sequence[tuple[str, str]]) -> None:`
    ///
    /// A message with inline ``url`` buttons, one per row: each opens its link in the browser.
    async fn send_url_buttons(&self, chat_id: i64, text: &str, buttons: &[(String, String)]) -> Result<(), BotApiError>;

    /// py: telegram.py:2427 `async def send_photo_web_apps(self, chat_id: int, jpeg: bytes, caption: str, buttons: Sequence[tuple[str, str]]) -> None:`
    ///
    /// A picture (JPEG bytes, not a file) with inline ``web_app`` buttons under it: a finished app's
    /// screenshot and its Open button (apps_maker.ChatMaker). A multipart form carries every field as text, so
    /// the keyboard goes as JSON. The caption is written in code, not by a model, and leads with the app's own
    /// icon, so it is sent as it is (like send_web_apps), only cut to the caption limit.
    async fn send_photo_web_apps(&self, chat_id: i64, jpeg: &[u8], caption: &str, buttons: &[(String, String)]) -> Result<(), BotApiError>;

    /// py: telegram.py:2438 `async def set_commands(self, commands: Sequence[tuple[str, str]], chat_id: int) -> None:`
    ///
    /// The / menu for one chat only (setMyCommands with a BotCommandScopeChat scope): the owner's
    /// private chat shows eve's code words, and no other chat's menu changes.
    async fn set_commands(&self, commands: &[(String, String)], chat_id: i64) -> Result<(), BotApiError>;

    /// `api.close()`.
    async fn close(&self) {}

    /// `getattr(api, "muted", False)`: a MutedBotApi (EVE_TELEGRAM=app), which polling is never asked of.
    fn muted(&self) -> bool {
        false
    }
}

/// `receive_image`'s failures: Telegram's (BotApiError, a network OSError folded in) or the image's own
/// (telegram_images.ImageError).
#[derive(Debug, thiserror::Error)]
pub enum ApiImageError {
    #[error(transparent)]
    Api(#[from] BotApiError),
    #[error(transparent)]
    Image(#[from] eve_telegram_images::ImageError),
}

#[async_trait::async_trait]
impl TelegramApi for BotApi {
    async fn get_me(&self) -> Result<Obj, BotApiError> {
        todo!("telegram.py:2283 BotApi.get_me")
    }

    async fn get_updates(&self, offset: Option<i64>, timeout: i64) -> Result<Vec<Obj>, BotApiError> {
        todo!("telegram.py:2287 BotApi.get_updates")
    }

    async fn receive_image(&self, item: &eve_telegram_images::Attachment) -> Result<eve_telegram_images::ReceivedImage, ApiImageError> {
        todo!("telegram.py:2295 BotApi.receive_image")
    }

    async fn send_message(&self, chat_id: i64, text: &str, title: Option<&str>, subtitle: Option<&str>, buttons: &[String], reply_to: i64, force_reply: Option<&str>) -> Result<(), BotApiError> {
        todo!("telegram.py:2298 BotApi.send_message")
    }

    async fn send_inline(&self, chat_id: i64, text: &str, keyboard: &[Vec<(String, String, String)>], title: Option<&str>, subtitle: Option<&str>) -> Result<i64, BotApiError> {
        todo!("telegram.py:2320 BotApi.send_inline")
    }

    async fn edit_message(&self, chat_id: i64, message_id: i64, text: &str, title: Option<&str>, subtitle: Option<&str>, keyboard: Option<&[Vec<(String, String, String)>]>) -> Result<(), BotApiError> {
        todo!("telegram.py:2353 BotApi.edit_message")
    }

    async fn drop_buttons(&self, chat_id: i64, message_id: i64) -> Result<(), BotApiError> {
        todo!("telegram.py:2372 BotApi.drop_buttons")
    }

    async fn answer_callback(&self, query_id: &str, text: &str) -> Result<(), BotApiError> {
        todo!("telegram.py:2377 BotApi.answer_callback")
    }

    async fn react(&self, chat_id: i64, message_id: i64, emoji: &str) -> Result<(), BotApiError> {
        todo!("telegram.py:2385 BotApi.react")
    }

    async fn send_photo(&self, chat_id: i64, path: &Path, caption: &str) -> Result<(), BotApiError> {
        todo!("telegram.py:2391 BotApi.send_photo")
    }

    async fn send_document(&self, chat_id: i64, path: &Path, caption: &str) -> Result<(), BotApiError> {
        todo!("telegram.py:2396 BotApi.send_document")
    }

    async fn typing(&self, chat_id: i64) -> Result<(), BotApiError> {
        todo!("telegram.py:2401 BotApi.typing")
    }

    async fn send_draft(&self, chat_id: i64, draft_id: i64, text: &str) -> Result<(), BotApiError> {
        todo!("telegram.py:2406 BotApi.send_draft")
    }

    async fn send_web_apps(&self, chat_id: i64, text: &str, buttons: &[(String, String)]) -> Result<(), BotApiError> {
        todo!("telegram.py:2416 BotApi.send_web_apps")
    }

    async fn send_url_buttons(&self, chat_id: i64, text: &str, buttons: &[(String, String)]) -> Result<(), BotApiError> {
        todo!("telegram.py:2422 BotApi.send_url_buttons")
    }

    async fn send_photo_web_apps(&self, chat_id: i64, jpeg: &[u8], caption: &str, buttons: &[(String, String)]) -> Result<(), BotApiError> {
        todo!("telegram.py:2427 BotApi.send_photo_web_apps")
    }

    async fn set_commands(&self, commands: &[(String, String)], chat_id: i64) -> Result<(), BotApiError> {
        todo!("telegram.py:2438 BotApi.set_commands")
    }
}
