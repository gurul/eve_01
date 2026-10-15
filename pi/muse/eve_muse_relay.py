"""eve's Muse relay for the Raspberry Pi.

Runs Meta's musegadget service unchanged, plus one extra request on its local
socket: ``{"message": "...", "wait": true, "session_id": "..."}`` sends the
message to Muse and answers with Muse's reply, ``{"ok": true, "reply": "..."}``.
Every other request goes to musegadget as before.

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

from musegadget import service as gadget_service
from musegadget.muse_turn import MuseTurn

log = logging.getLogger("eve_muse_relay")

ANSWER_TIMEOUT_S = 120.0
POLL_S = 0.5

_plain_request = gadget_service.Service._local_request
_one_at_a_time: asyncio.Lock | None = None


def _error(text: str, **extra) -> dict:
    return {"ok": False, "error": text, **extra}


async def _local_request(self, line: bytes) -> dict:
    try:
        request = json.loads(line)
    except ValueError:
        return _error("expected one JSON line")
    if not (isinstance(request, dict) and request.get("wait") is True):
        return await _plain_request(self, line)
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
        return await ask(session, message, session_id)


async def ask(session, message: str, session_id: str | None,
              timeout: float = ANSWER_TIMEOUT_S) -> dict:
    """Send ``message`` on ``session`` and return Muse's answer to it."""
    events: asyncio.Queue = asyncio.Queue()

    async def pump() -> None:
        async for event in session.subscribe_chat(session_id):
            events.put_nowait(event)

    reader: asyncio.Task | None = None
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
        turn.acknowledge(response)
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
                turn.feed(event)
            except asyncio.TimeoutError:
                pass
            now = time.monotonic()
            if turn.settled(now) or (turn.replies and turn.complete(now)):
                return {"ok": True, "reply": _text(turn), "session_id": turn.session_id}
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

    gadget_service.Service._local_request = _local_request
    return cli.main(["run"])


if __name__ == "__main__":
    sys.exit(main())
