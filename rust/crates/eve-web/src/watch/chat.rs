//! watch.py's `Watcher` chat tools: watch_add / list / remove / pause / resume / set_end, and /watches.

use serde_json::{Map, Value};

use super::rules::Watch;
use super::watcher::{WatchRef, Watcher};

impl Watcher {
    /// The kinds this computer can watch: ticketmaster needs its key.
    pub fn kinds(&self) -> Vec<&'static str> {
        todo!("watch.Watcher.kinds")
    }

    /// INSTRUCTIONS_BLOCK, with TICKETMASTER_LINE before "The condition:" when ticketmaster is on offer.
    pub fn instructions(&self) -> String {
        todo!("watch.Watcher.instructions")
    }

    /// The schemas, naming only the kinds on offer, so the model never picks one this computer lacks.
    pub fn tools(&self) -> Value {
        todo!("watch.Watcher.tools")
    }

    /// One tool call by name: its JSON answer.
    pub async fn handle(&self, name: &str, args: &Map<String, Value>) -> Value {
        let _ = (name, args);
        todo!("watch.Watcher.handle")
    }

    /// `_find(wid)`: by id, case and spacing aside.
    pub(crate) fn find(&self, wid: &str) -> Option<WatchRef> {
        let _ = wid;
        todo!("watch.Watcher._find")
    }

    /// `_no_such(wid)`.
    pub(crate) fn no_such(&self, wid: &str) -> Value {
        let _ = wid;
        todo!("watch.Watcher._no_such")
    }

    /// No checks and no alerts until resumed, or until `for_hours` run out; the watch keeps its state.
    pub fn pause(&self, wid: &str, for_hours: &Value) -> Value {
        let _ = (wid, for_hours);
        todo!("watch.Watcher.pause")
    }

    pub fn resume(&self, wid: &str) -> Value {
        let _ = wid;
        todo!("watch.Watcher.resume")
    }

    pub fn set_end(&self, wid: &str, for_hours: &Value) -> Value {
        let _ = (wid, for_hours);
        todo!("watch.Watcher.set_end")
    }

    /// `_summary(w)`: one watch for watch_list.
    pub(crate) fn summary(&self, w: &Watch) -> Value {
        let _ = w;
        todo!("watch.Watcher._summary")
    }

    pub fn remove(&self, wid: &str) -> Value {
        let _ = wid;
        todo!("watch.Watcher.remove")
    }

    /// `_new_id()`: one past the highest id ever handed out or on the list, never one in use.
    pub(crate) fn new_id(&self) -> String {
        todo!("watch.Watcher._new_id")
    }

    /// A new watch from watch_add's arguments, or why not.
    pub fn validate(&self, args: &Map<String, Value>) -> Result<Watch, String> {
        let _ = args;
        todo!("watch.Watcher.validate")
    }

    /// Validate, check once (waiting up to FIRST_CHECK_WAIT_SECS for the limiter), then keep it. A first check that
    /// fails for a reason that will not go away (an unknown symbol, a bad link) keeps nothing.
    pub async fn add(&self, args: &Map<String, Value>) -> Value {
        let _ = args;
        todo!("watch.Watcher.add")
    }

    /// /watches, by code: one line per watch.
    pub fn listing(&self) -> String {
        todo!("watch.Watcher.listing")
    }
}
