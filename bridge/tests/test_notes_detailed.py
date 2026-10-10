"""notes.py, detailed notes (owner, 2026-10-04): the write-up's shape, the full transcript first, and who spoke.

Every model here is a fake: no audio leaves the machine and nothing is called, except the one test marked
``live``, which sends a synthetic two-voice clip made with macOS ``say`` to the real speaker pass (run it with
EVE_LIVE=1). No real voice is ever written to the repository: the live test makes its clip in a temporary
folder."""

from __future__ import annotations

import asyncio
import base64
import io
import json
import shutil
import struct
import subprocess
import wave
from datetime import datetime
from pathlib import Path

import pytest
from test_notes import FakeEars, FakeTranscriber

from eve_bridge import phone_call
from eve_bridge.notes import (
    SAMPLE_RATE,
    SUMMARY_PROMPT,
    Line,
    NotesConfig,
    NotesState,
    RoomNotes,
    SpeakerNames,
    check_quotes,
    clean_summary,
    configured,
    diarize_recording,
    encode_pieces,
    note_sections,
    render,
    render_lines,
    windows,
    write_up,
)
from eve_bridge.recall import RecallConfig

WHEN = datetime(2026, 10, 4, 9, 5)
LOUD = struct.pack("<2400h", *([6000, -6000] * 1200))        # 100 ms at 24 kHz

LINES = [
    Line(3.0, "Good morning. I think we should ship on Friday the twelfth.", "Speaker 1"),
    Line(9.0, "Friday works, but the right servo still sticks under load.", "Speaker 2"),
    Line(15.0, "Then order two spare servos today for forty dollars.", "Speaker 1"),
]

CONVERSATION = {
    "kind": "conversation",
    "title": "Shipping the robot Friday",
    "summary": "Two people agreed to ship on Friday the twelfth once spare servos are ordered.",
    "speakers": [{"label": "Speaker 1", "name": None, "about": "led and set the date"},
                 {"label": "Speaker 2", "name": None, "about": "raised the servo"}],
    "topics": [{"heading": "Ship date", "points": ["Friday the twelfth", "the right servo sticks under load"]}],
    "decisions": ["ship on Friday the twelfth"],
    "actions": [{"who": "Speaker 1", "what": "order two spare servos", "when": "today", "at": "00:15"}],
    "questions": ["is the sticking mechanical"],
    "quotes": [{"at": "00:15", "speaker": "Speaker 1", "text": "order two spare servos today for forty dollars"},
               {"at": "00:09", "speaker": "Speaker 2", "text": "the servo is broken beyond repair"}],
    "ideas": [],
    "todos": [],
    "follow_ups": ["check the servo's datasheet"],
    "unclear": [],
}

MEMO = {
    "kind": "memo",
    "title": "Ideas for the robot case",
    "summary": "Thinking out loud about the case.",
    "speakers": [{"label": "Speaker 1", "name": None, "about": None}],
    "topics": [{"heading": "The case", "points": ["print it in PETG"]}],
    "decisions": [],
    "actions": [],
    "questions": [],
    "quotes": [],
    "ideas": ["a magnetic lid"],
    "todos": ["buy PETG"],
    "follow_ups": [],
    "unclear": [],
}


def _cfg(tmp_path: Path) -> RecallConfig:
    return RecallConfig(store=tmp_path / "debrief", notes=tmp_path / "notes")


def _wav_secs(data: bytes) -> float:
    with wave.open(io.BytesIO(data)) as w:
        return w.getnframes() / w.getframerate()


# ---- the shape --------------------------------------------------------------------------

def test_schema_and_render_round_trip() -> None:
    """What the model returns survives JSON, the cleaning and the file: every section, every number."""
    back = clean_summary(json.loads(json.dumps(CONVERSATION)), LINES)
    assert back["kind"] == "conversation" and back["title"] == "Shipping the robot Friday"
    assert back["actions"] == CONVERSATION["actions"]
    state = NotesState(started_wall=WHEN, words=30, stopped_by="asked")
    text = render(back, render_lines(LINES), state, speakers=["Speaker 1", "Speaker 2"], diarized="yes")
    assert "status: notes" in text and "kind: conversation" in text and "speakers: 2" in text
    assert "# Shipping the robot Friday" in text and "Friday the twelfth once spare servos" in text
    for heading in ("## Speakers", "## Topics", "### Ship date", "## Decisions", "## Actions", "## Open questions",
                    "## Quotes", "## Follow-ups", "## Transcript"):
        assert heading in text, heading
    assert "- Speaker 1: led and set the date" in text
    assert "- Speaker 1: order two spare servos (by today) [00:15]" in text
    assert "[00:09] Speaker 2: Friday works" in text
    assert "forty dollars" in text


def test_a_conversation_and_a_memo_get_their_own_sections() -> None:
    conv = [h for h, _ in note_sections(clean_summary(CONVERSATION, LINES), ["Speaker 1", "Speaker 2"])]
    assert conv[:5] == ["Speakers", "Topics", "Decisions", "Actions", "Open questions"]
    memo = [h for h, _ in note_sections(clean_summary(MEMO, LINES), ["Speaker 1"])]
    assert memo[:3] == ["Topics", "Ideas", "To-dos"], "a memo leads with its ideas and to-dos"
    assert "Speakers" not in memo, "one voice talking to itself needs no speaker list"
    # a voice the pass heard is listed even when the write-up says nothing about it
    rows = dict(note_sections(clean_summary(CONVERSATION, LINES), ["Speaker 1", "Speaker 2", "Speaker 3"]))
    assert rows["Speakers"][-1] == "Speaker 3"


def test_the_prompt_states_its_values_as_sets_and_carries_no_banned_title() -> None:
    assert "exactly one of two values" in SUMMARY_PROMPT
    assert "conversation:" in SUMMARY_PROMPT and "memo:" in SUMMARY_PROMPT
    assert "Not \"" not in SUMMARY_PROMPT and "Meeting notes" not in SUMMARY_PROMPT
    assert "instruction" in SUMMARY_PROMPT and "Never invent" in SUMMARY_PROMPT


def test_a_quote_that_is_not_in_its_line_is_dropped() -> None:
    kept = check_quotes(CONVERSATION["quotes"] + [
        {"at": "00:03", "speaker": "Speaker 2", "text": "we should ship on Friday"},     # the wrong voice
        {"at": "00:09", "speaker": None, "text": "ship on Friday the twelfth"},         # the wrong line
        {"at": "[00:03]", "speaker": None, "text": "“I think we should ship on Friday”"},
        {"at": "00:15", "speaker": "Speaker 1", "text": "order three spare servos"},    # a changed word
        "not a quote",
    ], LINES)
    assert [q["text"] for q in kept] == ["order two spare servos today for forty dollars",
                                        "I think we should ship on Friday"]
    assert kept[1]["speaker"] == "Speaker 1", "the line's own speaker, not the model's"
    # an undiarized transcript: the quote carries no speaker
    assert check_quotes([{"at": "00:01", "speaker": "Speaker 1", "text": "hello there"}],
                        [Line(1.0, "Hello there, everyone.")]) == [
        {"at": "00:01", "speaker": None, "text": "hello there"}]


# ---- long recordings --------------------------------------------------------------------

class WindowClient:
    def __init__(self, combine_fails: bool = False) -> None:
        self.inputs: list[str] = []
        self.combined: list[str] = []
        self.combine_fails = combine_fails

    def summarize(self, transcript: str) -> str:
        self.inputs.append(transcript)
        n = len(self.inputs)
        return json.dumps({**MEMO, "kind": "conversation", "title": f"Part {n}", "summary": f"part {n}",
                           "topics": [{"heading": f"Topic {n}", "points": [f"point {n}"]}],
                           "decisions": [f"decision {n}", "shared decision"]})

    def combine(self, partials: str) -> str:
        self.combined.append(partials)
        if self.combine_fails:
            raise RuntimeError("the model refused")
        return json.dumps({**CONVERSATION, "quotes": []})


def _long(minutes: int) -> list[Line]:
    return [Line(t * 20.0, f"line {t}", "Speaker 1") for t in range(minutes * 3)]


def test_a_long_recording_is_written_up_in_overlapping_windows_then_merged() -> None:
    lines = _long(55)                                       # past the 40 minute threshold
    client = WindowClient()
    out = write_up(client, lines)
    assert len(client.inputs) == 6 == len(windows(lines)), "ten-minute windows over 55 minutes"
    assert client.inputs[1].startswith("(Part 2 of 6 of one longer recording, ")
    # 30 s of overlap: the last lines of a window open the next one
    assert "[09:40] Speaker 1: line 29" in client.inputs[0] and "[09:40] Speaker 1: line 29" in client.inputs[1]
    assert "[10:00]" not in client.inputs[0]
    assert len(json.loads(client.combined[0])) == 6
    assert out["title"] == "Shipping the robot Friday"


def test_a_failed_merge_is_done_in_code_and_a_short_recording_is_one_call() -> None:
    client = WindowClient(combine_fails=True)
    out = write_up(client, _long(45))
    assert [t["heading"] for t in out["topics"]] == [f"Topic {n}" for n in range(1, 6)]
    assert out["decisions"].count("shared decision") == 1, "the overlap's repeats are merged away"
    assert out["title"] == "Part 1"
    short = WindowClient()
    write_up(short, _long(39))
    assert len(short.inputs) == 1 and not short.combined
    assert not short.inputs[0].startswith("(Part")


# ---- the transcript reaches disk first --------------------------------------------------

def test_the_transcript_is_on_disk_before_the_summary_and_survives_its_crash(tmp_path: Path) -> None:
    seen: dict = {}

    class Crashing(FakeTranscriber):
        def summarize(self, transcript: str) -> str:
            path = taker.state.path
            seen["text"] = path.read_text() if path is not None else ""
            seen["mode"] = path.stat().st_mode & 0o777 if path is not None else 0
            raise RuntimeError("the summary crashed")

    client = Crashing(["the right servo sticks", "stop taking notes"])
    taker = RoomNotes(_cfg(tmp_path), NotesConfig(segment_secs=0.5), client, FakeEars(), wall=lambda: WHEN)

    async def go():
        taker.start()
        await asyncio.wait_for(taker._task, timeout=10.0)

    asyncio.run(go())
    assert "status: transcript" in seen["text"] and "the right servo sticks" in seen["text"]
    assert "Writing these up" in seen["text"] and seen["mode"] == 0o600
    final = taker.state.path.read_text()
    assert "the right servo sticks" in final and "write-up failed" in final
    assert not list(taker.state.path.parent.glob(".*.tmp")), "no temporary file is left behind"


# ---- who spoke --------------------------------------------------------------------------

def _pcm(secs: float) -> bytes:
    return LOUD * int(secs * 10)


class DiarizeClient:
    """Scripted pieces: each call returns the next list of segments, or raises when it is an exception."""

    def __init__(self, script) -> None:
        self.script = list(script)
        self.calls: list[dict] = []

    def diarize(self, audio, filename, mime, names=(), refs=()):
        self.calls.append({"audio": audio, "filename": filename, "mime": mime, "names": list(names),
                           "refs": list(refs)})
        item = self.script.pop(0)
        if isinstance(item, Exception):
            raise item
        return item


def _seg(speaker: str, start: float, end: float, text: str) -> dict:
    return {"speaker": speaker, "start": start, "end": end, "text": text}


def test_labels_stay_one_voice_across_pieces_through_known_speakers() -> None:
    audio = _pcm(75.0)
    client = DiarizeClient([
        [_seg("A", 0.0, 4.0, "first words"), _seg("B", 4.5, 6.0, "short"), _seg("B", 6.2, 9.0, "and more"),
         _seg("A", 10.0, 12.0, "back to A")],
        [_seg("Speaker 2", 0.0, 3.0, "B again"), _seg("Speaker 1", 3.0, 6.0, "A again"),
         _seg("A", 7.0, 9.0, "a new voice")],                   # labels reset: this A is not the first A
        RuntimeError("down"), RuntimeError("still down"),
    ])

    def read(start: float, end: float) -> bytes:
        return audio[int(start * SAMPLE_RATE) * 2:int(end * SAMPLE_RATE) * 2]

    d = diarize_recording(read, 75.0, client, piece_secs=30.0, which=lambda _: None)
    assert [c["names"] for c in client.calls] == [[], ["Speaker 1", "Speaker 2"], ["Speaker 1", "Speaker 2"], []]
    for ref in client.calls[1]["refs"]:
        assert ref.startswith("data:audio/wav;base64,")
        assert 2.0 <= _wav_secs(base64.b64decode(ref.split(",", 1)[1])) <= 10.0
    # B's clip is its two consecutive segments together (4.5 to 9.0 s): the longest run, at least 2 s
    assert abs(_wav_secs(base64.b64decode(client.calls[1]["refs"][1].split(",", 1)[1])) - 4.5) < 0.01
    assert d.speakers == ["Speaker 1", "Speaker 2", "Speaker 3"]
    assert [(round(ln.at), ln.speaker) for ln in d.lines] == [
        (0, "Speaker 1"), (4, "Speaker 2"), (10, "Speaker 1"), (30, "Speaker 2"), (33, "Speaker 1"),
        (37, "Speaker 3")]
    assert d.lines[1].text == "short and more", "one voice's consecutive segments are one line"
    assert d.status == "partial" and d.failed == [(60.0, 75.0)]


def test_the_owner_is_reference_one_on_every_piece() -> None:
    audio = _pcm(50.0)
    client = DiarizeClient([[_seg("Ada", 0.0, 3.0, "mine"), _seg("A", 3.0, 8.0, "theirs")],
                            [_seg("Ada", 0.0, 2.0, "mine again"), _seg("Speaker 1", 2.0, 5.0, "theirs again")]])

    def read(start: float, end: float) -> bytes:
        return audio[int(start * SAMPLE_RATE) * 2:int(end * SAMPLE_RATE) * 2]

    d = diarize_recording(read, 50.0, client, ("Ada", "data:audio/wav;base64,AAAA"), piece_secs=25.0,
                          which=lambda _: None)
    assert client.calls[0]["names"] == ["Ada"] and client.calls[0]["refs"] == ["data:audio/wav;base64,AAAA"]
    assert client.calls[1]["names"] == ["Ada", "Speaker 1"]
    assert d.speakers == ["Ada", "Speaker 1"] and d.status == "yes"


def test_a_bad_reference_is_dropped_and_the_piece_tried_again() -> None:
    client = DiarizeClient([RuntimeError("bad reference"), [_seg("A", 0.0, 3.0, "hello")]])
    d = diarize_recording(lambda s, e: _pcm(e - s), 10.0, client, ("Ada", "data:audio/wav;base64,AAAA"),
                          which=lambda _: None)
    assert [c["names"] for c in client.calls] == [["Ada"], []]
    assert d.speakers == ["Speaker 1"] and d.status == "yes"


def test_speaker_names_follow_first_appearance() -> None:
    names = SpeakerNames(("Ada",))
    assert [names.name(0, "B"), names.name(0, "A"), names.name(0, "B"), names.name(1, "B"),
            names.name(1, "Ada", ("Ada",))] == ["Speaker 1", "Speaker 2", "Speaker 1", "Speaker 3", "Ada"]


def test_without_afconvert_or_when_it_fails_the_pieces_go_as_wav_under_the_limit() -> None:
    pcm = _pcm(30.0)
    limit = 500_000                                           # about 10 s of 24 kHz WAV

    def broken(*a, **k):
        raise subprocess.CalledProcessError(1, "afconvert")

    for which, run in ((lambda _: None, subprocess.run), (lambda _: "/usr/bin/afconvert", broken)):
        pieces = list(encode_pieces(pcm, 100.0, max_bytes=limit, which=which, run=run))
        assert len(pieces) == 3
        assert all(mime == "audio/wav" and len(data) <= limit for _, _, data, _, mime in pieces)
        assert pieces[0][0] == 100.0 and abs(pieces[-1][1] - 130.0) < 0.01
        assert abs(sum(_wav_secs(p[2]) for p in pieces) - 30.0) < 0.01

    def fake_afconvert(cmd, **k):
        Path(cmd[-1]).write_bytes(b"m4a bytes")

    [piece] = list(encode_pieces(pcm, 0.0, which=lambda _: "/usr/bin/afconvert", run=fake_afconvert))
    assert piece[2:] == (b"m4a bytes", "piece.m4a", "audio/mp4")
    # without afconvert the pieces are planned short enough for WAV
    client = DiarizeClient([[_seg("A", 0, 1, "x")]] * 3)
    diarize_recording(lambda s, e: _pcm(e - s), 25.0, client, max_bytes=limit, which=lambda _: None)
    assert len(client.calls) == 3


@pytest.mark.skipif(shutil.which("afconvert") is None, reason="macOS afconvert")
def test_afconvert_makes_a_small_aac_piece() -> None:
    [piece] = list(encode_pieces(_pcm(20.0), 0.0))
    assert piece[4] == "audio/mp4" and 0 < len(piece[2]) < len(_pcm(20.0)) / 5


class SpeakingClient(FakeTranscriber):
    def __init__(self) -> None:
        super().__init__(["we ship on friday", "the servo sticks", "stop taking notes"])
        self.diarized: list[dict] = []
        self.summaries: list[str] = []

    def diarize(self, audio, filename, mime, names=(), refs=()):
        self.diarized.append({"mime": mime, "names": list(names)})
        return [_seg("A", 0.0, 3.0, "we ship on friday"), _seg("B", 3.0, 6.0, "the servo sticks"),
                _seg("A", 6.0, 8.0, "stop taking notes")]

    def summarize(self, transcript: str) -> str:
        self.summaries.append(transcript)
        return json.dumps({**CONVERSATION, "quotes": [{"at": "00:03", "speaker": "Speaker 2",
                                                       "text": "the servo sticks"}]})


def test_a_session_ends_with_the_speaker_pass_and_its_count(tmp_path: Path) -> None:
    client = SpeakingClient()
    taker = RoomNotes(_cfg(tmp_path), NotesConfig(segment_secs=0.5), client, FakeEars(), wall=lambda: WHEN)

    async def go():
        taker.start()
        await asyncio.wait_for(taker._task, timeout=20.0)

    asyncio.run(go())
    assert len(client.diarized) == 1
    assert client.summaries[0].startswith("[00:00] Speaker 1: we ship on friday"), "notes from the diarized text"
    text = taker.state.path.read_text()
    assert "speakers: 2" in text and "diarized: yes" in text and "status: notes" in text
    assert "[00:03] Speaker 2: the servo sticks" in text
    assert "## Speakers" in text and "## Quotes" in text and '"the servo sticks" (Speaker 2, 00:03)' in text
    assert taker.speakers == ["Speaker 1", "Speaker 2"] and taker.diarized == "yes"
    assert taker.state.path.name.endswith("-shipping-the-robot-friday.md"), "named for its title at the end"
    assert taker._spool_dir is None, "the kept audio is gone once the notes are written"


def test_the_pass_can_be_turned_off_and_the_owner_named(tmp_path: Path) -> None:
    cfg = configured({"EVE_NOTES_DIARIZE": "0"})
    assert cfg.diarize is False
    client = SpeakingClient()
    taker = RoomNotes(_cfg(tmp_path), cfg, client, FakeEars(), wall=lambda: WHEN)

    async def go():
        taker.start()
        await asyncio.sleep(0.3)
        await taker.stop("asked")

    asyncio.run(go())
    assert client.diarized == [] and "speakers:" not in taker.state.path.read_text()
    on = configured({"EVE_NOTES_OWNER_VOICE": "~/me.wav", "EVE_NOTES_OWNER_NAME": "  Ada  "})
    assert on.diarize and on.owner_voice == "~/me.wav" and on.owner_name == "Ada"
    assert configured({}).owner_name == "Owner"


def test_a_failed_pass_keeps_the_live_transcript(tmp_path: Path) -> None:
    class Failing(SpeakingClient):
        def diarize(self, *a, **k):
            raise RuntimeError("the pass failed")

    client = Failing()
    taker = RoomNotes(_cfg(tmp_path), NotesConfig(segment_secs=0.5), client, FakeEars(), wall=lambda: WHEN)

    async def go():
        taker.start()
        await asyncio.wait_for(taker._task, timeout=20.0)

    asyncio.run(go())
    text = taker.state.path.read_text()
    assert "diarized: no" in text and "speakers:" not in text
    assert "] we ship on friday" in text and "no speaker labels" in text


# ---- the text to the owner --------------------------------------------------------------

def test_the_texted_notes_carry_every_section_in_messages_under_the_limit() -> None:
    big = clean_summary({**CONVERSATION, "topics": [
        {"heading": f"Topic {n}", "points": [f"point {n}.{k} " + "detail " * 30 for k in range(6)]}
        for n in range(12)]}, LINES)
    pieces = phone_call.notes_messages(big, 40, 63.0, Path("0905-shipping.md"), speakers=["Speaker 1", "Speaker 2"])
    assert len(pieces) > 1 and all(len(p) <= phone_call.NOTES_PIECE_CHARS for p in pieces)
    assert pieces[0].startswith("Notes from your phone: Shipping the robot Friday (63 min, 2 speakers).")
    joined = "\n".join(pieces)
    for needle in ("Speakers:", "Topic 11:", "point 11.5", "Decisions:", "Actions:", "Open questions:",
                   "Quotes:", "Follow-ups:"):
        assert needle in joined, needle
    assert pieces[-1].endswith("The full transcript is in 0905-shipping.md, in the meeting notes.")


def test_the_texted_notes_cap_out_at_the_file_and_carry_the_words_when_there_is_no_file() -> None:
    huge = clean_summary({**CONVERSATION, "topics": [
        {"heading": f"T{n}", "points": ["x " * 1500]} for n in range(30)]}, LINES)
    pieces = phone_call.notes_messages(huge, 9, 90.0, Path("f.md"))
    assert len(pieces) <= phone_call.NOTES_MAX_PIECES + 1 and "rest of the notes are in the file" in "".join(pieces)
    assert all(len(p) <= phone_call.NOTES_PIECE_CHARS for p in pieces)
    alone = phone_call.notes_messages({}, 2, 3.0, None, "[00:00] first words\n[00:12] second words")
    joined = "\n".join(alone)
    assert "only copy" in joined and "first words" in joined and "second words" in joined
    assert "speakers" not in alone[0], "no count when there was no speaker pass"


# ---- the real speaker pass --------------------------------------------------------------

@pytest.mark.live
def test_live_one_real_diarize_call_on_two_synthetic_voices(tmp_path: Path) -> None:
    """Two macOS voices, four lines, one real request: grounds the reply's shape. The clip is synthetic and
    lives in tmp_path only."""
    import os

    from eve_bridge.envfile import load_env_file
    from eve_bridge.notes import OpenAINotesClient

    if not callable(getattr(OpenAINotesClient, "diarize", None)):
        pytest.skip("the speaker pass is off: OpenRouter has no diarizing transcriber (docs/openrouter.md)")
    if shutil.which("say") is None:
        pytest.skip("needs macOS say")
    load_env_file()
    if not os.environ.get("OPENAI_API_KEY"):
        pytest.skip("no OPENAI_API_KEY")
    lines = [("Samantha", "Good morning. I think we should ship the robot on Friday, the twelfth."),
             ("Daniel", "Friday works for me, but the right servo still sticks under load."),
             ("Samantha", "Then order two spare servos today, and I will reprint the case."),
             ("Daniel", "Agreed. I will place the order this afternoon for forty dollars.")]
    pcm = b""
    for i, (voice, text) in enumerate(lines):
        out = tmp_path / f"{i}.wav"
        subprocess.run(["say", "-v", voice, "--file-format=WAVE", f"--data-format=LEI16@{SAMPLE_RATE}",
                        "-o", str(out), text], check=True)
        with wave.open(str(out)) as w:
            pcm += w.readframes(w.getnframes()) + b"\x00\x00" * (SAMPLE_RATE // 2)
    client = OpenAINotesClient()
    [(_, _, audio, filename, mime)] = list(encode_pieces(pcm, 0.0))
    segs = client.diarize(audio, filename, mime)
    print("live diarize:", json.dumps(segs))
    assert segs and all(set(s) == {"speaker", "start", "end", "text"} for s in segs)
    assert len({s["speaker"] for s in segs}) == 2
    d = diarize_recording(lambda s, e: pcm[int(s * SAMPLE_RATE) * 2:int(e * SAMPLE_RATE) * 2],
                          len(pcm) / (SAMPLE_RATE * 2), client, piece_secs=10.0)
    print("live pieces:", d.requests, d.status, d.speakers, render_lines(d.lines))
    assert d.status == "yes" and d.speakers == ["Speaker 1", "Speaker 2"], "one voice, one label, across pieces"
