"""eve's Muse relay for the Raspberry Pi.

Runs Meta's musegadget service unchanged, plus one extra request on its local
socket: ``{"message": "...", "wait": true, "session_id": "..."}`` sends the
message to Muse and answers with Muse's reply, ``{"ok": true, "reply": "..."}``.
With ``"stream": true`` the answer comes as several JSON lines: one
``{"progress": "..."}`` each time Muse says what it is doing, then the reply.
Every other request goes to musegadget as before.

Muse can work on one question for minutes (research, tasks), so the relay
waits up to ``ANSWER_TIMEOUT_S``. An answer over the SDK's 64 KiB bound is
kept as far as it got, marked ``"cut": true``.

The service is installed by Meta's ``linux/install.sh``; ``install.sh`` next to
this file adds a systemd drop-in so this module starts it. eve on the Mac
reaches it over SSH with ``ask.py``.
"""

from __future__ import annotations

import asyncio
import json
import logging
import sys
import time
from typing import Callable, Optional

from musegadget import service as gadget_service
from musegadget.muse_turn import AgentStatus, MuseTurn

log = logging.getLogger("eve_muse_relay")

ANSWER_TIMEOUT_S = 900.0
POLL_S = 0.5
READ_S = 10.0

_plain_handle = gadget_service.Service._handle_local
_one_at_a_time: asyncio.Lock | None = None


def _error(text: str, **extra) -> dict:
    return {"ok": False, "error": text, **extra}


def _waits(request: object) -> bool:
    return isinstance(request, dict) and request.get("wait") is True


async def _handle_local(self, reader, writer) -> None:
    """The socket's one request. A wait request is ours (streamed when asked); anything else is musegadget's."""
    try:
        line = await asyncio.wait_for(reader.readline(), READ_S)
        try:
            request = json.loads(line)
        except ValueError:
            request = None
        if not _waits(request):
            await _plain_handle(self, _Replay(line), writer)
            return

        async def progress(text: str) -> None:
            if request.get("stream") is True:
                writer.write(json.dumps({"progress": text}).encode() + b"\n")
                await writer.drain()

        reply = await _wait_request(self, request, progress)
    except Exception as exc:  # reported to the asker, never crashes the service
        reply = _error(f"{type(exc).__name__}: {exc}")
    writer.write(json.dumps(reply).encode() + b"\n")
    try:
        await writer.drain()
    finally:
        writer.close()


class _Replay:
    """A reader that gives musegadget's own handler the line already read."""

    def __init__(self, line: bytes) -> None:
        self.line = line

    async def readline(self) -> bytes:
        line, self.line = self.line, b""
        return line


async def _wait_request(self, request: dict, progress) -> dict:
    message = request.get("message")
    if not isinstance(message, str) or not message.strip():
        return _error('expected {"message": "..."}')
    session_id = request.get("session_id")
    if session_id is not None and not (
        isinstance(session_id, str) and gadget_service._SESSION_ID_RE.fullmatch(session_id)
    ):
        return _error("session_id must be letters, digits and dashes")
    session = self._current
    if session is None or session.registered_at is None:
        return _error("not connected to the Muse")
    global _one_at_a_time
    if _one_at_a_time is None:
        _one_at_a_time = asyncio.Lock()
    # One question at a time, so answers cannot swap between askers.
    async with _one_at_a_time:
        log.info("asking the Muse a %d-character message", len(message))
        return await ask(session, message, session_id, progress=progress)


async def ask(session, message: str, session_id: str | None, timeout: float = ANSWER_TIMEOUT_S,
              progress: Optional[Callable[[str], object]] = None) -> dict:
    """Send ``message`` on ``session`` and return Muse's answer to it. ``progress`` (async) hears Muse's status."""
    events: asyncio.Queue = asyncio.Queue()

    async def pump() -> None:
        async for event in session.subscribe_chat(session_id):
            events.put_nowait(event)

    said = ""

    async def tell(found: list) -> None:
        nonlocal said
        for item in found:
            text = (item.activity_text or "").strip() if isinstance(item, AgentStatus) else ""
            if text and text != said and progress is not None:
                said = text
                try:
                    await progress(text)
                except Exception as exc:  # the asker hung up: keep following, the answer is still logged
                    log.info("progress not delivered: %s", type(exc).__name__)

    reader: asyncio.Task | None = None
    turn: MuseTurn | None = None
    try:
        # Answers already on the chat (history replayed on subscribe) are not ours.
        turn = MuseTurn(session_id, ignore_parentless_until=time.monotonic())
        ack = await session.send_chat(message, session_id)
        if not ack.get("ok"):
            return _error(f"Muse refused the message (HTTP {ack.get('status')})",
                          detail=str(ack.get("response"))[:500])
        response = ack.get("response")
        if not isinstance(response, dict):
            return _error("Muse sent no acknowledgement")
        log.info("Muse acknowledged: %s", sorted(response.get("result", response)))
        await tell(turn.acknowledge(response))
        # Subscribe after the send: a side chat exists only once it has a message.
        reader = asyncio.create_task(pump())
        deadline = time.monotonic() + timeout
        while True:
            now = time.monotonic()
            if now >= deadline:
                return _error("Muse did not finish answering in time", reply=_text(turn))
            if reader.done():
                reader.result()  # raises the subscription's error, if any
                return _error("Muse closed the chat stream", reply=_text(turn))
            try:
                event = await asyncio.wait_for(events.get(), min(POLL_S, deadline - now))
                log.debug("event %s", event.get("event"))
                await tell(turn.feed(event))
            except asyncio.TimeoutError:
                pass
            now = time.monotonic()
            if turn.settled(now) or (turn.replies and turn.complete(now)):
                return {"ok": True, "reply": _text(turn), "session_id": turn.session_id}
    except ValueError as exc:
        # The SDK's size bounds (64 KiB of text, 32 messages): what Muse said up to there is still the answer.
        text = _text(turn) if turn is not None else ""
        if text:
            log.warning("answer cut at the SDK's bound: %s", exc)
            return {"ok": True, "reply": text, "cut": True, "session_id": turn.session_id}
        log.warning("ask failed: ValueError: %s", exc)
        return _error(f"ValueError: {exc}")
    except Exception as exc:  # reported to the asker, never crashes the service
        log.warning("ask failed: %s: %s", type(exc).__name__, exc)
        return _error(f"{type(exc).__name__}: {exc}")
    finally:
        if reader is not None:
            reader.cancel()
            try:
                await reader
            except BaseException:
                pass


def _text(turn: MuseTurn) -> str:
    return "\n\n".join(r.text.strip() for r in turn.replies.values() if r.text.strip())


def main() -> int:
    from musegadget import cli

    gadget_service.Service._handle_local = _handle_local
    return cli.main(["run"])


if __name__ == "__main__":
    sys.exit(main())
