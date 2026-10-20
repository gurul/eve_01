//! What the daemon (daemon.py, eve-daemon's `door` and `inlet`) reads and calls on the inlet's private attributes:
//! `inlet._chat_id`, `inlet._awaiting_answer`, `inlet._conv = None`, `inlet._create`, `inlet._purchases`,
//! `inlet._ask_user(...)`, `inlet._say(...)` and `inlet._spawn(...)`. Additive accessors only: each reads or calls what
//! the inlet already has.

#[allow(unused_imports)]
use crate::prelude::*;
#[allow(unused_imports)]
use crate::telegram::*;

impl TelegramInlet {
    /// The inlet's own `Arc` (None once it is being dropped), for a caller holding `&TelegramInlet` that needs the
    /// methods taking `self: &Arc<Self>`.
    pub fn door_arc(&self) -> Option<Arc<TelegramInlet>> {
        self.me.upgrade()
    }

    /// `inlet._chat_id`: the owner's chat, None until there is one.
    pub fn door_chat_id(&self) -> Option<i64> {
        self.st.lock().chat_id
    }

    /// `inlet._awaiting_answer`: a question (a task's, or a permission yes/no) is waiting.
    pub fn door_awaiting_answer(&self) -> bool {
        self.st.lock().pending_answer.as_ref().is_some_and(|future| !future.done())
    }

    /// `inlet._conv = None`: the daemon closed the conversation itself (shutdown).
    pub fn door_forget_conversation(&self) {
        self.st.lock().conv = None;
    }

    /// `inlet._create`: the door's own Responses creator (the chief's options call through it).
    pub fn door_create(&self) -> Create {
        self.create.clone()
    }

    /// `inlet._purchases`: the purchase approvals, None without a card.
    pub fn door_purchases(&self) -> Option<Arc<dyn Purchases>> {
        self.purchases.clone()
    }

    /// `await inlet._ask_user(question, chat_id, title=title)`.
    pub async fn door_ask_user(self: &Arc<Self>, question: &str, chat_id: i64, title: &str) -> String {
        self.ask_user(question, chat_id, title, None, None, false, false, None, false).await
    }

    /// `await inlet._say(chat_id, text)`.
    pub async fn door_say(self: &Arc<Self>, chat_id: i64, text: &str) {
        self.say(chat_id, text, None, None, &[], 0, None).await;
    }

    /// `inlet._spawn(coro, name)`: a job of the chat's, kept until it is done.
    pub fn door_spawn(self: &Arc<Self>, coro: futures::future::BoxFuture<'static, ()>, name: &str) -> Job {
        self.spawn(coro, name)
    }
}
