//! A port of `telegram.py`, lines 7594-7699: diagnose, MutedBotApi, make_inlet.
//!
//! Skeleton: constants and prompts are verbatim (generated from the Python values); bodies are
//! `todo!()` until this file's unit fills them.

#[allow(unused_imports)]
use crate::prelude::*;
#[allow(unused_imports)]
use crate::telegram::*;

/// py: telegram.py:7594 `def diagnose(environ: Any = None, api: Any = None) -> int:`
///
/// `eve-bridge telegram-check`: is the token good, and what is my numeric id?
///
/// Reads without acknowledging (no offset), so the daemon still sees every message afterwards. Prints
/// ids and first names only — never what anyone wrote. Run it with the daemon's inlet off: Telegram
/// allows one poller per bot and answers the second with 409.
pub async fn diagnose(environ: Option<&Environ>, api: Option<Arc<dyn TelegramApi>>) -> i64 {
    todo!("telegram.py:7594 diagnose")
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
/// Rust: implements [`TelegramApi`] (this file's unit writes the impl), every send answered by [`MutedBotApi::call`].
pub struct MutedBotApi {
    pub(crate) token: String,
    /// Bot API calls not made, for the log and tests
    pub dropped: std::sync::atomic::AtomicI64,
    pub notifier: Option<Arc<crate::notify::Notifier>>,
}

impl MutedBotApi {
    /// py: telegram.py:7654 `def __init__(self, token: str = "", notifier: Any = None) -> None:   # no HTTP client: nothing to reach self._token = "" self.dropped = 0                   # Bot API calls not made, for the log and tests self.notifier = notifier hide_token(token)`
    pub fn new(token: &str, notifier: Option<Arc<crate::notify::Notifier>>) -> Self {
        todo!("telegram.py:7654 MutedBotApi.__init__")
    }

    /// py: telegram.py:7660 `async def _call(self, method: str, data: dict[str, Any], files: Any = None) -> Any:`
    pub(crate) async fn call(&self, method: &str, data: &Obj, files: Option<&BotFiles>) -> Value {
        todo!("telegram.py:7660 MutedBotApi._call")
    }

    /// py: telegram.py:7672 `async def close(self) -> None:`
    pub async fn close(&self) {
        todo!("telegram.py:7672 MutedBotApi.close")
    }
}

/// py: telegram.py:7677 `def make_inlet(config: TelegramConfig, **lent: Any) -> Optional[TelegramInlet]:`
///
/// The real inlet, or None (with one log line) when it cannot run.
/// `**lent` is `lent`; `create` and `stream_create` are made here (make_response_creator, make_stream_creator).
pub fn make_inlet(config: TelegramConfig, lent: InletOptions) -> Option<Arc<TelegramInlet>> {
    todo!("telegram.py:7677 make_inlet")
}
