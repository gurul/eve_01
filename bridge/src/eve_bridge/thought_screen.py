"""What earns a place on the robot's own screen.

The diary writes down a great deal (diary.py): that is what a memory is for,
and a record nobody reads costs nothing. A screen is the opposite. eve sits
on a desk all day in someone's peripheral vision, and every line it puts up is
a small claim on their attention. Two thoughts that say nearly the same thing
are worth keeping and not worth showing twice.

So the screen has its own gate, stricter than the memory's and independent of
it. The rules, and where the numbers come from:

* **A floor on the thought itself.** Importance >= 6, novelty >= 7, or a cool
  score >= 0.70. Deliberately *not* "the diary wrote it": the diary's write
  gate also fires after three hours of silence, which writes thoughts that are
  dull by construction, and frequent low-importance messages are the strongest
  predictor of a person muting a source outright (Sahami Shirazi et al., *Large-
  scale assessment of mobile notifications*, CHI 2014).
* **The subject veto, which does the real work.** A different sentence about
  the same vent is still the vent. Tags overlapping a recent caption by
  ``SUBJECT_OVERLAP`` inside ``SUBJECT_WINDOW_SECS`` are refused. This is the
  load-bearing rule: measured on eve's own repetitive pairs, no surface
  similarity measure separates them at any threshold anyone ships — "the vent
  is still there" against "the vent is annoyingly centered today" scores 0.25
  content-word Jaccard, far under the 0.7-0.85 that de-duplication pipelines
  use. Only the subject tells them apart.
* **A phrase veto and a similarity backstop**, for literal restatement that the
  subject rule would miss because the tags happened to differ.
* **Silence between captions.** ``MIN_INTERVAL_SECS`` and ``PER_HOUR``: the
  cost of an interruption accrues over about twenty minutes and does not depend
  on how relevant the interruption was (Mark, Gudith & Klocke, *The cost of
  interrupted work*, CHI 2008), so the rate is capped and nothing can buy an
  extra turn.
* **A returning subject gets fewer turns each time.** Beyond
  ``TOPIC_LIMIT`` showings inside ``TOPIC_WINDOW_SECS`` a subject is done for
  the day, however it is phrased.

Ubiquitous tags are stripped before any of this: on a desk robot nearly every
frame contains "desk" or "ceiling", and a tag that appears in everything
identifies nothing.

A refused thought is dropped, not queued. By the time the screen frees up
eve is looking somewhere else, and a stale caption is worse than none.

Pure: no clock of its own, no I/O. The daemon calls ``offer`` with the
monotonic time it already has.
"""

from __future__ import annotations

import logging
import re
from dataclasses import dataclass, field
from typing import Iterable, Optional

log = logging.getLogger(__name__)

# The floor. Any one of these clears it.
FLOOR_IMPORTANCE = 6
FLOOR_NOVELTY = 7
FLOOR_COOL = 0.70

SUBJECT_OVERLAP = 0.5           # tag Jaccard: the same subject
SUBJECT_WINDOW_SECS = 20 * 60.0
TOPIC_LIMIT = 3                 # showings of one subject inside...
TOPIC_WINDOW_SECS = 6 * 3600.0  # ... this window
SIMILARITY = 0.34               # content-word Jaccard: a backstop, not the main rule
HISTORY = 12                    # captions compared against, about two hours of them
MIN_INTERVAL_SECS = 240.0
PER_HOUR = 6
UBIQUITY = 0.5                  # a tag in this share of recent captions identifies nothing

_WORD = re.compile(r"[a-z][a-z\'-]+")

# Words that say nothing about what eve saw. Overlap on these is not
# similarity, and leaving them in makes every sentence look like every other.
STOPWORDS = frozenset("""
a an and are as at be been being but by can could did do does doing done for from had has have
having he her hers him his how i if in into is it its just like me might must my no nor not now
of off on once only or other our out over own same she should so some still such than that the
their them then there these they this those through to too under until up very was we were what
when where which while who whom why will with would you your yours
""".split())


def content_words(text: str) -> set[str]:
    """The words that carry what a sentence is about."""
    return {w for w in _WORD.findall(text.lower()) if w not in STOPWORDS and len(w) > 2}


def content_sequence(text: str) -> list[str]:
    return [w for w in _WORD.findall(text.lower()) if w not in STOPWORDS and len(w) > 2]


def similarity(a: str, b: str) -> float:
    """Jaccard over content words: 1.0 is the same sentence, 0.0 shares nothing."""
    x, y = content_words(a), content_words(b)
    if not x or not y:
        return 0.0
    return len(x & y) / len(x | y)


def trigrams(text: str) -> set[tuple[str, str, str]]:
    """Content-word triples — a repeated turn of phrase, whatever surrounds it."""
    seq = content_sequence(text)
    return {tuple(seq[i:i + 3]) for i in range(len(seq) - 2)}


def tag_overlap(a: set[str], b: set[str]) -> float:
    if not a or not b:
        return 0.0
    return len(a & b) / len(a | b)


@dataclass(frozen=True)
class Shown:
    at: float
    text: str
    tags: frozenset[str]
    subject: frozenset[str]


@dataclass
class ThoughtScreen:
    """Decides which thoughts reach the screen, and remembers what it showed."""

    floor_importance: int = FLOOR_IMPORTANCE
    floor_novelty: int = FLOOR_NOVELTY
    floor_cool: float = FLOOR_COOL
    similarity: float = SIMILARITY
    history: int = HISTORY
    subject_overlap: float = SUBJECT_OVERLAP
    subject_window_secs: float = SUBJECT_WINDOW_SECS
    topic_limit: int = TOPIC_LIMIT
    topic_window_secs: float = TOPIC_WINDOW_SECS
    min_interval_secs: float = MIN_INTERVAL_SECS
    per_hour: int = PER_HOUR
    ubiquity: float = UBIQUITY
    shown: list[Shown] = field(default_factory=list)
    refused: int = 0

    # -- what a thought is about -------------------------------------------------

    def ubiquitous_tags(self) -> set[str]:
        """Tags so common in what eve has shown that they name nothing."""
        recent = self.shown[-self.history:]
        if len(recent) < 4:
            return set()
        counts: dict[str, int] = {}
        for s in recent:
            for tag in s.tags:
                counts[tag] = counts.get(tag, 0) + 1
        limit = self.ubiquity * len(recent)
        return {tag for tag, n in counts.items() if n >= limit}

    def subject_of(self, tags: Iterable[str]) -> set[str]:
        """The tags that actually identify this thought."""
        wanted = {str(t).lower() for t in tags if str(t).strip()}
        narrowed = wanted - self.ubiquitous_tags()
        return narrowed or wanted

    def clears_the_floor(self, importance: int, novelty: int, cool: float) -> bool:
        return (importance >= self.floor_importance
                or novelty >= self.floor_novelty
                or cool >= self.floor_cool)

    # -- the gate ----------------------------------------------------------------

    def refusal(self, now: float, thought: str, tags: Iterable[str] = (), *,
                importance: int = 0, novelty: int = 0, cool: float = 0.0) -> Optional[str]:
        """Why this thought would not go up, or None if it would. Pure."""
        text = " ".join(thought.split())
        if not text:
            return "empty"
        if not self.clears_the_floor(importance, novelty, cool):
            return f"below the floor (importance {importance}, novelty {novelty}, cool {cool:.2f})"
        if self.shown:
            since = now - self.shown[-1].at
            if since < self.min_interval_secs:
                return f"only {since:.0f}s since the last one"
        if len([s for s in self.shown if now - s.at < 3600.0]) >= self.per_hour:
            return f"{self.per_hour} already this hour"

        recent = self.shown[-self.history:]
        subject = self.subject_of(tags)
        for s in recent:
            if now - s.at <= self.subject_window_secs and tag_overlap(subject, set(s.subject)) >= self.subject_overlap:
                shared = "/".join(sorted(subject & set(s.subject)))
                return f"about {shared} again"
        if subject:
            seen = sum(1 for s in self.shown
                       if now - s.at <= self.topic_window_secs
                       and tag_overlap(subject, set(s.subject)) >= self.subject_overlap)
            if seen >= self.topic_limit:
                return f"{seen} about {'/'.join(sorted(subject))} today already"
        tri = trigrams(text)
        for s in recent:
            if tri and tri & trigrams(s.text):
                return f"phrase already used: {sorted(tri & trigrams(s.text))[0]}"
            score = similarity(text, s.text)
            if score >= self.similarity:
                return f"says the same as {s.text[:40]!r} ({score:.2f})"
        return None

    def offer(self, now: float, thought: str, tags: Iterable[str] = (), *,
              importance: int = 0, novelty: int = 0, cool: float = 0.0) -> bool:
        """Show this thought? Records it when the answer is yes."""
        why = self.refusal(now, thought, tags, importance=importance, novelty=novelty, cool=cool)
        if why is not None:
            self.refused += 1
            log.debug("screen: not showing (%s)", why)
            return False
        text = " ".join(thought.split())
        wanted = {str(t).lower() for t in tags if str(t).strip()}
        self.shown.append(Shown(now, text, frozenset(wanted), frozenset(self.subject_of(wanted))))
        # Keep enough history for the longest window that counts back over it.
        keep = max(self.history, self.per_hour * 8)
        if len(self.shown) > keep:
            del self.shown[:-keep]
        return True
