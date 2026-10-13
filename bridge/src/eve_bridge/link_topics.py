"""Horizontal History: eve's one conversation cut into topics the app shows as cards, and the ones the owner saved.

Owner, 2026-10-13: past topics "organized as swipeable horizontal cards. Users can bookmark important topics and
quickly jump back into them" — "for our eve app", and "remember occams razor". eve has one continuous
conversation, so a topic is simply a run of messages with no pause longer than ``GAP_SECS``. Its title is its first
owner message. No model is asked anything.

Bookmarks live in ``~/.config/eve-bridge/chat/bookmarks.json`` (0600), keyed by the topic's first message id, so a
saved topic stays saved while it grows.

Titles (owner, 2026-10-13: "use a model for the titles"): a cheap model names each topic in a few words and says
whether it was only small talk (``Titler``). One call per topic, made in the background after a list is served,
cached in ``~/.config/eve-bridge/chat/topic-titles.json`` (0600) by topic id and message count, so a topic is titled
once and again only when it grows. Until then, and with ``EVE_TOPIC_TITLES=0`` or no OpenRouter key, the title is
the owner's first message that says something and small talk is judged by the rules below.
"""

from __future__ import annotations

import json
import logging
import os
import re
import tempfile
import threading
import time
from pathlib import Path
from typing import Any, Iterable, Optional

GAP_SECS = 45 * 60            # a pause longer than this starts a new topic
DAYS = 30                     # transcript days read for the cards
TITLE_CHARS = 60
PREVIEW_CHARS = 120
DEFAULT_LIMIT = 30
MAX_LIMIT = 60
BOOKMARKS_FILE = "~/.config/eve-bridge/chat/bookmarks.json"
TITLES_FILE = "~/.config/eve-bridge/chat/topic-titles.json"
TITLE_MODEL = "deepseek/deepseek-v4.1-flash"     # the cheap background model (docs/openrouter.md, watches)
TITLES_PER_PASS = 8                               # topics titled per background pass, newest first
TOPIC_CHARS = 3000                                # what of a topic the model reads: its start, where it is named
TITLE_SYSTEM = (
    "You name one conversation between a person and their assistant, eve, for a card in a list of past topics. "
    "Reply with one JSON object only: {\"title\": string, \"small_talk\": boolean}. The title is 2 to 6 words in "
    "sentence case with no end punctuation and no quotes: the subject the person cared about, the way a note's "
    "heading reads (for example: Lisbon trip in November; Zelle charge from Oct 12; Webcam angle and tracking). "
    "small_talk is true when the conversation is only greetings, pleasantries and checking in, with no task, "
    "question or information exchanged; otherwise false.")
ID_RE = re.compile(r"^\d{4}-\d{2}-\d{2}:\d{1,7}$")    # a message id: <day>:<line index>
# what makes a line read badly on a card: markdown marks, an OpenUI program, a component action's context
_PROGRAM = re.compile(r"```[\s\S]*?(```|$)|\n\n\[\"User clicked: [\s\S]*$")
_MARKS = re.compile(r"[*_`#>|~\[\]]+")
# Small talk: a message that only says hello, thanks or ok names nothing and makes nothing important.
_FRIEND = r"( (there|eve|friend|buddy|bro|dude|man|mate|pal))*"
_GREETING = re.compile(
    r"^((hi+|hey+|hello|yo|hiya|sup|morning|night|gm|gn|good (morning|afternoon|evening|night))" + _FRIEND +
    r"|(how are (you|u|ya)|how r u|how('?s| is) it going|how('?s| are) (you|u) doing|how have you been|"
    r"what'?s up|wassup|are (you|u) (there|awake|up|ok|okay)|(you|u) there)" + _FRIEND +
    r"|(ok(ay)?|k|kk|cool|nice|great|perfect|awesome|thanks|thank (you|u)|thx|ty|lol|lmao|haha+|yes|yeah|yep|no|"
    r"nope|sure|got it|bye|cya|test(ing)?|nm|not much|good|fine|i'?m good|all good)" + _FRIEND +
    r")[\s!.,?]*$", re.I)
# A short message built around a pleasantry ("It's going amazing, how are you?") is small talk too.
_PLEASANTRY = re.compile(r"\b(how are (you|u|ya)|how('?s| is) it going|what'?s (good|up|new)|how('?s| was| is) "
                         r"(your|ur) (day|night|morning|weekend)|i'?m (good|great|fine|well|doing (well|good|great))|"
                         r"(it'?s|its) going (good|great|well|amazing)|nice to (meet|see) you)\b", re.I)
SMALL_TALK_WORDS = 7
TITLE_WORDS = 3               # a title says something: shorter messages (a garbled voice line) are passed over
SUBSTANCE_WORDS = 6           # an important topic: the owner said at least this many words beyond small talk,
SUBSTANCE_COUNT = 4           # or it went back and forth at least this many messages


log = logging.getLogger(__name__)


def plain(text: str) -> str:
    return " ".join(_MARKS.sub(" ", _PROGRAM.sub(" ", text)).split())


def clip(text: str, n: int) -> str:
    if len(text) <= n:
        return text
    cut = text[: n - 1].rsplit(" ", 1)[0] if " " in text[: n - 1] else text[: n - 1]
    return cut.rstrip(" ,.;:-") + "…"


def small_talk(text: str) -> bool:
    return bool(_GREETING.match(text)) or (len(text.split()) <= SMALL_TALK_WORDS and bool(_PLEASANTRY.search(text)))


def segment(rows: Iterable[dict[str, Any]], gap: float = GAP_SECS) -> list[dict[str, Any]]:
    """Rows as link_chat._row makes them, oldest first → topics, oldest first. A run with no owner message (eve's
    own reminders alone) is not a topic. ``important``: the owner said something beyond small talk, and either
    enough of it or the run went back and forth (owner, 2026-10-13: "important topics", "i dont need to see hi")."""
    runs: list[list[dict[str, Any]]] = []
    for row in rows:
        if runs and row["t"] - runs[-1][-1]["t"] <= gap:
            runs[-1].append(row)
        else:
            runs.append([row])
    out = []
    for run in runs:
        said = [plain(r["text"]) for r in run if r["role"] == "owner" and plain(r["text"])]
        if not said:
            continue
        meant = [s for s in said if not small_talk(s)]
        first_owner = next((s for s in meant if len(s.split()) >= TITLE_WORDS), meant[0] if meant else said[0])
        words = sum(len(s.split()) for s in meant)
        important = bool(meant) and (words >= SUBSTANCE_WORDS or len(run) >= SUBSTANCE_COUNT)
        out.append({"important": important, "_lines": [(r["role"], plain(r["text"])) for r in run],
                    "id": run[0]["id"], "first_id": run[0]["id"], "last_id": run[-1]["id"],
                    "started_at": run[0]["t"], "ended_at": run[-1]["t"], "count": len(run),
                    "title": clip(first_owner, TITLE_CHARS),
                    "preview": clip(next((plain(r["text"]) for r in reversed(run)
                                          if r["role"] == "eve" and plain(r["text"])), ""), PREVIEW_CHARS)})
    return out


def read_rows(store: Any, days: int = DAYS) -> list[dict[str, Any]]:
    """Every shown message of the last ``days`` transcript days, oldest first. Blocking: call it in a thread."""
    from .link_chat import _row

    rows: list[dict[str, Any]] = []
    for day in store.days()[-days:]:
        rows.extend(r for r in (_row(day, i, ln) for i, ln in enumerate(store.lines(day))) if r is not None)
    return rows


class Bookmarks:
    def __init__(self, path: str | Path = BOOKMARKS_FILE) -> None:
        self.path = Path(path).expanduser()

    def load(self) -> dict[str, float]:
        try:
            items = json.loads(self.path.read_text()).get("items", {})
        except (OSError, ValueError, AttributeError):
            return {}
        return {k: float(v) for k, v in items.items() if isinstance(k, str) and ID_RE.match(k)
                and isinstance(v, (int, float))}

    def set(self, topic_id: str, on: bool, now: Optional[float] = None) -> None:
        items = self.load()
        if on:
            items.setdefault(topic_id, time.time() if now is None else now)
        else:
            items.pop(topic_id, None)
        self.path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
        fd, tmp = tempfile.mkstemp(dir=self.path.parent, prefix=".bookmarks.")
        with os.fdopen(fd, "w") as f:
            json.dump({"version": 1, "items": items}, f)
        os.chmod(tmp, 0o600)
        os.replace(tmp, self.path)


class Titler:
    """Model titles for topics, cached by (topic id, message count). ``apply`` puts what is known on the topics;
    ``kick`` starts one background pass over those that are not titled yet (one pass at a time)."""

    def __init__(self, path: str | Path = TITLES_FILE, environ: Any = None, ask: Any = None) -> None:
        env = os.environ if environ is None else environ
        self.path = Path(path).expanduser()
        self.model = (env.get("EVE_TOPIC_TITLE_MODEL") or TITLE_MODEL).strip() or TITLE_MODEL
        self.enabled = (env.get("EVE_TOPIC_TITLES") or "1").strip().lower() not in ("0", "off", "false", "no")
        self._env = env
        self._ask = ask                     # tests: (system, text) -> {"title", "small_talk"}
        self._lock = threading.Lock()
        self._running = False

    def load(self) -> dict[str, dict[str, Any]]:
        try:
            raw = json.loads(self.path.read_text())
        except (OSError, ValueError):
            return {}
        return {k: v for k, v in raw.items() if isinstance(v, dict) and isinstance(v.get("title"), str)} \
            if isinstance(raw, dict) else {}

    def _save(self, items: dict[str, dict[str, Any]]) -> None:
        self.path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
        fd, tmp = tempfile.mkstemp(dir=self.path.parent, prefix=".topic-titles.")
        with os.fdopen(fd, "w") as f:
            json.dump(items, f)
        os.chmod(tmp, 0o600)
        os.replace(tmp, self.path)

    def apply(self, topics: list[dict[str, Any]]) -> None:
        known = self.load()
        for t in topics:
            got = known.get(t["id"])
            if got and got.get("n") == t["count"]:
                t["title"] = clip(str(got["title"]), TITLE_CHARS) or t["title"]
                t["important"] = not got.get("small_talk", False)

    def _call(self, text: str) -> dict[str, Any]:
        if self._ask is not None:
            return self._ask(TITLE_SYSTEM, text)
        from . import openrouter, spend

        key = openrouter.key(self._env)
        if not key:
            raise RuntimeError("no OpenRouter key")
        extra: dict[str, Any] = {"usage": {"include": True}}
        if self.model == TITLE_MODEL:
            extra["reasoning"] = {"enabled": False}          # a short JSON reply; DeepSeek reasons by default
        resp = openrouter.client(key).chat.completions.create(
            model=self.model, max_tokens=80, temperature=0, extra_body=extra,
            messages=[{"role": "system", "content": TITLE_SYSTEM}, {"role": "user", "content": text}])
        spend.record_chat_completion(spend.MEMORY, resp.model_dump(), model=self.model)
        raw = (resp.choices[0].message.content or "").strip()
        m = re.search(r"\{[\s\S]*\}", raw)
        return json.loads(m.group(0) if m else raw)

    def title_pass(self, topics: list[dict[str, Any]]) -> int:
        """Title up to TITLES_PER_PASS untitled topics, newest first. Blocking. Returns how many were titled."""
        known = self.load()
        todo = [t for t in reversed(topics) if known.get(t["id"], {}).get("n") != t["count"]][:TITLES_PER_PASS]
        done = 0
        for t in todo:
            text = "\n".join(f"{'Person' if who == 'owner' else 'eve'}: {line}" for who, line in t["_lines"] if line)
            try:
                got = self._call(text[:TOPIC_CHARS])
                title = " ".join(str(got.get("title") or "").replace('"', "").split()).rstrip(" .")
                if not title:
                    continue
                known[t["id"]] = {"n": t["count"], "title": title[:TITLE_CHARS], "small_talk": got.get("small_talk") is True}
                done += 1
            except Exception as e:  # noqa: BLE001 — a title that fails keeps the first-message title; tried again later
                log.info("topics: a title failed (%s)", type(e).__name__)
                break
        if done:
            self._save({**self.load(), **known})
            log.info("topics: titled %d", done)
        return done

    def kick(self, topics: list[dict[str, Any]]) -> bool:
        """Start one background pass when titles are on and none is running. Never blocks."""
        if not self.enabled or (self._ask is None and not self._has_key()):
            return False
        with self._lock:
            if self._running:
                return False
            self._running = True

        def run() -> None:
            try:
                self.title_pass(topics)
            finally:
                with self._lock:
                    self._running = False

        threading.Thread(target=run, name="topic-titles", daemon=True).start()
        return True

    def _has_key(self) -> bool:
        from . import openrouter

        return bool(openrouter.key(self._env))


def view(store: Any, bookmarks: Bookmarks, limit: int = DEFAULT_LIMIT, titler: Optional[Titler] = None) -> dict[str, Any]:
    """``GET /api/link/topics``: the newest ``limit`` important topics, and the saved ones (newest bookmark first), each with
    ``bookmarked``. A saved topic whose first message was forgotten drops out. Blocking: call it in a thread."""
    found = segment(read_rows(store))
    saved = bookmarks.load()
    if titler is not None:
        titler.apply(found)                         # model titles and small-talk calls already made
        titler.kick(found)                          # and the rest, in the background, for the next look
    found = [t for t in found if t.pop("important") or t["id"] in saved]       # small talk never makes a card
    for t in found:
        t.pop("_lines", None)
    for t in found:
        t["bookmarked"] = t["id"] in saved
    by_id = {t["id"]: t for t in found}
    kept = sorted((i for i in saved if i in by_id), key=lambda i: saved[i], reverse=True)
    return {"topics": list(reversed(found))[:limit], "bookmarked": [by_id[i] for i in kept]}


def known(store: Any, topic_id: str) -> bool:
    return any(t["id"] == topic_id for t in segment(read_rows(store)))
