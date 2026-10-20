//! A port of `voice_agent.py`, lines 2241-2365: the Live API connection (open_session) and helpers.
//!
//! The Python opens the Live session with the openai SDK (`AsyncOpenAI().live.connect()`). Rust has no SDK for it,
//! so [`LiveSocket`] speaks the same wire: a websocket to `<base url>/live/sessions` (wss, `OPENAI_BASE_URL` or
//! `https://api.openai.com/v1`), the key as a bearer token, `OpenAI-Organization` / `OpenAI-Project` from
//! `OPENAI_ORG_ID` / `OPENAI_PROJECT_ID`, and the SDK's client events as JSON text frames.

#[allow(unused_imports)]
use crate::prelude::*;
#[allow(unused_imports)]
use crate::voice_agent::*;

use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::{self, protocol::frame::coding::CloseCode, Message};

/// py: voice_agent.py:2241 `def _clip_output(result: dict[str, Any], cap: int) -> str:`
///
/// A tool result as JSON of at most `cap` chars, still valid JSON when clipped.
pub(crate) fn clip_output(result: &Obj, cap: i64) -> String {
    let dumps = eve_learning::py::dumps;
    let chars = |s: &str| s.chars().count() as i64;
    let text = dumps(&Value::Object(result.clone()), false);
    if chars(&text) <= cap {
        return text;
    }
    let ok = result.get("ok").cloned().unwrap_or(Value::Null);
    let mut head = Obj::new();
    head.insert("ok".into(), ok.clone());
    head.insert("clipped".into(), Value::Bool(true));
    head.insert("text".into(), Value::String(String::new()));
    let mut room = cap - chars(&dumps(&Value::Object(head.clone()), true));
    let all: Vec<char> = text.chars().collect();
    while room > 0 {
        let take = usize::try_from(room).unwrap_or(usize::MAX).min(all.len());
        head.insert("text".into(), Value::String(all[..take].iter().collect()));
        let out = dumps(&Value::Object(head.clone()), false);
        let n = chars(&out);
        if n <= cap {
            return out;
        }
        room -= n - cap;
    }
    let mut bare = Obj::new();
    bare.insert("ok".into(), ok);
    bare.insert("clipped".into(), Value::Bool(true));
    dumps(&Value::Object(bare), true)
}

/// py: voice_agent.py:2257 `def _attr(event: Any, name: str) -> Any:`
///
/// A field of a Live event (the server's JSON), `null` when it has none.
pub(crate) fn attr(event: &Value, name: &str) -> Value {
    event.get(name).cloned().unwrap_or(Value::Null)
}

// ---- the real wiring --------------------------------------------------------------------------

// The desk voice is OpenAI's realtime Live API, which OpenRouter does not carry. Until its rebuild on OpenRouter (wave
// 2 of the one-key change, docs/openrouter.md) it is the one eve feature that still needs OPENAI_API_KEY: without
// it the desk voice is off, with one log line per wake, and nothing else is.

/// py: voice_agent.py:2268 `LIVE_KEY_ENV`
pub const LIVE_KEY_ENV: &str = "OPENAI_API_KEY";

/// The SDK's default API address (`OPENAI_BASE_URL` overrides it).
pub const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

/// py: voice_agent.py:2271 `def live_available(env: Any = None) -> bool:`
///
/// Whether the desk voice can open a Live session: OPENAI_API_KEY is set (wave 1 of docs/openrouter.md).
pub fn live_available(env: Option<&Environ>) -> bool {
    !eve_computer_agent::py::py_strip(&env_get(env, LIVE_KEY_ENV).unwrap_or_default()).is_empty()
}

/// The Live session's websocket address, as the SDK's `_prepare_url` builds it: the API base (`OPENAI_BASE_URL`,
/// else [`DEFAULT_BASE_URL`]) with `ws` for `http` and `wss` otherwise, and `/live/sessions` after its path.
pub fn live_url(environ: Option<&Environ>) -> anyhow::Result<String> {
    let base = env_get(environ, "OPENAI_BASE_URL").filter(|b| !b.is_empty()).unwrap_or_else(|| DEFAULT_BASE_URL.into());
    let mut url = url::Url::parse(&base)?;
    let scheme = if url.scheme() == "http" { "ws" } else { "wss" };
    url.set_scheme(scheme).map_err(|()| anyhow::anyhow!("live: cannot use {base} for a websocket"))?;
    let path = format!("{}/live/sessions", url.path().trim_end_matches('/'));
    url.set_path(&path);
    Ok(url.to_string())
}

/// py: voice_agent.py:2277 `def warm_live_import() -> None:`
///
/// Import what `AsyncOpenAI().live` imports lazily: `openai.resources`, some
/// hundreds of modules. Idempotent and cheap once cached; call it off the loop.
///
/// Rust: there is no import to pay for. What the first connect pays for instead is looking up the Live API's
/// host, so this resolves it once (the system resolver then has it cached).
pub fn warm_live_import() {
    static WARMED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    WARMED.get_or_init(|| {
        use std::net::ToSocketAddrs;
        let Ok(raw) = live_url(None) else {
            return;
        };
        let Ok(url) = url::Url::parse(&raw) else {
            return;
        };
        if let (Some(host), Some(port)) = (url.host_str(), url.port_or_known_default()) {
            let _ = (host, port).to_socket_addrs();
        }
    });
}

type WsStream = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// The real Live connection (`client.live.connect()`): one websocket, written by the session's tasks, read by one.
pub struct LiveSocket {
    sink: tokio::sync::Mutex<futures::stream::SplitSink<WsStream, Message>>,
    stream: tokio::sync::Mutex<futures::stream::SplitStream<WsStream>>,
}

impl LiveSocket {
    /// Open the session's websocket with `api_key` (None: `OPENAI_API_KEY`).
    pub async fn connect(api_key: Option<&str>, environ: Option<&Environ>) -> anyhow::Result<Self> {
        use tungstenite::client::IntoClientRequest;
        use tungstenite::http::HeaderValue;
        let key = match api_key.filter(|k| !k.is_empty()) {
            Some(k) => k.to_string(),
            None => env_get(environ, LIVE_KEY_ENV).unwrap_or_default(),
        };
        if key.is_empty() {
            anyhow::bail!("live: no API key (set {LIVE_KEY_ENV})");
        }
        let mut request = live_url(environ)?.into_client_request()?;
        let headers = request.headers_mut();
        headers.insert("Authorization", HeaderValue::from_str(&format!("Bearer {key}"))?);
        for (env, header) in [("OPENAI_ORG_ID", "OpenAI-Organization"), ("OPENAI_PROJECT_ID", "OpenAI-Project")] {
            if let Some(value) = env_get(environ, env) {
                headers.insert(header, HeaderValue::from_str(&value)?);
            }
        }
        let (ws, _response) = tokio_tungstenite::connect_async(request).await?;
        let (sink, stream) = ws.split();
        Ok(Self { sink: tokio::sync::Mutex::new(sink), stream: tokio::sync::Mutex::new(stream) })
    }

    /// One client event, as the SDK sends it (`json.dumps(event)` in a text frame).
    async fn send(&self, event: Value) -> anyhow::Result<()> {
        let text = serde_json::to_string(&event)?;
        self.sink.lock().await.send(Message::text(text)).await?;
        Ok(())
    }

    /// `__aexit__`: close the websocket (code 1000).
    pub async fn close(&self) {
        let _ = self.sink.lock().await.close().await;
    }
}

#[async_trait]
impl LiveConnection for LiveSocket {
    async fn session_start(&self, session: Value) -> anyhow::Result<()> {
        self.send(json!({"type": "session.start", "session": session})).await
    }

    async fn session_update(&self, session: Value) -> anyhow::Result<()> {
        self.send(json!({"type": "session.update", "session": session})).await
    }

    async fn commentary_append(&self, content: &str, delegation_id: Option<&str>) -> anyhow::Result<()> {
        self.send(json!({"type": "session.commentary.append", "content": content, "delegation_id": delegation_id}))
            .await
    }

    async fn thinking_append(&self, content: &str, delegation_id: Option<&str>) -> anyhow::Result<()> {
        self.send(json!({"type": "session.thinking.append", "content": content, "delegation_id": delegation_id}))
            .await
    }

    async fn input_audio_append(&self, audio_b64: &str) -> anyhow::Result<()> {
        self.send(json!({"type": "session.input_audio.append", "audio": audio_b64})).await
    }

    async fn response_create(&self) -> anyhow::Result<()> {
        self.send(json!({"type": "response.create"})).await
    }

    async fn response_item_create(&self, item: Value) -> anyhow::Result<()> {
        self.send(json!({"type": "response.item.create", "item": item})).await
    }

    /// `async for event in conn`: ends on a normal close (the SDK's `ConnectionClosedOK`), fails on any other.
    async fn next_event(&self) -> Option<anyhow::Result<Value>> {
        let mut stream = self.stream.lock().await;
        loop {
            let message = match stream.next().await? {
                Ok(m) => m,
                Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => return None,
                Err(e) => return Some(Err(e.into())),
            };
            match message {
                Message::Text(text) => return Some(serde_json::from_str(text.as_str()).map_err(Into::into)),
                Message::Binary(data) => return Some(serde_json::from_slice(&data).map_err(Into::into)),
                Message::Close(frame) => {
                    return match frame.map(|f| f.code) {
                        None | Some(CloseCode::Normal) | Some(CloseCode::Away) => None,
                        Some(code) => Some(Err(anyhow::anyhow!("live: the connection closed ({code})"))),
                    };
                }
                Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => continue,
            }
        }
    }
}

/// Hands the turns on when the conversation is over, on every exit path including a hush (the future dropped).
struct ClosedHandoff {
    session: Arc<VoiceSession>,
    on_closed: Option<OnClosed>,
}

impl Drop for ClosedHandoff {
    fn drop(&mut self) {
        if let Some(on_closed) = self.on_closed.take() {
            let turns = self.session.turns();
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| on_closed(turns))).is_err() {
                tracing::error!("voice: on_closed failed");
            }
        }
    }
}

/// `finally: speaker.close()`.
struct SpeakerCloser(Arc<dyn VoiceSpeaker>);

impl Drop for SpeakerCloser {
    fn drop(&mut self) {
        self.0.close();
    }
}

/// py: voice_agent.py:2283 `async def open_session( mic: "asyncio.Queue[bytes]", on_state: Callable[[str], None], agent_factory: Callable[..., ComputerAgent], config: Optional[VoiceConfig] = None, agent_enabled: bool = True, api_key: Optional[str] = None, on_caption: Optional[Callable[[dict], None]] = None, on_explore: Optional[Callable[[], None]] = None, scene: Any = None, head: Any = None, intent: Optional[Callable[[str], Awaitable[Optional[str]]]] = None, on_sound: Optional[Callable[[bool], None]] = None, muted: Callable[[], bool] = lambda: False, thinker: Optional[Callable[..., Awaitable[dict[str, Any]]]] = None, on_photo: Optional[Callable[[str], Awaitable[dict[str, Any]]]] = None, brief: str = "", memory: Optional["Memory"] = None, conv: str = "", today: str = "", backend_today: str = "", on_closed: Optional[Callable[[list[tuple[str, str]]], None]] = None, learning: Optional[Callable[..., dict[str, Any]]] = None, think_aloud: Optional[dict[str, Any]] = None, on_spoken_idea: Optional[Callable[[str, str], Any]] = None, on_think_aloud: Optional[Callable[[bool, Optional[str]], None]] = None, on_open: Optional[Callable[[VoiceSession], None]] = None, lesson_wake: bool = False, head_pose: Optional[Callable[[str], Awaitable[str]]] = None, gate: Any = None, on_expression: Optional[Callable[[str, str], None]] = None, mac_busy: Callable[[], bool] = lambda: False, meeter: Any = None, **app_hubs: Any, ) -> None:`
///
/// Run one full conversation on the real Live API — captions to the robot,
/// or the real speaker in audio mode.
///
/// `think_aloud` opens it for think out loud on that lesson. `on_open` hands the
/// session to the daemon, so a toggle can switch or stop it while it runs.
/// `memory` (with `conv`, `today`, `backend_today` from the daemon) is lent when
/// memory is on; the session writes its own transcript lines and close marker.
/// Rust: the session's own keyword arguments (config ... meeter, `**app_hubs`) are `options`. What Python raises to
/// the daemon (a connection that cannot be opened, a session that cannot start: [`VoiceSession::try_run`]) is
/// returned as `Err`, after the turns are handed on and the speaker closed; no key is `Ok`, with its warning. A hush
/// is dropping this future: the session's own drop guard runs its cleanup, then the turns are handed on and the
/// speaker closed.
pub async fn open_session(mic: eve_ears::PcmQueue, on_state: OnState, agent_factory: AgentFactory, api_key: Option<&str>, on_closed: Option<OnClosed>, on_open: Option<OnOpenSession>, options: VoiceSessionOptions) -> anyhow::Result<()> {
    let mut options = options;
    let api_key = api_key.filter(|k| !k.is_empty());
    if api_key.is_none() && !live_available(None) {
        tracing::warn!(
            "voice: the desk voice is off: it still runs on OpenAI's Live API and OPENAI_API_KEY is not set (everything else runs on OPENROUTER_API_KEY; docs/openrouter.md)"
        );
        return Ok(());
    }
    let cfg = options.config.take().unwrap_or_else(|| configured(None));
    let speaker: Arc<dyn VoiceSpeaker> =
        if cfg.output == "captions" { Arc::new(NullSpeaker::default()) } else { Arc::new(Speaker::new()) };
    speaker.start();
    let _closer = SpeakerCloser(speaker.clone());
    // `client.live` imports the SDK's whole resources package the first time it is
    // touched. Done on the loop thread inside the daemon that took 12–20 s (the loop
    // watchdog named it, 2026-09-21): every wake of a fresh daemon sat silent that long.
    // A thread pays it instead, and the daemon warms it at start (warm_live_import).
    let _ = tokio::task::spawn_blocking(warm_live_import).await;
    let socket = Arc::new(LiveSocket::connect(api_key, None).await?);
    options.config = Some(cfg);
    let session = VoiceSession::new(socket.clone(), mic, speaker.clone(), agent_factory, on_state, options);
    if let Some(on_open) = &on_open {
        on_open(&session);
    }
    // Hand the turns on, on every exit path including a hush and an
    // exception. The transcript lines and the close marker are already written.
    let handoff = ClosedHandoff { session: session.clone(), on_closed };
    let result = session.try_run().await;
    drop(handoff);
    socket.close().await; // `async with client.live.connect()`'s exit
    result
}

/// `on_closed(turns)`: the conversation's (speaker, text) turns once it is over.
pub type OnClosed = Arc<dyn Fn(Vec<(String, String)>) + Send + Sync>;
/// `on_open(session)`: the session handed to the daemon, so a toggle can switch or stop it while it runs.
pub type OnOpenSession = Arc<dyn Fn(&Arc<VoiceSession>) + Send + Sync>;

#[cfg(test)]
mod tests {
    use super::*;

    fn obj(v: Value) -> Obj {
        v.as_object().cloned().unwrap_or_default()
    }

    #[test]
    fn a_small_result_goes_through_untouched() {
        assert_eq!(clip_output(&obj(json!({"ok": true, "hits": []})), 1500), r#"{"ok": true, "hits": []}"#);
        assert_eq!(clip_output(&obj(json!({"ok": true, "text": "café"})), 1500), r#"{"ok": true, "text": "café"}"#);
    }

    #[test]
    fn a_large_result_is_clipped_to_valid_json_of_at_most_cap_chars() {
        let hits: Vec<Value> = (0..50).map(|_| json!({"text": "word ".repeat(40), "day": "2026-09-20"})).collect();
        let out = clip_output(&obj(json!({"ok": true, "hits": hits})), 1500);
        assert!(out.chars().count() <= 1500);
        let parsed: Value = serde_json::from_str(&out).unwrap();
        assert!(parsed["ok"] == true && parsed["clipped"] == true);
        assert!(parsed["text"].as_str().unwrap().starts_with(r#"{"ok": true, "hits": [{"text": "word word"#));
        // characters, not bytes, and escapes count once they are written: the clip still fits
        let wide = clip_output(&obj(json!({"ok": false, "text": "é\"\n".repeat(900)})), 300);
        assert!(wide.chars().count() <= 300, "{}", wide.chars().count());
        let parsed: Value = serde_json::from_str(&wide).unwrap();
        assert!(parsed["ok"] == false && parsed["clipped"] == true);
        // no ok in the result: None
        let none: Value = serde_json::from_str(&clip_output(&obj(json!({"text": "x".repeat(100)})), 60)).unwrap();
        assert_eq!(none["ok"], Value::Null);
    }

    #[test]
    fn no_room_for_any_text_is_the_bare_marker() {
        assert_eq!(clip_output(&obj(json!({"ok": true, "text": "x".repeat(100)})), 10), r#"{"ok": true, "clipped": true}"#);
    }

    #[test]
    fn attr_reads_a_field_or_null() {
        let e = json!({"type": "session.started", "usage": {"seconds": 3}});
        assert_eq!(attr(&e, "type"), "session.started");
        assert_eq!(attr(&attr(&e, "usage"), "seconds"), 3);
        assert_eq!(attr(&e, "error"), Value::Null);
        assert_eq!(attr(&json!("not an object"), "type"), Value::Null);
    }

    /// Python's own `_clip_output` on the same inputs (run with CPython 3, 2026-10-20), byte for byte.
    #[test]
    fn clip_output_matches_python() {
        let hits: Vec<Value> = (0..50).map(|_| json!({"text": "word ".repeat(40), "day": "2026-09-20"})).collect();
        let big = clip_output(&obj(json!({"ok": true, "hits": hits})), 1500);
        assert_eq!(big.chars().count(), 1492);
        assert!(big.ends_with(" word word word word word word word word word word word wo\"}"));
        let cases: [(Value, i64, &str); 4] = [
            (json!({"ok": false, "text": "é\"\n".repeat(900)}), 300, r#""{\"ok\": false, \"clipped\": true, \"text\": \"{\\\"ok\\\": false, \\\"text\\\": \\\"é\\\\\\\"\\\\né\\\\\\\"\\\\né\\\\\\\"\\\\né\\\\\\\"\\\\né\\\\\\\"\\\\né\\\\\\\"\\\\né\\\\\\\"\\\\né\\\\\\\"\\\\né\\\\\\\"\\\\né\\\\\\\"\\\\né\\\\\\\"\\\\né\\\\\\\"\\\\né\\\\\\\"\\\\né\\\\\\\"\\\\né\\\\\\\"\\\\né\\\\\\\"\\\\né\\\\\\\"\\\\né\\\\\\\"\\\\\"}""#),
            (json!({"text": "x".repeat(100)}), 60, r#""{\"ok\": null, \"clipped\": true, \"text\": \"{\\\"text\\\": \\\"xxxxxx\"}""#),
            (json!({"ok": true, "text": "x".repeat(100)}), 10, r#""{\"ok\": true, \"clipped\": true}""#),
            (json!({"ok": true, "text": "日本語".repeat(200), "n": 1.5}), 120, r#""{\"ok\": true, \"clipped\": true, \"text\": \"{\\\"ok\\\": true, \\\"text\\\": \\\"日本語日本語日本語日本語日本語日本語日本語日本語日本語日本語日本語日本語日本語日本語日本語日本語日本語日\"}""#),
        ];
        for (result, cap, want) in cases {
            let want: String = serde_json::from_str(want).unwrap();
            assert_eq!(clip_output(&obj(result), cap), want);
        }
    }
}
