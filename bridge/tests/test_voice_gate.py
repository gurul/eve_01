"""The voice gate (voice_gate.py): only the person who said the wake word reaches the model.

No microphone and no model: a FakeEmbedder reads the speaker off a block's first sample, so every test says
exactly whose voice each 100 ms block carries. Block sizes are small on purpose.
"""

from __future__ import annotations

import asyncio
import base64
import struct
from typing import Optional, Sequence

from eve_bridge import voice_gate as vg

BLOCK = 8                      # samples per fake block
OWNER, OTHER, THIRD = 1000, 2000, 3000
VEC = {OWNER: [1.0, 0.0, 0.0], OTHER: [0.0, 1.0, 0.0], THIRD: [0.0, 0.0, 1.0]}


def speech(who: int, n: int = 1) -> list[bytes]:
    """`n` loud blocks whose samples all carry the speaker's number (rms ≈ who/32768 ≥ speech_rms)."""
    return [struct.pack(f"<{BLOCK}h", *([who] * BLOCK)) for _ in range(n)]


def quiet(n: int = 1) -> list[bytes]:
    return [bytes(BLOCK * 2) for _ in range(n)]


class FakeEmbedder:
    """The mean voice of the audio: a mix of speakers embeds between them, as a real mixture does."""

    def __init__(self, fail: bool = False) -> None:
        self.calls, self.fail = 0, fail

    def embed(self, pcm: bytes) -> Optional[Sequence[float]]:
        self.calls += 1
        if self.fail:
            raise RuntimeError("onnx went away")
        total = [0.0, 0.0, 0.0]
        for (v,) in struct.iter_unpack("<h", pcm):
            if v in VEC:
                total = [a + b for a, b in zip(total, VEC[v], strict=True)]
        return total if any(total) else None


class Clock:
    def __init__(self) -> None:
        self.t = 0.0

    def __call__(self) -> float:
        return self.t


def gate(mode: str = "on", embedder=None, clock: Optional[Clock] = None, **cfg) -> tuple[vg.SpeakerGate, Clock]:
    clock = clock or Clock()
    config = vg.GateConfig(mode=mode, speech_rms=0.01, **cfg)
    g = vg.SpeakerGate(config, embedder if embedder is not None else FakeEmbedder(), clock, block_bytes=BLOCK * 2)
    assert g.enrol_seed(b"".join(quiet(5) + speech(OWNER, 7) + quiet(2)))
    return g, clock


def feed(g: vg.SpeakerGate, clock: Clock, blocks: list[bytes]) -> list[bytes]:
    out: list[bytes] = []
    for b in blocks:
        clock.t += vg.BLOCK_SECS
        out.extend(g.process(b))
    return out


def lock(g: vg.SpeakerGate, clock: Clock) -> None:
    """The owner finishes their first sentence: 3 s, enough to leave PROVISIONAL."""
    feed(g, clock, speech(OWNER, 30) + quiet(5))
    assert g.state == "locked", g.summary()


def silent(blocks: list[bytes]) -> int:
    return sum(1 for b in blocks if not any(b))


# ---- configuration ---------------------------------------------------------------------------------


def test_it_ships_off_and_off_means_no_gate_at_all() -> None:
    assert vg.DEFAULT_MODE == "off" and vg.configured({}).mode == "off"
    assert vg.configured({"EVE_VOICE_GATE": "shadow"}).mode == "shadow"
    assert vg.configured({"EVE_VOICE_GATE": "loud"}).mode == "off"
    assert vg.build(vg.GateConfig(mode="off"), b"x" * 64, embedder=FakeEmbedder()) is None
    # no wake audio — a conversation the lesson toggle opened — is ungated whatever the mode
    assert vg.build(vg.GateConfig(mode="on"), b"", embedder=FakeEmbedder()) is None


def test_the_seed_is_the_speech_in_the_wake_audio_not_the_room_before_it() -> None:
    g, _ = gate()
    assert g.state == "provisional" and abs(g.stats.enrolled_secs - 0.7) < 1e-6
    assert vg.trim_to_speech(b"".join(quiet(6)), 0.01, BLOCK * 2) == b""


# ---- the stream --------------------------------------------------------------------------------------


def test_the_owner_is_forwarded_whole_in_order_after_a_short_hold() -> None:
    g, clock = gate()
    said = speech(OWNER, 20) + quiet(5)
    out = feed(g, clock, said)
    assert out == said                                   # every sample, in order: held, then burst, then live
    assert g.stats.accepted == 1 and g.stats.secs_silenced == 0
    first = [g.process(b) for b in speech(OWNER, 5)]
    assert first == [[], [], [], [], []]                 # nothing leaves while the first 0.6 s is being judged


def test_another_voice_becomes_silence_of_the_same_length() -> None:
    g, clock = gate()
    lock(g, clock)
    tv = speech(OTHER, 30) + quiet(5)
    out = feed(g, clock, tv)
    assert len(out) == len(tv) and all(len(a) == len(b) for a, b in zip(out, tv, strict=True))
    assert silent(out) == len(out)                       # not one sample of it reaches the model
    assert g.stats.rejected == 1 and g.stats.decisions[-1].why == "mismatch"


def test_the_positive_control_the_same_audio_with_the_gate_off_is_forwarded() -> None:
    g, clock = gate(mode="off")
    tv = speech(OTHER, 30) + quiet(5)
    assert feed(g, clock, tv) == tv


def test_shadow_judges_everything_and_changes_nothing() -> None:
    g, clock = gate(mode="shadow")
    lock(g, clock)
    tv = speech(OTHER, 30) + quiet(5)
    assert feed(g, clock, tv) == tv                      # heard exactly as the mic heard it
    assert g.stats.rejected == 1 and g.stats.secs_silenced > 2.5      # …and counted as `on` would have


def test_quiet_room_noise_passes_untouched() -> None:
    g, clock = gate()
    lock(g, clock)
    hum = [struct.pack(f"<{BLOCK}h", *([40] * BLOCK)) for _ in range(10)]
    assert feed(g, clock, hum) == hum and g.stats.segments == 1       # only the owner's segment so far


def test_the_owner_speaking_up_mid_segment_is_heard_and_a_takeover_is_cut() -> None:
    g, clock = gate()
    lock(g, clock)
    out = feed(g, clock, speech(OTHER, 15) + speech(OWNER, 40) + quiet(5))
    assert silent(out[:15]) == 15 and any(any(b) for b in out[-25:])  # silenced, then forwarded once it is the owner
    g2, clock2 = gate()
    lock(g2, clock2)
    out2 = feed(g2, clock2, speech(OWNER, 15) + speech(OTHER, 50) + quiet(5))
    assert any(out2[0]) and silent(out2[-20:]) == 20                  # forwarded, then cut when someone took over


# ---- what must never be lost -----------------------------------------------------------------------


def test_a_word_too_short_to_judge_passes() -> None:
    g, clock = gate()
    lock(g, clock)
    clock.t += 5.0                                       # long after anyone last spoke
    word = speech(OTHER, 3) + quiet(5)
    assert feed(g, clock, word) == word and g.stats.decisions[-1].why == "too_short"


def test_a_fragment_right_after_judged_speech_gets_that_verdict() -> None:
    g, clock = gate()
    lock(g, clock)
    tv = speech(OTHER, 20) + quiet(5) + speech(OTHER, 3) + quiet(5)      # a halting voice: the fragment is the TV's too
    out = feed(g, clock, tv)
    assert silent(out) == len(out) and g.stats.decisions[-1].why == "continues"
    owner = speech(OWNER, 20) + quiet(5) + speech(OWNER, 3) + quiet(5)
    assert feed(g, clock, owner) == owner


def test_nothing_is_rejected_on_the_first_fraction_of_a_second() -> None:
    g, clock = gate()
    lock(g, clock)
    # 0.6 s of someone else, then the owner: the first judgement is "not them", so the gate holds on and
    # judges again on 1.5 s — by then mostly the owner — instead of silencing on too little.
    out = feed(g, clock, speech(OTHER, 6) + speech(OWNER, 24) + quiet(5))
    assert len(out) == 35 and any(out[0]) and g.stats.decisions[-1].accepted


def test_while_an_answer_is_awaited_a_short_reply_is_never_held_back() -> None:
    g, clock = gate()
    lock(g, clock)
    g.lenient = True
    reply = speech(OTHER, 9) + quiet(5)                  # 0.9 s, and it does not sound like the owner (a cold, a cough)
    assert feed(g, clock, reply) == reply and g.stats.decisions[-1].why == "lenient"
    g.lenient = False
    assert silent(feed(g, clock, reply)) == len(reply)   # without the question pending, it is judged as usual


def test_notes_are_not_gated_a_think_aloud_lesson_hears_everyone() -> None:
    g, clock = gate()
    lock(g, clock)
    g.bypass = True
    room = speech(OTHER, 20) + speech(THIRD, 20) + quiet(5)
    calls = g.embedder.calls
    assert feed(g, clock, room) == room and g.embedder.calls == calls    # forwarded whole, and nobody was judged


# ---- failing open --------------------------------------------------------------------------------------


def test_a_gate_that_cannot_judge_is_todays_behaviour() -> None:
    tv = speech(OTHER, 30) + quiet(5)
    for g in (vg.SpeakerGate(vg.GateConfig(mode="on", speech_rms=0.01), None, Clock(), block_bytes=BLOCK * 2),
              vg.build(vg.GateConfig(mode="on", speech_rms=0.01), b"".join(quiet(10)), embedder=FakeEmbedder())):
        assert g is not None and g.state == "open"       # no embedder; a wake snapshot with no voice in it
        assert feed(g, Clock(), tv) == tv
    broken, clock = gate()
    lock(broken, clock)
    broken.embedder = FakeEmbedder(fail=True)
    out = feed(broken, clock, tv)
    assert out == tv and broken.stats.opened.startswith("error")      # nothing held back is lost either


def test_when_nothing_has_ever_matched_the_enrolment_is_what_is_wrong() -> None:
    g, clock = gate(open_after_rejected_secs=6.0)
    g._woke_at = -100.0                                   # long after the wake: no first-turn leniency
    g.stats.enrolled_secs = 5.0                           # a seed that was really the TV
    feed(g, clock, speech(OTHER, 35) + quiet(5))
    assert g.state == "locked" and g.stats.rejected == 1
    feed(g, clock, speech(OTHER, 35) + quiet(5))
    assert g.state == "open" and "nothing matched" in g.stats.opened
    again = speech(OTHER, 10) + quiet(5)
    assert feed(g, clock, again) == again


def test_a_tv_that_talks_for_a_minute_never_opens_a_gate_that_has_heard_its_owner() -> None:
    g, clock = gate(open_after_rejected_secs=6.0)
    lock(g, clock)
    for _ in range(12):
        out = feed(g, clock, speech(OTHER, 50) + quiet(5))
        assert silent(out) == len(out)
    assert g.state == "locked" and g.stats.rejected == 12


def test_another_voice_cannot_walk_the_enrolment_away_from_the_seed() -> None:
    g, clock = gate()
    lock(g, clock)
    before = list(g._centroid)
    feed(g, clock, speech(OTHER, 40) + quiet(5))
    assert g._centroid == before
    g.reset()
    assert g._seed is None and g._centroid is None and g.state == "open"   # nothing of a voice is kept


# ---- the wake ring and the session ---------------------------------------------------------------------


def test_the_wake_ring_keeps_only_the_last_seconds_in_ram() -> None:
    ring = vg.WakeRing(secs=0.3)
    for n in range(5):
        ring.add(bytes([n]) * 4)
    assert ring.snapshot() == bytes([2]) * 4 + bytes([3]) * 4 + bytes([4]) * 4
    ring.clear()
    assert ring.snapshot() == b""


def test_the_session_appends_what_the_gate_lets_through() -> None:
    from test_voice_agent import FakeConnection, _session  # noqa: PLC0415 — the session fakes live there

    g, clock = gate()
    lock(g, clock)
    conn = FakeConnection([])
    session, _states, mic = _session(conn, [], gate=g)

    async def run() -> list[bytes]:
        for b in speech(OTHER, 12) + quiet(5):
            clock.t += vg.BLOCK_SECS
            mic.put_nowait(b)
        pump = asyncio.create_task(session._pump_mic())
        while not mic.empty():
            await asyncio.sleep(0)
        await asyncio.sleep(0)
        pump.cancel()
        return [base64.b64decode(a["audio"]) for kind, a in conn.sent if kind == "session.input_audio.append"]

    sent = asyncio.run(run())
    assert sent and silent(sent) == len(sent)
