"""telegram.py: the two counterexamples the Lean models found (verification/Eve/Questions.lean and
verification/Eve/TaskStop.lean), replayed against the real TelegramInlet. Each test is the trace from
that file's ``current_violates`` theorem; each failed on the code before its fix."""

from __future__ import annotations

import asyncio
from typing import Any, Optional

from test_telegram import OWNER, FakeApi, FakeCreate, Rig, call, run_rig, settle, update

from eve_bridge.telegram import ON_IT_LINE, PROGRESS_DONE_LINE, STOPPED_LINE


def test_a_typed_answer_reaches_the_outer_question_after_two_inner_ones_close() -> None:
    """Questions.lean: A, B and C open in that order; B's asker goes away, C is answered. A still waits,
    so the owner's next message must be A's answer. Before the fix C put back the slot it had saved (B,
    already closed), found it closed and emptied the slot: A's answer became a new chat turn instead."""
    api = FakeApi()
    rig = Rig(api, FakeCreate())

    async def during() -> None:
        inlet = rig.inlet
        a = asyncio.ensure_future(inlet._ask_user("Which folder, A?", OWNER))
        await settle()
        b = asyncio.ensure_future(inlet._ask_user("Which file, B?", OWNER))
        await settle()
        c = asyncio.ensure_future(inlet._ask_user("Which line, C?", OWNER))
        await settle()
        b.cancel()                                        # B's asker went away (a stopped task)
        await asyncio.gather(b, return_exceptions=True)
        await settle()
        api.feed(update("the third one", update_id=2))    # the slot is C: this is C's answer
        assert await asyncio.wait_for(c, 1) == "the third one"
        await settle()
        assert inlet._awaiting_answer, "A is still waiting, so a question must still be waiting"
        api.feed(update("the docs folder", update_id=3))  # A's answer
        await settle()
        assert a.done() and a.result() == "the docs folder"
        assert not inlet._awaiting_answer and inlet._pending_answer is None

    run_rig(rig, during)
    assert rig.create.requests == []                      # no answer became a chat turn


def test_a_stop_typed_after_the_task_finished_does_not_eat_its_result() -> None:
    """TaskStop.lean: the agent's run returns a result, and "stop" arrives while the progress message is
    being closed (the task is still not done). Before the fix the late stop said "Stopped." and the
    result was never sent; now the result arrives and nothing says the task stopped."""
    gate = asyncio.Event()

    class SlowCloseApi(FakeApi):
        async def edit_message(self, chat_id: int, message_id: int, text: str, title: Optional[str] = None,
                               subtitle: Optional[str] = None, keyboard: Any = None) -> None:
            if PROGRESS_DONE_LINE in text:
                await gate.wait()                         # the closing edit is on its way
            await super().edit_message(chat_id, message_id, text, title, subtitle, keyboard)

    api = SlowCloseApi([update("open the calculator", update_id=1)])
    rig = Rig(api, FakeCreate(call("start_task", {"goal": "open the calculator"})))

    async def during() -> None:
        assert rig.inlet.task_running
        rig.agents[0].release.set()                       # the run returns its result
        await settle()
        assert rig.inlet.task_running                     # still closing the progress message
        api.feed(update("stop", update_id=2))
        await settle()
        gate.set()
        await settle()

    run_rig(rig, during)
    assert api.sent[0] == (OWNER, ON_IT_LINE) and (OWNER, "Calculator is open.") in api.sent
    assert (OWNER, STOPPED_LINE) not in api.sent
    assert rig.agents[0].cancel_reason is None
