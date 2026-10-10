"""America.gov integration with the shared chat/call brain, entirely offline."""

from __future__ import annotations

import asyncio
import copy
import json
import time
from typing import Any

import pytest
from test_telegram import OWNER, STRANGER, FakeApi, FakeCreate, Rig, _bare_daemon, call, say

from eve_bridge import america, telegram
from eve_bridge.daemon import Daemon

ARGS = {"question": "What notice must a landlord give?", "jurisdiction": "California"}
SOURCE = {"title": "California Courts", "url": "https://selfhelp.courts.ca.gov/eviction-tenant",
          "currency": "unverified"}


class Service:
    def __init__(self) -> None:
        self.calls: list[tuple[str, dict[str, Any]]] = []
        self.result = {"ok": True, "status": "answered", "provider": "america_gov",
                       "answer": "Check the notice rules for the tenancy.", "sources": [SOURCE],
                       **ARGS, "america_url": "https://america.gov/chat", "legal_advice": False,
                       "retrieved_at": "2026-10-04T00:00:00Z"}

    def tools(self) -> list[dict[str, Any]]:
        return [{"type": "function", "name": "america_ask", "strict": True,
                 "parameters": {"type": "object", "additionalProperties": False,
                                "properties": {"question": {"type": "string"},
                                               "jurisdiction": {"type": ["string", "null"]}},
                                "required": ["question", "jurisdiction"]}}]

    def instructions(self) -> str:
        return "America.gov: america_ask supplies legal information with sources; unavailable answers need a browser handoff."

    async def handle(self, name: str, args: dict[str, Any]) -> dict[str, Any]:
        self.calls.append((name, copy.deepcopy(args)))
        return copy.deepcopy(self.result)


async def until(predicate: Any) -> None:
    deadline = time.monotonic() + 3
    while not predicate():
        assert time.monotonic() < deadline, "shared brain did not reach the expected state"
        await asyncio.sleep(0.001)


async def clean(inlet: Any) -> None:
    await inlet._shutdown()
    await asyncio.sleep(0)


def test_optional_registration_owner_dispatch_and_disabled_fallback_boundary() -> None:
    async def run() -> None:
        off = Rig(FakeApi(), FakeCreate())
        parts, allowed, _ = await off.inlet._turn_parts(0)
        assert not america.TOOL_NAMES & allowed
        assert not america.TOOL_NAMES & {tool["name"] for tool in parts["watch_tools"]}
        assert "America.gov" not in parts["watch_block"]
        fallback: list[str] = []

        async def think(question: str, chat_id: int) -> dict[str, Any]:
            fallback.append(question)
            return {"ok": True, "answer": "fallback control"}

        off.inlet._think_hard = think
        assert (await off.inlet._tool("unknown_tool", {"question": "control"}, OWNER))["ok"]
        assert fallback == ["control"]
        fallback.clear()
        assert await off.inlet._tool("america_ask", ARGS, OWNER) == {
            "ok": False, "status": "unavailable", "reason": america.OFF_REASON,
            "america_url": america.AMERICA_URL,
        }
        assert fallback == [] and off.create.requests == []

        service = Service()
        on = Rig(FakeApi(), FakeCreate(), america=service)
        parts, allowed, _ = await on.inlet._turn_parts(0)
        assert america.TOOL_NAMES <= allowed
        assert service.tools()[0] in parts["watch_tools"]
        assert service.instructions() in parts["watch_block"]
        on.inlet.turns = [("user", "PRIVATE_HISTORY_SENTINEL"), ("eve", "PRIVATE_ATTACHMENT_SENTINEL")]
        assert await on.inlet._tool("america_ask", ARGS, OWNER) == service.result
        assert service.calls == [("america_ask", ARGS)]
        denied = await on.inlet._tool("america_ask", ARGS, STRANGER)
        assert denied == {"ok": False, "status": "unavailable",
                          "reason": "Only a configured owner can use /america.",
                          "america_url": america.AMERICA_URL}
        assert service.calls == [("america_ask", ARGS)]
        await clean(off.inlet)
        await clean(on.inlet)

    asyncio.run(run())


@pytest.mark.parametrize("spoken", [False, True])
def test_answer_and_sources_reach_text_and_calls(spoken: bool) -> None:
    """The provider result and unverified source currency survive both shared-brain paths."""
    reply = "America.gov says to check the notice rules. See https://selfhelp.courts.ca.gov/eviction-tenant"

    async def run() -> None:
        service = Service()
        responses = [call("america_ask", ARGS), say(reply)]
        create = FakeCreate(*([say("")] if spoken else []), *responses)
        rig = Rig(FakeApi(), create, america=service)
        read: list[str] = []
        if spoken:
            rig.inlet._chat_id = OWNER
            assert rig.inlet.listen(read.append)
            await until(lambda: len(create.requests) == 1)
            assert service.calls == []  # warming never executes the advertised tool
            rig.inlet.hear(ARGS["question"])
            await until(lambda: reply in read)
            warm = create.requests[0]
            assert warm["tools"] == create.requests[1]["tools"]
            assert warm["instructions"] == create.requests[1]["instructions"]
        else:
            await rig.inlet._turn(telegram.Inbound(OWNER, OWNER, ARGS["question"], message_id=1))
            assert (OWNER, reply) in rig.api.sent
        turns = create.requests[1:] if spoken else create.requests
        assert len(turns) == 2
        assert service.tools()[0] in turns[0]["tools"]
        assert service.instructions() in turns[0]["instructions"]
        output = next(item for item in turns[1]["input"] if item.get("type") == "function_call_output")
        assert json.loads(output["output"]) == service.result
        assert service.calls == [("america_ask", ARGS)]
        assert SOURCE["url"] in " ".join(text for _, text in rig.api.sent)
        await clean(rig.inlet)

    asyncio.run(run())


def test_upstream_unavailable_reaches_the_brain_without_an_answer_or_sources() -> None:
    async def run() -> None:
        service = Service()
        service.result = {"ok": False, "status": "upstream_unavailable", "reason": "America.gov is unavailable.",
                          "america_url": "https://america.gov/chat"}
        reply = "America.gov is unavailable. Open https://america.gov/chat to ask in your browser."
        create = FakeCreate(call("america_ask", ARGS), say(reply))
        rig = Rig(FakeApi(), create, america=service)
        await rig.inlet._turn(telegram.Inbound(OWNER, OWNER, ARGS["question"], message_id=1))
        output = next(item for item in create.requests[1]["input"] if item.get("type") == "function_call_output")
        result = json.loads(output["output"])
        assert result == service.result and "answer" not in result and "sources" not in result
        assert service.calls == [("america_ask", ARGS)]
        assert (OWNER, reply) in rig.api.sent
        await clean(rig.inlet)

    asyncio.run(run())


def test_service_exception_returns_normalized_failure_and_logs_only_its_type(caplog: pytest.LogCaptureFixture) -> None:
    async def run() -> None:
        service = Service()

        async def failed(name: str, args: dict[str, Any]) -> dict[str, Any]:
            raise ValueError("PRIVATE_PROVIDER_BODY_SENTINEL")

        service.handle = failed
        rig = Rig(FakeApi(), FakeCreate(), america=service)
        result = await rig.inlet._tool("america_ask", ARGS, OWNER)
        assert result == {"ok": False, "status": "upstream_unavailable",
                          "reason": "The /america lookup failed. Please try official-source research or the source website.",
                          "america_url": america.AMERICA_URL}
        assert rig.create.requests == []
        await clean(rig.inlet)

    asyncio.run(run())
    assert "ValueError" in caplog.text and "PRIVATE_PROVIDER_BODY_SENTINEL" not in caplog.text


@pytest.mark.parametrize("enabled", [False, True])
def test_daemon_lends_optional_service_only_to_an_available_brain(enabled: bool,
                                                               monkeypatch: pytest.MonkeyPatch) -> None:
    env = {"EVE_TELEGRAM": "1", "EVE_TELEGRAM_TOKEN": "mock-telegram-token",
           "EVE_TELEGRAM_OWNER": str(OWNER), "EVE_WATCH": "0", "EVE_COMPOSIO": "0", "EVE_APPS": "0",
           "EVE_AGENTCARD": "0", "EVE_SECOND_BRAIN": "0", "EVE_CHIEF": "off",
           "EVE_AMERICA": "1" if enabled else "0"}
    for key, value in env.items():
        monkeypatch.setenv(key, value)
    for name in ("_make_canvas", "_lights_hub", "_spotify_hub", "_firetv_hub", "_make_chief"):
        monkeypatch.setattr(Daemon, name, lambda *args: None)
    monkeypatch.delenv("OPENROUTER_API_KEY", raising=False)
    service = Service()
    factories: list[bool] = []

    def factory() -> Any:
        factories.append(enabled)
        return service if enabled else None

    monkeypatch.setattr(america, "make_america", factory)
    assert Daemon._make_telegram(_bare_daemon()) is None and factories == []
    monkeypatch.setenv("OPENROUTER_API_KEY", "mock-openrouter-key")
    monkeypatch.setenv("EVE_TELEGRAM", "0")
    assert Daemon._make_telegram(_bare_daemon()) is None and factories == []
    monkeypatch.setenv("EVE_TELEGRAM", "1")
    lent: list[Any] = []

    def inlet_factory(config: Any, **kwargs: Any) -> Any:
        lent.append(kwargs)
        return Rig(FakeApi(), FakeCreate(), config, america=kwargs["america"]).inlet

    monkeypatch.setattr(telegram, "make_inlet", inlet_factory)
    daemon = _bare_daemon()
    inlet = Daemon._make_telegram(daemon)
    expected = service if enabled else None
    assert inlet._america is expected and lent[0]["america"] is expected
    assert Daemon._make_america(daemon) is expected and factories == [enabled]
    assert service.calls == []  # startup constructs only; it cannot research or contact a provider


def test_daemon_factory_failure_disables_service_without_logging_private_data(monkeypatch: pytest.MonkeyPatch,
                                                                            caplog: pytest.LogCaptureFixture) -> None:
    def factory() -> Any:
        raise ValueError("PRIVATE_PROVIDER_BODY_SENTINEL")

    monkeypatch.setattr(america, "make_america", factory)
    daemon = _bare_daemon()
    assert Daemon._make_america(daemon) is None
    assert Daemon._make_america(daemon) is None
    assert "ValueError" in caplog.text and "PRIVATE_PROVIDER_BODY_SENTINEL" not in caplog.text
