"""Consequential-action approval and tracking of outside content.

Reads and reversible work run without extra confirmation. ``needs_confirmation`` identifies native
consequential actions; app policies classify connected-app writes, and purchase/share executors own their
final exact-action approval. ``acting`` remains separate so this policy does not change provenance.

A turn is tainted when its prompt carries outside text, attachments, or earlier tainted replies. Reads keep
that taint across subsequent rounds and persisted history. A consequential approval shows the complete
arguments or refuses unasked if they cannot fit. Voice approval requires the question to have been spoken
whole and binds one bare owner yes to the exact call, once. An owner's actual answer to a delegated task
question passes through without a second approval.

Every name below is a tool eve offers somewhere; none is written into a prompt.
"""

from __future__ import annotations

import json
import re
import unicodedata
from collections import deque
from typing import Any, Callable, Iterable, Mapping, Optional

from . import app_registry

# Tools that change something, open a session or send something, whatever their arguments. The direct apps' and
# Composio's writes are decided by their own policies (telegram._direct_tool, _app_tool), not listed here. eve's
# apps (lights, music, the TV, the camera, ...) declare theirs in app_registry.py; every set below is eve's own
# tools unioned with the apps' declarations.
ACTING: frozenset[str] = frozenset({
    # the / menu's hand-offs (menu_tools.py): each sends a button, a link or a line, or flips a relay
    "claude_relay", "codex_relay", "set_stealth", "pair_eve_link",
    # the Mac and coding sessions: a task started, steered or stopped (a stop also drops a purchase's consent)
    "start_task", "steer_task", "stop_task", "start_coding_session", "screenshot", "take_photo", "send_file",
    "opentable_search",                          # a browser task in the owner's signed-in Chrome
    # the watcher, Meet, the room's notes, memory and the vault, the app maker, the chief
    "watch_add", "watch_remove", "watch_pause", "watch_resume", "watch_set_end",
    "meet_join", "meet_leave", "take_notes",
    "remember", "forget_apply",                  # a starred line is read by every later turn (the profile)
    "capture_note", "edit_note", "file_note", "undo_note",
    "make_app", "change_app", "delete_app", "rename_app", "restore_app", "undo_app",
    "take_on", "close_card",
    # places (places.py): place_name names a place, thing_place keeps or forgets where a thing is (it also finds and
    # lists); both write the places history. Routine, so neither asks (POLICY)
    "place_name", "thing_place",
    # buying
    "agentcard_buy", "agentcard_confirm",
    # the voice's own (voice_agent.TOOLS): the robot sent off, its sound, a task's question answered, Meet
    "go_explore", "set_sound", "answer_question", "join_meeting", "leave_meeting",
}) | app_registry.ACTING()   # the apps' acts: every Fire TV action changes the TV, every camera_control moves it
# Acting calls whose results still carry outside words: the page a new watch read first, what a photo or the
# screen shows. They taint the turn after them like any read.
ACTING_READS: frozenset[str] = frozenset({"watch_add", "take_photo", "screenshot",
                                         "meet_join", "join_meeting",   # an invite's title comes back
                                         # a place's name can be the phone's geocoder's words (a business's own)
                                         "place_name", "thing_place"})
# Acting calls whose result carries outside words for most of their actions: spotify_control answers with the
# catalog's own text (a track's and an artist's names, a public playlist's name and its owner's display name) when it
# plays something by name, skips, or says what is on (round-4 review). These actions answer with eve's own words
# only; every other action, an unknown one included, taints the turn after it like any read.
_OWN_WORDS_ACTIONS: dict[str, frozenset[str]] = {**app_registry.OWN_WORDS()}
# A call whose one read-only action only says what is on: that action is a read.
_STATUS_ONLY: dict[str, str] = {"take_notes": "status"}

# Calls whose results are eve's own state, with no text from outside: they never taint a turn. (task_status is
# not one: it answers with the running task's own words.)
CLEAN: frozenset[str] = frozenset({"spend_report", "open_app",
                                   "open_apps",
                                   "move_head",
                                   "forget_preview"}) | app_registry.CLEAN()   # forget_preview: counts and a token

# What each kind of read is called in the owner's question ("after reading ..."); anything else is "a tool result".
_SOURCES: dict[str, str] = {
    "web_search": "a web search", "events_search": "Ticketmaster's listings", "opentable_search": "OpenTable",
    # a read, never acting, so it never asks; not CLEAN: its titles and abstracts are other people's words
    "science_search": "a search of research papers",
    # reads, never acting, so they never ask; not CLEAN: headlines and abstracts are the paper's words (nyt.py)
    **dict.fromkeys(("nyt_search", "nyt_archive", "nyt_bestsellers", "nyt_popular", "nyt_headlines"),
                    "the New York Times"),
    "find_image": "Wikipedia",
    "recent_messages": "earlier chat messages", "memory_search": "memory", "memory_read": "memory",
    "read_note": "a note", "search_notes": "your notes", "list_inbox": "your notes", "list_todos": "your todos",
    "daily_rundown": "the rundown's sources", "america_ask": "a government site", "think_hard": "a long think",
    "jobs_list": "the job cards", "watch_list": "the watches", "list_files": "the Mac's file list",
    "read_file": "a file on the Mac",
    # a read, never acting, so it never asks; not CLEAN: its place is the phone's geocoder's words, which can be a
    # business's own name (phone_location.py)
    "phone_location": "the phone's location",
    "meet_join": "a meeting invite", "join_meeting": "a meeting invite",
    "watch_add": "the watched page", "take_photo": "a photo", "screenshot": "the screen", "look": "the camera",
    "look_around": "the camera", "find": "the camera",
    **app_registry.SOURCES(),
}
# ---- approval policy: who approves each tool's call ---------------------------------------------------------------

# Every tool eve offers declares one of app_registry.POLICIES here (exactly read, routine, asks, taint_asks and
# executor_owned), and tests/test_tool_gate_coverage.py fails CI for an offered tool that has none. It is declared, not
# defaulted: a tool missing here does not ask at runtime (that would put questions on the phone whenever this map
# fell behind); the test is what stops a new tool from shipping undeclared. needs_confirmation reads it. Names with
# "__" (the direct apps) and COMPOSIO_* names are decided by their own policies (apps_mcp, composio_tools.decide).
#
# Asks every time: deleting (forget, delete an app, remove a watch), and a new Eve Link pairing (_ASK_WHEN).
ASKS: frozenset[str] = frozenset({"forget_apply", "delete_app", "watch_remove", "pair_eve_link"})
# Runs on the owner's own request; asks once the turn has read outside text, because a page's words could have
# asked for it: recording the room (_ASK_WHEN), joining a meeting, answering a delegated task's question.
TAINT_ASKS: frozenset[str] = frozenset({"take_notes", "meet_join", "join_meeting", "answer_question"})
# The tool's own handler asks for the exact act, so the gate does not ask twice: a purchase's confirmation, sharing a
# file, the vault's fill (Codex; its value never reaches a model), a Mac task's Python (the computer agent's own
# approval of its final act).
EXECUTOR_OWNED: frozenset[str] = frozenset({"agentcard_confirm", "share_file", "vault_fill", "exec_py"})
# Changes something and never asks, though not in ACTING (its result keeps tainting like a read's): ending the voice
# conversation, the lesson whiteboard.
_ROUTINE: frozenset[str] = frozenset({"end_conversation", "lesson"})
# Reads with no other entry above (not in _SOURCES or CLEAN): each changes nothing.
_READS: frozenset[str] = frozenset({
    "canvas_courses", "canvas_due", "canvas_announcements", "canvas_find_link",   # canvas.py
    "places_recall", "task_status", "find_contact", "vault_list", "second_brain_workflow", "list_apps",
    "meet_status", "recent_conversation", "ask_user",                             # ask_user: the Mac task's question
    "agentcard_merchants", "agentcard_status",                                    # agentcard.py
})
# Narrower than the name: which arguments make an asks or taint_asks tool ask (absent: every call does).
_ASK_WHEN: dict[str, Callable[[Mapping[str, Any]], bool]] = {
    "pair_eve_link": lambda args: bool(args.get("new_link")),        # match the handler: a new link revokes pairing
    "take_notes": lambda args: str(args.get("action") or "").strip().lower() == "start",   # recording the room
}
# Later entries win: an acting read (watch_add) is routine, and eve's own asking sets come last, so no app's line can
# turn one of them into a tool that runs unasked.
POLICY: dict[str, str] = {
    **dict.fromkeys((*_SOURCES, *CLEAN, *_READS), "read"),
    **dict.fromkeys((*ACTING, *_ROUTINE), "routine"),
    **app_registry.POLICY(),
    **dict.fromkeys(EXECUTOR_OWNED, "executor_owned"),
    **dict.fromkeys(TAINT_ASKS, "taint_asks"),
    **dict.fromkeys(ASKS, "asks"),
}
assert set(POLICY.values()) <= set(app_registry.POLICIES)


def policy(name: str) -> Optional[str]:
    """The tool's declared approval policy; None for a name eve does not declare (a direct app's, Composio's)."""
    return POLICY.get(name)


# A Composio read in eve lite (EVE_PROFILE=lite), where Composio holds whichever apps the owner connected (source).
COMPOSIO_LITE_SOURCE = "your connected apps"
TASK_RESULT = "a computer task's result"
TASK_QUESTION = "a computer task's question"
ATTACHED = "the attached file"
PICTURE = "the attached picture"


def acting(name: str, args: Mapping[str, Any]) -> bool:
    """Whether this call changes something, opens a session or sends something (provenance classification)."""
    if name in _STATUS_ONLY:
        return str(args.get("action") or "").strip().lower() != _STATUS_ONLY[name]
    return name in ACTING


def needs_confirmation(name: str, args: Mapping[str, Any], tainted: Any = "") -> bool:
    """Deletions always ask; recording, joining or answering ask only after outside text. Delegated work keeps
    its final approval in its executor.

    The owner's rule (2026-10-09): ask for sending, deleting and purchasing, not routine work. ACTING remains the
    provenance classification: routine edits still change state, but reading outside text does not revoke the
    owner's authorization to do routine work. Starting a recording, joining a meeting or answering a task's
    question is the owner's own request in a clean turn; after a read, a page's words could have asked for it,
    so it asks then. Purchases and sharing have their own exact confirmation handlers; asking before those
    handlers would ask twice for one action.

    Read from POLICY: ``asks`` asks always and ``taint_asks`` after outside text, each narrowed by _ASK_WHEN; every
    other policy, and a name with none, runs unasked.
    """
    kind = POLICY.get(name)
    if kind == "asks" or (kind == "taint_asks" and tainted):
        when = _ASK_WHEN.get(name)
        return True if when is None else when(args)
    return False


def taints(name: str, args: Mapping[str, Any]) -> bool:
    """Whether this call's result can carry words from outside eve. Most acting calls answer with eve's own
    status; ACTING_READS answer with what they saw."""
    if name in CLEAN:
        return False
    if name in _OWN_WORDS_ACTIONS:
        action = str(args.get("action") or "").strip().lower()
        if action == "play":                     # a resume says "Playing."; a play by name names the catalog's item
            return bool(str(args.get("query") or "").strip())
        return action not in _OWN_WORDS_ACTIONS[name]
    return name in ACTING_READS or not acting(name, args)


def source(name: str) -> str:
    """The read, in the owner's words, for the question. In eve lite (profile.is_lite) Composio's reads are the
    owner's own connected apps, whichever they are, never three named ones."""
    if name in _SOURCES:
        return _SOURCES[name]
    if "__" in name:
        return f"{name.split('__', 1)[0].capitalize()}"
    if name.startswith("COMPOSIO_"):
        return COMPOSIO_LITE_SOURCE if _lite() else "Slack, Notion or Linear"
    return "a tool result"


def _lite() -> bool:
    """eve lite (EVE_PROFILE=lite): profile.py is read when asked, so this module imports as it always has."""
    from . import profile

    return profile.is_lite()


# The question shows the act whole; a call longer than this is refused unasked (``describe`` returns None).
SHOW_CHARS = 1500


# Characters that show nothing yet change what a line says or how it reads: zero-width ones (U+200B-U+200F, the
# word joiner and invisible operators U+2060-U+2064, the BOM U+FEFF) and bidi controls (U+202A-U+202E,
# U+2066-U+2069), which can make "send to evil" display as something else. A question shows each one visibly,
# "[U+202E]", never strips it: the call runs with its arguments as written, so the owner sees what will run
# (round-4 review).
# Beyond those named, every format character (Unicode category Cf: the Arabic letter mark, the tag characters
# U+E0000-U+E007F that can hide whole words, the soft hyphen), the variation selectors and the blank fillers
# (U+180E, U+3164, U+115F, U+1160, U+FFA0) are written out too (round-4 verify).
_FILLERS = frozenset("\u180e\u3164\u115f\u1160\uffa0")


def _hidden(ch: str) -> bool:
    o = ord(ch)
    return (unicodedata.category(ch) == "Cf" or ch in _FILLERS or 0xFE00 <= o <= 0xFE0F
            or 0xE0100 <= o <= 0xE01EF or 0xE0000 <= o <= 0xE007F)


def visible(text: Any) -> str:
    """``text`` with every hidden or direction-changing character written out as [U+XXXX]."""
    return "".join(f"[U+{ord(c):04X}]" if _hidden(c) else c for c in str(text))


def _flat(text: Any) -> str:
    return " ".join(visible(text).split())


def describe(name: str, args: Mapping[str, Any]) -> Optional[str]:
    """The act, whole, in a line the owner can judge; None when it is too long to show whole (SHOW_CHARS)."""
    a = {k: v for k, v in args.items() if v not in (None, "", [], {})}
    if name == "start_task":
        line = "start a Mac task: " + _flat(a.get("goal", ""))
    elif name == "steer_task":
        line = "tell the running task: " + _flat(a.get("text", ""))
    elif name == "claude_relay":
        on = str(a.get("action") or "on").lower() != "off"
        line = ("connect this chat to Claude Code" + (f" ({_flat(a['session'])})" if a.get("session") else "")
                if on else "turn the Claude relay off")
    elif name == "codex_relay":
        line = f"Codex {_flat(a.get('action', 'on'))}" + (f" in {_flat(a['folder'])}" if a.get("folder") else "")
    elif name == "set_stealth":
        line = "turn stealth mode " + ("on" if args.get("on") else "off")
    else:
        try:
            shown = json.dumps(a, ensure_ascii=False, sort_keys=True)
        except (TypeError, ValueError):
            shown = str(a)
        line = f"{name.replace('_', ' ')}" + (f": {_flat(shown)}" if a else "")
    return line if len(line) <= SHOW_CHARS else None


def question(name: str, args: Mapping[str, Any], read: str) -> Optional[str]:
    """The owner's yes/no: what would happen, and what was read before it. None: too long to show whole."""
    act = describe(name, args)
    if act is None:
        return None
    return f"Before I do this, a check: it comes after reading {read}, and I am about to {act}. Go ahead?"


def about(name: str, read: str) -> str:
    """The history's line for the question: no words from the read become eve's own (as tell_owner's about)."""
    return f"(asked the owner before {name} because it came after reading {read})"


REFUSED = {"ok": False, "reason": "the owner did not approve it; it did not run. Do not try it again this turn."}


def withdrawn(text: str) -> dict[str, Any]:
    """The result of a call whose question the owner withdrew by typing something else (EVE_GATE_WITHDRAW): it did
    not run, and the owner's own words are handed to the same turn to act on. The words are the owner's, typed in the
    chat, so they taint nothing. Model-visible text: it ships behind the switch until its blind eval passes."""
    return {"ok": False, "reason": ('the owner did not approve this, so it did not run; instead they wrote: "'
                                    + str(text) + '". Act on that now; do not retry the refused call unless '
                                    "their words ask for it.")}
TOO_LONG = {"ok": False, "reason": "too long to show the owner whole in a question, so it did not run; ask the owner "
                                   "to say what they want in their own words."}


# Out loud the act must be short enough to hear whole: a call whose spoken line is longer is refused unasked.
SAY_CHARS = 220
_SAY_HEADS: dict[str, str] = {
    "start_task": "start a computer task to", "steer_task": "tell the running task", "join_meeting": "join the meeting",
    "meet_join": "join the meeting", "answer_question": "answer the task's question with", "take_notes": "take notes",
    "remember": "remember that", "watch_add": "watch",
    **app_registry.SAY(),
}
_SAY_WORDS: dict[str, str] = {"start_task": "goal", "steer_task": "text", "answer_question": "answer"}
_SAY_FIXED: dict[str, str] = {"leave_meeting": "leave the meeting", "meet_leave": "leave the meeting",
                              "go_explore": "go off exploring", "stop_task": "stop the running task"}


def _said_value(value: Any) -> str:
    if isinstance(value, bool):
        return "yes" if value else "no"
    if isinstance(value, (str, int, float)):
        return _flat(value)
    try:
        return _flat(json.dumps(value, ensure_ascii=False, sort_keys=True))
    except (TypeError, ValueError):
        return _flat(value)


def say_act(name: str, args: Mapping[str, Any]) -> Optional[str]:
    """The act as a short line code writes for the voice to say, from the call's name and arguments alone (never
    the model's own sentence); None when it cannot be said briefly (SAY_CHARS)."""
    a = {k: v for k, v in args.items() if v not in (None, "", [], {})}
    if name in _SAY_FIXED:
        line = _SAY_FIXED[name]
    elif name == "set_sound":
        line = "turn my sound " + ("on" if args.get("on") else "off")
    elif name in _SAY_WORDS:
        words = _flat(a.get(_SAY_WORDS[name], ""))
        line = f"{_SAY_HEADS[name]}: {words}" if words else _SAY_HEADS[name]
    else:
        head = _SAY_HEADS.get(name, name.replace("_", " "))
        said = ", ".join(f"{k.replace('_', ' ')} {_said_value(v)}" for k, v in sorted(a.items()))
        line = f"{head}: {said}" if said else head
    return line if len(line) <= SAY_CHARS else None


def spoken_question(name: str, args: Mapping[str, Any], read: str) -> Optional[str]:
    """What the voice is handed to say, word for word: code's own question about this exact call. None: refused
    unasked (it cannot be said briefly)."""
    act = say_act(name, args)
    if act is None:
        return None
    return f"Quick check first, because I just read {read}. Should I {act}? Say yes to go ahead."


def _words(text: Any) -> str:
    return " ".join(re.sub(r"[^\w@.]+|(?<!\w)[.@]|[.@](?!\w)", " ", str(text).lower()).split())


def said_whole(question: str, spoken: str) -> bool:
    """Whether the voice's own output transcript (what it said) ends with the question code wrote, word for word,
    punctuation and case aside: the last thing the owner heard before their yes is code's whole question. The voice
    (a Live speech model) has no say-this-verbatim path: code hands it the question as commentary, and this is the
    check that it said it. A yes counts for the held call only when it did (voice_agent._gate); a paraphrase, or the
    act framed by words of the model's own ("should I not …", an aside after it), drops the held call, so it is
    asked again (round-4 verify: containment let the act be framed falsely). Words said earlier in the same turn
    come before the question and change nothing it asks."""
    want, said = _words(question), _words(spoken)
    # the voice's transcript arrives in pieces joined as they came ("one.Quick"), so a full stop also bounds it
    return bool(want) and (said == want or said.endswith((" " + want, "." + want)))


SAY_EXACTLY = "Say exactly this, word for word, and nothing else: "
ASKED_ALOUD = {"ok": False,
               "reason": "Not run yet: eve has asked the owner out loud whether to do exactly this. Say nothing "
                         "more about it now. Only after they say yes, call it again with the same arguments."}
TOO_LONG_ALOUD = {"ok": False, "reason": "too long to say to the owner whole, so it did not run; ask the owner to "
                                         "say in their own words what they want."}


# ---- carried taint: outside words already in a turn's prompt ----------------------------------------------------

# A fingerprint is a line's first FP_CHARS characters, whitespace flattened: short enough to survive the voice
# block's clip (transcripts.VOICE_LINE_CHARS), long enough to name one line. Shorter lines carry too little to act on.
FP_CHARS, FP_MIN = 96, 24
CARRIED = "an earlier tool result"
# A tool line whose words are not eve's although the call itself is acting: a computer task's closing sentence,
# written after it read screens and pages (voice_agent._run_agent). carried() treats it as a read.
RESULT_LINES: frozenset[str] = frozenset({"start_task"})
# A tool's line as a transcript block renders it: "(texted) eve (web search): ..." (transcripts.render_line).
_TOOL_LINE = re.compile(r"\((?:spoken|texted|sent)\) eve \(([a-z0-9 ]{1,40})\): ")


def fingerprint(text: Any) -> str:
    flat = _flat(text)
    return flat[:FP_CHARS] if len(flat) >= FP_MIN else ""


class Marks:
    """Lines eve wrote with outside words in them (a tainted turn's reply, a task's closing sentence), by
    fingerprint: one per process, shared by the chat and the voice, which read one transcript. Bounded; a line long
    gone from every window costs nothing but a slot. In memory only: after a restart the transcript's tool lines
    still carry (``carried`` reads them from the prompt itself), a tainted reply from before it does not."""

    def __init__(self, size: int = 400) -> None:
        self._prints: deque[str] = deque(maxlen=size)

    def add(self, *texts: Any) -> None:
        for text in texts:
            fp = fingerprint(text)
            if fp and fp not in self._prints:
                self._prints.append(fp)

    def clear(self) -> None:
        self._prints.clear()

    def found(self, prompt: str) -> bool:
        return any(fp in prompt for fp in self._prints)


MARKS = Marks()


def _texts(value: Any) -> Iterable[str]:
    """Every string in a request's instructions and input, pictures left out."""
    if isinstance(value, str):
        if not value.startswith("data:"):
            yield value
    elif isinstance(value, Mapping):
        for v in value.values():
            yield from _texts(v)
    elif isinstance(value, (list, tuple)):
        for v in value:
            yield from _texts(v)


def carried(*parts: Any, marks: Optional[Marks] = None) -> str:
    """CARRIED when the prompt (``parts``: its instructions, its input items) already holds outside words: a
    tainting tool's line in a transcript block, or a marked line. "" when it holds only eve's and the owner's."""
    prompt = _flat(" ".join(t for p in parts for t in _texts(p)))
    for m in _TOOL_LINE.finditer(prompt):
        tool = m.group(1).replace(" ", "_")
        if tool in RESULT_LINES or taints(tool, {}):
            return CARRIED
    return CARRIED if (marks if marks is not None else MARKS).found(prompt) else ""
