"""web_reader.py: the web body, and firecrawl.py's Firecrawl search and Alexandria tools it reaches through
search_router.py. Fakes for TinyFish, Firecrawl, the engine search and the model, so no request, credit or token is
spent; the live end-to-end check is outside the suite. What is pinned: the answer comes only from what eve read,
only those links come back, the router's provider answers the task, and every way it can fail hands the whole task
to Codex."""

from __future__ import annotations

import asyncio
import json
from typing import Any

import pytest

from eve_bridge import firecrawl, search_router, spend, tinyfish, watch, web_reader, websearch
from eve_bridge.agent_contract import AgentEvent
from eve_bridge.web_reader import ReaderConfig, WebReaderAgent

PAGES = [{"url": "https://www.weather.example/denver", "title": "Denver forecast", "markdown": "Sat: snow, 28F."},
         {"url": "https://news.example/denver", "title": "Denver news", "markdown": "Storm this weekend."}]


def model(reply: dict[str, Any]) -> Any:
    sent: list[dict[str, Any]] = []

    def ask(body: dict[str, Any]) -> dict[str, Any]:
        sent.append(body)
        return {"choices": [{"message": {"content": json.dumps(reply)}}], "usage": {"cost": 0.0001}}
    ask.sent = sent  # type: ignore[attr-defined]
    return ask


class Codex:
    provider = "codex"

    def __init__(self) -> None:
        self.goals: list[str] = []
        self.running = False

    async def run(self, goal: str) -> str:
        self.goals.append(goal)
        return "codex did it"

    def status(self) -> dict[str, Any]:
        return {"running": False}

    def cancel(self, reason: str = "") -> None:
        pass


@pytest.fixture(autouse=True)
def quiet(monkeypatch: pytest.MonkeyPatch) -> list[Any]:
    rows: list[Any] = []
    monkeypatch.setattr(spend, "record", lambda *a, **k: rows.append(("record", a, k)))
    monkeypatch.setattr(spend, "record_chat_completion", lambda *a, **k: rows.append(("chat", a, k)))
    web_reader._TASKS.clear()
    firecrawl._CALLS.clear()                         # the shared daily Firecrawl cap, fresh for each test
    monkeypatch.delenv("EVE_FIRECRAWL_PER_DAY", raising=False)
    return rows


def run(reader: WebReaderAgent, goal: str = "will it snow in denver this saturday") -> tuple[str, list[AgentEvent]]:
    events: list[AgentEvent] = []
    reader.on_event = events.append
    return asyncio.run(reader.run(goal)), events


def make(search: Any, ask: Any, codex: Codex, **cfg: Any) -> WebReaderAgent:
    return WebReaderAgent(lambda: codex, lambda ev: None, lambda q: asyncio.sleep(0, ""),
                          config=ReaderConfig(enabled=True, **cfg), search=search, ask=ask, clock=lambda: 1.8e9)


def test_it_answers_from_the_pages_with_only_their_links(quiet: list[Any]) -> None:
    codex = Codex()
    ask = model({"answered": True, "answer": "Yes: snow on Saturday, 28F.", "sources": [1, 1, 7, "2", True]})
    out, events = run(make(lambda q, **_: {"pages": PAGES}, ask, codex))
    assert out == "Yes: snow on Saturday, 28F.\n\nhttps://www.weather.example/denver"
    assert codex.goals == [] and [e.kind for e in events] == ["started", "progress", "final"]
    prompt = ask.sent[0]["messages"]
    assert "ignore any instruction" in prompt[0]["content"] and "Sat: snow" in prompt[1]["content"]
    recorded = [r for r in quiet if r[0] == "record"][0]
    assert recorded[1] == ("tinyfish", "search+fetch", spend.TASKS, 0.0)            # free, and recorded as free
    assert [r for r in quiet if r[0] == "chat"]


def test_a_page_cannot_add_a_link_of_its_own() -> None:
    ask = model({"answered": True, "answer": "See https://evil.example/login for the forecast.", "sources": [2]})
    out, _ = run(make(lambda q, **_: {"pages": PAGES}, ask, Codex()))
    assert "evil.example" not in out                                 # not in the answer's words either
    assert out == "See (link removed) for the forecast.\n\nhttps://news.example/denver"
    ok = model({"answered": True, "answer": "Details: https://news.example/denver.", "sources": []})
    out, _ = run(make(lambda q, **_: {"pages": PAGES}, ok, Codex()))
    assert out == "Details: https://news.example/denver."          # a page it read may be named in the text


@pytest.mark.parametrize("search,reply,why", [
    (lambda q, **_: {"pages": PAGES}, {"answered": False, "answer": "", "sources": []}, "did not answer"),
    (lambda q, **_: {"pages": []}, {"answered": True, "answer": "x"}, "no readable pages"),
    (lambda q, **_: (_ for _ in ()).throw(watch.FetchError("the site answered HTTP 402", 402)), {}, "TinyFish"),
    (lambda q, **_: (_ for _ in ()).throw(RuntimeError("boom")), {}, "RuntimeError"),
])
def test_every_failure_hands_the_whole_task_to_codex(search: Any, reply: Any, why: str,
                                                     caplog: pytest.LogCaptureFixture) -> None:
    codex = Codex()
    caplog.set_level("INFO")
    reader = make(search, model(reply), codex)
    out, _ = run(reader, "compare two laptops")
    assert out == "codex did it" and codex.goals == ["compare two laptops"] and reader.handed_on
    assert why in caplog.text


def test_the_daily_cap_hands_to_codex_without_spending() -> None:
    searched: list[str] = []
    ask = model({"answered": True, "answer": "ok", "sources": []})
    for _ in range(2):
        out, _ = run(make(lambda q, **_: searched.append(q) or {"pages": PAGES}, ask, Codex(),
                          tasks_per_day=2))
        assert out == "ok"
    codex = Codex()
    out, _ = run(make(lambda q, **_: searched.append(q) or {"pages": PAGES}, ask, codex, tasks_per_day=2))
    assert out == "codex did it" and len(searched) == 2


def test_stop_while_reading_is_stopped_not_handed_on() -> None:
    codex = Codex()
    reader: WebReaderAgent

    def search(q: str, **_: Any) -> dict[str, Any]:
        reader.cancel("owner said stop")
        return {"pages": PAGES}

    reader = make(search, model({"answered": True, "answer": "x", "sources": []}), codex)
    out, events = run(reader)
    assert out == "Stopped." and events[-1].kind == "cancelled" and codex.goals == []


def tinyfish_api(results: list[dict[str, Any]], read: list[dict[str, Any]], errors: Any = ()) -> Any:
    """A fake watch.http_request answering TinyFish's search (GET) and fetch (POST); records what was sent."""
    sent: list[dict[str, Any]] = []

    def fake(url: str, *, data: Any = None, headers: Any = None, timeout: float = 0) -> tuple[int, str]:
        sent.append({"url": url, "body": json.loads(data) if data else None, "headers": headers})
        if url.startswith(tinyfish.SEARCH_URL):
            return 200, json.dumps({"query": "q", "results": results, "total_results": len(results), "page": 0})
        if isinstance(errors, Exception):
            raise errors
        return 200, json.dumps({"results": read, "errors": list(errors)})
    fake.sent = sent  # type: ignore[attr-defined]
    return fake


def test_the_search_and_fetch_requests(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("TINYFISH_API_KEY", "tf-test")
    api = tinyfish_api(
        [{"position": i + 1, "url": f"https://{c}.example", "title": c.upper(), "snippet": f"{c} snippet"}
         for i, c in enumerate("abcd")] + [{"title": "no link"}],
        [{"url": "https://b.example", "title": "B page", "text": "b text"},
         {"url": "https://a.example", "title": None, "text": "a text"},
         {"url": "https://c.example", "text": "   "}],                     # read but empty: its snippet stands in
        [{"url": "https://x.example", "error": "timeout"}])
    monkeypatch.setattr(watch, "http_request", api)
    got = web_reader.search_and_read("  will   it snow ")
    search, fetch = api.sent
    assert search["url"] == tinyfish.SEARCH_URL + "?query=will+it+snow" and search["body"] is None
    assert search["headers"]["X-API-Key"] == "tf-test" and "Authorization" not in search["headers"]
    assert fetch["url"] == tinyfish.FETCH_URL and fetch["headers"]["X-API-Key"] == "tf-test"
    assert fetch["body"]["urls"] == ["https://a.example", "https://b.example", "https://c.example"]  # the top 3
    assert fetch["body"]["format"] == "markdown" and fetch["body"]["ttl"] == web_reader.TTL_SECS
    assert got["pages"] == [{"url": "https://a.example", "title": "A", "markdown": "a text"},     # search order
                            {"url": "https://b.example", "title": "B page", "markdown": "b text"},
                            {"url": "https://c.example", "title": "C", "markdown": "c snippet"}]


def test_a_failed_fetch_answers_from_the_snippets(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("TINYFISH_API_KEY", "tf-test")
    api = tinyfish_api([{"url": "https://a.example", "title": "A", "snippet": "a snippet"},
                        {"url": "https://b.example", "title": "B", "snippet": ""}], [],
                       watch.FetchError("the site answered HTTP 429", 429))
    monkeypatch.setattr(watch, "http_request", api)
    assert web_reader.search_and_read("q")["pages"] == [{"url": "https://a.example", "title": "A",
                                                         "markdown": "a snippet"}]


def test_a_failed_search_is_a_fetch_error_from_tinyfish(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("TINYFISH_API_KEY", "tf-test")

    def refused(*a: Any, **k: Any) -> Any:
        raise watch.FetchError("the site answered HTTP 401", 401)

    monkeypatch.setattr(watch, "http_request", refused)
    with pytest.raises(watch.FetchError, match="TinyFish: the site answered HTTP 401") as e:
        web_reader.search_and_read("q")
    assert e.value.host == tinyfish.SEARCH_HOST and e.value.status == 401
    monkeypatch.delenv("TINYFISH_API_KEY")
    with pytest.raises(watch.FetchError, match="TINYFISH_API_KEY"):
        web_reader.search_and_read("q")


def test_the_switch(monkeypatch: pytest.MonkeyPatch) -> None:
    from eve_bridge import browser_router

    assert web_reader.configured({}).enabled is False                                   # no key
    assert web_reader.configured({"TINYFISH_API_KEY": "tf-x"}).enabled is browser_router.SHIPPED
    assert web_reader.configured({"TINYFISH_API_KEY": "tf-x", "EVE_WEB_READER": "0"}).enabled is False
    monkeypatch.setattr(browser_router, "SHIPPED", False)
    assert web_reader.configured({"TINYFISH_API_KEY": "tf-x"}).enabled is False       # held gates: off
    assert web_reader.configured({"TINYFISH_API_KEY": "tf-x", "EVE_WEB_READER": "1"}).enabled is True
    assert web_reader.configured({"EVE_WEB_READER_TASKS": "9"}).tasks_per_day == 9


def test_the_daemon_routes_jev_first_then_the_chrome_rule(monkeypatch: pytest.MonkeyPatch) -> None:
    from types import SimpleNamespace

    from eve_bridge.daemon import Daemon

    monkeypatch.setattr(web_reader, "configured", lambda env=None: ReaderConfig(enabled=True))
    verdicts = {"weather in denver": "web", "check my gmail": "codex", "boom": RuntimeError}

    def jev(goal: str) -> str:
        v = verdicts[goal]
        if v is RuntimeError:
            raise RuntimeError
        return v

    async def chrome_route(goal: str) -> str:
        return "chrome"

    host = SimpleNamespace(_reader_router=jev)
    monkeypatch.setattr(Daemon, "_chrome_body", lambda self, *a: {"make_auto": object, "route_body": chrome_route})
    wiring = Daemon._bodies(host, lambda: Codex(), lambda ev: None, None)
    route = wiring["route_body"]
    assert asyncio.run(route("weather in denver")) == "web"
    assert asyncio.run(route("check my gmail")) == "chrome"            # Jev said owner: the Chrome rule decides
    assert asyncio.run(route("boom")) == "chrome"                      # Jev failed: as if it had said owner
    assert isinstance(wiring["bodies"]["web"](), WebReaderAgent)
    host_off = SimpleNamespace(_reader_router=None)
    assert "bodies" not in Daemon._bodies(host_off, lambda: Codex(), lambda ev: None, None)


# ---- routed: the search router picks this body's provider (search_router.py) ---------------------------------

ROUTED = websearch.SearchConfig(engine="openrouter-perplexity", providers=("perplexity", "tinyfish", "firecrawl"))


def routed(provider: str, codex: Codex, *, search: Any = None, ask: Any = None, fc_search: Any = None,
           tool_runner: Any = None, perplexity: Any = None) -> WebReaderAgent:
    def unused(*a: Any, **k: Any) -> Any:
        pytest.fail("this provider should not run")

    return WebReaderAgent(lambda: codex, lambda ev: None, lambda q: asyncio.sleep(0, ""),
                          config=ReaderConfig(enabled=True, search=ROUTED), clock=lambda: 1.8e9,
                          search=search or unused, ask=ask or unused, firecrawl_search=fc_search or unused,
                          tool_runner=tool_runner or unused, perplexity=perplexity or unused,
                          route=lambda text, available: search_router.Route(provider, 0.96, 200.0))


def engine(answer: str) -> Any:
    calls: list[tuple[str, str, str]] = []

    def run(query: str, system: str, feature: str) -> dict[str, Any]:
        calls.append((query, system, feature))
        return {"ok": True, "answer": answer, "usage": {"in": 1, "out": 1}, "ms": 900, "cost_usd": 0.005,
                "sources": [{"title": "Review A", "url": "https://reviews.example/a", "snippet": ""},
                            {"title": "Review B", "url": "https://reviews.example/b", "snippet": ""}]}
    run.calls = calls  # type: ignore[attr-defined]
    return run


def test_a_many_source_task_is_answered_by_the_engine_search_with_its_citations() -> None:
    per = engine("The Sony and the Bose lead most reviews (Review A, Review B).")
    reader = routed("perplexity", Codex(), perplexity=per)
    out, events = run(reader, "best noise cancelling headphones this year")
    assert out == ("The Sony and the Bose lead most reviews (Review A, Review B).\n\n"
                   "https://reviews.example/a\nhttps://reviews.example/b")
    assert per.calls[0][1] == search_router.READER_SYSTEM and per.calls[0][2] == spend.TASKS
    assert reader.provider == "web-reader" and reader.status()["search_provider"] == "perplexity"
    assert [e.kind for e in events] == ["started", "progress", "final"]


def test_an_engine_search_that_did_not_answer_hands_the_task_to_codex() -> None:
    codex = Codex()
    out, _ = run(routed("perplexity", codex, perplexity=engine("NOT FOUND")), "compare two laptops")
    assert out == "codex did it" and codex.goals == ["compare two laptops"]


def test_a_routed_reader_that_fails_tries_the_engine_search_before_codex() -> None:
    per = engine("Open 10am to 5pm.")
    reader = routed("tinyfish", Codex(), search=lambda q: {"pages": []}, perplexity=per)
    out, _ = run(reader, "exploratorium hours sunday")
    assert out.startswith("Open 10am to 5pm.") and len(per.calls) == 1
    assert reader.status()["search_provider"] == "perplexity"


# ---- Alexandria: data tools beside the pages (firecrawl.py, routed) --------------------------------------------

FLIGHTS = {"provider": "flights-google-com", "capability": "flights/search_flights", "name": "Search Google Flights",
           "about": "One-way fares.", "options": [
               {"name": "origin", "type": "string", "about": "", "required": True},
               {"name": "destination", "type": "string", "about": "", "required": True},
               {"name": "departure_date", "type": "string", "about": "", "required": True},
               {"name": "adults", "type": "number", "about": "", "required": False}]}
FARES = {"flights": [{"price": 129, "airline": "JetBlue", "url": "https://www.google.com/travel/flights/b1"}]}


def models(*replies: dict[str, Any]) -> Any:
    """The model answering in turn: the first reply to the first call, and so on."""
    sent: list[dict[str, Any]] = []

    def ask(body: dict[str, Any]) -> dict[str, Any]:
        sent.append(body)
        return {"choices": [{"message": {"content": json.dumps(replies[len(sent) - 1])}}], "usage": {"cost": 0.0001}}
    ask.sent = sent  # type: ignore[attr-defined]
    return ask


PICK = {"answered": False, "tool": {"id": 1, "options": {"origin": "SFO", "destination": "JFK",
                                                         "departure_date": "2026-10-02", "adults": 1,
                                                         "api_key": "x", "nested": {"a": 1}}}}


def test_a_fitting_tool_is_run_and_answers_with_only_its_own_links(quiet: list[Any],
                                                                   monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("EVE_FIRECRAWL_USD", "0.004")
    ran: list[dict[str, Any]] = []
    ask = models(PICK, {"answered": True, "answer": "Cheapest: JetBlue $129 https://www.google.com/travel/flights/b1 "
                                                    "or https://evil.example/pay", "sources": ["T"]})
    searched: list[Any] = []
    reader = routed("firecrawl", Codex(), ask=ask,
                    fc_search=lambda q, **k: searched.append(k) or {"pages": PAGES, "tools": [FLIGHTS], "credits": 4},
                    tool_runner=lambda call: ran.append(call) or {"data": FARES, "credits": 5})
    out, events = run(reader, "cheapest flight SFO to JFK this Friday")
    assert searched == [{"tools": True}]
    # Only the declared, plain options reach Firecrawl; the invented ones are dropped.
    assert ran == [{"provider": "flights-google-com", "capability": "flights/search_flights",
                    "options": {"origin": "SFO", "destination": "JFK", "departure_date": "2026-10-02", "adults": 1}}]
    assert out == "Cheapest: JetBlue $129 https://www.google.com/travel/flights/b1 or (link removed)"
    assert "Checking Search Google Flights…" in [e.text for e in events]
    assert "Tools:" in ask.sent[0]["messages"][1]["content"] and "[T]" in ask.sent[1]["messages"][1]["content"]
    records = [r[1] for r in quiet if r[0] == "record"]
    assert [r[:3] for r in records] == [("firecrawl", "search", spend.TASKS),
                                        ("firecrawl", "alexandria flights-google-com", spend.TASKS)]
    assert records[0][3] == pytest.approx(0.016) and records[1][3] == pytest.approx(0.02)
    assert reader.status()["search_provider"] == "firecrawl"


@pytest.mark.parametrize("pick", [
    {"answered": False, "tool": {"id": 2, "options": {}}},                             # not a tool the search offered
    {"answered": False, "tool": {"id": 1, "options": {"origin": "SFO"}}},              # a required option missing
    {"answered": False, "tool": "flights"},
])
def test_a_tool_pick_that_does_not_check_out_runs_nothing(pick: dict[str, Any]) -> None:
    codex = Codex()
    per = engine("NOT FOUND")
    reader = routed("firecrawl", codex, ask=models(pick), perplexity=per,
                    fc_search=lambda q, **k: {"pages": PAGES, "tools": [FLIGHTS]})
    out, _ = run(reader)
    assert out == "codex did it" and len(per.calls) == 1        # tool_runner would fail the test if it ran


def test_a_failed_tool_or_unaccepted_terms_falls_back_to_the_pages() -> None:
    def refused(call: dict[str, Any]) -> dict[str, Any]:
        raise watch.FetchError("Alexandria flights-google-com/flights/search_flights: THIRD_PARTY_DATA_TERMS_REQUIRED")

    ask = models(PICK, {"answered": True, "answer": "Fares start near $130.", "sources": [1]})
    out, _ = run(routed("firecrawl", Codex(), ask=ask, tool_runner=refused,
                        fc_search=lambda q, **k: {"pages": PAGES, "tools": [FLIGHTS]}))
    assert out == "Fares start near $130.\n\nhttps://www.weather.example/denver"
    assert "[T]" not in ask.sent[1]["messages"][1]["content"]


def test_the_firecrawl_search_request(monkeypatch: pytest.MonkeyPatch) -> None:
    sent: dict[str, Any] = {}
    monkeypatch.setenv("FIRECRAWL_API_KEY", "fc-test")

    def fake(url: str, *, data: bytes, headers: Any, timeout: float) -> tuple[int, str]:
        sent.update(url=url, body=json.loads(data), headers=headers)
        return 200, json.dumps({"success": True, "creditsUsed": 5, "data": {"web": [
            {"url": "https://a.example", "title": "A", "markdown": "text"},
            {"url": "https://b.example", "title": "B", "markdown": ""},          # nothing to read: dropped
            {"metadata": {"sourceURL": "https://c.example"}, "description": "desc only"}]}})

    monkeypatch.setattr(watch, "http_request", fake)
    got = firecrawl.search("  will   it snow ")
    assert sent["url"] == firecrawl.SEARCH_URL and sent["headers"]["Authorization"] == "Bearer fc-test"
    assert sent["body"]["query"] == "will it snow" and sent["body"]["limit"] == firecrawl.RESULTS
    assert sent["body"]["scrapeOptions"]["formats"] == ["markdown"] and sent["body"]["scrapeOptions"]["maxAge"] > 0
    assert [p["url"] for p in got["pages"]] == ["https://a.example", "https://c.example"] and got["credits"] == 5


def test_the_alexandria_requests(monkeypatch: pytest.MonkeyPatch) -> None:
    sent: list[dict[str, Any]] = []
    monkeypatch.setenv("FIRECRAWL_API_KEY", "fc-test")

    def fake(url: str, *, data: bytes, headers: Any, timeout: float) -> tuple[int, str]:
        sent.append({"url": url, "body": json.loads(data)})
        if url == firecrawl.TOOL_URL:
            return 200, json.dumps({"success": True, "data": {"alexandria": [
                {"provider": "p", "capability": "c", "creditsCost": 5, "data": FARES}]}})
        return 200, json.dumps({"success": True, "creditsUsed": 4, "data": {
            "web": [{"url": "https://a.example", "markdown": "text"}],
            "tools": [{"provider": "flights-google-com", "capability": "flights/search_flights",
                       "name": "Search Google Flights", "description": "Fares",
                       "options": [{"name": "origin", "type": "string", "required": True}]},
                      {"name": "no provider"}]}})

    monkeypatch.setattr(watch, "http_request", fake)
    got = firecrawl.search("flights", tools=True)
    assert sent[0]["body"]["sources"] == [{"type": "web"}, {"type": "alexandria"}]
    assert sent[0]["body"]["toolDetail"] == "full"
    assert [t["provider"] for t in got["tools"]] == ["flights-google-com"] and got["tools"][0]["options"][0]["required"]
    ran = firecrawl.run_tool({"provider": "p", "capability": "c", "options": {}})
    assert sent[1]["url"] == firecrawl.TOOL_URL and sent[1]["body"]["alexandria"][0]["provider"] == "p"
    assert ran == {"data": FARES, "credits": 5}
    firecrawl.search("flights")
    assert sent[2]["body"]["sources"] == [{"type": "web"}] and "toolDetail" not in sent[2]["body"]


def test_a_firecrawl_failure_is_a_fetch_error_from_firecrawl(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("FIRECRAWL_API_KEY", "fc-test")
    monkeypatch.setattr(watch, "http_request", lambda *a, **k: (200, json.dumps({"success": True, "data": {
        "alexandria": [{"provider": "p", "capability": "c", "error": "upstream 500"}]}})))
    with pytest.raises(watch.FetchError, match="upstream 500") as e:
        firecrawl.run_tool({"provider": "p", "capability": "c", "options": {}})
    assert e.value.host == firecrawl.HOST

    def refused(*a: Any, **k: Any) -> Any:
        raise watch.FetchError("the site answered HTTP 402", 402)

    monkeypatch.setattr(watch, "http_request", refused)
    with pytest.raises(watch.FetchError, match="Firecrawl: the site answered HTTP 402") as e:
        firecrawl.search("q")
    assert e.value.status == 402 and e.value.host == firecrawl.HOST
    monkeypatch.delenv("FIRECRAWL_API_KEY")
    with pytest.raises(watch.FetchError, match="FIRECRAWL_API_KEY"):
        firecrawl.search("q")


def test_a_credits_price_is_read_or_left_unpriced() -> None:
    assert firecrawl.usd_per_credit({}) is None
    assert firecrawl.usd_per_credit({"EVE_FIRECRAWL_USD": "0.004"}) == 0.004
    assert firecrawl.usd_per_credit({"EVE_FIRECRAWL_USD": "cheap"}) is None
    assert firecrawl.usd_per_credit({"EVE_FIRECRAWL_USD": "7"}) is None


def test_the_reader_takes_the_routers_providers_and_the_off_switch_keeps_tinyfish() -> None:
    keys = {"OPENROUTER_API_KEY": "r", "TYPESAFE_API_KEY": "ts-test", "TINYFISH_API_KEY": "tf-x",
            "FIRECRAWL_API_KEY": "fc-x", "EVE_WEB_READER": "1"}
    assert web_reader.configured({**keys, "EVE_SEARCH_ROUTER": "on"}).search.providers == (
        "perplexity", "tinyfish", "firecrawl")
    off = web_reader.configured({**keys, "EVE_SEARCH_ROUTER": "off"})
    assert off.search.providers == () and off.enabled
    reader = make(lambda q, **_: {"pages": PAGES}, model({"answered": True, "answer": "ok", "sources": []}), Codex())
    assert reader.provider == "tinyfish" and run(reader)[0] == "ok"
