//! watch.py's `fetch_hosted`: the page as TinyFish Fetch read it on its side.

use serde_json::{Map, Value};

use super::net::check_url;
use super::page::{py_str, py_strip, truthy};
use super::FetchError;
use crate::tinyfish;
use crate::Ctx;

/// `fetch_hosted`'s default timeout.
pub const HOSTED_TIMEOUT_SECS: f64 = 70.0;

/// `int(x or 0)`, 0 when Python's int() would raise (TypeError, ValueError).
fn py_int(v: Option<&Value>) -> i64 {
    match v {
        Some(v) if truthy(v) => match v {
            Value::Bool(b) => i64::from(*b),
            Value::Number(n) => n.as_i64().or_else(|| n.as_f64().filter(|x| x.is_finite()).map(|x| x.trunc() as i64)).unwrap_or(0),
            Value::String(s) => py_strip(s).replace('_', "").parse::<i64>().unwrap_or(0),
            _ => 0,
        },
        _ => 0,
    }
}

/// The entry for `url`, or the only entry when TinyFish keyed it otherwise (a redirect, a trailing slash).
fn entry_for<'a>(found: &'a indexmap::IndexMap<String, Map<String, Value>>, url: &str) -> Option<&'a Map<String, Value>> {
    found.get(url).or_else(|| if found.len() == 1 { found.values().next() } else { None })
}

/// The page as TinyFish Fetch read it on its side (its own network): (200, cleaned HTML). For pages that refuse
/// every local reader. Fresh every time (`ttl` 0): a watch is about now. The HTML has its scripts stripped, so
/// structured data in a script tag is gone and the page's text answers instead. TinyFish's own refusals (a bad
/// key, its rate limit) carry `host` and so never move a watch down the ladder, while the page's own refusal,
/// which Fetch reports per URL (tinyfish::PAGE_REFUSALS, or an HTTP error with its status), does. `resolve` is
/// check_url's (None: `_public`). Blocking.
pub fn fetch_hosted(ctx: &Ctx, url: &str, timeout: f64, resolve: Option<&dyn Fn(&str) -> bool>) -> Result<(u16, String), FetchError> {
    let why = check_url(url, resolve);
    if !why.is_empty() {
        return Err(FetchError::new(why));
    }
    let (results, errors) = tinyfish::fetch(ctx, &[url.to_string()], "html", Some(0), timeout)?;
    let Some(page) = entry_for(&results, url) else {
        let none = Map::new();
        let err = entry_for(&errors, url).unwrap_or(&none);
        let code = match err.get("error") {
            Some(v) if truthy(v) => py_str(v),
            _ => String::new(),
        };
        let mut status = py_int(err.get("status"));
        if status == 0 {
            status = i64::from(tinyfish::page_refusal(&code).unwrap_or(0));
        }
        if status != 0 && !(200..300).contains(&status) {
            return Err(FetchError::with_status(format!("the site answered HTTP {status}"), u16::try_from(status).unwrap_or(0)));
        }
        let what = if code.is_empty() { "no answer".to_string() } else { code };
        return Err(FetchError::with_host(format!("TinyFish could not read the page ({what})"), tinyfish::FETCH_HOST));
    };
    match page.get("text") {
        Some(Value::String(html)) if !py_strip(html).is_empty() => Ok((200, html.clone())),
        _ => Err(FetchError::with_host("TinyFish returned an empty page", tinyfish::FETCH_HOST)),
    }
}
