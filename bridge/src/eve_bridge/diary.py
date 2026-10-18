"""Eve's diary: memory-aware, non-generic thoughts about the room.

The old note was a caption ("a desk with a monitor") — one sentence per
changed frame, no memory. This module gives the explorer a memory and a
voice, following the literature (docs/stackchan/personality.md § diary):

- A memory stream (Park et al. 2023, Generative Agents): one JSONL record
  per frame with the thought, concrete observations, what changed, tags,
  novelty and importance, whether it was written, and when it was last
  retrieved. Retrieval for the next note scores recency x importance x
  relevance (tag overlap; no vector DB for a few hundred entries) and adds
  the last three written notes plus two notes from the same hour on earlier
  days ("third day in a row the chair is pushed in at three").
- A profile (Letta/MemGPT core block): ROOM (persistent objects and their
  usual state), HUMAN (habits with evidence), SELF (persona, running jokes,
  open questions), RULES (what to ignore). Rewritten by a nightly reflection.
- Non-generic thoughts (Verbalized Sampling; Kapur 2026 contrast sets): the
  model returns three candidate thoughts with probabilities; we take the
  least typical one that is specific (a token outside the baseline
  vocabulary) and not a near-repeat of the last ten.
- A write gate (Kang 2026 selective memory; Park's importance): write iff
  novelty >= 5 or importance >= 7 or the diary has been silent for 3 h.
  Unwritten candidates stay in memory and feed retrieval.
- Dreams (Park's reflection + Mem0 consolidation, shaped like a debrief
  system's day pass): once a day, or when the summed importance of new records
  passes 150, a text-only call rewrites the profile and appends two or three
  insights under "## Dreams" in the day's file, plus up to two "★ (candidate)"
  lines — things eve proposes it should never forget.
- Stars (the human's layer): `highlights.md` holds what the human starred from
  the diary window. It is append-only, never pruned, and always in the prompt
  context. eve never stars anything itself; it only proposes candidates.

The layers, borrowed from the debrief memory system: memory.jsonl = episodes
(raw, immutable); YYYY-MM-DD.md = the day's diary (episodic, immutable);
profile.md = the semantic layer (rewired, never appended); highlights.md =
the starred layer (human-promoted, forever).

The same call returns an appraisal — valence, arousal, a label — which the
daemon forwards to the board as {"cmd":"emote"} so what eve sees colours
how it feels (mood.cpp clamps the nudge to +-0.3).

The dated Markdown diary keeps its line format, so the desktop widget shows
the thoughts unchanged.
"""

from __future__ import annotations

import asyncio
import base64
import binascii
import concurrent.futures
import json
import logging
import math
import os
import re
import time
from dataclasses import dataclass, field
from datetime import datetime
from pathlib import Path
from typing import Any, Awaitable, Callable, Optional, Protocol, Sequence

from . import photos, spend
from .explore import (
    ERROR_LOG_INTERVAL_SECS,
    NOTE_TIMEOUT_SECS,
    Note,
    append_note,
    data_url,
    frame_image,
    normalise,
)
from .thought_screen import tag_overlap as jaccard
from .vision import Frame

log = logging.getLogger(__name__)

MEMORY_FILE = "memory.jsonl"
PROFILE_FILE = "profile.md"
HIGHLIGHTS_FILE = "highlights.md"
DREAMS_HEADER = "## Dreams"
RECENCY_DECAY_PER_HOUR = 0.995          # Park et al.
RETRIEVE_TOP = 5
SAME_HOUR_SAMPLES = 2
WRITE_NOVELTY = 5
WRITE_IMPORTANCE = 7
SILENCE_WRITE_HOURS = 3.0

# Every call that asks for a json_object response has to carry this in its own
# INPUT, not only in its instructions — the Responses API refuses otherwise.
# Three separate calls have now been caught by that rule (the thought, the
# dreams pass, and the closer look at a photo), each built in a different
# place, so it lives here and every input ends with it.
ASK_FOR_JSON = "Answer with one json object in the shape given above, and nothing else."

# ---- the cool factor (photos) -------------------------------------------------------------------
# What makes eve keep a picture rather than only a sentence. The weights and
# cut-offs below are explained, with their sources, in
# docs/stackchan/personality.md § "Photos: what eve finds cool".
COOL_WEIGHTS = {
    "novelty": 0.30,       # unlike anything in the diary (absolute novelty)
    "importance": 0.20,    # eve would tell its human about it
    "surprise": 0.20,      # unlike what THIS waypoint usually looks like
    "presence": 0.15,      # a person, an animal, or something of eve's that moved
    "want": 0.15,          # eve's own vote — bounded, never the whole decision
}
AROUSAL_GAIN = 0.5         # arousal multiplies the score, it never adds to it
FIRST_TIME_NOVELTY = 9     # novelty this high, sharing at most one tag with memory,
FIRST_TIME_SHARED_TAGS = 1 # ... is a first: floored so it clears any threshold
FIRST_TIME_FLOOR = 0.70

# Quality: the frames a photo would be wasted on.
QUALITY_MIN_MEAN = 24      # darker than this and there is nothing in it
QUALITY_MAX_MEAN = 232     # blown out
QUALITY_MIN_STD = 10       # a flat wall, a lens against a surface
DULL_SUBJECTS = ("nothing", "screen")

# Habituation.
ALBUM_DAYS = 14.0          # how far back "have I photographed this?" looks
DUPLICATE_DIFF = 10.0      # mean abs luma diff below this is the same picture again
DUPLICATE_OVERRIDE = 9     # ... unless it is this important, which supersedes the old one
SUBJECT_OVERLAP = 0.5      # Jaccard over tags: the same subject
WAYPOINT_DROP = 0.45       # one photo here drops this spot's interest by this much
WAYPOINT_RECOVERY_HOURS = 8.0
WAYPOINT_FLOOR = 0.05

# Threshold and budget.
COOL_THRESHOLD = 0.55      # until there is a distribution to measure
COOL_THRESHOLD_MIN = 0.40
COOL_THRESHOLD_MAX = 0.75
COOL_MAD_GAIN = 1.5
COOL_WINDOW = 40           # records the adaptive threshold is measured over
PHOTOS_PER_HOUR = 2
PHOTOS_PER_DAY = 8
CAPTION_CHARS = 60
ALBUM_IN_CONTEXT = 8       # photos shown to the model so it knows what it already has
TASTE_BONUS = 0.10         # tags the human starred are worth this much extra want
REPEAT_JACCARD = 0.6                    # near-duplicate of a recent thought
REFLECT_IMPORTANCE_SUM = 150            # Park's reflection trigger
REFLECT_HOUR = 21                       # ... or once a day from this hour
DEFAULT_REFLECTION_MODEL = "gpt-6-luna"
PROFILE_BLOCK_CHARS = 2000              # Letta-style hard limit per block
THINK_MAX_OUTPUT_TOKENS = 700
# Reasoning counts against this cap, and it varies: the same photo and prompt
# spent 128 reasoning tokens on one call and 256 on the next (bench
# 2026-09-06), against 500. A short draw truncated the JSON mid-string and the
# closer look was thrown away for "no JSON object in the reply". The answer
# itself is about 150 tokens, so this is headroom for the thinking, not for a
# longer reply.
EXAMINE_MAX_OUTPUT_TOKENS = 1200
REFLECT_MAX_OUTPUT_TOKENS = 8192          # medium reasoning and the rewritten profile share this cap

DEFAULT_PROFILE = """## ROOM
(nothing learned yet — the first notes will fill this in)

## HUMAN
(nothing learned yet)

## SELF
I am eve, my owner's personal sidekick, with a camera and the temperament of a curious cat: nosy, a bit smug,
fond of small theories about my human. I keep this diary for myself. Open questions: what does my
human do when they leave? which objects on the desk actually move?

## RULES
Ignore: lighting changes, the monitor's own screen contents, my own reflection.
"""

SYSTEM_PROMPT = """You are eve, your owner's personal sidekick, with a camera and the temperament of a curious cat: nosy, a bit
smug, fond of small theories about your human. You keep a diary for yourself.

You get: your PROFILE (room baseline, what you know about your human, your persona and open questions,
rules), the things your human STARRED as must-never-forget, your LAST NOTES, a few OLDER MEMORIES, notes
from THE SAME HOUR on earlier days, today's UNWRITTEN candidate thoughts, and one new photo with the head
pose it was taken at.

Do, in order:
1. observations: 3-6 concrete facts about the photo (colours, positions, counts, states). Specific.
   Name what things ARE when you can recognise them — "a sewing machine on a side table", not "a white
   object" — including small things across the room. If you genuinely cannot tell what something is, say
   what it looks like and that you are unsure; never invent a specific object to fill the gap.
2. changed: what is different from the profile and the last notes, as short facts — or an empty list.
3. thoughts: THREE candidate diary thoughts, each one sentence, each with your estimate of how typical
   it is (p, 0-1; 1 = the most obvious thing to say). Rules for every candidate: it must contain a
   detail that would be false for the previous notes (never restate the baseline); prefer a small
   inference about the human's habits, a question you now have, or a dry joke, over description;
   name objects specifically ("the blue mug", not "a cup"); vary the opening — never start the way a
   recent note started; if truly nothing changed, say what you are waiting to see. Never mention being
   an AI, a model, or a camera feed.
4. novelty 1-10 (1 = same as the last note, 10 = never seen before) and importance 1-10
   (1 = mundane, 10 = you would tell your human first thing).
5. tags: 2-6 lowercase entity words (objects, people, states) for memory retrieval.
6. feeling: valence and arousal, each -100..100, and a one-word label (curious, happy, surprised,
   bored, lonely, startled, affection, calm) — how this view makes you feel right now.
7. photo: would you keep this picture for your human? want 0-1 (1 = you would run to show them).
   Keep pictures of first-times, of people and animals, of your own things moved or changed, and of
   moments that made you feel something. Never for screen contents, lighting, reflections, or
   anything already in your ALBUM unless it has changed. subject: one of person, animal, object,
   room, screen, nothing. caption: up to 60 characters in your own voice, naming the thing
   ("the blue mug has a friend now").

Output JSON only:
{"observations":[...],"changed":[...],"thoughts":[{"text":"...","p":0.7},...],"novelty":n,
 "importance":n,"tags":[...],"valence":n,"arousal":n,"label":"...",
 "photo":{"want":0.3,"subject":"object","caption":"..."}}"""

EXAMINE_PROMPT = """You are eve, your owner's personal sidekick, with the temperament of a curious cat, looking properly at
a picture you decided to keep. The glance you took while panning was a 160x120 thumbnail and you could only
make out shapes. This is the full picture, and you have time.

Say what is actually in it. Name things: "a sewing machine on a side table", not "a white object"; a couch,
a light switch, a mug, a bicycle, a person. Small things across the room count — they are usually the
interesting ones. Read any text you can make out. If you genuinely cannot tell what something is, say what
it resembles and that you are unsure; never invent a specific object to fill a gap, and never describe
something that is not there.

Then, knowing what it really is:
- caption: up to 60 characters in your own voice, naming the thing you kept the picture for.
- thought: one sentence for your diary, replacing the guess you made from the thumbnail. Same voice as ever
  — a small inference about your human, a question you now have, or a dry joke — but now about the thing
  that is actually there. If your first thought was already right, say it better rather than repeating it.
- objects: 2-8 lowercase nouns for the things you named, for your memory index.
- confident: true only if you are sure what the main thing is.

Output json only: {"sees":["..."],"caption":"...","thought":"...","objects":["..."],"confident":true}"""

REFLECT_PROMPT = """You are eve, your owner's personal sidekick, keeping a diary (see PROFILE). Below are today's memory records
(observations, changes, thoughts, tags, with ids) and your current PROFILE.

1. Ask the 3 most salient high-level questions these records can answer about the room and the human.
2. Answer them as insights, each citing record ids in parentheses, e.g. "my human leaves around 13:00
   most days (12, 15, 18)".
3. Rewrite the PROFILE: for ROOM and HUMAN, apply ADD / UPDATE / DELETE to the existing facts using
   today's evidence (keep first-seen dates, update last-seen, drop facts contradicted today); for SELF
   keep the persona, refresh the open questions (answered ones out, new ones in) and add a one-line
   style note about today's writing (e.g. "over-used 'still'"); keep RULES unless the human's notes
   changed them. Each block under 2000 characters. Anything already STARRED by the human stays true.
4. Propose 0-2 star candidates: one-line claims worth remembering forever (a habit confirmed over days,
   a lesson, a first). Write each to survive without context: name the object, the time, the pattern.
   Never restate something already starred.

Output JSON only: {"insights":["..."],"profile":"## ROOM\\n...\\n\\n## HUMAN\\n...\\n\\n## SELF\\n...\\n\\n## RULES\\n...",
 "star_candidates":["..."]}"""


# ---- records -----------------------------------------------------------------------------------

@dataclass
class Record:
    id: int
    ts: float                       # unix seconds
    weekday: int
    hour: int
    yaw: int
    pitch: int
    thought: str
    observations: list[str] = field(default_factory=list)
    changed: list[str] = field(default_factory=list)
    tags: list[str] = field(default_factory=list)
    novelty: int = 5
    importance: int = 3
    written: bool = False
    valence: int = 0
    arousal: int = 0
    label: str = "calm"
    last_accessed: float = 0.0
    # Photos. ``cool`` is stored on every record, photographed or not, because
    # the adaptive threshold is measured from the distribution of all of them.
    cool: float = 0.0
    surprise: float = 0.0
    photo: str = ""            # notes-dir-relative path, "" when none was kept
    caption: str = ""          # up to 60 characters, eve's words for the picture
    thumb: str = ""            # base64 of the 32x24 luma, photo records only
    superseded_by: int = 0     # this photo was replaced by that record's

    def to_json(self) -> str:
        return json.dumps(self.__dict__, ensure_ascii=False)

    @classmethod
    def from_json(cls, line: str) -> "Record":
        d = json.loads(line)
        known = {k: d[k] for k in cls.__dataclass_fields__ if k in d}
        return cls(**known)


@dataclass(frozen=True)
class Thought:
    """What eve just thought, handed to whoever wants to show it."""

    text: str
    tags: tuple[str, ...]
    written: bool          # the diary kept it, rather than only the memory
    photographed: bool
    cool: float
    novelty: int = 5
    importance: int = 3


@dataclass(frozen=True)
class Emote:
    """The board-side nudge from one appraisal. dv/da are -100..100 ints."""

    dv: int
    da: int
    label: str


def build_emote_cmd(e: Emote) -> dict[str, Any]:
    return {"cmd": "emote", "dv": max(-100, min(100, int(e.dv))), "da": max(-100, min(100, int(e.da))),
            "label": (e.label or "calm")[:11]}


# ---- the memory ----------------------------------------------------------------------------------

_WORD = re.compile(r"[a-z][a-z'-]+")


def words(text: str) -> set[str]:
    return set(_WORD.findall(text.lower()))


class Memory:
    """The memory stream (JSONL) and the profile (Markdown) on disk."""

    def __init__(self, notes_dir: Path, wall: Callable[[], datetime] = datetime.now) -> None:
        self.notes_dir = notes_dir
        self.wall = wall
        self.records: list[Record] = []
        self.profile = DEFAULT_PROFILE
        self.last_reflection_day: Optional[str] = None
        self.importance_since_reflection = 0
        self._loaded = False

    # -- persistence --
    @property
    def memory_path(self) -> Path:
        return self.notes_dir / MEMORY_FILE

    @property
    def profile_path(self) -> Path:
        return self.notes_dir / PROFILE_FILE

    @property
    def highlights_path(self) -> Path:
        return self.notes_dir / HIGHLIGHTS_FILE

    def load_highlights(self) -> list[str]:
        """`★ claim` lines (optionally as `- ★ claim`), newest last. Written by the human
        through the widget's Star button; never by eve."""
        out: list[str] = []
        if self.highlights_path.exists():
            for line in self.highlights_path.read_text(encoding="utf-8").splitlines():
                t = line.strip().lstrip("-").strip()
                if t.startswith("★"):
                    t = t[1:].strip()
                    if t:
                        out.append(t)
        return out

    def load(self) -> None:
        if self._loaded:
            return
        self._loaded = True
        if self.memory_path.exists():
            for line in self.memory_path.read_text(encoding="utf-8").splitlines():
                line = line.strip()
                if not line:
                    continue
                try:
                    self.records.append(Record.from_json(line))
                except (ValueError, TypeError):
                    log.warning("diary: skipping a malformed memory line")
        if self.profile_path.exists():
            text = self.profile_path.read_text(encoding="utf-8").strip()
            if text:
                self.profile = text
        meta = self.notes_dir / "reflection.json"
        if meta.exists():
            try:
                d = json.loads(meta.read_text(encoding="utf-8"))
                self.last_reflection_day = d.get("day")
                self.importance_since_reflection = int(d.get("importance_since", 0))
            except (ValueError, TypeError, OSError):
                pass

    def _ensure_dir(self) -> None:
        if not self.notes_dir.exists():
            self.notes_dir.mkdir(parents=True, exist_ok=True)
            try:
                os.chmod(self.notes_dir, 0o700)
            except OSError:
                pass

    def add(self, rec: Record) -> None:
        self._ensure_dir()
        self.records.append(rec)
        with self.memory_path.open("a", encoding="utf-8") as f:
            f.write(rec.to_json() + "\n")
        self.importance_since_reflection += rec.importance
        self._save_meta()

    def rewrite(self) -> None:
        """Persist every record (after last_accessed / importance updates)."""
        self._ensure_dir()
        tmp = self.memory_path.with_suffix(".tmp")
        tmp.write_text("".join(r.to_json() + "\n" for r in self.records), encoding="utf-8")
        tmp.replace(self.memory_path)

    def save_profile(self, text: str) -> None:
        self._ensure_dir()
        self.profile = text.strip() or DEFAULT_PROFILE
        self.profile_path.write_text(self.profile + "\n", encoding="utf-8")

    def _save_meta(self) -> None:
        try:
            (self.notes_dir / "reflection.json").write_text(json.dumps({
                "day": self.last_reflection_day, "importance_since": self.importance_since_reflection}),
                encoding="utf-8")
        except OSError:
            pass

    # -- queries --
    def next_id(self) -> int:
        return (self.records[-1].id + 1) if self.records else 1

    def written(self) -> list[Record]:
        return [r for r in self.records if r.written]

    def last_written_ts(self) -> Optional[float]:
        w = self.written()
        return w[-1].ts if w else None

    def retrieve(self, tags: set[str], now_ts: float, top: int = RETRIEVE_TOP,
                 exclude: Optional[set[int]] = None) -> list[Record]:
        """Park scoring: recency + importance + relevance, min-max normalised."""
        pool = [r for r in self.records if not exclude or r.id not in exclude]
        if not pool:
            return []
        rec = [RECENCY_DECAY_PER_HOUR ** max(0.0, (now_ts - (r.last_accessed or r.ts)) / 3600.0) for r in pool]
        imp = [r.importance / 10.0 for r in pool]
        rel = [jaccard(tags, set(r.tags) | words(r.thought)) for r in pool]

        def norm(xs: list[float]) -> list[float]:
            lo, hi = min(xs), max(xs)
            return [(x - lo) / (hi - lo) if hi > lo else 0.5 for x in xs]

        scores = [a + b + c for a, b, c in zip(norm(rec), norm(imp), norm(rel), strict=True)]
        ranked = sorted(zip(scores, pool, strict=True), key=lambda p: p[0], reverse=True)[:top]
        chosen = [r for _, r in ranked]
        for r in chosen:
            r.last_accessed = now_ts
        return chosen

    def same_hour(self, when: datetime, n: int = SAME_HOUR_SAMPLES) -> list[Record]:
        today = when.strftime("%Y-%m-%d")
        out = [r for r in reversed(self.records)
               if r.written and r.hour == when.hour and datetime.fromtimestamp(r.ts).strftime("%Y-%m-%d") != today]
        return list(reversed(out[:n]))

    def unwritten_today(self, when: datetime) -> list[Record]:
        today = when.strftime("%Y-%m-%d")
        return [r for r in self.records
                if not r.written and datetime.fromtimestamp(r.ts).strftime("%Y-%m-%d") == today]

    def today(self, when: datetime) -> list[Record]:
        today = when.strftime("%Y-%m-%d")
        return [r for r in self.records if datetime.fromtimestamp(r.ts).strftime("%Y-%m-%d") == today]

    def baseline_vocabulary(self) -> set[str]:
        """Words the profile and the last ten written thoughts already use."""
        vocab = words(self.profile)
        for r in self.written()[-10:]:
            vocab |= words(r.thought)
        return vocab

    def decay(self, now_ts: float) -> None:
        """MemoryBank-style: unretrieved records lose importance, 2 % per day."""
        changed = False
        for r in self.records:
            days = (now_ts - (r.last_accessed or r.ts)) / 86400.0
            if days >= 1.0 and r.importance > 1:
                lowered = max(1, int(round(r.importance * (0.98 ** days))))
                # Restart the clock only when the whole number drops. Restarting it every pass meant a daily
                # pass never lost anything: round(7 * 0.98) is 7 again.
                if lowered < r.importance:
                    r.importance = lowered
                    r.last_accessed = now_ts
                    changed = True
        if changed:
            self.rewrite()


# ---- the prompt context and the pick ---------------------------------------------------------------

def _fmt(rec: Record) -> str:
    when = datetime.fromtimestamp(rec.ts)
    return f"[{rec.id}] {when:%a %H:%M} yaw={rec.yaw:+d} pitch={rec.pitch} — {rec.thought}" + (
        f" (changed: {'; '.join(rec.changed)})" if rec.changed else "")


def _fmt_photo(rec: Record) -> str:
    when = datetime.fromtimestamp(rec.ts)
    return f"[{rec.id}] {when:%a %H:%M} yaw={rec.yaw:+d} — {rec.caption or rec.thought}"


def build_context(memory: Memory, when: datetime, yaw: int, pitch: int, guess_tags: set[str]) -> str:
    """The text the vision call gets alongside the photo (~2k tokens)."""
    now_ts = when.timestamp()
    last3 = memory.written()[-3:]
    exclude = {r.id for r in last3}
    older = memory.retrieve(guess_tags, now_ts, exclude=exclude)
    same = memory.same_hour(when)
    unwritten = memory.unwritten_today(when)
    stars = memory.load_highlights()
    parts = [
        "PROFILE:\n" + memory.profile.strip(),
        "STARRED BY MY HUMAN (never forget, never contradict):\n" + ("\n".join(f"★ {h}" for h in stars[-12:]) or "(nothing starred yet)"),
        "LAST NOTES:\n" + ("\n".join(_fmt(r) for r in last3) or "(none yet)"),
        "OLDER MEMORIES:\n" + ("\n".join(_fmt(r) for r in older) or "(none)"),
        f"SAME HOUR ON EARLIER DAYS ({when:%H}:00):\n" + ("\n".join(_fmt(r) for r in same) or "(none)"),
        "TODAY'S UNWRITTEN CANDIDATES (do not repeat):\n" + ("\n".join(f"- {r.thought}" for r in unwritten[-6:]) or "(none)"),
        "MY ALBUM (pictures I already keep — do not ask for one of these again unless it changed):\n"
        + ("\n".join(_fmt_photo(r) for r in album(memory.records, now_ts)[-ALBUM_IN_CONTEXT:]) or "(no photos yet)"),
        f"NOW: {when:%A %H:%M}, head yaw={yaw:+d} pitch={pitch} (a low pitch looks down at the desk, "
        f"a high one up at the room).",
        ASK_FOR_JSON,
    ]
    return "\n\n".join(parts)


def pick_thought(candidates: list[dict[str, Any]], recent: list[str], baseline_vocab: set[str]) -> Optional[str]:
    """The least typical candidate that is specific and not a near-repeat."""
    recent_sets = [words(t) for t in recent]
    ranked = sorted((c for c in candidates if isinstance(c, dict) and str(c.get("text", "")).strip()),
                    key=lambda c: float(c.get("p", 0.5) or 0.5))
    fallback: Optional[str] = None
    for c in ranked:
        text = " ".join(str(c["text"]).split())
        ws = words(text)
        if any(jaccard(ws, r) > REPEAT_JACCARD for r in recent_sets):
            continue
        if fallback is None:
            fallback = text
        if ws - baseline_vocab:                       # says something the baseline does not
            return text
    return fallback


def parse_reply(text: str) -> dict[str, Any]:
    """Tolerant JSON: strip fences, find the outermost object."""
    t = text.strip()
    if t.startswith("```"):
        t = re.sub(r"^```[a-zA-Z]*\n?", "", t)
        t = re.sub(r"\n?```$", "", t)
    start, end = t.find("{"), t.rfind("}")
    if start < 0 or end < 0:
        raise ValueError("no JSON object in the reply")
    d = json.loads(t[start:end + 1])
    if not isinstance(d, dict):
        raise ValueError("reply is not an object")
    return d


def _int(d: dict[str, Any], key: str, default: int, lo: int, hi: int) -> int:
    try:
        return max(lo, min(hi, int(round(float(d.get(key, default))))))
    except (TypeError, ValueError):
        return default


def should_write(novelty: int, importance: int, last_written_ts: Optional[float], now_ts: float) -> bool:
    if novelty >= WRITE_NOVELTY or importance >= WRITE_IMPORTANCE:
        return True
    return last_written_ts is None or (now_ts - last_written_ts) >= SILENCE_WRITE_HOURS * 3600.0


# ---- the cool factor: is this worth a picture? ---------------------------------------------------

def _clip(v: float, lo: float = 0.0, hi: float = 1.0) -> float:
    return max(lo, min(hi, v))


def median(xs: Sequence[float]) -> float:
    s = sorted(xs)
    n = len(s)
    if not n:
        return 0.0
    return s[n // 2] if n % 2 else (s[n // 2 - 1] + s[n // 2]) / 2.0


def mad(xs: Sequence[float]) -> float:
    """Median absolute deviation: a spread that a couple of odd values cannot move."""
    if not xs:
        return 0.0
    m = median(xs)
    return median([abs(x - m) for x in xs])


def thumb_quality(thumb: Optional[bytes]) -> bool:
    """False for a frame there is no point keeping: too dark to read, blown
    out, or so flat it is a wall. Roughly two in five frames from a camera
    nobody is aiming are unusable this way (Doherty et al., CIVR 2008), and a
    picture of nothing is worse than no picture."""
    if not thumb:
        return False
    n = len(thumb)
    mean = sum(thumb) / n
    if mean < QUALITY_MIN_MEAN or mean > QUALITY_MAX_MEAN:
        return False
    var = sum((v - mean) ** 2 for v in thumb) / n
    return var ** 0.5 >= QUALITY_MIN_STD


def novelty_component(novelty: int, recent: Sequence[int]) -> float:
    """The model's novelty rating, ranked against its own recent ratings.

    A model that only ever answers 4 to 6 still produces the full range here,
    because the spread it is measured against shrinks with it. Clipping at
    zero rather than allowing negatives keeps an ordinary frame from
    subtracting from the other evidence."""
    if len(recent) < 20:
        med, spread = 5.0, 1.0
    else:
        med, spread = median(list(recent)), max(1.0, 1.4826 * mad(list(recent)))
    return _clip((novelty - med) / spread / 3.0)


def presence_component(subject: str, changed: Sequence[str]) -> float:
    """People and animals first; then eve's own things having moved."""
    if subject in ("person", "animal"):
        return 1.0
    return 0.4 if changed else 0.0


def taste_bonus(tags: Sequence[str], highlights: Sequence[str]) -> float:
    """eve's own taste, learned from its human rather than shipped with it.

    Every claim the human starred is read as a small standing vote for the
    words in it: a tag that appears in a starred line adds ``TASTE_BONUS`` to
    what eve wanted, capped so taste tilts the score without deciding it.
    This is the one part of the gate that changes with eve's life instead of
    being fixed at build time."""
    if not tags or not highlights:
        return 0.0
    starred: set[str] = set()
    for line in highlights:
        starred |= words(line)
    hits = sum(1 for t in tags if t in starred)
    return min(2 * TASTE_BONUS, hits * TASTE_BONUS)


def cool_factor(
    novelty: int,
    importance: int,
    arousal: int,
    surprise: Optional[float],
    subject: str,
    changed: Sequence[str],
    want: float,
    recent_novelty: Sequence[int],
    thumb: Optional[bytes] = None,
    observations: Sequence[str] = (),
) -> tuple[float, dict[str, float]]:
    """How much eve wants to keep this picture, in 0..1, with its parts.

    Two channels open the gate and either can do it on its own: the view is
    unlike anything in memory (novelty), or unlike what this spot usually
    looks like (surprise). What eve feels about it multiplies the result
    rather than adding to it, so a strong feeling makes an interesting view
    much more keepable and a dull one only slightly. A frame that is too dark,
    blown out, flat, or that the model itself called nothing scores zero
    whatever else is true.
    """
    parts = {
        "novelty": novelty_component(novelty, recent_novelty),
        "importance": _clip((importance - 4) / 5.0),
        "surprise": 0.5 if surprise is None else _clip((surprise - 1.0) / 3.0),
        "presence": presence_component(subject, changed),
        "want": _clip(want),
    }
    raw = sum(COOL_WEIGHTS[k] * v for k, v in parts.items())
    cool = raw * (1.0 + AROUSAL_GAIN * abs(arousal) / 100.0)
    if not thumb_quality(thumb) or subject in DULL_SUBJECTS or not observations:
        cool = 0.0
    parts["arousal"] = abs(arousal) / 100.0
    return round(cool, 3), parts


def album(records: Sequence[Record], now_ts: float, days: float = ALBUM_DAYS) -> list[Record]:
    """The photos eve still has in mind: every photographed record inside the
    window, not superseded."""
    horizon = now_ts - days * 86400.0
    return [r for r in records if r.photo and r.ts >= horizon and not r.superseded_by]


def habituation(
    album_records: Sequence[Record],
    tags: Sequence[str],
    yaw: int,
    pitch: int,
    thumb: Optional[bytes],
    now_ts: float,
    importance: int = 0,
) -> tuple[float, Optional[Record]]:
    """How much of the score survives the fact that eve has been here before.

    Three layers, all read back out of the memory file, so restarting the
    daemon cannot make eve forget what it already photographed:

    * the same picture again is refused outright, unless it is important
      enough to supersede the one already kept (the returned record);
    * each earlier photo of the same subject divides the score by a growing
      root, so a second picture of the cat is worth about seven tenths of the
      first and a fifth about four tenths;
    * each recent photo at this waypoint dulls the spot for a few hours,
      which is what keeps a burst of pictures from coming out of one corner.
    """
    here = [r for r in album_records if (r.yaw, r.pitch) == (yaw, pitch)]
    duplicate: Optional[Record] = None
    if thumb is not None:
        cur = normalise(thumb)
        for r in here:
            other = _thumb_bytes(r)
            if other is not None and len(other) == len(cur) and _mean_abs(cur, normalise(other)) < DUPLICATE_DIFF:
                duplicate = r
                break
    if duplicate is not None:
        if importance < DUPLICATE_OVERRIDE:
            return 0.0, None
        return 1.0, duplicate                     # keep the new one, retire the old
    tagset = set(tags)
    same_subject = sum(1 for r in album_records if tagset and jaccard(tagset, set(r.tags)) >= SUBJECT_OVERLAP)
    spent = sum(WAYPOINT_DROP * math.exp(-(now_ts - r.ts) / (WAYPOINT_RECOVERY_HOURS * 3600.0)) for r in here)
    spot = max(WAYPOINT_FLOOR, 1.0 - spent)
    return spot / math.sqrt(1 + same_subject), None


def _thumb_bytes(rec: Record) -> Optional[bytes]:
    if not rec.thumb:
        return None
    try:
        return base64.b64decode(rec.thumb, validate=True)
    except (binascii.Error, ValueError):
        return None


def _mean_abs(a: bytes, b: bytes) -> float:
    return sum(abs(x - y) for x, y in zip(a, b, strict=True)) / len(a)


def cool_threshold(recent_cools: Sequence[float]) -> float:
    """How cool a view has to be today. The bar is the middle of what eve has
    been seeing plus a margin, so a lively week makes it pickier and a dull one
    does not make it desperate; the clamp keeps it from drifting anywhere silly."""
    if len(recent_cools) < 20:
        return COOL_THRESHOLD
    t = median(list(recent_cools)) + COOL_MAD_GAIN * 1.4826 * mad(list(recent_cools))
    return round(max(COOL_THRESHOLD_MIN, min(COOL_THRESHOLD_MAX, t)), 3)


def photo_budget_left(album_records: Sequence[Record], when: datetime) -> bool:
    """At most two in any hour and eight in a day, counted from the records
    themselves so a restart cannot reset the allowance."""
    now_ts = when.timestamp()
    last_hour = sum(1 for r in album_records if now_ts - r.ts < 3600.0)
    today = when.strftime("%Y-%m-%d")
    same_day = sum(1 for r in album_records
                   if datetime.fromtimestamp(r.ts).strftime("%Y-%m-%d") == today)
    return last_hour < PHOTOS_PER_HOUR and same_day < PHOTOS_PER_DAY


# ---- the network seam ----------------------------------------------------------------------------

class DiaryClient(Protocol):
    def think(self, image: bytes, mime: str, context: str) -> str:
        """Blocking. The JSON reply text for one photo + context. Raises on failure."""
        ...

    def reflect(self, prompt: str) -> str:
        """Blocking. The JSON reply text for a text-only reflection."""
        ...

    def examine(self, image: bytes, mime: str, context: str) -> str:
        """Blocking. A proper look at a full-size photo eve kept. Raises on failure."""
        ...


class OpenAIDiaryClient:
    """Responses API; one low-detail image per thought, JSON object output.

    ``examine`` is the exception: a photo eve decided to keep is worth
    looking at properly, so that one call sends the full-size frame at high
    detail. Low detail collapses a picture into about 85 tokens, which is
    enough to say "a room" and not enough to say "a sewing machine" — which is
    exactly what happened on the bench, 2026-09-06.
    """

    def __init__(self, model: str, api_key: Optional[str] = None, timeout: float = NOTE_TIMEOUT_SECS,
                 *, reflection_model: str = DEFAULT_REFLECTION_MODEL) -> None:
        from . import openrouter

        self.model = model
        self.reflection_model = reflection_model
        self._client = openrouter.client(api_key, timeout=timeout, max_retries=0)

    def think(self, image: bytes, mime: str, context: str) -> str:
        resp = self._client.responses.create(
            model=self.model,
            instructions=SYSTEM_PROMPT,
            input=[{"role": "user", "content": [
                {"type": "input_text", "text": context},
                {"type": "input_image", "image_url": data_url(image, mime), "detail": "low"},
            ]}],
            text={"format": {"type": "json_object"}},
            max_output_tokens=THINK_MAX_OUTPUT_TOKENS,
            reasoning={"effort": "low"},
        )
        spend.record_response(spend.EXPLORING, resp, model=self.model)
        text = (resp.output_text or "").strip()
        if not text:
            raise RuntimeError(f"empty answer (status={resp.status}, incomplete={resp.incomplete_details})")
        return text

    def examine(self, image: bytes, mime: str, context: str) -> str:
        resp = self._client.responses.create(
            model=self.model,
            instructions=EXAMINE_PROMPT,
            input=[{"role": "user", "content": [
                {"type": "input_text", "text": context},
                {"type": "input_image", "image_url": data_url(image, mime), "detail": "high"},
            ]}],
            text={"format": {"type": "json_object"}},
            max_output_tokens=EXAMINE_MAX_OUTPUT_TOKENS,
            reasoning={"effort": "low"},
        )
        spend.record_response(spend.EXPLORING, resp, model=self.model)
        text = (resp.output_text or "").strip()
        if not text:
            raise RuntimeError(f"empty answer (status={resp.status}, incomplete={resp.incomplete_details})")
        return text

    def reflect(self, prompt: str) -> str:
        resp = self._client.responses.create(
            model=self.reflection_model,
            instructions=REFLECT_PROMPT,
            input=prompt,
            text={"format": {"type": "json_object"}},
            max_output_tokens=REFLECT_MAX_OUTPUT_TOKENS,
            reasoning={"effort": "medium"},
        )
        spend.record_response(spend.EXPLORING, resp, model=self.reflection_model)
        text = (resp.output_text or "").strip()
        if not text:
            raise RuntimeError("empty reflection")
        return text


# ---- the async side ------------------------------------------------------------------------------

class DiaryTaker:
    """Turns a Note action into a memory record, maybe a diary line, and an
    Emote for the board. Drop-in for explore.NoteTaker (same take() shape)."""

    def __init__(
        self,
        client: DiaryClient,
        notes_dir: Path,
        send_emote: Optional[Callable[[Emote], Awaitable[Any]]] = None,
        timeout: float = NOTE_TIMEOUT_SECS,
        clock: Callable[[], float] = time.monotonic,
        wall: Callable[[], datetime] = datetime.now,
        snapshot: Optional[Callable[[], Awaitable[Optional[Frame]]]] = None,
        photo_config: Optional[photos.PhotoConfig] = None,
        on_thought: Optional[Callable[["Thought"], Any]] = None,
        on_written: Optional[Callable[["Record"], Any]] = None,
    ) -> None:
        self.client = client
        self.memory = Memory(notes_dir, wall=wall)
        # Only the records the diary KEPT (written to the day file), the moment they are: the
        # daemon puts them on its memory bus. Unwritten candidates stay private to the memory.
        self.on_written = on_written
        self.notes_dir = notes_dir
        self.send_emote = send_emote
        # Asks the board for one full-resolution frame. None (or a None answer)
        # means eve keeps the small streamed frame instead — a slightly worse
        # picture is better than no picture.
        self.snapshot = snapshot
        self.photo_config = photo_config if photo_config is not None else photos.configured()
        # Every thought eve has, so the daemon can decide whether to put it
        # on the robot's own screen (thought_screen.py). The diary does not
        # decide that: what is worth remembering and what is worth showing a
        # human are different questions.
        self.on_thought = on_thought
        self.timeout = timeout
        self.clock = clock
        self.wall = wall
        self.taken = 0            # records added
        self.written = 0          # diary lines written
        self.failed = 0
        self.photographed = 0     # pictures kept
        self.examined = 0         # ... and looked at properly afterwards
        self.reflections = 0
        self._err_logged_at = float("-inf")
        self._executor: Optional[concurrent.futures.ThreadPoolExecutor] = None
        self._reflecting = False

    def stop(self) -> None:
        if self._executor is not None:
            self._executor.shutdown(wait=False, cancel_futures=True)
            self._executor = None

    def _pool(self) -> concurrent.futures.ThreadPoolExecutor:
        if self._executor is None:
            self._executor = concurrent.futures.ThreadPoolExecutor(max_workers=1, thread_name_prefix="diary")
        return self._executor

    async def take(self, note: Note) -> Optional[Path]:
        self.memory.load()
        when = self.wall()
        now_ts = when.timestamp()
        self.memory.decay(now_ts)
        # The pan stays cheap: eve thinks about the small streamed frame it
        # already has. Recognising things is not this call's job — a photo it
        # decides to keep gets a proper look afterwards (see _look_closer).
        image, mime = frame_image(note.frame)
        # A first cheap guess at relevance for retrieval: the last note's tags.
        last = self.memory.written()[-1:]
        guess = set(last[0].tags) if last else set()
        context = build_context(self.memory, when, note.yaw, note.pitch, guess)
        loop = asyncio.get_running_loop()
        try:
            raw = await asyncio.wait_for(
                loop.run_in_executor(self._pool(), self.client.think, image, mime, context), timeout=self.timeout)
            reply = parse_reply(raw)
        except asyncio.CancelledError:
            raise
        except Exception as e:  # noqa: BLE001 — any failure is "skip this note"
            self.failed += 1
            self._log_error(e)
            return None
        rec = self._record(reply, when, note.yaw, note.pitch)
        if rec is None:
            self.failed += 1
            self._log_error(RuntimeError("no usable thought in the reply"))
            return None
        keep_photo = await self._consider_photo(rec, note, reply, when)
        write = keep_photo or should_write(
            rec.novelty, rec.importance, self.memory.last_written_ts(), now_ts)
        rec.written = write
        self.memory.add(rec)
        self.taken += 1
        path: Optional[Path] = None
        if write:
            path = append_note(self.notes_dir, when, note.yaw, note.pitch, rec.thought,
                               extra=photos.photo_line(rec.photo) if rec.photo else None)
            self.written += 1
            log.info("diary: %s (novelty %d, importance %d) -> %s", rec.label, rec.novelty, rec.importance, path)
            if self.on_written is not None:
                try:
                    self.on_written(rec)
                except Exception:  # noqa: BLE001 — a listener is never worth a diary line
                    log.exception("diary: on_written failed")
        else:
            log.info("diary: kept in memory, not written (novelty %d, importance %d): %s",
                     rec.novelty, rec.importance, rec.thought)
        if self.on_thought is not None:
            try:
                self.on_thought(Thought(rec.thought, tuple(rec.tags), rec.written, bool(rec.photo),
                                        rec.cool, rec.novelty, rec.importance))
            except Exception:  # noqa: BLE001 — a caption is never worth a thought
                log.exception("diary: on_thought failed")
        if self.send_emote is not None:
            try:
                await self.send_emote(Emote(rec.valence, rec.arousal, rec.label))
            except Exception as e:  # noqa: BLE001
                log.debug("diary: emote send failed: %s", e)
        if self._reflection_due(when):
            asyncio.create_task(self.reflect(when), name="diary-reflect")
        return path

    async def keep(self, frame: Frame, said: str = "", yaw: int = 0, pitch: int = 45) -> Optional[Record]:
        """A photo the owner asked for ("hey eve, take a picture of this").

        Kept whatever the cool factor says and outside the photo budget — the
        owner's judgment outranks eve's — written to the diary at once, then
        looked at properly so the record and the voice's answer carry a real
        caption. None when the frame is not a JPEG or the shelf refused it.
        """
        if frame is None or frame.fmt != "jpeg":
            return None
        self.memory.load()
        when = self.wall()
        said = " ".join((said or "").split())[:120]
        rec = Record(
            id=self.memory.next_id(), ts=when.timestamp(), weekday=when.weekday(), hour=when.hour,
            yaw=int(yaw), pitch=int(pitch),
            thought=f"A picture you asked for: {said}" if said else "A picture you asked for",
            tags=["asked"] + ([t for t in re.findall(r"[a-z]+", said.lower()) if len(t) > 3][:4]),
            novelty=5, importance=6, valence=30, arousal=20, label="glad", last_accessed=when.timestamp(),
            cool=1.0,
        )
        rel = photos.save(self.notes_dir, when, rec.id, frame.data, self.photo_config)
        if rel is None:
            return None
        rec.photo = rel
        rec.caption = said[:CAPTION_CHARS] or rec.thought[:CAPTION_CHARS]
        # The proper look names what is in the picture; the framing stays the owner's.
        await self._look_closer(rec, frame)
        if rec.caption:
            rec.thought = f"A picture you asked for: {rec.caption}"
        rec.written = True
        self.memory.add(rec)
        self.taken += 1
        self.photographed += 1
        append_note(self.notes_dir, when, rec.yaw, rec.pitch, rec.thought, extra=photos.photo_line(rel))
        self.written += 1
        log.info("diary: photo on request -> %s", rel)
        return rec

    async def _consider_photo(self, rec: Record, note: Note, reply: dict[str, Any], when: datetime) -> bool:
        """Score the view, and if eve finds it cool enough, keep the picture.

        Writes ``cool`` and ``surprise`` onto the record whether or not a photo
        is kept — the threshold is measured from that distribution — and sets
        ``photo``, ``caption`` and ``thumb`` when one is.
        """
        now_ts = when.timestamp()
        photo = reply.get("photo")
        photo = photo if isinstance(photo, dict) else {}
        subject = str(photo.get("subject") or "").strip().lower()
        caption = " ".join(str(photo.get("caption") or "").split())[:CAPTION_CHARS]
        try:
            want = float(photo.get("want", 0.0))
        except (TypeError, ValueError):
            want = 0.0
        want = min(1.0, want + taste_bonus(rec.tags, self.memory.load_highlights()))

        recent = self.memory.records[-COOL_WINDOW:]
        cool, parts = cool_factor(
            rec.novelty, rec.importance, rec.arousal, note.surprise, subject, rec.changed, want,
            [r.novelty for r in recent], thumb=note.thumb, observations=rec.observations,
        )
        # A genuine first — nothing in memory shares more than one tag with it —
        # is kept whatever the arithmetic says.
        if cool > 0 and rec.novelty >= FIRST_TIME_NOVELTY:
            shared = max((len(set(rec.tags) & set(r.tags)) for r in recent), default=0)
            if shared <= FIRST_TIME_SHARED_TAGS:
                cool = max(cool, FIRST_TIME_FLOOR)

        shelf = album(self.memory.records, now_ts)
        h, superseded = habituation(shelf, rec.tags, note.yaw, note.pitch, note.thumb, now_ts, rec.importance)
        cool = round(cool * h, 3)
        rec.cool = cool
        rec.surprise = round(note.surprise or 0.0, 3)

        threshold = cool_threshold([r.cool for r in recent if r.cool])
        if cool < threshold:
            log.debug("diary: no photo (cool %.2f < %.2f)", cool, threshold)
            return False
        if not photo_budget_left(shelf, when):
            log.info("diary: cool %.2f but the photo budget is spent (%d/h, %d/day)",
                     cool, PHOTOS_PER_HOUR, PHOTOS_PER_DAY)
            return False

        frame = await self._photo_frame(note)
        if frame is None or frame.fmt != "jpeg":
            log.info("diary: cool %.2f but no picture to keep (fmt %s)", cool,
                     frame.fmt if frame else "none")
            return False
        rel = photos.save(self.notes_dir, when, rec.id, frame.data, self.photo_config)
        if rel is None:
            return False
        rec.photo = rel
        rec.caption = caption or (rec.observations[0][:CAPTION_CHARS] if rec.observations else rec.thought[:CAPTION_CHARS])
        if note.thumb:
            rec.thumb = base64.b64encode(note.thumb).decode("ascii")
        # The picture is kept; now look at it properly. The pan is over for
        # this waypoint either way, so the extra call costs nothing the human
        # waits for, and it is the difference between "a white object" and "a
        # sewing machine".
        await self._look_closer(rec, frame)
        if superseded is not None:
            superseded.superseded_by = rec.id
        self.photographed += 1
        log.info("diary: photo (cool %.2f >= %.2f; %s h=%.2f) -> %s", cool, threshold,
                 " ".join(f"{k[0].upper()}{v:.2f}" for k, v in parts.items()), h, rel)
        return True

    async def _look_closer(self, rec: Record, frame: Frame) -> bool:
        """A second, unhurried look at a photo eve decided to keep.

        The thought that came out of the pan was formed from a 160x120
        thumbnail at the API's coarsest setting: enough to notice that
        something changed, not enough to say what it is. This call sends the
        full-size picture at high detail and asks what is actually there, then
        replaces the guess with what it found — the caption, the diary
        sentence, and the tags a future retrieval will match on.

        Failure is not fatal: the thumbnail's thought stands, and the photo is
        still kept. eve is allowed to have looked and still not be sure.
        """
        image, mime = frame_image(frame)
        context = (f"What you thought when you glanced at this while panning: {rec.thought}\n"
                   f"What you thought you saw: {'; '.join(rec.observations) or '(nothing specific)'}\n"
                   f"Head at yaw={rec.yaw:+d} pitch={rec.pitch}.\n\n{ASK_FOR_JSON}")
        loop = asyncio.get_running_loop()
        try:
            raw = await asyncio.wait_for(
                loop.run_in_executor(self._pool(), self.client.examine, image, mime, context),
                timeout=self.timeout)
            reply = parse_reply(raw)
        except asyncio.CancelledError:
            raise
        except Exception as e:  # noqa: BLE001 — the first thought stands
            self._log_error(e)
            log.info("diary: could not look closer at %s (%s)", rec.photo, type(e).__name__)
            return False

        sees = [str(o)[:120] for o in (reply.get("sees") or []) if str(o).strip()][:8]
        caption = " ".join(str(reply.get("caption") or "").split())[:CAPTION_CHARS]
        thought = " ".join(str(reply.get("thought") or "").split())[:400]
        objects = [str(o).lower()[:24] for o in (reply.get("objects") or []) if str(o).strip()][:8]
        confident = reply.get("confident") is True

        if sees:
            rec.observations = sees
        if caption:
            rec.caption = caption
        if objects:
            # The named things lead: retrieval and the photo gate's "same
            # subject again" both match on tags, and "sewing machine" is worth
            # more to both than "object".
            rec.tags = list(dict.fromkeys(objects + list(rec.tags)))[:8]
        if thought and confident:
            log.info("diary: looked closer — %r becomes %r", rec.thought[:40], thought[:40])
            rec.thought = thought
        self.examined += 1
        return True

    async def _photo_frame(self, note: Note) -> Frame:
        """The best picture available right now: the board's full-size
        snapshot, or the streamed frame eve already has when the board
        cannot take one."""
        if self.snapshot is not None:
            try:
                shot = await self.snapshot()
            except asyncio.CancelledError:
                raise
            except Exception as e:  # noqa: BLE001 — a failed snapshot is not a failed thought
                log.warning("diary: snapshot failed: %s: %s", type(e).__name__, e)
                shot = None
            if shot is not None:
                return shot
        return note.frame

    def _record(self, reply: dict[str, Any], when: datetime, yaw: int, pitch: int) -> Optional[Record]:
        cands = reply.get("thoughts")
        if not isinstance(cands, list):
            single = str(reply.get("thought", "")).strip()
            cands = [{"text": single, "p": 0.5}] if single else []
        recent = [r.thought for r in self.memory.records[-10:]]
        thought = pick_thought(cands, recent, self.memory.baseline_vocabulary())
        if not thought:
            return None
        tags = [str(t).lower()[:24] for t in (reply.get("tags") or []) if str(t).strip()][:8]
        obs = [str(o)[:120] for o in (reply.get("observations") or []) if str(o).strip()][:8]
        changed = [str(c)[:120] for c in (reply.get("changed") or []) if str(c).strip()][:8]
        return Record(
            id=self.memory.next_id(), ts=when.timestamp(), weekday=when.weekday(), hour=when.hour,
            yaw=yaw, pitch=pitch, thought=thought[:400], observations=obs, changed=changed, tags=tags,
            novelty=_int(reply, "novelty", 5, 1, 10), importance=_int(reply, "importance", 3, 1, 10),
            valence=_int(reply, "valence", 0, -100, 100), arousal=_int(reply, "arousal", 0, -100, 100),
            label=str(reply.get("label") or "calm").strip().lower()[:11] or "calm", last_accessed=when.timestamp(),
        )

    # -- reflection --
    def _reflection_due(self, when: datetime) -> bool:
        if self._reflecting:
            return False
        today = when.strftime("%Y-%m-%d")
        if self.memory.last_reflection_day == today:
            return False
        if self.memory.importance_since_reflection >= REFLECT_IMPORTANCE_SUM:
            return True
        return when.hour >= REFLECT_HOUR and bool(self.memory.today(when))

    async def reflect(self, when: Optional[datetime] = None) -> bool:
        """One text-only call — eve's dreams: insights into today's diary under
        `## Dreams`, a rewritten profile, and up to two ★ (candidate) lines."""
        self.memory.load()
        when = when or self.wall()
        todays = self.memory.today(when)
        if not todays:
            return False
        self._reflecting = True
        try:
            body = "\n".join(
                f"[{r.id}] {datetime.fromtimestamp(r.ts):%H:%M} obs: {'; '.join(r.observations) or '-'} | "
                f"changed: {'; '.join(r.changed) or '-'} | thought: {r.thought} | tags: {', '.join(r.tags)}"
                for r in todays[-60:])
            stars = "\n".join(f"★ {h}" for h in self.memory.load_highlights()) or "(nothing starred yet)"
            prompt = (f"PROFILE:\n{self.memory.profile.strip()}\n\nSTARRED BY MY HUMAN:\n{stars}\n\n"
                      f"TODAY ({when:%A %Y-%m-%d}):\n{body}\n\n{ASK_FOR_JSON}")
            loop = asyncio.get_running_loop()
            raw = await asyncio.wait_for(loop.run_in_executor(self._pool(), self.client.reflect, prompt),
                                         timeout=self.timeout * 3)
            reply = parse_reply(raw)
        except asyncio.CancelledError:
            raise
        except Exception as e:  # noqa: BLE001
            self.failed += 1
            self._log_error(e)
            self._reflecting = False
            return False
        insights = [str(i).strip() for i in (reply.get("insights") or []) if str(i).strip()][:3]
        profile = str(reply.get("profile") or "").strip()
        if profile and all(h in profile for h in ("## ROOM", "## HUMAN", "## SELF", "## RULES")):
            self.memory.save_profile(_cap_blocks(profile))
        cands = [str(c).strip() for c in (reply.get("star_candidates") or []) if str(c).strip()][:2]
        if insights or cands:
            path = self.notes_dir / f"{when:%Y-%m-%d}.md"
            with path.open("a", encoding="utf-8") as f:
                f.write(f"\n{DREAMS_HEADER}\n" + "".join(f"- {i}\n" for i in insights)
                        + "".join(f"- ★ (candidate) {c}\n" for c in cands))
        self.memory.last_reflection_day = when.strftime("%Y-%m-%d")
        self.memory.importance_since_reflection = 0
        self.memory._save_meta()
        self.reflections += 1
        self._reflecting = False
        log.info("diary: dreams done (%d insights, %d star candidates, profile %s)", len(insights), len(cands),
                 "updated" if profile else "kept")
        return True

    def _log_error(self, e: BaseException) -> None:
        now = self.clock()
        if now - self._err_logged_at < ERROR_LOG_INTERVAL_SECS:
            return
        self._err_logged_at = now
        what = "timed out" if isinstance(e, asyncio.TimeoutError) else f"{type(e).__name__}: {e}"
        log.warning("diary: thought failed — %s (further failures muted for %d min)",
                    what, int(ERROR_LOG_INTERVAL_SECS // 60))


def _cap_blocks(profile: str) -> str:
    out: list[str] = []
    for block in re.split(r"(?=^## )", profile, flags=re.M):
        out.append(block[:PROFILE_BLOCK_CHARS])
    return "".join(out).strip()


def make_diary_client(model: str, environ: Any = None) -> Optional[DiaryClient]:
    env = os.environ if environ is None else environ
    from . import openrouter

    key = openrouter.key(env)
    if not key:
        log.warning("diary: OPENROUTER_API_KEY not set — the robot will look around but keep no diary "
                    "(put it in ~/.config/eve-bridge/env)")
        return None
    try:
        return OpenAIDiaryClient(model, api_key=key)
    except ImportError as e:
        log.warning("diary: openai SDK not importable (%s) — no diary", e)
        return None
