"""Find the name the owner meant when speech-to-text wrote it a little differently.

A device, light or app name that someone made up ("bluefin7") does not survive transcription. On 2026-11-08 the
owner said "bluefin7" to the Voice PE, the transcript said "Blue Fin 7", and the Spotify fast path answered "No
Spotify device called 'blue fin 7'" with bluefin7 in the list next to it. Each module's own matcher compares
words exactly. This module is the last try, after those matchers miss. It forgives what transcription does to a
name:

- spacing and punctuation: "blue fin 7", "Blue-Fin 7", "bluefin 7" all mean bluefin7;
- numbers said as words: "blue fin seven", "pixel seven";
- spellings that sound the same: "grey" for "gray", "kitchen speeker" for "kitchen speaker";
- words heard in another order or with one left out: "pro macbook", "macbook" (when only one name has it);
- one or two wrong letters in a longer name.

It never guesses between two names that are about as close as each other: it returns all of them, and the caller
asks which one.

``resolve(spoken, names)`` returns the indices of the names that ``spoken`` can mean. An empty list means none of
them. One index means that name. Two or more means the caller asks.
"""

from __future__ import annotations

import difflib
import re
from typing import Sequence

# A close spelling must be at least this similar (difflib ratio on the squashed forms).
CLOSE = 0.8
# A second name this close to the best one makes the answer ambiguous.
MARGIN = 0.06
# Names shorter than this (squashed) must match exactly: "tv" and "pc" are too short to be close to anything.
MIN_FUZZY = 4

_UNITS = {"zero": 0, "oh": 0, "one": 1, "two": 2, "three": 3, "four": 4, "five": 5, "six": 6, "seven": 7,
          "eight": 8, "nine": 9, "ten": 10, "eleven": 11, "twelve": 12, "thirteen": 13, "fourteen": 14,
          "fifteen": 15, "sixteen": 16, "seventeen": 17, "eighteen": 18, "nineteen": 19}
_TENS = {"twenty": 20, "thirty": 30, "forty": 40, "fifty": 50, "sixty": 60, "seventy": 70, "eighty": 80,
         "ninety": 90}


def words(text: str) -> list[str]:
    """The name as lower-case words, apostrophes dropped ("sam's" is "sams"), number words as digits
    ("twenty one" is "21", "twelve" is "12")."""
    t = (text or "").lower().replace("’", "'").replace("'", "").replace("+", " plus ")
    raw = re.findall(r"[a-z]+|\d+", t)
    out: list[str] = []
    i = 0
    while i < len(raw):
        w = raw[i]
        if w in _TENS:
            n = _TENS[w]
            if i + 1 < len(raw) and raw[i + 1] in _UNITS and 0 < _UNITS[raw[i + 1]] < 10:
                n += _UNITS[raw[i + 1]]
                i += 1
            out.append(str(n))
        elif w in _UNITS and w != "oh":
            out.append(str(_UNITS[w]))
        else:
            out.append(w)
        i += 1
    return out


def squash(text: str) -> str:
    """The name with every space and mark gone: "Blue Fin 7" is "bluefin7"."""
    return "".join(words(text))


def skeleton(text: str) -> str:
    """How the name sounds, roughly: its consonants with the vowels and doubled letters gone, and the letters that
    sound alike made one ("ph" is "f", "c"/"k"/"q" are "k", "z" is "s"). "blew fin 7" and "bluefin7" are both
    "blfn7". A vowel at the very start stays, so "eve" is not "v"."""
    out = []
    for w in words(text):
        if w.isdigit():
            out.append(w)
            continue
        s = w.replace("ph", "f").replace("ck", "k").replace("qu", "kw").replace("x", "ks")
        s = re.sub(r"[cq]", "k", s).replace("z", "s")
        head, rest = s[0], re.sub(r"[aeiouyhw]", "", s[1:])
        s = head + rest if head in "aeiou" else re.sub(r"[aeiouyhw]", "", s)
        out.append(re.sub(r"(.)\1+", r"\1", s))
    return "".join(out)


def _word_close(a: str, b: str) -> bool:
    if a == b:
        return True
    if a.isdigit() or b.isdigit() or min(len(a), len(b)) < MIN_FUZZY:
        return False
    # the same sound only for longer words: "tubi" and "tube" are both "tb", and are different apps
    if min(len(a), len(b)) >= 5 and skeleton(a) == skeleton(b):
        return True
    return difflib.SequenceMatcher(None, a, b).ratio() >= CLOSE


def resolve(spoken: str, names: Sequence[str]) -> list[int]:
    """The indices of ``names`` that ``spoken`` can mean, tried in tiers; the first tier with a hit answers.

    1. The same name once the spaces are gone ("blue fin 7" → bluefin7).
    2. The same sound ("blew fin seven" → bluefin7).
    3. Every word said is (close to) a word of the name, in any order ("macbook" → "Sam's MacBook Pro").
    4. One close spelling of the whole name. When a second name scores within ``MARGIN`` of the best, both are
       returned, so the caller asks rather than guesses.
    """
    sq = squash(spoken)
    if not sq:
        return []
    keys = [squash(n) for n in names]
    hit = [i for i, k in enumerate(keys) if k and k == sq]
    if hit:
        return hit
    if len(sq) >= MIN_FUZZY:
        sk = skeleton(spoken)
        hit = [i for i, n in enumerate(names) if len(sk) >= 4 and skeleton(n) == sk]
        if hit:
            return hit
    said = words(spoken)
    if said and not all(w.isdigit() for w in said):      # "seven" alone is not a name
        hit = []
        for i, n in enumerate(names):
            mine, joined = words(n), squash(n)
            # each word said is a word of the name, or (for a split name) a piece of the joined name
            if all(any(_word_close(w, m) for m in mine) or (len(w) >= MIN_FUZZY and w in joined) for w in said) \
                    and (len(said) > 1 or len(sq) >= MIN_FUZZY or said[0] in mine):
                hit.append(i)
        if hit:
            return hit
    if len(sq) < MIN_FUZZY:
        return []
    scored = sorted(((difflib.SequenceMatcher(None, sq, k).ratio(), i) for i, k in enumerate(keys) if k),
                    reverse=True)
    if not scored or scored[0][0] < CLOSE:
        return []
    best = scored[0][0]
    return [i for s, i in scored if s >= best - MARGIN]
