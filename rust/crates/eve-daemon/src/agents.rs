//! The computer-use agent factory (`_make_agent`) and its bodies: the reflex in front of Codex, the Chrome lane,
//! the web reader, the desk lane (`_warn_removed_computer`, `_make_chrome_lane`, `_desk`, `_bodies`,
//! `_reader_route`, `_chrome_body`).

use std::collections::HashMap;
use std::sync::Arc;

use crate::daemon::Daemon;
use crate::seams::{AskUser, Environ, OnEvent, TaskAgent};

/// `make_inner(floor)`: the agent a body wraps (Codex, warm when one is ready).
pub type MakeInner = Arc<dyn Fn(Option<String>) -> Arc<dyn TaskAgent> + Send + Sync>;

/// One body `_bodies` offers the reflex: its name and the agent it makes.
pub type Bodies = HashMap<String, MakeInner>;

/// `_warn_removed_computer(environ)`: EVE_COMPUTER / EVE_COMPUTER_AGENT named a body that was removed (Holo, the
/// legacy worker); log once and say so. True when it warned.
pub fn warn_removed_computer(environ: Option<&Environ>) -> bool {
    todo!()
}

impl Daemon {
    /// `_make_agent(on_event, ask_user, floor=None)`: the agent for one desktop task. Codex behind the reflex
    /// (app_reflex) with the Chrome lane, the web reader and the desk lane as bodies; remembered as
    /// `active_agent`.
    pub fn make_agent(&self, on_event: OnEvent, ask_user: AskUser, floor: Option<String>) -> Arc<dyn TaskAgent> {
        todo!()
    }

    /// The daemon's [`eve_brain::seams::AgentFactory`]: `make_agent` behind an `Arc`.
    pub fn agent_factory(&self) -> eve_brain::seams::AgentFactory {
        todo!()
    }

    /// `_make_chrome_lane()`: the browser lane for the daemon's life (EVE_BROWSER_ATTACH=1), made once; None when
    /// off.
    pub fn make_chrome_lane(&self) -> Option<Arc<eve_browser_lane::BrowserLane>> {
        todo!()
    }

    /// `_desk()`: the desk lane, made once (EVE_DESK_LANE on), None when off.
    pub fn desk(&self) -> Option<Arc<eve_desk_lane::DeskLane>> {
        todo!()
    }

    /// `_bodies(make_inner, on_event, ask_user)`: the bodies the reflex may hand a goal to.
    pub fn bodies(&self, make_inner: MakeInner, on_event: OnEvent, ask_user: AskUser) -> Bodies {
        todo!()
    }

    /// `_reader_route()`: browser_router's Jev router for the web reader, made once; None when off.
    pub fn reader_route(&self) -> Option<Arc<eve_browser_router::Router>> {
        todo!()
    }

    /// `_chrome_body(make_inner, on_event, ask_user)`: the Chrome lane as a body (browser-only goals), with its
    /// planner; empty when the lane is off.
    pub fn chrome_body(&self, make_inner: MakeInner, on_event: OnEvent, ask_user: AskUser) -> Bodies {
        todo!()
    }
}
