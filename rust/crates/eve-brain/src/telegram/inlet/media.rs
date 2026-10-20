//! A port of `telegram.py`, lines 7340-7593: media, the screen, the robot body, files, photos, think_hard.
//!
//! Skeleton: constants and prompts are verbatim (generated from the Python values); bodies are
//! `todo!()` until this file's unit fills them.

#[allow(unused_imports)]
use crate::prelude::*;
#[allow(unused_imports)]
use crate::telegram::*;

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
    pub(crate) async fn send_media(self: &Arc<Self>, chat_id: i64, path: Option<&Path>, caption: &str, document: bool, data: Option<&[u8]>, name: &str, buttons: &[(String, String)]) {
        todo!("telegram.py:7340 TelegramInlet._send_media")
    }

    /// py: telegram.py:7366 `async def _send_app_photo(self, chat_id: int, jpeg: bytes, caption: str, buttons: Sequence[tuple[str, str]]) -> None:`
    ///
    /// apps_maker.ChatMaker's ``send_photo``: a finished app's picture and its Open button (_send_media).
    pub(crate) async fn send_app_photo(self: &Arc<Self>, chat_id: i64, jpeg: &[u8], caption: &str, buttons: &[(String, String)]) {
        todo!("telegram.py:7366 TelegramInlet._send_app_photo")
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
        todo!("telegram.py:7371 TelegramInlet._keep_for_app")
    }

    /// py: telegram.py:7403 `async def _send_screen(self, chat_id: int, caption: str, *, agent: Any = None) -> dict[str, Any]:`
    pub(crate) async fn send_screen(self: &Arc<Self>, chat_id: i64, caption: &str, agent: Option<&Value>) -> Obj {
        todo!("telegram.py:7403 TelegramInlet._send_screen")
    }

    /// py: telegram.py:7446 `async def _robot_tool(self, name: str, args: dict[str, Any]) -> dict[str, Any]:`
    ///
    /// The robot's own tools, as the voice has them (voice_agent._slow_tool / _tool). In stealth the
    /// robot stays asleep: the camera may still look (it moves nothing), the head and the explorer may not.
    pub(crate) async fn robot_tool(self: &Arc<Self>, name: &str, args: &Obj) -> Obj {
        todo!("telegram.py:7446 TelegramInlet._robot_tool")
    }

    /// py: telegram.py:7517 `async def _send_file(self, chat_id: int, raw: str, caption: str) -> dict[str, Any]:`
    pub(crate) async fn send_file(self: &Arc<Self>, chat_id: i64, raw: &str, caption: &str) -> Obj {
        todo!("telegram.py:7517 TelegramInlet._send_file")
    }

    /// py: telegram.py:7544 `async def _take_photo(self, note: str, chat_id: int) -> dict[str, Any]:`
    pub(crate) async fn take_photo(self: &Arc<Self>, note: &str, chat_id: i64) -> Obj {
        todo!("telegram.py:7544 TelegramInlet._take_photo")
    }

    /// py: telegram.py:7554 `async def _think_hard(self, question: str, chat_id: Optional[int] = None) -> dict[str, Any]:`
    pub(crate) async fn think_hard(self: &Arc<Self>, question: &str, chat_id: Option<i64>) -> Obj {
        todo!("telegram.py:7554 TelegramInlet._think_hard")
    }
}

/// py: telegram.py:7574 `ROBOT_TOOLS`
pub const ROBOT_TOOLS: &[&str] = &["look", "look_around", "find", "move_head", "go_explore", "set_sound", "remember", "take_notes"];

// py: telegram.py:7575 `TOOL_HANDLERS` (dict): ported by this file's unit.
