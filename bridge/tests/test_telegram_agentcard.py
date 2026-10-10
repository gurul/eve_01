"""Offline AgentCard integration: actual question routing, owner checks and optional service seams."""

from __future__ import annotations

import asyncio
import copy
import dataclasses
import json
import time
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import pytest
from test_telegram import (
    CFG,
    OWNER,
    STRANGER,
    FakeApi,
    FakeCreate,
    FakeMemory,
    Rig,
    _bare_daemon,
    call,
    say,
    tap_update,
    update,
)

from eve_bridge import agentcard, phone_call, self_context, telegram
from eve_bridge.daemon import Daemon

ARGS = {"conversation_id": "conversation-1", "cart_hashes": ["0123456789abcdef"]}


class Service:
    def __init__(self) -> None:
        self.reads: list[Any] = []
        self.confirmed: list[Any] = []
        self.snapshot = object()
        self.closed = False

    def tools(self) -> list[dict[str, Any]]:
        return [{"type": "function", "name": name, "strict": True,
                 "parameters": {"type": "object", "properties": {}, "required": [],
                                "additionalProperties": False}}
                for name in agentcard.TOOL_NAMES]

    def instructions(self) -> str:
        return "AgentCard: use agentcard_confirm for the exact cart; the owner approves it."

    def handle(self, name: str, args: dict[str, Any]) -> dict[str, Any]:
        self.reads.append((name, args))
        return {"ok": True, "status": "needs_input", "reply": "SAFE_CART_REPLY"}

    def prepare_confirm(self, conversation_id: str, cart_hashes: list[str]) -> dict[str, Any]:
        return {"ok": True, "snapshot": self.snapshot,
                "question": "PRIVATE_PREVIEW: Paper, quantity 1. Total $5.00. Purchase this cart?",
                "about": "AgentCard: asked for approval of a cart totalling $5.00."}

    def confirm(self, snapshot: Any, cancelled: Any = None) -> dict[str, Any]:
        assert snapshot is self.snapshot
        assert cancelled is not None and not cancelled.is_set()
        self.confirmed.append(snapshot)
        return {"ok": True, "status": "order_placed", "order_id": "safe-order"}

    def close(self) -> None:
        self.closed = True


async def until(predicate: Any) -> None:
    end = time.monotonic() + 3
    while not predicate():
        assert time.monotonic() < end, "integration did not reach expected state"
        await asyncio.sleep(0.001)


async def clean(inlet: Any) -> None:
    await inlet._shutdown()
    await asyncio.sleep(0)


def test_long_purchase_preview_is_read_completely_on_the_call() -> None:
    async def run() -> None:
        service = Service()
        preview = "\n".join(f"Merchant {merchant}: Item {item}, quantity 2, API price 521 cents."
                            for merchant in range(8) for item in range(12))
        preview += "\nFinal total 50016 API cents. Purchase these carts?"
        prepared = service.prepare_confirm(**ARGS)
        prepared["question"] = preview
        service.prepare_confirm = lambda *args, **kwargs: prepared
        rig = Rig(FakeApi(), FakeCreate(), agentcard=service)
        spoken: list[str] = []
        rig.inlet._call_say = spoken.append
        telegram._FROM_CALL.set(spoken.append)  # the call's own work: only that is read on it (CallScope.lean)
        task = asyncio.create_task(rig.inlet._tool("agentcard_confirm", ARGS, OWNER))
        await until(lambda: bool(rig.api.keyboards))
        rendered = " ".join(sentence for piece in spoken
                            for sentence in phone_call.sentences(phone_call.speakable(piece)))
        assert rendered == " ".join(telegram.spoken_question(preview, telegram.YES_NO).split())
        assert "The rest is in the chat" not in rendered
        assert service.confirmed == []
        rig.inlet.hear("no")
        assert not (await task)["ok"]
        await clean(rig.inlet)
    asyncio.run(run())


def test_optional_registration_dispatch_and_no_executor_fallback() -> None:
    async def run() -> None:
        off = Rig(FakeApi(), FakeCreate())
        parts, allowed, _ = await off.inlet._turn_parts([])
        assert not set(agentcard.TOOL_NAMES) & allowed
        assert not set(agentcard.TOOL_NAMES) & {t["name"] for t in parts["watch_tools"]}
        assert "AgentCard" not in parts["watch_block"]
        for name in agentcard.TOOL_NAMES:
            result = await off.inlet._tool(name, {}, OWNER)
            assert result == {"ok": False, "reason": agentcard.OFF_REASON}
        assert off.create.requests == []
        service = Service()
        on = Rig(FakeApi(), FakeCreate(), agentcard=service)
        parts, allowed, _ = await on.inlet._turn_parts([])
        assert set(agentcard.TOOL_NAMES) <= allowed
        assert set(agentcard.TOOL_NAMES) <= {t["name"] for t in parts["watch_tools"]}
        assert "agentcard_confirm" in parts["watch_block"]
        result = await on.inlet._tool("agentcard_buy", {"ask": "paper", "conversation_id": None}, OWNER)
        assert result["reply"] == "SAFE_CART_REPLY" and len(service.reads) == 1
        assert not (await on.inlet._tool("agentcard_buy", {}, STRANGER))["ok"]
        assert len(service.reads) == 1
        await clean(off.inlet)
        await clean(on.inlet)
    asyncio.run(run())


def test_purchase_preview_is_firm_spoken_and_only_safe_about_enters_model_history(tmp_path: Path) -> None:
    async def run() -> None:
        service = Service()
        api, create = FakeApi(), FakeCreate(call("agentcard_confirm", ARGS), say("Placed safe-order."))
        rig = Rig(api, create, agentcard=service, memory=FakeMemory(tmp_path))
        spoken: list[str] = []
        rig.inlet._call_say = spoken.append
        telegram._FROM_CALL.set(spoken.append)  # the call's own work: only that is read on it (CallScope.lean)
        turn = asyncio.create_task(rig.inlet._turn(telegram.Inbound(OWNER, OWNER, "buy paper", message_id=1)))
        await until(lambda: bool(api.keyboards))
        asked = rig.inlet._live_question()
        assert asked.firm and asked.words == frozenset({"yes", "no"})
        assert "PRIVATE_PREVIEW" in " ".join(spoken)
        assert service.confirmed == []
        rig.inlet._dispatch(update("yes", uid=STRANGER))
        rig.inlet.hear("yes, but buy the cheaper one")
        await asyncio.sleep(0.01)
        assert service.confirmed == [] and not asked.future.done()
        assert len(create.requests) == 1  # firm approval never calls the model to judge an answer
        rig.inlet.hear("yes")
        await asyncio.wait_for(turn, 3)
        assert len(service.confirmed) == 1
        mid, key = api.key("Yes")
        rig.inlet._dispatch(tap_update(key, message_id=mid))
        await asyncio.sleep(0.01)
        assert len(service.confirmed) == 1
        model_input = json.dumps(create.requests)
        history = json.dumps(rig.inlet.turns)
        assert "PRIVATE_PREVIEW" not in model_input + history
        assert "safe-order" in model_input and "$5.00" in history
        saved = "\n".join(path.read_text() for path in (tmp_path / "transcripts").glob("*.jsonl"))
        assert "PRIVATE_PREVIEW" not in saved
        assert "safe-order" in saved and "$5.00" in saved
        await clean(rig.inlet)
    asyncio.run(run())


@pytest.mark.parametrize("answer,spoken", [("no", False), ("stop", False), ("stop", True)])
def test_denial_or_stop_withdraws_purchase(answer: str, spoken: bool) -> None:
    async def run() -> None:
        service, api = Service(), FakeApi()
        rig = Rig(api, FakeCreate(), agentcard=service)
        job = asyncio.create_task(rig.inlet._tool("agentcard_confirm", dict(ARGS), OWNER))
        await until(lambda: bool(api.keyboards))
        if spoken:
            rig.inlet.hear(answer)
        else:
            rig.inlet._handle(telegram.Inbound(OWNER, OWNER, answer))
        result = await asyncio.wait_for(job, 3)
        assert not result["ok"] and service.confirmed == []
        assert rig.inlet._live_question() is None
        await clean(rig.inlet)
    asyncio.run(run())


@pytest.mark.parametrize("spoken", [False, True])
@pytest.mark.parametrize("relay_listing", [False, True])
def test_stop_after_yes_before_dispatch_also_withdraws_queued_confirmations(spoken: bool,
                                                                         relay_listing: bool) -> None:
    async def run() -> None:
        service, api = Service(), FakeApi()
        rig = Rig(api, FakeCreate(), agentcard=service)
        first = asyncio.create_task(rig.inlet._tool("agentcard_confirm", dict(ARGS), OWNER))
        await until(lambda: bool(api.keyboards))
        second = asyncio.create_task(rig.inlet._tool("agentcard_confirm", dict(ARGS), OWNER))
        await asyncio.sleep(0)
        if relay_listing:
            rig.inlet._claude_on_job = asyncio.get_running_loop().create_future()
        # A tap resolves its exact future even while a slow relay listing holds typed messages.
        mid, key = api.key("Yes")
        rig.inlet._dispatch(tap_update(key, message_id=mid))
        if spoken:
            rig.inlet.hear("stop")
        else:
            rig.inlet._handle(telegram.Inbound(OWNER, OWNER, "stop"))
        assert not (await asyncio.wait_for(first, 3))["ok"]
        assert not (await asyncio.wait_for(second, 3))["ok"]
        assert not service.confirmed and len(api.keyboards) == 1
        assert not rig.inlet._held
        if relay_listing:
            rig.inlet._claude_on_job.cancel()
        await clean(rig.inlet)
    asyncio.run(run())


def test_button_send_failure_stays_firm_and_shutdown_withdraws_approval() -> None:
    async def run() -> None:
        service, api = Service(), FakeApi()
        async def broken(*args: Any, **kwargs: Any) -> int:
            raise telegram.BotApiError(400, "mock button refusal")
        api.send_inline = broken
        rig = Rig(api, FakeCreate(), agentcard=service)
        job = asyncio.create_task(rig.inlet._tool("agentcard_confirm", dict(ARGS), OWNER))
        await until(lambda: bool(api.sent))
        assert rig.inlet._live_question().firm
        assert not rig.inlet._answers_pending("yes, also change the total")
        await clean(rig.inlet)
        result = await asyncio.wait_for(job, 3)
        assert not result["ok"] and not service.confirmed
    asyncio.run(run())


def test_other_owner_chat_cannot_answer_and_keyboard_is_single_use() -> None:
    async def run() -> None:
        service, api = Service(), FakeApi()
        rig = Rig(api, FakeCreate(), dataclasses.replace(CFG, owner_ids=frozenset({OWNER, 7})), agentcard=service)
        job = asyncio.create_task(rig.inlet._tool("agentcard_confirm", dict(ARGS), OWNER))
        await until(lambda: bool(api.keyboards))
        rig.inlet._dispatch(update("yes", uid=7))
        await asyncio.sleep(0.01)
        assert not service.confirmed
        mid, key = api.key("Yes")
        rig.inlet._dispatch(tap_update(key, message_id=mid))
        rig.inlet._dispatch(tap_update(key, message_id=mid))
        result = await asyncio.wait_for(job, 3)
        assert result["status"] == "order_placed" and len(service.confirmed) == 1
        await clean(rig.inlet)
    asyncio.run(run())


@pytest.mark.parametrize("delayed_judge", [False, True])
def test_spoken_answer_cannot_move_to_a_new_question(delayed_judge: bool) -> None:
    async def run() -> None:
        rig = Rig(FakeApi(), FakeCreate())
        loop = asyncio.get_running_loop()
        old = rig.inlet._open_question(loop.create_future(), OWNER, strict=True,
                                      words=frozenset({"yes", "no"}), firm=not delayed_judge)
        judgement = loop.create_future()
        entered = asyncio.Event()
        async def judge(*args: Any) -> str:
            entered.set()
            return await judgement
        rig.inlet._judge_answer = judge
        rig.inlet.hear("please do" if delayed_judge else "yes")
        if delayed_judge:
            await entered.wait()
        new = rig.inlet._open_question(loop.create_future(), OWNER, strict=True,
                                      words=frozenset({"yes", "no"}), firm=True)
        if delayed_judge:
            judgement.set_result("yes")
        await asyncio.sleep(0.01)
        assert not old.future.done() and not new.future.done()
        rig.inlet.hear("yes")  # control: a new answer to the current question still works
        await until(new.future.done)
        assert new.future.result() == "yes" and not old.future.done()
        await clean(rig.inlet)
    asyncio.run(run())


def test_cache_warm_and_rundown_never_execute_a_purchase() -> None:
    async def run() -> None:
        service = Service()
        rig = Rig(FakeApi(), FakeCreate(call("agentcard_confirm", ARGS), say("Your day.")), agentcard=service)
        await rig.inlet._warm()
        assert not service.confirmed and not service.reads
        await rig.inlet._turn(telegram.Inbound(OWNER, OWNER, "rundown", message_id=2))
        assert not set(agentcard.TOOL_NAMES) & {t.get("name") for t in rig.create.requests[-1]["tools"]}
        assert not service.confirmed and not service.reads
        await clean(rig.inlet)
    asyncio.run(run())


def test_real_client_through_chat_waits_for_owner_then_sends_exact_vault_request(tmp_path: Path) -> None:
    """Exercise actual client -> gateway -> question -> POST, with a fully local HTTP double."""
    token = "PRIVATE_AGENTCARD_ACCESS_TOKEN_SENTINEL"
    address = {"street": "123 PrivateAddressSentinel Road", "city": "PrivateCitySentinel",
               "state": "CA", "zip": "94107", "country": "US", "phone": "+15555550123"}
    cid, cart_hash = "conv_cafe", "cafecafecafecafe"
    cart = {"merchant": "retail", "merchant_name": "Retail", "hash": cart_hash,
            "items": [{"name": "Printer paper", "qty": 1, "priceCents": 500, "product_id": "paper-ca"}],
            "serviceFeesCents": 20, "tipCents": 0, "totalCents": 520}
    requests: list[tuple[str, dict[str, Any] | None]] = []
    def request(method: str, url: str, **kwargs: Any) -> Any:
        assert url.startswith(agentcard.API_ORIGIN + "/buy")
        assert kwargs["headers"]["Authorization"] == "Bearer " + token
        payload = kwargs.get("json")
        requests.append((method, copy.deepcopy(payload)))
        if method == "GET":
            body = {"conversation_id": cid, "status": "open", "turn_in_progress": False,
                    "orders": [], "last_checkout": None, "carts": [cart], "unmatched": []}
        elif "ask" in payload:
            body = {"conversation_id": cid, "status": "needs_input", "cart": cart,
                    "carts": [cart], "unmatched": [], "catalog": None,
                    "reply": "Paper is ready for " + address["street"] + "; " + token}
        else:
            body = {"conversation_id": cid, "status": "order_placed", "order_id": "order-safe",
                    "charge_status": "settled", "carts": [], "cart": None, "unmatched": [],
                    "placements": None, "payment_source": {"brand": "visa", "last4": "4832"},
                    "reply": "Charged Visa ending 4832; " + address["phone"] + "; " + token}
        return SimpleNamespace(status_code=200, json=lambda: copy.deepcopy(body))

    async def run() -> None:
        client = agentcard.AgentCard(agentcard.AgentCardConfig(enabled=True, token=token,
                                                              address_json=json.dumps(address)),
                                     request=request, state_dir=tmp_path / "state")
        api = FakeApi()
        rig = Rig(api, FakeCreate(), agentcard=client, memory=FakeMemory(tmp_path / "memory"))
        built = await rig.inlet._tool("agentcard_buy", {"ask": "printer paper", "conversation_id": None}, OWNER)
        assert built["ok"] and built["conversation_id"] == cid and built["carts"][0]["hash"] == cart_hash
        task = asyncio.create_task(rig.inlet._tool("agentcard_confirm",
                                                   {"conversation_id": cid, "cart_hashes": [cart_hash]}, OWNER))
        await until(lambda: bool(api.keyboards))
        assert not any(payload and "confirm" in payload for _, payload in requests)
        preview = api.sent[-1][1]
        assert "Printer paper" in preview and "520" in preview
        mid, key = api.key("Yes")
        rig.inlet._dispatch(tap_update(key, message_id=mid))
        result = await asyncio.wait_for(task, 3)
        assert result["status"] == "order_placed" and result["order_id"] == "order-safe"
        confirms = [payload for _, payload in requests if payload and "confirm" in payload]
        assert confirms == [{"conversation_id": cid, "confirm": cart_hash, "payment_source": "vault",
                             "delivery_address": address}]
        assert requests[0][1]["delivery_address"] == address
        projection = json.dumps(built) + json.dumps(result) + json.dumps(rig.inlet.turns)
        disk = "\n".join(path.read_text() for path in tmp_path.rglob("*.json*"))
        for secret in (token, address["street"], address["phone"], "4832"):
            assert secret not in projection + disk
        assert "order-safe" in projection and "order-safe" in disk
        await clean(rig.inlet)
        client.close()
    asyncio.run(run())


def test_daemon_prerequisites_factory_and_safe_self_context(monkeypatch: pytest.MonkeyPatch) -> None:
    env = {"EVE_TELEGRAM": "1", "EVE_TELEGRAM_TOKEN": "mock-telegram-token",
           "EVE_TELEGRAM_OWNER": str(OWNER), "EVE_WATCH": "0", "EVE_COMPOSIO": "0", "EVE_APPS": "0",
           "EVE_CHIEF": "off", "EVE_AGENTCARD": "1", "AGENTCARD_ACCESS_TOKEN": "SECRET_TOKEN"}
    for key, value in env.items():
        monkeypatch.setenv(key, value)
    monkeypatch.delenv("OPENROUTER_API_KEY", raising=False)
    built: list[Service] = []
    def factory() -> Service:
        service = Service()
        built.append(service)
        return service
    monkeypatch.setattr(agentcard, "make_agentcard", factory)
    assert Daemon._make_telegram(_bare_daemon()) is None and not built
    monkeypatch.setenv("OPENROUTER_API_KEY", "mock-openrouter")
    monkeypatch.setattr(Daemon, "_make_chief", lambda *args: None)
    lent: list[Any] = []
    def inlet_factory(config: Any, **kwargs: Any) -> Any:
        lent.append(kwargs)
        return Rig(FakeApi(), FakeCreate(), config, agentcard=kwargs["agentcard"]).inlet
    monkeypatch.setattr(telegram, "make_inlet", inlet_factory)
    daemon = _bare_daemon()
    inlet = Daemon._make_telegram(daemon)
    assert inlet._agentcard is built[0] and lent[0]["agentcard"] is built[0]
    monkeypatch.setattr(telegram, "make_inlet", lambda *args, **kwargs: None)
    failed = _bare_daemon()
    assert Daemon._make_telegram(failed) is None and built[-1].closed and failed._agentcard is None
    base = self_context.lines({**env, "OPENROUTER_API_KEY": "mock-openrouter", "EVE_AGENTCARD": "0"}, commit="")
    on = self_context.lines({**env, "OPENROUTER_API_KEY": "mock-openrouter"}, commit="")
    assert not any("AgentCard" in line for line in base)
    assert any("AgentCard" in line for line in on)
    assert "SECRET_TOKEN" not in "\n".join(on)
    invalid = self_context.lines({**env, "OPENROUTER_API_KEY": "mock-openrouter",
                                  "EVE_AGENTCARD_TIMEOUT_SECS": "bad"}, commit="")
    assert not any("AgentCard" in line for line in invalid)
    assert any("Telegram brain" in line for line in invalid)
    invalid_address = self_context.lines({**env, "OPENROUTER_API_KEY": "mock-openrouter",
                                          "EVE_AGENTCARD_ADDRESS_FILE": "/nonexistent/agentcard-address.json"},
                                         commit="")
    assert not any("AgentCard" in line for line in invalid_address)
    assert any("Telegram brain" in line for line in invalid_address)


@pytest.mark.parametrize("approve", [False, True])
def test_tainted_chat_purchase_has_only_the_final_cart_approval(approve: bool) -> None:
    async def run() -> None:
        service = Service()
        rig = Rig(FakeApi(), FakeCreate(call("agentcard_confirm", ARGS), say("Handled.")), agentcard=service)
        task = asyncio.create_task(rig.inlet._think([telegram.message_item("user", "buy paper")], OWNER,
                                                   taint="a web search"))
        await until(lambda: rig.inlet._live_question() is not None)
        asked = rig.inlet._live_question()
        assert asked.firm and "PRIVATE_PREVIEW" in asked.text
        assert service.confirmed == []
        rig.inlet.hear("yes" if approve else "no")
        await task
        assert len(service.confirmed) == int(approve)
        assert len([t for t in rig.api.titled if "PRIVATE_PREVIEW" in t[2]]) == 1
        assert not any(t[0] == telegram.GATE_TITLE for t in rig.api.titled)
        await clean(rig.inlet)
    asyncio.run(run())
