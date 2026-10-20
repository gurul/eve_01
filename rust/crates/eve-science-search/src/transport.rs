//! The HTTP seam under `search`. Python hands httpx a transport (tests use `httpx.MockTransport`); here a
//! `Transport` sends one GET and reads the whole answer, as `httpx.AsyncClient.get` does, and `ReqwestTransport`
//! is the real one. `search` puts each attempt's timeout around the call.

use std::sync::{Arc, LazyLock};

use async_trait::async_trait;

/// Why a request got no answer: httpx's `TimeoutException`, or another `HTTPError` named by its class.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TransportError {
    #[error("timed out")]
    Timeout,
    #[error("{0}")]
    Failed(String),
}

/// One GET: the full URL (query included) and the request headers.
#[derive(Debug, Clone)]
pub struct HttpRequest {
    pub url: String,
    pub headers: Vec<(String, String)>,
}

impl HttpRequest {
    /// A header, matched case-insensitively.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

/// The answer: status, headers and the whole body.
#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpResponse {
    /// A header, matched case-insensitively (httpx's `Headers.get`).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

#[async_trait]
pub trait Transport: Send + Sync {
    async fn get(&self, request: HttpRequest) -> Result<HttpResponse, TransportError>;
}

/// The network: reqwest (rustls), redirects not followed (`follow_redirects=False`), bodies decoded (gzip,
/// brotli) as httpx does.
pub struct ReqwestTransport {
    client: reqwest::Client,
}

impl ReqwestTransport {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self { client }
    }
}

impl Default for ReqwestTransport {
    fn default() -> Self {
        Self::new()
    }
}

/// httpx's exception class names, for the log line that names the failure kind.
fn kind(e: &reqwest::Error) -> TransportError {
    if e.is_timeout() {
        TransportError::Timeout
    } else if e.is_connect() {
        TransportError::Failed("ConnectError".into())
    } else if e.is_decode() {
        TransportError::Failed("DecodingError".into())
    } else if e.is_body() {
        TransportError::Failed("ReadError".into())
    } else if e.is_redirect() {
        TransportError::Failed("TooManyRedirects".into())
    } else {
        TransportError::Failed("RemoteProtocolError".into())
    }
}

#[async_trait]
impl Transport for ReqwestTransport {
    async fn get(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        let mut builder = self.client.get(&request.url);
        for (k, v) in &request.headers {
            builder = builder.header(k, v);
        }
        let response = builder.send().await.map_err(|e| kind(&e))?;
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .filter_map(|(k, v)| v.to_str().ok().map(|v| (k.as_str().to_string(), v.to_string())))
            .collect();
        let body = response.bytes().await.map_err(|e| kind(&e))?.to_vec();
        Ok(HttpResponse { status, headers, body })
    }
}

/// One client for the process (Python opens one per call; the connection pool is reqwest's).
pub static DEFAULT_TRANSPORT: LazyLock<Arc<dyn Transport>> = LazyLock::new(|| Arc::new(ReqwestTransport::new()));
