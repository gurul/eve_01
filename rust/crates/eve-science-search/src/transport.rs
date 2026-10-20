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
    /// The first value of a header, matched case-insensitively.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    /// httpx's `Headers.get`: every value of a header, matched case-insensitively, joined by ", " (a repeated
    /// `Retry-After` reads "12, 13", which no number parses).
    pub fn joined_header(&self, name: &str) -> Option<String> {
        let values: Vec<&str> =
            self.headers.iter().filter(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str()).collect();
        (!values.is_empty()).then(|| values.join(", "))
    }
}

/// Header bytes as httpx decodes them: ASCII when every name and value is ASCII, else UTF-8 when every one is
/// valid UTF-8, else ISO-8859-1 (`Headers.encoding`).
fn decode_headers(raw: &[(&[u8], &[u8])]) -> Vec<(String, String)> {
    let all = |ok: &dyn Fn(&[u8]) -> bool| raw.iter().all(|(k, v)| ok(k) && ok(v));
    let latin1 = |b: &[u8]| b.iter().map(|&c| char::from(c)).collect::<String>();
    if all(&|b: &[u8]| std::str::from_utf8(b).is_ok()) {
        raw.iter()
            .map(|(k, v)| (String::from_utf8_lossy(k).into_owned(), String::from_utf8_lossy(v).into_owned()))
            .collect()
    } else {
        raw.iter().map(|(k, v)| (latin1(k), latin1(v))).collect()
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
        let raw: Vec<(&[u8], &[u8])> =
            response.headers().iter().map(|(k, v)| (k.as_str().as_bytes(), v.as_bytes())).collect();
        let headers = decode_headers(&raw);
        let body = response.bytes().await.map_err(|e| kind(&e))?.to_vec();
        Ok(HttpResponse { status, headers, body })
    }
}

/// One client for the process (Python opens one per call; the connection pool is reqwest's).
pub static DEFAULT_TRANSPORT: LazyLock<Arc<dyn Transport>> = LazyLock::new(|| Arc::new(ReqwestTransport::new()));

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headers_decode_as_httpx_does() {
        assert_eq!(decode_headers(&[(b"a", b"x"), (b"b", "\u{661}".as_bytes())])[1].1, "\u{661}");
        assert_eq!(decode_headers(&[(b"a", b"x"), (b"b", b"\xe9")])[1].1, "\u{e9}");
        let r = HttpResponse {
            status: 429,
            headers: vec![("Retry-After".into(), "12".into()), ("retry-after".into(), "13".into())],
            body: Vec::new(),
        };
        assert_eq!(r.joined_header("RETRY-AFTER").as_deref(), Some("12, 13"));
        assert_eq!(r.header("retry-after"), Some("12"));
        assert_eq!(r.joined_header("x"), None);
    }
}
