//! watch.py's `fetch_hosted`: the page as TinyFish Fetch read it on its side.

use super::FetchError;
use crate::Ctx;

/// `fetch_hosted`'s default timeout.
pub const HOSTED_TIMEOUT_SECS: f64 = 70.0;

/// The page as TinyFish Fetch read it on its side (its own network): (200, cleaned HTML). For pages that refuse
/// every local reader. Fresh every time (`ttl` 0): a watch is about now. The HTML has its scripts stripped, so
/// structured data in a script tag is gone and the page's text answers instead. TinyFish's own refusals (a bad
/// key, its rate limit) carry `host` and so never move a watch down the ladder, while the page's own refusal,
/// which Fetch reports per URL (tinyfish::PAGE_REFUSALS, or an HTTP error with its status), does. `resolve` is
/// check_url's (None: `_public`). Blocking.
pub fn fetch_hosted(ctx: &Ctx, url: &str, timeout: f64, resolve: Option<&dyn Fn(&str) -> bool>) -> Result<(u16, String), FetchError> {
    let _ = (ctx, url, timeout, resolve);
    todo!("watch.fetch_hosted")
}
