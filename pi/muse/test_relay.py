"""eve's Muse relay (eve_muse_relay.py) against a fake Muse session, with the SDK's own MuseTurn.

Needs Meta's ``musegadget`` package importable for ``muse_turn`` (on the Pi: ``/opt/eve-muse/venv-python -m pytest``;
on a Mac: PYTHONPATH at a checkout's ``linux/src``). The service module is a stand-in: nothing connects to Muse.
"""

from __future__ import annotations

import asyncio
import json
import re
import sys
import types

import pytest

pytest.importorskip("musegadget.muse_turn")


class _PlainService:
    _current = None

    async def _handle_local(self, reader, writer) -> None:
        line = await reader.readline()
        writer.write(json.dumps({"ok": True, "plain": json.loads(line)}).encode() + b"\n")


_service = types.ModuleType("musegadget.service")
_service.Service = _PlainService
_service._SESSION_ID_RE = re.compile(r"[A-Za-z0-9-]{1,64}")
sys.modules["musegadget.service"] = _service
import musegadget  # noqa: E402

musegadget.service = _service

import eve_muse_relay as relay  # noqa: E402

SESSION = "0b6f3c1e-1111-4222-8333-944455556666"


def ev(name: str, **payload) -> dict:
    return {"type": "event", "event": name, "payload": {"session_id": SESSION, **payload}}


class FakeSession:
    registered_at = 1.0

    def __init__(self, events: list[dict]) -> None:
        self.events = events

    async def send_chat(self, message, session_id):
        return {"ok": True, "response": {"result": {"message_id": "u1", "session_id": SESSION}}}

    async def subscribe_chat(self, session_id):
        for event in self.events:
            await asyncio.sleep(0)
            yield event
        await asyncio.Event().wait()                 # the stream stays open, as Muse's does


def answer_events(text: str) -> list[dict]:
    return [
        ev("agent.status", reply_to_message_id="u1", activity_code="researching", activity_text="Searching the web"),
        ev("task.status", reply_to_message_id="u1", task_id="t1", status="running"),
        ev("delta.message_start", message_id="m1", reply_to_message_id="u1"),
        ev("delta.text_append", message_id="m1", reply_to_message_id="u1", text=text),
        ev("delta.message_done", message_id="m1", reply_to_message_id="u1"),
        ev("task.status", reply_to_message_id="u1", task_id="t1", status="completed"),
    ]


class Reader:
    def __init__(self, request: dict) -> None:
        self.line = json.dumps(request).encode() + b"\n"

    async def readline(self) -> bytes:
        line, self.line = self.line, b""
        return line


class Writer:
    def __init__(self) -> None:
        self.data = b""

    def write(self, b: bytes) -> None:
        self.data += b

    async def drain(self) -> None:
        pass

    def close(self) -> None:
        pass

    def lines(self) -> list[dict]:
        return [json.loads(x) for x in self.data.splitlines() if x.strip()]


def serve(request: dict, events: list[dict]) -> list[dict]:
    service = _PlainService()
    service._current = FakeSession(events)
    writer = Writer()
    asyncio.run(relay._handle_local(service, Reader(request), writer))
    return writer.lines()


def test_a_streamed_request_gets_progress_then_the_answer() -> None:
    lines = serve({"message": "hi", "wait": True, "stream": True, "session_id": SESSION},
                  answer_events("Here is the answer."))
    assert lines[0] == {"progress": "Searching the web"}
    assert lines[-1]["ok"] is True and lines[-1]["reply"] == "Here is the answer."


def test_an_unstreamed_request_gets_one_line() -> None:
    lines = serve({"message": "hi", "wait": True, "session_id": SESSION}, answer_events("One line."))
    assert len(lines) == 1 and lines[0]["reply"] == "One line."


def test_an_answer_over_the_sdk_bound_is_kept_and_marked_cut() -> None:
    events = answer_events("a" * 1000)[:4] + [
        ev("delta.text_append", message_id="m1", reply_to_message_id="u1", text="b" * (70 * 1024))]
    lines = serve({"message": "hi", "wait": True, "session_id": SESSION}, events)
    assert lines[-1]["ok"] is True and lines[-1]["cut"] is True and lines[-1]["reply"].startswith("a" * 1000)


def test_other_requests_go_to_musegadget() -> None:
    lines = serve({"message": "hi"}, [])
    assert lines == [{"ok": True, "plain": {"message": "hi"}}]


def test_the_relay_waits_minutes() -> None:
    assert relay.ANSWER_TIMEOUT_S >= 900
