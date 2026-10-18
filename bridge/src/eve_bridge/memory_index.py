"""Search memory by meaning and by words: a local index over the transcripts, and the three search switches.

The memory plan (2026-10-08, items 1, 4 and 5) names three changes to ``memory_search``. Each ships behind its own
switch, off in code until the memory eval (``tools/memory_eval.py``) clears the pre-registered bar for it:

- ``EVE_MEMORY_HYBRID=1``: the transcript part of a search is ranked, not filtered. Before, every word of the
  query had to be in a line ("keyword AND"), so "landlord name" missed "Carla, my landlord, says ...". Now each
  round of a conversation (one owner line and eve's reply to it) is one unit in a local SQLite file, with an FTS5
  (BM25) row and a vector from the same embeddings model mem0 uses. A search scores every unit in its days as
  ``0.3 * BM25 (divided by the best BM25 of the search) + 0.7 * cosine`` and returns the best 8, each with the line
  before it and the reply. When the index or the embeddings call is not there, the search is the keyword AND it
  always was.
- ``EVE_MEMORY_TIMEARGS=1``: ``memory_search`` takes ``from`` and ``to``. Each is a day (year-month-day) or the
  owner's own words for a time ("yesterday", "last week", "in May", "two weeks ago"); ``resolve_when`` turns the
  words into days in code, on the owner's day (it starts at 04:00). A range that finds nothing is widened once.
- ``EVE_MEMORY_RETRY=1``: a search that finds nothing tries again by itself over a whole year (and, without
  the index, with fewer of the words), and says to search once more with other words before answering "I don't
  have that". The Telegram brain allows at most three memory calls in one turn (telegram.py).

### The index file

``<memory>/index/memory.sqlite3`` (0600, in a 0700 folder, under the same guards as the transcripts). Tables:
``units`` (one row per round: day, conversation, time, channel, the opening line, the reply, the line before, the
hash of its text), ``fts`` (the text, BM25), ``vectors`` (hash → float32 vector) and ``days`` (each day file's
size, mtime and inode when it was indexed). A day whose file changed is indexed again before it is searched, so a
line appended a moment ago, or a line the forget CLI redacted in another process, is never stale. Vectors are kept
by the hash of the text: indexing a day again embeds only the rounds that changed.

New lines reach the index off the event loop: ``Transcripts.append`` calls ``notify`` (a set and an Event), and a
worker thread indexes the day 1.5 s after the last line. It embeds only after EMBED_IDLE_SECS with no new line, so
a round that is still growing is not sent to the embeddings endpoint once per line; until then the new round is
found by its words. Every refresh that indexed a day again drops the vectors no round uses. Only the daemon runs
the worker (``Memory(background=True)``); the forget, dream and reindex commands open the file without it. A
missing file is built again in the background when the daemon opens memory; until it is ready, a search is the
keyword AND.

### Forget reaches it

``forget`` deletes every unit whose full stored text (the opening line, every reply line, the line before)
matches, its FTS row and every vector no unit uses any more. Then every indexed day whose file changed is indexed
again from the redacted transcripts (so words a match could not see, like a round redacted by its own line, go
too), and the FTS segments are rewritten and the file VACUUMed, every time, with ``secure_delete`` on and FTS5's
own secure-delete on, so the words are not left in free pages or old segments. A step that fails after retries
raises ``IndexForgetError``: the forget reports the index as failed, never as done. The forget opens the file
whenever it exists, with the hybrid switch on or off (``Memory._ranked_for_forget``).

### What leaves the machine

The text of each round and each query goes to OpenAI's embeddings endpoint (``text-embedding-3-small``, 512
dimensions), as mem0's already does. The endpoint has no ``store`` flag. Logs carry counts and milliseconds only.
"""

from __future__ import annotations

import calendar
import hashlib
import json
import logging
import os
import re
import sqlite3
import threading
import time
from dataclasses import dataclass, field
from datetime import date, datetime, timedelta
from pathlib import Path
from typing import Any, Callable, Iterable, Optional, Sequence

log = logging.getLogger(__name__)

HYBRID_ENV = "EVE_MEMORY_HYBRID"
TIMEARGS_ENV = "EVE_MEMORY_TIMEARGS"
RETRY_ENV = "EVE_MEMORY_RETRY"
SWITCHES = {"hybrid": HYBRID_ENV, "timeargs": TIMEARGS_ENV, "retry": RETRY_ENV}
# Each switch's value when its variable is not set: the memory eval's holdout verdict
# (tests/fixtures/memory_eval/VERDICT.md, 2026-10-08). tests/test_memory_verdict.py keeps the two equal.
DEFAULTS = {"hybrid": False, "timeargs": False, "retry": False}

INDEX_SUBDIR = "index"
DB_FILE = "memory.sqlite3"
INDEX_VERSION = 1                     # bump when a unit's text or the tables change: the file is built again
EMBED_MODEL = "text-embedding-3-small"   # mem0_memory.DEFAULT_EMBED_MODEL
EMBED_DIMS = 512
EMBED_BATCH = 256
EMBED_TIMEOUT_SECS = 20.0             # indexing, off the loop
QUERY_TIMEOUT_SECS = 2.0              # one query's vector; past this the search is the keyword AND
BM25_WEIGHT = 0.3
COS_WEIGHT = 0.7
TOP_K = 8
MIN_COS = 0.30                        # a unit with no word in common must be at least this close to count as found
FTS_CANDIDATES = 300
UNIT_CHARS = 2000                     # one unit's text, for FTS and the embedding
HIT_CHARS = 300
NEIGHBOUR_CHARS = 200
REPLY_CHARS = 300
DEBOUNCE_SECS = 1.5
EMBED_IDLE_SECS = 30.0                # the worker embeds new rounds after this long with no new line
COMPACT_WAITS = (0.0, 0.5, 1.0, 2.0)  # forget's optimize and VACUUM: tried again after these waits (a busy file)
DAY_START_HOUR = 4
FORGOTTEN = "(forgotten)"

STOPWORDS = frozenset("""
a about after again all also am an and any are as at be been before being but by can could did do does doing done
for from had has have having he her him his how i if in into is it its just me my myself no not of on or our ours
out over she so some than that the their them then there these they this those to too up us was we were what when
where which while who whom why will with would you your yours yourself did say said tell told ever much many
""".split())

_WORD = re.compile(r"\w+", re.UNICODE)


def switch(name: str, environ: Any = None) -> bool:
    """One of the three search switches (``SWITCHES``: a key or the variable's name). On when set to 1/true/yes/on,
    off when set to anything else, and ``DEFAULTS`` when not set (or empty)."""
    env = os.environ if environ is None else environ
    var = SWITCHES.get(name, name)
    key = next((k for k, v in SWITCHES.items() if v == var), name)
    raw = str(env.get(var) or "").strip().lower()
    if not raw:
        return DEFAULTS.get(key, False)
    return raw in ("1", "true", "yes", "on")


# ---- time: the owner's words to a range of days -------------------------------------------------------------

_MONTHS = {m.lower(): i for i, m in enumerate(calendar.month_name) if m}
_MONTHS.update({m.lower(): i for i, m in enumerate(calendar.month_abbr) if m})
_MONTHS["sept"] = 9
_MON = "|".join(sorted(_MONTHS, key=len, reverse=True))
_WEEKDAYS = {d.lower(): i for i, d in enumerate(calendar.day_name)}
_WD = "|".join(_WEEKDAYS)
_NUMBERS = {"a": 1, "an": 1, "one": 1, "two": 2, "three": 3, "four": 4, "five": 5, "six": 6, "seven": 7,
            "eight": 8, "nine": 9, "ten": 10, "eleven": 11, "twelve": 12, "couple": 2, "a couple": 2,
            "a couple of": 2, "few": 3, "a few": 3}
_NUM = r"(\d+|a couple of|a couple|a few|an|a|one|two|three|four|five|six|seven|eight|nine|ten|eleven|twelve)"


def owner_day(now: datetime) -> date:
    """The owner's day at `now`: it starts at 04:00, so 01:00 belongs to the evening before."""
    return (now - timedelta(hours=DAY_START_HOUR)).date()


def _num(word: str) -> int:
    word = word.strip().lower()
    return int(word) if word.isdigit() else _NUMBERS.get(word, 1)


def _month_range(year: int, month: int) -> tuple[date, date]:
    return date(year, month, 1), date(year, month, calendar.monthrange(year, month)[1])


def _shift_month(year: int, month: int, back: int) -> tuple[int, int]:
    n = year * 12 + (month - 1) - back
    return n // 12, n % 12 + 1


def _past_date(today: date, month: int, day: int, year: Optional[int]) -> Optional[date]:
    """month/day in `year`, or with no year the latest one on or before today."""
    try:
        if year is not None:
            return date(year, month, day)
        d = date(today.year, month, day)
        return d if d <= today else date(today.year - 1, month, day)
    except ValueError:
        return None


def resolve_when(text: Any, now: datetime) -> Optional[tuple[date, date]]:
    """The first and last day the owner's words for a time mean, at `now` on the owner's clock (a day starts at
    04:00), or None when the words are not a time this knows. Never a day after today.

    The set it knows: a day (2026-09-03, "September 3", "3 Sept 2026"); today, tonight, this morning; yesterday,
    last night, the day before yesterday; N days ago; N weeks ago (that Monday-to-Sunday week); this week, last
    week, the past week; this weekend, last weekend; a weekday ("on Tuesday", "last Tuesday"); a month ("in May",
    "early May", "late May", "May 2026"); this month, last month, N months ago; this year, last year; and
    "since" any of these (from its first day to today)."""
    raw = " ".join(str(text or "").lower().replace(",", " ").split())
    if not raw:
        return None
    today = owner_day(now)
    s = re.sub(r"^(on|in|during|from|at|around|over|for)\s+", "", raw).strip()
    s = re.sub(r"^the\s+", "", s)
    since = False
    m = re.match(r"^(since|after)\s+(.*)$", s)
    if m:
        since, s = True, re.sub(r"^the\s+", "", m.group(2))
    got = _resolve_core(s, today)
    if got is None:
        return None
    lo, hi = got
    if since:
        hi = today
    hi = min(hi, today)
    if lo > hi:
        lo = hi
    return lo, hi


def _resolve_core(s: str, today: date) -> Optional[tuple[date, date]]:
    m = re.match(r"^(\d{4})-(\d{1,2})-(\d{1,2})$", s)
    if m:
        try:
            d = date(int(m[1]), int(m[2]), int(m[3]))
        except ValueError:
            return None
        return d, d
    if s in ("today", "this morning", "this afternoon", "this evening", "tonight", "earlier today", "now"):
        return today, today
    if s in ("yesterday", "last night", "yesterday morning", "yesterday afternoon", "yesterday evening"):
        d = today - timedelta(days=1)
        return d, d
    if s == "day before yesterday":
        d = today - timedelta(days=2)
        return d, d
    m = re.match(rf"^{_NUM}\s+days?\s+ago$", s)
    if m:
        d = today - timedelta(days=_num(m[1]))
        return d, d
    m = re.match(rf"^{_NUM}\s+weeks?\s+ago$", s)
    if m:
        d = today - timedelta(days=7 * _num(m[1]))
        mon = d - timedelta(days=d.weekday())
        return mon, mon + timedelta(days=6)
    if s in ("this week", "earlier this week"):
        return today - timedelta(days=today.weekday()), today
    if s == "last week":
        mon = today - timedelta(days=today.weekday() + 7)
        return mon, mon + timedelta(days=6)
    m = re.match(rf"^(?:past|last)\s+{_NUM}\s+(days?|weeks?)$", s)
    if m:
        n = _num(m[1]) * (7 if m[2].startswith("week") else 1)
        return today - timedelta(days=n), today
    if s in ("past week", "past 7 days", "last 7 days", "past few days", "last few days", "last couple of days",
             "past couple of days", "recently", "lately"):
        n = 7 if "week" in s or "7" in s or s in ("recently", "lately") else 3
        return today - timedelta(days=n), today
    if s in ("weekend", "this weekend", "last weekend", "past weekend"):
        sat = today - timedelta(days=(today.weekday() - 5) % 7)
        if s == "last weekend" and today.weekday() in (5, 6):
            sat -= timedelta(days=7)
        return sat, sat + timedelta(days=1)
    m = re.match(rf"^(last|this|past)?\s*({_WD})(?:\s+(?:morning|afternoon|evening|night))?$", s)
    if m:
        back = (today.weekday() - _WEEKDAYS[m[2]]) % 7
        if m[1] == "last" and back == 0:
            back = 7
        d = today - timedelta(days=back)
        return d, d
    if s in ("this month", "earlier this month"):
        return date(today.year, today.month, 1), today
    if s == "last month":
        return _month_range(*_shift_month(today.year, today.month, 1))
    m = re.match(rf"^{_NUM}\s+months?\s+ago$", s)
    if m:
        return _month_range(*_shift_month(today.year, today.month, _num(m[1])))
    if s == "this year":
        return date(today.year, 1, 1), today
    if s == "last year":
        return date(today.year - 1, 1, 1), date(today.year - 1, 12, 31)
    m = re.match(rf"^({_MON})\.?\s+(\d{{1,2}})(?:st|nd|rd|th)?(?:\s+(\d{{4}}))?$", s) or None
    if m:
        d = _past_date(today, _MONTHS[m[1]], int(m[2]), int(m[3]) if m[3] else None)
        return (d, d) if d else None
    m = re.match(rf"^(\d{{1,2}})(?:st|nd|rd|th)?\s+(?:of\s+)?({_MON})\.?(?:\s+(\d{{4}}))?$", s)
    if m:
        d = _past_date(today, _MONTHS[m[2]], int(m[1]), int(m[3]) if m[3] else None)
        return (d, d) if d else None
    m = re.match(rf"^(?:(early|mid|late|end of|the end of|beginning of|the beginning of|start of)\s+)?({_MON})\.?"
                 rf"(?:\s+(\d{{4}}))?$", s)
    if m:
        month = _MONTHS[m[2]]
        year = int(m[3]) if m[3] else (today.year if month <= today.month else today.year - 1)
        lo, hi = _month_range(year, month)
        part = (m[1] or "").replace("the ", "")
        if part in ("early", "beginning of", "start of"):
            hi = lo + timedelta(days=9)
        elif part == "mid":
            lo, hi = lo + timedelta(days=10), lo + timedelta(days=19)
        elif part in ("late", "end of"):
            lo = lo + timedelta(days=20)
        return lo, hi
    return None


def resolve_args(date_from: Any, date_to: Any, now: datetime) -> tuple[Optional[tuple[date, date]], bool]:
    """(the range a search's `from` and `to` mean, whether a value that was given could not be read). `from` alone
    is its own range ("last week" is Monday to Sunday); `to` alone ends a year-long range."""
    given = [v for v in (date_from, date_to) if v is not None and str(v).strip()]
    if not given:
        return None, False
    a = resolve_when(date_from, now) if date_from is not None and str(date_from).strip() else None
    b = resolve_when(date_to, now) if date_to is not None and str(date_to).strip() else None
    unread = (bool(str(date_from or "").strip()) and a is None) or (bool(str(date_to or "").strip()) and b is None)
    if a and b:
        lo, hi = min(a[0], b[0]), max(a[1], b[1])
    elif a:
        lo, hi = a
    elif b:
        lo, hi = b[1] - timedelta(days=365), b[1]
    else:
        return None, unread
    return (lo, hi), unread


def widen(rng: tuple[date, date], today: date) -> tuple[date, date]:
    """A range made wider once: by its own length (at least a week) on each side, never past today."""
    lo, hi = rng
    pad = timedelta(days=max(7, (hi - lo).days + 1))
    return lo - pad, min(today, hi + pad)


# ---- words ----------------------------------------------------------------------------------------------------

def terms(query: str) -> list[str]:
    """The query's telling words: lowercased, no stop words, no one-letter words, each once, in order."""
    out: list[str] = []
    for w in _WORD.findall(str(query or "").lower()):
        if len(w) > 1 and w not in STOPWORDS and w not in out:
            out.append(w)
    return out


def fts_query(query: str) -> str:
    """An FTS5 MATCH string: any of the telling words (each quoted, so no word is read as an operator)."""
    return " OR ".join('"' + w.replace('"', "") + '"' for w in terms(query))


# ---- units: one round of a conversation ----------------------------------------------------------------------

@dataclass
class Unit:
    day: str
    conv: str
    ts: str
    ch: str
    who: str
    head: str                         # the line that opened the round, with its speaker
    reply: list[str] = field(default_factory=list)
    before: str = ""

    @property
    def body(self) -> str:
        return _clip("\n".join([self.head, *self.reply]), UNIT_CHARS)

    @property
    def hash(self) -> str:
        return hashlib.sha256(f"{EMBED_MODEL}|{EMBED_DIMS}|{self.body}".encode("utf-8")).hexdigest()


def _flat(text: Any) -> str:
    return " ".join(str(text or "").split())


def _clip(text: str, cap: int) -> str:
    return text if len(text) <= cap else text[: max(0, cap - 1)].rstrip() + "…"


def units_of(day: str, lines: Sequence[dict[str, Any]], speaker: Callable[[dict[str, Any]], str]) -> list[Unit]:
    """A day's lines as rounds: each line that is not eve's (the owner's, a memory-bus line) opens a round, and
    eve's lines after it in the same conversation are its reply. Eve lines with nothing before them open
    their own round. Close markers end a conversation's round; forgotten and empty lines are left out."""
    units: list[Unit] = []
    open_: dict[str, Unit] = {}
    last: dict[str, str] = {}
    for ln in lines:
        conv = ln.get("conv", "")
        if ln.get("kind") == "close":
            open_.pop(conv, None)
            continue
        text = _flat(ln.get("text"))
        if ln.get("kind") not in ("say", "tool", "image") or not text or text == FORGOTTEN:
            continue
        said = f"{speaker(ln)}: {text}"
        cur = open_.get(conv)
        if ln.get("who") == "eve" and cur is not None:
            cur.reply.append(said)
        else:
            cur = Unit(day=day, conv=conv, ts=str(ln.get("ts") or ""), ch=str(ln.get("ch") or ""),
                       who=str(ln.get("who") or ""), head=said, before=last.get(conv, ""))
            units.append(cur)
            open_[conv] = cur
        last[conv] = said
    return units


# ---- embeddings -----------------------------------------------------------------------------------------------

Embedder = Callable[[Sequence[str], float], list[list[float]]]


def openai_embedder() -> Optional[Embedder]:
    """OpenAI's text-embedding-3-small at EMBED_DIMS dimensions, through OpenRouter on the owner's key (openrouter.py,
    2026-10-08; it was OpenAI's own endpoint before). The same model and the same dimensions, so the vectors already
    in the index stay comparable and nothing is re-embedded. Each call is on the spend meter. None without a key."""
    from . import openrouter

    if not openrouter.available():
        return None

    clients: dict[float, Any] = {}

    def embed(texts: Sequence[str], timeout: float) -> list[list[float]]:
        from . import spend

        client = clients.get(timeout)
        if client is None:
            client = clients[timeout] = openrouter.client(timeout=timeout, max_retries=1)
        resp = client.embeddings.create(model=EMBED_MODEL, input=list(texts), dimensions=EMBED_DIMS)
        try:
            spend.record_embedding(spend.MEMORY, EMBED_MODEL, resp)
        except Exception:  # noqa: BLE001 - the meter never costs a search
            pass
        return [list(d.embedding) for d in sorted(resp.data, key=lambda d: d.index)]

    return embed


def _unit_vec(values: Sequence[float]) -> Any:
    import numpy as np

    v = np.asarray(values, dtype=np.float32)
    n = float(np.linalg.norm(v))
    return v / n if n > 0 else v


# ---- the index ----------------------------------------------------------------------------------------------

SCHEMA = """
CREATE TABLE IF NOT EXISTS meta(k TEXT PRIMARY KEY, v TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS days(day TEXT PRIMARY KEY, sig TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS units(id INTEGER PRIMARY KEY, day TEXT NOT NULL, conv TEXT NOT NULL, ts TEXT NOT NULL,
    ch TEXT NOT NULL, who TEXT NOT NULL, head TEXT NOT NULL, reply TEXT NOT NULL, before TEXT NOT NULL,
    hash TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS units_day ON units(day);
CREATE INDEX IF NOT EXISTS units_hash ON units(hash);
CREATE VIRTUAL TABLE IF NOT EXISTS fts USING fts5(body, tokenize='porter unicode61');
CREATE TABLE IF NOT EXISTS vectors(hash TEXT PRIMARY KEY, v BLOB NOT NULL);
"""
META = {"version": str(INDEX_VERSION), "model": EMBED_MODEL, "dims": str(EMBED_DIMS)}


def index_path(store: Path) -> Path:
    return Path(store).expanduser() / INDEX_SUBDIR / DB_FILE


class IndexForgetError(Exception):
    """A forget could not finish cleaning the index (the file stayed busy or failed): the words may remain."""


class MemoryIndex:
    """The ranked search over one memory folder's transcripts. Every method is synchronous, thread-safe and never
    raises; ``search`` answers None when it cannot rank (the caller then runs the keyword AND)."""

    def __init__(self, path: Path, transcripts: Any, embedder: Optional[Embedder] = None) -> None:
        self.path = Path(path)
        self.transcripts = transcripts
        self.embedder = embedder
        self._lock = threading.RLock()
        self._dirty: set[str] = set()
        self._wake = threading.Event()
        self._stop = False
        self._worker: Optional[threading.Thread] = None
        self._built = False
        self.ok = False
        try:
            self._db = self._open()
            self.ok = True
        except (OSError, sqlite3.Error) as e:
            log.warning("memory index: off — it could not be opened (%s)", type(e).__name__)

    # -- the file --

    def _open(self) -> sqlite3.Connection:
        folder = self.path.parent
        folder.mkdir(parents=True, exist_ok=True)
        if folder.is_symlink():
            raise OSError("the index folder is a symlink")
        os.chmod(folder, 0o700)
        if not self.path.exists():
            os.close(os.open(self.path, os.O_WRONLY | os.O_CREAT | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600))
        os.chmod(self.path, 0o600)
        db = sqlite3.connect(str(self.path), check_same_thread=False, isolation_level=None, timeout=10.0)
        db.execute("PRAGMA secure_delete=ON")
        db.execute("PRAGMA journal_mode=DELETE")
        db.executescript(SCHEMA)
        try:                                          # a delete leaves no term behind in an older FTS segment
            db.execute("INSERT INTO fts(fts, rank) VALUES('secure-delete', 1)")
        except sqlite3.Error:                         # an SQLite before 3.44: optimize and VACUUM still run
            pass
        have = dict(db.execute("SELECT k, v FROM meta").fetchall())
        if have and have != META:                     # another version or model: everything is built again
            db.executescript("DELETE FROM units; DELETE FROM fts; DELETE FROM vectors; DELETE FROM days;")
            have = {}
        if not have:
            db.executemany("INSERT OR REPLACE INTO meta(k, v) VALUES(?, ?)", list(META.items()))
        return db

    def close(self) -> None:
        self._stop = True
        self._wake.set()
        worker = self._worker
        if worker is not None and worker is not threading.current_thread():
            worker.join(timeout=5.0)
        with self._lock:
            if self.ok:
                try:
                    self._db.close()
                except sqlite3.Error:
                    pass
                self.ok = False

    # -- keeping it current --

    def _sig(self, day: str) -> Optional[str]:
        try:
            st = os.stat(self.transcripts.path_of(day))
        except OSError:
            return None
        return f"{st.st_mtime_ns}:{st.st_size}:{st.st_ino}"

    def refresh(self, days: Optional[Iterable[str]] = None, *, embed: bool = True) -> int:
        """Index again every day (or each of `days`) whose file changed; with `embed`, embed the rounds that have no
        vector yet. A full refresh (no `days`) also drops days that are gone and vectors no round uses. → rounds
        indexed."""
        if not self.ok:
            return 0
        try:
            changed = self._refresh(days)
        except sqlite3.Error as e:
            log.warning("memory index: refresh failed (%s)", type(e).__name__)
            return 0
        if embed:
            self.embed_missing()
        return changed

    def _refresh(self, days: Optional[Iterable[str]]) -> int:
        """refresh's work, in one transaction; raises sqlite3.Error after a rollback. Every refresh that indexed or
        dropped a day also drops the vectors no round uses, so a round that grew line by line leaves none behind."""
        full = days is None
        changed = 0
        with self._lock:
            try:
                on_disk = list(self.transcripts.days()) if full else sorted(set(days or ()))
                have = dict(self._db.execute("SELECT day, sig FROM days").fetchall())
                touched = False
                self._db.execute("BEGIN")
                for day in on_disk:
                    sig = self._sig(day)
                    if sig is None:
                        if day in have:
                            self._drop_day(day)
                            touched = True
                        continue
                    if have.get(day) == sig:
                        continue
                    changed += self._index_day(day, sig)
                    touched = True
                if full:
                    for day in set(have) - set(on_disk):
                        self._drop_day(day)
                if full or touched:
                    self._gc()
                self._db.execute("COMMIT")
            except sqlite3.Error:
                self._rollback()
                raise
        return changed

    def _rollback(self) -> None:
        try:
            self._db.execute("ROLLBACK")
        except sqlite3.Error:
            pass

    def _drop_day(self, day: str) -> None:
        self._db.execute("DELETE FROM fts WHERE rowid IN (SELECT id FROM units WHERE day = ?)", (day,))
        self._db.execute("DELETE FROM units WHERE day = ?", (day,))
        self._db.execute("DELETE FROM days WHERE day = ?", (day,))

    def _index_day(self, day: str, sig: str) -> int:
        from .transcripts import speaker

        units = units_of(day, self.transcripts.lines(day), speaker)
        self._db.execute("DELETE FROM fts WHERE rowid IN (SELECT id FROM units WHERE day = ?)", (day,))
        self._db.execute("DELETE FROM units WHERE day = ?", (day,))
        for u in units:
            cur = self._db.execute(
                "INSERT INTO units(day, conv, ts, ch, who, head, reply, before, hash) VALUES(?,?,?,?,?,?,?,?,?)",
                (u.day, u.conv, u.ts, u.ch, u.who, u.head, json.dumps(u.reply, ensure_ascii=False), u.before,
                 u.hash))
            self._db.execute("INSERT INTO fts(rowid, body) VALUES(?, ?)", (cur.lastrowid, u.body))
        self._db.execute("INSERT OR REPLACE INTO days(day, sig) VALUES(?, ?)", (day, sig))
        return len(units)

    def _gc(self) -> int:
        cur = self._db.execute("DELETE FROM vectors WHERE hash NOT IN (SELECT hash FROM units)")
        return cur.rowcount or 0

    def embed_missing(self, budget_secs: float = 60.0) -> int:
        """Embed the rounds that have no vector, in batches, outside the lock. A vector is kept only while a round
        still uses it (a forget in between leaves none behind). → vectors added."""
        if not self.ok or self.embedder is None:
            return 0
        added = 0
        deadline = time.monotonic() + budget_secs
        while time.monotonic() < deadline:
            with self._lock:
                try:
                    rows = self._db.execute(
                        "SELECT u.hash, f.body FROM units u JOIN fts f ON f.rowid = u.id "
                        "LEFT JOIN vectors v ON v.hash = u.hash WHERE v.hash IS NULL GROUP BY u.hash LIMIT ?",
                        (EMBED_BATCH,)).fetchall()
                except sqlite3.Error:
                    return added
            if not rows:
                break
            try:
                vecs = self.embedder([body for _h, body in rows], EMBED_TIMEOUT_SECS)
            except Exception as e:  # noqa: BLE001 - no vectors this time; the words still rank
                log.warning("memory index: embedding failed (%s); %d rounds wait", type(e).__name__, len(rows))
                break
            with self._lock:
                try:
                    self._db.execute("BEGIN")
                    for (h, _body), v in zip(rows, vecs, strict=False):
                        self._db.execute("INSERT OR IGNORE INTO vectors(hash, v) SELECT ?, ? "
                                         "WHERE EXISTS (SELECT 1 FROM units WHERE hash = ?)",
                                         (h, _unit_vec(v).tobytes(), h))
                        added += 1
                    self._db.execute("COMMIT")
                except sqlite3.Error:
                    self._rollback()
                    break
        return added

    def ensure_built(self) -> bool:
        """The whole index current and embedded, now (the eval calls this before a question). → ok."""
        if not self.ok:
            return False
        self.refresh(None, embed=True)
        self._built = True
        return True

    # -- the worker --

    def start(self) -> None:
        """The background worker: builds a missing index once, then indexes each day `notify` names."""
        if not self.ok or self._worker is not None:
            return
        self._worker = threading.Thread(target=self._run, name="memory-index", daemon=True)
        self._worker.start()

    def notify(self, day: str) -> None:
        """A line was appended to `day` (Transcripts.append calls this on the event loop: a set and an Event)."""
        self._dirty.add(day)
        self._wake.set()

    def _run(self) -> None:
        try:
            self.ensure_built()
        except Exception as e:  # noqa: BLE001 - the worker never dies of one failure
            log.warning("memory index: build failed (%s)", type(e).__name__)
        embed_at: Optional[float] = None              # when the new rounds are embedded: EMBED_IDLE_SECS of quiet
        while not self._stop:
            wait = None if embed_at is None else max(0.0, embed_at - time.monotonic())
            woke = self._wake.wait(wait)
            if self._stop:
                break
            if woke:
                time.sleep(DEBOUNCE_SECS)
                self._wake.clear()
                days, self._dirty = self._dirty, set()
                if days:
                    try:
                        self.refresh(days, embed=False)   # the words now; the vectors once the talk goes quiet
                    except Exception as e:  # noqa: BLE001
                        log.warning("memory index: update failed (%s)", type(e).__name__)
                embed_at = time.monotonic() + EMBED_IDLE_SECS
                continue
            embed_at = None
            try:
                self.embed_missing()
            except Exception as e:  # noqa: BLE001
                log.warning("memory index: embedding failed (%s)", type(e).__name__)

    # -- search --

    def search(self, query: str, first_day: str, last_day: str, limit: int = TOP_K,
               channels: Optional[Sequence[str]] = None) -> Optional[dict[str, Any]]:
        """The best `limit` rounds between two days (inclusive), as transcript-search hits, best first:
        {"hits", "found"}. A round counts as found when it shares a telling word with the query or its cosine is at
        least MIN_COS. None when the index or the query's vector is not there."""
        # Not built yet: the keyword AND answers. A search never starts the build-and-embed worker itself: only
        # the daemon runs it (Memory(background=True) starts it when memory opens), and a CLI search that did
        # would begin embedding a year of transcripts in a process about to exit.
        if not self.ok or self.embedder is None or not self._built:
            return None
        t0 = time.monotonic()
        days = [d for d in self.transcripts.days() if first_day <= d <= last_day]
        self.refresh(days, embed=False)               # a line said a moment ago, or redacted elsewhere
        try:
            qv = _unit_vec(self.embedder([str(query)], QUERY_TIMEOUT_SECS)[0])
        except Exception as e:  # noqa: BLE001
            log.warning("memory index: the query's vector failed (%s); keyword search instead", type(e).__name__)
            return None
        import numpy as np

        span = (first_day, last_day)
        with self._lock:
            try:
                # Only the id, the channel and the vector of each round in range: the text is read for the best few.
                rows = self._db.execute("SELECT u.id, u.ch, v.v FROM units u LEFT JOIN vectors v ON v.hash = u.hash "
                                        "WHERE u.day BETWEEN ? AND ?", span).fetchall()
                bm: dict[int, float] = {}
                fq = fts_query(query)
                if fq:                                     # the BM25 candidates come from the range too
                    # A join, not "rowid IN (subquery)": FTS5 runs the IN form row by row (0.7 s over a year).
                    for rid, score in self._db.execute(
                            "SELECT f.rowid, bm25(fts) AS s FROM fts f JOIN units u ON u.id = f.rowid "
                            "WHERE fts MATCH ? AND u.day BETWEEN ? AND ? ORDER BY s LIMIT ?",
                            (fq, *span, FTS_CANDIDATES)):
                        bm[int(rid)] = -float(score)       # FTS5's bm25 is lower-is-better
            except sqlite3.Error as e:
                log.warning("memory index: search failed (%s)", type(e).__name__)
                return None
        if channels:
            rows = [r for r in rows if r[1] in channels]
        ids = [int(r[0]) for r in rows]
        cos = np.zeros(len(rows), dtype=np.float32)
        width = int(qv.shape[0]) * 4
        with_vec = [i for i, r in enumerate(rows) if r[2] and len(r[2]) == width]
        if with_vec:                                       # one matrix product over the range's vectors
            mat = np.frombuffer(b"".join(rows[i][2] for i in with_vec), dtype=np.float32).reshape(len(with_vec), -1)
            cos[with_vec] = mat @ qv
        best_bm = max((bm.get(i, 0.0) for i in ids), default=0.0)
        scored: list[tuple[float, int]] = []
        for k, rid in enumerate(ids):
            c = float(cos[k])
            b = bm.get(rid, 0.0)
            if b > 0 or c >= MIN_COS:                      # found: a word in common, or close enough in meaning
                norm = b / best_bm if best_bm > 0 and b > 0 else 0.0
                scored.append((BM25_WEIGHT * norm + COS_WEIGHT * c, rid))
        scored.sort(key=lambda x: x[0], reverse=True)
        best = [rid for _s, rid in scored[:limit]]
        hits: list[dict[str, Any]] = []
        if best:
            with self._lock:
                try:
                    full = {int(r[0]): r for r in self._db.execute(
                        "SELECT id, day, ts, ch, who, head, reply, before FROM units WHERE id IN "
                        f"({','.join('?' * len(best))})", best)}
                except sqlite3.Error as e:
                    log.warning("memory index: search failed (%s)", type(e).__name__)
                    return None
            hits = [self._hit(full[rid]) for rid in best if rid in full]
        log.info("memory index: %d rounds over %d days → %d hits in %d ms", len(rows), len(days), len(hits),
                 (time.monotonic() - t0) * 1000)
        return {"hits": hits, "found": bool(hits)}

    @staticmethod
    def _hit(r: tuple) -> dict[str, Any]:
        _id, day, ts, ch, who, head, reply, before = r
        try:
            at = datetime.fromisoformat(ts)
            hhmm = f"{at:%H:%M}"
        except ValueError:
            hhmm = ""
        text = head.split(": ", 1)[1] if ": " in head else head
        try:
            replies = json.loads(reply) or []
        except ValueError:
            replies = []
        return {"day": day, "time": hhmm, "ch": ch, "who": who, "text": _clip(text, HIT_CHARS),
                "before": _clip(before, NEIGHBOUR_CHARS), "after": _clip(" ".join(replies), REPLY_CHARS)}

    # -- forget --

    def _matching(self, pred: Callable[[str], bool]) -> list[int]:
        """Every round whose full stored text matches: the opening line and every reply line (not the FTS body,
        which is clipped at UNIT_CHARS), or the line before it."""
        out = []
        for rid, head, reply, before in self._db.execute("SELECT id, head, reply, before FROM units"):
            try:
                replies = json.loads(reply) or []
            except ValueError:
                replies = [reply]
            text = "\n".join([head, *(str(r) for r in (replies if isinstance(replies, list) else [replies]))])
            if pred(text) or pred(before):
                out.append(int(rid))
        return out

    def count(self, pred: Callable[[str], bool]) -> int:
        """How many rounds hold words `pred` matches (forget's preview). 0 when off."""
        if not self.ok:
            return 0
        with self._lock:
            try:
                return len(self._matching(pred))
            except sqlite3.Error:
                return 0

    def forget(self, pred: Callable[[str], bool]) -> int:
        """Delete every round `pred` matches (its full text or the line before it) and its FTS row; index again
        every indexed day whose file changed (the transcripts are already redacted), which also drops every vector
        no round uses; then rewrite the FTS segments and VACUUM, every time, so the words are not left in old
        segments or free pages. → rounds deleted. Raises IndexForgetError when a step still fails after
        COMPACT_WAITS: the caller reports the index as not done."""
        if not self.ok:
            return 0
        with self._lock:
            try:
                ids = self._matching(pred)
                indexed = [d for (d,) in self._db.execute("SELECT day FROM days")]
                if ids:
                    days = sorted({d for (d,) in self._db.execute(
                        f"SELECT DISTINCT day FROM units WHERE id IN ({','.join('?' * len(ids))})", ids)})
                    self._db.execute("BEGIN")
                    self._db.executemany("DELETE FROM fts WHERE rowid = ?", [(i,) for i in ids])
                    self._db.executemany("DELETE FROM units WHERE id = ?", [(i,) for i in ids])
                    self._db.executemany("DELETE FROM days WHERE day = ?", [(d,) for d in days])
                    self._gc()
                    self._db.execute("COMMIT")
            except sqlite3.Error as e:
                self._rollback()
                log.warning("memory index: forget failed (%s)", type(e).__name__)
                raise IndexForgetError(type(e).__name__) from e
        # Only the days already in the index: a forget cleans the file, it never adds days to one left switched off.
        self._retry(lambda: self._refresh(indexed))
        self._retry(self._compact)
        if self._worker is not None:                  # the daemon embeds the re-indexed rounds when it is quiet
            self._wake.set()
        log.info("memory index: forgot %d rounds", len(ids))
        return len(ids)

    def _compact(self) -> None:
        with self._lock:
            self._db.execute("INSERT INTO fts(fts) VALUES('optimize')")
            self._db.execute("VACUUM")

    def _retry(self, step: Callable[[], Any]) -> None:
        """Run one forget step; on an SQLite error (a busy file: the daemon and a CLI share it) wait and try
        again, COMPACT_WAITS in all. Still failing: IndexForgetError."""
        err: Optional[BaseException] = None
        for wait in COMPACT_WAITS:
            if wait:
                time.sleep(wait)
            try:
                step()
                return
            except sqlite3.Error as e:
                err = e
        log.warning("memory index: forget could not finish (%s)", type(err).__name__)
        raise IndexForgetError(type(err).__name__) from err

    def stats(self) -> dict[str, int]:
        if not self.ok:
            return {"units": 0, "vectors": 0, "days": 0}
        with self._lock:
            return {k: int(self._db.execute(f"SELECT COUNT(*) FROM {k}").fetchone()[0])
                    for k in ("units", "vectors", "days")}


def open_index(store: Path, transcripts: Any, embedder: Optional[Embedder] = None, *,
               embed: bool = True) -> Optional[MemoryIndex]:
    """The index for a memory folder, or None when the transcripts are off or the folder may not hold it (the
    transcripts' own guards: no git work tree, no synced folder, no symlink). `embed=False`: no embedder at all
    (a forget of a file left from when the switch was on sends nothing anywhere)."""
    if not getattr(transcripts, "enabled", False):
        return None
    from .transcripts import refusal

    path = index_path(store)
    why = refusal(path.parent)
    if why:
        log.warning("memory index: off — %s", why)
        return None
    if not embed:
        embedder = None
    elif embedder is None:
        embedder = openai_embedder()
    idx = MemoryIndex(path, transcripts, embedder)
    return idx if idx.ok else None
