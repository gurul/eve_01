"""Memory, one door: the facade both brains talk to, the one tool set, forget, and the move to one root.

The owner asked for memory to be simpler (owner, 2026-09-23: "too many stores", "reconcile and minimize").
There are now three stores under one folder, and this module is the only thing a brain holds:

    transcripts/   every word, written the moment it is said (transcripts.py): the source
    records/       what is true about the owner, rewritten nightly by the dream (records.py): the truth
    mem0/          a search by meaning, rebuilt from the transcripts (mem0_memory.py): the index
    archive/       the retired debrief store, moved here once, read-only, still searched

### One tool set, the same on both channels

Before this, the Telegram brain could search records and the voice brain could search nothing, so what
eve "knew" depended on how the owner reached it. Now both get ``memory_search`` (three dated groups:
record lines, recalled meanings, and the words themselves), ``memory_read`` (a record, or a stretch of a
day's transcript), and the two-step forget. Voice also gets ``recent_conversation``: what was typed on the
other channel since this voice session opened.

### Forget is two calls and one confirmation

"Forget that" used to remove one record file and leave the same words in nine other places. Now
``forget_preview`` counts every place a match lives — transcripts, records (and their journal and stars),
the index, the archive, meeting notes — and hands back a single-use token that lives ten minutes and is
bound to that query. It never returns the matched words: a preview is a count, so the owner decides on
numbers, not on eve reading their secret back to them. ``forget_apply`` runs only with the token, after
the owner confirms in a new message. The git histories that held the words are squashed to one commit, so
the words are gone from history too. claude-mem and the Obsidian vault are not eve's stores, and the chief of
staff's ledger is not reached yet; the preview names them as not covered rather than pretending.

### The move, once

``migrate`` renames the old debrief store into the new root the first time memory starts with the new
layout. Renames only, on one volume: nothing is copied and nothing is deleted. A target that already
exists is skipped and counted, never overwritten. ``.migrated`` records the counts, and a second run
does nothing.

Logs carry counts and milliseconds, never a word that was said, and never a file name (a day file's name
is a summary of the day).
"""

from __future__ import annotations

import json
import logging
import math
import os
import re
import secrets
import threading
import time
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path
from typing import Any, Callable, Mapping, Optional

from . import memory_index
from .recall import RecallConfig
from .transcripts import (
    MEETINGS_SUBDIR,
    SEARCH_DAYS_BACK,
    VOICE_CHARS,
    Transcripts,
    configured,
    match,
    render,
)

log = logging.getLogger(__name__)

FORGET_TTL_SECS = 600.0                        # a forget token lives ten minutes
MAX_PENDING_TOKENS = 16
RECENT_CHARS = VOICE_CHARS                     # recent_conversation: the other channel, newest kept
MIGRATED_FILE = ".migrated"
RETRY_DAYS_BACK = 365                          # EVE_MEMORY_RETRY: an empty search is tried again this far back
# Places eve cannot forget in: named, never pretended. The chief ledger (chief_ledger.py, the cards and their
# events under ~/.config/eve-bridge/chief) holds card titles and goals; it is named here until a forget
# layer covers it (chief design, 2026-09-29, P6).
NOT_COVERED = ("claude-mem", "vault", "chief ledger")
LAYERS = ("transcripts", "records", "index", "archive", "meetings")
CHAT_LOG_NOT_COVERED = "telegram chat log"     # named while no chat log is attached (Memory.chat_log is None)

# A forget of a sentence also takes what the dream wrote from it in its own words ("failed my driving test" →
# "failing the driving test"): in the stores the dream writes (records, the index, the archive), a line goes when
# it holds most of the sentence's distinctive words, compared by their first four letters. The transcripts keep
# the exact rule: they hold the owner's own words.
REWORD_MIN_WORDS = 3            # a query with fewer distinctive words than this forgets by the exact rule only
REWORD_MIN_SHARE = 0.5          # and a line must hold at least this share of them (and at least REWORD_MIN_WORDS)
_WORD = re.compile(r"[a-z0-9]+")
_COMMON = frozenset("""a about after again all also am an and any are as at back be been before being but by can
could did do does doing done for from get got had has have having he her here him his how i if in into is it its
just me more most my no not now of off on once only or our out over really said she so some still such than
that the their them then there these they this those to too up very was we were what when where which while
who why will with would yes yet you your honestly actually basically""".split())


def _keys(text: str) -> set[str]:
    words = _WORD.findall(str(text or "").casefold().replace("'", ""))
    return {w[:4] for w in words if w not in _COMMON and (len(w) > 2 or w.isdigit())}


def reworded(query: str) -> Optional[Callable[[str], bool]]:
    """The predicate for a line the dream wrote from `query` in other words, or None when `query` is too short
    (or quoted) to judge that by its words."""
    if '"' in str(query or ""):
        return None
    want = _keys(query)
    if len(want) < REWORD_MIN_WORDS:
        return None
    need = max(REWORD_MIN_WORDS, math.ceil(len(want) * REWORD_MIN_SHARE))

    def predicate(text: str) -> bool:
        return len(want & _keys(text)) >= need

    return predicate
# The retired debrief store seeds HIGHLIGHTS.md with a worked example; only eve's own section is the owner's.
OWN_SECTION = "from talking"
EXAMPLE_MARK = "*(example)*"
TEMPLATE_BANNER = "> **Template.**"

_DAY = re.compile(r"^\d{4}-\d{2}-\d{2}$")
_READ_REF = re.compile(r"^(\d{4}-\d{2}-\d{2})(?:[ T]+(\d{1,2}:\d{2})(?:\s*-\s*(\d{1,2}:\d{2}))?)?$")
_STAR_DATE = re.compile(r"\s*\((\d{4}-\d{2}-\d{2})\)\s*$")
_FRONTMATTER_END = re.compile(r"\n---[ \t]*(?:\n|\Z)")


def enabled(environ: Any = None) -> bool:
    """``EVE_MEMORY=1`` turns memory on; the older ``EVE_RECORDS=1`` still does."""
    return configured(environ).enabled


def _records_mod() -> Any:
    """records.py, imported when first used: it is rewritten beside this module, and tests swap it."""
    from . import records
    return records


# ---- the tools ---------------------------------------------------------------------------------

def _tool(name: str, description: str, properties: dict[str, Any]) -> dict[str, Any]:
    return {"type": "function", "name": name, "strict": True, "description": description,
            "parameters": {"type": "object", "additionalProperties": False,
                           "required": list(properties), "properties": properties}}


MEMORY_SEARCH = _tool(
    "memory_search",
    "Search your memory of the owner. Use it before you answer a question about their life, their plans, "
    "their taste or a past conversation, and before you say that you do not know. It returns three groups, "
    "each with dates: 'hits' are lines from your records, 'recalled' are memories found by meaning, and "
    "'said' are the words from past conversations, meetings and old notes, with the line before and after. "
    "Every word of the query must be in a 'said' line, so use few words. Put a phrase in double quotes to "
    "find it as written.",
    {"query": {"type": "string", "description": "A few words, as the owner would say them."},
     "days_back": {"type": ["integer", "null"],
                   "description": "How many days of conversation to search. Null for the last 30 days."}})
MEMORY_READ = _tool(
    "memory_read",
    "Read one thing from memory in full. Give a record id to read that record: the ids are in your profile "
    "and in search results. Give a day to read what was said on that day: write the day as year-month-day. "
    "After the day, you can add a start time, or a start time and an end time joined by a hyphen, in "
    "24-hour hours:minutes. When the text stops early, read again from the time it gives.",
    {"ref": {"type": "string", "description": "A record id, or a day with an optional time or time range."}})
FORGET_PREVIEW = _tool(
    "forget_preview",
    "Use this only when the owner explicitly asks you to forget something. It counts every place in your "
    "memory that holds those words and returns a token. It changes nothing and does not show the words. "
    "Tell the owner the counts and ask them to confirm. Do not call forget_apply in the same turn.",
    {"query": {"type": "string", "description": "The words to forget, as few as identify them."}})
FORGET_APPLY = _tool(
    "forget_apply",
    "Forget, permanently, what forget_preview counted. Call it only after the owner confirms in a new "
    "message. The token works once and expires after ten minutes; when it has expired, preview again.",
    {"token": {"type": "string", "description": "The token from forget_preview."}})
RECENT_CONVERSATION = _tool(
    "recent_conversation",
    "Read what the owner and you said on the other channel, typed in chat, since this voice conversation "
    "started. Use it when the owner refers to something they just wrote to you.",
    {})

TOOLS = (MEMORY_SEARCH, MEMORY_READ, FORGET_PREVIEW, FORGET_APPLY)
VOICE_TOOLS = TOOLS + (RECENT_CONVERSATION,)

# memory_search as the three search switches shape it (memory_index.py). Each sentence states what is true with
# its switch on; with every switch off this is MEMORY_SEARCH, word for word. Fixed for the process, so the tool
# list stays a stable, cached prefix.
_SEARCH_OPENING = MEMORY_SEARCH["description"].split(" Every word of the query")[0]
_SEARCH_KEYWORD = (" Every word of the query must be in a 'said' line, so use few words. Put a phrase in double "
                   "quotes to find it as written.")
_SEARCH_RANKED = (" The 'said' lines come best match first, ranked by meaning and by the words they share with "
                  "the query, so name the thing itself: the person, the place or the object.")
_SEARCH_TIME = (" When the question names a time (yesterday, last week, in May, two weeks ago, a date), pass it as "
                "from and to.")
_SEARCH_RETRY = (" When a search finds nothing, search once more with other words (a synonym, or the one most "
                 "telling word) before you say it is not in your memory.")
_SEARCH_CAP = " A turn has at most three memory calls."     # only where code enforces it: the Telegram brain
_TIME_ARG = ("A day as year-month-day, or the owner's own words for the time, like yesterday, last week, in May "
             "or two weeks ago. Null for no limit.")


def memory_search_tool(hybrid: bool = False, timeargs: bool = False, retry: bool = False, *,
                       capped: bool = True) -> dict[str, Any]:
    """The memory_search schema for these switches. All off: MEMORY_SEARCH itself. `capped`: the caller counts
    memory calls per turn (the Telegram brain does; voice does not), so the description may say so."""
    if not (hybrid or timeargs or retry):
        return MEMORY_SEARCH
    text = _SEARCH_OPENING + (_SEARCH_RANKED if hybrid else _SEARCH_KEYWORD)
    text += _SEARCH_TIME if timeargs else ""
    text += (_SEARCH_RETRY + (_SEARCH_CAP if capped else "")) if retry else ""
    props = dict(MEMORY_SEARCH["parameters"]["properties"])
    if timeargs:
        props["from"] = {"type": ["string", "null"], "description": "The first day to search. " + _TIME_ARG}
        props["to"] = {"type": ["string", "null"], "description": "The last day to search. " + _TIME_ARG}
    return _tool("memory_search", text, props)


# ---- markdown line removal (archive and meeting notes) --------------------------------------------

def _split_frontmatter(text: str) -> tuple[str, str]:
    """(frontmatter including its closing fence, body). Forget never touches frontmatter: it holds ids."""
    if not text.startswith("---\n"):
        return "", text
    m = _FRONTMATTER_END.search(text, 3)
    if m is None:
        return "", text
    return text[: m.end()], text[m.end():]


def _markdown_files(folder: Path) -> list[Path]:
    """Every regular .md file under `folder`, never inside a .git and never through a symlink."""
    out: list[Path] = []
    if not folder.is_dir() or folder.is_symlink():
        return out
    for base, dirs, files in os.walk(folder, followlinks=False):
        dirs[:] = [d for d in dirs if d != ".git" and not os.path.islink(os.path.join(base, d))]
        for name in files:
            p = Path(base) / name
            if name.endswith(".md") and not p.is_symlink():
                out.append(p)
    return sorted(out)


def _atomic_rewrite(path: Path, text: str) -> None:
    """Replace `path` whole: a temp file beside it with the same mode, fsync, rename."""
    mode = os.stat(path).st_mode & 0o7777
    tmp = path.with_name(f".{path.name}.{os.getpid()}.{secrets.token_hex(4)}.tmp")
    fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600)
    try:
        try:
            os.fchmod(fd, mode)
            view = memoryview(text.encode("utf-8"))
            while view:
                n = os.write(fd, view)
                view = view[n:]
            os.fsync(fd)
        finally:
            os.close(fd)
        os.replace(tmp, path)
    except BaseException:
        try:
            os.unlink(tmp)
        except OSError:
            pass
        raise


def forget_markdown(folder: Path, pred: Callable[[str], bool], dry_run: bool = False) -> int:
    """Remove every body line `pred` matches from the markdown under `folder`. → lines (to be) removed."""
    total = 0
    for path in _markdown_files(folder):
        try:
            text = path.read_text(encoding="utf-8", errors="replace")
        except OSError:
            continue
        head, body = _split_frontmatter(text)
        rows = body.splitlines(keepends=True)
        kept = [r for r in rows if not pred(r)]
        gone = len(rows) - len(kept)
        if not gone:
            continue
        total += gone
        if dry_run:
            continue
        try:
            _atomic_rewrite(path, head + "".join(kept))
        except OSError as e:
            log.warning("memory: a markdown file could not be rewritten during forget (%s)", type(e).__name__)
    return total


# ---- the facade ----------------------------------------------------------------------------------

@dataclass
class _Pending:
    query: str
    expires: float


class Memory:
    """What both brains are lent. Built once by the daemon; every method is synchronous and never raises,
    so callers run the slow ones (search, forget) through ``asyncio.to_thread``.

    ``transcripts`` is public: the brains append each turn through it. ``index`` is mem0 (or None when it is
    off). ``clock`` times the forget tokens; tests inject one. ``background``: start the search index's worker
    (its build and its embeddings). Only the daemon passes True; the forget, dream and reindex commands and the
    eval open the index without it, so a short-lived process sends nothing to the embeddings endpoint and holds
    no lock a forget waits on."""

    def __init__(self, cfg: RecallConfig, transcripts: Transcripts, index: Optional[Any],
                 clock: Callable[[], float] = time.monotonic, *, background: bool = False) -> None:
        self.cfg = cfg
        self.transcripts = transcripts
        self.index = index
        self._clock = clock
        self._lock = threading.Lock()
        self._pending: dict[str, _Pending] = {}
        # The Telegram chat log (chat_log.ChatLog), set by the daemon when it keeps one: forget reaches it as
        # the "chat_log" layer. None: no such layer is counted or named.
        self.chat_log: Optional[Any] = None
        # The search switches (memory_index.py), read once: the tool schemas must not change mid-process.
        self.hybrid = memory_index.switch("hybrid")
        self.timeargs = memory_index.switch("timeargs")
        self.retry = memory_index.switch("retry")
        self.ranked: Optional[memory_index.MemoryIndex] = None
        if self.hybrid:
            self.ranked = memory_index.open_index(cfg.store, transcripts)
            if self.ranked is not None:
                transcripts.ranker = self.ranked
                transcripts.listeners.append(self.ranked.notify)
                if background:
                    self.ranked.start()

    # -- what goes in the prompt ---------------------------------------------------------------

    def profile(self) -> str:
        """The one-pager (≤ 6000 chars), with stars the dream has not absorbed yet on top. "" on any failure."""
        try:
            return _records_mod().profile(self.cfg) or ""
        except Exception as e:  # noqa: BLE001 - a missing page is no page
            log.warning("memory: profile unavailable (%s)", type(e).__name__)
            return ""

    def profile_for_voice(self) -> str:
        """The profile without the record index (≤ 3000 chars): the Live voice prompt is latency-bound."""
        try:
            return _records_mod().profile_for_voice(self.cfg) or ""
        except Exception as e:  # noqa: BLE001
            log.warning("memory: voice profile unavailable (%s)", type(e).__name__)
            return ""

    def talking_section(self) -> str:
        """The profile's "How they like to talk" section, whole (records.talking_section). "" on any failure."""
        try:
            return _records_mod().talking_section(self.cfg) or ""
        except Exception as e:  # noqa: BLE001
            log.warning("memory: talking section unavailable (%s)", type(e).__name__)
            return ""

    def today(self, max_chars: int, *, exclude_conv: Optional[str] = None, exclude_tail: int = 0,
              style: str = "text", exclude: Optional[Mapping[str, int]] = None) -> str:
        """Today on both channels, within `max_chars` (see Transcripts.today_block)."""
        try:
            return self.transcripts.today_block(max_chars, exclude_conv=exclude_conv,
                                                exclude_tail=exclude_tail, style=style, exclude=exclude)
        except Exception as e:  # noqa: BLE001
            log.warning("memory: today block unavailable (%s)", type(e).__name__)
            return ""

    def star(self, text: str) -> Optional[str]:
        """The owner said "remember that": into starred.md, on the profile from the next message."""
        try:
            return _records_mod().star(self.cfg, text)
        except Exception as e:  # noqa: BLE001
            log.warning("memory: star failed (%s)", type(e).__name__)
            return None

    # -- don't mention this again (records.hide; the app's Memory card) ------------------------------

    def hide(self, text: str) -> Optional[str]:
        """Leave out every line these words match, from the profile and every search, until unhidden. Deletes
        nothing. → the line as kept in hidden.md, or None."""
        try:
            return _records_mod().hide(self.cfg, text)
        except Exception as e:  # noqa: BLE001
            log.warning("memory: hide failed (%s)", type(e).__name__)
            return None

    def unhide(self, text: str) -> bool:
        try:
            return bool(_records_mod().unhide(self.cfg, text))
        except Exception as e:  # noqa: BLE001
            log.warning("memory: unhide failed (%s)", type(e).__name__)
            return False

    def hidden(self) -> list[str]:
        try:
            return list(_records_mod().hidden(self.cfg))
        except Exception as e:  # noqa: BLE001
            log.warning("memory: hidden list unavailable (%s)", type(e).__name__)
            return []

    def _hidden_match(self) -> Optional[Callable[[str], bool]]:
        try:
            return _records_mod().hidden_match(self.cfg)
        except Exception as e:  # noqa: BLE001 - an unreadable hidden list hides nothing
            log.warning("memory: hidden list unavailable (%s)", type(e).__name__)
            return None

    # -- tools -------------------------------------------------------------------------------------

    def tools(self, *, voice: bool = False) -> list[dict[str, Any]]:
        """The memory tools, strict schemas, stable order. Copies: a caller may annotate its own."""
        search = memory_search_tool(self.hybrid, self.timeargs, self.retry, capped=not voice)
        tools = [search, *(VOICE_TOOLS if voice else TOOLS)[1:]]
        return json.loads(json.dumps(tools))

    def handle_tool(self, name: str, args: dict[str, Any], *, since: Optional[datetime] = None,
                    channel: str = "") -> dict[str, Any]:
        """Run one memory tool. `since` and `channel` serve recent_conversation: the session's start and
        the caller's own channel, which is left out. Never raises."""
        args = args if isinstance(args, dict) else {}
        try:
            if name == "memory_search":
                if self.timeargs:
                    return self.search(str(args.get("query") or ""), args.get("days_back"),
                                       date_from=args.get("from"), date_to=args.get("to"))
                return self.search(str(args.get("query") or ""), args.get("days_back"))
            if name == "memory_read":
                return self.read(str(args.get("ref") or ""))
            if name == "forget_preview":
                return self.forget_preview(str(args.get("query") or ""))
            if name == "forget_apply":
                return self.forget_apply(str(args.get("token") or ""))
            if name == "recent_conversation":
                return self.recent_conversation(since, channel)
        except Exception as e:  # noqa: BLE001 - a tool failure is an answer, never a crash
            log.warning("memory: %s failed (%s)", name, type(e).__name__)
            return {"ok": False, "reason": "memory is unavailable right now"}
        return {"ok": False, "reason": f"unknown tool {name!r}"}

    def search(self, query: str, days_back: Any = None, *, date_from: Any = None,
               date_to: Any = None) -> dict[str, Any]:
        """Three dated groups: record lines, recalled meanings, and the words said.

        With EVE_MEMORY_TIMEARGS, `date_from`/`date_to` (a day, or words like "last week") limit the words
        said to a range (memory_index.resolve_args). Without the ranked index a range that finds nothing is read again
        with half the telling words ("fewer_words"); still nothing, it is widened once ("widened"). With
        EVE_MEMORY_RETRY, words said that find nothing are searched again by code over a year (and, without
        the ranked index, with half the words), and the result says to search once more with other words."""
        started = time.monotonic()
        if match(query) is None:
            return {"ok": False, "reason": "the query has no word to search for"}
        try:
            days = int(days_back) if days_back is not None else SEARCH_DAYS_BACK
        except (TypeError, ValueError):
            days = SEARCH_DAYS_BACK
        out: dict[str, Any] = {"ok": True, "hits": [], "recalled": [], "said": []}
        # The index search is an embedding round-trip (mem0_memory.py, its own 3 s cap); it runs on a thread while
        # the records and the transcripts are read here, and is joined at the end (2026-10-09 review: it was first,
        # and the local reads waited the whole round-trip). Its result, order and cap are the same.
        recalled: dict[str, Any] = {}
        joiner: Optional[threading.Thread] = None
        if self.index is not None:
            def recall() -> None:
                try:
                    recalled["rows"] = list(self.index.search(query) or [])
                except Exception as e:  # noqa: BLE001
                    recalled["error"] = e
            joiner = threading.Thread(target=recall, name="memory-recall", daemon=True)
            joiner.start()
        try:
            got = _records_mod().RecordsReader(self.cfg, None).search(query)
            out["hits"] = list(got.get("hits") or [])
        except Exception as e:  # noqa: BLE001
            log.warning("memory: records search failed (%s)", type(e).__name__)
        rng = None
        if self.timeargs and (date_from is not None or date_to is not None):
            rng, unread = memory_index.resolve_args(date_from, date_to, self.transcripts.now())
            if unread:
                out["time_note"] = "a time could not be read as a date; searched without it"
            if rng is not None:
                out["range"] = {"from": rng[0].isoformat(), "to": rng[1].isoformat()}
        said = self._said(query, days, rng)
        if rng is not None and not said.get("hits") and not said.get("ranked"):
            # Inside a named time, half the telling words are enough: the range already keeps it narrow.
            said = self._said(query, days, rng, loose=True)
            if said.get("hits"):
                out["fewer_words"] = True
        if rng is not None and not said.get("hits"):
            today = memory_index.owner_day(self.transcripts.now())
            rng = memory_index.widen(rng, today)
            said = self._said(query, days, rng, loose=not said.get("ranked"))
            out["widened"] = {"from": rng[0].isoformat(), "to": rng[1].isoformat()}
        if self.retry and not said.get("hits"):
            wide = max(days, RETRY_DAYS_BACK)
            said = self._said(query, wide, None)
            if not said.get("hits") and not said.get("ranked"):
                said = self._said(query, wide, None, loose=True)
            out["searched_again"] = f"the last {wide} days" + ("" if said.get("ranked") else ", with fewer words")
        hide = self._hidden_match()
        out["said"] = [h for h in said.get("hits") or []
                       if hide is None or not (isinstance(h, dict) and hide(str(h.get("text") or "")))]
        if said.get("truncated"):
            out["truncated"] = True
        if joiner is not None:
            joiner.join()
            if "error" in recalled:
                log.warning("memory: index search failed (%s)", type(recalled["error"]).__name__)
            else:
                try:
                    out["recalled"] = [self._recalled(r) for r in recalled.get("rows") or []
                                       if hide is None or not hide(str(r.get("text") or "") if isinstance(r, dict)
                                                                   else str(r))]
                except Exception as e:  # noqa: BLE001
                    log.warning("memory: index search failed (%s)", type(e).__name__)
        if not (out["hits"] or out["recalled"] or out["said"]):
            out["note"] = "nothing matched"
            if self.retry:
                out["note"] = ("nothing matched; search once more with other words (a synonym, or the one most "
                               "telling word) before you say it is not in your memory")
        log.info("memory: search → %d hits, %d recalled, %d said in %d ms", len(out["hits"]),
                 len(out["recalled"]), len(out["said"]), (time.monotonic() - started) * 1000)
        return out

    def _said(self, query: str, days: int, rng: Any, *, loose: bool = False) -> dict[str, Any]:
        try:
            if rng is not None:
                return self.transcripts.search(query, days_back=days, since=rng[0].isoformat(),
                                               until=rng[1].isoformat(), loose=loose)
            return self.transcripts.search(query, days_back=days, loose=loose)
        except Exception as e:  # noqa: BLE001
            log.warning("memory: transcript search failed (%s)", type(e).__name__)
            return {"hits": []}

    @staticmethod
    def _recalled(row: Any) -> str:
        if not isinstance(row, dict):
            return str(row)
        day, ch, text = str(row.get("day") or ""), str(row.get("channel") or ""), str(row.get("text") or "")
        tag = ", ".join(p for p in (str(row.get("src") or "") or day, ch) if p)   # src: the line's day and time
        return f"({tag}) {text}" if tag else text

    def read(self, ref: str) -> dict[str, Any]:
        """A day or a stretch of one (transcript), else a record id."""
        ref = " ".join(ref.split())
        if not ref:
            return {"ok": False, "reason": "nothing to read"}
        m = _READ_REF.match(ref)
        if m is not None:
            day, start, end = m.group(1), m.group(2) or "", m.group(3) or ""
            text = self.transcripts.read(day, start, end)
            if not text:
                return {"ok": False, "reason": "nothing was said then"}
            return {"ok": True, "day": day, "text": text}
        return _records_mod().RecordsReader(self.cfg, None).get(ref)

    def recent_conversation(self, since: Optional[datetime], channel: str) -> dict[str, Any]:
        """The other channel since `since`, newest kept within RECENT_CHARS."""
        if since is None:
            return {"ok": False, "reason": "no conversation is open"}
        rows = [render([ln], style="voice") for ln in self.transcripts.lines_since(since, exclude_ch=channel)]
        kept: list[str] = []
        used = 0
        for r in reversed(rows):
            if used + len(r) + 1 > RECENT_CHARS:
                break
            kept.append(r)
            used += len(r) + 1
        kept.reverse()
        if not kept:
            return {"ok": True, "text": "", "note": "nothing on the other channel since this conversation began"}
        return {"ok": True, "text": "\n".join(kept), "omitted": len(rows) - len(kept)}

    # -- forget ------------------------------------------------------------------------------------

    def _ranked_for_forget(self) -> tuple[Optional[memory_index.MemoryIndex], bool]:
        """(the search index a forget must clean, whether it was opened just for this). The live one when the
        hybrid switch is on; with it off, the file left from when it was on, opened without an embedder, so its
        words are counted and forgotten too. (None, False) when there is no file."""
        if self.ranked is not None:
            return self.ranked, False
        if not memory_index.index_path(self.cfg.store).exists():
            return None, False
        idx = memory_index.open_index(self.cfg.store, self.transcripts, embed=False)
        return idx, idx is not None

    def _count(self, query: str, pred: Callable[[str], bool]) -> dict[str, int]:
        layers = dict.fromkeys(LAYERS, 0)
        try:
            layers["transcripts"] = int(self.transcripts.redact(query, dry_run=True) or 0)
        except Exception as e:  # noqa: BLE001
            log.warning("memory: forget count failed for transcripts (%s)", type(e).__name__)
        try:
            layers["records"] = int(_records_mod().forget_lines(self.cfg, pred, dry_run=True) or 0)
        except Exception as e:  # noqa: BLE001
            log.warning("memory: forget count failed for records (%s)", type(e).__name__)
        if self.index is not None:
            try:
                layers["index"] = len(self.index.find(pred) or [])
            except Exception as e:  # noqa: BLE001
                log.warning("memory: forget count failed for the index (%s)", type(e).__name__)
        ranked, opened = self._ranked_for_forget()
        if ranked is not None:
            layers["index"] += ranked.count(pred)
            if opened:
                ranked.close()
        layers["archive"] = forget_markdown(self.cfg.archive_dir, pred, dry_run=True)
        layers["meetings"] = forget_markdown(self.cfg.transcripts_dir / MEETINGS_SUBDIR, pred, dry_run=True)
        if self.chat_log is not None:
            try:
                layers["chat_log"] = int(self.chat_log.count(pred) or 0)
            except Exception as e:  # noqa: BLE001
                log.warning("memory: forget count failed for the chat log (%s)", type(e).__name__)
                layers["chat_log"] = 0
        return layers

    def _not_covered(self) -> list[str]:
        """The places this forget does not reach. The Telegram chat log is one of them while none is attached: a
        forget never claims "everywhere" while its words stay on disk (review, 2026-10-08)."""
        return list(NOT_COVERED) + ([CHAT_LOG_NOT_COVERED] if self.chat_log is None else [])

    def _follow_sources(self, query: str, pred: Callable[[str], bool]) -> Callable[[str], bool]:
        """`pred`, widened to every record fact and index memory whose src points at a transcript line `query`
        matches, so a fact the dream wrote in other words goes with the line it came from. Whatever
        EVE_MEMORY_SOURCES says now: a src written while it was on is still followed after it is turned off.
        Also widened to a line holding most of the query's words in other forms (`reworded`). Call before the
        transcripts are redacted."""
        marks: set[str] = set()
        try:
            for ln in self.transcripts.find(query):
                try:
                    marks.add(f"src: {datetime.fromisoformat(str(ln.get('ts'))):%Y-%m-%d %H:%M}")
                except (TypeError, ValueError):
                    continue
        except Exception as e:  # noqa: BLE001 - no pointers to follow is the plain forget
            log.warning("memory: forget could not read the source lines (%s)", type(e).__name__)
        alt = reworded(query)
        if alt is not None:
            plain = pred
            pred = lambda text: plain(text) or alt(text)  # noqa: E731
        if not marks:
            return pred
        return lambda text: pred(text) or any(m in str(text) for m in marks)

    def forget_preview(self, query: str) -> dict[str, Any]:
        """Count what `query` matches in every store and mint a token. Changes nothing; shows no words."""
        started = time.monotonic()
        query = " ".join(str(query or "").split())
        pred = match(query)
        if pred is None:
            return {"ok": False, "reason": "the query has no word to match"}
        layers = self._count(query, self._follow_sources(query, pred))
        total = sum(layers.values())
        out: dict[str, Any] = {"ok": True, "total": total, "layers": layers, "not_covered": self._not_covered()}
        if total:
            token = secrets.token_urlsafe(12)
            now = self._clock()
            with self._lock:
                self._pending = {k: p for k, p in self._pending.items() if p.expires > now}
                while len(self._pending) >= MAX_PENDING_TOKENS:
                    self._pending.pop(next(iter(self._pending)))
                self._pending[token] = _Pending(query, now + FORGET_TTL_SECS)
            out.update(token=token, expires_in=int(FORGET_TTL_SECS))
        else:
            out.update(token="", expires_in=0, note="nothing in memory matches")
        log.info("memory: forget preview → %d matches in %d ms", total, (time.monotonic() - started) * 1000)
        return out

    def forget_apply(self, token: str) -> dict[str, Any]:
        """Forget what the token's preview counted, in every store. Single use."""
        started = time.monotonic()
        with self._lock:
            pending = self._pending.pop(str(token or ""), None)
        if pending is None or pending.expires <= self._clock():
            return {"ok": False, "reason": "the token is unknown, used or expired; preview again"}
        pred = match(pending.query)
        if pred is None:                                  # pragma: no cover - preview refused such a query
            return {"ok": False, "reason": "the query has no word to match"}
        pred = self._follow_sources(pending.query, pred)  # before the redact: it reads the lines' times
        rec = _records_mod()
        done = dict.fromkeys(LAYERS, 0)
        failed: list[str] = []                        # layers a step failed in: the words may still be there
        try:
            done["transcripts"] = int(self.transcripts.redact(pending.query) or 0)
        except Exception as e:  # noqa: BLE001
            failed.append("transcripts")
            log.warning("memory: forget failed in transcripts (%s)", type(e).__name__)
        try:
            done["records"] = int(rec.forget_lines(self.cfg, pred) or 0)
        except Exception as e:  # noqa: BLE001
            failed.append("records")
            log.warning("memory: forget failed in records (%s)", type(e).__name__)
        if self.index is not None:
            try:
                forget_matching = getattr(self.index, "forget_matching", None)
                if forget_matching is not None:       # mem0: find and delete in one hold of its lock
                    done["index"] = int(forget_matching(pred) or 0)
                else:
                    ids = [str(r.get("id")) for r in self.index.find(pred) or []
                           if isinstance(r, dict) and r.get("id")]
                    done["index"] = int(self.index.forget(ids) or 0) if ids else 0
            except Exception as e:  # noqa: BLE001
                failed.append("index")
                log.warning("memory: forget failed in the index (%s)", type(e).__name__)
        ranked, opened = self._ranked_for_forget()    # the search index, switch on or off: rounds, FTS, vectors
        if ranked is not None:
            try:
                done["index"] += int(ranked.forget(pred) or 0)
            except Exception as e:  # noqa: BLE001 - IndexForgetError: the words may remain; say so
                if "index" not in failed:
                    failed.append("index")
                log.warning("memory: forget failed in the search index (%s)", type(e).__name__)
            finally:
                if opened:
                    ranked.close()
        done["archive"] = forget_markdown(self.cfg.archive_dir, pred)
        done["meetings"] = forget_markdown(self.cfg.transcripts_dir / MEETINGS_SUBDIR, pred)
        if self.chat_log is not None:
            try:
                done["chat_log"] = int(self.chat_log.forget(pred) or 0)
            except Exception as e:  # noqa: BLE001
                failed.append("chat_log")
                log.warning("memory: forget failed in the chat log (%s)", type(e).__name__)
                done["chat_log"] = 0
        total = sum(done.values())
        squashed: list[str] = []
        # History held the words too: the records repo committed every change, and the archive repo still
        # holds the old records and notes in its commits. Both become one commit of what is left.
        if total:
            for name, repo in (("records", self.cfg.records_dir), ("archive", self.cfg.archive_dir)):
                if (repo / ".git").is_dir():
                    try:
                        if rec.squash_history(repo):
                            squashed.append(name)
                    except Exception as e:  # noqa: BLE001
                        log.warning("memory: history squash failed for %s (%s)", name, type(e).__name__)
        log.info("memory: forgot %d lines (%s) in %d ms%s", total,
                 ", ".join(f"{k} {v}" for k, v in done.items()), (time.monotonic() - started) * 1000,
                 f"; failed in {', '.join(failed)}" if failed else "")
        out: dict[str, Any] = {"ok": not failed, "total": total, "layers": done, "history_squashed": squashed,
                               "not_covered": self._not_covered()}
        if failed:
            out["failed"] = failed
            out["reason"] = ("the forget did not finish in: " + ", ".join(failed) +
                             "; the words may still be there. Preview and forget again.")
        return out


# ---- the one-time move -------------------------------------------------------------------------

def _file_count(path: Path) -> int:
    if path.is_symlink() or path.is_file():
        return 1
    n = 0
    for _base, _dirs, files in os.walk(path, followlinks=False):
        n += len(files)
    return n


def _same(a: Path, b: Path) -> bool:
    try:
        return os.path.realpath(a) == os.path.realpath(b)
    except OSError:
        return False


def _private_dir(path: Path) -> None:
    path.mkdir(parents=True, exist_ok=True)
    try:
        os.chmod(path, 0o700)
    except OSError:
        pass


def _owner_stars(text: str) -> list[tuple[str, Optional[datetime]]]:
    """★ lines from eve's own "From talking" section of the old HIGHLIGHTS.md, never the seeded example."""
    out: list[tuple[str, Optional[datetime]]] = []
    inside = False
    for row in text.splitlines():
        s = row.strip()
        if s.startswith("#"):
            inside = s.lstrip("# ").lower().startswith(OWN_SECTION)
            continue
        if not inside or TEMPLATE_BANNER in s:
            continue
        line = s.lstrip("-* ").strip()
        if not line.startswith("★"):
            continue
        claim = line[1:].strip()
        if not claim or EXAMPLE_MARK in claim or claim.lower().startswith("(candidate)"):
            continue
        when = None
        m = _STAR_DATE.search(claim)
        if m is not None:
            claim = claim[: m.start()].strip()
            try:
                when = datetime.strptime(m.group(1), "%Y-%m-%d")
            except ValueError:
                when = None
        if claim:
            out.append((claim, when))
    return out


def migrate(cfg: RecallConfig, legacy_store: Path, legacy_mem0: Path) -> dict[str, Any]:
    """Move the old debrief store and the old mem0 folder under the new memory root, once.

    legacy/records → records/; the owner's ★ lines in HIGHLIGHTS "From talking" → records/starred.md
    (through records.star, so the format is records'); legacy_mem0 → mem0/; legacy/notes/<date> →
    transcripts/meetings/<date>; everything else in legacy (sessions, day files, INDEX, HIGHLIGHTS, .git)
    → archive/, and the archive's repository is sealed. Renames only: nothing is copied or deleted, and a
    rename across volumes is refused rather than turned into copy-and-delete. A target that exists is
    skipped and counted. Idempotent: ``<root>/.migrated`` holds the counts, and while it exists this
    returns them without touching anything. When a move failed, no marker is written, so the next start
    tries again (a moved source is simply no longer there)."""
    root = Path(cfg.store).expanduser()
    marker = root / MIGRATED_FILE
    if marker.exists():
        try:
            prior = json.loads(marker.read_text(encoding="utf-8"))
        except (OSError, ValueError):
            prior = {}
        return {"ok": True, "already": True, **(prior if isinstance(prior, dict) else {})}
    legacy = Path(legacy_store).expanduser()
    old_mem0 = Path(legacy_mem0).expanduser()
    counts: dict[str, int] = {"records": 0, "stars": 0, "mem0": 0, "meetings": 0, "archive": 0,
                              "skipped": 0, "failed": 0}
    try:
        lr, rr = os.path.realpath(legacy), os.path.realpath(root)
    except OSError:
        lr, rr = str(legacy), str(root)
    if lr == rr or rr.startswith(lr + os.sep) or lr.startswith(rr + os.sep):
        log.warning("memory: the old store and the new memory folder overlap — nothing moved")
        return {"ok": False, "reason": "the old store and the memory folder overlap", **counts}
    started = time.monotonic()
    _private_dir(root)

    def move(src: Path, dst: Path, key: str) -> bool:
        if not (src.exists() or src.is_symlink()):
            return False
        if dst.exists() or dst.is_symlink():
            counts["skipped"] += 1
            log.warning("memory: migration skipped one %s entry: its target already exists", key)
            return False
        n = _file_count(src)
        try:
            _private_dir(dst.parent)
            os.rename(src, dst)
        except OSError as e:
            counts["failed"] += 1
            log.warning("memory: migration could not move one %s entry (%s)", key, type(e).__name__)
            return False
        counts[key] += n
        return True

    have_legacy = legacy.is_dir() and not legacy.is_symlink()
    if legacy.is_symlink():
        log.warning("memory: the old store is a symlink — it is left where it is")
    hold: set[str] = set()                     # entries that must stay put this run, for the retry
    if have_legacy:
        move(legacy / "records", cfg.records_dir, "records")
        highlights = legacy / "HIGHLIGHTS.md"
        if highlights.is_file():
            try:
                stars = _owner_stars(highlights.read_text(encoding="utf-8", errors="replace"))
            except OSError:
                stars = []
            if stars:
                rec = _records_mod()
                for claim, when in stars:
                    try:
                        if rec.star(cfg, claim, when):
                            counts["stars"] += 1
                    except Exception as e:  # noqa: BLE001
                        counts["failed"] += 1
                        hold.add(highlights.name)   # the next start reads the stars from here again
                        log.warning("memory: migration could not carry a star (%s)", type(e).__name__)
    if old_mem0.is_dir() and not old_mem0.is_symlink() and not _same(old_mem0, cfg.mem0_dir):
        move(old_mem0, cfg.mem0_dir, "mem0")
    if have_legacy:
        notes = legacy / "notes"
        if notes.is_dir() and not notes.is_symlink():
            meetings = cfg.transcripts_dir / MEETINGS_SUBDIR
            for entry in sorted(os.listdir(notes)):
                if _DAY.match(entry) and (notes / entry).is_dir():
                    move(notes / entry, meetings / entry, "meetings")
        for entry in sorted(os.listdir(legacy)):
            if entry not in hold:
                move(legacy / entry, cfg.archive_dir / entry, "archive")
    if (cfg.archive_dir / ".git").is_dir():
        try:
            _records_mod().seal(cfg.archive_dir)
        except Exception as e:  # noqa: BLE001
            log.warning("memory: could not seal the archive's repository (%s)", type(e).__name__)
    ms = int((time.monotonic() - started) * 1000)
    log.info("memory: migration moved %d record files, %d stars, %d mem0 files, %d meeting files, "
             "%d archive files; %d skipped, %d failed, in %d ms", counts["records"], counts["stars"],
             counts["mem0"], counts["meetings"], counts["archive"], counts["skipped"], counts["failed"], ms)
    if counts["failed"] == 0:
        try:
            body = json.dumps({**counts, "at": datetime.now().astimezone().isoformat(timespec="seconds")})
            fd = os.open(marker, os.O_WRONLY | os.O_CREAT | os.O_TRUNC | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600)
            try:
                os.write(fd, (body + "\n").encode("utf-8"))
            finally:
                os.close(fd)
        except OSError as e:
            log.warning("memory: could not write the migration marker (%s)", type(e).__name__)
    return {"ok": counts["failed"] == 0, "already": False, **counts}
