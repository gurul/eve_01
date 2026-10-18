"""Meet: eve joins a Google Meet call, listens, and texts the owner the notes.

The owner says ``/meet <link>`` on Telegram, "join my 3pm" in the chat, or "join my meeting" out loud. Eve
opens a tab of its own in the owner's Chrome (attach mode, so the owner's Google account and its meetings),
turns the microphone and the camera off, presses Join, reads Meet's live captions with the speakers' names,
and when the call ends — or the owner says leave — writes the notes under ``transcripts/meetings/<date>/``
and texts a summary.

### Listen only, for good

The owner chose a notetaker that never speaks (2026-09-25). So there is no audio path out at all, and the page
script (``meet_page.js``) can click only a short list of safe controls: microphone off, camera off, continue
without a microphone, Join now / Ask to join / Join here too, captions on, and Leave. It presses Join only once
it has SEEN both the microphone and the camera off, and in the call it turns either off again if it comes on.
The tab is kept quiet too: the call is not played out loud on the Mac, where the robot's own microphone is.

### The owner's account, twice

The owner may be in the same call from another computer with the same Google account. Meet then offers
"Join here too" and "Switch here". Eve presses only Join here too: Switch here moves the call to eve's tab
and drops the owner's own device.

### Captions, not audio

Meet's captions carry who said what, cost nothing, and need no audio device. The page only records what the
caption region shows (a snapshot per change); ``CaptionMerger`` here turns those snapshots into lines. That is
the hard part and it is plain Python, so it is tested without a browser: Meet shows a sliding window of a few
lines per speaker, revises the last words as it hears more, and re-renders the block now and then.

The transcript is appended to the notes file as lines settle, so a crash mid-call keeps what was heard.

The page script is a port of OpenClaw's google-meet plugin (MIT, Copyright (c) 2026 OpenClaw Foundation).
"""
from __future__ import annotations

import asyncio
import dataclasses
import difflib
import json
import logging
import os
import re
import time
import urllib.parse
from dataclasses import dataclass, field
from datetime import datetime, timedelta
from pathlib import Path
from typing import Any, Awaitable, Callable, Optional, Protocol

from . import spend
from .notes import _items

log = logging.getLogger(__name__)

MEET_DEFAULT = True                     # on wherever Telegram and the owner's Chrome (attach mode) are
MEET_HOST = "meet.google.com"
CODE = re.compile(r"^[a-z]{3}-[a-z]{4}-[a-z]{3}$")
PAGE_SCRIPT = (Path(__file__).with_name("meet_page.js")).read_text(encoding="utf-8")
DEFAULT_GUEST_NAME = "Eve"            # only asked for when Chrome is signed out; the owner's account has a name
DEFAULT_SUMMARY_MODEL = "gpt-6-luna"  # the room notes' summary model (notes.py)
JOIN_SECS = 90.0                        # from the page opening to being in the call or in the lobby
NO_CONTROLS_SECS = 12.0                 # no microphone/camera toggle shown this long: no device, join without
DEFAULT_LOBBY_MINUTES = 15.0
DEFAULT_MAX_HOURS = 4.0
JOIN_POLL_SECS = 1.0
LISTEN_POLL_SECS = 1.5
OUT_OF_CALL_POLLS = 3                   # "not in the call" this many polls running: it is over, not a redraw
PAGE_ERRORS = 5                         # the page failing this many polls running: the tab or Chrome is gone
NO_CAPTIONS_SECS = 60.0                 # in the call this long with no captions: tell the owner, once
SETTLE_SECS = 1.0                       # a caption block gone this long is finished (OpenClaw's settle window)
IDLE_COMMIT_SECS = 4.0                  # a block unchanged this long is written down (the speaker paused)
PARAGRAPH_WORDS = 60                    # a monologue is written down in paragraphs of about this many words
REVISED_TAIL_WORDS = 3                  # Meet revises at most this many of the last words it showed
CALENDAR_LOOKBACK = timedelta(minutes=10)
CALENDAR_AHEAD = timedelta(minutes=60)
TIME_SLACK = timedelta(minutes=20)      # "my 3pm" matches an event starting within this of 3pm
MAX_TRANSCRIPT_CHARS = 120_000          # sent to the summary model, at most (the file keeps everything)
OFF_REASON = "joining meetings is off on this computer (EVE_MEET, and the owner's Chrome: EVE_BROWSER_ATTACH)"

TOOLS: list[dict[str, Any]] = [
    {"type": "function", "name": "meet_join", "strict": True,
     "description": "Join a Google Meet call as a silent notetaker: microphone and camera off, captions read, "
                    "notes texted to the owner when it ends. Returns at once; the joining carries on.",
     "parameters": {"type": "object", "additionalProperties": False, "required": ["meeting"],
                    "properties": {"meeting": {
                        "type": "string",
                        "description": "A meet.google.com link or meeting code when you have one; otherwise the "
                                       "owner's words for which meeting on their calendar: \"now\", \"next\", "
                                       "a time like \"3pm\", or words from its title."}}}},
    {"type": "function", "name": "meet_status", "strict": True,
     "description": "Whether eve is in a meeting now: which, since when, how many lines it has heard.",
     "parameters": {"type": "object", "additionalProperties": False, "required": [], "properties": {}}},
    {"type": "function", "name": "meet_leave", "strict": True,
     "description": "Leave the meeting eve is in; the notes are written and texted.",
     "parameters": {"type": "object", "additionalProperties": False, "required": [], "properties": {}}},
]
TOOL_NAMES = tuple(t["name"] for t in TOOLS)

INSTRUCTIONS_BLOCK = """You can sit in on the owner's Google Meet calls as a silent notetaker with meet_join:
eve joins from the owner's Chrome with the microphone and camera off, never speaks, reads the captions, and
texts the notes when the call ends. Pass the meet.google.com link when you have it. Otherwise pass the owner's
own words for which meeting ("now", "next", "3pm", "the standup"): eve finds its link on their calendar
itself. meet_status says whether eve is in a call; meet_leave leaves and sends the notes. Joining can take a
minute and may wait in the lobby until someone lets eve in: tell the owner it is on its way, not that it is
in."""


# ---- links ----------------------------------------------------------------------------------------

def parse_link(text: str) -> tuple[Optional[str], str]:
    """The Meet link in ``text`` (a link, a link without https, or a bare meeting code), normalised to
    ``https://meet.google.com/<code>?hl=en`` with ``authuser`` kept; or (None, why not).

    English is forced because the page script reads English labels (OpenClaw does the same). Only
    meet.google.com itself, over https, with a meeting code or a lookup path: a lookalike host, another scheme
    or another site is refused, so a message cannot send eve's tab anywhere else."""
    for raw in (text or "").split():
        token = raw.strip("<>()[]{}\"'.,;!")
        if not token:
            continue
        if CODE.match(token.lower()):
            return f"https://{MEET_HOST}/{token.lower()}?hl=en", ""
        low = token.lower()
        if low.startswith(MEET_HOST + "/"):
            token = "https://" + token
        elif not re.match(r"^[a-z][a-z0-9+.-]*://", low):
            continue
        try:
            u = urllib.parse.urlsplit(token)
        except ValueError:
            continue
        if (u.hostname or "").lower() != MEET_HOST:
            continue
        if u.scheme.lower() != "https":
            return None, "only an https Meet link is joined"
        if u.username or u.password or u.port not in (None, 443):
            return None, "that is not a plain Meet link"
        path = u.path.rstrip("/")
        parts = [p for p in path.split("/") if p]
        if len(parts) == 1 and CODE.match(parts[0].lower()):
            path = "/" + parts[0].lower()
        elif len(parts) == 2 and parts[0].lower() == "lookup" and re.fullmatch(r"[A-Za-z0-9_-]{3,64}", parts[1]):
            path = "/lookup/" + parts[1]
        else:
            return None, "that Meet link has no meeting code in it"
        query = [(k, v) for k, v in urllib.parse.parse_qsl(u.query) if k == "authuser" and re.fullmatch(r"[\w.@+-]{1,80}", v)]
        query.append(("hl", "en"))
        return f"https://{MEET_HOST}{path}?{urllib.parse.urlencode(query)}", ""
    return None, "no Google Meet link or meeting code in that"


def meeting_code(url: str) -> str:
    path = urllib.parse.urlsplit(url).path.strip("/")
    return path.split("/")[-1] if path else "the meeting"


# ---- captions ------------------------------------------------------------------------------------

def _key(word: str) -> str:
    return re.sub(r"[^\w]+", "", word.lower())


def merge(previous: str, new: str) -> Optional[str]:
    """``new`` as a continuation of ``previous``, or None when it is a new utterance.

    Meet shows a window of a speaker's turn that grows at the end, drops words at the front as it scrolls, and
    rewrites words anywhere in it as it hears more (live, 2026-09-25: "He will most likely" became "We will
    most likely", and "it's quite" became "it's. It's quite"). So the two are aligned word by word
    (difflib): when enough of them agree, ``new`` is the same turn, and the result is whatever ``previous``
    had before the point where ``new`` begins, then ``new`` itself — Meet's latest reading wins. When too
    little agrees, it is a new utterance."""
    pt, nt = previous.split(), new.split()
    pk, nk = [_key(w) for w in pt], [_key(w) for w in nt]
    if not nt:
        return previous
    if not pt:
        return new
    blocks = [bl for bl in difflib.SequenceMatcher(None, pk, nk, autojunk=False).get_matching_blocks() if bl.size]
    shorter = min(len(pk), len(nk))
    agreed = sum(bl.size for bl in blocks)
    # one or two short words in common is chance, not the same turn
    if not blocks or agreed < min(2, shorter) or agreed * 2 < shorter:
        return None
    first = blocks[0]
    if agreed == len(nk) and len(blocks) == 1:
        return previous                            # nothing new: the window only scrolled or shrank
    # ``new`` begins ``first.b`` words before its first agreement: that many of previous's words are replaced
    return " ".join(pt[:max(0, first.a - first.b)] + nt)


@dataclass
class Line:
    speaker: str
    text: str
    at: float                                      # epoch seconds, when the words were first shown


@dataclass
class _Open:
    speaker: str
    text: str
    first: float
    last: float
    emitted: int = 0                               # words of ``text`` already written down


class CaptionMerger:
    """Snapshots of the caption region in, settled lines out. A snapshot is ``{"t": ms, "rows": [{"id",
    "speaker", "text", "self"}]}``: one row per visible caption block, ``id`` stable while Meet keeps the
    block's element. Eve's own tile (``self``) is never transcribed."""

    def __init__(self) -> None:
        self.open: dict[int, _Open] = {}
        self.now = 0.0

    def _emit(self, entry: _Open, at: float, out: list[Line], final: bool = False) -> None:
        words = entry.text.split()
        rest = words[entry.emitted:]
        if not rest:
            return
        if not final and len(rest) > PARAGRAPH_WORDS:
            rest = rest[:PARAGRAPH_WORDS]
        entry.emitted += len(rest)
        out.append(Line(entry.speaker, " ".join(rest), at))

    def feed(self, snap: dict[str, Any]) -> list[Line]:
        out: list[Line] = []
        t = float(snap.get("t") or 0) / 1000.0 or time.time()
        self.now = max(self.now, t)
        seen: set[int] = set()
        for row in snap.get("rows") or []:
            if not isinstance(row, dict) or row.get("self"):
                continue
            try:
                rid = int(row.get("id"))
            except (TypeError, ValueError):
                continue
            speaker = " ".join(str(row.get("speaker") or "").split())[:80]
            text = " ".join(str(row.get("text") or "").split())
            if not text:
                continue
            seen.add(rid)
            cur = self.open.get(rid)
            if cur is None:
                # a re-rendered block: the same speaker's open line under a new element
                for oid, other in list(self.open.items()):
                    if oid not in seen and other.speaker == speaker and t - other.last <= SETTLE_SECS * 3:
                        if merge(other.text, text) is not None:
                            cur = self.open.pop(oid)
                            self.open[rid] = cur
                            break
            if cur is not None and cur.speaker != speaker:
                self._emit(cur, cur.first, out, final=True)
                cur = None
            if cur is None:
                self.open[rid] = _Open(speaker, text, t, t)
                continue
            merged = merge(cur.text, text)
            if merged is None:
                self._emit(cur, cur.first, out, final=True)
                self.open[rid] = _Open(speaker, text, t, t)
                continue
            if merged != cur.text:
                cur.text, cur.last = merged, t
            if len(cur.text.split()) - cur.emitted > PARAGRAPH_WORDS + REVISED_TAIL_WORDS:
                self._emit(cur, cur.first, out)
        for rid in list(self.open):
            if rid not in seen and t - self.open[rid].last >= SETTLE_SECS:
                self._emit(self.open.pop(rid), 0, out, final=True)
        return self._stamp(out)

    def tick(self, now: float) -> list[Line]:
        """Between snapshots: a block gone or unchanged long enough is written down."""
        out: list[Line] = []
        for entry in self.open.values():
            if now - entry.last >= IDLE_COMMIT_SECS:
                self._emit(entry, 0, out, final=True)
        return self._stamp(out)

    def finish(self) -> list[Line]:
        out: list[Line] = []
        for entry in self.open.values():
            self._emit(entry, 0, out, final=True)
        self.open.clear()
        return self._stamp(out)

    def _stamp(self, lines: list[Line]) -> list[Line]:
        for ln in lines:
            if not ln.at:
                ln.at = self.now
        return lines


# ---- the calendar --------------------------------------------------------------------------------

def events_in(obj: Any, depth: int = 0) -> list[dict[str, Any]]:
    """Every calendar event in a reply, however deep it is nested (the MCP result wraps Google's list)."""
    if depth > 6:
        return []
    if isinstance(obj, dict):
        if isinstance(obj.get("start"), dict) and ("summary" in obj or "hangoutLink" in obj or "conferenceData" in obj):
            return [obj]
        out: list[dict[str, Any]] = []
        for v in obj.values():
            out.extend(events_in(v, depth + 1))
        return out
    if isinstance(obj, list):
        out = []
        for v in obj:
            out.extend(events_in(v, depth + 1))
        return out
    if isinstance(obj, str) and obj.lstrip().startswith(("{", "[")) and depth < 3:
        try:
            return events_in(json.loads(obj), depth + 1)
        except ValueError:
            return []
    return []


def event_link(event: dict[str, Any]) -> Optional[str]:
    candidates = [str(event.get("hangoutLink") or "")]
    for ep in ((event.get("conferenceData") or {}).get("entryPoints") or []):
        if isinstance(ep, dict) and ep.get("entryPointType") == "video":
            candidates.append(str(ep.get("uri") or ""))
    candidates.append(str(event.get("location") or ""))
    candidates.append(str(event.get("description") or "")[:4000])
    for c in candidates:
        link, _ = parse_link(c)
        if link:
            return link
    return None


def _when(value: Any) -> Optional[datetime]:
    if not isinstance(value, dict) or not value.get("dateTime"):
        return None                                 # an all-day event has no call time
    try:
        dt = datetime.fromisoformat(str(value["dateTime"]).replace("Z", "+00:00"))
    except ValueError:
        return None
    return dt if dt.tzinfo else dt.astimezone()


# (?!\w), not \b, at the end: after "p.m." there is no word boundary, so \b made the dotted forms never match
_CLOCK = re.compile(r"\b(\d{1,2})(?::(\d{2}))?\s*(am|pm|a\.m\.|p\.m\.)?(?!\w)", re.I)
_NOW_WORDS = re.compile(r"^\s*(?:(?:join|my|the|a|this|current|meeting|call|meet|now|right now|in progress)\s*)*$", re.I)


def find_meeting(events: list[dict[str, Any]], words: str, now: datetime) -> tuple[Optional[dict[str, Any]], str, str]:
    """(event, link, "") for the meeting the owner means, or (None, "", why not).

    "now" or nothing: one in progress, else the next within the hour. "next": the soonest starting from ten
    minutes ago. A time ("3pm", "15:30"): the one starting nearest it today. Other words: today's event whose
    title has them. Only events with a Meet link count."""
    now = now if now.tzinfo else now.astimezone()
    timed = []
    for e in events:
        start, end = _when(e.get("start")), _when(e.get("end"))
        link = event_link(e)
        if start is None or link is None or str(e.get("status") or "") == "cancelled":
            continue
        timed.append((start, end or start + timedelta(hours=1), e, link))
    timed.sort(key=lambda x: x[0])
    if not timed:
        return None, "", "no meeting with a Google Meet link on the calendar around then"
    w = (words or "").strip().lower()
    if re.search(r"\bnext\b", w):
        for start, _end, e, link in timed:
            if start >= now - CALENDAR_LOOKBACK:
                return e, link, ""
        return None, "", "no upcoming meeting with a Meet link"
    clock = _CLOCK.search(w)
    if clock and (clock.group(2) or clock.group(3) or re.search(r"\b(at|my)\s+\d", w)):
        hour, minute = int(clock.group(1)), int(clock.group(2) or 0)
        ampm = (clock.group(3) or "").replace(".", "").lower()
        if ampm == "pm" and hour < 12:
            hour += 12
        elif ampm == "am" and hour == 12:
            hour = 0
        elif not ampm and 1 <= hour <= 7:
            hour += 12                              # "my 3" at work means the afternoon
        if hour > 23 or minute > 59:
            return None, "", "that time is not a time"
        target = now.replace(hour=hour, minute=minute, second=0, microsecond=0)
        best = min(timed, key=lambda x: abs(x[0] - target))
        if abs(best[0] - target) <= TIME_SLACK:
            return best[2], best[3], ""
        return None, "", f"no meeting with a Meet link near {target:%-I:%M %p}".replace(":00 ", " ")
    if not w or _NOW_WORDS.match(w):
        for start, end, e, link in timed:
            if start - CALENDAR_LOOKBACK <= now < end:
                return e, link, ""
        for start, _end, e, link in timed:
            if now <= start <= now + CALENDAR_AHEAD:
                return e, link, ""
        return None, "", "no meeting with a Meet link is on now or in the next hour"
    keys = [k for k in re.findall(r"[a-z0-9]+", w) if k not in ("join", "my", "the", "meeting", "call", "meet", "with", "a", "to")]
    for start, _end, e, link in timed:
        title = str(e.get("summary") or "").lower()
        if keys and all(k in title for k in keys) and start.date() == now.date():
            return e, link, ""
    return None, "", "no meeting today whose title matches that"


# ---- the page ------------------------------------------------------------------------------------

class MeetPage(Protocol):
    async def open(self, url: str) -> None: ...
    async def run(self, opts: dict[str, Any]) -> dict[str, Any]: ...
    async def close(self) -> None: ...


class ChromeMeetPage:
    """Eve's own tab in the owner's Chrome, on a browser lane of its own (browser_lane.BrowserLane in attach
    mode): a web task on the shared lane never takes the meeting's tab. ``answer`` presses Chrome's "Allow
    remote debugging?" for this connection (chrome_consent.ConsentBroker.answer_own_connection)."""

    def __init__(self, lane: Any, answer: Optional[Callable[[], Awaitable[Any]]] = None) -> None:
        self._lane = lane
        self._answer = answer

    async def open(self, url: str) -> None:
        await self._lane.connect(self._answer)
        await self._lane.open_url(url)

    def _run(self, opts: dict[str, Any]) -> dict[str, Any]:
        p = self._lane._page
        if p is None or p.page.is_closed():
            return {"closed": True}                  # never reopened: a closed tab is the owner leaving
        out = p.page.evaluate(PAGE_SCRIPT, opts)
        return out if isinstance(out, dict) else {}

    async def run(self, opts: dict[str, Any]) -> dict[str, Any]:
        return await self._lane._run(self._run, opts)

    async def close(self) -> None:
        await self._lane.close()                     # eve's tab only; the owner's Chrome stays


# ---- the notes -----------------------------------------------------------------------------------

SUMMARY_PROMPT = """You are writing up notes from a video call that someone's assistant sat in on. You are
given the call's live captions, one line per speaker turn, as "Speaker: words". Captions are automatic, so
expect wrong words and missing punctuation; the speaker names come from the call and are reliable.

Write the notes a competent person would write:
- A title of six words or fewer, naming the actual subject, the way someone labels a file to find it again.
- The gist: what this call was, in one or two sentences.
- The points that carry information. Merge repetition. Drop greetings and scheduling chatter.
- Decisions, only where something was actually decided, with who decided when the captions show it.
- Actions, each one a thing somebody is meant to do, with the person's name when the captions give it.
- Open questions: what was raised and not resolved.
- Anything you could not make out, said plainly, rather than a confident guess.

Every list may be empty; do not pad one. Output the JSON the schema asks for."""

# The call's own shape: room and phone notes moved to the detailed one (notes.SUMMARY_SCHEMA, 2026-10-04), and the
# Meet notes and their message (summary_message) keep this one.
_LIST = {"type": "array", "items": {"type": "string"}}
MEET_SCHEMA = {
    "type": "object",
    "additionalProperties": False,
    "properties": {"title": {"type": "string"}, "gist": {"type": "string"}, "points": _LIST, "decisions": _LIST,
                   "actions": _LIST, "questions": _LIST, "unclear": _LIST},
    "required": ["title", "gist", "points", "decisions", "actions", "questions", "unclear"],
}


def make_summarizer(environ: Any = None) -> Optional[Callable[[str], dict[str, Any]]]:
    env = os.environ if environ is None else environ
    from . import openrouter

    key = openrouter.key(env)
    if not key:
        log.warning("meet: OPENROUTER_API_KEY is not set — notes are written without a summary")
        return None
    model = (env.get("EVE_MEET_SUMMARY_MODEL") or DEFAULT_SUMMARY_MODEL).strip() or DEFAULT_SUMMARY_MODEL
    def summarize(transcript: str) -> dict[str, Any]:
        client = openrouter.client(key)
        resp = client.responses.create(
            model=model, instructions=SUMMARY_PROMPT, input=transcript[-MAX_TRANSCRIPT_CHARS:],
            text={"format": {"type": "json_schema", "name": "notes", "strict": True, "schema": MEET_SCHEMA}},
            max_output_tokens=2500, reasoning={"effort": "low"})
        spend.record_response(spend.MEET, resp, model=model)
        return json.loads((resp.output_text or "").strip() or "{}")

    return summarize


def line_text(ln: Line) -> str:
    when = datetime.fromtimestamp(ln.at).strftime("%H:%M") if ln.at else ""
    who = ln.speaker or "Someone"
    return f"- **{who}**{f' ({when})' if when else ''}: {ln.text}"


class NotesFile:
    """One file per call, 0600 in 0700 folders, under ``transcripts/meetings/<date>/`` where memory_search finds
    it. Created when the call starts; the transcript is appended as lines settle; at the end it is rewritten
    with the notes above the transcript. Never written over another call's file."""

    def __init__(self, root: Path, started: datetime, code: str) -> None:
        self.started = started
        self.lines: list[Line] = []
        day = root / f"{started:%Y-%m-%d}"
        day.mkdir(parents=True, exist_ok=True)
        for d in (root.parent, root, day):
            try:
                os.chmod(d, 0o700)
            except OSError:
                pass
        stem = f"{started:%H%M}-meet-{re.sub(r'[^a-z0-9-]+', '', code.lower()) or 'call'}"
        for n in range(1, 51):
            path = day / (f"{stem}.md" if n == 1 else f"{stem}-{n}.md")
            try:
                fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600)
            except FileExistsError:
                continue
            with os.fdopen(fd, "w", encoding="utf-8") as f:
                f.write(self._head("in progress", code) + "## Transcript\n\n")
            self.path = path
            self.code = code
            return
        raise OSError("fifty calls already share this minute")

    def _head(self, status: str, code: str, title: str = "") -> str:
        return (f"---\nstatus: {status}\nsource: eve-meet\nmeeting: {code}\n"
                f"started: {self.started:%Y-%m-%d %H:%M}\n---\n\n# {title or 'Call ' + code}\n\n")

    def append(self, lines: list[Line]) -> None:
        if not lines:
            return
        self.lines.extend(lines)
        with open(self.path, "a", encoding="utf-8") as f:
            f.write("".join(line_text(ln) + "\n" for ln in lines))

    def transcript(self) -> str:
        return "\n".join(f"{ln.speaker or 'Someone'}: {ln.text}" for ln in self.lines)

    def finish(self, summary: dict[str, Any], label: str, ended: str) -> None:
        title = str(summary.get("title") or label).strip().rstrip(".")
        parts = [self._head("notes", self.code, title).rstrip("\n").replace(
            "status: notes\n", f"status: notes\nended: {datetime.now():%Y-%m-%d %H:%M} ({ended})\n", 1), ""]
        gist = str(summary.get("gist") or "").strip()
        if gist:
            parts += [gist, ""]
        for heading, key in (("Points", "points"), ("Decisions", "decisions"), ("Actions", "actions"),
                             ("Open questions", "questions"), ("Not made out", "unclear")):
            rows = _items(summary.get(key))
            if rows:
                parts += [f"## {heading}", ""] + [f"- {r}" for r in rows] + [""]
        if not summary and self.lines:
            parts += ["> The write-up failed, so this is the transcript alone.", ""]
        parts += ["## Transcript", "", "<!-- Meet's live captions: expect wrong words; the names are Meet's -->", ""]
        parts += [line_text(ln) for ln in self.lines] or ["(nothing was said while eve was in the call)"]
        tmp = self.path.with_suffix(".tmp")
        fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600)
        with os.fdopen(fd, "w", encoding="utf-8") as f:
            f.write("\n".join(parts) + "\n")
        os.replace(tmp, self.path)


def summary_message(label: str, ended: str, summary: dict[str, Any], lines: int, path: Optional[Path]) -> str:
    if not lines:
        return f"{label} is over ({ended}). Nothing was said while I was in it, so there are no notes."
    title = str(summary.get("title") or "").strip()
    parts = [f"Notes from {label}" + (f": {title}" if title and title != label else "") + f" ({ended})."]
    gist = str(summary.get("gist") or "").strip()
    if gist:
        parts.append(gist)
    for heading, key in (("Decisions", "decisions"), ("Actions", "actions"), ("Open questions", "questions")):
        rows = _items(summary.get(key), 8)
        if rows:
            parts.append(heading + ":\n" + "\n".join(f"• {r}" for r in rows))
    if not summary:
        parts.append(f"The write-up failed; the transcript ({lines} lines) is saved.")
    if path is not None:
        parts.append(f"Saved as {path.name}.")
    return "\n\n".join(parts)


# ---- one call ------------------------------------------------------------------------------------

Notify = Callable[[str, str], Awaitable[Any]]    # (text, about): see MeetSession._tell


@dataclass(frozen=True)
class MeetConfig:
    enabled: bool = MEET_DEFAULT
    profile: str = ""                   # the Google account whose Chrome profile joins ("": the lane's default)
    lobby_minutes: float = DEFAULT_LOBBY_MINUTES
    max_hours: float = DEFAULT_MAX_HOURS
    quiet: bool = True                  # the call is not played out loud on the Mac
    guest_name: str = DEFAULT_GUEST_NAME


def configured(environ: Any = None) -> MeetConfig:
    env = os.environ if environ is None else environ

    def num(name: str, default: float, lo: float, hi: float) -> float:
        try:
            return min(hi, max(lo, float(str(env.get(name) or default))))
        except ValueError:
            return default

    on = (env.get("EVE_MEET") or ("1" if MEET_DEFAULT else "0")).strip().lower() in ("1", "true", "yes", "on")
    return MeetConfig(enabled=on, profile=(env.get("EVE_MEET_PROFILE") or "").strip().lower(),
                      lobby_minutes=num("EVE_MEET_LOBBY_MINUTES", DEFAULT_LOBBY_MINUTES, 1, 120),
                      max_hours=num("EVE_MEET_MAX_HOURS", DEFAULT_MAX_HOURS, 0.25, 12),
                      quiet=(env.get("EVE_MEET_QUIET") or "1").strip().lower() not in ("0", "false", "no", "off"),
                      guest_name=(env.get("EVE_MEET_GUEST_NAME") or DEFAULT_GUEST_NAME).strip()[:40])


# The history's lines for the texts that name the meeting (meet._tell's ``about``): code's own words, never the
# label, which can be a calendar invite's title (review, round 4).
ABOUT_IN = "I texted the owner that I am in the meeting, microphone and camera off, taking notes."
ABOUT_LOBBY = "I texted the owner that I am waiting in the meeting's lobby for someone to admit me."
ABOUT_CAPTIONS = "I texted the owner that the meeting's captions won't turn on, so I can't take notes there yet."
ABOUT_JOINING = "I told the owner I am joining the meeting with the microphone and camera off."
ABOUT_NOT_JOINING = "I told the owner I could not join that meeting."


@dataclass
class SessionState:
    phase: str = "joining"              # joining → lobby → in call → done
    joined_at: Optional[float] = None
    ended: str = ""
    lines: int = 0
    notes: Optional[Path] = None
    clicks: list[str] = field(default_factory=list)


class MeetSession:
    """One call, from opening the tab to the notes. Everything outside is lent — the page, the clock, the sleep,
    the summary model, and ``notify`` (a text to the owner) — so the whole state machine runs in tests."""

    def __init__(self, url: str, label: str, page: MeetPage, notes_root: Path, notify: Notify, *,
                 config: MeetConfig = MeetConfig(), summarize: Optional[Callable[[str], dict[str, Any]]] = None,
                 clock: Callable[[], float] = time.monotonic, sleep: Callable[[float], Awaitable[Any]] = asyncio.sleep,
                 wall: Callable[[], datetime] = datetime.now, epoch: Callable[[], float] = time.time) -> None:
        self.url, self.label, self.page = url, label, page
        self.epoch = epoch                             # the page's clock (Date.now) is the Mac's wall clock
        self.notes_root, self.notify, self.config = notes_root, notify, config
        self.summarize = summarize
        self.clock, self.sleep, self.wall = clock, sleep, wall
        self.state = SessionState()
        self.started_at = clock()
        self.leave_asked = asyncio.Event()
        self.merger = CaptionMerger()
        self.file: Optional[NotesFile] = None
        self._cursor = 0

    def status(self) -> dict[str, Any]:
        s = self.state
        out: dict[str, Any] = {"meeting": self.label, "phase": s.phase, "lines_heard": s.lines}
        if s.joined_at is not None:
            out["minutes_in_call"] = round((self.clock() - s.joined_at) / 60.0, 1)
        if s.ended:
            out["ended"] = s.ended
        return out

    def leave(self) -> None:
        self.leave_asked.set()

    async def _tell(self, text: str, about: str = "") -> None:
        """A text to the owner. ``about`` is what the chat's history keeps instead, for a text carrying words from
        the call (the summary): what was said in a meeting is never read back to the brain as eve's own words."""
        try:
            await self.notify(text, about)
        except Exception:  # noqa: BLE001 — a text that did not go costs the text, never the call
            log.exception("meet: could not text the owner")

    async def _poll(self, opts: dict[str, Any]) -> dict[str, Any]:
        st = await self.page.run(opts)
        self.state.clicks.extend(st.get("clicked") or [])
        return st

    async def run(self) -> SessionState:
        try:
            await self.page.open(self.url)
            if await self._join():
                await self._listen()
        except asyncio.CancelledError:
            self.state.ended = self.state.ended or "eve stopped"
            raise
        except Exception as e:  # noqa: BLE001 — Chrome refused, the tab died: the owner is told why
            log.warning("meet: %s failed: %s", self.label, e)
            self.state.ended = self.state.ended or f"it failed: {e}"[:200]
        finally:
            await asyncio.shield(self._finish())
        return self.state

    async def _join(self) -> bool:
        errors, lobby_since, told_lobby = 0, None, False
        last: dict[str, Any] = {}
        while True:
            if self.leave_asked.is_set():
                self.state.ended = "you asked me to leave before I was in"
                return False
            now = self.clock()
            try:
                st = await self._poll({"act": True, "quiet": self.config.quiet, "captions": False,
                                       "guestName": self.config.guest_name,
                                       "noControlsOk": now - self.started_at >= NO_CONTROLS_SECS})
                errors = 0
            except Exception as e:  # noqa: BLE001
                errors += 1
                if errors >= PAGE_ERRORS:
                    raise RuntimeError(f"the Meet tab stopped answering ({type(e).__name__})") from None
                await self.sleep(JOIN_POLL_SECS)
                continue
            last = st
            if st.get("closed"):
                self.state.ended = "the tab was closed"
                return False
            if st.get("inCall"):
                self.state.phase, self.state.joined_at = "in call", self.clock()
                self.file = NotesFile(self.notes_root, self.wall(), meeting_code(self.url))
                self.state.notes = self.file.path
                # the label can be an invite's title (outside words): every text naming it leaves the history a
                # line of code's own (``about``), so it never comes back to the brain as eve's own words
                await self._tell(f"I'm in {self.label}: microphone and camera off, taking notes. "
                                 "Say /meet leave when you want me out.", about=ABOUT_IN)
                return True
            if st.get("signIn"):
                self.state.ended = "Chrome's Google account is signed out, so Meet asked me to sign in"
                return False
            if st.get("ended"):
                self.state.ended = f"Meet said: {st['ended']}"
                return False
            if st.get("lobby"):
                self.state.phase = "lobby"
                if lobby_since is None:
                    lobby_since = now
                if not told_lobby:
                    told_lobby = True
                    await self._tell(f"I'm waiting to be let into {self.label}. Someone in the call has to admit me.",
                                     about=ABOUT_LOBBY)
                if now - lobby_since >= self.config.lobby_minutes * 60:
                    self.state.ended = f"nobody let me in within {self.config.lobby_minutes:g} minutes"
                    await self._leave_page()
                    return False
            elif now - self.started_at >= JOIN_SECS:
                self.state.ended = self._why_not_in(last)
                return False
            await self.sleep(JOIN_POLL_SECS)

    @staticmethod
    def _why_not_in(st: dict[str, Any]) -> str:
        if st.get("switchHere") and not st.get("joinHereToo"):
            return "Meet only offered to move the call off your other device, and I won't take it from you"
        if st.get("permission"):
            return "Meet is asking for a microphone or camera permission I won't give"
        if st.get("micOn") or st.get("camOn"):
            return "I couldn't turn the microphone and camera off, so I didn't join"
        return "the Join button never came up"

    async def _listen(self) -> None:
        errors, out_polls, told_captions = 0, 0, False
        deadline = self.state.joined_at + self.config.max_hours * 3600 if self.state.joined_at else None
        while True:
            now = self.clock()
            if self.leave_asked.is_set():
                self.state.ended = "you asked me to leave"
                await self._leave_page()
                return
            if deadline is not None and now >= deadline:
                self.state.ended = f"it ran past {self.config.max_hours:g} hours"
                await self._leave_page()
                return
            try:
                st = await self._poll({"act": True, "quiet": self.config.quiet, "captions": True,
                                       "since": self._cursor})
                errors = 0
            except Exception as e:  # noqa: BLE001
                errors += 1
                if errors >= PAGE_ERRORS:
                    self.state.ended = f"the Meet tab stopped answering ({type(e).__name__})"
                    return
                await self.sleep(LISTEN_POLL_SECS)
                continue
            if st.get("closed"):
                self.state.ended = "the tab was closed"
                return
            for snap in st.get("snaps") or []:
                self._cursor = max(self._cursor, int(snap.get("seq") or 0))
                self._write(self.merger.feed(snap))
            self._write(self.merger.tick(self.epoch()))
            if st.get("inCall"):
                out_polls = 0
            else:
                out_polls += 1
                if out_polls >= OUT_OF_CALL_POLLS or st.get("ended"):
                    self.state.ended = f"Meet said: {st['ended']}" if st.get("ended") else "the call ended"
                    return
            if (not told_captions and not st.get("captionsOn") and self.state.lines == 0
                    and self.state.joined_at is not None and now - self.state.joined_at >= NO_CAPTIONS_SECS):
                told_captions = True
                await self._tell(f"Captions won't turn on in {self.label}, so I can't take notes there yet. "
                                 "Captions may be off for this meeting or its language.", about=ABOUT_CAPTIONS)
            await self.sleep(LISTEN_POLL_SECS)

    def _write(self, lines: list[Line]) -> None:
        if lines and self.file is not None:
            self.file.append(lines)
            self.state.lines += len(lines)

    async def _leave_page(self) -> None:
        for _ in range(3):
            try:
                st = await self._poll({"leave": True})
            except Exception:  # noqa: BLE001
                return
            if not st.get("inCall"):
                return
            await self.sleep(0.5)

    async def _finish(self) -> None:
        self.state.phase = "done"
        ended = self.state.ended or "the call ended"
        if self.file is not None:
            self._write(self.merger.finish())
            summary: dict[str, Any] = {}
            if self.file.lines and self.summarize is not None:
                try:
                    summary = await asyncio.to_thread(self.summarize, self.file.transcript())
                except Exception as e:  # noqa: BLE001
                    log.warning("meet: the summary failed (%s)", type(e).__name__)
            try:
                self.file.finish(summary, self.label, ended)
            except OSError as e:
                log.warning("meet: could not write the notes (%s)", type(e).__name__)
            await self._tell(summary_message(self.label, ended, summary, len(self.file.lines), self.file.path),
                             about=f"I texted the owner the meeting's notes ({ended}); they are saved as "
                                   f"{self.file.path.name} in the meeting notes, where memory_search finds them.")
        else:
            # the label is an invite's title (outside words): the history gets a line of code's own (v3 review)
            await self._tell(f"I couldn't join {self.label}: {ended}.",
                             about="I texted the owner that I could not join the meeting.")
        try:
            await self.page.close()
        except Exception:  # noqa: BLE001
            pass


# ---- the owner's side ----------------------------------------------------------------------------

class Meeter:
    """At most one call at a time, and the tools the chat and the voice use to start and stop it.

    ``make_page`` opens a fresh tab connection per call; ``list_events`` reads the owner's calendar (a blocking
    call, run on a thread) for "join my 3pm"; ``notify`` texts the owner (telegram.TelegramInlet.tell_owner)."""

    def __init__(self, config: MeetConfig, make_page: Callable[[], MeetPage], notes_root: Path, *,
                 list_events: Optional[Callable[[datetime, datetime], Any]] = None,
                 summarize: Optional[Callable[[str], dict[str, Any]]] = None,
                 notify: Optional[Notify] = None, clock: Callable[[], float] = time.monotonic,
                 sleep: Callable[[float], Awaitable[Any]] = asyncio.sleep,
                 wall: Callable[[], datetime] = datetime.now, epoch: Callable[[], float] = time.time) -> None:
        self.config = config
        self.make_page = make_page
        self.notes_root = notes_root
        self.list_events = list_events
        self.summarize = summarize
        self.notify: Notify = notify or _no_notify
        self.clock, self.sleep, self.wall, self.epoch = clock, sleep, wall, epoch
        self.session: Optional[MeetSession] = None
        self.task: Optional[asyncio.Task[Any]] = None

    @property
    def busy(self) -> bool:
        return self.task is not None and not self.task.done()

    def instructions(self) -> str:
        return INSTRUCTIONS_BLOCK

    def tools(self) -> list[dict[str, Any]]:
        return json.loads(json.dumps(TOOLS))

    async def resolve(self, meeting: str) -> tuple[Optional[str], str, str]:
        """(link, label, "") or (None, "", why not): a link or code in the words, else the calendar."""
        link, why = parse_link(meeting)
        if link:
            return link, meeting_code(link), ""
        if why and why != "no Google Meet link or meeting code in that":
            return None, "", why
        if self.list_events is None:
            return None, "", "send me the meet.google.com link: I can't read your calendar from here"
        now = self.wall().astimezone()
        day_start = now.replace(hour=0, minute=0, second=0, microsecond=0)
        try:
            reply = await asyncio.to_thread(self.list_events, day_start, day_start + timedelta(days=1))
        except Exception as e:  # noqa: BLE001
            return None, "", f"I couldn't read your calendar ({type(e).__name__})"
        event, link, why = find_meeting(events_in(reply), meeting, now)
        if event is None:
            return None, "", why
        return link, str(event.get("summary") or meeting_code(link)).strip()[:80], ""

    async def join(self, meeting: str) -> dict[str, Any]:
        if not self.config.enabled:
            return {"ok": False, "reason": OFF_REASON}
        if self.busy and self.session is not None:
            return {"ok": False, "reason": f"I'm already in {self.session.label}; leave it first"}
        link, label, why = await self.resolve(meeting)
        if link is None:
            return {"ok": False, "reason": why}
        if self.busy:                                   # another join started while the calendar was read
            return {"ok": False, "reason": "I'm already joining a meeting"}
        self.session = MeetSession(link, label, self.make_page(), self.notes_root, self.notify, config=self.config,
                                   summarize=self.summarize, clock=self.clock, sleep=self.sleep, wall=self.wall,
                                   epoch=self.epoch)
        self.task = asyncio.create_task(self.session.run(), name="meet")
        log.info("meet: joining %s", meeting_code(link))
        return {"ok": True, "joining": label, "link": link,
                "note": "joining now with the microphone and camera off; the owner is texted when eve is in"}

    def status(self) -> dict[str, Any]:
        if self.session is None:
            return {"ok": True, "in_meeting": False}
        out = {"ok": True, "in_meeting": self.busy, **self.session.status()}
        if self.session.state.notes is not None:
            out["notes_file"] = self.session.state.notes.name
        return out

    def leave(self) -> dict[str, Any]:
        if not self.busy or self.session is None:
            return {"ok": False, "reason": "I'm not in a meeting"}
        self.session.leave()
        return {"ok": True, "leaving": self.session.label}

    def status_line(self) -> str:
        """The bare ``/meet`` answer, by code."""
        if not self.config.enabled:
            return "Joining meetings is off on this computer (EVE_MEET)."
        if not self.busy or self.session is None:
            return ("I'm not in a meeting. Send /meet with a Google Meet link, or say which one "
                    "(\"/meet now\", \"/meet 3pm\"), and I'll join muted with the camera off and take notes.")
        s = self.session.status()
        where = {"joining": "joining", "lobby": "waiting in the lobby of", "in call": "in"}.get(s["phase"], s["phase"])
        extra = (f", {s.get('minutes_in_call', 0):g} min, {s['lines_heard']} lines heard"
                 if s["phase"] == "in call" else "")
        return f"I'm {where} {s['meeting']}{extra}. /meet leave takes me out."

    async def handle(self, name: str, args: dict[str, Any]) -> dict[str, Any]:
        if name == "meet_join":
            return await self.join(str(args.get("meeting") or "").strip())
        if name == "meet_status":
            return self.status()
        if name == "meet_leave":
            return self.leave()
        return {"ok": False, "reason": f"unknown tool {name}"}

    async def close(self) -> None:
        if self.busy and self.task is not None:
            if self.session is not None:
                self.session.state.ended = "eve was shutting down"
            self.task.cancel()
            await asyncio.gather(self.task, return_exceptions=True)


async def _no_notify(text: str, about: str = "") -> None:
    log.info("meet: (no chat to tell) %s", text)


def calendar_lister(apps: Any) -> Callable[[datetime, datetime], Any]:
    """The owner's calendar through Google's Calendar MCP server (apps_mcp.AppsBridge.execute, blocking)."""
    def list_events(start: datetime, end: datetime) -> Any:
        out = apps.execute("calendar__list_events", {
            "startTime": start.isoformat(), "endTime": end.isoformat(), "orderBy": "startTime", "pageSize": 50})
        if isinstance(out, dict) and out.get("ok") is False and not events_in(out):
            raise RuntimeError(str(out.get("reason") or out.get("error") or "the calendar call failed")[:200])
        return out

    return list_events


def make_meeter(config: Optional[MeetConfig], notes_root: Path, *, answer: Optional[Callable[[], Awaitable[Any]]] = None,
                apps: Any = None, environ: Any = None) -> Optional[Meeter]:
    """The real Meeter, or None (one log line) when it is off, Chrome attach is off, or Playwright is missing."""
    from . import browser_lane

    cfg = config or configured(environ)
    if not cfg.enabled:
        log.info("meet: off (EVE_MEET)")
        return None
    lane_cfg = browser_lane.configured(environ)
    if not lane_cfg.attach:
        log.info("meet: off — it joins from the owner's Chrome, and EVE_BROWSER_ATTACH is off")
        return None
    try:
        import playwright  # noqa: F401 — the import is the check
    except ImportError:
        log.warning("meet: Playwright is not installed; off")
        return None
    if lane_cfg.steel:
        # Steel's Chrome is headless with its sound muted: a meeting needs real audio, so Meet joins from
        # eve's own Chrome (EVE'S OWN CHROME in browser_lane.py) whatever drives the other web tasks.
        lane_cfg = dataclasses.replace(lane_cfg, steel=False, own=True)
    lane_cfg = dataclasses.replace(lane_cfg, chrome_profile=cfg.profile or lane_cfg.chrome_profile)

    def make_page() -> MeetPage:
        return ChromeMeetPage(browser_lane.BrowserLane(lane_cfg), answer)

    if lane_cfg.own:
        log.info("meet: on — joins from eve's own Chrome, as the account signed in there, listen only")
    else:
        log.info("meet: on — joins from %s's Chrome profile, listen only", lane_cfg.chrome_profile or "the first open")
    return Meeter(cfg, make_page, notes_root, list_events=calendar_lister(apps) if apps is not None else None,
                  summarize=make_summarizer(environ))
