"""Records: what eve knows about its owner, as files the owner can read and edit.

Memory is three stores (owner, 2026-09-23: "simplify the memory system — too many stores"): the
transcripts (every word, transcripts.py), mem0 (meaning search, mem0_memory.py) and these records, the
truth. The shape follows the memory Instinct (the iMessage assistant) was found to use — reverse-engineered
by Dhravya Shah, 2026-09-20 — because it has one property the owner cares about: **the brains read; they
never write.**

    <memory>/records/                its own git repository, sealed to this computer
        <id>.md                      one typed record per thing: a preference, a person, a project, a place.
                                     Frontmatter: id, type, aliases. Body: dated facts, [[id]] links.
        profile.md                   the one-pager every conversation starts with: life context, how much
                                     to ask before acting, how to talk, and an index of the records.
        starred.md                   what the owner said to remember, in their words, one dated line each.
        hidden.md                    what the owner said not to bring up again, one dated line each.
        days/<day>.md                the dream journal: what happened, what eve learned, what is still
                                     open, what it corrected.

Who writes, and nothing else does:

1. **"Remember that"** (voice, Telegram, the notes widget) → ``star`` appends one line to starred.md. The
   owner's voice is the human promoting a claim, so it counts on the next message: ``profile`` puts every
   star the page has not absorbed yet on top of it, verbatim.
2. **The nightly dream** (dream.py) → ``reconcile_day`` reads one whole day of transcript beside the
   current records, profile and stars, and returns — in one model call — the records that change (whole),
   the profile as it should read now, the ids the owner said to forget, and the day's journal. Where the
   day contradicts a record, the record gets a dated correction and the journal says so. ``consolidate``
   then merges the same thing filed under two ids and dates superseded facts. The model can shorten,
   merge and correct; owner edits survive because the current files are always its input.
3. **Forget** (owner-confirmed, memory.py) → ``forget_lines`` removes matching lines everywhere here, and
   ``squash_history`` makes git forget them too. A forget that lands while a dream's model call is out
   would be undone by that dream's write (the model answered from the words before the forget), so every
   forget moves a generation (``.forgets``) and a dream writes only when the generation it pinned before
   reading the day has not moved (``forgets_pinned``; proved in verification/Eve/Forget.lean).

**Don't mention this again** (the app's Memory card) → ``hide`` appends the owner's words to hidden.md. Nothing
is deleted: the profile and the searches leave out every line the words match (``hidden_match``, by the one
matching rule, transcripts.match), and ``unhide`` brings them back (docs/stackchan/memory.md).

Git is how nothing is lost otherwise: every dream is one commit, so a wrong edit is one revert away. The
repository root is the records folder itself — never the memory root, which holds the transcripts, and a
spoken word must never reach a git object. ``seal`` locks it to this computer.

Search is grep (aliases are the index; the profile lists them so a brain knows what it can look up), plus
mem0's meaning search beside it, each hit dated so a newer fact can win a tie.
"""

from __future__ import annotations

import contextlib
import fcntl
import json
import logging
import os
import re
import shutil
import subprocess
import tempfile
import threading
from dataclasses import dataclass, field
from datetime import datetime, timedelta
from pathlib import Path
from typing import Any, Callable, Iterator, Optional

from . import spend
from .recall import RecallConfig

log = logging.getLogger(__name__)

DEFAULT_MODEL = "gpt-6-luna"
DEFAULT_CONSOLIDATION_MODEL = "gpt-6-luna"
RECONCILE_TIMEOUT_SECS = 180.0             # one model call; the dream runs it on a worker thread
MAX_OUTPUT_TOKENS = 16000                  # records whole + profile + journal; 4000 truncated busy days
TYPES = ("preference", "person", "organization", "project", "place", "routine")
PROFILE_ID = "profile"
STARRED_FILE = "starred.md"
HIDDEN_FILE = "hidden.md"
MAX_HIDDEN_CHARS = 500                     # one hidden line: a fact, a sentence said, a few words
DAYS_DIR = "days"
MAX_PROFILE_CHARS = 6000                   # ~1.5k tokens: the one-pager stays one page
MAX_VOICE_PROFILE_CHARS = 3000             # the Live voice front has no tools, so no index, and less room
MAX_STARS = 40                             # what stars() returns by default
MAX_STARS_READ = 200                       # the stars a dream reads (the newest)
MAX_RECORD_CHARS = 4000
MAX_FACTS = 40
MAX_ALIASES = 12
MAX_HITS = 8
MAX_HIT_CHARS = 200
MAX_CONSOLIDATE_CHARS = 120_000            # above this one call cannot hold every record: skip, log once
DAY_START_HOUR = 4                         # a star at 01:00 belongs to the evening before, as a transcript does
FORGOTTEN = "(forgotten)"                  # what a forgotten journal title reads
INDEX_HEADING = "## Records you can search or read by id"
FRESH_STARS_HEADING = "## They asked you to remember (since the page below was written)"
GIT_TIMEOUT_SECS = 30
GC_TIMEOUT_SECS = 180
_ID = re.compile(r"^[a-z0-9][a-z0-9-]{0,63}$")
_DAY = re.compile(r"^\d{4}-\d{2}-\d{2}$")
_DAY_FILE = re.compile(r"^(\d{4}-\d{2}-\d{2})\.md$")
_LINK = re.compile(r"\[\[([a-z0-9-]+)\]\]")
_WORD = re.compile(r"[a-z0-9]+")
_FRONTMATTER = re.compile(r"\A---\n.*?\n---[ \t]*(?:\n|\Z)", re.S)
_STAR_LINE = re.compile(r"^\s*[-*]\s+(?:★\s*)?(.+?)\s*$")
_STAR_DATE = re.compile(r"^(.*?)\s*\((\d{4}-\d{2}-\d{2})\)$")
_UPDATED = re.compile(r"^updated:\s*(\d{4}-\d{2}-\d{2})\s*$", re.M)

# ---- two switches over what a fact line carries (memory plan items 2 and 3, 2026-10-08) -----------------------
#
# EVE_MEMORY_VALIDITY=1: a fact line carries said / valid_from / valid_to. The dream closes a superseded fact by
# setting valid_to instead of writing a prose correction, and rewrites a plan whose date has passed into the past
# tense. profile() leaves closed facts out; search labels them "(until YYYY-MM-DD)". A night may close at most
# MAX_CLOSES_PER_NIGHT facts (a valid_to newly set, whatever its date). Past that, the night is still written, so a
# busy day is never stuck and never dreamt out of order: the closures over the budget are held open and written to
# the day's journal as plain dated corrections instead.
#
# EVE_MEMORY_SOURCES=1: every fact the dream writes carries src, "<day> <HH:MM>" of the transcript line it came
# from (the line's own date and time, as the dream reads them, and a ref memory_read accepts). Search shows it, and a
# forget that redacts a transcript line also forgets every fact pointing at it (memory.Memory._follow_sources).
#
# Both off by default. The meta goes at the end of the line, in braces: "- Rent is $1,180 a month. {said: 2026-08-29;
# valid_from: 2026-09-01; src: 2026-08-29 18:05}". A line without it is a plain fact, read and written unchanged,
# so every record file written before the switches loads as it was.

VALIDITY_ENV = "EVE_MEMORY_VALIDITY"
SOURCES_ENV = "EVE_MEMORY_SOURCES"
# The guard on closures. A busy real day (a move: address, rent, commute, landlord) closes four or five facts; more
# than six in one night is more likely the model misreading the day than the owner's life changing that much, and a
# wrong closure hides a true fact from the profile. Reconcile and consolidate of one night share the budget.
MAX_CLOSES_PER_NIGHT = 6
FACT_KEYS = ("said", "valid_from", "valid_to", "src")
_META = re.compile(r"\s*\{\s*((?:said|valid_from|valid_to|src)\s*:[^{}]*)\}\s*$")
_SRC = re.compile(r"^\d{4}-\d{2}-\d{2} \d{2}:\d{2}$")


# Each record switch's value when its variable is not set: the memory eval's holdout verdict
# (tests/fixtures/memory_eval/VERDICT.md, 2026-10-08). tests/test_memory_verdict.py keeps the two equal.
DEFAULTS = {VALIDITY_ENV: False, SOURCES_ENV: False}


def _switch(name: str, environ: Any = None) -> bool:
    """On when set to 1/true/yes/on, off when set to anything else, and ``DEFAULTS`` when not set (or empty)."""
    env = os.environ if environ is None else environ
    raw = str(env.get(name) or "").strip().lower()
    if not raw:
        return DEFAULTS.get(name, False)
    return raw in ("1", "true", "yes", "on")


def validity_on(environ: Any = None) -> bool:
    return _switch(VALIDITY_ENV, environ)


def sources_on(environ: Any = None) -> bool:
    return _switch(SOURCES_ENV, environ)


@dataclass
class Fact:
    """One fact line, split: its text and its meta (each "" when the line does not carry it)."""
    text: str
    said: str = ""
    valid_from: str = ""
    valid_to: str = ""
    src: str = ""

    def closed(self, today: str) -> bool:
        """valid_to set and reached by `today` (YYYY-MM-DD). A fact with a valid_to still ahead is open."""
        return bool(self.valid_to) and (not today or self.valid_to <= today)


def parse_fact(line: str) -> Fact:
    """A fact line → Fact. A line with no trailing meta is all text: old files read as they were written."""
    m = _META.search(line)
    if m is None:
        return Fact(text=line)
    meta: dict[str, str] = {}
    for part in m.group(1).split(";"):
        key, sep, value = part.partition(":")
        key = key.strip().lower()
        if sep and key in FACT_KEYS:
            meta[key] = " ".join(value.split())
    return Fact(text=line[:m.start()].rstrip(), **meta)


def render_fact(fact: Fact) -> str:
    """Fact → its line. No meta, no braces: a plain fact renders back to exactly its text."""
    meta = "; ".join(f"{k}: {getattr(fact, k)}" for k in FACT_KEYS if getattr(fact, k))
    return f"{fact.text} {{{meta}}}" if meta else fact.text


def fact_display(line: str) -> str:
    """A fact line as a search hit shows it: a closed fact labelled "(until YYYY-MM-DD)" in front, the src after
    ("(source YYYY-MM-DD HH:MM)"), the said date kept; braces never shown."""
    f = parse_fact(line)
    if f.text == line:
        return line
    out = f"(until {f.valid_to}) {f.text}" if f.valid_to else f.text
    if f.said and f.said not in f.text:
        out += f" (said {f.said})"
    if f.src:
        out += f" (source {f.src})"
    return out


def _today() -> str:
    return (datetime.now() - timedelta(hours=DAY_START_HOUR)).strftime("%Y-%m-%d")


def _date_or_empty(value: Any) -> str:
    s = " ".join(str(value or "").split())
    return s if _DAY.match(s) else ""


# ---- records on disk --------------------------------------------------------------------------

@dataclass
class Record:
    id: str
    type: str
    aliases: list[str] = field(default_factory=list)
    facts: list[str] = field(default_factory=list)
    updated: str = ""

    @property
    def links(self) -> list[str]:
        seen: list[str] = []
        for fact in self.facts:
            for target in _LINK.findall(fact):
                if target not in seen and target != self.id:
                    seen.append(target)
        return seen


def parse_record(text: str) -> Optional[Record]:
    """A record file → Record, or None when it is not one. Tolerant: the owner edits these by hand."""
    if not text.startswith("---"):
        return None
    m = re.match(r"---\n(.*?)\n---[ \t]*(?:\n|$)(.*)", text, re.S)
    if m is None:
        return None
    head, body = m.group(1), m.group(2)
    meta: dict[str, str] = {}
    for line in head.splitlines():
        if ":" in line:
            key, value = line.split(":", 1)
            meta[key.strip().lower()] = value.strip()
    rid = meta.get("id", "")
    if not _ID.match(rid):
        return None
    rtype = meta.get("type", "").strip().lower() or "preference"
    raw_aliases = meta.get("aliases", "").strip().strip("[]")
    aliases = [a.strip().strip("\"'") for a in raw_aliases.split(",") if a.strip().strip("\"'")]
    facts = [m.group(1).strip() for m in (re.match(r"^\s*[-*]\s+(.+?)\s*$", ln) for ln in body.splitlines()) if m]
    return Record(id=rid, type=rtype, aliases=aliases, facts=facts, updated=meta.get("updated", ""))


def render_record(rec: Record) -> str:
    aliases = ", ".join(rec.aliases)
    lines = ["---", f"id: {rec.id}", f"type: {rec.type}", f"aliases: [{aliases}]"]
    if rec.updated:
        lines.append(f"updated: {rec.updated}")
    lines.append("---")
    lines.extend(f"- {fact}" for fact in rec.facts)
    return "\n".join(lines) + "\n"


def records_dir(cfg: RecallConfig) -> Path:
    """The records folder, and the root of its own git repository (never the memory root)."""
    return cfg.records_dir


def load_records(cfg: RecallConfig) -> dict[str, Record]:
    out: dict[str, Record] = {}
    folder = records_dir(cfg)
    if not folder.is_dir():
        return out
    for path in sorted(folder.glob("*.md")):
        if path.stem == PROFILE_ID or path.name in (STARRED_FILE, HIDDEN_FILE):
            continue
        try:
            rec = parse_record(path.read_text(encoding="utf-8"))
        except OSError:
            continue
        if rec is not None and rec.id == path.stem:
            out[rec.id] = rec
    return out


def _write(path: Path, text: str) -> None:
    """Whole-file write, atomic (a reader never sees half a record), private to this user."""
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, tmp = tempfile.mkstemp(dir=path.parent, prefix=f".{path.name}.", suffix=".tmp")
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as fh:
            fh.write(text)
        os.chmod(tmp, 0o600)
        os.replace(tmp, path)
    except BaseException:
        with contextlib.suppress(OSError):
            os.unlink(tmp)
        raise


def _make_dir(folder: Path) -> None:
    folder.mkdir(parents=True, exist_ok=True)
    with contextlib.suppress(OSError):
        os.chmod(folder, 0o700)


# One writer at a time: the dream (a worker thread), a star (a conversation), a forget (a tool call), and
# the CLI in another process. A thread lock inside the process, an flock on the folder across processes;
# the flock is taken once per thread (a second descriptor in the same process would wait on the first).
_WRITE_LOCK = threading.RLock()
_HELD = threading.local()
# starred.md alone has its own short lock: a star comes from a live conversation and must never wait
# behind a dream's commit or a forget's gc. forget_lines takes it too while it rewrites starred.md.
_STAR_LOCK = threading.Lock()
# hidden.md likewise: a hide comes from the app and never waits for a dream.
_HIDE_LOCK = threading.Lock()


@contextlib.contextmanager
def _locked(folder: Path) -> Iterator[None]:
    with _WRITE_LOCK:
        depth = getattr(_HELD, "depth", 0)
        fd = None
        if depth == 0:
            with contextlib.suppress(OSError):
                _make_dir(folder)
                fd = os.open(folder, os.O_RDONLY)
                fcntl.flock(fd, fcntl.LOCK_EX)
        _HELD.depth = depth + 1
        try:
            yield
        finally:
            _HELD.depth = depth
            if fd is not None:
                with contextlib.suppress(OSError):
                    fcntl.flock(fd, fcntl.LOCK_UN)
                os.close(fd)


# The forget generation. The dream reads the day and the records, calls a model for minutes with the lock
# released, then writes; a forget that ran in between would be undone by that write, and its commit would
# land after the forget squashed the history. So a forget moves the generation inside the same hold of the
# lock in which it scrubs the records, the dream pins it before it reads anything, and a write whose pin
# moved is dropped: the night is dreamt again, from what is left. On disk because the CLI forgets from
# another process; in memory too, so a failed write of the file cannot hide a forget from this process.
FORGETS_FILE = ".forgets"
_FORGETS = [0]                               # forgets begun in this process
_PIN = threading.local()                     # the generation this thread's dream pinned, or None


def _generation(folder: Path) -> tuple[int, str]:
    """Call with _locked(folder) held."""
    try:
        disk = (folder / FORGETS_FILE).read_text(encoding="utf-8").strip()
    except (OSError, UnicodeDecodeError):
        disk = ""
    return _FORGETS[0], disk


def _bump(folder: Path) -> None:
    """Move the generation. Call with _locked(folder) held, in the hold that scrubs the records."""
    with _WRITE_LOCK:
        _FORGETS[0] += 1
        if not folder.is_dir():
            return
        disk = _generation(folder)[1]
        try:
            _write(folder / FORGETS_FILE, f"{int(disk) + 1 if disk.isdigit() else 1}\n")
        except OSError as e:
            log.warning("records: could not note a forget on disk (%s)", type(e).__name__)


def _pinned(folder: Path) -> tuple[int, str]:
    """The generation a write is checked against: the dream's pin, or now. Call with _locked(folder) held."""
    pin = getattr(_PIN, "gen", None)
    return pin if pin is not None else _generation(folder)


@contextlib.contextmanager
def forgets_pinned(cfg: RecallConfig) -> Iterator[None]:
    """Pin the forget generation for this thread before reading what a model will rewrite (the day's
    transcript included). reconcile_day and consolidate inside it write nothing when a forget ran since."""
    folder = records_dir(cfg)
    outer = getattr(_PIN, "gen", None)
    if outer is None:
        with _locked(folder):
            _PIN.gen = _generation(folder)
        _NIGHT.closed = 0                        # one night, one closure budget (MAX_CLOSES_PER_NIGHT)
    try:
        yield
    finally:
        if outer is None:
            _PIN.gen = None
            _NIGHT.closed = 0


# ---- stars: the owner's "remember that" ---------------------------------------------------------

_STARRED_HEAD = "# Starred\n\nWhat the owner asked eve to remember, in their words, oldest first.\n\n"


def _norm(text: str) -> str:
    return " ".join(str(text or "").split()).strip(" .,;:!").casefold()


def _star_rows(cfg: RecallConfig) -> list[tuple[str, str]]:
    """Every star as (text, day), oldest first. An undated hand-typed line has day ""."""
    try:
        raw = (records_dir(cfg) / STARRED_FILE).read_text(encoding="utf-8")
    except OSError:
        return []
    rows: list[tuple[str, str]] = []
    for line in _FRONTMATTER.sub("", raw, count=1).splitlines():
        m = _STAR_LINE.match(line)
        if m is None:
            continue
        body = m.group(1)
        d = _STAR_DATE.match(body)
        text, day = (d.group(1).strip(), d.group(2)) if d else (body, "")
        if text:
            rows.append((text, day))
    return rows


def _star_line(text: str, day: str) -> str:
    return f"- {text} ({day})" if day else f"- {text}"


def star(cfg: RecallConfig, text: str, when: Optional[datetime] = None) -> Optional[str]:
    """Keep one claim for good, because the owner said so. → the line as stored, or None.

    A human promotes and an agent only proposes; the owner's voice IS the human, so "remember that" is a
    promotion (owner, 2026-09-11). Append-only here; saying it twice (any case, any trailing punctuation)
    returns the line already there. Dated by the memory's day, which starts at 04:00, so a star at 01:00
    is the evening's and tonight's dream of that evening reads it. One appended line; it never waits for
    a dream or a forget, so a conversation can call it."""
    claim = " ".join(str(text or "").split()).strip(" .,;:").strip()
    if claim.startswith("★"):
        claim = claim.lstrip("★ ").strip()
    if len(claim) < 4:
        return None
    claim = claim[0].upper() + claim[1:] if claim[0].islower() else claim
    day = ((when or datetime.now()) - timedelta(hours=DAY_START_HOUR)).strftime("%Y-%m-%d")
    folder = records_dir(cfg)
    try:
        with _STAR_LOCK:
            key = _norm(claim)
            for old_text, old_day in _star_rows(cfg):
                if _norm(old_text) == key:
                    return _star_line(old_text, old_day)
            _make_dir(folder)
            path = folder / STARRED_FILE
            line = _star_line(claim, day)
            fd = os.open(path, os.O_RDWR | os.O_CREAT | os.O_APPEND | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600)
            try:
                os.fchmod(fd, 0o600)
                size = os.fstat(fd).st_size
                data = line + "\n"
                if size == 0:
                    data = _STARRED_HEAD + data
                elif os.pread(fd, 1, size - 1) != b"\n":
                    data = "\n" + data
                os.write(fd, data.encode("utf-8"))
            finally:
                os.close(fd)
        log.info("records: starred one line")
        return line
    except OSError as e:
        log.warning("records: could not star it (%s)", type(e).__name__)
        return None


def stars(cfg: RecallConfig, limit: int = MAX_STARS) -> list[str]:
    """The starred claims, text only (no date), newest last."""
    rows = _star_rows(cfg)
    return [text for text, _ in rows[-limit:]] if limit > 0 else []


# ---- hidden: the owner's "don't mention this again" ----------------------------------------------
#
# ChatGPT's memory: "Don't mention this again reduces future references to the information. It does not delete the
# original source." Here: one line in hidden.md, and every line its words match is left out of what eve reads
# first (the profile, both fronts) and of what it finds (records, meanings, the words said). The records, the
# transcripts and mem0 are not touched; unhide is the line going.

_HIDDEN_HEAD = "# Hidden\n\nWhat the owner asked eve not to bring up again. Nothing was deleted.\n\n"


def _hidden_rows(cfg: RecallConfig) -> list[tuple[str, str]]:
    """Every hidden line as (words, day), oldest first."""
    try:
        raw = (records_dir(cfg) / HIDDEN_FILE).read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError):
        return []
    rows: list[tuple[str, str]] = []
    for line in _FRONTMATTER.sub("", raw, count=1).splitlines():
        m = _STAR_LINE.match(line)
        if m is None:
            continue
        d = _STAR_DATE.match(m.group(1))
        text, day = (d.group(1).strip(), d.group(2)) if d else (m.group(1), "")
        if text:
            rows.append((text, day))
    return rows


def hidden(cfg: RecallConfig) -> list[str]:
    """The hidden words, oldest first."""
    return [text for text, _ in _hidden_rows(cfg)]


def _matcher(query: str) -> Optional[Callable[[str], bool]]:
    """transcripts.match: the one matching rule for search, forget, and this."""
    from .transcripts import match

    return match(query)


def hidden_match(cfg: RecallConfig) -> Optional[Callable[[str], bool]]:
    """A predicate true for a line any hidden entry matches (transcripts.match: a "quoted phrase" as written,
    otherwise every word), or None when nothing is hidden."""
    preds = [p for p in (_matcher(t) for t in hidden(cfg)) if p is not None]
    if not preds:
        return None
    return lambda text: any(p(text) for p in preds)


def hide(cfg: RecallConfig, text: str, when: Optional[datetime] = None) -> Optional[str]:
    """Don't bring these words up again. → the line as stored (the one already there when the same words, any
    case, are hidden), or None when there is no word to match or the file could not be written. Deletes nothing."""
    words = " ".join(str(text or "").split()).strip(" .,;:")[:MAX_HIDDEN_CHARS].strip()
    if not words or _matcher(words) is None:
        return None
    day = ((when or datetime.now()) - timedelta(hours=DAY_START_HOUR)).strftime("%Y-%m-%d")
    folder = records_dir(cfg)
    try:
        with _HIDE_LOCK:
            key = _norm(words)
            for old_text, old_day in _hidden_rows(cfg):
                if _norm(old_text) == key:
                    return _star_line(old_text, old_day)
            _make_dir(folder)
            line = _star_line(words, day)
            flags = os.O_RDWR | os.O_CREAT | os.O_APPEND | os.O_NOFOLLOW | os.O_CLOEXEC
            fd = os.open(folder / HIDDEN_FILE, flags, 0o600)
            try:
                os.fchmod(fd, 0o600)
                size = os.fstat(fd).st_size
                data = line + "\n"
                if size == 0:
                    data = _HIDDEN_HEAD + data
                elif os.pread(fd, 1, size - 1) != b"\n":
                    data = "\n" + data
                os.write(fd, data.encode("utf-8"))
            finally:
                os.close(fd)
        log.info("records: hid one line")
        return line
    except OSError as e:
        log.warning("records: could not hide it (%s)", type(e).__name__)
        return None


def unhide(cfg: RecallConfig, text: str) -> bool:
    """Take a hidden line back (the same words, any case, as hide dedupes them). → whether one went."""
    key = _norm(" ".join(str(text or "").split())[:MAX_HIDDEN_CHARS])
    if not key:
        return False
    path = records_dir(cfg) / HIDDEN_FILE
    try:
        with _HIDE_LOCK:
            raw = path.read_text(encoding="utf-8")
            kept, removed = [], 0
            for line in raw.split("\n"):
                m = _STAR_LINE.match(line)
                if m is not None:
                    d = _STAR_DATE.match(m.group(1))
                    if _norm(d.group(1) if d else m.group(1)) == key:
                        removed += 1
                        continue
                kept.append(line)
            if not removed:
                return False
            _write(path, "\n".join(kept))
        log.info("records: unhid %d line(s)", removed)
        return True
    except (OSError, UnicodeDecodeError) as e:
        log.warning("records: could not unhide it (%s)", type(e).__name__)
        return False


def _drop_hidden(text: str, hide_pred: Optional[Callable[[str], bool]]) -> str:
    """`text` without the body lines (never a heading or a front-matter line) a hidden entry matches."""
    if hide_pred is None or not text:
        return text
    fm = _FRONTMATTER.match(text)
    head, body = (text[:fm.end()], text[fm.end():]) if fm else ("", text)
    kept = [ln for ln in body.split("\n") if ln.lstrip().startswith("#") or not ln.strip() or not hide_pred(ln)]
    return head + "\n".join(kept)


# ---- what the brains may do: search and read ---------------------------------------------------

def _tokens(text: str) -> list[str]:
    return _WORD.findall(text.lower())


def search(records: dict[str, Record], query: str, limit: int = MAX_HITS,
           hide: Optional[Callable[[str], bool]] = None) -> list[dict[str, Any]]:
    """Keyword search, no vectors. A hit is one fact line. Score: query words found in the record's id or
    aliases count three (that is what aliases are for), words in the line itself count one. A fact `hide`
    matches (hidden_match) is never a hit."""
    words = [w for w in _tokens(query) if len(w) > 1]
    if not words:
        return []
    hits: list[tuple[int, bool, str, Fact, str]] = []
    today = _today()
    for rec in records.values():
        name_words = set(_tokens(rec.id + " " + " ".join(rec.aliases)))
        name_score = 3 * sum(1 for w in words if w in name_words or any(w in n for n in name_words if len(w) > 3))
        for line in rec.facts:
            fact = parse_fact(line)
            if hide is not None and hide(fact.text):
                continue
            fact_words = set(_tokens(fact.text))
            score = name_score + sum(1 for w in words if w in fact_words)
            if score > 0:
                hits.append((score, fact.closed(today), rec.id, fact, line))
    hits.sort(key=lambda h: (-h[0], h[1], h[2]))             # on a tie, what is still true comes first
    out: list[dict[str, Any]] = []
    for _, _, rid, fact, line in hits[:limit]:
        hit = {"id": rid, "line": fact_display(line)[:MAX_HIT_CHARS]}
        if fact.src:
            hit["src"] = fact.src
        out.append(hit)
    return out


def get(records: dict[str, Record], rid: str) -> Optional[str]:
    rec = records.get(rid.strip().lower())
    return render_record(rec)[:MAX_RECORD_CHARS] if rec is not None else None


def render_index(records: dict[str, Record]) -> str:
    """The part of the profile that says what can be looked up."""
    if not records:
        return ""
    rows = [f"- {rec.id} ({rec.type}): {', '.join(rec.aliases[:6])}" if rec.aliases else f"- {rec.id} ({rec.type})"
            for rec in records.values()]
    return INDEX_HEADING + "\n" + "\n".join(rows)


def _clip_lines(text: str, cap: int) -> str:
    if len(text) <= cap:
        return text
    return text[:cap - 2].rsplit("\n", 1)[0] + "\n…"


def _fresh_block(cfg: RecallConfig, updated: str, hide: Optional[Callable[[str], bool]] = None) -> str:
    """Stars the page has not absorbed yet (dated after its `updated:`; an undated star counts as new), less the
    hidden ones."""
    fresh = [_star_line(t, d) for t, d in _star_rows(cfg)
             if (not d or not updated or d > updated) and not (hide is not None and hide(t))]
    return FRESH_STARS_HEADING + "\n" + "\n".join(fresh) if fresh else ""


def _profile_page(cfg: RecallConfig) -> str:
    try:
        return (records_dir(cfg) / f"{PROFILE_ID}.md").read_text(encoding="utf-8").strip()
    except OSError:
        return ""


def _open_only(page: str) -> str:
    """The page without its closed fact lines, and with the braces off its open ones. A page with no fact meta
    (every page written with the validity switch off) comes back as it is."""
    if "{" not in page:
        return page
    today = _today()
    out: list[str] = []
    for line in page.split("\n"):
        f = parse_fact(line)
        if f.text == line:
            out.append(line)
        elif not f.closed(today):
            out.append(f.text)
    return "\n".join(out)


def profile(cfg: RecallConfig) -> str:
    """The one-pager, bounded, for the start of a turn. "" when there is none yet.

    A star has to count on the next message, not after tonight's dream, so stars the page has not absorbed
    yet go on top, verbatim. With no page yet, the stars alone are the page. A line that carries fact meta
    (a fact copied from a record) is shown only while it is open, and without its braces. A line a hidden entry
    matches is left out (hide); the page on disk keeps it."""
    hide_pred = hidden_match(cfg)
    text = _drop_hidden(_open_only(_profile_page(cfg)), hide_pred)
    m = _UPDATED.search(text)
    block = _fresh_block(cfg, m.group(1) if m else "", hide_pred)
    if block:
        # above the page's first section, under its frontmatter, title and intro: a reader (the app's Memory card)
        # strips frontmatter only at the top and joins loose lines to the fact before them, so a block above the
        # frontmatter glued "id: profile updated: …" onto the last star
        at = text.find("\n## ")
        if not text:
            text = block
        elif at < 0:
            text = text + "\n\n" + block
        else:
            text = text[:at].rstrip("\n") + "\n\n" + block + "\n\n" + text[at + 1:]
    return _clip_lines(text, MAX_PROFILE_CHARS)


def _drop_section(text: str, heading: str) -> str:
    """`text` without the '## heading' section (up to the next '## ' heading or the end)."""
    out: list[str] = []
    skipping = False
    for line in text.splitlines():
        if line.startswith("## "):
            skipping = line.strip() == heading
        if not skipping:
            out.append(line)
    return "\n".join(out).strip()


def profile_for_voice(cfg: RecallConfig) -> str:
    """The profile for the Live voice front: no frontmatter, no record index (it has no tools to use the
    ids with, and the index is most of the page), at most MAX_VOICE_PROFILE_CHARS."""
    hide_pred = hidden_match(cfg)
    page = _drop_hidden(_open_only(_profile_page(cfg)), hide_pred)
    m = _UPDATED.search(page)
    block = _fresh_block(cfg, m.group(1) if m else "", hide_pred)
    page = _drop_section(_FRONTMATTER.sub("", page, count=1), INDEX_HEADING)
    text = block + ("\n\n" + page if page and block else page)
    return _clip_lines(text.strip(), MAX_VOICE_PROFILE_CHARS)


TALKING_HEADING = "## How they like to talk"


def talking_section(cfg: RecallConfig) -> str:
    """The profile page's "How they like to talk" section, its heading and lines, from the whole page (closed facts
    and hidden lines dropped, never clipped here); "" when the page has none. For the small-talk lane
    (small_talk.voice_from_profile clips it). Not read from profile_for_voice: that one is cut to
    MAX_VOICE_PROFILE_CHARS with fresh stars on top, and this section, the page's last, is the first to go."""
    page = _drop_hidden(_open_only(_profile_page(cfg)), hidden_match(cfg))
    out: list[str] = []
    inside = False
    for line in page.splitlines():
        if line.startswith("## "):
            inside = line.strip() == TALKING_HEADING
        if inside:
            out.append(line)
    return "\n".join(out).strip()


def _journal_title(path: Path) -> str:
    try:
        raw = path.read_text(encoding="utf-8")
    except OSError:
        return ""
    fm = _FRONTMATTER.match(raw)
    if fm:
        t = re.search(r"^title:\s*(.+?)\s*$", fm.group(0), re.M)
        if t:
            return t.group(1)
    h = re.search(r"^#\s+(.+?)\s*$", raw, re.M)
    return h.group(1) if h else ""


def latest_day(cfg: RecallConfig) -> Optional[tuple[str, str]]:
    """(day, title) of the newest dream journal, or None when there is none."""
    folder = records_dir(cfg) / DAYS_DIR
    try:
        days = sorted(m.group(1) for m in (_DAY_FILE.match(n) for n in os.listdir(folder)) if m)
    except OSError:
        return None
    if not days:
        return None
    return days[-1], _journal_title(folder / f"{days[-1]}.md")


def _recalled_line(hit: dict[str, Any]) -> str:
    day, channel = str(hit.get("day") or ""), str(hit.get("channel") or "")
    stamp = ", ".join(x for x in (day, channel) if x)
    return f"({stamp}) {hit.get('text', '')}" if stamp else str(hit.get("text", ""))


class RecordsReader:
    """Read-only, re-read from disk per call (the owner may have edited). `recall` is the mem0 index
    (mem0_memory.OwnerMemory) or None."""

    def __init__(self, cfg: RecallConfig, recall: Any = None) -> None:
        self.cfg = cfg
        self.recall = recall

    def profile(self) -> str:
        return profile(self.cfg)

    def search(self, query: str) -> dict[str, Any]:
        hide_pred = hidden_match(self.cfg)
        hits = search(load_records(self.cfg), query, hide=hide_pred)
        recalled: list[str] = []
        if self.recall is not None:
            try:
                recalled = [_recalled_line(h) for h in self.recall.search(query) if isinstance(h, dict)
                            and not (hide_pred is not None and hide_pred(str(h.get("text") or "")))]
            except Exception as e:  # noqa: BLE001 - the meaning search failing is just no meaning hits
                log.warning("records: meaning search failed (%s)", type(e).__name__)
        out: dict[str, Any] = {"ok": True, "hits": hits}
        if recalled:
            out["recalled"] = recalled   # "(day, channel) fact": dated, so the newer one wins a tie
        if not hits and not recalled:
            out["note"] = "nothing matched"
        return out

    def get(self, rid: str) -> dict[str, Any]:
        records = load_records(self.cfg)
        hide_pred = hidden_match(self.cfg)
        if hide_pred is not None:                 # a hidden fact is not read back either; the file keeps it
            for rec in records.values():
                rec.facts = [f for f in rec.facts if not hide_pred(parse_fact(f).text)]
        text = get(records, rid)
        return {"ok": True, "record": text} if text else {"ok": False, "reason": f"no record {rid!r}"}


# ---- git: history is how nothing is lost ---------------------------------------------------------

# A commit never asks for a signature or an identity: this repository belongs to eve, on this machine.
_COMMIT_CFG = ("-c", "user.name=eve", "-c", "user.email=eve@localhost", "-c", "commit.gpgsign=false")


def _git(cwd: Path, *args: str, timeout: float = GIT_TIMEOUT_SECS) -> subprocess.CompletedProcess:
    env = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}   # never another repo's GIT_DIR
    return subprocess.run(["git", "-C", str(cwd), *args], capture_output=True, text=True, timeout=timeout,
                          env=env)


def _own_repo(repo: Path) -> bool:
    """Whether `repo` is the root of its own git repository (not a folder inside some other one)."""
    top = _git(repo, "rev-parse", "--show-toplevel")
    return top.returncode == 0 and Path(top.stdout.strip()).resolve() == repo.resolve()


def ensure_repo(repo: Path) -> bool:
    """`repo` as a sealed git repository of its own. False (with one log line) when git is not there, or
    when `repo` sits inside another repository: forcing an add there would put the owner's memory in it."""
    if shutil.which("git") is None:
        log.warning("records: git is not installed — records will be kept, but without history")
        return False
    try:
        _make_dir(repo)
        top = _git(repo, "rev-parse", "--show-toplevel")
        if top.returncode == 0:
            if Path(top.stdout.strip()).resolve() == repo.resolve():
                seal(repo)                    # an existing repository gets the same lock, every start
                return True
            log.warning("records: %s is inside another git repository — kept without history", repo)
            return False
        r = _git(repo, "init", "-q")
        if r.returncode != 0:
            log.warning("records: git init failed (exit %d)", r.returncode)
            return False
        _git(repo, "config", "user.name", "eve")
        _git(repo, "config", "user.email", "eve@localhost")
        seal(repo)
        return True
    except (OSError, subprocess.SubprocessError) as e:
        log.warning("records: git unavailable (%s)", type(e).__name__)
        return False


# Every URL shape git can push to, rewritten to one it cannot. A second lock beside the pre-push hook,
# because `git push --no-verify` skips hooks but never skips pushInsteadOf.
_PUSH_PREFIXES = ("https://", "http://", "ssh://", "git://", "git@", "file://", "/", "~", ".")
_NO_PUSH_URL = "local-only-never-pushed:"
_PRE_PUSH = """#!/bin/sh
# eve's memory lives on this computer only (records.py seal). Pushing it anywhere is refused.
echo "eve memory is local-only: push refused" >&2
exit 1
"""


def seal(repo: Path) -> None:
    """Lock the repository to this computer: no remote, no push, ever.

    The owner's memory is private (owner, 2026-09-23: "nothing should leak out ever"). The history exists
    so a bad dream is one revert away, not so it can go anywhere. So on every start any remote is removed,
    every push URL is rewritten to one that cannot resolve, and a pre-push hook refuses — three independent
    locks, so none of them failing alone lets it out."""
    for name in _git(repo, "remote").stdout.split():
        _git(repo, "remote", "remove", name)
        log.warning("records: removed a git remote from the memory store — it stays on this computer")
    for prefix in _PUSH_PREFIXES:
        _git(repo, "config", "--replace-all", f"url.{_NO_PUSH_URL}.pushInsteadOf", prefix, f"^{re.escape(prefix)}$")
    hooks = repo / ".git" / "hooks"
    hooks.mkdir(parents=True, exist_ok=True)
    (hooks / "pre-push").write_text(_PRE_PUSH, encoding="utf-8")
    (hooks / "pre-push").chmod(0o755)
    _git(repo, "config", "core.hooksPath", str(hooks))   # a global hooksPath must not bypass it


def commit(repo: Path, message: str) -> Optional[str]:
    """Commit everything under `repo`. The short hash, or None when there was nothing or git failed.

    Forced (an ignore file must not quietly cost the history), and so only in the folder's own repository."""
    try:
        if not _own_repo(repo):
            return None
        _git(repo, "add", "-A", "--force", ".")
        if _git(repo, "diff", "--cached", "--quiet").returncode == 0:
            return None
        r = _git(repo, *_COMMIT_CFG, "commit", "-q", "--no-verify", "-m", message)
        if r.returncode != 0:
            log.warning("records: commit failed (exit %d)", r.returncode)
            return None
        return _git(repo, "rev-parse", "--short", "HEAD").stdout.strip() or None
    except (OSError, subprocess.SubprocessError) as e:
        log.warning("records: commit failed (%s)", type(e).__name__)
        return None


def squash_history(repo: Path) -> bool:
    """Make git forget: the current tree becomes the only commit, and every older object is pruned.

    A forget has to reach history too, or the forgotten line is one `git log -p` away. Everything under
    the tree is committed first, the branch is pointed at one parentless commit of it, every other ref and
    every reflog entry goes, and gc prunes what nothing reaches any more. False when `repo` is not the root
    of its own repository (never someone else's) or git failed."""
    try:
        if shutil.which("git") is None or not _own_repo(repo):
            return False
        with _locked(repo):
            _git(repo, "add", "-A", "--force", ".")
            if _git(repo, "rev-parse", "--verify", "-q", "HEAD").returncode != 0:
                if _git(repo, "diff", "--cached", "--quiet").returncode == 0:
                    return True                               # no history at all: nothing to forget
                if _git(repo, *_COMMIT_CFG, "commit", "-q", "--no-verify", "-m", "memory").returncode != 0:
                    return False
            tree = _git(repo, "write-tree")
            if tree.returncode != 0:
                return False
            new = _git(repo, *_COMMIT_CFG, "commit-tree", tree.stdout.strip(), "-m",
                       "memory (history squashed by a forget)")
            if new.returncode != 0:
                return False
            branch = _git(repo, "symbolic-ref", "-q", "HEAD").stdout.strip() or "refs/heads/main"
            if _git(repo, "update-ref", branch, new.stdout.strip()).returncode != 0:
                return False
            if _git(repo, "symbolic-ref", "HEAD", branch).returncode != 0:
                return False
            _git(repo, "reset", "-q")                          # the index matches the new commit
            for ref in _git(repo, "for-each-ref", "--format=%(refname)").stdout.split():
                if ref != branch:
                    _git(repo, "update-ref", "-d", ref)
            for leftover in ("ORIG_HEAD", "FETCH_HEAD", "MERGE_HEAD"):
                with contextlib.suppress(OSError):
                    (repo / ".git" / leftover).unlink()
            _git(repo, "reflog", "expire", "--expire=now", "--expire-unreachable=now", "--all")
            gc = _git(repo, "-c", "gc.reflogExpire=now", "-c", "gc.reflogExpireUnreachable=now",
                      "gc", "-q", "--prune=now", timeout=GC_TIMEOUT_SECS)
            return gc.returncode == 0
    except (OSError, subprocess.SubprocessError) as e:
        log.warning("records: could not squash the history (%s)", type(e).__name__)
        return False


def _as_found(repo: Path, git: bool, what: str) -> None:
    """Whatever is on disk now — the owner's hand edits and new stars included — goes into history first,
    as its own commit, so the dream's commit is exactly what the model changed and reverts cleanly."""
    if git:
        commit(repo, f"as found before {what}")


# ---- the dream: the only automatic writer ----------------------------------------------------------

# The reconcile prompt, in pieces: the plain prompt (both switches off) is the pieces joined, word for word what it
# was; a switch swaps in its own piece. Each combination is one fixed string, so the prompt cache still holds.
_R_HEAD = """You are eve, your owner's personal sidekick, keeping the records of what you know about your owner.
Tonight you read one whole day of what was said between you, on voice and by text, beside the records as
they are now, the profile page, and what the owner asked you to remember. Return the records that should
change, whole; the profile as it should read now; the ids of records the owner asked you to forget; and your
journal of the day.

Records are facts about durable things: a preference, a person, an organization, a project, a place, a
routine. One record per thing, id in lowercase-with-hyphens, with aliases: every word the owner might use
for it. """
_R_FACT_PLAIN = 'Each fact is one line, stated as fact, with its date: "Loves pasta (said 2026-09-15)." '
_R_FACT_META = "Each fact is one line of text, stated as fact, with its fields beside it.{fields} "
_R_FIELD_DATES = """ said: the date of
the line that said it. valid_from: the date it became true (the said date when the line gives no other).
valid_to: the date it stopped being true, empty while it is still true."""
_R_FIELD_SRC = """ src: the date and time the
transcript line it came from starts with, YYYY-MM-DD HH:MM, so the line can be found again; a fact from
several lines takes the line that says it most plainly, and a fact you keep from before keeps its src."""
_R_FACT_TEXT_DATE = """ The text still
ends with its date, as in "Loves pasta (said 2026-09-15)."."""
_R_REST = """Link related
records with [[id]] inside a fact. Keep records short: merge examples into traits, drop incidental detail,
move what belongs to a project into that project's record. What happened once belongs in the journal, not
in a record. Only what the owner said is a fact about them; what eve said or did is not, and neither is a
request to operate the robot or its tools. Return only records that change or are new.

"""
_R_CORRECTIONS_PLAIN = """Corrections: when the day contradicts a record, never delete the old fact silently. Replace it with the
correction and its date, "Now prefers the 9 am slot (changed 2026-09-20; was 8 am).", and add one line
saying what changed to the journal's corrections. Anything the owner said to forget is removed: list the
record's id under forget, or return the record without that fact. That is the only removal.

"""
_R_CORRECTIONS_VALIDITY = """Corrections: when the day contradicts a fact that is still true, never delete it and never reword it.
Close it: return it as it was, with valid_to set to the date of the line that changed it, and add the new
fact beside it with valid_from that date. Add one line saying what changed to the journal's corrections. A
fact with a valid_to is history: keep it as it is. Anything the owner said to forget is removed: list the
record's id under forget, or return the record without that fact. That is the only removal.

Plans that have passed: a fact about something the owner meant to do or attend on a date before this day (an
appointment, a trip, a deadline) is no longer a plan. Return its record with that fact rewritten in the past
tense, as what happened ("Had a dentist cleaning on 2026-09-16."), or as what did not happen when the day
says so. Its dates stay.

"""
_R_PROFILE = """The profile is the page you read before every conversation. Three short sections, plain prose or a few
bullets each: "Life context" (their name first, when you know it, then who they are, what is going on, the
people and projects that matter now), "Acting on their behalf" (what they want done without asking and what
needs a yes first), "How they like to talk" (tone, length, what annoys them). Dates where they matter.
Nothing that is not in the records, the stars or the day."""
_R_PROFILE_VALIDITY = """ Only what is true now: no fact with a valid_to, and a
plan whose date has passed is written in the past tense or left out."""
_R_TAIL = """

What the owner asked you to remember is in their words, each with its date. Those are the most reliable
facts you have: fold every real fact about them into the records and the profile, and keep it there. A line
that is not a fact about them — a stray question, a half sentence the microphone caught — you leave out.

Dates: every transcript line starts with the real date and time it was said. A transcript day runs from
04:00 to 04:00, so its last lines can carry the next calendar date; they belong in this journal, under
their own date. Date a fact by the date of the line that said it, and read "tomorrow", "Friday" or "tonight"
from that line's date too.

The journal: a title of a few words for the day; what happened (one line per conversation or event,
starting with the real date and time as the transcript gives it, YYYY-MM-DD HH:MM); what you learned about
the owner; what is still open (a question left unanswered, a promise eve made); the plans; and the
corrections. A plan is anything the owner means to do or attend (an appointment, a trip, a deadline, a
project milestone): what it is, when it happens (YYYY-MM-DD, with HH:MM when given; empty when the day gave
no date), and the date it was said. The day it happens and the day it was said are different dates; keep
both. Short lines. An empty list when there is nothing."""


def _fact_piece(validity: bool, sources: bool) -> str:
    if not (validity or sources):
        return _R_FACT_PLAIN
    fields = (_R_FIELD_DATES if validity else "") + (_R_FIELD_SRC if sources else "")
    if not validity:
        fields += _R_FACT_TEXT_DATE
    return _R_FACT_META.format(fields=fields)


def reconcile_prompt(validity: bool = False, sources: bool = False) -> str:
    """The reconcile instructions for this combination of switches. Both off: RECONCILE_PROMPT itself."""
    return (_R_HEAD + _fact_piece(validity, sources) + _R_REST
            + (_R_CORRECTIONS_VALIDITY if validity else _R_CORRECTIONS_PLAIN)
            + _R_PROFILE + (_R_PROFILE_VALIDITY if validity else "") + _R_TAIL)


RECONCILE_PROMPT = reconcile_prompt()

_C_HEAD = """You are eve, your owner's personal sidekick, tidying the records of what you know about your owner.
You are shown every record and the profile page. Return only what should change:

- The same thing filed under two ids (a person, a place or a project under two names): keep one id, return
  it whole with the aliases of both and every fact of both, and list the other id under merged with the id
  it went into. Never lose a fact in a merge.
"""
_C_CONTRADICT_PLAIN = """- Two facts that contradict each other: keep the newest dated fact as it is, and rewrite the older one as
  "(until YYYY-MM-DD) <the older fact>", dated by when the newer one was said.
"""
_C_CONTRADICT_VALIDITY = """- Two facts with no valid_to that contradict each other: keep the newest as it is, and close the older one:
  set its valid_to to the date the newer one was said, and change nothing else in it.
"""
_C_TAIL = """- A fact repeated in one record: keep one copy.

Records you do not change are not returned. Change nothing else: the wording is the owner's as much as
yours, and they may have edited it by hand."""
_C_KEEP_FIELDS = """ Every fact you return keeps its fields as they are, except a valid_to
this list tells you to set."""


def consolidate_prompt(validity: bool = False, sources: bool = False) -> str:
    """The consolidate instructions for this combination of switches. Both off: CONSOLIDATE_PROMPT itself."""
    return (_C_HEAD + (_C_CONTRADICT_VALIDITY if validity else _C_CONTRADICT_PLAIN) + _C_TAIL
            + (_C_KEEP_FIELDS if validity or sources else ""))


CONSOLIDATE_PROMPT = consolidate_prompt()

_LIST = {"type": "array", "items": {"type": "string"}}
_RECORD = {"type": "object", "additionalProperties": False,
           "required": ["id", "type", "aliases", "facts"],
           "properties": {"id": {"type": "string"}, "type": {"type": "string", "enum": list(TYPES)},
                          "aliases": _LIST, "facts": _LIST}}
_FACT_FIELDS = {
    "text": {"type": "string"},
    "said": {"type": "string", "description": "the date of the line that said it: YYYY-MM-DD"},
    "valid_from": {"type": "string", "description": "the date it became true: YYYY-MM-DD"},
    "valid_to": {"type": "string", "description": "the date it stopped being true: YYYY-MM-DD, or empty"},
    "src": {"type": "string", "description": "the date and time its transcript line starts with: YYYY-MM-DD HH:MM"},
}
_PLANS = {"type": "array", "items": {
    "type": "object", "additionalProperties": False, "required": ["what", "when", "said"],
    "properties": {"what": {"type": "string"},
                   "when": {"type": "string", "description": "when it happens: YYYY-MM-DD [HH:MM], or empty"},
                   "said": {"type": "string", "description": "the date of the line that said it: YYYY-MM-DD"}}}}


def _record_schema(validity: bool, sources: bool) -> dict[str, Any]:
    if not (validity or sources):
        return _RECORD
    keys = ["text"] + (["said", "valid_from", "valid_to"] if validity else []) + (["src"] if sources else [])
    fact = {"type": "object", "additionalProperties": False, "required": keys,
            "properties": {k: _FACT_FIELDS[k] for k in keys}}
    return {**_RECORD, "properties": {**_RECORD["properties"], "facts": {"type": "array", "items": fact}}}


def reconcile_schema(validity: bool = False, sources: bool = False) -> dict[str, Any]:
    """Both switches off: RECONCILE_SCHEMA itself. On, facts are objects with the switched-on fields."""
    if not (validity or sources) and "RECONCILE_SCHEMA" in globals():
        return RECONCILE_SCHEMA
    return {
        "type": "object", "additionalProperties": False,
        "required": ["records", "forget", "profile", "journal"],
        "properties": {
            "records": {"type": "array", "items": _record_schema(validity, sources)},
            "forget": {"type": "array", "items": {"type": "string"},
                       "description": "ids of records the owner asked to forget entirely"},
            "profile": {"type": "object", "additionalProperties": False,
                        "required": ["life_context", "acting", "talking"],
                        "properties": {"life_context": _LIST, "acting": _LIST, "talking": _LIST}},
            "journal": {"type": "object", "additionalProperties": False,
                        "required": ["title", "happened", "learned", "open", "plans", "corrections"],
                        "properties": {"title": {"type": "string"}, "happened": _LIST, "learned": _LIST,
                                       "open": _LIST, "plans": _PLANS, "corrections": _LIST}},
        },
    }


def consolidate_schema(validity: bool = False, sources: bool = False) -> dict[str, Any]:
    """Both switches off: CONSOLIDATE_SCHEMA itself."""
    if not (validity or sources) and "CONSOLIDATE_SCHEMA" in globals():
        return CONSOLIDATE_SCHEMA
    return {
        "type": "object", "additionalProperties": False,
        "required": ["records", "merged"],
        "properties": {
            "records": {"type": "array", "items": _record_schema(validity, sources)},
            "merged": {"type": "array", "items": {
                "type": "object", "additionalProperties": False, "required": ["id", "into"],
                "properties": {"id": {"type": "string"}, "into": {"type": "string"}}}},
        },
    }


RECONCILE_SCHEMA = reconcile_schema()
CONSOLIDATE_SCHEMA = consolidate_schema()


@dataclass(frozen=True)
class DayChange:
    changed: int          # existing records rewritten or forgotten
    created: int          # records new tonight
    corrections: int      # dated corrections the journal lists
    journal: Path         # days/<day>.md
    closed: int = 0       # facts closed by a valid_to tonight (the validity switch)


def render_profile(prof: dict[str, Any], records: dict[str, Record], day: str) -> str:
    def section(title: str, key: str) -> str:
        lines = [str(x).strip() for x in (prof.get(key) or []) if str(x).strip()]
        return f"## {title}\n" + ("\n".join(f"- {ln}" for ln in lines) if lines else "- (nothing yet)")

    parts = [f"---\nid: {PROFILE_ID}\nupdated: {day}\n---",
             "# What eve knows about its owner",
             f"Dreamt from conversations up to {day}. What was said since is in today's transcript.",
             section("Life context", "life_context"),
             section("Acting on their behalf", "acting"),
             section("How they like to talk", "talking")]
    index = render_index(records)
    if index:
        parts.append(index)
    return "\n\n".join(parts) + "\n"


def _reindex_profile(cfg: RecallConfig) -> None:
    """Rewrite only the profile's record index, from the records as they are now."""
    path = records_dir(cfg) / f"{PROFILE_ID}.md"
    page = _profile_page(cfg)
    if not page:
        return
    body = _drop_section(page, INDEX_HEADING)
    index = render_index(load_records(cfg))
    _write(path, body + ("\n\n" + index if index else "") + "\n")


def render_journal(journal: dict[str, Any], day: str) -> str:
    def items(key: str) -> list[str]:
        return [" ".join(str(x).split()) for x in (journal.get(key) or []) if str(x).strip()]

    title = " ".join(str(journal.get("title") or "").split()) or day
    parts = [f"---\nday: {day}\ntitle: {title}\n---", f"# {title}"]
    for heading, key in (("What happened", "happened"), ("What eve learned", "learned"),
                         ("Still open", "open"), ("Plans", "plans"), ("Corrections", "corrections")):
        rows = _plan_rows(journal.get(key)) if key == "plans" else items(key)
        parts.append(f"## {heading}\n" + ("\n".join(f"- {r}" for r in rows) if rows else "- (nothing)"))
    return "\n\n".join(parts) + "\n"


def _plan_rows(plans: Any) -> list[str]:
    """One line per plan: when it happens apart from when it was said, e.g. "Dentist (when 2026-09-30 10:00;
    said 2026-09-23)". Malformed entries are skipped."""
    def flat(value: Any) -> str:
        return " ".join(str(value or "").split())

    rows = []
    for plan in plans if isinstance(plans, list) else []:
        if not isinstance(plan, dict) or not flat(plan.get("what")):
            continue
        when, said = flat(plan.get("when")), flat(plan.get("said"))
        rows.append(f"{flat(plan.get('what'))} (when{' ' + when if when else ': no date yet'}"
                    + (f"; said {said})" if said else ")"))
    return rows


def _fact_line(raw: Any, srcs: Optional[set[str]]) -> str:
    """One fact from a model reply → its line. A string is taken as a line (a plain fact, or one with meta); an
    object ({text, said, valid_from, valid_to, src}, the switched-on fields) is rendered. A date that is not
    YYYY-MM-DD is dropped, and so is a src that is not one of `srcs` (the lines the model was shown, and the
    srcs the records already carry): a src must point at a real line. "" when there is no text."""
    if isinstance(raw, dict):
        text = parse_fact(" ".join(str(raw.get("text") or "").split())).text
        fact = Fact(text=text, said=_date_or_empty(raw.get("said")), valid_from=_date_or_empty(raw.get("valid_from")),
                    valid_to=_date_or_empty(raw.get("valid_to")), src=" ".join(str(raw.get("src") or "").split()))
    else:
        fact = parse_fact(" ".join(str(raw or "").split()))
    if fact.src and (not _SRC.match(fact.src) or (srcs is not None and fact.src not in srcs)):
        fact.src = ""
    return render_fact(fact) if fact.text else ""


def _cap_facts(facts: list[str]) -> list[str]:
    """At most MAX_FACTS. Over it, the closed facts go first (the oldest valid_to first), then the newest open
    ones are cut, as before the switches."""
    if len(facts) <= MAX_FACTS:
        return facts
    parsed = [parse_fact(f) for f in facts]
    closed = sorted((i for i, f in enumerate(parsed) if f.valid_to), key=lambda i: parsed[i].valid_to)
    drop = set(closed[:len(facts) - MAX_FACTS])
    return [f for i, f in enumerate(facts) if i not in drop][:MAX_FACTS]


def _record_from(raw: Any, day: str, srcs: Optional[set[str]] = None) -> Optional[Record]:
    if not isinstance(raw, dict):
        return None
    rid = str(raw.get("id") or "").strip().lower()
    if not _ID.match(rid) or rid == PROFILE_ID or rid in (Path(STARRED_FILE).stem, Path(HIDDEN_FILE).stem):
        return None
    facts = [line for line in (_fact_line(f, srcs) for f in (raw.get("facts") or [])) if line]
    if not facts:
        return None
    rtype = str(raw.get("type") or "")
    return Record(id=rid, type=rtype if rtype in TYPES else "preference",
                  aliases=[str(a).strip() for a in (raw.get("aliases") or []) if str(a).strip()][:MAX_ALIASES],
                  facts=_cap_facts(facts), updated=day)


def _srcs_of(records: dict[str, Record]) -> set[str]:
    return {f.src for rec in records.values() for f in map(parse_fact, rec.facts) if f.src}


def day_srcs(day_text: str) -> set[str]:
    """Every "YYYY-MM-DD HH:MM" a line of the dream's day text starts with: the srcs a fact may point at."""
    return {line[:16] for line in (day_text or "").splitlines() if _SRC.match(line[:16])}


def _new_closures(before: dict[str, Record], after: list[Record]) -> list[tuple[Record, int]]:
    """(record, fact index) of every fact in `after` with a valid_to it did not have in `before`, in order. Any
    date counts: a valid_to set ahead still hides the fact once that day comes."""
    out: list[tuple[Record, int]] = []
    for rec in after:
        old = before.get(rec.id)
        was = {_norm(f.text) for f in map(parse_fact, old.facts) if f.valid_to} if old else set()
        out += [(rec, i) for i, f in enumerate(map(parse_fact, rec.facts)) if f.valid_to and _norm(f.text) not in was]
    return out


def _closures(before: dict[str, Record], after: list[Record], today: str = "") -> int:
    """How many facts `after` closes that `before` had open (see _new_closures)."""
    return len(_new_closures(before, after))


def _hold_closures(before: dict[str, Record], after: list[Record], left: int) -> list[str]:
    """Keep the first `left` new closures in `after`; reopen the rest (valid_to cleared, in place). → one plain
    dated correction per reopened fact, for the night's journal, so what the model saw is still written down."""
    held: list[str] = []
    for rec, i in _new_closures(before, after)[max(0, left):]:
        f = parse_fact(rec.facts[i])
        held.append(f"No longer true from {f.valid_to}: {f.text}")
        f.valid_to = ""
        rec.facts[i] = render_fact(f)
    return held


def _raw(rec: Record) -> dict[str, Any]:
    return {"id": rec.id, "type": rec.type, "aliases": list(rec.aliases), "facts": list(rec.facts)}


# Closures this night, per thread: the dream's night is one forgets_pinned hold, and reconcile and consolidate of
# that night share MAX_CLOSES_PER_NIGHT. Outside a night each call has the whole budget.
_NIGHT = threading.local()


def _closes_left() -> int:
    return MAX_CLOSES_PER_NIGHT - int(getattr(_NIGHT, "closed", 0) or 0)


def _spend_closes(n: int) -> None:
    if getattr(_PIN, "gen", None) is not None:
        _NIGHT.closed = int(getattr(_NIGHT, "closed", 0) or 0) + n


def _same(a: Optional[Record], b: Record) -> bool:
    return a is not None and (a.type, a.aliases, a.facts) == (b.type, b.aliases, b.facts)


def apply_day(cfg: RecallConfig, result: dict[str, Any], day: str, srcs: Optional[set[str]] = None) -> DayChange:
    """Write one dream: changed records, forgotten records, the profile, the journal. Malformed entries
    are skipped, never fatal; nothing is written outside the records folder. `srcs`: the srcs a fact may carry
    (None: any well-formed one)."""
    folder = records_dir(cfg)
    _make_dir(folder)
    before = load_records(cfg)
    changed = created = 0
    written: list[Record] = []
    for raw in result.get("records") or []:
        rec = _record_from(raw, day, srcs)
        if rec is not None:
            written.append(rec)
        if rec is None or _same(before.get(rec.id), rec):
            continue
        _write(folder / f"{rec.id}.md", render_record(rec))
        if rec.id in before:
            changed += 1
        else:
            created += 1
    for rid in result.get("forget") or []:
        rid = str(rid).strip().lower()
        path = folder / f"{rid}.md"
        if _ID.match(rid) and rid != PROFILE_ID and rid in before and path.exists():
            path.unlink()                     # gone from the working tree; git history keeps it until a forget
            changed += 1
    prof = result.get("profile") if isinstance(result.get("profile"), dict) else {}
    _write(folder / f"{PROFILE_ID}.md", render_profile(prof, load_records(cfg), day))
    journal = result.get("journal") if isinstance(result.get("journal"), dict) else {}
    journal_path = folder / DAYS_DIR / f"{day}.md"
    _write(journal_path, render_journal(journal, day))
    corrections = sum(1 for c in journal.get("corrections") or [] if str(c).strip())
    return DayChange(changed=changed, created=created, corrections=corrections, journal=journal_path,
                     closed=_closures(before, written, day))


def _call(client: Any, method: str, body: str) -> Optional[dict[str, Any]]:
    try:
        result = json.loads(getattr(client, method)(body))
        if not isinstance(result, dict):
            raise ValueError("not an object")
        return result
    except Exception as e:  # noqa: BLE001 - a failed call is a night without a dream, never a crash
        log.warning("records: the %s call failed (%s)", method, type(e).__name__)
        return None


def _render_all(records: dict[str, Record]) -> str:
    return "\n\n".join(render_record(r) for r in records.values())


def reconcile_day(cfg: RecallConfig, client: Any, day: str, day_text: str) -> Optional[DayChange]:
    """One day of transcript into the records, the profile and the day's journal, in ONE model call.

    `client` has ``reconcile(body) -> JSON text`` (OpenAIReconcileClient, or a fake). Blocking: the dream
    runs it on a worker thread. → the change, or None when the day is empty, the day is not a date or the
    call failed — and then nothing is written. One commit "dream: <day>" (after an as-found commit of
    whatever the owner changed by hand)."""
    if not _DAY.match(day or "") or not (day_text or "").strip():
        return None
    folder = records_dir(cfg)
    with _locked(folder):
        git = ensure_repo(folder)
        _as_found(folder, git, f"dream: {day}")
        current = load_records(cfg)
        page = _profile_page(cfg)
        star_rows = _star_rows(cfg)[-MAX_STARS_READ:]
        since = _pinned(folder)
    srcs = (day_srcs(day_text) | _srcs_of(current)) if sources_on() else None
    body = "## Records now\n\n" + (_render_all(current) or "(none yet)")
    body += "\n\n## The profile page now\n\n" + (page or "(none yet)")
    if star_rows:
        body += "\n\n## What the owner said to remember for good\n\n" + \
                "\n".join(_star_line(t, d) for t, d in star_rows)
    after = (datetime.strptime(day, "%Y-%m-%d") + timedelta(days=1)).strftime("%Y-%m-%d")
    body += (f"\n\n## Everything said in the transcript day {day}\n\nIt runs from {DAY_START_HOUR:02d}:00 on {day} "
             f"to {DAY_START_HOUR:02d}:00 on {after}. Each line starts with the real date and time it was said."
             f"\n\n{day_text}")
    result = _call(client, "reconcile", body)
    if result is None:
        return None
    try:
        with _locked(folder):
            if _generation(folder) != since:
                log.info("records: a forget ran during the dream of a day — nothing written, it is dreamt again")
                return None
            proposed = [r for r in (_record_from(raw, day, srcs) for raw in result.get("records") or []) if r]
            closes, left = _closures(load_records(cfg), proposed), _closes_left()
            if closes > left:
                held = _hold_closures(load_records(cfg), proposed, left)
                log.warning("records: the dream of a day would close %d facts, over the %d a night may — %d held "
                            "open and written as corrections", closes, MAX_CLOSES_PER_NIGHT, len(held))
                journal = dict(result.get("journal")) if isinstance(result.get("journal"), dict) else {}
                journal["corrections"] = [*(journal.get("corrections") or []), *held]
                result = {**result, "records": [_raw(r) for r in proposed], "journal": journal}
            change = apply_day(cfg, result, day, srcs)
            _spend_closes(change.closed)
            sha = commit(folder, f"dream: {day}") if git else None
    except OSError as e:
        log.warning("records: could not write the dream of a day (%s)", type(e).__name__)
        return None
    log.info("records: dreamt %s — %d changed, %d new, %d corrected, %d closed%s", day, change.changed,
             change.created, change.corrections, change.closed, f", commit {sha}" if sha else "")
    return change


def _rewrite_links(facts: list[str], renames: dict[str, str], own: str) -> list[str]:
    def swap(m: re.Match) -> str:
        target = renames.get(m.group(1), m.group(1))
        return target if target == own else f"[[{target}]]"      # a record never links to itself

    return [_LINK.sub(swap, fact) for fact in facts]


def consolidate(cfg: RecallConfig, client: Any) -> tuple[int, int]:
    """One model call over every record and the profile: merge the same thing filed under two ids, date
    superseded facts "(until …)". → (records changed, records removed). (0, 0) and no commit when nothing
    changed, when the call failed, or when the records are over MAX_CONSOLIDATE_CHARS (logged once).

    `client` has ``consolidate(body) -> JSON text``. The model returns the merged record whole and says
    which id went into which; the code, not the model, rewrites every [[old]] link to [[new]] and joins the
    aliases, so a merge can never strand a link or drop a name."""
    folder = records_dir(cfg)
    with _locked(folder):
        current = load_records(cfg)
        page = _profile_page(cfg)
        since = _pinned(folder)
    if len(current) < 2 and not any(len(r.facts) > 1 for r in current.values()):
        return 0, 0
    rendered = _render_all(current)
    if len(rendered) > MAX_CONSOLIDATE_CHARS:
        log.warning("records: %d chars of records is over one call's room — not consolidated tonight",
                    len(rendered))
        return 0, 0
    result = _call(client, "consolidate", "## Records\n\n" + rendered + "\n\n## The profile page\n\n" +
                   (page or "(none yet)"))
    if result is None:
        return 0, 0
    day = datetime.now().strftime("%Y-%m-%d")
    srcs = _srcs_of(current) if sources_on() else None
    try:
        with _locked(folder):
            if _generation(folder) != since:     # a forget ran during the call: its answer holds the words
                log.info("records: a forget ran during the consolidation — nothing written")
                return 0, 0
            latest = load_records(cfg)       # a star or an edit may have landed during the call
            renames: dict[str, str] = {}
            for m in result.get("merged") or []:
                if not isinstance(m, dict):
                    continue
                old, into = str(m.get("id") or "").strip().lower(), str(m.get("into") or "").strip().lower()
                if old != into and old in latest and _ID.match(into) and into != PROFILE_ID:
                    renames[old] = into
            returned = {r.id: r for r in (_record_from(raw, day, srcs) for raw in result.get("records") or []) if r}
            closes, left = _closures(latest, list(returned.values())), _closes_left()
            if closes > left:                    # over the budget: the first ones close, the rest stay open
                _hold_closures(latest, list(returned.values()), left)
                log.warning("records: the consolidation would close %d facts, over what is left of the %d a night "
                            "may — %d held open", closes, MAX_CLOSES_PER_NIGHT, closes - max(0, left))
                closes = max(0, left)
            _spend_closes(closes)
            for old, into in list(renames.items()):
                target = returned.get(into) or latest.get(into)
                if target is None or into in renames:       # merged into nothing, or into a merged id: skip
                    del renames[old]
                    continue
                if into not in returned:
                    returned[into] = Record(target.id, target.type, list(target.aliases), list(target.facts), day)
                merged = returned[into]
                for alias in [old, *latest[old].aliases]:
                    if alias.lower() not in {a.lower() for a in merged.aliases} and alias != into:
                        merged.aliases.append(alias)
                merged.aliases = merged.aliases[:MAX_ALIASES * 2]
            changed = removed = 0
            for rid in renames:
                (folder / f"{rid}.md").unlink(missing_ok=True)
                removed += 1
            for rid, rec in latest.items():                 # links into a merged id now point at its new home
                if rid in renames or rid in returned:
                    continue
                facts = _rewrite_links(rec.facts, renames, rid)
                if facts != rec.facts:
                    returned[rid] = Record(rec.id, rec.type, rec.aliases, facts, day)
            for rid, rec in returned.items():
                if rid in renames:
                    continue
                rec.facts = _rewrite_links(rec.facts, renames, rid)
                if _same(latest.get(rid), rec):
                    continue
                _write(folder / f"{rid}.md", render_record(rec))
                changed += 1
            if changed or removed:
                _reindex_profile(cfg)
                if ensure_repo(folder):
                    commit(folder, "dream: consolidate")
    except OSError as e:
        log.warning("records: could not write the consolidation (%s)", type(e).__name__)
        return 0, 0
    if changed or removed:
        log.info("records: consolidated — %d changed, %d merged away", changed, removed)
    return changed, removed


# ---- forget: owner-confirmed, every line, every file ---------------------------------------------

def _forget_head(head: str, match: Callable[[str], bool], record: bool) -> tuple[int, Optional[str]]:
    """Frontmatter keeps its lines (a file must still parse), but not what it says of a forgotten thing:
    a matching alias is dropped from the list, a matching title reads FORGOTTEN, and a record whose own id
    matches is about that thing and goes whole (None). → (values removed, new head or None)."""
    out, removed = [], 0
    for line in head.split("\n"):
        key, sep, value = line.partition(":")
        key = key.strip().lower()
        if not sep or not line.strip() or not match(line):
            out.append(line)
        elif key == "id" and record:
            return removed + 1, None
        elif key == "aliases":
            items = [a.strip() for a in value.strip().strip("[]").split(",") if a.strip()]
            kept = [a for a in items if not match(a.strip("\"'"))]
            removed += len(items) - len(kept)
            out.append(f"aliases: [{', '.join(kept)}]")
        elif key == "title":
            removed += 1
            out.append(f"title: {FORGOTTEN}")
        else:
            out.append(line)
    return removed, "\n".join(out)


def _forget_in(path: Path, match: Callable[[str], bool], record: bool) -> tuple[int, Optional[str]]:
    """(lines removed, new text or None when the file should go)."""
    raw = path.read_text(encoding="utf-8")
    fm = _FRONTMATTER.match(raw)
    head, body = (raw[:fm.end()], raw[fm.end():]) if fm else ("", raw)
    removed, new_head = _forget_head(head, match, record) if head else (0, "")
    kept = []
    for line in body.split("\n"):
        if line.strip() and match(line):
            removed += 1
        else:
            kept.append(line)
    if new_head is None:
        return removed + sum(1 for ln in kept if ln.strip()), None
    if not removed:
        return 0, raw
    if record and not any(re.match(r"^\s*[-*]\s+\S", ln) for ln in kept):
        return removed, None                      # a record with no fact left is only a name: it goes
    return removed, new_head + "\n".join(kept)


def forget_lines(cfg: RecallConfig, match: Callable[[str], bool], dry_run: bool = False) -> int:
    """Remove every line `match` accepts from the records, the profile, starred.md and the journals.
    → how many lines. Frontmatter lines stay (the files must still parse) but lose what matched: an alias,
    a journal title. A record whose id matches, or that is left with no fact, is removed whole. One commit
    "forget: N lines" (squash_history is the caller's next step, so git forgets too). A real forget moves
    the forget generation in the same hold of the lock, even when nothing here matched: a dream out on a
    model call may still hold the words."""
    folder = records_dir(cfg)
    if not folder.is_dir():
        if not dry_run:
            _bump(folder)
        return 0
    total = 0
    with _locked(folder):
        if not dry_run:
            _bump(folder)
        paths = sorted(folder.glob("*.md")) + sorted((folder / DAYS_DIR).glob("*.md"))
        for path in paths:
            is_record = path.parent == folder and path.stem != PROFILE_ID and path.name not in (STARRED_FILE, HIDDEN_FILE)
            try:
                n, text = _forget_in(path, match, is_record)
            except (OSError, UnicodeDecodeError):
                continue
            if not n:
                continue
            total += n
            if dry_run:
                continue
            try:
                if text is None:
                    path.unlink()
                elif path.name in (STARRED_FILE, HIDDEN_FILE):
                    # a line that landed since the read is kept
                    with _STAR_LOCK if path.name == STARRED_FILE else _HIDE_LOCK:
                        _, text2 = _forget_in(path, match, False)
                        if text2 is not None:
                            _write(path, text2)
                else:
                    _write(path, text)
            except OSError as e:
                log.warning("records: could not forget in one file (%s)", type(e).__name__)
        if total and not dry_run:
            if ensure_repo(folder):
                commit(folder, f"forget: {total} lines")
    return total


# ---- the dream's model client ----------------------------------------------------------------------

class OpenAIReconcileClient:
    """The dream's two calls, each one strict-JSON Responses call. store=False: the day's words are not
    kept on the provider's side (owner, 2026-09-23: "nothing should leak out ever")."""

    def __init__(self, model: str = DEFAULT_MODEL, api_key: Optional[str] = None,
                 *, consolidation_model: str = DEFAULT_CONSOLIDATION_MODEL) -> None:
        from . import openrouter

        self.model = model
        self.consolidation_model = consolidation_model
        self._client = openrouter.client(api_key, timeout=RECONCILE_TIMEOUT_SECS)

    def _call(self, model: str, instructions: str, body: str, name: str, schema: dict[str, Any]) -> str:
        resp = self._client.responses.create(
            model=model, instructions=instructions, input=body,
            text={"format": {"type": "json_schema", "name": name, "strict": True, "schema": schema}},
            max_output_tokens=MAX_OUTPUT_TOKENS, reasoning={"effort": "low"}, store=False)
        spend.record_response(spend.MEMORY, resp, model=model)
        text = (resp.output_text or "").strip()
        if not text:
            raise RuntimeError("empty reply")
        return text

    def reconcile(self, body: str) -> str:
        v, s = validity_on(), sources_on()
        return self._call(self.model, reconcile_prompt(v, s), body, "dream_day", reconcile_schema(v, s))

    def consolidate(self, body: str) -> str:
        v, s = validity_on(), sources_on()
        return self._call(self.consolidation_model, consolidate_prompt(v, s), body, "dream_consolidate",
                          consolidate_schema(v, s))


def make_client(environ: Any = None, model: str = DEFAULT_MODEL) -> Optional[OpenAIReconcileClient]:
    """The dream's real client, or None (with one log line) when there is no key or no SDK."""
    env = os.environ if environ is None else environ
    from . import openrouter

    key = openrouter.key(env)
    if not key:
        log.warning("records: OPENROUTER_API_KEY not set — the records are read but never dreamt")
        return None
    try:
        return OpenAIReconcileClient(model, api_key=key)
    except ImportError as e:
        log.warning("records: openai SDK not importable (%s) — the records are read but never dreamt",
                    type(e).__name__)
        return None
