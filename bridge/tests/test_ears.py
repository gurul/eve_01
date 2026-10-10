"""ears.py: wake-word gating, stream fan-out, and (when the model is present)
the real sherpa-onnx spotter on synthesized speech."""

from __future__ import annotations

import asyncio
import logging
import shutil
import subprocess
import sys
import wave
from pathlib import Path

import numpy as np
import pytest

from eve_bridge import ears
from eve_bridge.ears import (
    SAMPLE_RATE,
    Ears,
    EarsConfig,
    WakeGate,
    WakeSpotter,
    configured,
    keyword_id,
    keyword_line,
    model_present,
    rms_int16,
)

# ---- config -----------------------------------------------------------------

def test_configured_defaults() -> None:
    c = configured({})
    assert c.enabled and c.wake_word == "hey eve" and c.threshold == 0.25 and c.device is None


def test_configured_env_knobs() -> None:
    c = configured({"EVE_VOICE": "0", "EVE_WAKE_WORD": " hey robot ",
                    "EVE_WAKE_THRESHOLD": "0.4", "EVE_MIC": "Studio"})
    assert not c.enabled and c.wake_word == "hey robot" and c.threshold == 0.4 and c.device == "Studio"


def test_configured_bad_threshold_falls_back(caplog) -> None:
    with caplog.at_level(logging.WARNING):
        c = configured({"EVE_WAKE_THRESHOLD": "hot"})
    assert c.threshold == 0.25 and "not a number" in caplog.text


def test_keyword_id_and_line() -> None:
    assert keyword_id("Hey, Eve!") == "hey_eve"
    assert keyword_line(["▁HE", "Y", "▁BU", "D", "D", "Y"], "hey eve", 2.0, 0.25) == \
        "▁HE Y ▁BU D D Y :2 #0.25 @hey_eve"


# ---- gate -------------------------------------------------------------------

def test_gate_passes_first_hit_and_cools_down() -> None:
    g = WakeGate(cooldown_secs=2.0)
    assert g.consider("hey_eve", 10.0) == "hey_eve"
    assert g.consider("hey_eve", 11.0) is None       # inside the window
    assert g.consider("hey_eve", 12.5) == "hey_eve"
    assert g.consider(None, 20.0) is None
    assert g.hits == 3


def test_gate_suppressed_hits_are_counted_not_raised() -> None:
    g = WakeGate(suppressed=lambda: True)
    assert g.consider("hey_eve", 1.0) is None
    assert g.suppressed_hits == 1 and g.hits == 1


# ---- stream plumbing (fake spotter, no mic) -----------------------------------

class FakeSpotter:
    def __init__(self, hits_at_blocks: set[int]) -> None:
        self.hits_at_blocks = hits_at_blocks
        self.fed = 0

    def feed(self, samples, rate):
        self.fed += 1
        assert rate == SAMPLE_RATE
        return "hey_eve" if self.fed in self.hits_at_blocks else None


def _block(level: int = 500) -> np.ndarray:
    rng = np.random.default_rng(1)
    return (rng.standard_normal(2400) * level).astype(np.int16)


def test_wake_callback_runs_on_the_loop() -> None:
    async def go() -> list[str]:
        loop = asyncio.get_running_loop()
        got: list[str] = []
        e = Ears(EarsConfig(), got.append, loop, spotter=FakeSpotter({2}), clock=lambda: 0.0)
        assert e._on_block(_block()) is None
        assert e._on_block(_block()) == "hey_eve"
        await asyncio.sleep(0)
        return got
    assert asyncio.run(go()) == ["hey_eve"]


def test_subscribers_get_pcm_and_mute_the_spotter() -> None:
    async def go():
        loop = asyncio.get_running_loop()
        spot = FakeSpotter({1, 2, 3})
        e = Ears(EarsConfig(), lambda _k: None, loop, spotter=spot)
        q = e.subscribe()
        assert e.muted
        b = _block()
        assert e._on_block(b) is None
        await asyncio.sleep(0)
        raw = q.get_nowait()
        assert raw == b.tobytes() and spot.fed == 0
        e.unsubscribe(q)
        assert not e.muted
        assert e._on_block(b) == "hey_eve" and spot.fed == 1
    asyncio.run(go())


def test_slow_subscriber_drops_oldest_block() -> None:
    async def go():
        loop = asyncio.get_running_loop()
        e = Ears(EarsConfig(), lambda _k: None, loop, spotter=FakeSpotter(set()))
        q = e.subscribe()
        for i in range(105):
            e._on_block(np.full(2400, i, dtype=np.int16))
        await asyncio.sleep(0)
        assert q.qsize() == 100
        first = np.frombuffer(q.get_nowait(), dtype=np.int16)[0]
        assert first == 5            # blocks 0..4 were dropped
    asyncio.run(go())


def test_silence_warns_once_after_ten_seconds(caplog) -> None:
    async def go():
        loop = asyncio.get_running_loop()
        t = {"now": 0.0}
        e = Ears(EarsConfig(), lambda _k: None, loop, spotter=FakeSpotter(set()), clock=lambda: t["now"])
        with caplog.at_level(logging.WARNING):
            for i in range(120):
                t["now"] = i * 0.1
                e._on_block(np.zeros(2400, dtype=np.int16))
        assert caplog.text.count("silent for") == 1
        # sound again resets the timer; silence must run the full window again
        t["now"] = 13.0
        e._on_block(_block())
        assert e._silent_since is None
    asyncio.run(go())


def test_rms() -> None:
    assert rms_int16(np.zeros(10, dtype=np.int16)) == 0.0
    assert abs(rms_int16(np.full(10, 100, dtype=np.int16)) - 100.0) < 1e-6


def test_start_without_sounddevice_is_a_clean_false(monkeypatch, caplog) -> None:
    monkeypatch.setitem(sys.modules, "sounddevice", None)
    e = Ears(EarsConfig(), lambda _k: None, asyncio.new_event_loop(), spotter=FakeSpotter(set()))
    with caplog.at_level(logging.WARNING):
        assert e.start() is False
    assert "sounddevice" in caplog.text
    e.stop()


# ---- the real model on synthesized speech ---------------------------------------

_MODEL = EarsConfig().model_dir
real_model = pytest.mark.skipif(
    not (model_present(_MODEL) and sys.platform == "darwin" and shutil.which("say") and shutil.which("afconvert")),
    reason="needs the KWS model, macOS `say` and `afconvert`")


def _synth(tmp: Path, name: str, text: str, rate: int = SAMPLE_RATE) -> np.ndarray:
    aiff = tmp / f"{name}.aiff"
    wav = tmp / f"{name}.wav"
    subprocess.run(["say", "-v", "Samantha", "-o", str(aiff), text], check=True)
    subprocess.run(["afconvert", "-f", "WAVE", "-d", f"LEI16@{rate}", "-c", "1", str(aiff), str(wav)], check=True)
    with wave.open(str(wav)) as w:
        return np.frombuffer(w.readframes(w.getnframes()), dtype=np.int16)


def _run_clip(spot: WakeSpotter, pcm: np.ndarray) -> list[tuple[float, str]]:
    hits: list[tuple[float, str]] = []
    step = int(SAMPLE_RATE * 0.1)
    for i in range(0, len(pcm), step):
        r = spot.feed(pcm[i:i + step].astype(np.float32) / 32768.0, SAMPLE_RATE)
        if r:
            hits.append((round(i / SAMPLE_RATE, 1), r))
    r = spot.feed(np.zeros(SAMPLE_RATE, np.float32), SAMPLE_RATE)   # flush the lattice
    if r:
        hits.append((-1.0, r))
    return hits


# The rename to eve (2026-10-10) made the default phrase "hey eve", and the shipped spotter does not hear it in the
# synthesized clip. The owner does not use a wake word, so this stays a known gap rather than a tuned threshold.
# strict: the day the spotter hears it, these turn red and the marks come off.
hey_eve_unheard = pytest.mark.xfail(strict=True, reason='the spotter does not hear the two-syllable "hey eve"')


@real_model
@hey_eve_unheard
def test_real_model_hears_hey_eve_and_ignores_a_control_sentence(tmp_path: Path) -> None:
    cfg = EarsConfig()
    spot = WakeSpotter(cfg, keywords_file=None)
    assert "@hey_eve" in spot.keywords_file.read_text()   # the default file also lists okay/ok eve
    hits = _run_clip(spot, _synth(tmp_path, "pos", "hey eve, open my email"))
    assert [k for _, k in hits] == ["hey_eve"], hits
    assert _run_clip(spot, _synth(tmp_path, "neg", "the weather is nice today, nothing to see here")) == []


@real_model
def test_real_model_custom_phrase(tmp_path: Path) -> None:
    cfg = EarsConfig(wake_word="hello robot")
    spot = WakeSpotter(cfg, keywords_file=tmp_path / "kw.txt")
    assert "@hello_robot" in (tmp_path / "kw.txt").read_text()
    hits = _run_clip(spot, _synth(tmp_path, "pos2", "hello robot, what time is it"))
    assert [k for _, k in hits] == ["hello_robot"], hits
    # the default phrase must not fire on the custom-phrase spotter
    assert _run_clip(spot, _synth(tmp_path, "neg2", "hey eve, open my email")) == []


@real_model
@hey_eve_unheard
def test_real_model_through_ears_fanout(tmp_path: Path) -> None:
    """The whole path: 24 kHz int16 blocks → Ears → spotter → on_wake."""
    async def go() -> list[str]:
        loop = asyncio.get_running_loop()
        got: list[str] = []
        e = Ears(EarsConfig(), got.append, loop, spotter=WakeSpotter(EarsConfig()), clock=lambda: 0.0)
        pcm = _synth(tmp_path, "pos3", "hey eve")
        step = int(SAMPLE_RATE * 0.1)
        for i in range(0, len(pcm) + SAMPLE_RATE, step):
            chunk = pcm[i:i + step] if i < len(pcm) else np.zeros(step, dtype=np.int16)
            e._on_block(chunk)
        await asyncio.sleep(0)
        return got
    assert asyncio.run(go()) == ["hey_eve"]


def test_model_present_negative_control(tmp_path: Path) -> None:
    assert not model_present(tmp_path)
    assert not ears.model_present(tmp_path / "nope")


def test_default_wake_word_includes_okay_eve_aliases(tmp_path, monkeypatch):
    monkeypatch.setattr(ears, "encode_phrase", lambda phrase, _: phrase.upper().split())
    output = ears.write_keywords_file(EarsConfig(model_dir=tmp_path), tmp_path / "keywords.txt")
    lines = output.read_text().splitlines()
    assert len(lines) == 4
    assert lines[1].endswith("@okay_eve")
    assert lines[2].endswith("@ok_eve")
    assert lines[3].endswith("#0.35 @lesson")             # the lesson word rides in the same spotter, stricter
    assert lines[0].endswith("#0.25 @hey_eve")
    output = ears.write_keywords_file(EarsConfig(model_dir=tmp_path, wake_word="hello robot"), output)
    assert len(output.read_text().splitlines()) == 2
    output = ears.write_keywords_file(EarsConfig(model_dir=tmp_path, wake_word="hello robot", lesson_word=""), output)
    assert len(output.read_text().splitlines()) == 1


def test_the_lesson_word_is_configured_and_can_be_turned_off() -> None:
    assert ears.configured({}).lesson_word == "lesson"
    assert ears.configured({"EVE_LESSON_WORD": "eve lesson"}).lesson_word == "eve lesson"
    assert ears.configured({"EVE_LESSON_WORD": "off"}).lesson_word == ""
    assert ears.configured({"EVE_LESSON_WORD": ""}).lesson_word == ""
    assert ears.configured({}).lesson_threshold == 0.35
    assert ears.configured({"EVE_LESSON_THRESHOLD": "0.3"}).lesson_threshold == 0.3
    assert ears.configured({"EVE_LESSON_THRESHOLD": "high"}).lesson_threshold == 0.35
