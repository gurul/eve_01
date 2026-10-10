"""Owner-scoped America menu commands, prompt isolation and provider lifecycle, offline."""

from __future__ import annotations

import asyncio
import copy
import dataclasses
import json
import time
from typing import Any

import pytest
from test_telegram import CFG, OWNER, STRANGER, FakeApi, FakeCreate, Rig, call, say, update

from eve_bridge import america, telegram

QUESTION = "Where can I read the United States Code? This is U.S. federal."
ARGS = {"question": "Where can I read the United States Code?", "jurisdiction": "U.S. federal"}


class Service:
    def __init__(self) -> None:
        self.calls: list[tuple[str, dict[str, Any]]] = []
        self.closed = 0

    def tools(self) -> list[dict[str, Any]]:
        return [copy.deepcopy(america.TOOL)]

    def instructions(self) -> str:
        return "Use america_ask for the explicit question and jurisdiction."

    async def handle(self, name: str, args: dict[str, Any]) -> dict[str, Any]:
        self.calls.append((name, copy.deepcopy(args)))
        return {"ok": True, "status": "answered", "provider": "america_gov", "answer": "Read the U.S. Code.",
                "sources": [{"title": "U.S. Code", "url": "https://uscode.house.gov/", "currency": "unverified"}],
                "america_url": america.AMERICA_URL, "legal_advice": False}

    async def close(self) -> None:
        self.closed += 1


async def until(predicate: Any) -> None:
    deadline = time.monotonic() + 3
    while not predicate():
        assert time.monotonic() < deadline, "America menu did not reach the expected state"
        await asyncio.sleep(0.001)


async def clean(inlet: Any) -> None:
    await inlet._shutdown()
    await asyncio.sleep(0)


def test_menu_adds_america_only_for_an_enabled_service_and_keeps_owner_scope() -> None:
    async def run() -> None:
        cfg = dataclasses.replace(CFG, owner_ids=frozenset({OWNER, 77}))
        off = Rig(FakeApi(), FakeCreate(), cfg)
        await off.inlet._set_commands()
        assert off.api.commands == [(telegram.BOT_COMMANDS, 77), (telegram.BOT_COMMANDS, OWNER)]
        assert telegram.AMERICA_MENU not in telegram.BOT_COMMANDS
        service = Service()
        on = Rig(FakeApi(), FakeCreate(), cfg, america=service)
        await on.inlet._set_commands()
        commands = telegram.BOT_COMMANDS[:-1] + (telegram.AMERICA_MENU, telegram.BOT_COMMANDS[-1])
        assert on.api.commands == [(commands, 77), (commands, OWNER)]
        assert len([item for item in commands if item[0] == "america"]) == 1
        assert on.create.requests == [] and service.calls == []
        await clean(off.inlet)
        await clean(on.inlet)
        assert service.closed == 1

    asyncio.run(run())


@pytest.mark.parametrize("relay", ["none", "claude", "codex"])
@pytest.mark.parametrize("bare", [False, True])
def test_bare_and_argument_commands_bypass_relays_launch_and_preserve_other_questions(relay: str, bare: bool) -> None:
    async def run() -> None:
        service = Service()
        rig = Rig(FakeApi(), FakeCreate(), america=service)
        inlet = rig.inlet
        inlet.claude = relay == "claude"
        inlet._codex_chat = OWNER if relay == "codex" else None
        turns: list[Any] = []

        async def turn(inbound: Any) -> None:
            turns.append(inbound)

        def launch(*args: Any) -> bool:
            raise AssertionError("America command or answer reached the launch picker")

        inlet._turn = turn
        inlet._launch_dispatch = launch
        prior = inlet._open_question(asyncio.get_running_loop().create_future(), OWNER,
                                     strict=True, words=frozenset({"yes", "no"}), firm=True)
        if bare:
            inlet._dispatch(update("/america@EveBot"))
            await until(lambda: OWNER in inlet._america_questions)
            asked = inlet._america_questions[OWNER]
            assert asked is inlet._live_question() and "state/city" in asked.text
            inlet._dispatch(update(QUESTION, update_id=2))
        else:
            inlet._dispatch(update("/AMERICA@EveBot " + QUESTION))
        await until(lambda: bool(turns) and not inlet._america_jobs)
        assert len(turns) == 1 and turns[0].text == "Search official U.S. sources via america_ask: " + QUESTION
        assert not prior.future.done() and inlet._pending_answer is prior.future
        assert not inlet._held and rig.create.requests == [] and service.calls == []
        prior.future.cancel()
        await clean(inlet)

    asyncio.run(run())


def test_argument_turn_actually_advertises_and_dispatches_america_tool() -> None:
    async def run() -> None:
        service = Service()
        create = FakeCreate(call("america_ask", ARGS), say("Read https://uscode.house.gov/"))
        rig = Rig(FakeApi(), create, america=service)
        rig.inlet._dispatch(update("/america " + QUESTION))
        await until(lambda: bool(rig.api.sent))
        assert service.calls == [("america_ask", ARGS)]
        assert america.TOOL in create.requests[0]["tools"]
        texts = [part["text"] for item in create.requests[0]["input"] if item.get("role") == "user"
                 for part in item["content"] if part.get("type") == "input_text"]
        assert texts[-1] == "Search official U.S. sources via america_ask: " + QUESTION
        output = next(item for item in create.requests[1]["input"] if item.get("type") == "function_call_output")
        assert json.loads(output["output"])["provider"] == "america_gov"
        assert rig.api.sent[-1] == (OWNER, "Read https://uscode.house.gov/")
        await clean(rig.inlet)

    asyncio.run(run())


def test_disabled_and_stranger_commands_do_not_call_a_model_or_service() -> None:
    async def run() -> None:
        off = Rig(FakeApi(), FakeCreate())
        off.inlet._dispatch(update("/america " + QUESTION))
        await until(lambda: bool(off.api.sent))
        assert america.OFF_REASON in off.api.sent[0][1] and america.AMERICA_URL in off.api.sent[0][1]
        assert off.create.requests == [] and off.inlet._america_jobs == {}
        service = Service()
        on = Rig(FakeApi(), FakeCreate(), america=service)
        on.inlet._dispatch(update("/america " + QUESTION, uid=STRANGER))
        await asyncio.sleep(0)
        assert not on.api.sent and not on.inlet._america_jobs
        on.inlet._handle(telegram.Inbound(STRANGER, STRANGER, "/america " + QUESTION))
        await until(lambda: bool(on.api.sent))
        assert "owner" in on.api.sent[0][1] and not service.calls and not on.create.requests
        await clean(off.inlet)
        await clean(on.inlet)

    asyncio.run(run())


@pytest.mark.parametrize("cancel", ["cancel", "/cancel", "stop", "/america cancel"])
def test_cancellation_closes_only_the_america_prompt(cancel: str) -> None:
    async def run() -> None:
        service = Service()
        rig = Rig(FakeApi(), FakeCreate(), america=service)
        prior = rig.inlet._open_question(asyncio.get_running_loop().create_future(), OWNER,
                                         strict=True, words=frozenset({"yes", "no"}), firm=True)
        rig.inlet._dispatch(update("/america"))
        await until(lambda: OWNER in rig.inlet._america_questions)
        asked = rig.inlet._america_questions[OWNER]
        rig.inlet._dispatch(update(cancel, update_id=2))
        await until(lambda: not rig.inlet._america_jobs and any("Cancelled" in text for _, text in rig.api.sent))
        assert asked.future.cancelled() and not prior.future.done()
        assert rig.inlet._pending_answer is prior.future
        assert not rig.create.requests and not service.calls
        assert any("Cancelled" in text for _, text in rig.api.sent)
        prior.future.cancel()
        await clean(rig.inlet)

    asyncio.run(run())


def test_repeated_waiting_command_replaces_the_prompt_without_submitting_twice() -> None:
    async def run() -> None:
        rig = Rig(FakeApi(), FakeCreate(), america=Service())
        turns: list[Any] = []

        async def turn(inbound: Any) -> None:
            turns.append(inbound)

        rig.inlet._turn = turn
        rig.inlet._dispatch(update("/america"))
        await until(lambda: OWNER in rig.inlet._america_questions)
        first = rig.inlet._america_questions[OWNER]
        rig.inlet._dispatch(update("/america", update_id=2))
        await until(lambda: OWNER in rig.inlet._america_questions and rig.inlet._america_questions[OWNER] is not first)
        second = rig.inlet._america_questions[OWNER]
        assert first.future.cancelled() and not second.future.done()
        rig.inlet._dispatch(update("/america " + QUESTION, update_id=3))
        await until(lambda: bool(turns) and not rig.inlet._america_jobs)
        assert second.future.cancelled() and len(turns) == 1
        assert not rig.inlet._america_questions and rig.inlet._live_question() is None
        await clean(rig.inlet)

    asyncio.run(run())


def test_running_command_is_not_resubmitted_and_can_be_cancelled() -> None:
    async def run() -> None:
        rig = Rig(FakeApi(), FakeCreate(), america=Service())
        turns: list[Any] = []
        running = asyncio.Event()

        async def turn(inbound: Any) -> None:
            turns.append(inbound)
            running.set()
            await asyncio.Event().wait()

        rig.inlet._turn = turn
        rig.inlet._dispatch(update("/america " + QUESTION))
        await running.wait()
        rig.inlet._dispatch(update("/america " + QUESTION, update_id=2))
        await until(lambda: bool(rig.api.sent))
        assert "already running" in rig.api.sent[-1][1] and len(turns) == 1
        rig.inlet._dispatch(update("/america cancel", update_id=3))
        await until(lambda: not rig.inlet._america_jobs)
        assert len(turns) == 1
        await clean(rig.inlet)

    asyncio.run(run())


def test_each_owner_can_answer_their_own_america_prompt() -> None:
    async def run() -> None:
        other = 77
        cfg = dataclasses.replace(CFG, owner_ids=frozenset({OWNER, other}))
        rig = Rig(FakeApi(), FakeCreate(), cfg, america=Service())
        turns: list[Any] = []

        async def turn(inbound: Any) -> None:
            turns.append(inbound)

        rig.inlet._turn = turn
        rig.inlet._dispatch(update("/america"))
        await until(lambda: OWNER in rig.inlet._america_questions)
        rig.inlet._dispatch(update("/america", uid=other, update_id=2))
        await until(lambda: other in rig.inlet._america_questions)
        other_prompt = rig.inlet._america_questions[other]
        rig.inlet._dispatch(update(QUESTION, update_id=3))
        await until(lambda: bool(turns))
        assert turns[0].chat_id == OWNER and not other_prompt.future.done()
        rig.inlet._dispatch(update("cancel", uid=other, update_id=4))
        await until(lambda: not rig.inlet._america_jobs)
        assert other_prompt.future.cancelled() and len(turns) == 1
        await clean(rig.inlet)

    asyncio.run(run())


def test_prompt_timeout_is_not_submitted_as_a_question() -> None:
    async def run() -> None:
        service = Service()
        rig = Rig(FakeApi(), FakeCreate(), dataclasses.replace(CFG, ask_timeout_secs=0.01), america=service)
        rig.inlet._dispatch(update("/america"))
        await until(lambda: bool(rig.api.sent) and not rig.inlet._america_jobs)
        assert not rig.create.requests and not service.calls and not rig.inlet._america_questions
        await clean(rig.inlet)

    asyncio.run(run())


def test_shutdown_cancels_prompt_and_closes_async_provider_once() -> None:
    async def run() -> None:
        service = Service()
        rig = Rig(FakeApi(), FakeCreate(), america=service)
        rig.inlet._dispatch(update("/america"))
        await until(lambda: OWNER in rig.inlet._america_questions)
        asked = rig.inlet._america_questions[OWNER]
        await clean(rig.inlet)
        await clean(rig.inlet)
        assert asked.future.cancelled() and not rig.inlet._america_jobs
        assert service.closed == 1 and not service.calls
        absent = Rig(FakeApi(), FakeCreate(), america=object())
        await clean(absent.inlet)  # old injected services without close remain valid

    asyncio.run(run())
