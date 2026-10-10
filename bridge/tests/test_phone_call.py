"""phone_call.py: "Call eve" in the Mini App, push to talk into the chat's brain. The real Mini App server on a
loopback port, a real WebSocket client (the websockets library, as the phone's browser), a fake chat that answers
through the call's reader, and a fake voice (no OpenAI)."""
from __future__ import annotations

import asyncio
import json
import time
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import pytest
from initdata import sign_init_data
from websockets.asyncio.client import connect
from websockets.exceptions import InvalidStatus

from eve_bridge import miniapp, phone_call
from eve_bridge.miniapp import MiniAppConfig, MiniAppServer, SpendLedger, check_init_data

TOKEN = "123456:TEST-token"
OWNER = 4242


def signed(user_id: int = OWNER) -> str:
    return sign_init_data({"auth_date": str(int(time.time())), "query_id": "AAH",
                           "user": json.dumps({"id": user_id, "first_name": "O"})}, TOKEN)


class Brain:
    """The chat: hears the owner's words and answers through the call's reader, as telegram.TelegramInlet does."""

    def __init__(self, reply: str = "You have **two** meetings: [standup](https://x.example) and lunch.") -> None:
        self.reply = reply
        self.heard: list[str] = []
        self.say: Any = None

    def listen(self, say, owner=None) -> bool:
        if say is not None or owner is None or self.say == owner:    # as TelegramInlet.listen
            self.say = say
        return True

    def hear(self, text: str) -> None:
        self.heard.append(text)
        if self.say is not None and self.reply:
            self.say(self.reply)


class Voice:
    def __init__(self, text: str = "what's on my calendar") -> None:
        self.text = text
        self.spoken: list[str] = []
        self.transcribed: list[int] = []

    def transcribe(self, pcm: bytes) -> str:
        self.transcribed.append(len(pcm))
        return self.text

    def speak_stream(self, text: str):
        self.spoken.append(text)
        yield b"\x01\x00" * 1200
        yield b"\x01"                                                   # an odd chunk: carried, never split
        yield b"\x00" + b"\x01\x00" * 1199

    def live_ears(self):
        return None


async def serve(tmp_path: Path, brain: Brain | None, voice: Voice | None = None, public: str = "", on: bool = True):
    cfg = MiniAppConfig(enabled=True, token=TOKEN, owner_ids=frozenset({OWNER}), ledger_path=tmp_path / "l.json")
    server = MiniAppServer(cfg, SpendLedger(cfg.ledger_path), page=b"home")
    if on:
        server.calls = phone_call.PhoneCalls(lambda d: check_init_data(d, TOKEN, cfg.owner_ids), brain,
                                             voice if voice is not None else Voice())
    port = await server.start()
    server.public_url = public
    return server, f"ws://127.0.0.1:{port}/api/call", f"http://127.0.0.1:{port}"


async def until(ws, pred) -> list[Any]:
    out: list[Any] = []
    async with asyncio.timeout(5):
        while True:
            m = await ws.recv()
            out.append(m if isinstance(m, bytes) else json.loads(m))
            if pred(out[-1]):
                return out


def is_(kind: str, **fields):
    return lambda m: isinstance(m, dict) and m.get("type") == kind and all(m.get(k) == v for k, v in fields.items())


async def press(ws, secs: float = 1.0) -> None:
    await ws.send(json.dumps({"type": "talk"}))
    for _ in range(int(secs * 10)):
        await ws.send(bytes(4800))
    await ws.send(json.dumps({"type": "done"}))


def test_the_accept_key_is_rfc_6455s():
    assert phone_call.accept_key("dGhlIHNhbXBsZSBub25jZQ==") == "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="


def test_a_press_is_heard_handed_to_the_chat_and_the_reply_read_back(tmp_path):
    brain, voice = Brain(), Voice()

    async def go():
        server, url, origin = await serve(tmp_path, brain, voice)
        async with connect(url, origin=origin) as ws:
            await ws.send(json.dumps({"initData": signed()}))
            await until(ws, is_("state", state="connected"))
            await ws.send(bytes(4800))                                   # before a press: never recorded
            await press(ws, 1.0)
            got = await until(ws, is_("state", state="listening"))
            await ws.send(json.dumps({"type": "end"}))
            ended = await until(ws, is_("ended"))
        await server.close()
        return got, ended

    got, ended = asyncio.run(go())
    assert {"type": "heard", "text": "what's on my calendar"} in got
    assert brain.heard == ["what's on my calendar"]
    assert voice.transcribed == [48000]                                  # the press alone: 1 s at 24 kHz
    assert voice.spoken == ["You have two meetings: standup and lunch."]  # Markdown and links read as words
    assert sum(len(m) for m in got if isinstance(m, bytes)) == 4800
    assert [m["state"] for m in got if isinstance(m, dict) and m.get("type") == "state"][:2] == ["thinking", "speaking"]
    # the sentence's words, for voice mode's captions, just before its sound (2026-10-09)
    said = [i for i, m in enumerate(got) if isinstance(m, dict) and m.get("type") == "said"]
    sound = next(i for i, m in enumerate(got) if isinstance(m, bytes))
    assert [got[i]["text"] for i in said] == ["You have two meetings: standup and lunch."] and said[0] < sound
    assert ended[-1] == {"type": "ended", "reason": "you hung up"}
    assert brain.say is None                                             # the chat stops reading to a gone call


def test_a_tap_is_not_words_and_nothing_is_asked(tmp_path):
    brain = Brain()

    async def go():
        server, url, origin = await serve(tmp_path, brain)
        async with connect(url, origin=origin) as ws:
            await ws.send(json.dumps({"initData": signed()}))
            await press(ws, 0.1)
            note = await until(ws, lambda m: isinstance(m, dict) and bool(m.get("note")))
        await server.close()
        return note

    assert "Hold the button" in asyncio.run(go())[-1]["note"] and brain.heard == []


def test_a_press_while_eve_talks_interrupts_it(tmp_path):
    class SlowVoice(Voice):
        def speak_stream(self, text: str):
            time.sleep(0.2)
            yield from super().speak_stream(text)

    brain = Brain(reply="One. " * 60)                                  # many sentences, slowly made

    async def go():
        server, url, origin = await serve(tmp_path, brain, SlowVoice())
        async with connect(url, origin=origin) as ws:
            await ws.send(json.dumps({"initData": signed()}))
            await press(ws)
            await until(ws, is_("state", state="speaking"))
            await ws.send(json.dumps({"type": "talk"}))
            got = await until(ws, is_("flush"))
        await server.close()
        return got

    assert asyncio.run(go())[-1] == {"type": "flush"}


@pytest.mark.parametrize("hello", [{"initData": ""}, {"initData": "garbage"}, "not json", {"x": 1}])
def test_nobody_but_the_owner_can_call(tmp_path, hello):
    brain = Brain()

    async def go():
        server, url, origin = await serve(tmp_path, brain)
        async with connect(url, origin=origin) as ws:
            await ws.send(hello if isinstance(hello, str) else json.dumps(hello))
            assert (await until(ws, is_("ended")))[-1]["reason"] == "Only eve's owner can call."
        stranger = sign_init_data({"auth_date": str(int(time.time())), "user": json.dumps({"id": 999})}, TOKEN)
        async with connect(url, origin=origin) as ws:
            await ws.send(json.dumps({"initData": stranger}))
            assert (await until(ws, is_("ended")))[-1]["reason"] == "Only eve's owner can call."
        await server.close()

    asyncio.run(go())
    assert brain.say is None                                            # nothing was started


def test_calling_again_replaces_a_call_left_open_and_calls_are_off_without_a_chat(tmp_path):
    async def go():
        server, url, origin = await serve(tmp_path, Brain())
        async with connect(url, origin=origin) as first:
            await first.send(json.dumps({"initData": signed()}))
            await until(first, is_("state", state="connected"))
            async with connect(url, origin=origin) as second:           # the page reopened; the old socket lingers
                await second.send(json.dumps({"initData": signed()}))
                assert (await until(second, is_("state", state="connected")))[-1]["state"] == "connected"
                assert server.calls.active is not None and server.calls.active.ws is not None
        await server.close()
        server, url, origin = await serve(tmp_path, None)
        async with connect(url, origin=origin) as ws:
            await ws.send(json.dumps({"initData": signed()}))
            assert "aren't set up" in (await until(ws, is_("ended")))[-1]["reason"]
        await server.close()

    asyncio.run(go())


def test_a_phone_that_goes_quiet_ends_the_call(tmp_path, monkeypatch):
    monkeypatch.setattr(phone_call, "QUIET_SECS", 0.3)
    brain = Brain()

    async def go():
        server, url, origin = await serve(tmp_path, brain)
        async with connect(url, origin=origin) as ws:
            await ws.send(json.dumps({"initData": signed()}))
            assert (await until(ws, is_("ended")))[-1]["reason"] == "the phone went quiet"
        assert server.calls.active is None and brain.say is None
        await server.close()

    asyncio.run(go())


def test_the_upgrade_is_refused_from_elsewhere_or_when_calls_are_off(tmp_path):
    async def go():
        server, url, _ = await serve(tmp_path, Brain(), public="https://eve.example.trycloudflare.com")
        for origin in ("https://evil.example", "null"):
            with pytest.raises(InvalidStatus) as e:
                async with connect(url, origin=origin):
                    pass
            assert e.value.response.status_code == 403
        await server.close()
        server, url, origin = await serve(tmp_path, None, on=False)
        with pytest.raises(InvalidStatus) as e:
            async with connect(url, origin=origin):
                pass
        assert e.value.response.status_code == 404
        await server.close()

    asyncio.run(go())


def test_a_silent_socket_is_closed_after_the_auth_window(tmp_path, monkeypatch):
    monkeypatch.setattr(phone_call, "AUTH_SECS", 0.2)

    async def go():
        server, url, origin = await serve(tmp_path, Brain())
        async with connect(url, origin=origin) as ws:
            assert (await until(ws, is_("ended")))[-1]["reason"] == "Only eve's owner can call."
        await server.close()

    asyncio.run(go())


def test_speakable_reads_a_chat_message_as_words():
    assert phone_call.speakable("**Done.** See [the doc](https://d.example/x) or https://y.example/z") == \
        "Done. See the doc or a link"
    assert phone_call.speakable("- one\n- two") == "one two"
    long = "A sentence here. " * 200
    out = phone_call.speakable(long)
    assert len(out) <= phone_call.MAX_SPOKEN_CHARS + 40 and out.endswith("The rest is in the chat.")
    assert phone_call.sentences("Hi. Yes. " + "This one is long enough to stand alone as a sentence of its own. " * 2) \
        [0].startswith("Hi. Yes.")


def test_frames_round_trip_and_bad_frames_close():
    async def go():
        reader = asyncio.StreamReader()
        payload = b"x" * 70000                                       # the 64-bit length form
        mask = b"\x01\x02\x03\x04"
        masked = bytes(b ^ mask[i % 4] for i, b in enumerate(payload))
        reader.feed_data(bytes([0x82, 0x80 | 127]) + len(payload).to_bytes(8, "big") + mask + masked)
        ws = phone_call.WebSocket(reader, None)                      # type: ignore[arg-type]
        assert await ws.recv() == (phone_call.OP_BIN, payload)
        reader.feed_data(bytes([0x82, 0x05]) + b"hello")                # unmasked: a client must mask
        with pytest.raises(phone_call.Closed):
            await ws.recv()
        big = phone_call.MAX_FRAME_BYTES + 1
        reader2 = asyncio.StreamReader()
        reader2.feed_data(bytes([0x82, 0x80 | 127]) + big.to_bytes(8, "big"))
        with pytest.raises(phone_call.Closed):
            await phone_call.WebSocket(reader2, None).recv()     # type: ignore[arg-type]

    asyncio.run(go())
    assert phone_call.frame(phone_call.OP_TEXT, b"hi") == b"\x81\x02hi"


def test_the_page_has_the_call_button_and_speaks_the_wire_format():
    page = miniapp.PAGE_PATH.read_text()
    assert 'id="call-open"' in page and 'new URL("api/call"' in page
    assert "RATE = 24000" in page and "JSON.stringify({ initData })" in page and 'id="call-talk"' in page
    assert 'type: "talk"' in page and 'type: "done"' in page


# ---- latency: live ears, the fast voice and their fallbacks -------------------------------------------

class FakeEars:
    def __init__(self, text: str = "live words", fail: bool = False) -> None:
        self.text, self.fail = text, fail
        self.fed = 0
        self.begun = 0
        self.broken = False

    async def open(self) -> None:
        pass

    async def begin(self) -> None:
        self.begun += 1
        self.fed = 0

    async def feed(self, pcm: bytes) -> None:
        self.fed += len(pcm)

    async def finish(self, timeout: float) -> str:
        if self.fail:
            raise TimeoutError("no text")
        return self.text

    async def close(self) -> None:
        pass


class LiveVoice(Voice):
    def __init__(self, ears: FakeEars) -> None:
        super().__init__(text="uploaded words")
        self.ears = ears

    def live_ears(self):
        return self.ears


@pytest.mark.parametrize("fail,expect", [(False, "live words"), (True, "uploaded words")])
def test_a_press_is_transcribed_live_and_falls_back_to_the_upload(tmp_path, fail, expect):
    brain, ears = Brain(reply=""), FakeEars(fail=fail)
    voice = LiveVoice(ears)

    async def go():
        server, url, origin = await serve(tmp_path, brain, voice)
        async with connect(url, origin=origin) as ws:
            await ws.send(json.dumps({"initData": signed()}))
            await until(ws, is_("state", state="connected"))
            await asyncio.sleep(0.05)                                # the live session opens meanwhile
            await press(ws, 1.0)
            got = await until(ws, is_("heard"))
        await server.close()
        return got

    assert asyncio.run(go())[-1]["text"] == expect
    assert brain.heard == [expect] and ears.fed == 48000 and ears.begun == 1
    assert voice.transcribed == ([48000] if fail else [])           # uploaded only when the live one failed


def test_the_voice_is_openrouters_alone_and_a_failed_sentence_raises(monkeypatch):
    # until 2026-10-08 OpenAI's gpt-4o-mini-tts was the fallback; with no OpenAI key there is no second voice
    import contextlib as cl

    import httpx

    class Resp:
        def __init__(self, status: int) -> None:
            self.status_code = status

        def iter_bytes(self, n):
            yield b"\x01\x00" * 10

    calls: list[str] = []
    status = [503]

    @cl.contextmanager
    def fake_stream(method, url, **kw):
        calls.append((url, kw["json"]["model"], kw["headers"]["Authorization"]))
        yield Resp(status[0])

    monkeypatch.setattr(httpx, "stream", fake_stream)
    v = phone_call.make_voice({"OPENROUTER_API_KEY": "or-key"})
    with pytest.raises(RuntimeError, match="503"):
        b"".join(v.speak_stream("hello"))
    status[0] = 200
    assert b"".join(v.speak_stream("hello")) == b"\x01\x00" * 10
    assert calls == [(phone_call.OPENROUTER_SPEECH_URL, "deepgram/aura-2", "Bearer or-key")] * 2


def test_make_voice_needs_only_the_openrouter_key_and_the_live_ears_are_opt_in():
    v = phone_call.make_voice({"OPENROUTER_API_KEY": "or-test"})
    assert v.or_key == "or-test" and v.or_model == "deepgram/aura-2" and not v.live and v.live_ears() is None
    assert str(v._client.base_url).startswith("https://openrouter.ai/api/v1")
    assert phone_call.make_voice({"OPENAI_API_KEY": "sk-test"}) is None              # an OpenAI key alone: off
    assert phone_call.make_voice({}) is None
    # EVE_CALL_LIVE_STT=1 without an OpenAI key: uploads, no live ears
    assert not phone_call.make_voice({"OPENROUTER_API_KEY": "or-test", "EVE_CALL_LIVE_STT": "1"}).live
    v = phone_call.make_voice({"OPENROUTER_API_KEY": "or-test", "EVE_CALL_LIVE_STT": "1",
                               "OPENAI_API_KEY": "sk-test"})
    assert v.live and isinstance(v.live_ears(), phone_call.LiveEars) and v.live_ears().api_key == "sk-test"


def test_calls_are_transcribed_in_english_unless_told_otherwise():
    # a short Voice PE press, left to guess, was heard as Chinese and answered in Chinese (2026-09-27)
    assert phone_call.make_voice({"OPENROUTER_API_KEY": "or-test"}).language == "en"
    assert phone_call.make_voice({"OPENROUTER_API_KEY": "or-test", "EVE_CALL_LANGUAGE": "ES"}).language == "es"
    assert phone_call.make_voice({"OPENROUTER_API_KEY": "or-test", "EVE_CALL_LANGUAGE": ""}).language == ""

    asked: list = []

    class Transcriptions:
        def create(self, **kw):
            asked.append(kw)
            return type("R", (), {"text": " hi ", "usage": None})()

    v = phone_call.OpenAIVoice.__new__(phone_call.OpenAIVoice)
    v._client = type("C", (), {"audio": type("A", (), {"transcriptions": Transcriptions()})()})()
    v.stt_model, v.language = "m", "en"
    assert v.transcribe(b"\0\0" * 2400) == "hi" and asked[-1]["language"] == "en"
    v.language = ""
    v.transcribe(b"\0\0" * 2400)
    assert "language" not in asked[-1]

    sessions: list = []

    class Session:
        async def update(self, session):
            sessions.append(session)

    class Conn:
        session = Session()

        def __aiter__(self):
            return self

        async def __anext__(self):
            if not getattr(self, "configured", False):
                self.configured = True
                return SimpleNamespace(type="session.updated")
            await asyncio.Future()

    class Manager:
        async def __aenter__(self):
            return Conn()

        async def __aexit__(self, *a):
            return None

    import openai

    class Realtime:
        def connect(self, **kw):
            return Manager()

    class Client:
        def __init__(self, **kw):
            self.realtime = Realtime()

    orig = openai.AsyncOpenAI
    openai.AsyncOpenAI = Client
    try:
        async def go():
            ears = phone_call.LiveEars("k", "m", "en")
            await ears.open()
            await ears.close()
        asyncio.run(go())
    finally:
        openai.AsyncOpenAI = orig
    assert sessions[0]["audio"]["input"]["transcription"] == {"model": "m", "language": "en"}


def test_the_calls_ears_keep_the_full_press_context_with_near_field_noise_reduction(monkeypatch):
    """A physical press supplies the boundary; pauses within it stay in the same transcription turn."""
    assert phone_call.DEFAULT_STT_MODEL == "gpt-4o-transcribe"
    sent: list = []

    class Session:
        async def update(self, session):
            sent.append(session)

    class Conn:
        session = Session()

        def __aiter__(self):
            return self

        async def __anext__(self):
            if not getattr(self, "configured", False):
                self.configured = True
                return SimpleNamespace(type="session.updated")
            await asyncio.Future()

    class Manager:
        async def __aenter__(self):
            return Conn()

        async def __aexit__(self, *a):
            return None

    class Realtime:
        def connect(self, **kw):
            return Manager()

    class Client:
        def __init__(self, **kw):
            self.realtime = Realtime()

    import openai
    monkeypatch.setattr(openai, "AsyncOpenAI", Client)
    ears = phone_call.LiveEars("k", phone_call.DEFAULT_STT_MODEL)
    asyncio.run(ears.open())
    audio_in = sent[0]["audio"]["input"]
    assert audio_in["transcription"]["model"] == "gpt-4o-transcribe"
    assert audio_in["noise_reduction"] == {"type": "near_field"}
    assert audio_in["turn_detection"] is None
    # and the meter can price it
    from eve_bridge import pricing
    assert pricing.estimate_transcribe_cost("gpt-4o-transcribe", {}, seconds=60) == pytest.approx(0.006)


# ---- a call holds the chat from its first press, and an idle one gives it to the desk (2026-09-30) ----------------

class ReadyBrain(Brain):
    """The chat as telegram.TelegramInlet is now: asked whether it is there without taking its listener."""

    def chat_ready(self) -> bool:
        return True


def test_an_open_unused_call_holds_nothing_and_an_idle_one_gives_the_chat_to_the_desk(tmp_path):
    """An open call that was never pressed. On 2026-09-30 one kept the Voice PE's
    button waiting ("desk call: the phone is on a call; the button waits")."""
    now = {"t": 0.0}
    brain = ReadyBrain()

    async def go():
        cfg = MiniAppConfig(enabled=True, token=TOKEN, owner_ids=frozenset({OWNER}), ledger_path=tmp_path / "l.json")
        server = MiniAppServer(cfg, SpendLedger(cfg.ledger_path), page=b"home")
        server.calls = phone_call.PhoneCalls(lambda d: check_init_data(d, TOKEN, cfg.owner_ids), brain, Voice(),
                                             clock=lambda: now["t"])
        port = await server.start()
        url, origin = f"ws://127.0.0.1:{port}/api/call", f"http://127.0.0.1:{port}"
        calls = server.calls
        async with connect(url, origin=origin) as ws:
            await ws.send(json.dumps({"initData": signed()}))
            await until(ws, is_("state", state="connected"))
            # open, never pressed: no listener, and the desk is free
            assert brain.say is None and calls.holding_chat() is False
            # the first press takes the chat; the reply is read out
            await press(ws, 1.0)
            await until(ws, is_("state", state="listening"))
            call = calls.active
            assert call.listening and brain.say == call.say
            # just answered: still the phone's
            now["t"] += phone_call.IDLE_YIELD_SECS - 1
            assert calls.holding_chat() is True and brain.say == call.say
            # quiet long enough: the desk's button takes it; the call stays open
            now["t"] += 2
            assert calls.holding_chat() is False and brain.say is None and calls.active is call
            # the phone's next press takes it back
            await press(ws, 1.0)
            await until(ws, is_("state", state="listening"))
            assert brain.say == call.say
        await server.close()

    asyncio.run(go())


def test_a_call_that_is_speaking_or_answering_never_gives_the_chat_up(tmp_path):
    now = {"t": 0.0}

    class Brain2(ReadyBrain):
        pass

    brain = Brain2()
    call = phone_call.Call(SimpleNamespace(closed=False), brain, Voice(), clock=lambda: now["t"])
    call._take_chat()
    now["t"] = 100.0
    for busy in ("talking", "speaking"):
        setattr(call, busy, True)
        assert call.release_if_idle() is False and brain.say == call.say
        setattr(call, busy, False)
    call.lines.put_nowait("still to say")
    assert call.release_if_idle() is False
    call.lines.get_nowait()
    assert call.release_if_idle() is True and brain.say is None       # control: idle, it lets go
