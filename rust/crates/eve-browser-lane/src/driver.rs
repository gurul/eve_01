//! The browser the lane drives, as the handful of Playwright objects browser_lane.py touches: a driver
//! (`sync_playwright().start()`), a browser connected over CDP, its contexts (Chrome profiles), and pages.
//!
//! These traits are the seam the Python tests monkeypatched (`playwright.sync_api`, a fake page answering the
//! lane's scripts by identity). The real implementation is [`crate::cdp`]: the Chrome DevTools Protocol spoken
//! directly, with Playwright's waiting rules for the few actions the lane takes.
//!
//! Errors carry the Playwright exception's class name as an [`eve_desktop_lanes::LaneError`] kind
//! ("TimeoutError", "Error", "TargetClosedError"), because the lane's refusal lines print `type(e).__name__`.

use std::path::Path;
use std::sync::Arc;

use serde_json::Value;

/// `locator.select_option(value=...)` or `locator.select_option(label=...)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectBy<'a> {
    Value(&'a str),
    Label(&'a str),
}

/// What the lane asks of one Playwright page. Every locator call is `page.locator(selector).first`.
pub trait PageHandle: Send {
    /// `page.evaluate(script, arg)`; `arg` None calls the function with no argument.
    fn evaluate(&self, script: &str, arg: Option<&Value>) -> anyhow::Result<Value>;
    /// `page.title()`.
    fn title(&self) -> anyhow::Result<String>;
    /// `page.url`: the main frame's last committed URL, read without a round trip.
    fn url(&self) -> String;
    /// `page.inner_text(selector, timeout=)`.
    fn inner_text(&self, selector: &str, timeout_ms: u64) -> anyhow::Result<String>;
    /// `page.locator(selector).count()`.
    fn count(&self, selector: &str) -> anyhow::Result<usize>;
    /// `page.locator(selector).first.scroll_into_view_if_needed(timeout=)`.
    fn scroll_into_view_if_needed(&self, selector: &str, timeout_ms: u64) -> anyhow::Result<()>;
    /// `page.locator(selector).first.click(timeout=)`.
    fn click(&self, selector: &str, timeout_ms: u64) -> anyhow::Result<()>;
    /// `page.locator(selector).first.fill(value, timeout=)`.
    fn fill(&self, selector: &str, value: &str, timeout_ms: u64) -> anyhow::Result<()>;
    /// `page.locator(selector).first.select_option(value=|label=, timeout=)`.
    fn select_option(&self, selector: &str, choice: SelectBy<'_>, timeout_ms: u64) -> anyhow::Result<()>;
    /// `page.locator(selector).first.evaluate(script)`: `script` takes the element.
    fn evaluate_on(&self, selector: &str, script: &str) -> anyhow::Result<Value>;
    /// `page.keyboard.type(text, delay=)`.
    fn keyboard_type(&self, text: &str, delay_ms: u64) -> anyhow::Result<()>;
    /// `page.keyboard.press(key)`.
    fn keyboard_press(&self, key: &str) -> anyhow::Result<()>;
    /// `page.wait_for_timeout(ms)`.
    fn wait_for_timeout(&self, ms: u64);
    /// `page.wait_for_load_state(state, timeout=)`: "load", "domcontentloaded" or "networkidle".
    fn wait_for_load_state(&self, state: &str, timeout_ms: u64) -> anyhow::Result<()>;
    /// `page.goto(url, wait_until=, timeout=)`.
    fn goto(&self, url: &str, wait_until: &str, timeout_ms: u64) -> anyhow::Result<()>;
    /// `page.bring_to_front()`.
    fn bring_to_front(&self) -> anyhow::Result<()>;
    /// `page.close()`.
    fn close(&self) -> anyhow::Result<()>;
    /// `page.screenshot(type="jpeg", quality=, mask=[page.locator(selector)], mask_color=)`; `mask` is
    /// (selector, CSS colour).
    fn screenshot_jpeg(&self, quality: u8, mask: Option<(&str, &str)>) -> anyhow::Result<Vec<u8>>;
    /// The concrete page behind this handle (the real one is a [`crate::cdp::CdpPage`]), for a caller that
    /// needs what Python reaches through the raw Playwright page (`page.route`, `page.on`); None for a fake.
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        None
    }
}

/// One cookie as `context.cookies(urls)` lists it (the lane reads only the name and the value).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cookie {
    pub name: String,
    pub value: String,
}

/// One Playwright browser context: in the owner's Chrome, one profile.
pub trait ContextHandle: Send + Sync {
    /// `context.pages`.
    fn pages(&self) -> anyhow::Result<Vec<Box<dyn PageHandle>>>;
    /// `context.new_page()`.
    fn new_page(&self) -> anyhow::Result<Box<dyn PageHandle>>;
    /// `context.cookies(urls)`.
    fn cookies(&self, urls: &[&str]) -> anyhow::Result<Vec<Cookie>>;
    /// `context.close()`.
    fn close(&self) -> anyhow::Result<()>;
}

/// A browser `connect_over_cdp` returned.
pub trait BrowserHandle: Send + Sync {
    /// `browser.contexts`: the same objects (by identity) on every call while they exist.
    fn contexts(&self) -> anyhow::Result<Vec<Arc<dyn ContextHandle>>>;
    /// `browser.is_connected()`.
    fn is_connected(&self) -> bool;
}

/// A started Playwright (`sync_playwright().start()`).
pub trait Driver: Send {
    /// `pw.chromium.connect_over_cdp(endpoint, timeout=)`.
    fn connect_over_cdp(&mut self, endpoint: &str, timeout_ms: u64) -> anyhow::Result<Arc<dyn BrowserHandle>>;
    /// `pw.chromium.launch_persistent_context(profile, headless=, viewport=, args=)`.
    fn launch_persistent_context(
        &mut self,
        profile: &Path,
        headless: bool,
        viewport: (u32, u32),
        args: &[&str],
    ) -> anyhow::Result<Arc<dyn ContextHandle>>;
    /// `pw.stop()`: every connection dropped, every browser it launched ended.
    fn stop(&mut self) -> anyhow::Result<()>;
}

/// `sync_playwright().start()`, as a value the tests replace.
pub type StartDriver = Arc<dyn Fn() -> anyhow::Result<Box<dyn Driver>> + Send + Sync>;

/// Python's `is`: the same object behind two handles.
pub fn same_context(a: &Arc<dyn ContextHandle>, b: &Arc<dyn ContextHandle>) -> bool {
    std::ptr::eq(Arc::as_ptr(a) as *const (), Arc::as_ptr(b) as *const ())
}
