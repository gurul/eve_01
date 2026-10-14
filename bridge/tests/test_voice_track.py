"""lite_rtc.VoiceTrack: eve's voice on the web app's audio track (docs/plans/eve-lite.md, M6). 20 ms frames of 24 kHz
mono 16-bit with rising pts, paced in real time on a fake clock, silence while nothing is queued; markers (the
``said`` captions) firing when their audio is taken; and aiortc's own track and Opus encoder taking the frames."""
from __future__ import annotations

import asyncio
import fractions
from typing import Any

import numpy as np
import pytest

from eve_bridge import lite_rtc
from eve_bridge.lite_rtc import VOICE_FRAME_BYTES, VOICE_FRAME_SAMPLES, VoiceTrack

av = pytest.importorskip("av")


class Clock:
    """A clock that moves only when the track sleeps (or a test says so)."""

    def __init__(self) -> None:
        self.t = 100.0
        self.slept: list[float] = []

    def now(self) -> float:
        return self.t

    async def sleep(self, secs: float) -> None:
        self.slept.append(secs)
        self.t += secs


def track(**kw: Any) -> tuple[VoiceTrack, Clock]:
    clock = Clock()
    return VoiceTrack(clock=clock.now, sleep=clock.sleep, **kw), clock


def samples(frame: Any) -> np.ndarray:
    return frame.to_ndarray().reshape(-1)


def ramp(n: int, start: int = 1) -> bytes:
    return np.arange(start, start + n, dtype=np.int16).tobytes()


def test_frames_are_20_ms_of_24k_mono_with_rising_pts_and_silence_when_nothing_is_queued() -> None:
    async def go() -> None:
        voice, clock = track()
        frames = [await voice.recv() for _ in range(6)]
        for i, f in enumerate(frames):
            assert (f.samples, f.format.name, f.layout.name, f.sample_rate) == (VOICE_FRAME_SAMPLES, "s16", "mono",
                                                                                lite_rtc.SAMPLE_RATE)
            assert f.pts == i * VOICE_FRAME_SAMPLES and f.time_base == fractions.Fraction(1, lite_rtc.SAMPLE_RATE)
            assert not samples(f).any()                                            # silence
        assert VOICE_FRAME_SAMPLES == 480
        assert clock.slept == pytest.approx([0.02] * 5)                             # the first at once, then real time
        assert voice.frames == 6

    asyncio.run(go())


def test_queued_speech_plays_in_order_across_frames_then_silence() -> None:
    async def go() -> None:
        voice, _ = track()
        data = ramp(700)                                                           # 1,400 bytes: a frame and a half
        for i in range(0, len(data), 101):                                         # odd pieces: a sample split in two
            voice.push(data[i:i + 101])
        assert voice.queued == len(data)
        out = np.concatenate([samples(await voice.recv()) for _ in range(3)])
        assert out[:700].tolist() == list(range(1, 701)) and not out[700:].any()
        assert voice.queued == 0
        voice.push(b"\x05")                                                        # half a sample waits for the rest
        assert voice.queued == 0
        voice.push(b"\x00")
        assert samples(await voice.recv())[0] == 5

    asyncio.run(go())


def test_a_marker_fires_when_the_audio_after_it_is_taken() -> None:
    fired: list[str] = []
    voice, _ = track()
    voice.mark(lambda: fired.append("first"))
    voice.push(ramp(VOICE_FRAME_SAMPLES))
    voice.mark(lambda: fired.append("second"))
    voice.push(ramp(VOICE_FRAME_SAMPLES, 600))
    assert fired == []                                                             # queued is not playing
    voice.take()
    assert fired == ["first"]
    voice.take()
    assert fired == ["first", "second"]
    voice.push(ramp(100))
    voice.mark(lambda: fired.append("no sound of its own"))                        # its speech failed: no audio
    voice.take()
    assert fired[-1] == "no sound of its own"                                      # it goes where its sound would
    voice.mark(lambda: fired.append("flushed"))
    voice.push(ramp(VOICE_FRAME_SAMPLES))
    voice.flush()
    assert not np.frombuffer(voice.take(), dtype=np.int16).any() and "flushed" not in fired

    def broken() -> None:
        raise RuntimeError("the channel is gone")

    voice.mark(broken)
    voice.push(ramp(VOICE_FRAME_SAMPLES))
    assert np.frombuffer(voice.take(), dtype=np.int16)[0] == 1                     # the voice goes on


def test_the_track_keeps_real_time_and_starts_again_after_a_stall() -> None:
    async def go() -> None:
        voice, clock = track()
        for _ in range(3):
            await voice.recv()
        clock.t += 0.01                                                            # a little late: catch up
        await voice.recv()
        assert clock.slept[-1] == pytest.approx(0.01)
        clock.t += 1.0                                                             # the loop stalled for a second
        n = len(clock.slept)
        f = await voice.recv()
        assert len(clock.slept) == n                                               # this one at once
        await voice.recv()
        assert clock.slept[-1] == pytest.approx(0.02)                               # then 20 ms, not a burst of 50
        assert f.pts == 4 * VOICE_FRAME_SAMPLES                                    # pts never jumps or repeats

    asyncio.run(go())


def test_speech_queued_past_the_cap_is_dropped() -> None:
    voice, _ = track(max_bytes=4 * VOICE_FRAME_BYTES)
    voice.push(bytes(4 * VOICE_FRAME_BYTES))
    voice.push(bytes(2))
    assert voice.queued == 4 * VOICE_FRAME_BYTES and voice.dropped == 2
    assert lite_rtc.MAX_VOICE_BYTES == 600 * 2 * lite_rtc.SAMPLE_RATE


def test_aiortc_takes_the_track_and_its_opus_encoder_the_frames() -> None:
    aiortc = pytest.importorskip("aiortc")
    from aiortc.codecs.opus import OpusEncoder
    from aiortc.mediastreams import MediaStreamError

    async def go() -> None:
        voice, _ = track()
        media = lite_rtc.media_track(voice)
        assert isinstance(media, aiortc.MediaStreamTrack) and media.kind == "audio"
        assert lite_rtc.media_track(voice) is not media                           # one per peer, one class
        voice.push(np.full(10 * VOICE_FRAME_SAMPLES, 3000, dtype=np.int16).tobytes())
        encoder = OpusEncoder()
        stamps = []
        for _ in range(10):
            payloads, stamp = encoder.encode(await media.recv())
            if payloads:
                stamps.append(stamp)
        assert len(stamps) >= 8 and stamps == sorted(stamps)
        assert set(np.diff(stamps).tolist()) == {960}                                 # 20 ms at Opus's 48 kHz
        media.stop()
        with pytest.raises(MediaStreamError):
            await media.recv()

    asyncio.run(go())
