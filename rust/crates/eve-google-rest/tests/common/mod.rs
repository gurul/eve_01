//! The Python tests' `httpx.MockTransport`: every request is recorded and answered by a route function.
#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use base64::Engine;
use bytes::Bytes;
use eve_google_rest::{BodyStream, GoogleRestClient, HttpRequest, HttpResponse, Transport, TransportError, Zone};
use serde_json::Value;

pub const LA: Zone = Zone::Named(chrono_tz::America::Los_Angeles);

/// A request as a test reads it.
#[derive(Clone, Debug)]
pub struct Req {
    pub method: String,
    pub url: url::Url,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
}

impl Req {
    /// The decoded path, as httpx's `URL.path`.
    pub fn path(&self) -> String {
        urlencoding::decode(self.url.path()).map(|s| s.into_owned()).unwrap_or_default()
    }

    pub fn param(&self, k: &str) -> Option<String> {
        self.url.query_pairs().find(|(key, _)| key == k).map(|(_, v)| v.into_owned())
    }

    pub fn params_all(&self, k: &str) -> Vec<String> {
        self.url.query_pairs().filter(|(key, _)| key == k).map(|(_, v)| v.into_owned()).collect()
    }

    pub fn has_params(&self) -> bool {
        self.url.query().is_some_and(|q| !q.is_empty())
    }

    pub fn header(&self, k: &str) -> Option<String> {
        self.headers.iter().find(|(key, _)| key.eq_ignore_ascii_case(k)).map(|(_, v)| v.clone())
    }

    pub fn json(&self) -> Value {
        serde_json::from_slice(self.body.as_deref().unwrap_or(b"null")).expect("a JSON body")
    }
}

pub enum Reply {
    Json(u16, Value),
    Raw { status: u16, content_type: String, body: Vec<u8> },
    Stream { status: u16, content_type: String, body: BodyStream },
    Fail(TransportError),
}

impl From<Value> for Reply {
    fn from(v: Value) -> Self {
        Reply::Json(200, v)
    }
}

pub fn status(code: u16, v: Value) -> Reply {
    Reply::Json(code, v)
}

pub fn text(body: &str, content_type: &str) -> Reply {
    Reply::Raw { status: 200, content_type: content_type.into(), body: body.as_bytes().to_vec() }
}

type Route = Box<dyn Fn(&Req) -> Reply + Send + Sync>;

pub struct Mock {
    route: Route,
    pub seen: Mutex<Vec<Req>>,
}

impl Mock {
    pub fn seen(&self) -> Vec<Req> {
        self.seen.lock().unwrap().clone()
    }

    pub fn clear(&self) {
        self.seen.lock().unwrap().clear();
    }
}

#[async_trait]
impl Transport for Mock {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        let req = Req {
            method: request.method,
            url: url::Url::parse(&request.url).expect("a valid URL"),
            headers: request.headers,
            body: request.body,
        };
        self.seen.lock().unwrap().push(req.clone());
        let one = |b: Vec<u8>| -> BodyStream { Box::pin(futures::stream::iter(vec![Ok(Bytes::from(b))])) };
        match (self.route)(&req) {
            Reply::Json(status, v) => Ok(HttpResponse {
                status,
                headers: vec![("content-type".into(), "application/json".into())],
                body: one(serde_json::to_vec(&v).unwrap()),
            }),
            Reply::Raw { status, content_type, body } => Ok(HttpResponse {
                status,
                headers: vec![("content-type".into(), content_type)],
                body: one(body),
            }),
            Reply::Stream { status, content_type, body } => {
                Ok(HttpResponse { status, headers: vec![("content-type".into(), content_type)], body })
            }
            Reply::Fail(e) => Err(e),
        }
    }
}

/// `google(routes, app)`: a client signed in as "tok" over the mock, and the mock to read the requests back.
pub fn google<F, R>(app: &str, routes: F) -> (GoogleRestClient, Arc<Mock>)
where
    F: Fn(&Req) -> R + Send + Sync + 'static,
    R: Into<Reply>,
{
    let mock = Arc::new(Mock { route: Box::new(move |r| routes(r).into()), seen: Mutex::new(vec![]) });
    let client = GoogleRestClient::with_transport(app, || Some("tok".to_string()), mock.clone()).unwrap();
    (client, mock)
}

pub fn b64(text: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(text.as_bytes())
}

/// The email a send request carries, read back from its base64url raw: (unfolded headers, body).
pub fn sent(req: &Req) -> (Vec<(String, String)>, String) {
    let raw = req.json()["raw"].as_str().unwrap().to_string();
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(raw).unwrap();
    let text = String::from_utf8(bytes).unwrap();
    let (head, body) = text.split_once("\r\n\r\n").unwrap();
    let mut headers: Vec<(String, String)> = Vec::new();
    for line in head.split("\r\n") {
        if line.starts_with([' ', '\t']) {
            headers.last_mut().unwrap().1.push_str(line);
        } else {
            let (k, v) = line.split_once(':').unwrap();
            headers.push((k.to_string(), v.trim_start().to_string()));
        }
    }
    (headers, body.to_string())
}

pub fn hget(headers: &[(String, String)], k: &str) -> Option<String> {
    headers.iter().find(|(key, _)| key.eq_ignore_ascii_case(k)).map(|(_, v)| v.clone())
}
