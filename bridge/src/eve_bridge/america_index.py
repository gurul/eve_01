"""One Alexandria government-index query and bounded reads of its official sources.

The index returns evidence for Eve's existing answer model, never an answer or
a law's effective date. This module does not discover tools, retry, accept terms,
or switch providers. ``firecrawl.run_tool`` owns the shared paid-call cap.
"""

from __future__ import annotations

import math
import re
import time
from datetime import datetime, timezone
from typing import Any, Callable
from urllib.parse import parse_qsl, urlencode, urlsplit, urlunsplit

from . import firecrawl, watch

MAX_RESULTS = 5
MAX_SOURCES = 3
MAX_EXCERPT = 2000
MAX_PAGE_TEXT = 6000
MAX_QUERY_SECS = 55.0
MAX_READ_SECS = 8.0
_HOST = re.compile(r"[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\Z")
_BAD_PERCENT = re.compile(r"%(?![0-9a-fA-F]{2})")
_PATH_SESSION = re.compile(r"(?:;|%3b)(?:jsessionid|phpsessid|sessionid)(?:=|%3d)[^/;?#]*", re.I)
_UNRESERVED = frozenset("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-._~")
_SESSION_KEYS = frozenset({
    "jsessionid", "phpsessid", "session", "sessionid", "session_id", "sessiontoken", "session_token",
    "access_token", "auth_token", "cfid", "cftoken",
})
_TRACKING_KEYS = frozenset({"gclid", "dclid", "fbclid", "msclkid", "mc_cid", "mc_eid", "_ga", "_gl"})


class IndexFailure(RuntimeError):
    """A failed lookup, with static prose safe to show the owner."""


def canonical_url(value: Any) -> str | None:
    """An official HTTPS citation, without sessions, tracking or fragments.

    This is a syntax/domain check. The production reader separately checks and
    pins public DNS addresses before every connection, including redirects.
    Meaningful query parameters and the order of repeated parameters survive.
    """
    if (not isinstance(value, str) or not 1 <= len(value) <= 2000
            or any(c.isspace() or ord(c) < 32 or ord(c) == 127 for c in value)
            or "\\" in value or _BAD_PERCENT.search(value)):
        return None
    try:
        parts = urlsplit(value)
        host = (parts.hostname or "").lower().removesuffix(".")
        if (parts.scheme != "https" or parts.username is not None or parts.password is not None
                or parts.port not in (None, 443) or len(host) > 253
                or not host.endswith((".gov", ".mil"))
                or any(not _HOST.fullmatch(label) for label in host.split("."))):
            return None
        path = _PATH_SESSION.sub("", parts.path or "/")

        def percent(match: re.Match[str]) -> str:
            character = chr(int(match.group()[1:], 16))
            if ord(character) < 32 or ord(character) == 127 or character == "\\":
                raise ValueError("Invalid path.")
            return character if character in _UNRESERVED else match.group().upper()

        path = re.sub(r"%[0-9a-fA-F]{2}", percent, path)
        query = []
        for key, item in parse_qsl(parts.query, keep_blank_values=True, max_num_fields=100):
            if any(ord(c) < 32 or ord(c) == 127 or c == "\\" for c in key + item):
                return None
            lowered = key.lower()
            if lowered.startswith(("utm_", "aspsessionid")) or lowered in _SESSION_KEYS | _TRACKING_KEYS:
                continue
            query.append((key, item))
        canonical = urlunsplit(("https", host, path, urlencode(sorted(query, key=lambda pair: pair[0])), ""))
        return canonical if len(canonical) <= 2000 else None
    except (ValueError, UnicodeError):
        return None


def official_guard(url: str) -> str:
    """watch.http_request's additional guard, applied to initial URLs and redirects."""
    return "" if canonical_url(url) is not None else "Only official HTTPS government source links may be read."


def _fetch(url: str, *, timeout: float) -> tuple[int, str]:
    # No provider headers or cookies are passed to a source host.
    return watch.http_request(url, timeout=timeout, url_guard=official_guard)


def _utc_now() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="milliseconds").replace("+00:00", "Z")


def _indexed_at(value: Any) -> str | None:
    """Observed updated_at values are Unix milliseconds, not seconds/effective dates."""
    if not isinstance(value, (int, float)) or isinstance(value, bool):
        return None
    try:
        if not math.isfinite(value) or value < 0:
            return None
        return datetime.fromtimestamp(value / 1000, timezone.utc).isoformat(
            timespec="milliseconds").replace("+00:00", "Z")
    except (OverflowError, OSError, ValueError):
        return None


def _finite_number(value: Any) -> bool:
    try:
        return isinstance(value, (int, float)) and not isinstance(value, bool) and math.isfinite(value)
    except OverflowError:
        return False


def _text(value: Any, limit: int) -> tuple[str, bool]:
    if not isinstance(value, str):
        return "", False
    # Avoid parsing unlimited whitespace supplied by a malformed response.
    raw_limit = limit * 4
    text = " ".join(value[:raw_limit].split())
    return text[:limit], len(value) > raw_limit or len(text) > limit


def _readable_body(body: Any) -> bool:
    if not isinstance(body, str) or len(body) > watch.MAX_BYTES:
        return False
    prefix = body[:256].lstrip("\ufeff \t\r\n")
    # http_request decodes bytes but does not expose MIME. Binary documents are
    # not fresh page text; in particular, main_text is not a PDF text extractor.
    if prefix.startswith(("%PDF-", "\x89PNG", "\ufffdPNG", "GIF87a", "GIF89a", "PK\x03\x04")):
        return False
    return not any(ord(c) < 32 and c not in "\t\r\n" for c in prefix)


class GovernmentIndex:
    """Synchronous source lookup; America runs this off its event loop.

    Injected runners/readers implement the same finite-timeout contracts as the
    production dependencies. They receive only the query or canonical source URL.
    """

    def __init__(self, *, runner: Callable[..., dict[str, Any]] | None = None,
                 fetch: Callable[..., tuple[int, str]] | None = None, timeout_secs: float = MAX_QUERY_SECS):
        if not _finite_number(timeout_secs) or timeout_secs <= 0:
            raise ValueError("Government index timeout must be finite and positive.")
        self._runner = runner if runner is not None else firecrawl.run_tool
        self._fetch = fetch if fetch is not None else _fetch
        self._timeout_secs = min(float(timeout_secs), MAX_QUERY_SECS)

    def query(self, question: str, jurisdiction: str) -> dict[str, Any]:
        if (not isinstance(question, str) or not question.strip() or len(question) > 4000
                or not isinstance(jurisdiction, str) or not jurisdiction.strip() or len(jurisdiction) > 100):
            raise IndexFailure("A question and U.S. jurisdiction are required for government source research.")
        question, jurisdiction = question.strip(), jurisdiction.strip()
        deadline = time.monotonic() + self._timeout_secs
        remaining = deadline - time.monotonic()
        # run_tool reserves five seconds inside its upstream timeout field.
        if remaining < 6:
            raise IndexFailure("Government index lookup has no time left to start.")
        request = {"provider": "firecrawl-gov-index", "capability": "search", "options": {
            "query": f"Jurisdiction: {jurisdiction}\nQuestion: {question}",
            "top_k": MAX_RESULTS, "full_content": False,
        }}
        try:
            result = self._runner(request, timeout=min(remaining, firecrawl.TOOL_TIMEOUT_SECS))
        except Exception:
            # Provider bodies can contain the question, keys and account details.
            raise IndexFailure("Government index is unavailable. No alternate provider was called.") from None
        if not isinstance(result, dict):
            raise IndexFailure("Government index returned an unreadable source result.")
        credits = result.get("credits")
        if not _finite_number(credits) or credits < 0:
            credits = None
        try:
            firecrawl.meter("alexandria firecrawl-gov-index", credits, "america", firecrawl.usd_per_credit())
        except Exception:
            raise IndexFailure("Government index usage could not be recorded.") from None
        data = result.get("data")
        rows = data.get("data") if isinstance(data, dict) else None
        if not isinstance(rows, list):
            raise IndexFailure("Government index returned an unreadable source result.")
        selected: dict[str, dict[str, Any]] = {}
        for row in rows[:MAX_RESULTS]:
            if not isinstance(row, dict) or (url := canonical_url(row.get("url"))) is None:
                continue
            if url in selected:
                # Retain the first ranked record's excerpt and matching metadata.
                continue
            if len(selected) == MAX_SOURCES:
                continue
            title, _ = _text(row.get("title"), 500)
            excerpt, truncated = _text(row.get("snippet"), MAX_EXCERPT)
            selected[url] = {
                "title": title or urlsplit(url).hostname, "url": url, "currency": "unverified",
                "excerpt": excerpt, "page_text": "", "read_status": "indexed_only",
                "text_truncated": truncated, "indexed_at": _indexed_at(row.get("updated_at")), "fetched_at": None,
            }
        sources = []
        for source in selected.values():
            remaining = deadline - time.monotonic()
            if remaining > 0:
                try:
                    status, body = self._fetch(source["url"], timeout=min(remaining, MAX_READ_SECS))
                    if (not isinstance(status, int) or isinstance(status, bool) or not 200 <= status < 300
                            or not _readable_body(body)):
                        raise ValueError("Unreadable source.")
                    text, truncated = _text(watch.main_text(body), MAX_PAGE_TEXT)
                    if text:
                        source.update(page_text=text, read_status="live", fetched_at=_utc_now(),
                                      text_truncated=source["text_truncated"] or truncated)
                except Exception:
                    # A failed read is not a fresh answer, and never triggers a retry.
                    pass
            if source["excerpt"] or source["page_text"]:
                sources.append(source)
        if not sources:
            raise IndexFailure("Government index found no usable official sources for this question.")
        return {"ok": True, "status": "sources_found", "provider": "firecrawl_gov_index",
                "question": question, "jurisdiction": jurisdiction, "retrieved_at": _utc_now(),
                "legal_advice": False, "sources": sources}
