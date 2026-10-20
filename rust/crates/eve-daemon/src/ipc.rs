//! The IPC handler (`_handle_ipc`): one method per event, by the request's "evt" (`_ipc_<evt>`); unknown events
//! answer {"ok": false, "error": "unknown evt"} as Python's table did. Also `_mcp_late_done` and `_mcp_refusal`.

use std::sync::Arc;

use crate::daemon::Daemon;
use crate::seams::{Job, Obj};

/// The events [`Daemon::handle_ipc`] answers, in Python's table order.
pub const IPC_EVENTS: &[&str] = &[
    "expressions",
    "explore",
    "lesson",
    "notes",
    "trace",
    "pose",
    "follow",
    "axis",
    "move",
    "celebrate",
    "species",
    "diag",
    "identity",
    "session_start",
    "session_end",
    "turn_begin",
    "turn_end",
    "pretooluse",
    "permissionrequest",
    "push_character",
    "unpair",
    "sound",
    "chat",
    "mcp_tools",
    "mcp_call",
    "mic",
    "get_state",
    "posttooluse",
    "notification",
];

/// `_mcp_late_done(task)`: a Work session's MCP call that outlived its bound finished; log how it ended.
pub fn mcp_late_done(job: &Job) {
    todo!()
}

impl Daemon {
    /// `_handle_ipc(req)`: dispatch by "evt"; the Work registry is noted first.
    pub async fn handle_ipc(self: &Arc<Self>, req: Obj) -> anyhow::Result<Obj> {
        todo!()
    }

    /// The daemon's IPC handler as the server takes it.
    pub fn ipc_handler(self: &Arc<Self>) -> eve_ipc::Handler {
        todo!()
    }

    /// `_mcp_refusal(daemon, req)`: why a Work session's MCP request is refused (no door, a bad token, a session
    /// that is not eve's), None when it may go on.
    pub fn mcp_refusal(&self, req: &Obj) -> Option<Obj> {
        todo!()
    }

    /// `_ipc_expressions(req)`.
    pub async fn ipc_expressions(self: &Arc<Self>, req: Obj) -> anyhow::Result<Obj> {
        todo!()
    }

    /// `_ipc_explore(req)`.
    pub async fn ipc_explore(self: &Arc<Self>, req: Obj) -> anyhow::Result<Obj> {
        todo!()
    }

    /// `_ipc_lesson(req)`.
    pub async fn ipc_lesson(self: &Arc<Self>, req: Obj) -> anyhow::Result<Obj> {
        todo!()
    }

    /// `_ipc_notes(req)`.
    pub async fn ipc_notes(self: &Arc<Self>, req: Obj) -> anyhow::Result<Obj> {
        todo!()
    }

    /// `_ipc_trace(req)`.
    pub async fn ipc_trace(self: &Arc<Self>, req: Obj) -> anyhow::Result<Obj> {
        todo!()
    }

    /// `_ipc_pose(req)`.
    pub async fn ipc_pose(self: &Arc<Self>, req: Obj) -> anyhow::Result<Obj> {
        todo!()
    }

    /// `_ipc_follow(req)`.
    pub async fn ipc_follow(self: &Arc<Self>, req: Obj) -> anyhow::Result<Obj> {
        todo!()
    }

    /// `_ipc_axis(req)`.
    pub async fn ipc_axis(self: &Arc<Self>, req: Obj) -> anyhow::Result<Obj> {
        todo!()
    }

    /// `_ipc_move(req)`.
    pub async fn ipc_move(self: &Arc<Self>, req: Obj) -> anyhow::Result<Obj> {
        todo!()
    }

    /// `_ipc_celebrate(req)`.
    pub async fn ipc_celebrate(self: &Arc<Self>, req: Obj) -> anyhow::Result<Obj> {
        todo!()
    }

    /// `_ipc_species(req)`.
    pub async fn ipc_species(self: &Arc<Self>, req: Obj) -> anyhow::Result<Obj> {
        todo!()
    }

    /// `_ipc_diag(req)`.
    pub async fn ipc_diag(self: &Arc<Self>, req: Obj) -> anyhow::Result<Obj> {
        todo!()
    }

    /// `_ipc_identity(req)`.
    pub async fn ipc_identity(self: &Arc<Self>, req: Obj) -> anyhow::Result<Obj> {
        todo!()
    }

    /// `_ipc_session_start(req)`.
    pub async fn ipc_session_start(self: &Arc<Self>, req: Obj) -> anyhow::Result<Obj> {
        todo!()
    }

    /// `_ipc_session_end(req)`.
    pub async fn ipc_session_end(self: &Arc<Self>, req: Obj) -> anyhow::Result<Obj> {
        todo!()
    }

    /// `_ipc_turn_begin(req)`.
    pub async fn ipc_turn_begin(self: &Arc<Self>, req: Obj) -> anyhow::Result<Obj> {
        todo!()
    }

    /// `_ipc_turn_end(req)`.
    pub async fn ipc_turn_end(self: &Arc<Self>, req: Obj) -> anyhow::Result<Obj> {
        todo!()
    }

    /// `_ipc_pretooluse(req)`.
    pub async fn ipc_pretooluse(self: &Arc<Self>, req: Obj) -> anyhow::Result<Obj> {
        todo!()
    }

    /// `_ipc_permissionrequest(req)`.
    pub async fn ipc_permissionrequest(self: &Arc<Self>, req: Obj) -> anyhow::Result<Obj> {
        todo!()
    }

    /// `_ipc_push_character(req)`.
    pub async fn ipc_push_character(self: &Arc<Self>, req: Obj) -> anyhow::Result<Obj> {
        todo!()
    }

    /// `_ipc_unpair(req)`.
    pub async fn ipc_unpair(self: &Arc<Self>, req: Obj) -> anyhow::Result<Obj> {
        todo!()
    }

    /// `_ipc_sound(req)`.
    pub async fn ipc_sound(self: &Arc<Self>, req: Obj) -> anyhow::Result<Obj> {
        todo!()
    }

    /// `_ipc_chat(req)`.
    pub async fn ipc_chat(self: &Arc<Self>, req: Obj) -> anyhow::Result<Obj> {
        todo!()
    }

    /// `_ipc_mcp_tools(req)`.
    pub async fn ipc_mcp_tools(self: &Arc<Self>, req: Obj) -> anyhow::Result<Obj> {
        todo!()
    }

    /// `_ipc_mcp_call(req)`.
    pub async fn ipc_mcp_call(self: &Arc<Self>, req: Obj) -> anyhow::Result<Obj> {
        todo!()
    }

    /// `_ipc_mic(req)`.
    pub async fn ipc_mic(self: &Arc<Self>, req: Obj) -> anyhow::Result<Obj> {
        todo!()
    }

    /// `_ipc_get_state(req)`.
    pub async fn ipc_get_state(self: &Arc<Self>, req: Obj) -> anyhow::Result<Obj> {
        todo!()
    }

    /// `_ipc_posttooluse(req)`.
    pub async fn ipc_posttooluse(self: &Arc<Self>, req: Obj) -> anyhow::Result<Obj> {
        todo!()
    }

    /// `_ipc_notification(req)`.
    pub async fn ipc_notification(self: &Arc<Self>, req: Obj) -> anyhow::Result<Obj> {
        todo!()
    }
}
