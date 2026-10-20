//! Claude Code's PreToolUse and PermissionRequest hooks: the matchers, the audit log, the command-risk gate (Jev,
//! shadow or enforced), the owner's answer over the text door, and Read's in-cwd policy.

use std::sync::Arc;

use crate::daemon::Daemon;
use crate::seams::{CommandAsker, Obj};

impl Daemon {
    /// `_handle_permission_request(req)`: the PermissionRequest hook's decision.
    pub async fn handle_permission_request(self: &Arc<Self>, req: Obj) -> anyhow::Result<Obj> {
        todo!()
    }

    /// `_handle_pretooluse(req)`: the PreToolUse hook's decision: classify, audit, ask the owner when it asks.
    pub async fn handle_pretooluse(self: &Arc<Self>, req: Obj) -> anyhow::Result<Obj> {
        todo!()
    }

    /// `_command_risk()`: (mode, asker) for the command-risk gate, made once; None when off.
    pub fn command_risk(&self) -> Option<(String, CommandAsker)> {
        todo!()
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
        todo!()
    }

    /// `_ensure_session(req)`: a hook from a session the daemon has not seen starts it.
    pub fn ensure_session(&self, req: &Obj) {
        todo!()
    }

    /// `_handle_read_pretooluse(req, tool_use_id, session_id, hint)`: Read prompts: out-of-cwd reads are carded;
    /// approval grants the whole folder.
    pub async fn handle_read_pretooluse(
        self: &Arc<Self>,
        req: Obj,
        tool_use_id: &str,
        session_id: &str,
        hint: &str,
    ) -> anyhow::Result<Obj> {
        todo!()
    }
}
