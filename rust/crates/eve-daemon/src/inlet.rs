//! [`Inlet`] for the real text door: what the daemon reads and calls on `telegram.TelegramInlet`, mapped onto the
//! port's public API. A Python private the port does not expose (`_chat_id`, `_conv`, `_say`, `_purchases`) is
//! reached through the accessor the door unit adds to eve-brain, recorded in its PARITY.md.

use std::sync::Arc;

use serde_json::Value;

use crate::seams::{Inlet, Obj};

#[async_trait::async_trait]
impl Inlet for eve_brain::telegram::TelegramInlet {
    fn claude(&self) -> bool {
        todo!()
    }
    fn task_running(&self) -> bool {
        todo!()
    }
    fn chat_id(&self) -> Option<i64> {
        todo!()
    }
    fn awaiting_answer(&self) -> bool {
        todo!()
    }
    fn ask_timeout_secs(&self) -> f64 {
        todo!()
    }
    fn ask_permissions(&self) -> bool {
        todo!()
    }
    fn conversation(&self) -> Option<String> {
        todo!()
    }
    fn forget_conversation(&self) {
        todo!()
    }
    fn chief(&self) -> Option<Arc<eve_chief::Chief>> {
        todo!()
    }
    fn shutdown_purchases(&self) {
        todo!()
    }
    async fn ask_user(&self, question: &str, chat_id: i64, title: &str) -> String {
        todo!()
    }
    async fn say(&self, chat_id: i64, text: &str) {
        todo!()
    }
    async fn tell_owner(&self, text: &str, about: &str, title: &str) -> bool {
        todo!()
    }
    fn relay_session_started(&self, cwd: &str) {
        todo!()
    }
    fn relay_turn_began(&self, prompt: &str, cwd: &str) {
        todo!()
    }
    fn relay_turn_ended(&self, cwd: &str) {
        todo!()
    }
    async fn relay_notification(&self, kind: &str, message: &str, waits: bool) {
        todo!()
    }
    async fn relay_text(&self, text: &str, cwd: &str) {
        todo!()
    }
    fn relay_tool_call(&self, tool: &str, hint: &str) {
        todo!()
    }
    async fn decide_permission(&self, tool: &str, hint: &str, cwd: &str, always: bool) -> Option<String> {
        todo!()
    }
    fn hear(&self, text: &str, on_call: bool) {
        todo!()
    }
    fn session_tools(&self) -> anyhow::Result<(Vec<Obj>, Vec<String>)> {
        todo!()
    }
    async fn session_call(&self, name: &str, args: &Value, sid: &str) -> anyhow::Result<(Obj, String)> {
        todo!()
    }
}
