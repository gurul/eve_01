//! A port of `telegram.py`, lines 2194-2447: the Bot API client and token redaction.

#[allow(unused_imports)]
use crate::prelude::*;
#[allow(unused_imports)]
use crate::telegram::*;

use eve_telegram_format as fmt;

// ---- the Bot API ------------------------------------------------------------------------------

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
        Self { message: format!("{code}: {description}"), code, description: description.to_string() }
    }
}

/// py: telegram.py:2206 `_hidden_tokens`: every token no log line may show.
static HIDDEN_TOKENS: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

// py: telegram.py:2207 `_plain_record_factory`: Python keeps the logging module's own record factory to put back;
// Rust has no global factory to wrap. The redaction is [`redacting_record_factory`], applied to every formatted log
// line by [`RedactingWriter`] (the daemon's subscriber writes through it).

/// py: telegram.py:2210 `def _redacting_record_factory(*args: Any, **kwargs: Any) -> logging.LogRecord:`
/// (Python also takes *args, **kwargs.)
///
/// Rust: the redaction a log line goes through (every hidden token replaced), applied by the crate's log writes.
pub(crate) fn redacting_record_factory(message: &str) -> String {
    let tokens = HIDDEN_TOKENS.lock();
    let mut message = message.to_string();
    for token in tokens.iter() {
        if message.contains(token.as_str()) {
            message = message.replace(token.as_str(), "<token>");
        }
    }
    message
}

/// A log line with every hidden token replaced by "<token>" (what `_redacting_record_factory` makes of a record).
pub fn redact(message: &str) -> String {
    redacting_record_factory(message)
}

/// A log writer that redacts every hidden token from what passes through it (the record factory's job in Python):
/// `tracing_subscriber::fmt().with_writer(|| RedactingWriter(std::io::stderr()))`. One write is one formatted
/// line, as tracing-subscriber writes them.
pub struct RedactingWriter<W: std::io::Write>(pub W);

impl<W: std::io::Write> std::io::Write for RedactingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let any_hidden = !HIDDEN_TOKENS.lock().is_empty();
        if !any_hidden {
            return self.0.write(buf);
        }
        let text = String::from_utf8_lossy(buf);
        let clean = redacting_record_factory(&text);
        self.0.write_all(clean.as_bytes())?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
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
    if token.is_empty() {
        return;
    }
    HIDDEN_TOKENS.lock().insert(token.to_string());
}

/// py: telegram.py:2240 `def unhide_tokens() -> None:`
pub fn unhide_tokens() {
    HIDDEN_TOKENS.lock().clear();
}

/// py: telegram.py:2248 `class BotApi:`
///
/// The Bot API methods eve uses. ``client`` is an ``httpx.AsyncClient`` (tests hand it one
/// with a mock transport, so the real request and logging paths run).
///
/// Rust: the client is an [`HttpTransport`] (the real one over reqwest; a test lends a fake, as httpx's
/// MockTransport): [`BotApi::with_http`].
pub struct BotApi {
    pub(crate) token: String,
    pub(crate) http: Http,
}

impl BotApi {
    /// py: telegram.py:2252 `def __init__(self, token: str, client: Any = None) -> None:`
    ///
    /// With no client: one whose read waits out the long poll (POLL_TIMEOUT_SECS + 15 s) and whose connect gives up
    /// after 10 s, as `httpx.Timeout(POLL_TIMEOUT_SECS + 15.0, connect=10.0)`.
    pub fn new(token: &str, client: Option<reqwest::Client>) -> Self {
        let client = client.unwrap_or_else(|| {
            reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(10))
                .read_timeout(std::time::Duration::from_secs_f64(POLL_TIMEOUT_SECS + 15.0))
                .build()
                .unwrap_or_default()
        });
        Self::with_http(token, Arc::new(ReqwestTransport { client }))
    }

    /// `BotApi(token, client=httpx.AsyncClient(transport=...))`: the requests go to `http`.
    pub fn with_http(token: &str, http: Http) -> Self {
        hide_token(token);
        Self { token: token.to_string(), http }
    }

    /// py: telegram.py:2261 `async def _call(self, method: str, data: dict[str, Any], files: Any = None) -> Any:`
    pub(crate) async fn call(&self, method: &str, data: &Obj, files: Option<&BotFiles>) -> Result<Value, BotApiError> {
        let url = format!("{API_ROOT}/bot{}/{method}", self.token);
        let (content_type, body) = match files {
            Some(files) => multipart(data, files),
            None => ("application/json".to_string(), serde_json::to_vec(&Value::Object(data.clone())).unwrap_or_default()),
        };
        let request = HttpRequest {
            method: "POST".into(),
            url,
            headers: vec![("Content-Type".into(), content_type)],
            body,
            timeout: None,
        };
        // str(e) can carry the request URL. Keep the kind of failure, drop the rest, cut the chain.
        let response = self.http.send(request).await.map_err(|e| BotApiError::new(0, &e.kind))?;
        let status = i64::from(response.status);
        let Ok(body) = response.json() else {
            return Err(BotApiError::new(status, "reply was not JSON"));
        };
        let ok = body.as_object().is_some_and(|b| wire_truthy(b.get("ok")));
        if !ok {
            let (description, code) = match body.as_object() {
                Some(b) => {
                    let description = match b.get("description") {
                        Some(d) if wire_truthy(Some(d)) => wire_str(d),
                        _ => "no description".to_string(),
                    };
                    (description, wire_int(b.get("error_code")))
                }
                None => ("malformed reply".to_string(), None),
            };
            let description = if self.token.is_empty() { description } else { description.replace(&self.token, "<token>") };
            return Err(BotApiError::new(code.unwrap_or(status), &description));
        }
        Ok(body.get("result").cloned().unwrap_or(Value::Null))
    }

    /// py: telegram.py:2331 `async def _send_pieces(self, chat_id: int, text: str, title: Optional[str], subtitle: Optional[str], markup: Optional[dict[str, Any]], *, reply_to: int = 0) -> int:`
    pub(crate) async fn send_pieces(&self, chat_id: i64, text: &str, title: Option<&str>, subtitle: Option<&str>, markup: Option<&Obj>, reply_to: i64) -> Result<i64, BotApiError> {
        wire::send_pieces(self, chat_id, text, title, subtitle, markup, reply_to).await
    }

    /// py: telegram.py:2444 `async def close(self) -> None:`
    ///
    /// Rust: reqwest's pool closes its connections when the last handle goes; nothing to wait for.
    pub async fn close(&self) {}
}

/// A multipart upload's files: (form field, file name, bytes, content type), as httpx's `files=`.
pub type BotFiles = Vec<(String, String, Vec<u8>, String)>;

/// `name="..."` as httpx writes a form-data parameter: quotes, CR and LF percent-escaped.
fn form_quote(value: &str) -> String {
    value.replace('"', "%22").replace('\r', "%0D").replace('\n', "%0A")
}

/// httpx's multipart body for `data=` (every field as text) and `files=`.
fn multipart(data: &Obj, files: &BotFiles) -> (String, Vec<u8>) {
    let boundary = format!("{:032x}", rand::random::<u128>());
    let mut body: Vec<u8> = Vec::new();
    for (name, value) in data {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        body.extend_from_slice(format!("Content-Disposition: form-data; name=\"{}\"\r\n\r\n", form_quote(name)).as_bytes());
        body.extend_from_slice(wire_str(value).as_bytes());
        body.extend_from_slice(b"\r\n");
    }
    for (field, filename, blob, content_type) in files {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        body.extend_from_slice(
            format!(
                "Content-Disposition: form-data; name=\"{}\"; filename=\"{}\"\r\nContent-Type: {content_type}\r\n\r\n",
                form_quote(field),
                form_quote(filename)
            )
            .as_bytes(),
        );
        body.extend_from_slice(blob);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), body)
}

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

/// The method bodies [`BotApi`] and [`MutedBotApi`] share (Python's MutedBotApi inherits them and overrides
/// `_call` alone): each builds its request and hands it to the client's `_call`.
pub(crate) mod wire {
    use super::*;

    /// `self._call` and `self._token`: what a Bot API method body needs of its client.
    #[async_trait::async_trait]
    pub(crate) trait BotCall: Send + Sync {
        async fn bot_call(&self, method: &str, data: &Obj, files: Option<&BotFiles>) -> Result<Value, BotApiError>;
        fn bot_token(&self) -> &str;
    }

    #[async_trait::async_trait]
    impl BotCall for BotApi {
        async fn bot_call(&self, method: &str, data: &Obj, files: Option<&BotFiles>) -> Result<Value, BotApiError> {
            self.call(method, data, files).await
        }
        fn bot_token(&self) -> &str {
            &self.token
        }
    }

    #[async_trait::async_trait]
    impl BotCall for MutedBotApi {
        async fn bot_call(&self, method: &str, data: &Obj, files: Option<&BotFiles>) -> Result<Value, BotApiError> {
            Ok(self.call(method, data, files).await)
        }
        fn bot_token(&self) -> &str {
            &self.token
        }
    }

    fn obj(v: Value) -> Obj {
        match v {
            Value::Object(map) => map,
            _ => Obj::new(),
        }
    }

    /// `label[:MAX_BUTTON_CHARS]`.
    fn label(text: &str) -> String {
        wire_cut(text, MAX_BUTTON_CHARS)
    }

    /// Rows of (label, callback_data, style) as InlineKeyboardButtons.
    fn inline_rows(keyboard: &[Vec<(String, String, String)>]) -> Value {
        Value::Array(
            keyboard
                .iter()
                .map(|row| {
                    Value::Array(
                        row.iter()
                            .map(|(text, data, style)| {
                                let mut button = obj(json!({"text": label(text), "callback_data": data}));
                                if !style.is_empty() {
                                    button.insert("style".into(), json!(style));
                                }
                                Value::Object(button)
                            })
                            .collect(),
                    )
                })
                .collect(),
        )
    }

    /// One button per row, each opening `url` the way `kind` says ("web_app": a Mini App; "url": the browser).
    fn link_rows(buttons: &[(String, String)], kind: &str) -> Value {
        Value::Array(
            buttons
                .iter()
                .map(|(text, url)| {
                    let target = if kind == "web_app" { json!({"url": url}) } else { json!(url) };
                    let mut button = obj(json!({"text": label(text)}));
                    button.insert(kind.into(), target);
                    json!([Value::Object(button)])
                })
                .collect(),
        )
    }

    /// Telegram refused the HTML: `e.code == 400 and "parse" in e.description.lower()`.
    fn parse_refusal(e: &BotApiError) -> bool {
        e.code == 400 && e.description.to_lowercase().contains("parse")
    }

    pub(crate) async fn get_me(c: &dyn BotCall) -> Result<Obj, BotApiError> {
        let result = c.bot_call("getMe", &Obj::new(), None).await?;
        Ok(obj(result))
    }

    pub(crate) async fn get_updates(c: &dyn BotCall, offset: Option<i64>, timeout: i64) -> Result<Vec<Obj>, BotApiError> {
        // "callback_query" is a tap on an inline button (TelegramInlet._on_tap); every other kind stays off.
        let mut data = obj(json!({"timeout": timeout, "allowed_updates": ALLOWED_UPDATES}));
        if let Some(offset) = offset {
            data.insert("offset".into(), json!(offset));
        }
        let result = c.bot_call("getUpdates", &data, None).await?;
        Ok(match result {
            Value::Array(updates) => updates.into_iter().filter_map(|u| u.as_object().cloned()).collect(),
            _ => Vec::new(),
        })
    }

    pub(crate) async fn receive_image(c: &dyn BotCall, item: &eve_telegram_images::Attachment) -> Result<eve_telegram_images::ReceivedImage, ApiImageError> {
        let token = c.bot_token().to_string();
        let get_file = |method: &'static str, data: Value| async move {
            c.bot_call(method, &obj(data), None).await.map_err(anyhow::Error::from)
        };
        eve_telegram_images::download(get_file, &token, item).await.map_err(|e| match e.downcast::<BotApiError>() {
            Ok(api) => ApiImageError::Api(api),
            Err(e) => match e.downcast::<eve_telegram_images::ImageError>() {
                Ok(image) => ApiImageError::Image(image),
                Err(other) => ApiImageError::Image(eve_telegram_images::ImageError::new(other.to_string())),
            },
        })
    }

    pub(crate) async fn send_pieces(c: &dyn BotCall, chat_id: i64, text: &str, title: Option<&str>, subtitle: Option<&str>, markup: Option<&Obj>, reply_to: i64) -> Result<i64, BotApiError> {
        let pieces = fmt::split(&fmt::compose(text, title, subtitle), fmt::MAX_MESSAGE_CHARS);
        let mut sent_id = 0;
        let last = pieces.len().saturating_sub(1);
        for (i, piece) in pieces.iter().enumerate() {
            let mut extra = Obj::new();
            if reply_to > 0 && i == 0 {
                extra.insert("reply_parameters".into(), json!({"message_id": reply_to, "allow_sending_without_reply": true}));
            }
            if let Some(markup) = markup {
                if i == last {
                    extra.insert("reply_markup".into(), Value::Object(markup.clone()));
                }
            }
            let mut data = obj(json!({"chat_id": chat_id, "text": piece, "parse_mode": fmt::PARSE_MODE}));
            data.extend(extra.clone());
            let result = match c.bot_call("sendMessage", &data, None).await {
                Ok(result) => result,
                Err(e) if parse_refusal(&e) => {
                    tracing::warn!(
                        "telegram: Telegram would not parse a message ({}); sent as plain text",
                        wire_cut(&e.description, 80)
                    );
                    let mut data = obj(json!({"chat_id": chat_id, "text": fmt::visible(piece)}));
                    data.extend(extra);
                    c.bot_call("sendMessage", &data, None).await?
                }
                Err(e) => return Err(e),
            };
            if let Some(id) = wire_int(result.get("message_id")) {
                sent_id = id;
            }
        }
        Ok(sent_id)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn send_message(c: &dyn BotCall, chat_id: i64, text: &str, title: Option<&str>, subtitle: Option<&str>, buttons: &[String], reply_to: i64, force_reply: Option<&str>) -> Result<(), BotApiError> {
        let mut markup: Option<Obj> = None;
        if !buttons.is_empty() {
            let rows: Vec<Value> = buttons.iter().map(|b| json!([{"text": label(b)}])).collect();
            markup = Some(obj(json!({"keyboard": rows, "one_time_keyboard": true, "resize_keyboard": true})));
        } else if let Some(placeholder) = force_reply {
            let mut m = obj(json!({"force_reply": true}));
            if !placeholder.is_empty() {
                m.insert("input_field_placeholder".into(), json!(wire_cut(placeholder, 64)));
            }
            markup = Some(m);
        }
        send_pieces(c, chat_id, text, title, subtitle, markup.as_ref(), reply_to).await?;
        Ok(())
    }

    pub(crate) async fn send_inline(c: &dyn BotCall, chat_id: i64, text: &str, keyboard: &[Vec<(String, String, String)>], title: Option<&str>, subtitle: Option<&str>) -> Result<i64, BotApiError> {
        let markup = obj(json!({"inline_keyboard": inline_rows(keyboard)}));
        send_pieces(c, chat_id, text, title, subtitle, Some(&markup), 0).await
    }

    pub(crate) async fn edit_message(c: &dyn BotCall, chat_id: i64, message_id: i64, text: &str, title: Option<&str>, subtitle: Option<&str>, keyboard: Option<&[Vec<(String, String, String)>]>) -> Result<(), BotApiError> {
        let Some(piece) = fmt::split(&fmt::compose(text, title, subtitle), fmt::MAX_MESSAGE_CHARS).into_iter().next() else {
            // Python's `[0]` of no pieces raises; Telegram's own answer to an empty edit is this.
            return Err(BotApiError::new(400, "Bad Request: message text is empty"));
        };
        let rows = inline_rows(keyboard.unwrap_or(&[]));
        let data = obj(json!({"chat_id": chat_id, "message_id": message_id, "reply_markup": {"inline_keyboard": rows}}));
        let mut html = data.clone();
        html.insert("text".into(), json!(piece));
        html.insert("parse_mode".into(), json!(fmt::PARSE_MODE));
        match c.bot_call("editMessageText", &html, None).await {
            Ok(_) => Ok(()),
            Err(e) if parse_refusal(&e) => {
                let mut plain = data;
                plain.insert("text".into(), json!(fmt::visible(&piece)));
                c.bot_call("editMessageText", &plain, None).await.map(|_| ())
            }
            Err(e) => Err(e),
        }
    }

    pub(crate) async fn drop_buttons(c: &dyn BotCall, chat_id: i64, message_id: i64) -> Result<(), BotApiError> {
        let data = obj(json!({"chat_id": chat_id, "message_id": message_id, "reply_markup": {"inline_keyboard": []}}));
        c.bot_call("editMessageReplyMarkup", &data, None).await.map(|_| ())
    }

    pub(crate) async fn answer_callback(c: &dyn BotCall, query_id: &str, text: &str) -> Result<(), BotApiError> {
        let mut data = obj(json!({"callback_query_id": query_id}));
        if !text.is_empty() {
            data.insert("text".into(), json!(wire_cut(text, MAX_TOAST_CHARS)));
        }
        c.bot_call("answerCallbackQuery", &data, None).await.map(|_| ())
    }

    pub(crate) async fn react(c: &dyn BotCall, chat_id: i64, message_id: i64, emoji: &str) -> Result<(), BotApiError> {
        let reaction = if emoji.is_empty() { json!([]) } else { json!([{"type": "emoji", "emoji": emoji}]) };
        let data = obj(json!({"chat_id": chat_id, "message_id": message_id, "reaction": reaction}));
        c.bot_call("setMessageReaction", &data, None).await.map(|_| ())
    }

    /// `Path(path).read_bytes()` off the runtime's threads; an OSError is the Bot API's code 0 with its kind.
    async fn read_blob(path: &Path) -> Result<Vec<u8>, BotApiError> {
        let owned = path.to_path_buf();
        match tokio::task::spawn_blocking(move || std::fs::read(owned)).await {
            Ok(Ok(blob)) => Ok(blob),
            Ok(Err(e)) => Err(BotApiError::new(0, eve_transcripts::py::os_error_name(&e))),
            Err(_) => Err(BotApiError::new(0, "OSError")),
        }
    }

    fn file_name(path: &Path) -> String {
        path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }

    pub(crate) async fn send_photo(c: &dyn BotCall, chat_id: i64, path: &Path, caption: &str) -> Result<(), BotApiError> {
        let blob = read_blob(path).await?;
        let kind = mime_guess::from_path(path).first().map(|m| m.essence_str().to_string()).unwrap_or_else(|| "image/jpeg".into());
        let data = obj(json!({"chat_id": chat_id.to_string(), "caption": wire_cut(&fmt::plain(caption), MAX_CAPTION_CHARS)}));
        let files: BotFiles = vec![("photo".into(), file_name(path), blob, kind)];
        c.bot_call("sendPhoto", &data, Some(&files)).await.map(|_| ())
    }

    pub(crate) async fn send_document(c: &dyn BotCall, chat_id: i64, path: &Path, caption: &str) -> Result<(), BotApiError> {
        let blob = read_blob(path).await?;
        let data = obj(json!({"chat_id": chat_id.to_string(), "caption": wire_cut(&fmt::plain(caption), MAX_CAPTION_CHARS)}));
        let files: BotFiles = vec![("document".into(), file_name(path), blob, "application/octet-stream".into())];
        c.bot_call("sendDocument", &data, Some(&files)).await.map(|_| ())
    }

    pub(crate) async fn typing(c: &dyn BotCall, chat_id: i64) -> Result<(), BotApiError> {
        c.bot_call("sendChatAction", &obj(json!({"chat_id": chat_id, "action": "typing"})), None).await.map(|_| ())
    }

    pub(crate) async fn send_draft(c: &dyn BotCall, chat_id: i64, draft_id: i64, text: &str) -> Result<(), BotApiError> {
        let mut data = obj(json!({"chat_id": chat_id, "draft_id": draft_id}));
        if !text.is_empty() {
            data.insert("text".into(), json!(text));
        }
        c.bot_call("sendMessageDraft", &data, None).await.map(|_| ())
    }

    pub(crate) async fn send_web_apps(c: &dyn BotCall, chat_id: i64, text: &str, buttons: &[(String, String)]) -> Result<(), BotApiError> {
        let data = obj(json!({"chat_id": chat_id, "text": text, "reply_markup": {"inline_keyboard": link_rows(buttons, "web_app")}}));
        c.bot_call("sendMessage", &data, None).await.map(|_| ())
    }

    pub(crate) async fn send_url_buttons(c: &dyn BotCall, chat_id: i64, text: &str, buttons: &[(String, String)]) -> Result<(), BotApiError> {
        let data = obj(json!({"chat_id": chat_id, "text": text, "reply_markup": {"inline_keyboard": link_rows(buttons, "url")}}));
        c.bot_call("sendMessage", &data, None).await.map(|_| ())
    }

    pub(crate) async fn send_photo_web_apps(c: &dyn BotCall, chat_id: i64, jpeg: &[u8], caption: &str, buttons: &[(String, String)]) -> Result<(), BotApiError> {
        let markup = json!({"inline_keyboard": link_rows(buttons, "web_app")});
        let data = obj(json!({"chat_id": chat_id.to_string(), "caption": wire_cut(caption, MAX_CAPTION_CHARS),
                              "reply_markup": eve_computer_agent::py::dumps(&markup)}));
        let files: BotFiles = vec![("photo".into(), "app.jpg".into(), jpeg.to_vec(), "image/jpeg".into())];
        c.bot_call("sendPhoto", &data, Some(&files)).await.map(|_| ())
    }

    pub(crate) async fn set_commands(c: &dyn BotCall, commands: &[(String, String)], chat_id: i64) -> Result<(), BotApiError> {
        let commands: Vec<Value> = commands.iter().map(|(c, d)| json!({"command": c, "description": d})).collect();
        let data = obj(json!({"commands": commands, "scope": {"type": "chat", "chat_id": chat_id}}));
        c.bot_call("setMyCommands", &data, None).await.map(|_| ())
    }
}

#[async_trait::async_trait]
impl TelegramApi for BotApi {
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
        BotApi::close(self).await
    }
}
