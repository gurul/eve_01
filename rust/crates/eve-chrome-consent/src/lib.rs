//! The owner approves eve's connection to their Chrome from the phone.
//!
//! Chrome asks "Allow remote debugging?" for every new connection to the owner's real Chrome (browser_lane,
//! attach mode) — the only lock between their logged-in browser and any program on the Mac. The owner is often
//! away from the desk (owner, 2026-09-23: "i won't be on computer to approve … text through telegram if it's
//! ok"), so the dialog's answer comes from them over Telegram and eve presses the button they chose:
//!
//! * It is only ever used while eve ITSELF is connecting (`BrowserLane.connect`): the dialog it answers is
//!   the one eve's own connection raised, never a prompt some other program caused on its own.
//! * It acts only on a dialog that reads "Allow remote debugging?" and has an Allow button — Chrome's own.
//! * Allow is pressed only on the owner's clear yes (`eve_consent::approves`: fail-closed). No, anything
//!   unclear, no answer in time, or no way to ask (Telegram off): Cancel is pressed — never a dialog left
//!   hanging — and the task goes to Codex, which already drives the owner's Chrome without this prompt.
//! * `EVE_CHROME_ACCESS=allow` is the owner's standing yes (owner, 2026-09-24: "Allow remote debugging
//!   should be allowed automatically don't ask me"): Allow is pressed at once, with no question. The two rules
//!   above still hold — only eve's own connection, only Chrome's own dialog. The default is `ask`.
//!
//! The buttons are Chrome web-UI controls: macOS Accessibility's AXPress presses them (a synthetic click does
//! not), through the AX API directly, with the daemon's own Accessibility permission.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use parking_lot::Mutex;
use tokio::task::JoinHandle;
use tokio::time::{sleep, Instant};

#[cfg(target_os = "macos")]
mod mac;

pub const DIALOG_TEXT: &str = "Allow remote debugging?";
/// How long after eve starts connecting the dialog may take to show.
pub const APPEAR_SECS: f64 = 10.0;
pub const POLL_SECS: f64 = 0.3;
/// Presses of one button, SETTLE_SECS apart, until the dialog is gone.
pub const PRESS_TRIES: usize = 5;
/// After a press: Chrome's dialog ignores an AXPress in its first moments and still reports the press as done.
/// Measured 2026-09-24 (Chrome, 10 fresh dialogs): a press ~0.25 s after the heading appeared did nothing 9
/// times of 10; a second press 0.5 s later dismissed it 5 of 5. So "pressed" is never believed — only "gone".
pub const SETTLE_SECS: f64 = 0.5;
/// The owner's time to answer: inside browser_lane.CONSENT_TIMEOUT_MS (120 s), so a late yes never lands after
/// the connection has given up.
pub const ASK_TIMEOUT_SECS: f64 = 100.0;
pub const GONE_LINE: &str = "Chrome's question was answered at the Mac, so you don't need to reply.";
pub const PRESS_FAILED_LINE: &str = "I couldn't press the button in Chrome: macOS may need to let eve control System Events \
(System Settings → Privacy & Security → Automation). Codex will take this task.";
pub const QUESTION: &str = "eve wants to control your Chrome (your logged-in browser) for a task: Chrome is asking \
\"Allow remote debugging?\". Allow it? yes / no";

// Chrome's dialog, as macOS Accessibility sees it (probed live, 2026-09-23): an AXHeading titled
// "Allow remote debugging?" and AXButtons "Allow" / "Cancel" in the same window, several AXGroups deep. An
// earlier AppleScript version looked for an AXStaticText and never matched the heading, so the phone was never
// asked; the AX API is read directly now (the daemon's own Accessibility permission).
pub const MAX_NODES: usize = 20000;

// ---------------------------------------------------------------------------------------------------------------
// The Accessibility seam
// ---------------------------------------------------------------------------------------------------------------

/// An attribute value as `AXUIElementCopyAttributeValue` hands it back, converted the way pyobjc does.
#[derive(Debug, Clone, PartialEq)]
pub enum AxValue<N> {
    Str(String),
    Bool(bool),
    Int(i64),
    Float(f64),
    Element(N),
    Array(Vec<AxValue<N>>),
    Other(String),
}

impl<N> AxValue<N> {
    /// Python truthiness of the converted value.
    pub fn truthy(&self) -> bool {
        match self {
            AxValue::Str(s) => !s.is_empty(),
            AxValue::Bool(b) => *b,
            AxValue::Int(n) => *n != 0,
            AxValue::Float(x) => *x != 0.0,
            AxValue::Array(items) => !items.is_empty(),
            AxValue::Element(_) | AxValue::Other(_) => true,
        }
    }

    /// Python's `str()` of the converted value.
    pub fn py_str(&self) -> String {
        match self {
            AxValue::Str(s) | AxValue::Other(s) => s.clone(),
            AxValue::Bool(b) => if *b { "True" } else { "False" }.to_string(),
            AxValue::Int(n) => n.to_string(),
            AxValue::Float(x) => {
                if x.is_finite() && x.fract() == 0.0 && x.abs() < 1e16 {
                    format!("{x:.1}")
                } else {
                    format!("{x}")
                }
            }
            AxValue::Element(_) => "<AXUIElement>".to_string(),
            AxValue::Array(items) => {
                let inner: Vec<String> = items.iter().map(|v| v.py_str()).collect();
                format!("({})", inner.join(", "))
            }
        }
    }
}

/// The three AX calls the Python takes from `_ax()`: create an application element, copy an attribute, perform
/// an action. Tests fake it; [`mac`] is the real one.
pub trait Ax {
    type Node: Clone;
    /// `AXUIElementCreateApplication(pid)`.
    fn application(&self, pid: i32) -> Self::Node;
    /// `AXUIElementCopyAttributeValue`, `None` on any error (Python's `attr`).
    fn attr(&self, el: &Self::Node, name: &str) -> Option<AxValue<Self::Node>>;
    /// `AXUIElementPerformAction`: the AXError code, 0 on success.
    fn perform(&self, el: &Self::Node, action: &str) -> i32;
}

/// Why Chrome's dialog could not be read: Python logs the exception's type name, never a crash.
#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    /// No Accessibility API on this platform (Python: pyobjc's ApplicationServices does not import).
    #[error("no Accessibility API on this platform")]
    NoAccessibility,
    /// `pgrep` could not be run.
    #[error("pgrep: {0}")]
    Pgrep(#[from] std::io::Error),
    /// The blocking AX task died.
    #[error("accessibility task failed: {0}")]
    Task(String),
}

impl ReadError {
    /// The Python exception type name the log line carries.
    pub fn kind_name(&self) -> &'static str {
        match self {
            ReadError::NoAccessibility => "ModuleNotFoundError",
            ReadError::Pgrep(e) if e.kind() == std::io::ErrorKind::NotFound => "FileNotFoundError",
            ReadError::Pgrep(e) if e.kind() == std::io::ErrorKind::PermissionDenied => "PermissionError",
            ReadError::Pgrep(_) => "OSError",
            ReadError::Task(_) => "RuntimeError",
        }
    }
}

/// `press` only ever presses Allow or Cancel (Python raises `ValueError(label)`).
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("{0}")]
pub struct InvalidLabel(pub String);

/// The PIDs in `pgrep`'s output: every all-digit word.
pub fn parse_pids(stdout: &str) -> Vec<i32> {
    stdout
        .split_whitespace()
        .filter(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
        .filter_map(|p| p.parse().ok())
        .collect()
}

/// Every running Chrome (two profiles' instances, say): the dialog is in whichever one eve connected to, and
/// the first PID pgrep prints need not be it.
pub fn chrome_pids() -> Result<Vec<i32>, ReadError> {
    let out = std::process::Command::new("pgrep").args(["-x", "Google Chrome"]).output()?;
    Ok(parse_pids(&String::from_utf8_lossy(&out.stdout)))
}

/// Python's `str.strip()` with no argument: Rust's `trim` plus the four separators `\x1c`..`\x1f`, which
/// Python counts as whitespace and Rust does not.
pub fn py_strip(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || ('\x1c'..='\x1f').contains(&c))
}

fn label_of<A: Ax>(ax: &A, el: &A::Node) -> String {
    for name in ["AXTitle", "AXDescription", "AXValue"] {
        if let Some(v) = ax.attr(el, name) {
            if v.truthy() {
                return v.py_str();
            }
        }
    }
    String::new()
}

fn nodes_of<N>(value: Option<AxValue<N>>) -> Vec<N> {
    match value {
        Some(AxValue::Array(items)) => items
            .into_iter()
            .filter_map(|v| match v {
                AxValue::Element(n) => Some(n),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// (dialog showing?, the button labelled `label` in that dialog's window, or None), over the given Chrome
/// PIDs. Walks each Chrome's windows only as deep as needed; never presses anything.
pub fn find_dialog_in<A: Ax>(ax: &A, pids: &[i32], label: &str) -> (bool, Option<A::Node>) {
    for &pid in pids {
        let app = ax.application(pid);
        for window in nodes_of(ax.attr(&app, "AXWindows")) {
            let (mut heading, mut button, mut count, mut stack) = (false, None, 0usize, vec![window]);
            while count < MAX_NODES {
                let Some(el) = stack.pop() else { break };
                count += 1;
                let role = match ax.attr(&el, "AXRole") {
                    Some(AxValue::Str(s)) => s,
                    _ => String::new(),
                };
                let text = label_of(ax, &el);
                if (role == "AXHeading" || role == "AXStaticText") && py_strip(&text) == DIALOG_TEXT {
                    heading = true;
                } else if role == "AXButton" && !label.is_empty() && py_strip(&text) == label && button.is_none() {
                    button = Some(el.clone());
                }
                stack.extend(nodes_of(ax.attr(&el, "AXChildren")));
            }
            if heading {
                return (true, button);
            }
        }
    }
    (false, None)
}

/// Python's `_find_dialog` on this Mac: is Chrome's dialog showing, and does it have the `label` button.
/// Blocking (it walks the AX tree); call it off the async runtime.
pub fn find_dialog(label: &str) -> Result<(bool, bool), ReadError> {
    #[cfg(target_os = "macos")]
    {
        let ax = mac::MacAx;
        let pids = chrome_pids()?;
        let (showing, button) = find_dialog_in(&ax, &pids, label);
        Ok((showing, button.is_some()))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = label;
        Err(ReadError::NoAccessibility)
    }
}

/// Find the dialog and press `label` in it, in one blocking pass (the element never leaves the thread).
fn find_and_press(label: &str) -> Result<bool, ReadError> {
    #[cfg(target_os = "macos")]
    {
        let ax = mac::MacAx;
        let pids = chrome_pids()?;
        Ok(press_in(&ax, &pids, label))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = label;
        Err(ReadError::NoAccessibility)
    }
}

/// The press itself over any [`Ax`]: false when no dialog shows or it has no such button.
pub fn press_in<A: Ax>(ax: &A, pids: &[i32], label: &str) -> bool {
    let (showing, button) = find_dialog_in(ax, pids, label);
    match (showing, button) {
        (true, Some(button)) => ax.perform(&button, "AXPress") == 0,
        _ => false,
    }
}

/// Is Chrome's remote-debugging dialog showing? No Accessibility, no AX API: "not showing", never a crash.
pub async fn dialog_showing() -> bool {
    let result = tokio::task::spawn_blocking(|| find_dialog(""))
        .await
        .unwrap_or_else(|e| Err(ReadError::Task(e.to_string())));
    match result {
        Ok((showing, _)) => showing,
        Err(e) => {
            tracing::warn!("chrome-consent: cannot read Chrome's dialog ({})", e.kind_name());
            false
        }
    }
}

/// Press Allow or Cancel on Chrome's own remote-debugging dialog. `Ok(false)` when no such dialog is showing.
pub async fn press(label: &str) -> Result<bool, InvalidLabel> {
    if label != "Allow" && label != "Cancel" {
        return Err(InvalidLabel(label.to_string()));
    }
    let owned = label.to_string();
    let result = tokio::task::spawn_blocking(move || find_and_press(&owned))
        .await
        .unwrap_or_else(|e| Err(ReadError::Task(e.to_string())));
    Ok(match result {
        Ok(pressed) => pressed,
        Err(e) => {
            tracing::warn!("chrome-consent: could not press {} ({})", label, e.kind_name());
            false
        }
    })
}

/// `allow` (the owner's standing yes) or `ask` (the default), from the given environment. Anything else is
/// `ask`.
pub fn access_preference_in(environ: &HashMap<String, String>) -> &'static str {
    pref_of(environ.get("EVE_CHROME_ACCESS").map(String::as_str))
}

/// `allow` or `ask` from this process's `EVE_CHROME_ACCESS`.
pub fn access_preference() -> &'static str {
    pref_of(std::env::var("EVE_CHROME_ACCESS").ok().as_deref())
}

fn pref_of(value: Option<&str>) -> &'static str {
    if py_strip(value.unwrap_or("")).to_lowercase() == "allow" {
        "allow"
    } else {
        "ask"
    }
}

// ---------------------------------------------------------------------------------------------------------------
// The broker
// ---------------------------------------------------------------------------------------------------------------

/// `ask_owner(question) -> reply` (Telegram). An `Err` is "no answer".
pub type AskOwner = Arc<dyn Fn(String) -> BoxFuture<'static, anyhow::Result<String>> + Send + Sync>;
/// `tell_owner(text)`: best effort.
pub type TellOwner = Arc<dyn Fn(String) -> BoxFuture<'static, anyhow::Result<()>> + Send + Sync>;
/// Is Chrome's dialog showing (the Accessibility seam).
pub type Showing = Arc<dyn Fn() -> BoxFuture<'static, bool> + Send + Sync>;
/// Press a button, true when the press reported success (the Accessibility seam).
pub type Pressing = Arc<dyn Fn(String) -> BoxFuture<'static, bool> + Send + Sync>;

/// The real [`Showing`]: [`dialog_showing`].
pub fn default_showing() -> Showing {
    Arc::new(|| Box::pin(dialog_showing()))
}

/// The real [`Pressing`]: [`press`]. The broker only ever passes Allow or Cancel, so the label check never
/// fails here; a label it would reject presses nothing.
pub fn default_pressing() -> Pressing {
    Arc::new(|label: String| Box::pin(async move { press(&label).await.unwrap_or(false) }))
}

/// The keyword arguments of Python's `ConsentBroker.__init__`.
#[derive(Clone)]
pub struct BrokerOptions {
    pub tell_owner: Option<TellOwner>,
    pub showing: Showing,
    pub pressing: Pressing,
    pub appear_secs: f64,
    pub ask_timeout_secs: f64,
    pub poll_secs: f64,
    pub settle_secs: f64,
    pub auto_allow: bool,
}

impl Default for BrokerOptions {
    fn default() -> Self {
        Self {
            tell_owner: None,
            showing: default_showing(),
            pressing: default_pressing(),
            appear_secs: APPEAR_SECS,
            ask_timeout_secs: ASK_TIMEOUT_SECS,
            poll_secs: POLL_SECS,
            settle_secs: SETTLE_SECS,
            auto_allow: false,
        }
    }
}

/// A spawned task that is aborted when dropped: the phone question dies with the broker's answer, as the
/// Python's `asking` task is cancelled when `answer_own_connection` is.
struct Asking {
    task: JoinHandle<anyhow::Result<String>>,
    /// The handle has been awaited to its end (a JoinHandle must not be polled again after that).
    joined: bool,
}

impl Asking {
    fn done(&self) -> bool {
        self.joined || self.task.is_finished()
    }

    /// The finished question's reply (`asking.result()`): the error kind when it raised or died.
    async fn result(&mut self) -> Result<String, &'static str> {
        self.joined = true;
        match (&mut self.task).await {
            Ok(Ok(reply)) => Ok(reply),
            Ok(Err(_)) => Err("Exception"),
            Err(_) => Err("JoinError"),
        }
    }

    /// Withdraw the question and wait for it to wind down (`asking.cancel(); await gather(asking)`), when it
    /// has not finished yet.
    async fn cancel(&mut self) {
        if self.done() {
            return;
        }
        self.task.abort();
        self.joined = true;
        let _ = (&mut self.task).await;
    }
}

impl Drop for Asking {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn secs(x: f64) -> Duration {
    Duration::try_from_secs_f64(x.max(0.0)).unwrap_or(Duration::MAX)
}

/// Answers the Chrome dialog raised by eve's own connection with the owner's Telegram yes or no.
///
/// `ask_owner` (Telegram), or `None` when there is no way to ask: then the dialog is cancelled — unless
/// `auto_allow` (the owner's standing yes), which presses Allow without asking. `showing`/`pressing` are the
/// Accessibility seams (tests fake them). Cheap to clone: clones share `last`.
#[derive(Clone)]
pub struct ConsentBroker {
    ask: Option<AskOwner>,
    tell: Option<TellOwner>,
    showing: Showing,
    pressing: Pressing,
    auto_allow: bool,
    appear: Duration,
    ask_timeout: Duration,
    poll: Duration,
    settle: Duration,
    last: Arc<Mutex<&'static str>>,
}

impl ConsentBroker {
    pub fn new(ask_owner: Option<AskOwner>, opts: BrokerOptions) -> Self {
        Self {
            ask: ask_owner,
            tell: opts.tell_owner,
            showing: opts.showing,
            pressing: opts.pressing,
            auto_allow: opts.auto_allow,
            appear: secs(opts.appear_secs),
            ask_timeout: secs(opts.ask_timeout_secs),
            poll: secs(opts.poll_secs),
            settle: secs(opts.settle_secs),
            last: Arc::new(Mutex::new("")),
        }
    }

    /// What happened last: allowed | declined | no_dialog | … ("" before the first answer).
    pub fn last(&self) -> &'static str {
        *self.last.lock()
    }

    fn set_last(&self, outcome: &'static str) -> &'static str {
        *self.last.lock() = outcome;
        outcome
    }

    /// Called while eve's own connect is in flight. Waits for Chrome's dialog, asks the owner (or not, on the
    /// standing yes), presses the button. Returns allowed | auto_allowed | declined | unanswered | no_dialog |
    /// no_way_to_ask | answered_at_mac | press_failed | dialog_gone. Every outcome is logged: a silent early
    /// return once hid why the owner had to click at the Mac (2026-09-24).
    ///
    /// Dropping the future (connect() is done with it) withdraws the phone question too, or the owner's next
    /// unrelated message is swallowed as its answer.
    pub async fn answer_own_connection(&self) -> &'static str {
        let outcome = self.answer().await;
        self.set_last(outcome);
        tracing::info!("chrome-consent: {}", outcome);
        outcome
    }

    async fn answer(&self) -> &'static str {
        let deadline = Instant::now() + self.appear;
        while !(self.showing)().await {
            if Instant::now() >= deadline {
                // already allowed on this connection, or Chrome did not ask
                return self.set_last("no_dialog");
            }
            sleep(self.poll).await;
        }
        if self.auto_allow {
            let outcome = self.press_home("Allow").await;
            return if outcome == "pressed" { "auto_allowed" } else { outcome };
        }
        let Some(ask) = self.ask.clone() else {
            self.press_home("Cancel").await;
            return self.set_last("no_way_to_ask");
        };
        let mut asking = Asking { task: tokio::spawn(ask(QUESTION.to_string())), joined: false };
        let deadline = Instant::now() + self.ask_timeout;
        let reply: Result<String, &'static str> = loop {
            if asking.done() {
                break asking.result().await;
            }
            if Instant::now() >= deadline {
                break Err("TimeoutError");
            }
            sleep(self.poll).await;
            if !asking.done() && !(self.showing)().await {
                // Answered at the Mac (or the connection gave up): withdraw the phone question, so the owner's
                // next text is not swallowed as its answer.
                asking.cancel().await;
                self.say(GONE_LINE).await;
                return self.set_last("answered_at_mac");
            }
        };
        let reply = match reply {
            Ok(reply) => reply,
            Err(kind) => {
                // no answer is a no
                tracing::info!("chrome-consent: no answer from the owner ({}); cancelling", kind);
                asking.cancel().await;
                self.press_home("Cancel").await;
                return self.set_last("unanswered");
            }
        };
        if eve_consent::approves(&reply) {
            let outcome = self.press_home("Allow").await;
            if outcome == "press_failed" {
                self.say(PRESS_FAILED_LINE).await; // still up and we could not press it: say why
            }
            self.set_last(if outcome == "pressed" { "allowed" } else { outcome })
        } else {
            self.press_home("Cancel").await;
            self.set_last("declined")
        }
    }

    /// Press `label` until the dialog is gone. `pressed`: gone after a press of ours; `dialog_gone`: gone though
    /// no press landed (answered at the Mac, or the connection gave up); `press_failed`: still up after
    /// PRESS_TRIES. A press is only counted once the dialog has gone (see SETTLE_SECS): the daemon logged
    /// `auto_allowed` at 15:16 on 2026-09-24 while the dialog stayed on screen for the owner to click.
    async fn press_home(&self, label: &str) -> &'static str {
        for _ in 0..PRESS_TRIES {
            let pressed = (self.pressing)(label.to_string()).await;
            sleep(self.settle).await;
            if !(self.showing)().await {
                return if pressed { "pressed" } else { "dialog_gone" };
            }
        }
        "press_failed"
    }

    async fn say(&self, text: &str) {
        if let Some(tell) = &self.tell {
            // telling is best effort
            let _ = tell(text.to_string()).await;
        }
    }
}

#[cfg(test)]
mod tests;
