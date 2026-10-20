//! [`Inlet`] for the real text door: what the daemon reads and calls on `telegram.TelegramInlet`, mapped onto the
//! port's public API. A Python private the port does not expose (`_chat_id`, `_awaiting_answer`, `_conv`, `_say`,
//! `_ask_user`, `_purchases`) is reached through the `door_*` accessors in eve-brain's `telegram/inlet/door.rs`. The
//! inlet's methods that spawn take `self: &Arc<Self>`: they are reached through the inlet's own `Arc`
//! ([`TelegramInlet::door_arc`]), which exists for as long as anything holds the inlet.

use std::sync::Arc;

use serde_json::Value;

use eve_brain::telegram::TelegramInlet;

use crate::seams::{Inlet, Obj};

#[async_trait::async_trait]
impl Inlet for TelegramInlet {
    fn claude(&self) -> bool {
        TelegramInlet::claude(self)
    }
    fn task_running(&self) -> bool {
        self.door_arc().is_some_and(|me| TelegramInlet::task_running(&me))
    }
    fn chat_id(&self) -> Option<i64> {
        self.door_chat_id()
    }
    fn awaiting_answer(&self) -> bool {
        self.door_awaiting_answer()
    }
    fn ask_timeout_secs(&self) -> f64 {
        self.config.ask_timeout_secs
    }
    fn ask_permissions(&self) -> bool {
        self.config.ask_permissions
    }
    fn conversation(&self) -> Option<String> {
        TelegramInlet::conversation(self)
    }
    fn forget_conversation(&self) {
        self.door_forget_conversation();
    }
    fn chief(&self) -> Option<Arc<eve_chief::Chief>> {
        self.chief.lock().clone()
    }
    fn shutdown_purchases(&self) {
        if let Some(purchases) = self.door_purchases() {
            purchases.shutdown();
        }
    }
    async fn ask_user(&self, question: &str, chat_id: i64, title: &str) -> String {
        match self.door_arc() {
            Some(me) => me.door_ask_user(question, chat_id, title).await,
            None => String::new(),
        }
    }
    async fn say(&self, chat_id: i64, text: &str) {
        if let Some(me) = self.door_arc() {
            me.door_say(chat_id, text).await;
        }
    }
    async fn tell_owner(&self, text: &str, about: &str, title: &str) -> bool {
        match self.door_arc() {
            Some(me) => TelegramInlet::tell_owner(&me, text, about, title, &[]).await,
            None => false,
        }
    }
    fn relay_session_started(&self, cwd: &str) {
        if let Some(me) = self.door_arc() {
            TelegramInlet::relay_session_started(&me, cwd);
        }
    }
    fn relay_turn_began(&self, prompt: &str, cwd: &str) {
        if let Some(me) = self.door_arc() {
            TelegramInlet::relay_turn_began(&me, prompt, cwd);
        }
    }
    fn relay_turn_ended(&self, cwd: &str) {
        if let Some(me) = self.door_arc() {
            TelegramInlet::relay_turn_ended(&me, cwd);
        }
    }
    async fn relay_notification(&self, kind: &str, message: &str, waits: bool) {
        if let Some(me) = self.door_arc() {
            TelegramInlet::relay_notification(&me, kind, message, waits).await;
        }
    }
    async fn relay_text(&self, text: &str, cwd: &str) {
        if let Some(me) = self.door_arc() {
            TelegramInlet::relay_text(&me, text, cwd).await;
        }
    }
    fn relay_tool_call(&self, tool: &str, hint: &str) {
        if let Some(me) = self.door_arc() {
            TelegramInlet::relay_tool_call(&me, tool, hint);
        }
    }
    async fn decide_permission(&self, tool: &str, hint: &str, cwd: &str, always: bool) -> Option<String> {
        let me = self.door_arc()?;
        TelegramInlet::decide_permission(&me, tool, hint, cwd, always).await
    }
    fn hear(&self, text: &str, on_call: bool) {
        if let Some(me) = self.door_arc() {
            // hear(text, on_call=on_call): no captured question, no call reader, no app turn, grade "auto"
            TelegramInlet::hear(&me, text, eve_phone_call::Captured::default(), on_call, None, None, "auto", &[]);
        }
    }
    fn session_tools(&self) -> anyhow::Result<(Vec<Obj>, Vec<String>)> {
        let me = self.door_arc().ok_or_else(|| anyhow::anyhow!("the chat has shut down"))?;
        Ok(TelegramInlet::session_tools(&me))
    }
    async fn session_call(&self, name: &str, args: &Value, sid: &str) -> anyhow::Result<(Obj, String)> {
        let me = self.door_arc().ok_or_else(|| anyhow::anyhow!("the chat has shut down"))?;
        Ok(TelegramInlet::session_call(&me, name, args, sid).await)
    }
}
