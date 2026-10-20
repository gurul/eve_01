//! Port of `eve_bridge.browser_lane`: a browser drives a web task, the plan executor and Jev decide each step.
//!
//! [`browser_lane`] is the module itself; [`driver`] is the seam Playwright filled in Python, and [`cdp`] is
//! the real driver over the Chrome DevTools Protocol. [`js`] holds the page scripts, byte for byte.

pub mod browser_lane;
pub mod cdp;
pub mod driver;
pub mod js;

pub use browser_lane::*;
