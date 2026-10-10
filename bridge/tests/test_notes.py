"""notes.py: eve recording the room.

No audio leaves the machine and no model is called. The transcriber is a fake
that returns scripted text, so the tests are about the parts that decide
behaviour: when it stops, what reaches disk, and what happens when the
transcription fails.
"""

from __future__ import annotations

import asyncio
import json
import struct
import wave
from datetime import datetime
from io import BytesIO
from pathlib import Path

from eve_bridge.notes import (
    SAMPLE_RATE,
    NotesConfig,
    NotesState,
    RoomNotes,
    configured,
    make_notes_client,
    recent,
    render,
    rms,
    to_wav,
    wants_stop,
)
from eve_bridge.recall import RecallConfig

WHEN = datetime(2026, 9, 11, 14, 30)

SUMMARY = {
    "kind": "conversation",
    "title": "The servo order and the case",
    "summary": "Two people worked out what to buy and who does it.",
    "speakers": [],
    "topics": [{"heading": "The servo", "points": ["the right servo sticks under load",
                                                  "the case needs reprinting either way"]}],
    "decisions": ["order two spare SCS0009 servos"],
    "actions": [{"who": None, "what": "place the order", "when": "Friday", "at": None}],
    "questions": ["is the sticking mechanical or electrical"],
    "quotes": [],
    "ideas": [],
    "todos": [],
    "follow_ups": [],
    "unclear": ["a part number around 12 minutes"],
}


class FakeTranscriber:
    def __init__(self, texts=None, raise_on_transcribe=False, raise_on_summary=False) -> None:
        self.texts = list(texts or ["they talked about the servos"])
        self.calls: list[bytes] = []
        self.prompts: list[str] = []
        self.raise_on_transcribe = raise_on_transcribe
        self.raise_on_summary = raise_on_summary

    def transcribe(self, wav: bytes, prompt: str = "") -> str:
        self.calls.append(wav)
        self.prompts.append(prompt)
        if self.raise_on_transcribe:
            raise RuntimeError("the model refused")
        return self.texts[min(len(self.calls) - 1, len(self.texts) - 1)]

    def summarize(self, transcript: str) -> str:
        if self.raise_on_summary:
            raise RuntimeError("the model refused")
        return json.dumps(SUMMARY)


class FakeEars:
    """Hands out blocks of loud audio, like ears.py's 100 ms subscription."""

    def __init__(self, blocks: int = 400) -> None:
        self.queues: list[asyncio.Queue] = []
        self.blocks = blocks
        self.unsubscribed = 0

    def subscribe(self) -> asyncio.Queue:
        q: asyncio.Queue = asyncio.Queue()
        loud = struct.pack("<2400h", *([6000, -6000] * 1200))     # 100 ms at 24 kHz
        for _ in range(self.blocks):
            q.put_nowait(loud)
        self.queues.append(q)
        return q

    def unsubscribe(self, q) -> None:
        self.unsubscribed += 1


def _cfg(tmp_path: Path) -> RecallConfig:
    return RecallConfig(store=tmp_path / "debrief", notes=tmp_path / "notes")


def _taker(tmp_path: Path, client, secs: float = 0.5, **kw) -> RoomNotes:
    return RoomNotes(_cfg(tmp_path), NotesConfig(segment_secs=secs, **kw), client,
                     FakeEars(), wall=lambda: WHEN)


# ---- stopping ---------------------------------------------------------------------------

def test_the_stop_phrase_is_narrow_enough_for_a_real_meeting() -> None:
    """This fires on a transcript of people talking, where "stop" is a common word."""
    for said in ("ok stop taking notes", "stop the notes please", "that's enough notes",
                 "let's stop recording", "eve, stop", "no more notes thanks"):
        assert wants_stop(said), said
    for said in ("we should stop the meeting soon", "stop the car", "I took notes last time",
                 "can you note that down", "the recording studio", ""):
        assert not wants_stop(said), said


def test_hearing_the_stop_ends_the_recording(tmp_path: Path) -> None:
    client = FakeTranscriber(["alright that's enough notes"])
    taker = _taker(tmp_path, client)

    async def go():
        taker.start()
        await asyncio.wait_for(taker._task, timeout=10.0)

    asyncio.run(go())
    assert not taker.active
    assert taker.state.stopped_by == "asked out loud"
    assert taker.state.path is not None and taker.state.path.exists()


def test_stopping_by_hand_needs_no_transcript(tmp_path: Path) -> None:
    """The command and a tap on the robot are the stops with no latency."""
    client = FakeTranscriber(["a long meeting about nothing"])
    taker = _taker(tmp_path, client, secs=30.0)          # no segment would land in time

    async def go():
        taker.start()
        await asyncio.sleep(0.15)
        return await taker.stop("a tap on the robot")

    out = asyncio.run(go())
    assert out["ok"] and not taker.active
    assert taker.state.stopped_by == "a tap on the robot"


def test_a_time_cap_stops_it_even_if_nobody_does(tmp_path: Path) -> None:
    client = FakeTranscriber(["and then we talked some more"])
    taker = RoomNotes(_cfg(tmp_path), NotesConfig(segment_secs=0.4, max_minutes=0.0001),
                      client, FakeEars(), wall=lambda: WHEN)

    async def go():
        taker.start()
        await asyncio.wait_for(taker._task, timeout=10.0)

    asyncio.run(go())
    assert "cap" in taker.state.stopped_by


# ---- what reaches disk ------------------------------------------------------------------

def test_the_file_holds_the_notes_and_the_transcript(tmp_path: Path) -> None:
    client = FakeTranscriber(["the right servo sticks", "stop taking notes"])
    taker = _taker(tmp_path, client)

    async def go():
        taker.start()
        await asyncio.wait_for(taker._task, timeout=10.0)

    asyncio.run(go())
    text = taker.state.path.read_text()
    assert "source: eve-notes" in text
    assert "# The servo order and the case" in text
    assert "## Decisions" in text and "order two spare SCS0009 servos" in text
    assert "## Actions" in text and "Friday" in text
    assert "## Open questions" in text
    assert "## Transcript" in text and "the right servo sticks" in text
    assert "[00:" in text, "the transcript is stamped so you can find a moment"
    # what it could not make out is said, not guessed away
    assert "Not made out" in text and "part number" in text
    assert (taker.state.path.parent.stat().st_mode & 0o777) == 0o700


def test_the_transcript_can_be_left_out(tmp_path: Path) -> None:
    client = FakeTranscriber(["something private", "stop taking notes"])
    taker = _taker(tmp_path, client, keep_transcript=False)

    async def go():
        taker.start()
        await asyncio.wait_for(taker._task, timeout=10.0)

    asyncio.run(go())
    text = taker.state.path.read_text()
    assert "## Transcript" not in text and "something private" not in text
    assert "# The servo order" in text


def test_a_failed_write_up_still_keeps_every_word(tmp_path: Path) -> None:
    """The transcript is the irreplaceable half: it cannot be recomputed."""
    client = FakeTranscriber(["they said something important", "stop taking notes"],
                             raise_on_summary=True)
    taker = _taker(tmp_path, client)

    async def go():
        taker.start()
        await asyncio.wait_for(taker._task, timeout=10.0)

    asyncio.run(go())
    text = taker.state.path.read_text()
    assert "they said something important" in text
    assert "write-up failed" in text


def test_a_recording_with_nothing_said_leaves_no_file(tmp_path: Path) -> None:
    client = FakeTranscriber([""])
    taker = _taker(tmp_path, client)

    async def go():
        taker.start()
        await asyncio.sleep(0.4)
        await taker.stop("asked")

    asyncio.run(go())
    assert taker.state.path is None
    assert recent(_cfg(tmp_path)) == []


def test_a_failing_transcriber_loses_a_segment_not_the_session(tmp_path: Path) -> None:
    client = FakeTranscriber(raise_on_transcribe=True)
    taker = _taker(tmp_path, client)

    async def go():
        taker.start()
        await asyncio.sleep(0.5)
        await taker.stop("asked")

    asyncio.run(go())
    assert taker.state.failed >= 1
    assert not taker.active, "a failed segment must not wedge the recording"


# ---- refusals and housekeeping ----------------------------------------------------------

def test_it_refuses_to_start_without_a_key_or_a_microphone(tmp_path: Path) -> None:
    no_client = RoomNotes(_cfg(tmp_path), NotesConfig(), None, FakeEars())
    assert no_client.start()["ok"] is False
    no_ears = RoomNotes(_cfg(tmp_path), NotesConfig(), FakeTranscriber(), None)
    assert no_ears.start()["ok"] is False
    assert make_notes_client(environ={}) is None


def test_starting_twice_does_not_start_twice(tmp_path: Path) -> None:
    taker = _taker(tmp_path, FakeTranscriber(), secs=30.0)

    async def go():
        first = taker.start()
        second = taker.start()
        await taker.stop("asked")
        return first, second

    first, second = asyncio.run(go())
    assert first.get("started") and second.get("already")


def test_stopping_when_nothing_is_running_says_so(tmp_path: Path) -> None:
    taker = _taker(tmp_path, FakeTranscriber())
    assert asyncio.run(taker.stop())["ok"] is False


def test_the_microphone_is_always_handed_back(tmp_path: Path) -> None:
    """A subscription left behind would keep the wake word muted for good."""
    ears = FakeEars()
    taker = RoomNotes(_cfg(tmp_path), NotesConfig(segment_secs=0.4), FakeTranscriber(), ears,
                      wall=lambda: WHEN)

    async def go():
        taker.start()
        await asyncio.sleep(0.3)
        await taker.stop("asked")

    asyncio.run(go())
    assert ears.unsubscribed == 1


def test_a_segment_carries_the_tail_of_the_last_one(tmp_path: Path) -> None:
    """Context across a boundary is how names stay spelled the same way."""
    client = FakeTranscriber(["the SCS0009 servo", "is what sticks", "stop taking notes"])
    taker = _taker(tmp_path, client, secs=0.3)

    async def go():
        taker.start()
        await asyncio.wait_for(taker._task, timeout=10.0)

    asyncio.run(go())
    assert any("SCS0009" in p for p in client.prompts[1:]), client.prompts


# ---- audio helpers ----------------------------------------------------------------------

def test_silence_is_not_uploaded(tmp_path: Path) -> None:
    assert rms(b"\x00\x00" * 2000) == 0.0
    loud = struct.pack("<1000h", *([8000, -8000] * 500))
    assert rms(loud) > 1000


def test_wav_is_a_real_container_at_the_microphones_rate() -> None:
    pcm = struct.pack("<1000h", *([1234] * 1000))
    with wave.open(BytesIO(to_wav(pcm)), "rb") as w:
        assert w.getnchannels() == 1
        assert w.getsampwidth() == 2
        assert w.getframerate() == SAMPLE_RATE
        assert w.getnframes() == 1000


def test_render_survives_a_model_that_ignores_the_schema() -> None:
    state = NotesState(started_wall=WHEN, words=12, stopped_by="asked")
    for junk in ({}, {"title": None, "points": "not a list"}):
        text = render(junk, "[00:00] something", state)
        assert text.startswith("---") and "## Transcript" in text


def test_configured_reads_the_env() -> None:
    assert configured({}).segment_secs == 12.0
    assert configured({"EVE_NOTES_SEGMENT_SECS": "30"}).segment_secs == 30.0
    assert configured({"EVE_NOTES_SEGMENT_SECS": "9999"}).segment_secs == 60.0
    assert configured({"EVE_NOTES_KEEP_TRANSCRIPT": "0"}).keep_transcript is False


# ---- the request the API actually receives --------------------------------------------

def test_the_summary_asks_for_a_schema_and_not_a_bare_json_object() -> None:
    """A bare `json_object` format is rejected unless the word "json" appears in
    the input, and the input here is a transcript of whatever was said in the
    room. Live probe, 2026-09-11: every summary came back 400 until this was a
    schema. A fake client cannot catch that, so the request shape is asserted.
    """
    import sys
    import types

    sent: dict = {}

    class _Resp:
        output_text = '{"kind":"memo","title":"t","summary":"g","speakers":[],"topics":[],"decisions":[],' \
                      '"actions":[],"questions":[],"quotes":[],"ideas":[],"todos":[],"follow_ups":[],"unclear":[]}'

    class _Client:
        def __init__(self, **kw: object) -> None:
            self.responses = types.SimpleNamespace(create=self._create)

        def _create(self, **kw: object) -> _Resp:
            sent.update(kw)
            return _Resp()

    stub = types.ModuleType("openai")
    stub.OpenAI = _Client  # type: ignore[attr-defined]
    real = sys.modules.get("openai")
    sys.modules["openai"] = stub
    try:
        from eve_bridge.notes import SUMMARY_SCHEMA, OpenAINotesClient

        OpenAINotesClient(api_key="k").summarize("we talked about the servos")
    finally:
        if real is None:
            del sys.modules["openai"]
        else:
            sys.modules["openai"] = real

    fmt = sent["text"]["format"]
    assert fmt["type"] == "json_schema", "json_object needs the word json in the input"
    assert fmt["strict"] is True
    assert fmt["schema"] is SUMMARY_SCHEMA
    assert sent["max_output_tokens"] >= 8000, "detailed notes of a long meeting need the room"
    # every field the write-up renders must be required, or a model may omit it
    for key in ("kind", "title", "summary", "speakers", "topics", "decisions", "actions", "questions", "quotes",
                "ideas", "todos", "follow_ups", "unclear"):
        assert key in SUMMARY_SCHEMA["required"], key

    def strict(node):           # strict mode: every object closed, every property required
        if node.get("type") == "object":
            assert node["additionalProperties"] is False
            assert sorted(node["required"]) == sorted(node["properties"])
            for child in node["properties"].values():
                strict(child)
        if node.get("type") == "array":
            strict(node["items"])

    strict(SUMMARY_SCHEMA)


# ---- the seam between two segments ----------------------------------------------------

def test_the_overlap_is_not_transcribed_twice() -> None:
    """The exact line a live recording produced: a segment whose whole content was
    the last word of the one before it (2026-09-11)."""
    from eve_bridge.notes import stitch

    assert stitch("Do we need the transcript kept as well as the summary?", "Summary?") == ""
    assert stitch("...ship it on Friday.", "Friday. Guru will flash the board") \
        == "Guru will flash the board"
    # several words of overlap, and punctuation and case are not the difference
    assert stitch("we talked about the right servo", "The Right Servo, and it sticks") \
        == "and it sticks"


def test_a_segment_that_only_continues_is_left_alone() -> None:
    from eve_bridge.notes import stitch

    assert stitch("the first thing", "a different thing entirely") == "a different thing entirely"
    assert stitch("", "the very first segment") == "the very first segment"
    assert stitch("something", "") == ""
    # a word repeated inside a line is not a seam, so nothing is dropped
    assert stitch("the board", "the docs are written") == "the docs are written"


# ---- where the file goes (L6: one memory folder) -------------------------------------------

def test_meeting_notes_live_beside_the_transcripts_and_never_overwrite(tmp_path: Path) -> None:
    """Meeting notes are under transcripts/meetings/<date>/ so memory_search finds them; two meetings that
    start in the same minute under the same title are two files (they used to be one, the second winning)."""
    from eve_bridge.notes import notes_dir, write_notes

    cfg = _cfg(tmp_path)
    assert notes_dir(cfg) == cfg.transcripts_dir / "meetings"
    state = NotesState()
    state.started_wall = WHEN
    first = write_notes(cfg, SUMMARY, "first meeting words", state)
    second = write_notes(cfg, SUMMARY, "second meeting words", state)
    assert first is not None and second is not None and first != second
    assert first.parent == cfg.transcripts_dir / "meetings" / "2026-09-11"
    assert "first meeting words" in first.read_text() and "second meeting words" in second.read_text()
    assert second.name == first.stem + "-2.md"
    assert (first.stat().st_mode & 0o777) == 0o600
    assert recent(cfg)[:2] == sorted([first, second], reverse=True)
