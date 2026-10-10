"""The PTT wire supplies turn boundaries; a fake realtime server exercises actual event handling."""
from __future__ import annotations

import asyncio
import base64
import json
from types import SimpleNamespace

import pytest
from test_phone_call import Brain, LiveVoice, is_, press, serve, signed, until
from websockets.asyncio.client import connect

from eve_bridge import phone_call


class RealtimeServer:
    def __init__(self, texts=("remind me to call the dentist", "hello, cancel that")):
        self.events = asyncio.Queue()
        self.session = self.input_audio_buffer = self
        self.texts = texts
        self.sessions = []
        self.audio = bytearray()
        self.commits = []
        self.clears = 0
        self.auto_ack = True
        self.auto_complete = True
        self.closed = False

    def __aiter__(self):
        return self

    async def __anext__(self):
        return await self.events.get()

    def emit(self, kind, **kw):
        self.events.put_nowait(SimpleNamespace(type=kind, **kw))

    async def update(self, session):
        self.sessions.append(session)
        if self.auto_ack:
            self.emit("session.updated", session=session)

    async def clear(self):
        self.clears += 1
        self.audio.clear()

    async def append(self, audio):
        self.audio.extend(base64.b64decode(audio))

    async def commit(self):
        self.commits.append(bytes(self.audio))
        self.audio.clear()
        item = f"press-{len(self.commits)}"
        self.emit("input_audio_buffer.committed", item_id=item)
        if self.auto_complete:
            self.complete(item, self.texts[len(self.commits) - 1])

    def complete(self, item, transcript):
        self.emit("conversation.item.input_audio_transcription.completed", item_id=item, transcript=transcript)


def install_server(monkeypatch, server):
    import openai

    class Manager:
        async def __aenter__(self):
            return server

        async def __aexit__(self, *args):
            server.closed = True

    class Client:
        def __init__(self, **kwargs):
            self.realtime = SimpleNamespace(connect=lambda **kw: Manager())

    monkeypatch.setattr(openai, "AsyncOpenAI", Client)


def test_open_waits_for_the_manual_turn_configuration_ack(monkeypatch):
    server = RealtimeServer()
    server.auto_ack = False
    install_server(monkeypatch, server)

    async def go():
        ears = phone_call.LiveEars("fake", "gpt-4o-transcribe")
        opening = asyncio.create_task(ears.open())
        await asyncio.sleep(0)
        assert not opening.done()
        assert server.sessions[0]["audio"]["input"]["turn_detection"] is None
        server.emit("session.updated")
        await opening
        await ears.close()
        assert server.closed and ears._reader.done()

    asyncio.run(go())


@pytest.mark.parametrize("failure", ["rejected", "timeout"])
def test_session_configuration_failure_closes_the_unready_session(monkeypatch, failure):
    server = RealtimeServer()
    server.auto_ack = False
    if failure == "rejected":
        server.emit("error", error=SimpleNamespace(code="invalid_session_update"))
    install_server(monkeypatch, server)
    original_timeout = asyncio.timeout
    monkeypatch.setattr(phone_call.asyncio, "timeout", lambda delay: original_timeout(0.02 if delay == 5.0 else delay))

    async def go():
        ears = phone_call.LiveEars("fake", "gpt-4o-transcribe")
        call = phone_call.Call(SimpleNamespace(closed=True), Brain(), LiveVoice(ears))
        await call._open_ears()
        assert call.ears is None and server.closed and ears._reader.done()

    asyncio.run(go())


def test_unsolicited_committed_turn_breaks_the_session_instead_of_becoming_owner_text(monkeypatch):
    server = RealtimeServer()
    install_server(monkeypatch, server)

    async def go():
        ears = phone_call.LiveEars("fake", "gpt-4o-transcribe")
        await ears.open()
        await ears.begin()
        server.emit("input_audio_buffer.committed", item_id="unsolicited")
        server.complete("unsolicited", "random startup words")
        await asyncio.sleep(0)
        assert ears.broken and ears.order == [] and ears.done == {}
        with pytest.raises(ConnectionError):
            await ears.finish(timeout=0.2)
        await ears.close()

    asyncio.run(go())


def test_one_manual_commit_preserves_the_complete_press_and_ignores_unrelated_text(monkeypatch):
    server = RealtimeServer(texts=("hello, remind me to call the dentist",))
    install_server(monkeypatch, server)

    async def go():
        ears = phone_call.LiveEars("fake", "gpt-4o-transcribe")
        await ears.open()
        await ears.begin()
        # A sentence with a quiet pause: there is no extra turn or discarded initial word.
        chunks = (b"\x12\x00" * 2400, bytes(24000), b"\x34\x00" * 2400)
        server.complete("old-item", "unrelated old words")
        for chunk in chunks:
            await ears.feed(chunk)
        assert await ears.finish(timeout=1) == "hello, remind me to call the dentist"
        assert server.commits == [b"".join(chunks)]
        assert "old-item" not in ears.done
        await ears.close()

    asyncio.run(go())


def test_next_press_waits_for_previous_item_and_does_not_clear_its_audio(monkeypatch):
    server = RealtimeServer()
    server.auto_complete = False
    install_server(monkeypatch, server)

    async def go():
        ears = phone_call.LiveEars("fake", "gpt-4o-transcribe")
        await ears.open()
        await ears.begin()
        first = b"\x12\x00" * 4800
        await ears.feed(first)
        finishing = asyncio.create_task(ears.finish(timeout=1))
        await asyncio.sleep(0)
        beginning = asyncio.create_task(ears.begin())
        await asyncio.sleep(0)
        assert not beginning.done() and server.clears == 1
        server.complete("press-1", "first words")
        assert await finishing == "first words"
        await beginning
        second = b"\x34\x00" * 4800
        await ears.feed(second)
        # A late duplicate completion from the first item cannot prefix the second.
        server.complete("press-1", "old words")
        finishing = asyncio.create_task(ears.finish(timeout=1))
        await asyncio.sleep(0)
        server.complete("press-2", "hello, second words")
        assert await finishing == "hello, second words"
        assert server.commits == [first, second]
        await ears.close()

    asyncio.run(go())


@pytest.mark.parametrize("failure", ["timeout", "failed", "commit_error"])
def test_failure_unlocks_and_marks_the_session_unusable(monkeypatch, failure):
    server = RealtimeServer()
    server.auto_complete = False
    install_server(monkeypatch, server)

    async def go():
        ears = phone_call.LiveEars("fake", "gpt-4o-transcribe")
        await ears.open()
        await ears.begin()
        await ears.feed(b"\x12\x00" * 4800)
        finishing = asyncio.create_task(ears.finish(timeout=0.05))
        await asyncio.sleep(0)
        if failure == "failed":
            server.emit("conversation.item.input_audio_transcription.failed", item_id="press-1")
        elif failure == "commit_error":
            server.emit("error", error=SimpleNamespace(code="input_audio_buffer_commit_empty"))
        with pytest.raises((TimeoutError, ConnectionError)):
            await finishing
        assert ears.broken and not ears._press_lock.locked()
        with pytest.raises(ConnectionError):
            await asyncio.wait_for(ears.begin(), 0.2)
        await ears.close()

    asyncio.run(go())


def test_short_tap_discard_unlocks_without_creating_a_turn(monkeypatch):
    server = RealtimeServer()
    install_server(monkeypatch, server)

    async def go():
        ears = phone_call.LiveEars("fake", "gpt-4o-transcribe")
        await ears.open()
        await ears.begin()
        await ears.feed(bytes(2400))
        await ears.discard()
        await asyncio.wait_for(ears.begin(), 0.2)
        await ears.feed(b"\x12\x00" * 4800)
        assert await ears.finish(timeout=1) == server.texts[0]
        assert server.commits == [b"\x12\x00" * 4800]
        await ears.close()

    asyncio.run(go())


def test_failed_discard_cannot_reuse_the_short_taps_audio(monkeypatch):
    server = RealtimeServer()
    install_server(monkeypatch, server)

    async def go():
        ears = phone_call.LiveEars("fake", "gpt-4o-transcribe")
        await ears.open()
        await ears.begin()
        await ears.feed(bytes(2400))

        async def failed_clear():
            raise ConnectionError("clear failed")

        server.clear = failed_clear
        with pytest.raises(ConnectionError):
            await ears.discard()
        assert ears.broken and not ears._press_lock.locked()
        with pytest.raises(ConnectionError):
            await ears.begin()
        await ears.close()

    asyncio.run(go())


def test_cancelled_open_closes_the_session_that_the_call_never_owned(monkeypatch):
    server = RealtimeServer()
    server.auto_ack = False
    install_server(monkeypatch, server)

    async def go():
        ears = phone_call.LiveEars("fake", "gpt-4o-transcribe")
        call = phone_call.Call(SimpleNamespace(closed=True), Brain(), LiveVoice(ears))
        opening = asyncio.create_task(call._open_ears())
        await asyncio.sleep(0)
        assert ears._reader is not None and call.ears is None
        opening.cancel()
        with pytest.raises(asyncio.CancelledError):
            await opening
        assert server.closed and ears._reader.done()

    asyncio.run(go())


def test_cancelled_finish_unlocks_and_never_reuses_a_delayed_transcript(monkeypatch):
    server = RealtimeServer()
    server.auto_complete = False
    install_server(monkeypatch, server)

    async def go():
        ears = phone_call.LiveEars("fake", "gpt-4o-transcribe")
        await ears.open()
        await ears.begin()
        await ears.feed(bytes(4800))
        finishing = asyncio.create_task(ears.finish(timeout=1))
        await asyncio.sleep(0)
        finishing.cancel()
        with pytest.raises(asyncio.CancelledError):
            await finishing
        assert ears.broken and not ears._press_lock.locked()
        with pytest.raises(ConnectionError):
            await ears.begin()
        await ears.close()

    asyncio.run(go())


def test_rapid_websocket_presses_keep_all_audio_and_initial_words(tmp_path, monkeypatch):
    server = RealtimeServer()
    server.auto_complete = False
    install_server(monkeypatch, server)
    ears = phone_call.LiveEars("fake", "gpt-4o-transcribe")
    brain = Brain(reply="")
    voice = LiveVoice(ears)

    async def go():
        app, url, origin = await serve(tmp_path, brain, voice)
        try:
            async with connect(url, origin=origin) as ws:
                await ws.send(json.dumps({"initData": signed()}))
                await until(ws, is_("state", state="connected"))
                await ears.configured.wait()
                await press(ws)
                await until(ws, is_("state", state="thinking"))
                # Send a complete second press before the first transcript finishes.
                await press(ws)
                await until(ws, is_("flush"))
                server.complete("press-1", server.texts[0])
                assert (await until(ws, is_("heard")))[-1]["text"] == server.texts[0]
                await until(ws, is_("state", state="thinking"))
                server.complete("press-2", server.texts[1])
                assert (await until(ws, is_("heard")))[-1]["text"] == server.texts[1]
        finally:
            await app.close()
        assert server.commits == [bytes(48000), bytes(48000)]
        assert brain.heard == list(server.texts) and voice.transcribed == []

    asyncio.run(go())


def test_failed_live_press_and_waiting_next_press_both_recover_with_complete_uploads(tmp_path, monkeypatch):
    server = RealtimeServer()
    server.auto_complete = False
    install_server(monkeypatch, server)
    ears = phone_call.LiveEars("fake", "gpt-4o-transcribe")
    brain = Brain(reply="")
    voice = LiveVoice(ears)

    async def go():
        app, url, origin = await serve(tmp_path, brain, voice)
        try:
            async with connect(url, origin=origin) as ws:
                await ws.send(json.dumps({"initData": signed()}))
                await until(ws, is_("state", state="connected"))
                await ears.configured.wait()
                await press(ws)
                await until(ws, is_("state", state="thinking"))
                await press(ws)
                await until(ws, is_("flush"))
                server.emit("conversation.item.input_audio_transcription.failed", item_id="press-1")
                for _ in range(2):
                    assert (await until(ws, is_("heard")))[-1]["text"] == "uploaded words"
        finally:
            await app.close()
        assert voice.transcribed == [48000, 48000]
        assert brain.heard == ["uploaded words", "uploaded words"]
        assert server.closed and len(server.commits) == 1

    asyncio.run(go())


def test_duplicate_talk_and_audio_crossing_the_press_limit_do_not_reset_or_overshoot():
    class Socket:
        closed = True

        async def send_json(self, obj):
            pass

    async def go():
        call = phone_call.Call(Socket(), Brain(reply=""), LiveVoice(None))
        await call._begin_press()
        await call._feed(b"\x12\x00" * 4800)
        await call._begin_press()
        assert call.press == b"\x12\x00" * 4800
        cap = int(phone_call.MAX_PRESS_SECS * phone_call.BYTES_PER_SEC)
        call.press = bytearray(cap - 2)
        await call._feed(b"\x34\x00" * 2400)
        assert len(call.press) == cap and call.press[-2:] == b"\x34\x00"
        await call._feed(b"\x56\x00")
        assert len(call.press) == cap
        call.reading.cancel()
        await asyncio.gather(call.reading, return_exceptions=True)

    asyncio.run(go())
