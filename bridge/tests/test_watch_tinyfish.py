"""The hosted reader (watch.fetch_hosted, TinyFish Fetch) and its rung on the watcher's ladder:
"" -> tls -> browser -> hosted -> search.

fetch_hosted() is tested against a fake http_request, so nothing is sent: the request it sends, and which failures
are the page's (they move a watch down the ladder) and which are TinyFish's (they never do). The watcher tests
lend fakes, as test_watch does.
"""

from __future__ import annotations

import asyncio
import json
from pathlib import Path
from typing import Any

import pytest
from test_watch import JSON_LD_EVENT, args, make_watcher

from eve_bridge import spend, tinyfish, watch
from eve_bridge.watch import FetchError

URL = "https://shop.example/p"


@pytest.fixture
def sent(monkeypatch: pytest.MonkeyPatch) -> list[dict[str, Any]]:
    """fetch_hosted()'s requests, answered by ``sent.reply`` (a payload, or a FetchError to raise)."""
    calls: list[dict[str, Any]] = []
    monkeypatch.setenv("TINYFISH_API_KEY", "tf-test")
    monkeypatch.setattr(watch, "check_url", lambda url, **k: "")

    def fake(url: str, *, data: bytes = b"", headers: Any = None, timeout: float = 0) -> tuple[int, str]:
        calls.append({"url": url, "body": json.loads(data), "headers": headers, "timeout": timeout})
        reply = fake.reply
        if isinstance(reply, Exception):
            raise reply
        return 200, json.dumps(reply)

    fake.reply = {"results": [{"url": URL, "final_url": URL, "text": "<main>$85</main>", "format": "html"}],
                  "errors": []}
    monkeypatch.setattr(watch, "http_request", fake)
    calls.append({"fake": fake})
    return calls


def _reply(sent: list[dict[str, Any]], reply: Any) -> None:
    sent[0]["fake"].reply = reply


def _failed(error: str, status: int = 0) -> dict[str, Any]:
    return {"results": [], "errors": [{"url": URL, "error": error, **({"status": status} if status else {})}]}


def test_it_asks_for_a_fresh_page(sent: list[dict[str, Any]]) -> None:
    status, html = watch.fetch_hosted(URL)
    assert status == 200 and html == "<main>$85</main>"
    call = sent[1]
    assert call["url"] == tinyfish.FETCH_URL and call["headers"]["X-API-Key"] == "tf-test"
    body = call["body"]
    # ttl 0: a live read, never TinyFish's cache; a watch is about now
    assert body["urls"] == [URL] and body["ttl"] == 0 and body["format"] == "html"
    assert body["per_url_timeout_ms"] < call["timeout"] * 1000                    # TinyFish gives up first


def test_the_pages_own_refusal_moves_the_ladder_tinyfishs_never_does(sent: list[dict[str, Any]]) -> None:
    for reply, status in ((_failed("target_http_error", 403), 403), (_failed("bot_blocked"), 403),
                          (_failed("login_required"), 401), (_failed("page_not_found", 404), 404)):
        _reply(sent, reply)
        with pytest.raises(FetchError) as e:
            watch.fetch_hosted(URL)
        assert e.value.status == status and not e.value.host           # the site's: check() moves the watch on
    for code in ("timeout", "proxy_error", "empty_content"):
        _reply(sent, _failed(code))
        with pytest.raises(FetchError, match=code) as e:
            watch.fetch_hosted(URL)
        assert e.value.host == tinyfish.FETCH_HOST and e.value.status == 0
    for status in (401, 402, 429):
        _reply(sent, FetchError(f"the site answered HTTP {status}", status, 30.0 if status == 429 else None))
        with pytest.raises(FetchError) as e:
            watch.fetch_hosted(URL)
        assert e.value.host == tinyfish.FETCH_HOST and e.value.status == status
    assert e.value.retry_after == 30.0                                  # the limiter holds TinyFish, not the shop
    _reply(sent, {"results": [{"url": URL, "text": "  "}], "errors": []})
    with pytest.raises(FetchError, match="empty page") as e:
        watch.fetch_hosted(URL)
    assert e.value.host == tinyfish.FETCH_HOST


def test_a_redirected_or_renamed_single_result_still_counts(sent: list[dict[str, Any]]) -> None:
    _reply(sent, {"results": [{"url": URL + "/", "text": "<p>ok</p>"}], "errors": []})
    assert watch.fetch_hosted(URL) == (200, "<p>ok</p>")


def test_no_key_no_request(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("TINYFISH_API_KEY", raising=False)
    monkeypatch.setattr(watch, "check_url", lambda url, **k: "")
    monkeypatch.setattr(watch, "http_request", lambda *a, **k: pytest.fail("no key, no request"))
    with pytest.raises(FetchError, match="TINYFISH_API_KEY") as e:
        watch.fetch_hosted(URL)
    assert e.value.host == tinyfish.FETCH_HOST


def test_a_private_link_is_never_sent_to_tinyfish(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("TINYFISH_API_KEY", "tf-test")
    monkeypatch.setattr(watch, "http_request", lambda *a, **k: pytest.fail("a private link is not sent"))
    with pytest.raises(FetchError, match="private or unknown"):
        watch.fetch_hosted("http://192.168.1.1/admin")


def test_the_switches() -> None:
    assert watch.configured({}).hosted is False
    on = watch.configured({"TINYFISH_API_KEY": "tf-x"})
    assert on.hosted and on.hosted_calls_per_day == watch.DEFAULT_HOSTED_CALLS_PER_DAY
    assert watch.configured({"TINYFISH_API_KEY": "tf-x", "EVE_WATCH_HOSTED": "0"}).hosted is False
    assert watch.configured({"EVE_WATCH_HOSTED_CALLS": "5"}).hosted_calls_per_day == 5


# ---- the watcher --------------------------------------------------------------------------------

def refused(status: int = 403) -> Any:
    def fetch(url: str, **kw: Any) -> tuple[int, str]:
        raise FetchError(f"the site answered HTTP {status}", status)
    return fetch


def crawler(pages: list[str], html: str = JSON_LD_EVENT) -> Any:
    return lambda url, **kw: pages.append(url) or (200, html)


@pytest.fixture
def spent(monkeypatch: pytest.MonkeyPatch) -> list[tuple[Any, ...]]:
    rows: list[tuple[Any, ...]] = []
    monkeypatch.setattr(spend, "record", lambda *a, **k: rows.append((a, k)))
    return rows


def test_a_page_every_local_reader_is_refused_is_read_through_tinyfish(tmp_path: Path,
                                                                       spent: list[Any]) -> None:
    crawled: list[str] = []
    w = make_watcher(tmp_path / "w.json", fetch=refused(403), tls=True, browser=True, hosted=True)
    w._tls = refused(403)
    w._render = lambda url, **kw: (_ for _ in ()).throw(FetchError("the page shows a bot check", 403))
    w._crawl = crawler(crawled)
    out = asyncio.run(w.add(args(target="https://shop.example/p", condition="below", value=100)))
    item = w.watches[0]
    assert out["ok"] and "85" in out["now"] and "TinyFish" in out["note"]
    assert item.via == "hosted" and item.every_secs >= watch.FLOOR_SECS["hosted"]
    assert crawled == ["https://shop.example/p"]
    assert spent and spent[0][0][:4] == ("tinyfish", "fetch", spend.WATCH, 0.0)
    # from now on straight to TinyFish; nothing local is tried
    w._fetch = w._tls = lambda url, **kw: pytest.fail("a hosted page is not read locally")
    w._render = lambda url, **kw: pytest.fail("a hosted page is not rendered")
    asyncio.run(w.read(item))
    assert len(crawled) == 2
    assert "(through TinyFish)" in w.listing()


def test_without_the_browser_tinyfish_comes_straight_after_the_chrome_like_read(tmp_path: Path,
                                                                                spent: list[Any]) -> None:
    crawled: list[str] = []
    w = make_watcher(tmp_path / "w.json", fetch=refused(403), tls=True, browser=False, hosted=True)
    w._tls = refused(403)
    w._crawl = crawler(crawled)
    assert asyncio.run(w.add(args(condition="below", value=100)))["ok"]
    assert w.watches[0].via == "hosted" and len(crawled) == 1


def test_a_page_tinyfish_is_refused_too_goes_to_search(tmp_path: Path, spent: list[Any]) -> None:
    w = make_watcher(tmp_path / "w.json", fetch=refused(403), hosted=True)
    w._crawl = refused(403)
    asyncio.run(w.add(args(condition="below", value=100)))
    assert w.watches[0].via == "search"


def test_tinyfishs_own_trouble_never_moves_the_watch(tmp_path: Path, spent: list[Any]) -> None:
    w = make_watcher(tmp_path / "w.json", fetch=refused(403), hosted=True)

    def rate_limited(url: str, **kw: Any) -> tuple[int, str]:
        raise FetchError("TinyFish: the site answered HTTP 429", 429, host=tinyfish.FETCH_HOST)

    w._crawl = rate_limited
    asyncio.run(w.add(args(condition="below", value=100)))
    item = w.watches[0]
    assert item.via == "hosted" and item.errors == 1 and "429" in item.last_error


def test_the_daily_cap(tmp_path: Path, spent: list[Any]) -> None:
    crawled: list[str] = []
    w = make_watcher(tmp_path / "w.json", fetch=refused(403), hosted=True, hosted_calls_per_day=2)
    w._crawl = crawler(crawled)
    asyncio.run(w.add(args(condition="below", value=100)))
    item = w.watches[0]
    asyncio.run(w.read(item))
    with pytest.raises(FetchError, match="TinyFish reads are used up") as e:
        asyncio.run(w.read(item))
    assert e.value.host == tinyfish.FETCH_HOST and len(crawled) == 2
    w._clock.t += 86400                                                  # the next day
    asyncio.run(w.read(item))
    assert len(crawled) == 3


def test_off_without_a_key(tmp_path: Path) -> None:
    w = make_watcher(tmp_path / "w.json", fetch=refused(403), hosted=False)
    w._crawl = lambda url, **kw: pytest.fail("TinyFish is off")
    asyncio.run(w.add(args(condition="below", value=100)))
    assert w.watches[0].via == "search"
    assert watch.Watcher(watch.WatchConfig(enabled=True, path=tmp_path / "x.json", hosted=True))._crawl is None


def test_a_hosted_page_moves_to_search_when_tinyfish_is_turned_off(tmp_path: Path, spent: list[Any]) -> None:
    w = make_watcher(tmp_path / "w.json", fetch=refused(403), hosted=True)
    w._crawl = crawler([])
    asyncio.run(w.add(args(condition="below", value=100)))
    assert w.watches[0].via == "hosted"
    searched: list[Any] = []
    later = make_watcher(tmp_path / "w.json", hosted=False)             # the key removed, the daemon restarted
    later._crawl = lambda url, **kw: pytest.fail("TinyFish is off")
    item = later.watches[0]

    async def search(w: Any) -> Any:
        searched.append(w.id)
        return watch.Reading(value=80.0, currency="USD", source="search")

    later._read_search = search
    assert asyncio.run(later.read(item)).value == 80.0
    assert item.via == "search" and searched == [item.id] and item.every_secs >= watch.FLOOR_SECS["search"]


def test_the_via_survives_a_restart(tmp_path: Path, spent: list[Any]) -> None:
    w = make_watcher(tmp_path / "w.json", fetch=refused(403), hosted=True)
    w._crawl = crawler([])
    asyncio.run(w.add(args(condition="below", value=100)))
    again = make_watcher(tmp_path / "w.json", hosted=True)
    assert again.watches[0].via == "hosted" and again.watches[0].every_secs >= watch.FLOOR_SECS["hosted"]
