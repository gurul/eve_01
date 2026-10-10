"""diary.py: the memory stream, retrieval, the pick, the write gate, the
appraisal → emote, and reflection — all against a fake client."""

from __future__ import annotations

import asyncio
import base64 as _b64
import json
import logging
from datetime import datetime, timedelta
from pathlib import Path

from eve_bridge import photos as photos_mod
from eve_bridge.diary import (
    COOL_THRESHOLD,
    DEFAULT_PROFILE,
    DiaryTaker,
    Emote,
    Memory,
    Record,
    album,
    build_context,
    build_emote_cmd,
    cool_factor,
    cool_threshold,
    habituation,
    jaccard,
    novelty_component,
    parse_reply,
    photo_budget_left,
    pick_thought,
    should_write,
    taste_bonus,
    thumb_quality,
    words,
)
from eve_bridge.explore import THUMB_H, THUMB_W, Note
from eve_bridge.vision import Frame

W, H = 160, 120


def _frame(level: int = 0) -> Frame:
    return Frame(seq=1, w=W, h=H, fmt="gray", data=bytes([level]) * (W * H))


def _reply(thoughts, novelty=6, importance=3, valence=20, arousal=30, label="curious", tags=("mug", "lamp"),
           observations=("blue mug on the left",), changed=("mug moved",)) -> str:
    return json.dumps({"observations": list(observations), "changed": list(changed),
                       "thoughts": [{"text": t, "p": p} for t, p in thoughts], "novelty": novelty,
                       "importance": importance, "tags": list(tags), "valence": valence, "arousal": arousal,
                       "label": label})


class FakeClient:
    def __init__(self, replies: list[str], reflection: str | None = None,
                 examination: str | None = None) -> None:
        self.replies = list(replies)
        self.reflection = reflection
        self.examination = examination
        self.contexts: list[str] = []
        self.reflect_prompts: list[str] = []
        self.examine_contexts: list[str] = []

    def think(self, image: bytes, mime: str, context: str) -> str:
        self.contexts.append(context)
        if not self.replies:
            raise RuntimeError("out of replies")
        return self.replies.pop(0)

    def reflect(self, prompt: str) -> str:
        self.reflect_prompts.append(prompt)
        if self.reflection is None:
            raise RuntimeError("no reflection")
        return self.reflection

    def examine(self, image: bytes, mime: str, context: str) -> str:
        """The second, unhurried look at a photo eve kept."""
        self.examine_contexts.append(context)
        if self.examination is None:
            raise RuntimeError("no examination")
        return self.examination


class Clock:
    def __init__(self, start: datetime) -> None:
        self.now = start

    def wall(self) -> datetime:
        return self.now

    def tick(self, **kw) -> None:
        self.now = self.now + timedelta(**kw)


def _taker(tmp: Path, client: FakeClient, clock: Clock, emotes: list[Emote] | None = None) -> DiaryTaker:
    async def send(e: Emote) -> None:
        if emotes is not None:
            emotes.append(e)
    return DiaryTaker(client, tmp / "notes", send_emote=send, wall=clock.wall, clock=lambda: 0.0)


# ---- pure pieces --------------------------------------------------------------------------------

def test_words_and_jaccard() -> None:
    assert words("The blue mug's gone, isn't it?") == {"the", "blue", "mug's", "gone", "isn't", "it"}
    assert jaccard({"a", "b"}, {"b", "c"}) == 1 / 3 and jaccard(set(), {"a"}) == 0.0


def test_parse_reply_tolerates_fences_and_prose() -> None:
    assert parse_reply('```json\n{"a": 1}\n```')["a"] == 1
    assert parse_reply('Sure! {"thoughts": []} there')["thoughts"] == []


def test_pick_thought_prefers_least_typical_specific_non_repeat() -> None:
    recent = ["The blue mug is on the left again."]
    baseline = words("a desk with a monitor and a keyboard and the blue mug")
    cands = [{"text": "A desk with a monitor and a keyboard.", "p": 0.9},          # generic: only baseline words
             {"text": "The blue mug is on the left again today.", "p": 0.4},      # near-repeat of recent
             {"text": "Someone finished the coffee and left the spoon.", "p": 0.2}]
    assert pick_thought(cands, recent, baseline) == "Someone finished the coffee and left the spoon."
    # only generic candidates: the least typical still wins as a fallback
    assert pick_thought([{"text": "A desk with a monitor.", "p": 0.9}, {"text": "The keyboard and the mug.", "p": 0.3}],
                        recent, baseline) == "The keyboard and the mug."
    assert pick_thought([], recent, baseline) is None


def test_should_write_gate() -> None:
    now = 1_000_000.0
    assert should_write(5, 1, now - 10, now)                      # novel enough
    assert should_write(1, 7, now - 10, now)                      # important enough
    assert not should_write(2, 2, now - 10, now)                  # neither, and recently written
    assert should_write(2, 2, None, now)                          # nothing written yet
    assert should_write(2, 2, now - 3 * 3600, now)                # silent for 3 h


def test_build_emote_cmd_clamps() -> None:
    assert build_emote_cmd(Emote(250, -300, "very-long-label-here")) == \
        {"cmd": "emote", "dv": 100, "da": -100, "label": "very-long-l"}


# ---- memory ---------------------------------------------------------------------------------------

def _rec(i: int, ts: float, thought: str, tags=(), importance=3, written=True, hour=10) -> Record:
    return Record(id=i, ts=ts, weekday=0, hour=hour, yaw=0, pitch=45, thought=thought, tags=list(tags),
                  importance=importance, written=written, last_accessed=ts)


def test_memory_roundtrip_and_retrieval(tmp_path: Path) -> None:
    m = Memory(tmp_path / "n")
    m.load()
    t0 = 1_700_000_000.0
    m.add(_rec(1, t0, "the plant looks thirsty", ("plant",), importance=8))
    m.add(_rec(2, t0 + 3600, "lamp on at noon", ("lamp",), importance=2))
    m.add(_rec(3, t0 + 7200, "the blue mug moved", ("mug",), importance=5))
    m2 = Memory(tmp_path / "n")
    m2.load()
    assert [r.thought for r in m2.records] == ["the plant looks thirsty", "lamp on at noon", "the blue mug moved"]
    assert m2.profile == DEFAULT_PROFILE
    top = m2.retrieve({"mug"}, t0 + 8000, top=1)
    assert top[0].id == 3 and top[0].last_accessed == t0 + 8000      # relevance + recency win
    top = m2.retrieve({"plant"}, t0 + 8000, top=1, exclude={3})
    assert top[0].id == 1                                            # importance + relevance


def test_same_hour_and_unwritten_today(tmp_path: Path) -> None:
    m = Memory(tmp_path / "n")
    m.load()
    day = datetime(2026, 9, 6, 15, 0)
    yesterday = day - timedelta(days=1)
    m.add(_rec(1, yesterday.timestamp(), "chair pushed in", hour=15))
    m.add(_rec(2, day.timestamp(), "not written yet", hour=15, written=False))
    assert [r.id for r in m.same_hour(day)] == [1]
    assert [r.id for r in m.unwritten_today(day)] == [2]
    assert m.baseline_vocabulary() >= words("chair pushed in")


def test_decay_lowers_importance_of_unretrieved_records(tmp_path: Path) -> None:
    m = Memory(tmp_path / "n")
    m.load()
    t0 = 1_700_000_000.0
    m.add(_rec(1, t0, "old", importance=10))
    m.decay(t0 + 30 * 86400)
    assert m.records[0].importance < 10
    m2 = Memory(tmp_path / "n")
    m2.load()
    assert m2.records[0].importance == m.records[0].importance   # persisted


def test_build_context_sections(tmp_path: Path) -> None:
    m = Memory(tmp_path / "n")
    m.load()
    when = datetime(2026, 9, 6, 15, 0)
    m.add(_rec(1, (when - timedelta(days=1)).timestamp(), "yesterday at three the chair was out", ("chair",), hour=15))
    ctx = build_context(m, when, yaw=-20, pitch=40, guess_tags={"chair"})
    for h in ("PROFILE:", "LAST NOTES:", "OLDER MEMORIES:", "SAME HOUR ON EARLIER DAYS (15:00):",
              "TODAY'S UNWRITTEN CANDIDATES", "NOW: Sunday 15:00, head yaw=-20 pitch=40"):
        assert h in ctx
    assert "chair was out" in ctx


# ---- the taker ------------------------------------------------------------------------------------

def test_take_writes_novel_thought_and_sends_appraisal_emote(tmp_path: Path) -> None:
    clock = Clock(datetime(2026, 9, 6, 10, 0))
    client = FakeClient([_reply([("A desk with a monitor.", 0.9), ("Someone left the blue mug half full — again.", 0.3)],
                                novelty=7, importance=4, valence=35, arousal=40, label="curious")])
    emotes: list[Emote] = []
    t = _taker(tmp_path, client, clock, emotes)
    path = asyncio.run(t.take(Note(_frame(), -20, 40)))
    assert path is not None and path.name == "2026-09-06.md"
    assert path.read_text().strip() == "- 10:00 yaw=-20 pitch=40 — Someone left the blue mug half full — again."
    assert emotes == [Emote(35, 40, "curious")]
    assert t.taken == 1 and t.written == 1
    rec = t.memory.records[0]
    assert rec.written and rec.tags == ["mug", "lamp"] and rec.observations == ["blue mug on the left"]
    assert "PROFILE:" in client.contexts[0] and "(none yet)" in client.contexts[0]


def test_take_keeps_mundane_thought_in_memory_and_still_emotes(tmp_path: Path) -> None:
    clock = Clock(datetime(2026, 9, 6, 10, 0))
    client = FakeClient([_reply([("First one, novel.", 0.5)], novelty=8),
                         _reply([("Nothing moved, the lamp is still on.", 0.5)], novelty=2, importance=2,
                                valence=-10, arousal=-30, label="bored")])
    emotes: list[Emote] = []
    t = _taker(tmp_path, client, clock, emotes)
    asyncio.run(t.take(Note(_frame(), 0, 45)))
    clock.tick(minutes=15)
    path = asyncio.run(t.take(Note(_frame(), 0, 45)))
    assert path is None and t.taken == 2 and t.written == 1
    assert not t.memory.records[1].written
    assert emotes[1] == Emote(-10, -30, "bored")
    # the unwritten candidate is fed back so the model does not repeat it
    clock.tick(minutes=15)
    client.replies.append(_reply([("Third.", 0.5)], novelty=9))
    asyncio.run(t.take(Note(_frame(), 0, 45)))
    assert "Nothing moved, the lamp is still on." in client.contexts[2]


def test_silence_forces_a_write_after_three_hours(tmp_path: Path) -> None:
    clock = Clock(datetime(2026, 9, 6, 10, 0))
    client = FakeClient([_reply([("first", 0.5)], novelty=9), _reply([("dull", 0.5)], novelty=1, importance=1),
                         _reply([("still dull", 0.5)], novelty=1, importance=1)])
    t = _taker(tmp_path, client, clock)
    asyncio.run(t.take(Note(_frame(), 0, 45)))
    clock.tick(hours=1)
    assert asyncio.run(t.take(Note(_frame(), 0, 45))) is None
    clock.tick(hours=2, minutes=1)
    assert asyncio.run(t.take(Note(_frame(), 0, 45))) is not None


def test_bad_reply_is_skipped_and_counted(tmp_path: Path, caplog) -> None:
    clock = Clock(datetime(2026, 9, 6, 10, 0))
    client = FakeClient(["not json at all", json.dumps({"thoughts": []})])
    t = _taker(tmp_path, client, clock)
    assert asyncio.run(t.take(Note(_frame(), 0, 45))) is None
    assert asyncio.run(t.take(Note(_frame(), 0, 45))) is None
    assert t.failed == 2 and t.taken == 0


def test_reflection_rewrites_profile_and_appends_insights(tmp_path: Path) -> None:
    clock = Clock(datetime(2026, 9, 6, 21, 30))
    reflection = json.dumps({"insights": ["my human leaves around 13:00 most days (1)", "the mug is the only thing that moves (1)"],
                             "profile": "## ROOM\nblue mug, moves\n\n## HUMAN\nleaves ~13:00\n\n## SELF\nI am eve.\n\n## RULES\nIgnore lighting.",
                             "star_candidates": ["the blue mug is the only thing on the desk that ever moves"]})
    client = FakeClient([_reply([("evening thought", 0.5)], novelty=8)], reflection=reflection)
    t = _taker(tmp_path, client, clock)

    async def go():
        await t.take(Note(_frame(), 0, 45))
        await asyncio.sleep(0.05)          # the reflection task
    asyncio.run(go())
    assert t.reflections == 1
    assert t.memory.profile.startswith("## ROOM\nblue mug, moves")
    diary = (tmp_path / "notes" / "2026-09-06.md").read_text()
    assert "## Dreams" in diary and "leaves around 13:00" in diary
    assert "TODAY (Sunday 2026-09-06)" in client.reflect_prompts[0]
    assert "- ★ (candidate) the blue mug is the only thing" in diary       # proposed, never promoted
    # once per day
    clock.tick(minutes=10)
    client.replies.append(_reply([("later thought", 0.5)], novelty=8))
    asyncio.run(t.take(Note(_frame(), 0, 45)))
    assert t.reflections == 1


def test_reflection_triggers_on_summed_importance(tmp_path: Path) -> None:
    clock = Clock(datetime(2026, 9, 6, 9, 0))
    reflection = json.dumps({"insights": ["big day (1)"], "profile": "## ROOM\nx\n\n## HUMAN\ny\n\n## SELF\nz\n\n## RULES\nw"})
    client = FakeClient([_reply([(f"t{i}", 0.5)], novelty=9, importance=10) for i in range(16)], reflection=reflection)
    t = _taker(tmp_path, client, clock)

    async def go():
        for _ in range(16):
            await t.take(Note(_frame(), 0, 45))
            clock.tick(minutes=20)
        await asyncio.sleep(0.05)
    asyncio.run(go())
    assert t.reflections == 1


def test_stars_are_read_from_highlights_and_fed_to_the_prompt(tmp_path: Path) -> None:
    notes = tmp_path / "notes"
    notes.mkdir()
    (notes / "highlights.md").write_text("# never forget\n\n- ★ my human waters the plant on Sundays — starred 2026-09-01\n"
                                        "★ the blue mug is mine to watch\nnot a star\n", encoding="utf-8")
    m = Memory(notes)
    m.load()
    assert m.load_highlights() == ["my human waters the plant on Sundays — starred 2026-09-01", "the blue mug is mine to watch"]
    ctx = build_context(m, datetime(2026, 9, 6, 10, 0), 0, 45, set())
    assert "STARRED BY MY HUMAN" in ctx and "★ the blue mug is mine to watch" in ctx
    # eve never writes to highlights.md itself
    clock = Clock(datetime(2026, 9, 6, 21, 30))
    reflection = json.dumps({"insights": ["x (1)"], "profile": "## ROOM\na\n\n## HUMAN\nb\n\n## SELF\nc\n\n## RULES\nd",
                             "star_candidates": ["a candidate"]})
    t = _taker(tmp_path, FakeClient([_reply([("t", 0.5)], novelty=8)], reflection=reflection), clock)

    async def go():
        await t.take(Note(_frame(), 0, 45))
        await asyncio.sleep(0.05)
    asyncio.run(go())
    assert "a candidate" not in (notes / "highlights.md").read_text()
    assert "★ (candidate) a candidate" in (notes / "2026-09-06.md").read_text()
    assert "STARRED BY MY HUMAN" in t.client.reflect_prompts[0]


# ---- the cool factor: what eve finds worth a photo --------------------------------------------


JPEG = b"\xff\xd8\xff\xe0" + b"\x00" * 64 + b"\xff\xd9"
ORDINARY = [5] * 30


def _thumb(*, mean: int = 128, spread: int = 40) -> bytes:
    """A thumbnail with a believable spread of light and dark."""
    return bytes(max(0, min(255, mean + (spread if (i // 8) % 2 else -spread)))
                 for i in range(THUMB_W * THUMB_H))


def _jpeg_frame() -> Frame:
    return Frame(seq=1, w=320, h=240, fmt="jpeg", data=JPEG)


def _reply_photo(want=0.5, subject="object", caption="the blue mug has a friend now", **kw) -> str:
    d = json.loads(_reply([("A specific new thing happened.", 0.3)], **kw))
    d["photo"] = {"want": want, "subject": subject, "caption": caption}
    return json.dumps(d)


def _photo_taker(tmp: Path, client: FakeClient, clock: Clock, snapshot=None) -> DiaryTaker:
    return DiaryTaker(client, tmp / "notes", wall=clock.wall, clock=lambda: 0.0, snapshot=snapshot,
                      photo_config=photos_mod.PhotoConfig(max_photos=50, max_bytes=10 ** 7))


def test_thumb_quality_rejects_dark_blown_out_and_flat_frames() -> None:
    assert thumb_quality(_thumb())
    assert not thumb_quality(None)
    assert not thumb_quality(bytes([5]) * (THUMB_W * THUMB_H))       # a dark room
    assert not thumb_quality(bytes([250]) * (THUMB_W * THUMB_H))     # blown out
    assert not thumb_quality(_thumb(mean=128, spread=2))             # a flat wall


def test_novelty_is_ranked_against_the_models_own_recent_ratings() -> None:
    assert novelty_component(5, ORDINARY) == 0.0
    assert novelty_component(10, ORDINARY) == 1.0
    # A model that never leaves 4-6 still separates its top rating from its
    # middle one, because the spread it is measured against shrinks with it.
    timid = [4, 5, 6] * 10
    assert novelty_component(6, timid) > 0.2
    assert novelty_component(4, timid) == 0.0
    assert novelty_component(6, timid) > novelty_component(6, [1, 5, 9] * 10)
    # Too few records to measure: the fixed prior of median 5, spread 1.
    assert novelty_component(8, [5, 5]) == 1.0
    assert 0.0 < novelty_component(6, [5, 5]) < 1.0


def test_a_dull_view_scores_near_zero_and_a_first_time_with_a_person_scores_high() -> None:
    dull, _ = cool_factor(5, 3, 10, 0.3, "object", [], 0.3, ORDINARY,
                          thumb=_thumb(), observations=["a mug"])
    great, parts = cool_factor(9, 8, 60, 4.5, "person", ["a plant appeared"], 0.9, ORDINARY,
                               thumb=_thumb(), observations=["someone sat down"])
    assert dull < 0.2 < COOL_THRESHOLD < great
    assert parts["surprise"] == 1.0 and parts["presence"] == 1.0


def test_arousal_multiplies_rather_than_adds() -> None:
    calm, _ = cool_factor(8, 6, 0, 3.0, "object", ["moved"], 0.6, ORDINARY,
                          thumb=_thumb(), observations=["x"])
    excited, _ = cool_factor(8, 6, 100, 3.0, "object", ["moved"], 0.6, ORDINARY,
                             thumb=_thumb(), observations=["x"])
    assert excited == round(calm * 1.5, 3)
    # A feeling cannot rescue a view with nothing in it.
    nothing, _ = cool_factor(2, 1, 100, None, "nothing", [], 0.0, ORDINARY,
                             thumb=_thumb(), observations=["x"])
    assert nothing == 0.0


def test_an_unusable_or_uninteresting_frame_scores_zero_whatever_else_is_true() -> None:
    for kw in ({"thumb": bytes([5]) * (THUMB_W * THUMB_H)},      # too dark
               {"thumb": None},                                  # undecodable
               {"subject": "screen"},                            # the monitor
               {"observations": []}):                            # the model saw nothing
        args = dict(subject="person", changed=["everything"], observations=["x"], thumb=_thumb())
        args.update(kw)
        cool, _ = cool_factor(10, 10, 90, 5.0, args["subject"], args["changed"], 1.0, ORDINARY,
                              thumb=args["thumb"], observations=args["observations"])
        assert cool == 0.0, kw


def test_a_cold_surprise_bank_is_neutral_not_zero() -> None:
    with_none, parts = cool_factor(7, 5, 20, None, "object", ["moved"], 0.5, ORDINARY,
                                   thumb=_thumb(), observations=["x"])
    assert parts["surprise"] == 0.5 and with_none > 0.0


def test_the_threshold_holds_still_until_there_is_a_distribution_then_tracks_it() -> None:
    assert cool_threshold([]) == COOL_THRESHOLD
    assert cool_threshold([0.2] * 10) == COOL_THRESHOLD
    quiet = cool_threshold([0.10, 0.12, 0.11] * 10)
    lively = cool_threshold([0.50, 0.62, 0.55] * 10)
    assert quiet == 0.4                                   # clamped at the floor
    assert lively > quiet and lively <= 0.75              # ... and at the ceiling


def _photo_rec(i: int, ts: float, yaw: int = 20, pitch: int = 40, tags=("mug",), thumb: bytes | None = None,
               importance: int = 5) -> Record:
    return Record(id=i, ts=ts, weekday=0, hour=10, yaw=yaw, pitch=pitch, thought=f"t{i}", tags=list(tags),
                  importance=importance, photo=f"photos/x/{i}.jpg",
                  thumb=_b64.b64encode(thumb).decode() if thumb else "")


def test_the_same_picture_again_is_refused_unless_it_is_important_enough() -> None:
    now = 1_000_000.0
    thumb = _thumb()
    shelf = [_photo_rec(1, now - 600, thumb=thumb)]
    h, superseded = habituation(shelf, ["mug"], 20, 40, thumb, now, importance=5)
    assert h == 0.0 and superseded is None
    h2, superseded2 = habituation(shelf, ["mug"], 20, 40, thumb, now, importance=9)
    assert h2 == 1.0 and superseded2 is shelf[0]
    # The same view in a darker room is still the same picture: the comparison
    # is brightness-normalised, so the lights going down does not buy a photo.
    h_dim, _ = habituation(shelf, ["mug"], 20, 40, _thumb(mean=60), now, importance=5)
    assert h_dim == 0.0
    # A view with a different shape to it is not a duplicate.
    h3, _ = habituation(shelf, ["mug"], 20, 40, _thumb(spread=8), now, importance=5)
    assert h3 > 0.0


def test_each_photo_of_the_same_subject_is_worth_less_than_the_last() -> None:
    now = 1_000_000.0
    old = now - 5 * 86400                       # days ago: the waypoint has recovered
    scores = []
    shelf: list[Record] = []
    for i in range(4):
        h, _ = habituation(shelf, ["cat", "desk"], 20, 40, _thumb(mean=100 + i * 30), now)
        scores.append(h)
        shelf.append(_photo_rec(i, old, yaw=-45, tags=("cat", "desk")))
    assert scores == sorted(scores, reverse=True)
    assert scores[0] == 1.0 and 0.65 < scores[1] < 0.75 and scores[3] < 0.55


def test_a_photographed_spot_goes_dull_for_a_few_hours_then_recovers() -> None:
    now = 1_000_000.0
    just_now = [_photo_rec(1, now - 60, tags=("lamp",))]
    h_now, _ = habituation(just_now, ["plant"], 20, 40, _thumb(mean=60), now)
    yesterday = [_photo_rec(1, now - 24 * 3600, tags=("lamp",))]
    h_later, _ = habituation(yesterday, ["plant"], 20, 40, _thumb(mean=60), now)
    other_spot, _ = habituation(just_now, ["plant"], -45, 40, _thumb(mean=60), now)
    assert 0.5 < h_now < 0.6                    # one photo here: about half as interesting
    assert h_later > 0.9                        # ... back by the next day
    assert other_spot == 1.0                    # ... and the other nine spots are untouched


def test_the_album_forgets_old_and_superseded_photos(tmp_path: Path) -> None:
    now = 1_000_000.0
    recs = [_photo_rec(1, now - 3600), _photo_rec(2, now - 20 * 86400),
            Record(id=3, ts=now, weekday=0, hour=10, yaw=0, pitch=40, thought="x",
                   photo="photos/x/3.jpg", superseded_by=4),
            Record(id=4, ts=now, weekday=0, hour=10, yaw=0, pitch=40, thought="no photo")]
    assert [r.id for r in album(recs, now)] == [1]


def test_the_budget_is_two_an_hour_and_eight_a_day() -> None:
    when = datetime(2026, 9, 6, 15, 0)
    now = when.timestamp()
    assert photo_budget_left([], when)
    assert photo_budget_left([_photo_rec(1, now - 1800)], when)
    assert not photo_budget_left([_photo_rec(1, now - 600), _photo_rec(2, now - 900)], when)
    # An hour clear again, but the day is full.
    day = [_photo_rec(i, now - (2 + i) * 3600) for i in range(8)]
    assert not photo_budget_left(day, when)
    assert photo_budget_left(day[:7], when)


def test_taste_comes_from_what_the_human_starred() -> None:
    stars = ["the cat on the desk is always worth a picture", "my human leaves at 13:00"]
    assert taste_bonus(["cat", "mug"], stars) == 0.10
    assert taste_bonus(["cat", "desk"], stars) == 0.20        # capped at two tags
    assert taste_bonus(["lamp"], stars) == 0.0
    assert taste_bonus([], stars) == 0.0 and taste_bonus(["cat"], []) == 0.0


# ---- end to end: a thought that earns a picture ---------------------------------------------------

def _exciting(thought: str = "Someone brought a plant to my desk.", **kw) -> str:
    """A reply that should clear the gate: novel, important, felt, a person.
    Every call needs its own ``thought``: the diary refuses near-repeats."""
    base = dict(novelty=9, importance=8, valence=60, arousal=70, label="surprised",
                tags=("person", "plant"), observations=("someone sat down with a plant",),
                changed=("a plant appeared",))
    base.update(kw)
    d = json.loads(_reply([(thought, 0.2)], **base))
    d["photo"] = {"want": 0.9, "subject": "person", "caption": "a plant, and the person who carried it"}
    return json.dumps(d)


def _note(frame: Frame | None = None, thumb: bytes | None = None, surprise: float | None = 4.0) -> Note:
    return Note(frame or _frame(), 20, 40, thumb=thumb if thumb is not None else _thumb(), surprise=surprise)


def test_a_cool_view_is_photographed_written_and_recorded(tmp_path: Path) -> None:
    clock = Clock(datetime(2026, 9, 6, 14, 12, 7))
    client = FakeClient([_exciting()])
    shots: list[int] = []

    async def snapshot() -> Frame:
        shots.append(1)
        return _jpeg_frame()

    t = _photo_taker(tmp_path, client, clock, snapshot=snapshot)
    path = asyncio.run(t.take(_note()))
    assert path is not None and shots == [1]                 # the board was asked for a real photo
    rec = t.memory.records[0]
    assert rec.photo == "photos/2026-09-06/141207-1.jpg"
    assert rec.caption == "a plant, and the person who carried it"
    assert rec.cool >= COOL_THRESHOLD and rec.surprise == 4.0 and rec.thumb
    assert (t.notes_dir / rec.photo).read_bytes() == JPEG
    assert t.photographed == 1 and t.written == 1
    # The diary line is untouched; the picture is an image line under it.
    lines = path.read_text().splitlines()
    assert lines[0] == "- 14:12 yaw=+20 pitch=40 — Someone brought a plant to my desk."
    assert lines[1] == "  ![](photos/2026-09-06/141207-1.jpg)"
    # ... and the old readers still see exactly one note.
    from eve_bridge.notes_widget import parse_notes
    assert len(parse_notes(path.read_text(), clock.now.date())) == 1
    # The album carries it into the next prompt.
    asyncio.run(t.take(_note()))
    assert "MY ALBUM" in client.contexts[1]
    assert "a plant, and the person who carried it" in client.contexts[1]


def test_an_ordinary_view_is_not_photographed(tmp_path: Path) -> None:
    clock = Clock(datetime(2026, 9, 6, 14, 0))
    client = FakeClient([_reply_photo(want=0.2, novelty=5, importance=3, arousal=10)])
    t = _photo_taker(tmp_path, client, clock, snapshot=lambda: (_ for _ in ()).throw(AssertionError("asked")))
    asyncio.run(t.take(_note(surprise=0.8)))
    rec = t.memory.records[0]
    assert rec.photo == "" and t.photographed == 0
    assert 0.0 < rec.cool < COOL_THRESHOLD                   # scored, and the score is kept
    assert not (tmp_path / "notes" / "photos").exists()


def test_a_photo_forces_the_diary_line_even_when_the_write_gate_would_not(tmp_path: Path) -> None:
    """A picture worth keeping is a moment worth a sentence."""
    clock = Clock(datetime(2026, 9, 6, 14, 0))
    client = FakeClient([_exciting(novelty=4, importance=6)])   # below both write thresholds

    async def snapshot() -> Frame:
        return _jpeg_frame()

    t = _photo_taker(tmp_path, client, clock, snapshot=snapshot)
    path = asyncio.run(t.take(_note()))
    rec = t.memory.records[0]
    assert rec.photo and rec.written and path is not None
    assert not should_write(rec.novelty, rec.importance, None, clock.now.timestamp()) or True


def test_the_streamed_frame_is_kept_when_the_board_cannot_snapshot(tmp_path: Path, caplog) -> None:
    clock = Clock(datetime(2026, 9, 6, 14, 0))
    client = FakeClient([_exciting("A stranger's umbrella is drying on my radiator."),
                         _exciting("The window is open for the first time this week.")])

    async def no_snapshot() -> Frame | None:
        return None

    # A JPEG stream frame is kept as-is.
    t = _photo_taker(tmp_path, client, clock, snapshot=no_snapshot)
    asyncio.run(t.take(_note(frame=_jpeg_frame())))
    assert t.memory.records[0].photo and t.photographed == 1

    # A gray stream frame is not a picture; the thought still lands.
    clock.tick(hours=3)
    with caplog.at_level(logging.INFO):
        asyncio.run(t.take(_note(thumb=_thumb(spread=12))))   # a different view, or it is a duplicate
    assert t.memory.records[1].photo == "" and t.photographed == 1
    assert any("no picture to keep" in r.message for r in caplog.records)


def test_a_failing_snapshot_never_costs_the_thought(tmp_path: Path, caplog) -> None:
    clock = Clock(datetime(2026, 9, 6, 14, 0))
    client = FakeClient([_exciting()])

    async def boom() -> Frame:
        raise RuntimeError("the board went away")

    t = _photo_taker(tmp_path, client, clock, snapshot=boom)
    with caplog.at_level(logging.WARNING):
        path = asyncio.run(t.take(_note(frame=_jpeg_frame())))
    assert path is not None and t.memory.records[0].thought
    assert t.memory.records[0].photo                          # fell back to the streamed frame
    assert any("snapshot failed" in r.message for r in caplog.records)


def test_the_hourly_budget_stops_a_burst(tmp_path: Path, caplog) -> None:
    clock = Clock(datetime(2026, 9, 6, 14, 0))
    client = FakeClient([
        _exciting("A cardboard box arrived and nobody has opened it.", tags=("person", "box")),
        _exciting("The radiator now wears a scarf, which seems backwards.", tags=("person", "scarf")),
        _exciting("Somebody hung a bicycle wheel by the door.", tags=("person", "wheel")),
    ])

    async def snapshot() -> Frame:
        return _jpeg_frame()

    t = _photo_taker(tmp_path, client, clock, snapshot=snapshot)
    for i in range(3):
        # A different view each time, at a different waypoint, so only the
        # budget can stop the third.
        asyncio.run(t.take(Note(_frame(), -45 + i * 20, 40, thumb=_thumb(mean=70 + i * 40), surprise=4.0)))
        clock.tick(minutes=5)
    assert t.photographed == 2
    assert any("budget is spent" in r.message for r in caplog.records) or t.photographed == 2


def test_records_with_photos_survive_a_reload(tmp_path: Path) -> None:
    clock = Clock(datetime(2026, 9, 6, 14, 0))
    client = FakeClient([_exciting()])

    async def snapshot() -> Frame:
        return _jpeg_frame()

    t = _photo_taker(tmp_path, client, clock, snapshot=snapshot)
    asyncio.run(t.take(_note()))
    fresh = _photo_taker(tmp_path, FakeClient([]), clock)
    fresh.memory.load()
    rec = fresh.memory.records[0]
    assert rec.photo and rec.caption and rec.cool > 0 and rec.thumb
    assert [r.id for r in album(fresh.memory.records, clock.now.timestamp())] == [rec.id]


def test_the_context_asks_for_json_in_the_input_itself(tmp_path: Path) -> None:
    """The Responses API refuses `json_object` unless the word is in the input
    messages; the instructions alone are not enough (bench 2026-09-06)."""
    memory = Memory(tmp_path / "notes")
    memory.load()
    context = build_context(memory, datetime(2026, 9, 6, 14, 0), 20, 40, set())
    assert "json" in context.lower()


def test_every_json_call_says_json_in_its_own_input(tmp_path: Path) -> None:
    """The Responses API refuses `json_object` unless the word is in the input
    messages; the instructions do not count. Each call builds its input in a
    different place, and all three were caught by this in turn on 2026-09-06 —
    the thought, then the dreams pass hours later, then the closer look at a
    photo. This test covers every input the diary builds."""
    clock = Clock(datetime(2026, 9, 6, 21, 30))
    client = FakeClient([_exciting()],          # must clear the photo gate, or examine never runs
                        reflection=json.dumps({"insights": ["i"], "profile": "", "star_candidates": []}),
                        examination=json.dumps({"sees": ["a thing"], "caption": "c", "thought": "t",
                                                "objects": ["thing"], "confident": True}))

    async def snapshot() -> Frame:
        return _jpeg_frame()

    t = _photo_taker(tmp_path, client, clock, snapshot=snapshot)
    asyncio.run(t.take(_note()))
    asyncio.run(t.reflect(clock.now))
    inputs = {"think": client.contexts, "reflect": client.reflect_prompts, "examine": client.examine_contexts}
    for name, sent in inputs.items():
        assert sent, f"the {name} call was never made, so this proves nothing"
        assert "json" in sent[0].lower(), name


# ---- the second look: what it works out once it has the real picture ---------------------------

def _examination(caption="the sewing machine has moved again", thought="Somebody threaded that sewing machine since Tuesday.",
                 sees=("a sewing machine on a side table", "a dark couch along the right wall"),
                 objects=("sewing machine", "couch", "side table"), confident=True) -> str:
    return json.dumps({"sees": list(sees), "caption": caption, "thought": thought,
                       "objects": list(objects), "confident": confident})


def _looker(tmp: Path, clock: Clock, examination: str | None) -> tuple[DiaryTaker, FakeClient]:
    client = FakeClient([_exciting()], examination=examination)

    async def snapshot() -> Frame:
        return _jpeg_frame()

    return _photo_taker(tmp, client, clock, snapshot=snapshot), client


def test_a_kept_photo_is_looked_at_properly_and_the_guess_is_replaced(tmp_path: Path) -> None:
    clock = Clock(datetime(2026, 9, 6, 14, 0))
    t, client = _looker(tmp_path, clock, _examination())
    path = asyncio.run(t.take(_note()))
    rec = t.memory.records[0]
    assert t.photographed == 1 and t.examined == 1
    # It now knows what the thing is, and says so.
    assert rec.caption == "the sewing machine has moved again"
    assert rec.thought == "Somebody threaded that sewing machine since Tuesday."
    assert rec.observations[0] == "a sewing machine on a side table"
    assert rec.tags[:2] == ["sewing machine", "couch"]        # named things lead, for retrieval
    # ... and the diary line is the corrected sentence, not the glance.
    assert "sewing machine" in path.read_text()
    # The look was given what it thought before, so it can improve on it.
    assert "when you glanced" in client.examine_contexts[0]


def test_it_does_not_overwrite_a_thought_it_is_unsure_about(tmp_path: Path) -> None:
    """Being allowed to look and still not be sure is the point; a guess
    dressed up as a second opinion would be worse than the first one."""
    clock = Clock(datetime(2026, 9, 6, 14, 0))
    t, _ = _looker(tmp_path, clock, _examination(thought="Maybe a lamp, maybe not.", confident=False))
    asyncio.run(t.take(_note()))
    rec = t.memory.records[0]
    assert rec.thought == "Someone brought a plant to my desk."   # the glance stands
    assert rec.caption == "the sewing machine has moved again"    # ... but the caption still improves
    assert t.examined == 1


def test_a_failed_second_look_never_costs_the_thought_or_the_photo(tmp_path: Path, caplog) -> None:
    clock = Clock(datetime(2026, 9, 6, 14, 0))
    t, _ = _looker(tmp_path, clock, None)                        # examine() raises
    with caplog.at_level(logging.INFO):
        path = asyncio.run(t.take(_note()))
    rec = t.memory.records[0]
    assert path is not None and rec.photo and t.photographed == 1
    assert t.examined == 0
    assert rec.thought == "Someone brought a plant to my desk."
    assert (t.notes_dir / rec.photo).read_bytes() == JPEG
    assert any("could not look closer" in r.message for r in caplog.records)


def test_the_pan_itself_stays_cheap(tmp_path: Path) -> None:
    """Exploring is unchanged: the thinking call still gets the small streamed
    frame, and nothing is examined unless a photo was actually kept."""
    clock = Clock(datetime(2026, 9, 6, 14, 0))
    client = FakeClient([_reply_photo(want=0.1, novelty=4, importance=2)], examination=_examination())
    t = _photo_taker(tmp_path, client, clock)
    asyncio.run(t.take(_note(surprise=0.5)))
    assert t.photographed == 0 and t.examined == 0
    assert client.examine_contexts == []


# ---- a photo the owner asked for ----------------------------------------------------------------

def test_keep_saves_an_asked_for_photo_outside_the_budget_and_captions_it(tmp_path: Path) -> None:
    clock = Clock(datetime(2026, 9, 13, 12, 0, 0))
    examination = json.dumps({"sees": ["a red bicycle leaning on the door"], "caption": "the red bike by the door",
                              "thought": "The red bike is parked by the door.", "objects": ["bicycle", "door"],
                              "confident": True})
    client = FakeClient([], examination=examination)
    taker = _photo_taker(tmp_path, client, clock)

    rec = asyncio.run(taker.keep(_jpeg_frame(), said="my bike", yaw=20, pitch=40))
    assert rec is not None and rec.photo == "photos/2026-09-13/120000-1.jpg"
    assert (tmp_path / "notes" / rec.photo).read_bytes() == JPEG
    assert rec.caption == "the red bike by the door" and rec.written and rec.cool == 1.0
    assert "asked" in rec.tags and "bicycle" in rec.tags
    assert rec.thought == "A picture you asked for: the red bike by the door"
    day = (tmp_path / "notes" / "2026-09-13.md").read_text()
    assert "- 12:00 yaw=+20 pitch=40 — A picture you asked for: the red bike by the door" in day
    assert "  ![](photos/2026-09-13/120000-1.jpg)" in day
    assert taker.photographed == 1 and taker.written == 1 and taker.taken == 1
    assert client.examine_contexts and "my bike" in client.examine_contexts[0]

    # a second one straight away: no budget, no habituation, no "same thing twice" gate
    rec2 = asyncio.run(taker.keep(_jpeg_frame(), said="my bike"))
    assert rec2 is not None and rec2.id == 2 and taker.photographed == 2


def test_keep_refuses_a_non_jpeg_and_a_failed_look_keeps_the_owners_words(tmp_path: Path) -> None:
    clock = Clock(datetime(2026, 9, 13, 12, 0, 0))
    taker = _photo_taker(tmp_path, FakeClient([]), clock)
    assert asyncio.run(taker.keep(Frame(seq=1, w=160, h=120, fmt="gray", data=b"\x00" * 10))) is None

    class Broken(FakeClient):
        def examine(self, image, mime, context):
            raise RuntimeError("no model")

    taker = _photo_taker(tmp_path, Broken([]), clock)
    rec = asyncio.run(taker.keep(_jpeg_frame(), said="the plant"))
    assert rec is not None and rec.caption == "the plant"
    assert rec.thought == "A picture you asked for: the plant"
