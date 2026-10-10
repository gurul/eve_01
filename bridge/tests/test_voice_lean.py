"""Replays of the counterexamples in verification/Eve/Voice.lean against the real VoiceSession.

A: a backend `error` event ends the delegated response, so the face must leave "thinking" and a tool
   result's deferred response.create must go out (Lean trace `[delegation, error]`).
B: when handing the question to the backend or the voice raises, `_ask_user` must still release its
   question slot (Lean `B_current_violates`: note fails, the slot keeps the future).

Fakes only: the scripted Live connection and fake speaker from test_voice_agent.py.
"""

from __future__ import annotations

import asyncio

import pytest
from test_voice_agent import (
    CAPTIONS,
    FakeAgent,
    FakeConnection,
    _captions_session,
    _delegated,
    _session,
    _tool_call,
)


def _backend_error() -> dict:
    return {"type": "response.event", "event": {"type": "error", "code": "server_error", "message": "boom",
                                                "sequence_number": 3}}


def test_backend_error_ends_the_response_and_the_face_settles() -> None:
    """Lean A_current_violates: [delegation, error] leaves the face on thinking with nothing in flight."""
    conn = FakeConnection([_delegated(), _backend_error()])
    clock = {"now": 0.0}
    s, states, _ = _captions_session(conn, clock)

    async def go():
        task = asyncio.create_task(s.run())
        await asyncio.sleep(0.05)                      # both events handled, several caption ticks
        assert "thinking" in states                    # the delegation did show thinking
        assert states[-1] == "listening"               # the face settles: nothing is in flight
        assert s._response_active is False             # the error ended the response
        conn.feed(_tool_call("end_conversation"), None)
        await task
    asyncio.run(go())


def test_backend_error_releases_a_deferred_response_create() -> None:
    """A tool result that waited for the in-flight response is answered once the error ends it."""
    conn = FakeConnection([_delegated(), _tool_call("task_status", "c1"), _backend_error()])
    s, _, _ = _session(conn, [FakeAgent(None, None)])

    async def go():
        task = asyncio.create_task(s.run())
        await asyncio.sleep(0.05)
        assert sum(1 for k, _ in conn.sent if k == "response.create") == 1
        conn.feed(_tool_call("end_conversation", "c2"), None)
        await task
    asyncio.run(go())


class _Boom(RuntimeError):
    pass


async def _raise(**_kw):
    raise _Boom("socket closed")


@pytest.mark.parametrize("where", ["backend_note", "speak"])
def test_ask_user_releases_the_question_when_asking_fails(where: str) -> None:
    """Lean B_current_violates: the note (or the spoken question) raises before the wait begins."""
    conn = FakeConnection()
    if where == "backend_note":
        conn.response.item.create = _raise            # response.item.create -> _backend_note
    else:
        conn.session.commentary.append = _raise       # session.commentary.append -> _speak
    s, _, _ = _session(conn, [FakeAgent(None, None)], config=CAPTIONS)

    async def go():
        with pytest.raises(_Boom):
            await s._ask_user("Send the email to Sam?")
    asyncio.run(go())
    assert s._pending_answer is None                  # the slot is free: watchdog and goodbye are not blocked
    assert s._farewell_due(10_000.0) is True
