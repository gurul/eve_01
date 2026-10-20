//! The person's apps in eve lite: composio_connect.ConnectBridge, as the daemon, the page and the brain use it.
//!
//! Python duck-types one object for all three (`self.apps`, `WebContext.apps`, the inlet's `apps=`). In Rust each
//! reads its own type: the page reads [`eve_lite_web::AppsDoor`] (implemented for `ConnectBridge`), and the brain
//! reads `eve_composio_tools::ComposioBridge`, whose session is a trait. [`ConnectApps`] holds the one
//! ConnectBridge and lends the brain a ComposioBridge over a session that *is* that bridge ([`ConnectSession`]):
//! its tools are the bridge's, and every call is the bridge's `execute`. Nothing in it reaches Composio on its own.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::future::BoxFuture;
use futures::FutureExt;
use parking_lot::Mutex;
use serde_json::{Map, Value};

use eve_composio_connect::{ConnectBridge, ConnectTokenSource};
use eve_composio_tools::{ComposioBridge, ComposioClient, ComposioConfig, ComposioError, ComposioSession};
use eve_lite_web::AppsDoor;

/// How often the brain's side looks whether Connect has listed its tools yet (ConnectBridge.start returns at once
/// and lists them on a task of its own).
pub const FEED_POLL_SECS: f64 = 0.25;

/// What eve lite's daemon needs of its apps (`self.apps`): whether they started, whether only a new sign-in can
/// help (Settings says so), `start`, `close`, and the two views the page and the brain read.
pub trait LiteApps: Send + Sync {
    /// `apps.started`: the tools are listed.
    fn started(&self) -> bool;
    /// `apps.needs_sign_in`: only a new sign-in helps now.
    fn needs_sign_in(&self) -> bool;
    /// `apps.start()`: returns at once; the tools are listed in the background.
    fn start(&self);
    /// `apps.close()`.
    fn close(&self) -> BoxFuture<'static, ()>;
    /// What Settings → Apps reads (`WebContext.apps`).
    fn door(&self) -> Option<Arc<dyn AppsDoor>>;
    /// What the brain reads (the inlet's `apps=`).
    fn brain(&self) -> Option<Arc<ComposioBridge>>;
}

/// The ConnectBridge as a Composio session: the tools it lists, and its `execute` (which never fails).
pub struct ConnectSession(pub Arc<ConnectBridge>);

#[async_trait]
impl ComposioSession for ConnectSession {
    fn session_id(&self) -> String {
        "connect".to_string()
    }

    async fn tools(&self) -> Result<Vec<Value>, ComposioError> {
        Ok(self.0.tools().into_iter().map(Value::Object).collect())
    }

    async fn execute(&self, slug: &str, arguments: &Map<String, Value>) -> Result<Value, ComposioError> {
        Ok(Value::Object(self.0.execute(slug, arguments).await))
    }
}

/// The client that only ever hands out [`ConnectSession`]: Connect holds one person's own accounts, so there are no
/// connected accounts to list for the multi-account split, and no tool runs outside the bridge.
pub struct ConnectClient(pub Arc<ConnectBridge>);

#[async_trait]
impl ComposioClient for ConnectClient {
    async fn create_session(
        &self,
        _user_id: &str,
        _toolkits: &Value,
        _multi_account: Option<&Value>,
    ) -> Result<Arc<dyn ComposioSession>, ComposioError> {
        Ok(Arc::new(ConnectSession(Arc::clone(&self.0))))
    }

    async fn use_session(&self, _session_id: &str) -> Result<Arc<dyn ComposioSession>, ComposioError> {
        Ok(Arc::new(ConnectSession(Arc::clone(&self.0))))
    }

    async fn list_connected_accounts(&self, _user_ids: &[String]) -> Result<Vec<Value>, ComposioError> {
        Ok(Vec::new())
    }

    async fn execute_tool(
        &self,
        slug: &str,
        _arguments: &Map<String, Value>,
        _user_id: &str,
        _connected_account_id: &str,
    ) -> Result<Value, ComposioError> {
        Err(ComposioError::new("RuntimeError", format!("{slug} runs through Composio Connect's own tools")))
    }
}

/// The real apps of eve lite: one ConnectBridge, and the brain's view of it.
pub struct ConnectApps {
    pub bridge: Arc<ConnectBridge>,
    brain: Arc<ComposioBridge>,
    feeder: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// The brain's ComposioBridge keeps a session state file; it is kept here, in a private temporary folder that
    /// goes with the daemon, never beside the person's own files.
    _state: Option<tempfile::TempDir>,
}

impl ConnectApps {
    /// `ConnectBridge(ConnectTokenSource(token_path))`, and the brain's view of it.
    pub fn new(token_path: &Path) -> Self {
        let bridge = Arc::new(ConnectBridge::new(Arc::new(ConnectTokenSource::new(Some(token_path)))));
        Self::over(bridge)
    }

    /// The brain's view over a bridge made elsewhere.
    pub fn over(bridge: Arc<ConnectBridge>) -> Self {
        let state = tempfile::Builder::new().prefix("eve-lite-apps-").tempdir().ok();
        let mut config = ComposioConfig::new(true, "eve-lite");
        config.toolkits = None;
        config.state_path = match &state {
            Some(dir) => dir.path().join("composio-session.json"),
            None => std::env::temp_dir().join(format!("eve-lite-apps-{}.json", std::process::id())),
        };
        let client_bridge = Arc::clone(&bridge);
        let factory: eve_composio_tools::ClientFactory =
            Arc::new(move || Ok(Arc::new(ConnectClient(Arc::clone(&client_bridge))) as Arc<dyn ComposioClient>));
        let brain = Arc::new(ComposioBridge::new(config, Some(factory)));
        ConnectApps { bridge, brain, feeder: Mutex::new(None), _state: state }
    }
}

impl LiteApps for ConnectApps {
    fn started(&self) -> bool {
        self.bridge.started()
    }

    fn needs_sign_in(&self) -> bool {
        self.bridge.needs_sign_in()
    }

    fn start(&self) {
        self.bridge.start();
        let (bridge, brain) = (Arc::clone(&self.bridge), Arc::clone(&self.brain));
        let feed = async move {
            // the brain offers the app tools once Connect has listed them, as the Python brain reads them live
            while !bridge.started() {
                tokio::time::sleep(Duration::from_secs_f64(FEED_POLL_SECS)).await;
            }
            if let Err(e) = brain.start().await {
                tracing::warn!("apps: the chat could not take Composio's tools ({})", e.kind);
            }
        };
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let mut slot = self.feeder.lock();
            if slot.as_ref().is_none_or(|h| h.is_finished()) {
                *slot = Some(handle.spawn(feed));
            }
        }
    }

    fn close(&self) -> BoxFuture<'static, ()> {
        let feeder = self.feeder.lock().take();
        let bridge = Arc::clone(&self.bridge);
        async move {
            if let Some(feeder) = feeder {
                feeder.abort();
                let _ = feeder.await;
            }
            bridge.close().await;
        }
        .boxed()
    }

    fn door(&self) -> Option<Arc<dyn AppsDoor>> {
        Some(Arc::clone(&self.bridge) as Arc<dyn AppsDoor>)
    }

    fn brain(&self) -> Option<Arc<ComposioBridge>> {
        Some(Arc::clone(&self.brain))
    }
}
