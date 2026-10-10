"""Default government-index transport and shared-brain source handoff, offline."""

from __future__ import annotations

import asyncio
import json
import threading
from pathlib import Path
from unittest.mock import Mock

import pytest
from test_america_integration import clean, until
from test_telegram import OWNER, FakeApi, FakeCreate, Rig, call, say, update

from eve_bridge import america, firecrawl, telegram
from eve_bridge.america_index import GovernmentIndex, IndexFailure

ARGS = {"question": "Where can I read the United States Code?", "jurisdiction": "U.S. federal"}
URL = "https://uscode.house.gov/currency/currency.shtml"


def test_default_factory_uses_index_only_when_enabled_and_key_is_configured(monkeypatch):
    index = Mock()
    factory = Mock(return_value=index)
    monkeypatch.setattr("eve_bridge.america_index.GovernmentIndex", factory)
    assert america.configured({}).transport == "alexandria"
    assert america.make_america({"FIRECRAWL_API_KEY": "mock-key"}) is None
    missing = america.make_america({"EVE_AMERICA": "1"})
    assert missing._index is None
    assert asyncio.run(missing.handle("america_ask", ARGS))["status"] == "unavailable"
    factory.assert_not_called()
    service = america.make_america({"EVE_AMERICA": "1", "FIRECRAWL_API_KEY": "mock-key"})
    assert service._index is index and service._browser is None
    factory.assert_called_once_with(timeout_secs=55.0)
    index.query.assert_not_called()


@pytest.mark.parametrize("args,status", [
    ({**ARGS, "history": ["PRIVATE_HISTORY"]}, "invalid_request"),
    ({**ARGS, "jurisdiction": None}, "needs_jurisdiction"),
    ({**ARGS, "question": " "}, "invalid_request"),
])
def test_index_validation_precedes_any_external_call(args, status):
    index = Mock()
    service = america.America(america.AmericaConfig(True), index=index)
    assert asyncio.run(service.handle("america_ask", args))["status"] == status
    index.query.assert_not_called()


def test_index_failure_does_not_call_browser_or_http(monkeypatch):
    index = Mock()
    index.query.side_effect = IndexFailure("Government index is unavailable.")
    browser = Mock()
    http = Mock(side_effect=AssertionError("legacy HTTP attempted"))
    monkeypatch.setattr(america.httpx, "AsyncClient", http)
    service = america.America(america.AmericaConfig(True), index=index, browser=browser)
    result = asyncio.run(service.handle("america_ask", ARGS))
    assert result == {"ok": False, "status": "upstream_unavailable",
                      "reason": "Government index is unavailable.", "america_url": america.AMERICA_URL}
    index.query.assert_called_once_with(ARGS["question"], ARGS["jurisdiction"])
    browser.ask.assert_not_called()
    http.assert_not_called()


def test_index_runs_off_event_loop_and_timeout_does_not_retry():
    async def run():
        entered, release = threading.Event(), threading.Event()
        calls = []

        def query(question, jurisdiction):
            calls.append((question, jurisdiction))
            entered.set()
            assert release.wait(2), "test worker was not released"
            return {"ok": True}

        index = Mock(query=query)
        service = america.America(america.AmericaConfig(True, timeout_secs=0.05), index=index)
        task = asyncio.create_task(service.handle("america_ask", ARGS))
        try:
            await until(entered.is_set)
            result = await task
            assert result["status"] == "upstream_unavailable" and "timed out" in result["reason"]
            assert calls == [(ARGS["question"], ARGS["jurisdiction"])]
        finally:
            release.set()

    asyncio.run(run())


@pytest.mark.parametrize("spoken", [False, True])
def test_real_index_bundle_reaches_menu_and_call_synthesis(spoken, monkeypatch):
    calls, reads = [], []
    monkeypatch.setattr(firecrawl, "meter", Mock())

    def runner(request, *, timeout):
        calls.append(request)
        assert timeout > 0
        return {"data": {"results": 2, "data": [
            {"url": URL + ";jsessionid=abc", "title": "U.S. Code currency", "snippet": "Code information",
             "updated_at": 1787585414000},
            {"url": URL + ";jsessionid=def", "title": "Duplicate", "snippet": "Duplicate"},
        ]}, "credits": 2}

    def fetch(url, *, timeout):
        reads.append(url)
        assert timeout > 0
        return 200, "<html><body><main><h1>United States Code</h1><p>Official currency information.</p></main></body></html>"

    async def run():
        index = GovernmentIndex(runner=runner, fetch=fetch)
        service = america.America(america.AmericaConfig(True), index=index)
        reply = f"Read the [official Code]({URL})."
        create = FakeCreate(*([say("")] if spoken else []), call("america_ask", ARGS), say(reply))
        rig = Rig(FakeApi(), create, america=service)
        rig.inlet.turns = [("user", "PRIVATE_HISTORY_SENTINEL")]
        if spoken:
            heard = []
            rig.inlet._chat_id = OWNER
            assert rig.inlet.listen(heard.append)
            await until(lambda: len(create.requests) == 1)
            assert not calls
            rig.inlet.hear(ARGS["question"])
            await until(lambda: reply in heard)
        else:
            rig.inlet._dispatch(update("/america " + ARGS["question"] + " This is U.S. federal."))
            await until(lambda: (OWNER, reply) in rig.api.sent)
        output = next(item for item in create.requests[-1]["input"]
                      if item.get("type") == "function_call_output")
        result = json.loads(output["output"])
        assert result["ok"] and result["status"] == "sources_found"
        assert result["provider"] == "firecrawl_gov_index" and "answer" not in result
        assert len(result["sources"]) == 1 and result["sources"][0]["url"] == URL
        assert result["sources"][0]["read_status"] == "live"
        assert "Official currency information." in result["sources"][0]["page_text"]
        assert result["sources"][0]["currency"] == "unverified"
        assert "sources_found" in create.requests[-1]["instructions"]
        assert len(calls) == 1 and reads == [URL]
        assert "PRIVATE_HISTORY_SENTINEL" not in json.dumps(calls)
        assert america.TOOL in create.requests[-1]["tools"]
        assert telegram.AMERICA_MENU[1] == "Search U.S. law and government sources"
        await clean(rig.inlet)
        return result

    result = asyncio.run(run())
    contract = json.loads((Path(__file__).resolve().parents[2] / "docs/contracts/america-alexandria.json").read_text())
    # The local JSON artifact declares its source-bundle schema; never validate
    # Alexandria source output against the legacy America.gov generated answer.
    assert contract["type"] == "object" and contract["additionalProperties"] is False
    assert set(contract["required"]) == set(result) == set(contract["properties"])
    for name, value in result.items():
        prop = contract["properties"][name]
        if "const" in prop:
            assert value == prop["const"]
