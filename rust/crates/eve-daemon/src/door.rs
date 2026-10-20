//! The text door and what it is lent: `_make_apps`, `_make_direct`, `_make_telegram`, `_make_chief`,
//! `_make_canvas`, `_make_share`, `_make_agentcard`, `_make_america`, `_make_meeter`, `_ask_owner_on_phone`,
//! `_tell_owner_on_phone`, `_start_miniapp`, `_start_phone_expressions`.

use std::collections::BTreeSet;
use std::sync::Arc;

use crate::daemon::Daemon;
use crate::seams::Job;

impl Daemon {
    /// `_make_apps(owner_ids)`: Composio's apps (Slack, Notion, Linear, work mail and calendar) for the chat
    /// brain, None when off or keyless.
    pub fn make_apps(&self, owner_ids: &BTreeSet<i64>) -> Option<Arc<eve_composio_tools::ComposioBridge>> {
        todo!()
    }

    /// `_make_direct()`: the direct Google/GitHub/Maps bridge, None when off.
    pub fn make_direct(&self) -> Option<Arc<eve_apps_mcp::AppsBridge>> {
        todo!()
    }

    /// `_make_telegram()`: the text door with everything it is lent (the chief, the watcher, the meeter, the apps,
    /// the vault, the chat log, Canvas, purchases, sharing, the scene, the head, the notes, the terminal, the
    /// Claude sessions, the app hubs); stored as `telegram` and `telegram_inlet`. None unless EVE_TELEGRAM is on
    /// with a token and an owner id.
    pub fn make_telegram(&self) -> Option<Arc<eve_brain::telegram::TelegramInlet>> {
        todo!()
    }

    /// `_make_chief(inlet, watcher)`: the chief of staff, None when off (eve_chief::enabled).
    pub fn make_chief(
        &self,
        inlet: &Arc<eve_brain::telegram::TelegramInlet>,
        watcher: Option<Arc<eve_web::watch::Watcher>>,
    ) -> Option<Arc<eve_chief::Chief>> {
        todo!()
    }

    /// `_make_canvas()`: Canvas, read only, made once (`canvas`).
    pub fn make_canvas(&self) -> Option<Arc<eve_canvas::Canvas>> {
        todo!()
    }

    /// `_make_share()`: sharing, made once (`share`).
    pub fn make_share(&self) -> Option<Arc<eve_brain::share::Share>> {
        todo!()
    }

    /// `_make_agentcard()`: purchases, made once (`agentcard`).
    pub fn make_agentcard(&self) -> Option<Arc<eve_agentcard::AgentCard>> {
        todo!()
    }

    /// `_make_america()`: U.S. law and government sources, made once (`america`); a failure logs and is None.
    pub fn make_america(&self) -> Option<Arc<eve_america::America>> {
        todo!()
    }

    /// `_make_meeter(apps)`: the Meet notetaker, None when off.
    pub fn make_meeter(&self, apps: Option<&Arc<eve_composio_tools::ComposioBridge>>) -> Option<Arc<eve_meet::Meeter>> {
        todo!()
    }

    /// `_ask_owner_on_phone(question)`: the owner's answer through the text door, "" when there is no door or
    /// no chat yet.
    pub async fn ask_owner_on_phone(&self, question: &str) -> String {
        todo!()
    }

    /// `_tell_owner_on_phone(text)`: a line to the owner through the text door, when there is one.
    pub async fn tell_owner_on_phone(&self, text: &str) {
        todo!()
    }

    /// `_start_miniapp(tasks)`: the "Ask Claude" Mini App (EVE_MINIAPP=1) with its calls, Eve Link's panels, the
    /// apps' cards, the Work sessions and the phone's expressions; its jobs are pushed onto `tasks`.
    pub fn start_miniapp(&self, tasks: &mut Vec<Job>) {
        todo!()
    }

    /// `_start_phone_expressions(calls, tasks)`: the phone's expression picker, lent the Mini App's calls.
    pub fn start_phone_expressions(&self, calls: &Arc<eve_phone_call::PhoneCalls>, tasks: &mut Vec<Job>) {
        todo!()
    }
}
