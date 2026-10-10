"""TinyFish: eve's web search and hosted page reads, free on every plan (docs.tinyfish.ai, 2026-09-29).

Two calls, both with ``X-API-Key: $TINYFISH_API_KEY``, both through watch.http_request so its guards hold:

* Search (``GET api.search.tinyfish.ai?query=``): ranked results as title, snippet and link; no page text. Free
  at any wallet balance; 30 requests a minute per key (HTTP 429 past it).
* Fetch (``POST api.fetch.tinyfish.ai``): up to 10 pages read on TinyFish's side as markdown or cleaned HTML,
  each with its own error code when it fails; ``ttl`` 0 is a live read, a positive ``ttl`` accepts a cached copy
  that young.

Used by the web reader (web_reader.py: search, then read the top pages), eve's routed web search
(search_router.py sends a one-fact or one-page lookup here, the brain's and the web reader's alike), the watcher's
hosted reader for sites that refuse this Mac (watch.py), and the tutor's practice references (learning/search.py). Every TinyFish
refusal (a bad key, its rate limit, its outage) carries ``host``, so the watcher never mistakes it for the watched
site refusing; a page's own refusal, which Fetch reports per URL, does not.
"""

from __future__ import annotations

import json
import os
import urllib.parse
from typing import Any, Mapping, Optional

SEARCH_URL = "https://api.search.tinyfish.ai"
FETCH_URL = "https://api.fetch.tinyfish.ai"
SEARCH_HOST = "api.search.tinyfish.ai"
FETCH_HOST = "api.fetch.tinyfish.ai"
MAX_URLS = 10                        # Fetch's limit per request

# Fetch's per-URL error codes that are the page's own answer rather than TinyFish failing. As an HTTP status, so
# the watcher's ladder reads them as it read a site's 401/403/404 before.
PAGE_REFUSALS = {"bot_blocked": 403, "login_required": 401, "page_not_found": 404}


def api_key(environ: Optional[Mapping[str, str]] = None) -> str:
    env = os.environ if environ is None else environ
    return (env.get("TINYFISH_API_KEY") or "").strip()


def _call(url: str, host: str, *, data: Optional[bytes], timeout: float) -> dict[str, Any]:
    """One TinyFish request: its JSON object. Raises watch.FetchError carrying ``host``."""
    from . import watch

    key = api_key()
    if not key:
        raise watch.FetchError("TinyFish is not set up on this computer (TINYFISH_API_KEY)", host=host)
    headers = {"X-API-Key": key, "Accept": "application/json"}
    if data is not None:
        headers["Content-Type"] = "application/json"
    try:
        _, text = watch.http_request(url, data=data, timeout=timeout, headers=headers)
    except watch.FetchError as e:
        raise watch.FetchError(f"TinyFish: {e.reason}", e.status, e.retry_after, host=host) from None
    try:
        payload = json.loads(text)
    except ValueError:
        raise watch.FetchError("TinyFish sent an unreadable answer", host=host) from None
    if not isinstance(payload, dict):
        raise watch.FetchError("TinyFish sent an unreadable answer", host=host)
    return payload


def search(query: str, *, limit: int = 10, timeout: float = 15.0) -> list[dict[str, str]]:
    """Ranked results: [{"url", "title", "snippet"}], at most ``limit``. Raises watch.FetchError."""
    q = " ".join(query.split())[:500]
    payload = _call(f"{SEARCH_URL}?{urllib.parse.urlencode({'query': q})}", SEARCH_HOST, data=None, timeout=timeout)
    out: list[dict[str, str]] = []
    for item in payload.get("results") or []:
        url = item.get("url") if isinstance(item, dict) else None
        if not isinstance(url, str) or urllib.parse.urlsplit(url).scheme not in ("http", "https"):
            continue                                 # a result is only ever a web page to read
        out.append({"url": item["url"], "title": str(item.get("title") or "")[:200],
                    "snippet": str(item.get("snippet") or "")})
        if len(out) >= limit:
            break
    return out


def fetch(urls: list[str], *, fmt: str = "markdown", ttl: Optional[int] = None,
          timeout: float = 30.0) -> tuple[dict[str, dict[str, Any]], dict[str, dict[str, Any]]]:
    """Read ``urls`` (at most MAX_URLS) on TinyFish's side: ({url: result}, {url: error}), keyed by the URL as
    asked. A result has ``text`` (markdown or cleaned HTML, never raw: scripts are stripped) and ``title``; an
    error has ``error`` (TinyFish's code) and, for an HTTP error, ``status``. Raises watch.FetchError when the
    whole request fails."""
    body: dict[str, Any] = {"urls": urls[:MAX_URLS], "format": fmt,
                            "per_url_timeout_ms": int(max(1.0, min(110.0, timeout - 5)) * 1000)}
    if ttl is not None:
        body["ttl"] = ttl
    payload = _call(FETCH_URL, FETCH_HOST, data=json.dumps(body).encode("utf-8"), timeout=timeout)
    results = {str(r["url"]): r for r in payload.get("results") or [] if isinstance(r, dict) and r.get("url")}
    errors = {str(r["url"]): r for r in payload.get("errors") or [] if isinstance(r, dict) and r.get("url")}
    return results, errors
