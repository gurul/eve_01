"""The watcher (watch.py): the rate limiter, reading pages, edge-triggered conditions, the store and the loop.
No network: every request goes to a fake lent in its place, and the clock is a dict the test moves."""

from __future__ import annotations

import asyncio
import json
import random
from pathlib import Path
from typing import Any, Optional

import pytest

from eve_bridge import watch
from eve_bridge.watch import FetchError, RateLimiter, Reading, Watch, WatchConfig, Watcher


class Clock:
    def __init__(self, t: float = 1_800_000_000.0) -> None:
        self.t = t

    def __call__(self) -> float:
        return self.t


def limiter(clock: Clock, **kw: Any) -> RateLimiter:
    opts: dict[str, Any] = dict(rate_per_min=6, burst=2, host_gap_secs=20, backoff_base=60, backoff_max=600,
                                model_calls_per_day=2, clock=clock, day=lambda t: str(int(t // 86400)))
    opts.update(kw)
    return RateLimiter(**opts)


# ---- G3: the rate limiter ---------------------------------------------------------------------

def test_the_bucket_runs_dry_and_refills() -> None:
    clock = Clock()
    lim = limiter(clock, host_gap_secs=0)
    assert lim.ready_in("a") == 0
    lim.take("a")
    assert lim.ready_in("b") == 0
    lim.take("b")
    # two tokens spent at 6/min: the next one is 10 s away, for any host
    assert lim.ready_in("c") == pytest.approx(10.0)
    clock.t += 5
    assert lim.ready_in("c") == pytest.approx(5.0)
    clock.t += 5
    assert lim.ready_in("c") == 0
    # the bucket never holds more than its burst, however long it was quiet
    clock.t += 3600
    for host in ("x", "y"):
        assert lim.ready_in(host) == 0
        lim.take(host)
    assert lim.ready_in("z") > 0


def test_one_host_waits_out_its_gap_another_goes_at_once() -> None:
    clock = Clock()
    lim = limiter(clock, burst=10, rate_per_min=600)
    lim.take("shop.example")
    assert lim.ready_in("shop.example") == pytest.approx(20.0)
    assert lim.ready_in("other.example") == 0
    clock.t += 20
    assert lim.ready_in("shop.example") == 0


def test_retry_after_holds_the_host_and_backoff_doubles_capped() -> None:
    clock = Clock()
    lim = limiter(clock, burst=10, rate_per_min=600, host_gap_secs=0)
    # Retry-After longer than the base backoff wins
    assert lim.penalize("h", retry_after=120) == 120
    assert lim.ready_in("h") == pytest.approx(120)
    assert lim.ready_in("elsewhere") == 0
    # without a Retry-After, the backoff doubles per strike: 60, 120 (strike 2), 240, 480, then the 600 cap
    assert lim.penalize("h") == 120
    assert lim.penalize("h") == 240
    assert lim.penalize("h") == 480
    assert lim.penalize("h") == 600
    assert lim.penalize("h") == 600
    # a Retry-After past the cap is capped too
    assert lim.penalize("h", retry_after=99999) == 600
    # a success clears it: the next strike starts from the base again
    lim.clear("h")
    assert lim.ready_in("h") == 0
    assert lim.penalize("h") == 60


def test_the_daily_model_cap_refuses_and_resets() -> None:
    clock = Clock(86400 * 100 + 10)
    lim = limiter(clock)
    assert lim.model_ok()
    lim.model_used()
    assert lim.model_ok()
    lim.model_used()
    assert not lim.model_ok() and lim.model_calls_today == 2
    clock.t += 86400                         # the next day
    assert lim.model_ok() and lim.model_calls_today == 0


# ---- G4: reading a page -----------------------------------------------------------------------

JSON_LD_EVENT = """<html><head><title>Big Show - Tickets</title>
<script type="application/ld+json">{"@context":"https://schema.org","@type":"MusicEvent","name":"Big Show Seattle",
"offers":[{"@type":"Offer","price":"189.50","priceCurrency":"USD","availability":"https://schema.org/SoldOut"},
{"@type":"AggregateOffer","lowPrice":85,"highPrice":400,"priceCurrency":"USD","availability":"https://schema.org/InStock"}]}
</script></head><body><h1>Big Show</h1><script>var x = "Sold out";</script><p>Tickets on sale now</p></body></html>"""

META_PRODUCT = """<html><head><meta property="product:price:amount" content="1,299.00">
<meta property="product:price:currency" content="usd"><meta property="product:availability" content="out of stock">
<meta property="og:availability" content="oos"></head><body>Laptop</body></html>"""

ITEMPROP = """<html><body><span itemprop="price" content="42.10">$42.10</span></body></html>"""

PLAIN = """<html><head><title>Shop</title><style>.p{}</style></head><body><div>Now only $19.99 &amp; free shipping</div>
<script>window.price = 5</script></body></html>"""


def test_structured_prices_are_read_without_a_model() -> None:
    r = watch.structured(JSON_LD_EVENT)
    assert (r.value, r.currency, r.available) == (85.0, "USD", True)   # the lowest offer; one tier on sale
    assert r.name == "Big Show Seattle"
    r = watch.structured(META_PRODUCT)
    assert (r.value, r.currency) == (1299.0, "USD")
    r = watch.structured(ITEMPROP)
    assert r.value == 42.10 and r.currency == ""
    assert watch.structured(PLAIN).value is None                        # nothing stated: no guess
    # the positive control: a JSON-LD page read through the watcher makes zero model calls
    asks: list[Any] = []
    w = make_watcher(Path("/nonexistent/w.json"), fetch=lambda url, **kw: (200, JSON_LD_EVENT),
                     ask=lambda body: asks.append(body) or {})
    watch_ = Watch(id="w1", kind="page", target="https://tickets.example/show", label="Big Show",
                   condition="below", value=100, every_secs=600)
    r = asyncio.run(w.read(watch_))
    assert r.value == 85.0 and r.source == "structured" and asks == []


def model_reply(obj: Any, *, fence: bool = True) -> dict[str, Any]:
    text = json.dumps(obj)
    return {"choices": [{"message": {"content": f"```json\n{text}\n```" if fence else text}}],
            "usage": {"prompt_tokens": 900, "completion_tokens": 30, "cost": 0.0002}}


def test_a_page_without_structure_asks_the_model_once(tmp_path: Path) -> None:
    asks: list[dict[str, Any]] = []

    def ask(body: dict[str, Any]) -> dict[str, Any]:
        asks.append(body)
        return model_reply({"price": 19.99, "currency": "usd", "available": True,
                            "note": "In stock, ships free.", "url": "javascript:alert(1)"})

    w = make_watcher(tmp_path / "w.json", fetch=lambda url, **kw: (200, PLAIN), ask=ask)
    item = Watch(id="w1", kind="page", target="https://shop.example/p", label="Mug", condition="below",
                 value=15, every_secs=600)
    r = asyncio.run(w.read(item))
    assert len(asks) == 1 and r.value == 19.99 and r.currency == "USD" and r.available is True
    assert r.url == ""                                                   # not http(s): dropped
    user = asks[0]["messages"][1]["content"]
    assert "Now only $19.99 & free shipping" in user and "window.price" not in user and ".p{}" not in user
    assert w.limiter.model_calls_today == 1
    # the model's answer is checked, not passed on: a string price, a NaN, a non-bool availability are dropped
    bad = watch.model_reading(model_reply({"price": "cheap", "available": "yes", "currency": "dollars"}), "model")
    assert (bad.value, bad.available, bad.currency) == (None, None, "")
    with pytest.raises(FetchError):
        watch.model_reading({"choices": [{"message": {"content": "I think it is $5"}}]}, "model")
    # the same page text again: the last answer is reused, no second call
    again = asyncio.run(w.read(item))
    assert len(asks) == 1 and again.value == 19.99 and again.available is True
    # a changed page past the day's cap: the model is not called at all
    w.limiter.model_cap = 1
    w._fetch = lambda url, **kw: (200, PLAIN.replace("19.99", "17.49"))
    with pytest.raises(FetchError, match="used up"):
        asyncio.run(w.read(item))
    assert len(asks) == 1


def test_a_phrase_is_found_by_code(tmp_path: Path) -> None:
    asks: list[Any] = []
    w = make_watcher(tmp_path / "w.json", fetch=lambda url, **kw: (200, JSON_LD_EVENT),
                     ask=lambda body: asks.append(body) or {})
    item = Watch(id="w1", kind="page", target="https://tickets.example/show", label="Big Show",
                 condition="appears", text="tickets   ON SALE now", every_secs=600)
    assert asyncio.run(w.read(item)).hit is True
    # a phrase only inside a script is not on the page
    item.text = "sold out"
    assert asyncio.run(w.read(item)).hit is False
    assert asks == []


# ---- G5: conditions fire on the edge ----------------------------------------------------------

def run_readings(w: Watch, readings: list[Reading]) -> list[Optional[str]]:
    return [watch.evaluate(w, r) for r in readings]


def test_conditions_fire_on_the_edge_and_rearm() -> None:
    p = lambda v: Reading(value=v, currency="USD")  # noqa: E731
    below = Watch(id="w1", kind="quote", target="AAPL", label="AAPL", condition="below", value=300)
    alerts = run_readings(below, [p(310), p(305), p(299), p(290), p(301), p(298)])
    assert [a is not None for a in alerts] == [False, False, True, False, False, True]
    assert alerts[2] == "AAPL dropped to $299 (was $305), past your $300 mark."
    # already below at the first reading: nothing, until it goes back over and under again
    already = Watch(id="w2", kind="quote", target="AAPL", label="AAPL", condition="below", value=300)
    assert [a is not None for a in run_readings(already, [p(290), p(280), p(305), p(295)])] == [False, False, False, True]

    above = Watch(id="w3", kind="quote", target="BTC-USD", label="Bitcoin", condition="above", value=100000)
    assert [a is not None for a in run_readings(above, [p(90000), p(100500), p(101000)])] == [False, True, False]

    drop = Watch(id="w4", kind="quote", target="X", label="X", condition="drop_pct", value=10)
    alerts = run_readings(drop, [p(100), p(95), p(89), p(85), p(80)])
    assert [a is not None for a in alerts] == [False, False, True, False, True]
    assert alerts[2] == "X is down 11.0% to $89 (from $100)." and drop.baseline == 80
    rise = Watch(id="w5", kind="quote", target="X", label="X", condition="rise_pct", value=5)
    assert [a is not None for a in run_readings(rise, [p(100), p(104), p(106)])] == [False, False, True]

    change = Watch(id="w6", kind="page", target="https://t.example", label="Seats", condition="change")
    alerts = run_readings(change, [p(80), p(80), p(75), p(75)])
    assert [a is not None for a in alerts] == [False, False, True, False]
    assert alerts[2].startswith("Seats changed: $75, down from $80.") and alerts[2].endswith("\nhttps://t.example")

    a = lambda on: Reading(available=on)  # noqa: E731
    avail = Watch(id="w7", kind="search", target="tickets", label="Tour", condition="available")
    assert [x is not None for x in run_readings(avail, [a(False), a(None), a(True), a(True), a(False), a(True)])] == \
        [False, False, True, False, False, True]
    on_at_start = Watch(id="w8", kind="search", target="tickets", label="Tour", condition="available")
    assert run_readings(on_at_start, [a(True)]) == [None] and on_at_start.armed is False

    h = lambda on: Reading(hit=on)  # noqa: E731
    phrase = Watch(id="w9", kind="page", target="https://t.example", label="Show", condition="appears", text="on sale")
    assert [x is not None for x in run_readings(phrase, [h(False), h(True), h(True), h(False), h(True)])] == \
        [False, True, False, False, True]
    assert phrase.fired == 2


# ---- G6: the store and the guards -------------------------------------------------------------

def make_watcher(path: Path, *, fetch: Any = None, ask: Any = None, clock: Optional[Clock] = None,
                 notify: Any = None, **cfg: Any) -> Watcher:
    clock = clock or Clock()
    config = WatchConfig(enabled=True, path=path, **cfg)

    async def no_sleep(_s: float) -> None:
        return None

    return Watcher(config, notify=notify, fetch=fetch or (lambda url, **kw: (200, "")),
                   ask=ask or (lambda body: {}), clock=clock, sleep=no_sleep, rng=random.Random(0),
                   limiter=limiter(clock, burst=50, rate_per_min=600, host_gap_secs=0, model_calls_per_day=50),
                   offload=False, resolve=lambda host: host.endswith(".example"))


def quote_body(price: float, symbol: str = "AAPL") -> str:
    return json.dumps({"chart": {"result": [{"meta": {"symbol": symbol, "currency": "USD",
                                                       "regularMarketPrice": price, "shortName": "Apple Inc."}}]}})


def test_watches_survive_a_restart(tmp_path: Path) -> None:
    path = tmp_path / "sub" / "watches.json"
    w = make_watcher(path, fetch=lambda url, **kw: (200, quote_body(310)))
    out = asyncio.run(w.add({"kind": "quote", "target": "aapl", "label": "Apple", "condition": "below", "value": 300,
                             "text": None, "every_minutes": None}))
    assert out["ok"] and out["id"] == "w1" and out["now"] == "$310" and out["every"] == "15 min"
    assert "already" not in out
    assert path.stat().st_mode & 0o777 == 0o600
    assert not [p for p in path.parent.iterdir() if p.name.startswith(".watches-")]   # no temp file left behind
    again = make_watcher(path)
    assert [(x.id, x.target, x.condition, x.value, x.last_value, x.checks) for x in again.watches] == \
        [("w1", "aapl", "below", 300.0, 310.0, 1)]
    # an unreadable file is not a crash, and a bad entry is skipped
    path.write_text(json.dumps({"watches": [{"id": "w9", "kind": "nope"}, again.watches[0].to_json()]}))
    assert [x.id for x in make_watcher(path).watches] == ["w1"]
    path.write_text("{not json")
    assert make_watcher(path).watches == []
    # remove by id
    w = make_watcher(tmp_path / "b.json", fetch=lambda url, **kw: (200, quote_body(310)))
    asyncio.run(w.add({"kind": "quote", "target": "AAPL", "label": "Apple", "condition": "change", "value": None,
                       "text": None, "every_minutes": 1}))
    assert w.remove("W1") == {"ok": True, "removed": "Apple"} and make_watcher(tmp_path / "b.json").watches == []
    assert not w.remove("w1")["ok"]


def args(**kw: Any) -> dict[str, Any]:
    base = {"kind": "page", "target": "https://shop.example/p", "label": "Thing", "condition": "below",
            "value": 10, "text": None, "every_minutes": None}
    base.update(kw)
    return base


def test_guards_refuse_private_urls_and_raise_short_intervals(tmp_path: Path) -> None:
    public = lambda host: host.endswith(".example")  # noqa: E731
    assert watch.check_url("https://shop.example/p", resolve=public) == ""
    for bad in ("file:///etc/passwd", "ftp://shop.example/x", "https://user:pw@shop.example/", "javascript:alert(1)"):
        assert watch.check_url(bad, resolve=public) != "", bad
    # the real resolver: loopback, private, link-local (the cloud metadata address) and unknown names are refused
    for bad in ("http://127.0.0.1:8000/", "http://localhost/", "http://10.0.0.5/", "http://192.168.1.1/",
                "http://169.254.169.254/latest/meta-data/", "http://[::1]/", "http://no-such-host.invalid/"):
        assert watch.check_url(bad) != "", bad
    # the fetch itself refuses too, whoever calls it
    with pytest.raises(FetchError, match="private"):
        watch.http_request("http://127.0.0.1:1/")

    w = make_watcher(tmp_path / "w.json", fetch=lambda url, **kw: (200, JSON_LD_EVENT))
    assert not asyncio.run(w.add(args(target="http://192.168.1.1/admin")))["ok"]
    assert not asyncio.run(w.add(args(condition="below", value=None)))["ok"]
    assert not asyncio.run(w.add(args(condition="appears", text=None)))["ok"]
    assert not asyncio.run(w.add(args(kind="quote", target="AAPL", condition="available")))["ok"]
    assert not asyncio.run(w.add(args(kind="ticketmaster", target="x", condition="available")))["ok"]   # no key
    assert not asyncio.run(w.add(args(condition="drop_pct", value=100)))["ok"]
    # every_minutes below the kind's floor is raised to it, and above the ceiling lowered
    out = asyncio.run(w.add(args(every_minutes=0.5)))
    assert out["ok"] and w.watches[-1].every_secs == watch.FLOOR_SECS["page"]
    asyncio.run(w.add(args(every_minutes=10**9)))
    assert w.watches[-1].every_secs == watch.MAX_EVERY_SECS
    # the cap
    while len(w.watches) < watch.MAX_WATCHES:
        w.watches.append(Watch(id=f"x{len(w.watches)}", kind="quote", target="A", label="A", condition="change"))
    out = asyncio.run(w.add(args()))
    assert not out["ok"] and "remove one" in out["reason"]


# ---- G7: the scheduler ------------------------------------------------------------------------

def test_a_due_watch_fires_and_texts_once(tmp_path: Path) -> None:
    prices = iter([310, 305, 299, 295, 294])
    sent: list[str] = []
    clock = Clock()

    async def notify(text: str) -> None:
        sent.append(text)

    w = make_watcher(tmp_path / "w.json", fetch=lambda url, **kw: (200, quote_body(next(prices))),
                     clock=clock, notify=notify)
    out = asyncio.run(w.add({"kind": "quote", "target": "AAPL", "label": "Apple", "condition": "below",
                             "value": 300, "text": None, "every_minutes": 5}))
    assert out["now"] == "$310"

    async def ticks(n: int) -> None:
        for _ in range(n):
            clock.t += 400                   # past each next check (5 min ± 10%)
            await w.tick()

    asyncio.run(ticks(4))
    assert sent == ["Apple dropped to $299 (was $305), past your $300 mark."]
    assert w.watches[0].checks == 5 and w.watches[0].fired == 1
    assert json.loads((tmp_path / "w.json").read_text())["watches"][0]["fired"] == 1   # saved after each check
    # not due: a tick checks nothing
    before = w.watches[0].checks
    asyncio.run(w.tick())
    assert w.watches[0].checks == before


def test_a_blocked_page_falls_back_to_search(tmp_path: Path) -> None:
    fetched: list[str] = []
    asks: list[dict[str, Any]] = []

    def fetch(url: str, **kw: Any) -> tuple[int, str]:
        fetched.append(url)
        raise FetchError("the site answered HTTP 403", 403)

    def ask(body: dict[str, Any]) -> dict[str, Any]:
        asks.append(body)
        return model_reply({"available": False, "price": None, "currency": "", "url": "https://tix.example/e/1",
                            "note": "Presale starts Oct 3 (tix.example)."}, fence=False)

    w = make_watcher(tmp_path / "w.json", fetch=fetch, ask=ask)
    out = asyncio.run(w.add(args(target="https://tix.example/e/1", label="Big Show", condition="available",
                                 value=None)))
    assert out["ok"] and out["now"] == "not available" and "web search" in out["note"]
    assert out["detail"] == "Presale starts Oct 3 (tix.example)."
    item = w.watches[0]
    assert item.via == "search" and item.every_secs >= watch.FLOOR_SECS["search"] and w.host(item) == watch.OPENROUTER_HOST
    assert len(fetched) == 1 and len(asks) == 1
    tools = asks[0]["tools"]
    assert tools and tools[0]["type"] == "openrouter:web_search"
    assert "Big Show" in asks[0]["messages"][1]["content"]


def test_repeated_errors_back_off_and_tell_once(tmp_path: Path) -> None:
    sent: list[str] = []
    clock = Clock()
    state = {"fail": True, "retry_after": None, "status": 500}

    async def notify(text: str) -> None:
        sent.append(text)

    def fetch(url: str, **kw: Any) -> tuple[int, str]:
        if state["fail"]:
            raise FetchError(f"the site answered HTTP {state['status']}", state["status"], state["retry_after"])
        return 200, quote_body(100)

    w = make_watcher(tmp_path / "w.json", fetch=fetch, clock=clock, notify=notify)
    out = asyncio.run(w.add({"kind": "quote", "target": "AAPL", "label": "Apple", "condition": "change",
                             "value": None, "text": None, "every_minutes": 5}))
    assert out["ok"] and out["now"].startswith("the first check failed")   # a 500 is kept and retried
    item = w.watches[0]
    gaps = []
    for _ in range(5):
        clock.t = item.next_at + 1
        asyncio.run(w.tick())
        gaps.append(item.next_at - clock.t)
    # the watch's own interval stretches with each failure, up to MAX_ERROR_STRETCH times
    assert gaps[0] < gaps[1] < gaps[2] and max(gaps) <= item.every_secs * watch.MAX_ERROR_STRETCH * 1.1
    assert item.errors == 6
    assert sent == ["I can't read Apple right now (the site answered HTTP 500). I'll keep trying, less often."]
    # a 429 with Retry-After holds the host in the limiter (this rig's limiter caps a backoff at 600 s)
    state.update(status=429, retry_after=300)
    clock.t = item.next_at + 1
    asyncio.run(w.tick())
    assert w.limiter.backed_off("finance.yahoo.com") == pytest.approx(300)
    # while it is held, a due watch on that host is pushed back, not fetched
    checks = item.checks + item.errors
    item.next_at = clock.t
    asyncio.run(w.tick())
    assert item.checks + item.errors == checks and item.next_at >= clock.t + 299
    # recovery is said once, and clears the host's backoff
    state["fail"] = False
    clock.t += 1000
    item.next_at = clock.t
    asyncio.run(w.tick())
    assert sent[-1] == "I can read Apple again." and item.errors == 0
    assert w.limiter.backed_off("finance.yahoo.com") == 0


def test_the_listing_is_plain_and_the_switch_reads_the_environment(tmp_path: Path) -> None:
    w = make_watcher(tmp_path / "w.json", fetch=lambda url, **kw: (200, quote_body(310)))
    assert "not watching anything" in w.listing()
    asyncio.run(w.add({"kind": "quote", "target": "AAPL", "label": "Apple", "condition": "below", "value": 300,
                       "text": None, "every_minutes": None}))
    assert w.listing().splitlines()[0] == "w1 · Apple: at or below $300 · now $310 · every 15 min"
    assert watch.configured({}).enabled is True
    assert watch.configured({"EVE_WATCH": "0"}).enabled is False
    assert watch.make_watcher(watch.configured({"EVE_WATCH": "off"})) is None
    cfg = watch.configured({"EVE_WATCH_RATE": "abc", "EVE_WATCH_HOST_GAP": "5",
                            "EVE_WATCH_MODEL": "openai/gpt-x"})
    assert cfg.rate_per_min == watch.DEFAULT_RATE_PER_MIN and cfg.host_gap_secs == 5
    assert cfg.model == "openai/gpt-x"             # allowed since every model call is on OpenRouter (2026-10-08)
    assert watch.configured({}).model == watch.DEFAULT_MODEL


# ---- the swarm's ports: lenient JSON-LD, microdata, prices, quotes, the limiter's clock -------------------

def test_messy_structured_data_is_still_read() -> None:
    # JSON-LD with a raw newline inside a string, CDATA framing, and two objects back to back
    messy = ('<script type="application/ld+json">//<![CDATA[\n{"@type":"Product","name":"Desk\nLamp",'
             '"offers":{"price":"49,99","priceCurrency":"EUR","availability":"InStock"}}\n'
             '{"@type":"BreadcrumbList"}\n//]]></script>')
    r = watch.structured(messy)
    assert (r.value, r.currency, r.available) == (49.99, "EUR", True)
    # the item's name beats a founder's in an earlier block
    two = ('<script type="application/ld+json">{"@type":"Organization","founder":{"@type":"Person","name":"A Founder"}}'
           '</script><script type="application/ld+json">{"@type":"Product","name":"Falcon","offers":{"price":9}}</script>')
    assert watch.structured(two).name == "Falcon"
    # an entity-escaped block
    escaped = '<script type="application/ld+json">{&quot;offers&quot;:{&quot;price&quot;:12}}</script>'
    assert watch.structured(escaped).value == 12
    # microdata: price in the element's text, availability in a link's href, attributes in any order
    micro = ('<div itemscope itemtype="https://schema.org/Product"><h1>Mug</h1>'
             '<div itemprop="offers" itemscope itemtype="https://schema.org/Offer">'
             '<span itemprop="price">$1,299.00</span><meta content="USD" itemprop="priceCurrency">'
             "<link itemprop='availability' href='https://schema.org/OutOfStock'></div>"
             '<div itemprop="review" itemscope itemtype="https://schema.org/Review">'
             '<span itemprop="price">5</span></div></div>')
    r = watch.structured(micro)
    assert (r.value, r.currency, r.available) == (1299.0, "USD", False)
    # a price inside a review's scope is not the item's
    only_review = ('<div itemscope itemtype="https://schema.org/Review"><span itemprop="price">5</span></div>')
    assert watch.structured(only_review).value is None
    # European and grouped prices
    for raw, want in (("1.299,00 €", 1299.0), ("12,5", 12.5), ("1,299", 1299.0), ("CHF 1'250.50", 1250.5)):
        assert watch._price(raw) == want, raw


def test_quotes_read_minor_units_and_unknown_symbols(tmp_path: Path) -> None:
    now = 1_800_000_000
    meta = {"symbol": "VOD.L", "currency": "GBp", "regularMarketPrice": 7250,
            "currentTradingPeriod": {"regular": {"start": now - 50000, "end": now - 20000}}, "fulldayPrice": 7300}
    r = watch.quote_reading(meta, 7250.0, now)
    assert (r.value, r.currency) == (72.5, "GBP") and r.note == "after hours £73"
    # inside the session, no extended-hours note
    meta["currentTradingPeriod"]["regular"] = {"start": now - 100, "end": now + 100}
    assert watch.quote_reading(meta, 7250.0, now).note == ""
    sent: list[dict[str, Any]] = []
    w = make_watcher(tmp_path / "h.json", fetch=lambda url, **kw: sent.append(kw) or (200, quote_body(1)))
    asyncio.run(w.read(Watch(id="q", kind="quote", target="AAPL", label="A", condition="change")))
    assert sent[0]["headers"]["User-Agent"] == "Mozilla/5.0"          # a full Chrome UA gets Yahoo's 429
    missing = json.dumps({"chart": {"result": None, "error": {"code": "Not Found", "description": "delisted"}}})
    w = make_watcher(tmp_path / "w.json", fetch=lambda url, **kw: (200, missing))
    out = asyncio.run(w.add({"kind": "quote", "target": "NOPE", "label": "x", "condition": "change", "value": None,
                             "text": None, "city": None, "every_minutes": None}))
    assert out == {"ok": False, "reason": "no such symbol on Yahoo Finance: NOPE"} and w.watches == []


def test_a_clock_that_steps_back_mints_no_tokens() -> None:
    clock = Clock()
    lim = limiter(clock, burst=1, rate_per_min=6, host_gap_secs=0)
    lim.take("a")
    clock.t -= 30                                   # the clock steps back: waiting longer is the safe answer
    assert lim.ready_in("a") >= 10.0
    clock.t += 30                                   # and forward again to where it was: still nothing refilled
    assert lim.ready_in("a") == pytest.approx(10.0)
    # a Rate-Limit-Reset in epoch ms is honoured when there is no Retry-After (Ticketmaster's quota header)
    import time as _time
    assert 58 <= watch._retry_after({"Rate-Limit-Reset": str(int((_time.time() + 60) * 1000))}) <= 60


# ---- vision: a page that only shows its price in a browser --------------------------------------

SPA = "<html><head><title>Loading</title></head><body><div id=root></div><script src=app.js></script></body></html>"


def test_a_js_page_is_rendered_then_seen(tmp_path: Path) -> None:
    asks: list[dict[str, Any]] = []
    renders: list[str] = []

    def ask(body: dict[str, Any]) -> dict[str, Any]:
        asks.append(body)
        if isinstance(body["messages"][1]["content"], list):          # the screenshot
            return model_reply({"price": 64.0, "currency": "USD", "available": True, "note": "Get tickets button live."})
        return model_reply({"price": None, "currency": "", "available": None, "note": "The page is empty."})

    def render(url: str, **kw: Any) -> tuple[str, bytes]:
        renders.append(url)
        return SPA, b"\xff\xd8jpeg"

    w = make_watcher(tmp_path / "w.json", fetch=lambda url, **kw: (200, SPA), ask=ask, browser=True)
    w._render = render
    out = asyncio.run(w.add(args(target="https://seats.example/show", label="Show", condition="below", value=80)))
    assert out["ok"] and out["now"] == "$64, available" and "renders it and looks at it" in out["note"]
    item = w.watches[0]
    assert item.via == "browser" and item.every_secs >= watch.FLOOR_SECS["browser"]
    # the text model once (nothing there), then the vision model with the screenshot as an image
    assert len(asks) == 2
    image = asks[1]["messages"][1]["content"][1]
    assert image["type"] == "image_url" and image["image_url"]["url"].startswith("data:image/jpeg;base64,")
    # from now on it goes straight to the browser: no plain fetch, no text model
    w._fetch = lambda url, **kw: pytest.fail("a browser page is not fetched plainly")
    asyncio.run(w.read(item))
    assert len(renders) == 2 and len(asks) == 3
    # a rendered page whose DOM carries JSON-LD needs no model at all
    w._render = lambda url, **kw: (JSON_LD_EVENT, b"")
    assert asyncio.run(w.read(item)).value == 85.0 and len(asks) == 3
    # without the browser the page stays on its text reading
    plain = make_watcher(tmp_path / "p.json", fetch=lambda url, **kw: (200, SPA), ask=ask, browser=False)
    plain._render = lambda url, **kw: pytest.fail("no browser configured")
    out = asyncio.run(plain.add(args(target="https://seats.example/show", condition="below", value=80)))
    assert out["ok"] and plain.watches[0].via == ""


def test_a_refused_page_tries_the_browser_then_search(tmp_path: Path) -> None:
    def fetch(url: str, **kw: Any) -> tuple[int, str]:
        raise FetchError("the site answered HTTP 403", 403)

    def render(url: str, **kw: Any) -> tuple[str, bytes]:
        raise FetchError("the site answered HTTP 403", 403)

    asks: list[Any] = []
    w = make_watcher(tmp_path / "w.json", fetch=fetch, browser=True,
                     ask=lambda body: asks.append(body) or model_reply({"available": False, "note": "Not yet."}))
    w._render = render
    out = asyncio.run(w.add(args(target="https://tix.example/e/1", label="Show", condition="available", value=None)))
    assert out["ok"] and w.watches[0].via == "search" and out["now"] == "not available"
    assert asks and asks[0]["tools"][0]["type"] == "openrouter:web_search"


# ---- Ticketmaster ---------------------------------------------------------------------------------

def tm_event(code: str, *, public: Optional[tuple[float, Optional[float]]] = None,
             presale: Optional[tuple[float, float]] = None, price: Optional[float] = None, eid: str = "vvG1",
             url: str = "https://www.ticketmaster.com/show/event/0E0050681F51BA4C") -> dict[str, Any]:
    sales: dict[str, Any] = {}
    if public:
        sales["public"] = {"startDateTime": _iso(public[0]), **({"endDateTime": _iso(public[1])} if public[1] else {})}
    if presale:
        sales["presales"] = [{"name": "Fan presale", "startDateTime": _iso(presale[0]), "endDateTime": _iso(presale[1])}]
    e: dict[str, Any] = {"id": eid, "name": "Big Show", "url": url, "dates": {"status": {"code": code},
                                                                                 "start": {"localDate": "2026-12-01"}},
                         "sales": sales, "_embedded": {"attractions": [{"id": "K8vZ"}]}}
    if price is not None:
        e["priceRanges"] = [{"type": "standard", "currency": "USD", "min": price, "max": price * 3}]
    return e


def _iso(t: float) -> str:
    from datetime import datetime, timezone
    return datetime.fromtimestamp(t, timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def test_ticketmaster_statuses_follow_its_own_definitions() -> None:
    now = 1_800_000_000
    day = 86400
    # offsale before the public sale: watched, not available, and the note says when it opens
    r = watch.ticketmaster_reading([tm_event("offsale", public=(now + 3 * day, None))], now)
    assert r.available is False and "1 date listed, 0 on sale now." in r.note and "the public sale opens" in r.note
    # a presale window open now counts, even while the status is offsale
    r = watch.ticketmaster_reading([tm_event("offsale", public=(now + day, None), presale=(now - 60, now + 3600))], now)
    assert r.available is True
    # onsale and rescheduled are on sale; postponed and canceled are not; an ended offsale is dropped
    assert watch.ticketmaster_reading([tm_event("onsale")], now).available is True
    assert watch.ticketmaster_reading([tm_event("rescheduled")], now).available is True
    assert watch.ticketmaster_reading([tm_event("postponed"), tm_event("canceled")], now).available is False
    r = watch.ticketmaster_reading([tm_event("offsale", public=(now - 9 * day, now - day))], now)
    assert r.available is False and r.note.startswith("0 dates listed")
    # no priceRanges: the price is unknown, never 0; with one, the lowest min
    assert watch.ticketmaster_reading([tm_event("onsale")], now).value is None
    assert watch.ticketmaster_reading([tm_event("onsale", price=89.5), tm_event("onsale", price=45.0)], now).value == 45.0
    assert watch.ticketmaster_reading([], now).available is False


def test_ticketmaster_resolves_the_artist_and_matches_links(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("TICKETMASTER_API_KEY", "tm-test")
    attractions = {"_embedded": {"attractions": [
        {"id": "T1", "name": "Big Band Tribute", "aliases": ["big band"], "upcomingEvents": {"_total": 90}},
        {"id": "K8vZ", "name": "Big Band", "upcomingEvents": {"_total": 12}},
        {"id": "K9", "name": "Big Band Theory", "upcomingEvents": {"_total": 40}}]}}
    assert watch.pick_attraction("big band!", attractions) == "K8vZ"
    assert watch.pick_attraction("nobody", attractions) == ""
    calls: list[str] = []
    now = 1_800_000_000

    def fetch(url: str, **kw: Any) -> tuple[int, str]:
        calls.append(url)
        if "/attractions.json" in url:
            return 200, json.dumps(attractions)
        other = tm_event("onsale", eid="x", url="https://www.ticketmaster.com/x/event/1111111111111111")
        other["_embedded"] = {"attractions": [{"id": "OTHER"}]}
        return 200, json.dumps({"_embedded": {"events": [tm_event("offsale", public=(now + 86400, None)), other]}})

    w = make_watcher(tmp_path / "w.json", fetch=fetch, clock=Clock(now), ticketmaster=True)
    assert "ticketmaster" in w.tools()[0]["parameters"]["properties"]["kind"]["enum"]
    assert "kind ticketmaster" in w.instructions()
    out = asyncio.run(w.add({"kind": "ticketmaster", "target": "Big Band", "label": "Big Band", "condition": "available",
                             "value": None, "text": None, "city": "Seattle", "every_minutes": None}))
    assert out["ok"] and out["now"] == "not available" and w.watches[0].ref == "K8vZ"
    assert "attractionId=K8vZ" in calls[1] and "city=Seattle" in calls[1] and "apikey=tm-test" in calls[1]
    # the other attraction's event in the same page of results is not counted
    assert "1 date listed" in out["detail"]
    # a price condition is refused, with the reason
    refused = asyncio.run(w.add({"kind": "ticketmaster", "target": "Big Band", "label": "x", "condition": "below",
                                 "value": 50, "text": None, "city": None, "every_minutes": None}))
    assert not refused["ok"] and "no longer publishes ticket prices" in refused["reason"]
    # a ticketmaster.com link's hex id is matched against each event's url, never fetched as an API id
    item = Watch(id="w9", kind="ticketmaster", condition="available", label="Show",
                 target="https://www.ticketmaster.com/big-band-seattle-12-01-2026/event/0E0050681F51BA4C")
    calls.clear()
    r = asyncio.run(w.read(item))
    assert r.available is False and "/events/0E0050681F51BA4C" not in calls[0] and "keyword=big+band+seattle" in calls[0]
    # without the key the kind is not offered
    assert "ticketmaster" not in make_watcher(tmp_path / "p.json").tools()[0]["parameters"]["properties"]["kind"]["enum"]


# ---- after the Lean pass: delivery, attribution, currency, the store (2026-09-25) ------------------------

def test_an_alert_telegram_refused_is_sent_again(tmp_path: Path) -> None:
    prices = iter([310, 290, 291])
    sends: list[str] = []
    up = {"ok": False}

    async def notify(text: str) -> bool:
        sends.append(text)
        return up["ok"]

    clock = Clock()
    w = make_watcher(tmp_path / "w.json", fetch=lambda url, **kw: (200, quote_body(next(prices))), clock=clock,
                     notify=notify)
    asyncio.run(w.add({"kind": "quote", "target": "AAPL", "label": "Apple", "condition": "below", "value": 300,
                       "text": None, "city": None, "every_minutes": 5}))
    clock.t += 400
    asyncio.run(w.tick())                             # fires, but Telegram refuses it
    item = w.watches[0]
    assert len(sends) == 1 and item.pending.startswith("Apple dropped to $290")
    assert json.loads((tmp_path / "w.json").read_text())["watches"][0]["pending"] == item.pending   # survives a restart
    up["ok"] = True
    asyncio.run(w.tick())                             # not due: only the pending alert goes
    assert sends[-1] == sends[0] and item.pending == "" and len(sends) == 2


def test_a_model_services_429_holds_the_model_service_not_the_page() -> None:
    clock = Clock()
    w = make_watcher(Path("/nonexistent/w.json"), clock=clock)

    def ask(body: dict[str, Any]) -> dict[str, Any]:
        raise FetchError("the site answered HTTP 429", 429, 120, host=watch.OPENROUTER_HOST)

    w._fetch, w._ask = (lambda url, **kw: (200, "<p>no price here</p>")), ask
    item = Watch(id="w1", kind="page", target="https://shop.example/p", label="x", condition="below", value=5,
                 every_secs=600)
    with pytest.raises(FetchError):
        asyncio.run(w.check(item))
    assert w.limiter.backed_off(watch.OPENROUTER_HOST) >= 119 and w.limiter.backed_off("shop.example") == 0
    # the backoff ladder never overflows, however many strikes
    lim = limiter(clock)
    for _ in range(1100):
        secs = lim.penalize("h")
    assert secs == 600


def test_a_reading_in_another_currency_is_no_price() -> None:
    w = Watch(id="w1", kind="page", target="https://shop.example/p", label="Lamp", condition="below", value=50)
    assert watch.evaluate(w, Reading(value=60, currency="USD")) is None
    assert watch.evaluate(w, Reading(value=45, currency="EUR")) is None          # 45 EUR is not under $50
    assert w.last_value == 60 and w.currency == "USD"
    assert watch.evaluate(w, Reading(value=45, currency="USD")) is not None
    # a change that is both a price move and an availability flip says both
    c = Watch(id="w2", kind="page", target="https://shop.example/p", label="Lamp", condition="change")
    watch.evaluate(c, Reading(value=80, currency="USD", available=False))
    alert = watch.evaluate(c, Reading(value=70, currency="USD", available=True))
    assert alert is not None and "down from $80" in alert and "available now" in alert


def test_a_hand_edited_store_never_stops_the_loop(tmp_path: Path) -> None:
    path = tmp_path / "w.json"
    good = Watch(id="w1", kind="quote", target="AAPL", label="A", condition="change", every_secs=900).to_json()
    path.write_text(json.dumps({"watches": [
        {**good, "id": "W1"},                                   # upper case: read as w1
        {**good, "label": "dup"},                                # a second w1: dropped
        {**good, "id": "w2", "next_at": "soon", "every_secs": 1.0, "armed": "yes"},   # wrong types: defaults
        {"id": "w3", "kind": "quote", "target": ["AAPL"], "condition": "change"},     # target not a string
    ]}))
    w = make_watcher(path)
    assert [x.id for x in w.watches] == ["w1", "w2"]
    w2 = w.watches[1]
    assert w2.next_at == 0.0 and w2.armed is True and w2.every_secs == watch.FLOOR_SECS["quote"]
    assert w.remove("W1")["ok"] and [x.id for x in w.watches] == ["w2"]
    asyncio.run(w.tick())                                        # sorting by next_at no longer raises


def test_ticketmaster_far_future_dates_and_a_retried_lookup(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    now = 1_800_000_000
    far = tm_event("offsale", public=(now + 86400, None))
    far["sales"]["public"]["startDateTime"] = "9999-12-31T23:59:59Z"
    r = watch.ticketmaster_reading([far], now)
    assert r.available is False and "opens later" in r.note
    monkeypatch.setenv("TICKETMASTER_API_KEY", "tm-test")
    calls: list[str] = []

    def fetch(url: str, **kw: Any) -> tuple[int, str]:
        calls.append(url)
        return 200, json.dumps({"_embedded": {"events": []}} if "events.json" in url else {})

    clock = Clock(now)
    w = make_watcher(tmp_path / "w.json", fetch=fetch, clock=clock, ticketmaster=True)
    item = Watch(id="w1", kind="ticketmaster", target="New Act", label="New Act", condition="available")
    asyncio.run(w.read(item))
    assert item.ref == "-" and sum("attractions.json" in c for c in calls) == 1
    asyncio.run(w.read(item))                                    # the same day: not looked up again
    assert sum("attractions.json" in c for c in calls) == 1
    clock.t += 86401
    asyncio.run(w.read(item))                                    # a day later: looked up again
    assert sum("attractions.json" in c for c in calls) == 2


# ---- pause, resume, and a watch for a period (owner, 2026-09-25: "sometimes i only need it for a certain period") --

def test_a_paused_watch_is_not_checked_and_resumes_by_itself_or_on_ask(tmp_path: Path) -> None:
    fetched: list[str] = []
    clock = Clock()

    def fetch(url: str, **kw: Any) -> tuple[int, str]:
        fetched.append(url)
        return 200, quote_body(310)

    w = make_watcher(tmp_path / "w.json", fetch=fetch, clock=clock)
    asyncio.run(w.add({"kind": "quote", "target": "AAPL", "label": "Apple", "condition": "below", "value": 300,
                       "text": None, "city": None, "every_minutes": 5, "for_hours": None}))
    item = w.watches[0]
    out = asyncio.run(w.handle("watch_pause", {"id": "W1", "for_hours": None}))
    assert out == {"ok": True, "paused": "Apple", "until": "until you resume it"} and item.paused
    n = len(fetched)
    for _ in range(3):
        clock.t += 3600
        asyncio.run(w.tick())
    assert len(fetched) == n and item.checks == 1                  # paused: no checks, its state kept
    assert "paused" in w.listing().splitlines()[0]
    out = asyncio.run(w.handle("watch_resume", {"id": "w1"}))
    assert out["ok"] and not item.paused
    asyncio.run(w.tick())
    assert len(fetched) == n + 1                                     # checked again right away
    # a pause for a while ends by itself
    asyncio.run(w.handle("watch_pause", {"id": "w1", "for_hours": 2}))
    assert "paused until" in w.listing().splitlines()[0]
    clock.t += 3600
    asyncio.run(w.tick())
    assert item.paused and len(fetched) == n + 1
    clock.t += 3601
    asyncio.run(w.tick())
    assert not item.paused and len(fetched) == n + 2
    # it survives a restart paused
    asyncio.run(w.handle("watch_pause", {"id": "w1", "for_hours": None}))
    assert make_watcher(tmp_path / "w.json").watches[0].paused is True
    # an unknown id and a bad period are said, not raised
    assert not asyncio.run(w.handle("watch_pause", {"id": "w9", "for_hours": None}))["ok"]
    assert not asyncio.run(w.handle("watch_pause", {"id": "w1", "for_hours": -3}))["ok"]


def test_a_watch_for_a_period_ends_by_itself_and_says_so(tmp_path: Path) -> None:
    told: list[str] = []
    clock = Clock()

    async def notify(text: str) -> bool:
        told.append(text)
        return True

    w = make_watcher(tmp_path / "w.json", fetch=lambda url, **kw: (200, quote_body(310)), clock=clock, notify=notify)
    out = asyncio.run(w.add({"kind": "quote", "target": "AAPL", "label": "Apple", "condition": "below", "value": 300,
                             "text": None, "city": None, "every_minutes": 5, "for_hours": 24}))
    assert out["ok"] and "until" in w.listing().splitlines()[0]
    assert asyncio.run(w.tick()) <= 24 * 3600                        # the loop wakes for the end, at the latest
    clock.t += 23 * 3600
    asyncio.run(w.tick())
    assert len(w.watches) == 1
    clock.t += 3601
    asyncio.run(w.tick())
    assert w.watches == [] and told[-1] == "I've stopped watching Apple: the time you gave it is up."
    assert make_watcher(tmp_path / "w.json").watches == []
    # an end set later, and cleared
    asyncio.run(w.add({"kind": "quote", "target": "AAPL", "label": "Apple", "condition": "change", "value": None,
                       "text": None, "city": None, "every_minutes": None, "for_hours": None}))
    wid = w.watches[0].id
    assert asyncio.run(w.handle("watch_set_end", {"id": wid, "for_hours": 48}))["ends"] != "no end"
    assert asyncio.run(w.handle("watch_set_end", {"id": wid, "for_hours": None}))["ends"] == "no end"
    assert w.watches[0].ends_at == 0.0
    refused = asyncio.run(w.add({"kind": "quote", "target": "AAPL", "label": "x", "condition": "change", "value": None,
                                 "text": None, "city": None, "every_minutes": None, "for_hours": 10 ** 9}))
    assert not refused["ok"] and "year" in refused["reason"]


def test_the_pause_tools_are_offered_to_the_brain(tmp_path: Path) -> None:
    w = make_watcher(tmp_path / "w.json")
    names = [t["name"] for t in w.tools()]
    assert names == ["watch_add", "watch_list", "watch_remove", "watch_pause", "watch_resume", "watch_set_end"]
    assert "for_hours" in w.tools()[0]["parameters"]["required"]
    assert "watch_pause" in w.instructions() and "watch_set_end" in w.instructions()
