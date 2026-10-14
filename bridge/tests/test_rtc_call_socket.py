"""lite_rtc.RtcCallSocket and InboundAudio: a push-to-talk call on the web app's ``call`` data channel and the peer's
audio, as the WebSocket PhoneCalls drives (docs/plans/eve-lite.md, M6).

The channel is test_lite_link_channel.py's fake (aiortc's RTCDataChannel surface); the microphone is fed the way the
peer's pump feeds it (``RtcCallSocket.audio``, after InboundAudio); eve's voice is a real VoiceTrack, taken a frame
at a time as aiortc's sender would. The last tests run the unchanged PhoneCalls over it with test_phone_call.py's
fake chat and voice."""
from __future__ import annotations

import asyncio
import fractions
import json
from typing import Any

import numpy as np
import pytest
from test_lite_link_channel import FakeChannel, until
from test_phone_call import Brain, Voice

from eve_bridge import lite_rtc, phone_call
from eve_bridge.lite_rtc import BLOCK_BYTES, InboundAudio, RtcCallSocket, VoiceTrack
from eve_bridge.phone_call import OP_BIN, OP_TEXT, Closed, PhoneCalls

av = pytest.importorskip("av")

SECRET = "peer-secret-CANARY-NOT-A-KEY-7f3c"
RATE = lite_rtc.SAMPLE_RATE


def stereo48(freq: float, start: int, n: int = 960, amp: int = 12000) -> Any:
    """20 ms of a tone as aiortc's Opus decoder hands the page's microphone over: 48 kHz stereo, 16-bit."""
    t = (np.arange(n) + start) / 48000
    mono = (np.sin(2 * np.pi * freq * t) * amp).astype(np.int16)
    frame = av.AudioFrame.from_ndarray(np.stack([mono, mono], axis=1).reshape(1, -1), format="s16",
                                       layout="stereo")
    frame.sample_rate, frame.pts, frame.time_base = 48000, start, fractions.Fraction(1, 48000)
    return frame


def dominant_hz(pcm: bytes, rate: int = RATE) -> float:
    x = np.frombuffer(pcm, dtype=np.int16).astype(np.float64)
    spectrum = np.abs(np.fft.rfft(x * np.hanning(len(x))))
    return float(np.fft.rfftfreq(len(x), 1 / rate)[int(np.argmax(spectrum))])


def pcm(ms: int, value: int) -> bytes:
    """``ms`` of 24 kHz mono PCM holding one value, so where each piece went can be read back."""
    return np.full(RATE * ms // 1000, value, dtype=np.int16).tobytes()


def values(data: bytes) -> list[int]:
    return sorted(set(np.frombuffer(data, dtype=np.int16).tolist()))


async def received(sock: RtcCallSocket, secs: float = 0.05) -> list[tuple[int, Any]]:
    """What the call would read now: text as JSON, binary as bytes, until nothing comes for ``secs``."""
    out: list[tuple[int, Any]] = []
    while True:
        try:
            op, data = await asyncio.wait_for(sock.recv(), secs)
        except asyncio.TimeoutError:
            return out
        if op == OP_TEXT:
            try:
                data = json.loads(data)
            except ValueError:
                pass                                                               # passed on as the page sent it
        out.append((op, data))


def page_got(ch: FakeChannel) -> list[dict[str, Any]]:
    return [json.loads(m) for m in ch.sent if isinstance(m, str)]


async def opened(ch: FakeChannel, voice: VoiceTrack | None = None, **kw: Any) -> RtcCallSocket:
    sock = RtcCallSocket(ch, voice or VoiceTrack(), SECRET, **kw)
    op, hello = await sock.recv()
    assert op == OP_TEXT
    return sock


# ---- the hello and the microphone ------------------------------------------------------------------------------

def test_the_hello_is_built_here_and_comes_first() -> None:
    async def go() -> None:
        ch = FakeChannel("call")
        sock = RtcCallSocket(ch, VoiceTrack(), SECRET)
        ch.page({"type": "ping"})                                                  # sent before the call read
        op, data = await sock.recv()
        assert op == OP_TEXT and json.loads(data) == {"token": SECRET, "can": []}
        assert await received(sock) == [(OP_TEXT, {"type": "ping"})]

    asyncio.run(go())


def test_a_48k_stereo_microphone_becomes_24k_mono_16_bit() -> None:
    inbound = InboundAudio()
    out = b"".join(inbound.convert(stereo48(1000, i * 960)) for i in range(50))     # one second
    assert len(out) % 2 == 0 and abs(len(out) - 2 * RATE) <= 64                    # 24,000 samples, give or take
    assert abs(dominant_hz(out[4800:]) - 1000) <= 10                              # the tone, not a copy of it
    level = np.abs(np.frombuffer(out[4800:], dtype=np.int16)).max()
    assert 10000 <= level <= 13000                                                 # mono, not a sum of channels
    mono8k = av.AudioFrame.from_ndarray(np.zeros((1, 160), dtype=np.int16), format="s16", layout="mono")
    mono8k.sample_rate, mono8k.pts, mono8k.time_base = 8000, 0, fractions.Fraction(1, 8000)
    assert len(inbound.convert(mono8k)) % 2 == 0                                   # another codec mid-call: still read


def test_the_microphone_reaches_the_call_only_between_talk_and_done() -> None:
    async def go() -> None:
        ch = FakeChannel("call")
        sock = await opened(ch, postroll_secs=0.05)
        inbound = InboundAudio()
        for i in range(50):
            sock.audio(inbound.convert(stereo48(1000, i * 960)))                   # before the press
        assert await received(sock) == []
        ch.page({"type": "talk"})
        for i in range(50, 100):
            sock.audio(inbound.convert(stereo48(1000, i * 960)))                   # held for a second
        ch.page({"type": "done"})
        got = await received(sock, 0.3)
        assert got[0] == (OP_TEXT, {"type": "talk"}) and got[-1] == (OP_TEXT, {"type": "done"})
        press = [d for op, d in got[1:-1]]
        assert all(op == OP_BIN for op, _ in got[1:-1])
        assert all(len(d) == BLOCK_BYTES for d in press[:-1]) and 0 < len(press[-1]) <= BLOCK_BYTES
        audio = b"".join(press)
        assert abs(len(audio) - (lite_rtc.PREROLL_BYTES + 2 * RATE)) <= 64          # the pre-roll, then the press
        assert abs(dominant_hz(audio) - 1000) <= 10
        for i in range(100, 120):
            sock.audio(inbound.convert(stereo48(1000, i * 960)))                   # after the release: not the call's
        assert await received(sock) == []

    asyncio.run(go())


def test_the_300_ms_before_talk_are_replayed_after_it() -> None:
    async def go() -> None:
        ch = FakeChannel("call")
        sock = await opened(ch)
        for value in range(1, 11):                                                 # one second, 100 ms per value
            sock.audio(pcm(100, value))
        ch.page({"type": "talk"})
        got = await received(sock)
        assert got[0] == (OP_TEXT, {"type": "talk"})
        assert [values(d) for _, d in got[1:]] == [[8], [9], [10]]                 # the last 300 ms, in order
        assert lite_rtc.PREROLL_BYTES == 3 * BLOCK_BYTES

    asyncio.run(go())


def test_done_waits_for_the_post_roll_and_carries_its_audio() -> None:
    async def go() -> None:
        ch = FakeChannel("call")
        sock = await opened(ch)
        ch.page({"type": "talk"})
        sock.audio(pcm(250, 5))
        loop = asyncio.get_running_loop()
        t0 = loop.time()
        ch.page({"type": "done"})
        sock.audio(pcm(170, 6))                                                    # the last RTP, after the done
        got: list[tuple[int, Any]] = []
        while not got or got[-1] != (OP_TEXT, {"type": "done"}):
            op, data = await asyncio.wait_for(sock.recv(), 2)
            got.append((op, json.loads(data) if op == OP_TEXT else data))
        assert loop.time() - t0 >= lite_rtc.POSTROLL_SECS - 0.01                    # held for the post-roll
        audio = [d for op, d in got if op == OP_BIN]
        assert sum(len(d) for d in audio) == len(pcm(420, 0))                      # every byte of the press
        assert len(audio[-1]) == len(pcm(420, 0)) % BLOCK_BYTES                     # its last part block
        assert values(b"".join(audio)) == [5, 6]

    asyncio.run(go())


def test_a_new_press_or_an_end_during_the_post_roll_releases_the_done_first() -> None:
    async def go() -> None:
        ch = FakeChannel("call")
        sock = await opened(ch, postroll_secs=5.0)
        ch.page({"type": "talk"})
        sock.audio(pcm(100, 3))
        ch.page({"type": "done"})
        ch.page({"type": "done"})                                                  # twice: one done
        ch.page({"type": "talk"})                                                  # pressed again at once
        sock.audio(pcm(100, 4))
        ch.page({"type": "done"})
        ch.page({"type": "end"})
        got = await received(sock)
        shape = [m["type"] if op == OP_TEXT else values(m) for op, m in got]
        assert shape == ["talk", [3], "done", "talk", [4], "done", "end"]

    asyncio.run(go())


def test_a_stray_done_and_other_messages_pass_as_they_came() -> None:
    async def go() -> None:
        ch = FakeChannel("call")
        sock = await opened(ch)
        ch.page({"type": "done"})
        ch.page({"type": "ping"})
        ch.page("not json")
        ch.page(b"\x00binary on the call channel")                               # audio is the track, never this
        ch.page(json.dumps({"type": "ping", "pad": "x" * lite_rtc.MAX_CONTROL_CHARS}))
        got = await received(sock)
        assert got == [(OP_TEXT, {"type": "done"}), (OP_TEXT, {"type": "ping"}), (OP_TEXT, b"not json")]

    asyncio.run(go())


# ---- eve's side: captions, flush and the rest ------------------------------------------------------------------

def test_said_goes_out_when_its_audio_is_taken_and_flush_drops_the_rest() -> None:
    async def go() -> None:
        ch = FakeChannel("call")
        voice = VoiceTrack()
        sock = await opened(ch, voice)
        await sock.send_json({"type": "said", "text": "First sentence."})
        await sock.send_bytes(pcm(30, 9))
        await sock.send_json({"type": "said", "text": "Second sentence."})
        await sock.send_bytes(pcm(20, 8))
        assert page_got(ch) == []                                                  # made, not yet playing
        assert values(voice.take()) == [9]
        assert page_got(ch) == [{"type": "said", "text": "First sentence."}]       # with its first sound
        assert values(voice.take()) == [8, 9]                                      # the frame where it changes
        assert [m["text"] for m in page_got(ch)] == ["First sentence.", "Second sentence."]
        await sock.send_json({"type": "said", "text": "Third sentence."})
        await sock.send_bytes(pcm(500, 7))
        await sock.send_json({"type": "flush"})                                    # a barge-in
        assert page_got(ch)[-1] == {"type": "flush"} and voice.queued == 0
        assert values(voice.take()) == [0]                                         # silence, and no caption
        assert "Third sentence." not in [m.get("text") for m in page_got(ch)]

    asyncio.run(go())


def test_heard_state_and_ended_go_out_at_once() -> None:
    async def go() -> None:
        ch = FakeChannel("call")
        voice = VoiceTrack()
        sock = await opened(ch, voice)
        await sock.send_bytes(pcm(1000, 2))                                        # a second of speech queued
        for msg in ({"type": "heard", "text": "what time is it"}, {"type": "state", "state": "thinking"},
                    {"type": "ended", "reason": "you hung up"}):
            await sock.send_json(msg)
            assert page_got(ch)[-1] == msg
        assert voice.queued == len(pcm(1000, 2))                                   # and the voice is untouched

    asyncio.run(go())


def test_a_channel_the_page_closed_is_read_to_its_end_then_closed() -> None:
    async def go() -> None:
        closed: list[RtcCallSocket] = []
        ch = FakeChannel("call")
        voice = VoiceTrack()
        sock = await opened(ch, voice, on_close=closed.append)
        await sock.send_bytes(pcm(100, 1))
        ch.page({"type": "end"})
        ch.close()
        assert sock.closed and closed == [sock] and voice.queued == 0
        assert await sock.recv() == (OP_TEXT, b'{"type": "end"}')                  # what it sent first still counts
        for _ in range(2):
            with pytest.raises(Closed):
                await sock.recv()
        with pytest.raises(Closed):
            await sock.send_json({"type": "state", "state": "listening"})
        with pytest.raises(Closed):
            await sock.send_bytes(b"\x00\x00")
        await sock.close()
        assert closed == [sock]                                                    # once

    asyncio.run(go())


def test_this_side_closing_drops_what_is_unread_and_closes_the_channel() -> None:
    async def go() -> None:
        ch = FakeChannel("call")
        sock = await opened(ch)
        ch.page({"type": "talk"})
        sock.audio(pcm(300, 1))
        await sock.close()
        assert ch.readyState == "closed"
        with pytest.raises(Closed):
            await asyncio.wait_for(sock.recv(), 1)
        sock.audio(pcm(100, 1))                                                    # late audio: nobody's
        ch.page({"type": "talk"})

    asyncio.run(go())


# ---- the unchanged PhoneCalls over it ----------------------------------------------------------------------------

def test_a_press_on_the_call_channel_is_heard_and_answered_in_eves_voice() -> None:
    async def go() -> None:
        brain, voice = Brain(reply="It is **noon**."), Voice(text="what time is it")
        calls = PhoneCalls(lambda _: None, brain, voice)
        ch = FakeChannel("call")
        track = VoiceTrack()
        sock = RtcCallSocket(ch, track, SECRET)
        task = asyncio.ensure_future(calls.serve_socket(sock, lambda t: t == SECRET))
        await until(lambda: {"type": "state", "state": "connected"} in page_got(ch))
        ch.page({"type": "talk"})
        sock.audio(pcm(1000, 3))
        ch.page({"type": "done"})
        await until(lambda: voice.transcribed and track.queued == 4800, 5)          # the fake voice's 100 ms
        assert voice.transcribed == [len(pcm(1000, 3))]                             # the press, whole
        assert brain.heard == ["what time is it"]
        got = page_got(ch)
        assert {"type": "heard", "text": "what time is it"} in got
        assert not any(m["type"] == "said" for m in got)                           # its sound has not started
        track.take()
        said = [m for m in page_got(ch) if m["type"] == "said"]
        assert said == [{"type": "said", "text": phone_call.speakable(brain.reply)}]   # read as words
        assert track.queued == 4800 - lite_rtc.VOICE_FRAME_BYTES
        ch.page({"type": "end"})
        await asyncio.wait_for(task, 5)
        assert page_got(ch)[-1] == {"type": "ended", "reason": phone_call.HUNG_UP}
        assert sock.closed and ch.readyState == "closed" and calls.active is None and brain.say is None

    asyncio.run(go())


def test_a_hello_that_is_not_the_peers_is_refused() -> None:
    async def go() -> None:
        brain = Brain()
        calls = PhoneCalls(lambda _: None, brain, Voice())
        ch = FakeChannel("call")
        sock = RtcCallSocket(ch, VoiceTrack(), SECRET)
        await asyncio.wait_for(calls.serve_socket(sock, lambda t: t == "another secret"), 5)
        assert page_got(ch) == [{"type": "ended", "reason": "Only eve's owner can call."}]
        assert sock.closed and brain.say is None and calls.active is None

    asyncio.run(go())
