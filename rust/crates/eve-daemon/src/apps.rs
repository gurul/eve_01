//! eve's apps' hubs (`_app_hub`, `_app_hubs`, `_lights_hub`, `_spotify_hub`, `_firetv_hub`) and the Voice PE's
//! say (`_board_say`). Each app is made once and kept in `DaemonState::hubs` under its slot; a slot present with
//! None is an app that is off this run (Python's `hasattr` with None).

use std::sync::Arc;

use crate::daemon::Daemon;
use crate::seams::DaemonHub;

/// `make_lights()`'s hub as a [`DaemonHub`].
pub struct LightsHub(pub Arc<eve_lights::Lights>);
/// `make_spotify()`'s hub.
pub struct SpotifyHub(pub Arc<eve_spotify::Spotify>);
/// `make_firetv()`'s hub.
pub struct FireTvHub(pub Arc<eve_firetv::FireTV>);
/// The OBSBOT camera's panel (`make_panel`).
pub struct ObsbotHub(pub Arc<eve_obsbot_panel::ObsbotPanel>);
/// The car (`make_pika`).
pub struct PikaHub(pub Arc<eve_pika::Pika>);

/// The Voice PE's say: `(text) -> spoken on the controller's speaker`.
pub type BoardSay = Arc<dyn Fn(String) -> futures::future::BoxFuture<'static, ()> + Send + Sync>;

impl Daemon {
    /// `_app_hub(app)`: the app's hub, made on the first ask (app.make, by slot), None when it is off.
    pub fn app_hub(&self, app: &eve_app_registry::App) -> Option<Arc<dyn DaemonHub>> {
        todo!()
    }

    /// `_app_hubs(tools_only)`: every app's hub keyed by its `hub_kwarg` (e.g. "lights"), for the chat and the
    /// voice; `tools_only`: only the apps that give tools.
    pub fn app_hubs(&self, tools_only: bool) -> eve_brain::seams::AppHubs {
        todo!()
    }

    /// `_lights_hub()`.
    pub fn lights_hub(&self) -> Option<Arc<dyn DaemonHub>> {
        todo!()
    }

    /// `_spotify_hub()`.
    pub fn spotify_hub(&self) -> Option<Arc<dyn DaemonHub>> {
        todo!()
    }

    /// `_firetv_hub()`.
    pub fn firetv_hub(&self) -> Option<Arc<dyn DaemonHub>> {
        todo!()
    }

    /// `_board_say()`: speak on the Voice PE when one is attached, None otherwise.
    pub fn board_say(&self) -> Option<BoardSay> {
        todo!()
    }

    /// Close every app's hub made this run (shutdown).
    pub async fn close_hubs(&self) {
        todo!()
    }
}
