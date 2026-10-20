//! The HTTP seam under `CanvasApi`. Python hands httpx a transport (tests use `httpx.MockTransport`) and wraps it
//! in `ReadOnlyTransport`; here a `Transport` sends one request and gives the whole answer back,
//! `ReqwestTransport` is the network, and `ReadOnlyTransport` refuses every method but GET before the inner
//! transport sees it.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use parking_lot::Mutex;

use crate::{CanvasError, TIMEOUT_SECS};

/// Why a request got no answer.
#[derive(Debug, Clone, thiserror::Error)]
pub enum TransportError {
    /// httpx's `TimeoutException`.
    #[error("timed out")]
    Timeout,
    /// Another httpx `HTTPError`, named by its class (ConnectError, ReadError, ...).
    #[error("{0}")]
    Failed(String),
    /// The client was closed: httpx raises a RuntimeError, which is not an `HTTPError`.
    #[error("Cannot send a request, as the client has been closed.")]
    Closed,
    /// `ReadOnlyTransport` refused a write (a `WriteRefused`); nothing was sent.
    #[error("{0}")]
    WriteRefused(CanvasError),
}

/// One request: the method, the full URL (query included) and the headers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpRequest {
    pub method: String,
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
#[derive(Debug, Clone, Default)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpResponse {
    /// A JSON answer, as `httpx.Response(status, json=...)` builds one.
    pub fn json(status: u16, value: &serde_json::Value) -> Self {
        Self {
            status,
            headers: vec![("content-type".into(), "application/json".into())],
            body: serde_json::to_vec(value).unwrap_or_default(),
        }
    }

    /// A text answer, as `httpx.Response(status, text=...)` builds one.
    pub fn text(status: u16, text: &str) -> Self {
        Self {
            status,
            headers: vec![("content-type".into(), "text/plain; charset=utf-8".into())],
            body: text.as_bytes().to_vec(),
        }
    }

    /// Adds a header.
    pub fn with_header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// httpx's `Headers.get(name)`: matched case-insensitively, and several values of one header joined with
    /// ", ".
    pub fn header(&self, name: &str) -> Option<String> {
        let values: Vec<&str> =
            self.headers.iter().filter(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str()).collect();
        if values.is_empty() {
            None
        } else {
            Some(values.join(", "))
        }
    }

    /// `resp.text`: the body decoded (UTF-8, unreadable bytes replaced).
    pub fn text_body(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// `resp.json()`, which is `json.loads(resp.content)`: the encoding found as `json.detect_encoding` finds
    /// it (a byte order mark, or the zero bytes of UTF-16/32), then the JSON. Err names the Python exception:
    /// UnicodeDecodeError for bytes the encoding cannot read, JSONDecodeError for text that is not JSON.
    pub fn json_body(&self) -> Result<serde_json::Value, &'static str> {
        let text = decode_json_bytes(&self.body).ok_or("UnicodeDecodeError")?;
        serde_json::from_str(&text).map_err(|_| "JSONDecodeError")
    }
}

/// `json.loads(bytes)`'s decoding step; None where Python's decode raises.
fn decode_json_bytes(b: &[u8]) -> Option<String> {
    const BOM32_BE: &[u8] = b"\x00\x00\xfe\xff";
    const BOM32_LE: &[u8] = b"\xff\xfe\x00\x00";
    enum Enc {
        Utf8,
        Utf16 { le: bool, skip: usize },
        Utf32 { le: bool, skip: usize },
    }
    let enc = if b.starts_with(BOM32_BE) {
        Enc::Utf32 { le: false, skip: 4 }
    } else if b.starts_with(BOM32_LE) {
        Enc::Utf32 { le: true, skip: 4 }
    } else if b.starts_with(b"\xfe\xff") {
        Enc::Utf16 { le: false, skip: 2 }
    } else if b.starts_with(b"\xff\xfe") {
        Enc::Utf16 { le: true, skip: 2 }
    } else if let Some(rest) = b.strip_prefix(b"\xef\xbb\xbf") {
        return std::str::from_utf8(rest).ok().map(str::to_string);
    } else if b.len() >= 4 {
        if b[0] == 0 {
            if b[1] != 0 { Enc::Utf16 { le: false, skip: 0 } } else { Enc::Utf32 { le: false, skip: 0 } }
        } else if b[1] == 0 {
            if b[2] != 0 || b[3] != 0 { Enc::Utf16 { le: true, skip: 0 } } else { Enc::Utf32 { le: true, skip: 0 } }
        } else {
            Enc::Utf8
        }
    } else if b.len() == 2 && b[0] == 0 {
        Enc::Utf16 { le: false, skip: 0 }
    } else if b.len() == 2 && b[1] == 0 {
        Enc::Utf16 { le: true, skip: 0 }
    } else {
        Enc::Utf8
    };
    match enc {
        Enc::Utf8 => std::str::from_utf8(b).ok().map(str::to_string),
        Enc::Utf16 { le, skip } => {
            let body = &b[skip..];
            if !body.len().is_multiple_of(2) {
                return None;
            }
            let units = body.chunks(2).map(|c| if le { u16::from_le_bytes([c[0], c[1]]) } else { u16::from_be_bytes([c[0], c[1]]) });
            char::decode_utf16(units).collect::<Result<String, _>>().ok()
        }
        Enc::Utf32 { le, skip } => {
            let body = &b[skip..];
            if !body.len().is_multiple_of(4) {
                return None;
            }
            body.chunks(4)
                .map(|c| {
                    let a = [c[0], c[1], c[2], c[3]];
                    char::from_u32(if le { u32::from_le_bytes(a) } else { u32::from_be_bytes(a) })
                })
                .collect()
        }
    }
}

#[async_trait]
pub trait Transport: Send + Sync {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError>;

    /// Closes the connection pool; later requests fail.
    fn close(&self) {}
}

/// The network: reqwest (rustls), redirects not followed, a 10-second connect and read timeout, and one retry
/// of a failed connection (httpx's `HTTPTransport(retries=1)`).
pub struct ReqwestTransport {
    client: Mutex<Option<reqwest::Client>>,
}

impl ReqwestTransport {
    pub fn new() -> Self {
        let timeout = Duration::from_secs_f64(TIMEOUT_SECS);
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(timeout)
            .read_timeout(timeout)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self { client: Mutex::new(Some(client)) }
    }
}

impl Default for ReqwestTransport {
    fn default() -> Self {
        Self::new()
    }
}

/// httpx's exception class names, for the line that names the failure kind.
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
    } else if e.is_builder() {
        TransportError::Failed("UnsupportedProtocol".into())
    } else {
        TransportError::Failed("RemoteProtocolError".into())
    }
}

#[async_trait]
impl Transport for ReqwestTransport {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        let client = self.client.lock().clone().ok_or(TransportError::Closed)?;
        let method = reqwest::Method::from_bytes(request.method.as_bytes())
            .map_err(|_| TransportError::Failed("UnsupportedProtocol".into()))?;
        let mut attempt = 0;
        let response = loop {
            let mut builder = client.request(method.clone(), &request.url);
            for (k, v) in &request.headers {
                builder = builder.header(k, v);
            }
            match builder.send().await {
                Ok(r) => break r,
                Err(e) if e.is_connect() && attempt == 0 => attempt += 1,
                Err(e) => return Err(kind(&e)),
            }
        };
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .filter_map(|(k, v)| v.to_str().ok().map(|v| (k.as_str().to_string(), v.to_string())))
            .collect();
        let body = response.bytes().await.map_err(|e| kind(&e))?.to_vec();
        Ok(HttpResponse { status, headers, body })
    }

    fn close(&self) {
        self.client.lock().take();
    }
}

/// Wraps the real transport and refuses every method but GET before anything is sent.
#[derive(Clone)]
pub struct ReadOnlyTransport {
    pub inner: Arc<dyn Transport>,
}

impl ReadOnlyTransport {
    pub fn new(inner: Arc<dyn Transport>) -> Self {
        Self { inner }
    }
}

#[async_trait]
impl Transport for ReadOnlyTransport {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        if request.method.to_uppercase() != "GET" {
            return Err(TransportError::WriteRefused(CanvasError::write_refused(format!(
                "eve only reads Canvas; a {} was refused",
                request.method
            ))));
        }
        self.inner.send(request).await
    }

    fn close(&self) {
        self.inner.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn body(b: &[u8]) -> Result<serde_json::Value, &'static str> {
        HttpResponse { status: 200, headers: Vec::new(), body: b.to_vec() }.json_body()
    }

    #[test]
    fn json_body_decodes_as_json_loads_of_bytes_does() {
        assert_eq!(body(b"[1]"), Ok(json!([1])));
        assert_eq!(body(b"\xef\xbb\xbf{\"a\": 1}"), Ok(json!({"a": 1})));
        assert_eq!(body(b"\x00[\x001\x00]"), Ok(json!([1])));
        assert_eq!(body(b"[\x001\x00]\x00"), Ok(json!([1])));
        assert_eq!(body(b"\xff\xfe[\x001\x00]\x00"), Ok(json!([1])));
        assert_eq!(body(b"\x00\x00\x00[\x00\x00\x001\x00\x00\x00]"), Ok(json!([1])));
        assert_eq!(body(b"[\"\xff\"]"), Err("UnicodeDecodeError"));
        assert_eq!(body(b"\x00[\x001\x00"), Err("UnicodeDecodeError"));
        assert_eq!(body(b"\x00[\x001"), Err("JSONDecodeError"));
        assert_eq!(body(b"<html>"), Err("JSONDecodeError"));
        assert_eq!(body(b""), Err("JSONDecodeError"));
    }
}
