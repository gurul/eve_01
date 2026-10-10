"""Offline approval lifecycle: real workers, explicit barriers and no HTTP client."""

from __future__ import annotations

import asyncio
import threading
from dataclasses import dataclass
from typing import Any

import pytest

from eve_bridge.agentcard_tools import AgentCardTools

OWNER, OTHER_OWNER = 101, 202
HASH = "0123456789abcdef"
OTHER_HASH = "fedcba9876543210"
ARGS = {"conversation_id": "conversation-1", "cart_hashes": [HASH]}


@dataclass(frozen=True)
class Snapshot:
    conversation: str
    hashes: tuple[str, ...]


class Service:
    """Confirmation checks Stop before submitting, then retains any real outcome."""

    def __init__(self) -> None:
        self.handled: list[tuple[str, dict[str, Any], int]] = []
        self.prepared: list[Snapshot] = []
        self.confirmed: list[tuple[Snapshot, threading.Event]] = []
        self.submitted: list[Snapshot] = []
        self.prepare_entered = threading.Event()
        self.confirm_entered = threading.Event()
        self.submit_entered = threading.Event()
        self.prepare_release = threading.Event()
        self.dispatch_release = threading.Event()
        self.response_release = threading.Event()
        for event in (self.prepare_release, self.dispatch_release, self.response_release):
            event.set()
        self.prepare_result: dict[str, Any] | None = None
        self.confirm_error: Exception | None = None

    def handle(self, name: str, args: dict[str, Any]) -> dict[str, Any]:
        self.handled.append((name, args, threading.get_ident()))
        return {"ok": True, "status": "needs_input"}

    def prepare_confirm(self, conversation_id: str, cart_hashes: list[str]) -> dict[str, Any]:
        snapshot = Snapshot(conversation_id, tuple(cart_hashes))
        self.prepared.append(snapshot)
        self.prepare_entered.set()
        assert self.prepare_release.wait(3), "test did not release preparation"
        if self.prepare_result is not None:
            return self.prepare_result
        return {"ok": True, "snapshot": snapshot, "question": "Buy one item for USD 10.00?",
                "about": "Purchase approval for one item, USD 10.00."}

    def confirm(self, snapshot: Snapshot, cancelled: threading.Event) -> dict[str, Any]:
        self.confirmed.append((snapshot, cancelled))
        self.confirm_entered.set()
        assert self.dispatch_release.wait(3), "test did not release dispatch"
        if cancelled.is_set():
            return {"ok": False, "status": "not_submitted"}
        self.submitted.append(snapshot)
        self.submit_entered.set()
        assert self.response_release.wait(3), "test did not release response"
        if self.confirm_error is not None:
            raise self.confirm_error
        return {"ok": True, "status": "order_placed", "order_id": "order-1"}

    def release(self) -> None:
        for event in (self.prepare_release, self.dispatch_release, self.response_release):
            event.set()


class Question:
    def __init__(self) -> None:
        self.calls: list[tuple[str, str]] = []
        self.entered = asyncio.Event()
        self.closed = asyncio.Event()
        self.answer: asyncio.Future[str] = asyncio.get_running_loop().create_future()

    async def __call__(self, question: str, about: str) -> str:
        self.calls.append((question, about))
        self.entered.set()
        try:
            return await self.answer
        finally:
            self.closed.set()


async def yes(question: str, about: str) -> str:
    assert "USD 10.00" in question and "USD 10.00" in about
    return "yes"


async def wait_thread(event: threading.Event) -> None:
    assert await asyncio.to_thread(event.wait, 2), "worker did not reach its test barrier"


@pytest.mark.parametrize("name", ["agentcard_merchants", "agentcard_buy", "agentcard_status"])
def test_nonconfirm_dispatch_is_threaded_and_never_asks(name):
    async def go():
        service = Service()
        gateway = AgentCardTools(service)
        question = Question()
        caller = threading.get_ident()
        out = await gateway.handle(name, {"ask": "one item"}, OWNER, question)
        assert out == {"ok": True, "status": "needs_input"}
        assert service.handled == [(name, {"ask": "one item"}, service.handled[0][2])]
        assert service.handled[0][2] != caller
        assert question.calls == [] and service.prepared == [] and service.confirmed == []

    asyncio.run(go())


@pytest.mark.parametrize("answer", ["yes", "ok", "go ahead please", "yes, thank you"])
def test_only_one_exact_prepared_snapshot_reaches_confirmation(answer):
    async def go():
        service = Service()
        gateway, question = AgentCardTools(service), Question()
        task = asyncio.create_task(gateway.handle("agentcard_confirm", ARGS, OWNER, question))
        await question.entered.wait()
        assert service.confirmed == [] and service.submitted == []
        assert question.calls == [("Buy one item for USD 10.00?", "Purchase approval for one item, USD 10.00.")]
        question.answer.set_result(answer)
        out = await task
        assert out == {"ok": True, "status": "order_placed", "order_id": "order-1"}
        assert len(service.confirmed) == len(service.submitted) == 1
        assert service.confirmed[0][0] is service.prepared[0]
        assert not service.confirmed[0][1].is_set() and not gateway.active()
        assert "snapshot" not in out and "question" not in out

    asyncio.run(go())


@pytest.mark.parametrize("answer", ["no", "", "maybe", "yeah no", "yes but cheaper", "ok wait",
                                   "ok also update the README", "sure go for it I guess", "no (no answer within 180 seconds)"])
def test_ambiguous_denied_or_timed_out_answers_cannot_confirm(answer):
    async def go():
        service, question = Service(), Question()
        gateway = AgentCardTools(service)
        task = asyncio.create_task(gateway.handle("agentcard_confirm", ARGS, OWNER, question))
        await question.entered.wait()
        question.answer.set_result(answer)
        assert (await task)["status"] == "not_submitted"
        assert len(service.prepared) == 1 and service.confirmed == [] and service.submitted == []

    asyncio.run(go())


@pytest.mark.parametrize("error", [TimeoutError(), RuntimeError("private-provider-token")])
def test_failed_owner_question_does_not_confirm_or_expose_the_error(error, caplog):
    async def ask(question, about):
        raise error

    async def go():
        service = Service()
        out = await AgentCardTools(service).handle("agentcard_confirm", ARGS, OWNER, ask)
        assert out["ok"] is False and out["status"] == "not_submitted"
        assert "private-provider-token" not in str(out) + caplog.text
        assert service.confirmed == []

    asyncio.run(go())


@pytest.mark.parametrize("prepared", [{"ok": False, "status": "needs_input"}, {"ok": True},
                                     {"ok": True, "snapshot": Snapshot("x", (HASH,)), "question": "", "about": "safe"}])
def test_unready_or_malformed_preparation_cannot_ask_or_confirm(prepared):
    async def go():
        service, question = Service(), Question()
        service.prepare_result = prepared
        out = await AgentCardTools(service).handle("agentcard_confirm", ARGS, OWNER, question)
        assert out["ok"] is False and question.calls == [] and service.confirmed == []

    asyncio.run(go())


@pytest.mark.parametrize("args", [[], {}, {"conversation_id": "x", "cart_hashes": []},
                                 {"conversation_id": "", "cart_hashes": [HASH]},
                                 {"conversation_id": "x", "cart_hashes": [HASH.upper()]},
                                 {"conversation_id": "x", "cart_hashes": [HASH, HASH]},
                                 {"conversation_id": "x", "cart_hashes": [HASH] * 9},
                                 {**ARGS, "approved": True}, {"conversation_id": "x", "cart_hashes": [1]}])
def test_model_arguments_never_supply_approval_or_an_invalid_selection(args):
    async def go():
        service, question = Service(), Question()
        out = await AgentCardTools(service).handle("agentcard_confirm", args, OWNER, question)
        assert out["status"] == "not_submitted"
        assert service.prepared == [] and service.confirmed == [] and question.calls == []

    asyncio.run(go())


def test_argument_mutation_while_waiting_cannot_change_the_approved_snapshot():
    async def go():
        service, question = Service(), Question()
        args = {"conversation_id": "original", "cart_hashes": [HASH]}
        gateway = AgentCardTools(service)
        task = asyncio.create_task(gateway.handle("agentcard_confirm", args, OWNER, question))
        await question.entered.wait()
        args["conversation_id"] = "replacement"
        args["cart_hashes"][0] = OTHER_HASH
        question.answer.set_result("yes")
        assert (await task)["ok"]
        assert service.submitted == [Snapshot("original", (HASH,))]

    asyncio.run(go())


def test_confirmations_are_serialized_and_cross_chat_stop_only_invalidates_its_queue():
    async def go():
        service, question = Service(), Question()
        gateway = AgentCardTools(service)
        first = asyncio.create_task(gateway.handle("agentcard_confirm", ARGS, OWNER, question))
        await question.entered.wait()
        second = asyncio.create_task(gateway.handle("agentcard_confirm", ARGS, OTHER_OWNER, yes))
        await asyncio.sleep(0)
        assert len(service.prepared) == 1 and gateway.active(OTHER_OWNER)
        assert gateway.stop(OTHER_OWNER)["stopped"] == 1
        assert not question.answer.done() and service.confirmed == []
        question.answer.set_result("yes")
        assert (await first)["ok"]
        assert (await second)["status"] == "not_submitted"
        assert len(service.prepared) == len(service.confirmed) == 1
        assert not gateway.active()

    asyncio.run(go())


def test_stop_withdraws_the_owner_prompt_and_every_queued_confirmation():
    async def go():
        service, question = Service(), Question()
        gateway = AgentCardTools(service)
        first = asyncio.create_task(gateway.handle("agentcard_confirm", ARGS, OWNER, question))
        await question.entered.wait()
        second = asyncio.create_task(gateway.handle("agentcard_confirm", ARGS, OWNER, yes))
        await asyncio.sleep(0)
        stopped = gateway.stop(OWNER)
        assert stopped["stopped"] == 2 and stopped["in_flight"] is False
        assert (await first)["status"] == (await second)["status"] == "not_submitted"
        assert question.closed.is_set() and question.answer.cancelled()
        assert len(service.prepared) == 1 and service.confirmed == []
        assert not gateway.active()

    asyncio.run(go())


def test_a_question_that_suppresses_cancellation_cannot_return_a_late_yes_to_purchase():
    async def go():
        service = Service()
        entered = asyncio.Event()
        gateway = AgentCardTools(service)

        async def stubborn_question(question, about):
            entered.set()
            try:
                await asyncio.Future()
            except asyncio.CancelledError:
                return "yes"

        task = asyncio.create_task(gateway.handle("agentcard_confirm", ARGS, OWNER, stubborn_question))
        await entered.wait()
        assert gateway.stop(OWNER)["stopped"] == 1
        assert (await task)["status"] == "not_submitted"
        assert service.confirmed == [] and not gateway.active()

    asyncio.run(go())


def test_stop_during_preparation_prevents_the_late_prompt_and_submission():
    async def go():
        service, question = Service(), Question()
        service.prepare_release.clear()
        gateway = AgentCardTools(service)
        task = asyncio.create_task(gateway.handle("agentcard_confirm", ARGS, OWNER, question))
        try:
            await wait_thread(service.prepare_entered)
            assert gateway.stop(OWNER)["stopped"] == 1
            service.prepare_release.set()
            assert (await task)["status"] == "not_submitted"
            assert question.calls == [] and service.confirmed == []
        finally:
            service.release()
            await gateway.wait_idle()

    asyncio.run(go())


def test_stop_immediately_after_owner_answer_is_checked_before_confirm_dispatch():
    async def go():
        service = Service()
        gateway = AgentCardTools(service)

        async def answer(question, about):
            gateway.stop(OWNER)
            return "yes"

        out = await gateway.handle("agentcard_confirm", ARGS, OWNER, answer)
        assert out["status"] == "not_submitted" and service.confirmed == []

    asyncio.run(go())


def test_stop_signals_a_confirm_worker_before_it_submits():
    async def go():
        service = Service()
        service.dispatch_release.clear()
        gateway = AgentCardTools(service)
        task = asyncio.create_task(gateway.handle("agentcard_confirm", ARGS, OWNER, yes))
        try:
            await wait_thread(service.confirm_entered)
            out = gateway.stop(OWNER)
            assert out["in_flight"] is True and out["stopped"] == 0
            assert service.confirmed[0][1].is_set()
            service.dispatch_release.set()
            assert (await task)["status"] == "not_submitted" and service.submitted == []
        finally:
            service.release()
            await gateway.wait_idle()

    asyncio.run(go())


def test_stop_after_dispatch_reports_uncertainty_and_preserves_the_real_order_result():
    async def go():
        service = Service()
        service.response_release.clear()
        gateway = AgentCardTools(service)
        task = asyncio.create_task(gateway.handle("agentcard_confirm", ARGS, OWNER, yes))
        try:
            await wait_thread(service.submit_entered)
            out = gateway.stop(OWNER)
            assert out["stopped"] == 0 and out["in_flight"] is True
            assert "cannot undo" in out["line"] and "check" in out["line"]
            assert "purchase was cancelled" not in out["line"]
            assert gateway.active(OWNER) and not gateway.active(OTHER_OWNER)
            service.response_release.set()
            assert (await task)["status"] == "order_placed"
            assert len(service.submitted) == len(service.confirmed) == 1
            assert not gateway.active()
        finally:
            service.release()
            await gateway.wait_idle()

    asyncio.run(go())


def test_caller_cancellation_withdraws_approval_and_unlocks_the_next_request():
    async def go():
        service, question = Service(), Question()
        gateway = AgentCardTools(service)
        task = asyncio.create_task(gateway.handle("agentcard_confirm", ARGS, OWNER, question))
        await question.entered.wait()
        task.cancel()
        with pytest.raises(asyncio.CancelledError):
            await task
        await gateway.wait_idle()
        assert question.closed.is_set() and service.confirmed == []
        assert (await gateway.handle("agentcard_confirm", ARGS, OWNER, yes))["ok"]
        assert len(service.confirmed) == 1

    asyncio.run(go())


def test_caller_cancellation_during_preparation_retains_serialization_until_worker_finishes():
    async def go():
        service = Service()
        service.prepare_release.clear()
        gateway = AgentCardTools(service)
        first = asyncio.create_task(gateway.handle("agentcard_confirm", ARGS, OWNER, yes))
        try:
            await wait_thread(service.prepare_entered)
            first.cancel()
            with pytest.raises(asyncio.CancelledError):
                await first
            second = asyncio.create_task(gateway.handle("agentcard_confirm", ARGS, OWNER, yes))
            await asyncio.sleep(0)
            assert len(service.prepared) == 1
            service.prepare_release.set()
            assert (await second)["ok"]
            assert len(service.prepared) == 2 and len(service.submitted) == 1
        finally:
            service.release()
            await gateway.wait_idle()

    asyncio.run(go())


def test_cancelled_queued_request_never_prepares_after_the_first_confirmation():
    async def go():
        service, question = Service(), Question()
        gateway = AgentCardTools(service)
        first = asyncio.create_task(gateway.handle("agentcard_confirm", ARGS, OWNER, question))
        await question.entered.wait()
        queued = asyncio.create_task(gateway.handle("agentcard_confirm", ARGS, OWNER, yes))
        await asyncio.sleep(0)
        queued.cancel()
        with pytest.raises(asyncio.CancelledError):
            await queued
        question.answer.set_result("yes")
        assert (await first)["ok"]
        await gateway.wait_idle()
        assert len(service.prepared) == len(service.submitted) == 1

    asyncio.run(go())


def test_inflight_caller_cancellation_retains_worker_and_blocks_then_invalidates_queued_request():
    async def go():
        service = Service()
        service.response_release.clear()
        gateway = AgentCardTools(service)
        first = asyncio.create_task(gateway.handle("agentcard_confirm", ARGS, OWNER, yes))
        try:
            await wait_thread(service.submit_entered)
            first.cancel()
            with pytest.raises(asyncio.CancelledError):
                await first
            queued = asyncio.create_task(gateway.handle("agentcard_confirm", ARGS, OTHER_OWNER, yes))
            await asyncio.sleep(0)
            assert gateway.active(OWNER) and len(service.prepared) == 1
            stopped = gateway.stop()
            assert stopped["in_flight"] is True and stopped["stopped"] == 1
            service.response_release.set()
            assert (await queued)["status"] == "not_submitted"
            await gateway.wait_idle()
            assert len(service.confirmed) == len(service.submitted) == 1 and not gateway.active()
        finally:
            service.release()
            await gateway.wait_idle()

    asyncio.run(go())


def test_shutdown_stops_pending_work_and_closes_admission_for_every_tool():
    async def go():
        service, question = Service(), Question()
        gateway = AgentCardTools(service)
        first = asyncio.create_task(gateway.handle("agentcard_confirm", ARGS, OWNER, question))
        await question.entered.wait()
        assert gateway.shutdown()["stopped"] == 1
        assert (await first)["status"] == "not_submitted"
        for name in ("agentcard_buy", "agentcard_status", "agentcard_merchants", "agentcard_confirm"):
            assert (await gateway.handle(name, ARGS, OWNER, yes))["status"] == "not_submitted"
        assert service.handled == [] and service.confirmed == [] and not gateway.active()

    asyncio.run(go())


def test_unexpected_confirm_exception_is_unknown_without_retry_or_private_error(caplog):
    async def go():
        service = Service()
        service.confirm_error = RuntimeError("secret-payment-field")
        out = await AgentCardTools(service).handle("agentcard_confirm", ARGS, OWNER, yes)
        assert out["status"] == "unknown" and out["ok"] is False
        assert "status" in out["reason"] and "secret-payment-field" not in str(out) + caplog.text
        assert len(service.confirmed) == len(service.submitted) == 1

    asyncio.run(go())


def test_an_unusable_client_return_does_not_become_a_purchase_success():
    class Broken(Service):
        def handle(self, name, args):
            self.handled.append((name, args, threading.get_ident()))
            return None

        def confirm(self, snapshot, cancelled):
            super().confirm(snapshot, cancelled)
            return None

    async def go():
        service = Broken()
        gateway = AgentCardTools(service)
        out = await gateway.handle("agentcard_status", {"conversation_id": "x"}, OWNER, yes)
        assert out["ok"] is False
        out = await gateway.handle("agentcard_confirm", ARGS, OWNER, yes)
        assert out["ok"] is False and out["status"] == "unknown"
        assert len(service.handled) == len(service.confirmed) == len(service.submitted) == 1

    asyncio.run(go())


def test_unknown_tools_and_non_chat_principals_cannot_reach_the_client():
    async def go():
        service = Service()
        gateway = AgentCardTools(service)
        assert (await gateway.handle("some_other_tool", ARGS, OWNER, yes))["status"] == "not_submitted"
        for chat in (None, True, "101"):
            assert (await gateway.handle("agentcard_confirm", ARGS, chat, yes))["status"] == "not_submitted"
        assert service.prepared == [] and service.handled == [] and service.confirmed == []

    asyncio.run(go())
