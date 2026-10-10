"""Intent: does what the owner just said mean "go away", "be silent", or "sound back on"?

The voice model decides for itself whether to delegate a turn, and the log
shows it answering "Goodbye, pal." without ever calling end_conversation: the
session then sat open until the 20 s idle timer. So the session no longer
relies on the model to notice. Every finished user turn goes through two
checks, cheapest first:

1. ``fast_intent`` — a phrase table for the unmistakable cases ("bye eve",
   "stop listening", "I want you to leave", "mute"). No network, no delay.
2. A small text model (``gpt-5.4-nano`` by default) for everything else, so
   "alright, I'm heading out" or "you can go now, little guy" also end the
   conversation. It answers one label with a confidence; anything under
   ``MIN_CONFIDENCE`` counts as no intent.

The labels are about eve itself. "Leave the tab open", "mute Spotify" and
"tell Sam goodbye" are requests about the computer and must stay ``none`` —
the table only matches when the whole turn is the phrase, and the model is
told the same rule.
"""

from __future__ import annotations

import asyncio
import json
import logging
import os
import re
import sys
from typing import Any, Awaitable, Callable, Optional

from . import spend

log = logging.getLogger(__name__)

LEAVE = "leave"
MUTE = "mute"
UNMUTE = "unmute"
LOOK = "look"             # turn the head, look somewhere, look around, find something with its eyes
LESSON = "lesson"         # teach me, quiz me, tutor me, help me understand: open the lesson
REMEMBER = "remember"     # the owner is promoting something to the permanent layer, out loud
NOTES = "notes"           # start taking notes: eve records the room until told to stop
NONE = "none"
LABELS = (LEAVE, MUTE, UNMUTE, LOOK, LESSON, REMEMBER, NOTES, NONE)

DEFAULT_MODEL = "gpt-5.4-nano"
MIN_CONFIDENCE = 0.6
TIMEOUT_SECS = 4.0
MAX_WORDS = 25            # longer turns are requests, not dismissals; the backend owns them

# Words that decorate a dismissal without changing it: "okay, bye eve, thanks".
# "thank you" is folded to "thanks" first (normalize), so "you" never counts as filler:
# it is the subject of "you can go".
_LEAD = {"ok", "okay", "alright", "right", "well", "um", "uh", "so", "hey", "yeah", "yes", "oh",
         "thanks", "please", "eve", "cool", "great", "perfect", "nice"}
_TRAIL = {"eve", "now", "then", "please", "thanks", "pal", "friend", "man", "dude",
          "mate", "little", "guy", "today", "bud"}

_UNMUTE = re.compile(
    r"^(?:un ?mute(?: yourself)?|sound (?:back )?on|turn (?:your )?(?:sound|sounds|volume|beeps?) (?:back )?on"
    r"|you can (?:make (?:sounds?|noise)|beep|talk out loud)(?: again)?)$")
_MUTE = re.compile(
    r"^(?:mute(?: yourself)?|go (?:mute|silent|quiet)|silence|shh+|hush|sound off|be quiet|quiet"
    # "your", never "the": "turn the volume down" is about the Mac, not eve.
    r"|no (?:more )?(?:sounds?|noise|beeps?|beeping)|turn (?:your )?(?:sound|sounds|volume|beeps?) (?:off|down)"
    r"|stop (?:beeping|making (?:sounds?|noises?)))$")
# Dismissals that stand on their own anywhere in the turn: they only make sense said to eve.
_LEAVE_ANYWHERE = re.compile(
    r"\b(?:stop listening|leave me alone|(?:i|we) (?:want|need) you to (?:leave|go(?: away)?)"
    r"|get lost|end (?:the |this |our )?conversation|you'?re dismissed)\b")
# Dismissals that must be the whole (trimmed) turn: "go to sleep" alone, not "put the Mac to sleep".
_LEAVE_WHOLE = re.compile(
    r"^(?:(?:good ?)?bye(?: bye)?|see (?:you|ya)(?: later| soon| around)?|good ?night|farewell|later"
    r"|catch you later|talk (?:to you )?(?:later|soon)|ttyl|go away|just go|go to sleep|you can (?:go|leave)"
    r"|that'?s (?:all|it)(?: for(?: now)?)?|we'?re done(?: here)?|i'?m done(?: talking)?|dismissed|that will be all)$")
# Head moves: a look/turn verb first and a direction or room target last ("look a bit up and to your
# left", "turn around", "look at me"). Anything with a computer or media word is left to the backend:
# "turn the volume down", "look up the weather".
_LOOK_START = re.compile(r"^(?:(?:can|could|would|will) you |please |now |and )*(?:look|turn|glance|peek|face|spin)\b")
_LOOK_END = re.compile(
    r"\b(?:left|right|behind(?: you| yourself)?|around|round|backwards?|back here|ceiling|floor|desk|door|window"
    r"|me|up there|down there|over there|this way|that way|straight ahead|ahead|up|down)$")
_NOT_HEAD = re.compile(
    r"\b(?:volume|music|song|sound|it|them|brightness|screen|tab|page|app|light|lights|heat|tv|video|spotify"
    r"|safari|chrome|browser|file|email|mail|weather|off|on)\b")


# A lesson: the owner wants to be taught, quizzed, tutored or walked through something, or wants help
# understanding or working through a problem. The voice model tends to teach it itself instead of
# delegating, so the session routes these to the backend, which opens the lesson. Anchored at the start
# and always about the owner ("teach me"): "what is ownership in Rust" is a question, "teach the kids
# to say hi" is about other people, and both stay none.
_LESSON = re.compile(
    r"^(?:(?:can|could|would|will) you |please |now |and |so |maybe )*"
    r"(?:teach me|tutor me|quiz me|test me on|drill me on|coach me|walk me through"
    r"|help me (?:understand|learn|study|work through|practi[cs]e|figure out)"
    r"|(?:i|we)(?: want| wanna| would like|'d like| need) (?:to (?:learn|study|practi[cs]e|understand"
    r"|do a lesson|have a lesson|start a lesson)|a lesson)"
    r"|(?:let'?s|can we|could we|shall we) (?:do|have|start) a lesson|(?:let'?s|can we|shall we) (?:learn|study|practi[cs]e)"
    r"|(?:start|open|begin) (?:a |the |my )?lesson|lesson time)\b")
# "I want to learn more about this email" is a computer job, not a lesson.
_NOT_LESSON = re.compile(
    r"\b(?:email|mail|inbox|file|files|folder|tab|page|app|screen|window|messages?|text|texts|calendar"
    r"|spotify|safari|chrome|browser|youtube|video|song|playlist)\b")


def _is_head_move(core: str) -> bool:
    return bool(_LOOK_START.search(core) and _LOOK_END.search(core) and not _NOT_HEAD.search(core))


def normalize(text: str) -> str:
    t = text.lower().replace("’", "'").replace("‘", "'")
    t = re.sub(r"[^a-z0-9' ]+", " ", t)
    t = " ".join(t.split())
    return re.sub(r"\bthank you\b", "thanks", t)


def _core(words: list[str]) -> list[str]:
    while words and words[0] in _LEAD:
        words = words[1:]
    while words and words[-1] in _TRAIL:
        words = words[:-1]
    return words


# "remember that", "don't forget that", "keep that in mind", "note that down".
# Deliberately narrow: it must be a whole short clause about remembering, so
# "remember when you said" and "I remember that" do not promote anything.
_REMEMBER = re.compile(
    r"^(?:please\s+)?(?:remember|memorize|memorise|note)\s+(?:that|this|it)?$"
    r"|^(?:please\s+)?(?:do\s*not|don'?t|never)\s+forget\s+(?:that|this|it)?$"
    r"|^keep\s+(?:that|this|it)\s+in\s+mind$"
    r"|^(?:write|note)\s+(?:that|this|it)\s+down$"
    r"|^(?:remember|note)\s+(?:that|this|it)\s+(?:for|forever|always).*$"
)


# "start taking notes", "take notes", "can you take notes", "note this down for me".
# Narrow on purpose: it starts recording the room, so it must not fire on someone
# saying the word "notes" in passing.
_NOTES = re.compile(
    r"\b(?:start|begin|take|taking)\s+(?:some\s+|the\s+)?not(?:e|es)\b"
    r"|\bcan you (?:take|start)\s+(?:some\s+)?not(?:e|es)\b"
    r"|\bnote[- ]?tak(?:e|ing)\b"
    r"|\bstart\s+(?:record(?:ing)?|transcri\w+)\b",
    re.IGNORECASE,
)


def fast_intent(text: str) -> Optional[str]:
    """The phrase table. None means "not obvious" — ask the model."""
    t = normalize(text)
    if not t:
        return None
    core = " ".join(_core(t.split()))
    # A goodbye said as a separate clause: "alright, thanks, bye" → last clause "bye".
    tail = " ".join(_core(re.split(r"[,.!?;]", text.lower().replace("’", "'"))[-1].split())) if text else ""
    tail = normalize(tail)
    for candidate in (core, tail):
        if not candidate:
            continue
        if _UNMUTE.match(candidate):
            return UNMUTE
        if _MUTE.match(candidate):
            return MUTE
        if _LEAVE_WHOLE.match(candidate):
            return LEAVE
    if _LEAVE_ANYWHERE.search(t):
        return LEAVE
    if _is_head_move(core):
        return LOOK
    if _LESSON.match(core) and not _NOT_LESSON.search(t):
        return LESSON
    if _REMEMBER.match(core) or _REMEMBER.match(tail):
        return REMEMBER
    if _NOTES.search(t):
        return NOTES
    return None


PROMPT = """You label what a person just said to their personal sidekick, called eve. The label is about \
eve itself, never about the computer or other people.

- leave: they want eve to end the conversation — goodbye, see you, go away, leave, stop listening, go to \
sleep, "that's all", "I'm heading out", "you can go now".
- mute: they want eve itself to stop making sound.
- unmute: they want eve's own sound back on.
- look: they want eve to move its head or use its eyes on the room — look somewhere, turn, look around, \
look at them or at a thing in the room, find something, "what's behind you?", and asking where a physical \
thing is: "where did I leave my keys?", "where's my mug?".
- lesson: they want eve to teach, tutor, quiz or coach them, or to help them understand, learn or work \
through something — "teach me about fractions", "quiz me on French verbs", "help me work through this \
proof", "I want a lesson", "walk me through recursion". A plain question they want answered — "what is a \
mutex?", "what's 17 times 23?" — is none.
- remember: they are telling eve to keep something permanently — "remember that", "don't forget that", \
"keep that in mind", "note that down". Only when the whole point of what they said is that eve should keep \
it: "I remember that" and "remember when you said that?" are none.
- notes: they want eve to start taking notes, recording or transcribing what is said in the room — \
"take notes", "start taking notes", "can you write this down as we go". Not "remember that", which is \
remember, and not a request to read notes back.
- none: anything else. Requests about apps, music, windows, tabs, files or other people are none, even when \
they use the same words: "leave the tab open" is none, "mute Spotify" is none, "tell Sam goodbye" is none, \
"look up the weather" is none, "turn the volume down" is none.

Return the label and your confidence from 0 to 1."""

SCHEMA: dict[str, Any] = {
    "type": "object",
    "properties": {
        "intent": {"type": "string", "enum": list(LABELS)},
        "confidence": {"type": "number"},
    },
    "required": ["intent", "confidence"],
    "additionalProperties": False,
}


def parse_intent(raw: str) -> tuple[str, float]:
    obj = json.loads(raw)
    label = str(obj.get("intent") or NONE)
    if label not in LABELS:
        label = NONE
    try:
        conf = max(0.0, min(1.0, float(obj.get("confidence", 0.0))))
    except (TypeError, ValueError):
        conf = 0.0
    return label, conf


class OpenAIIntentClient:
    """Responses API, text only, strict JSON, ``store=False``."""

    def __init__(self, model: str, api_key: Optional[str] = None, timeout: float = TIMEOUT_SECS) -> None:
        from . import openrouter

        self.model = model
        self._client = openrouter.client(api_key, timeout=timeout, max_retries=0)

    def request(self, text: str) -> dict[str, Any]:
        return {
            "model": self.model,
            "instructions": PROMPT,
            "input": [{"role": "user", "content": [{"type": "input_text", "text": text}]}],
            "text": {"format": {"type": "json_schema", "name": "intent", "strict": True, "schema": SCHEMA}},
            "max_output_tokens": 40,
            "store": False,
        }

    def classify(self, text: str) -> tuple[str, float]:
        resp = self._client.responses.create(**self.request(text))
        spend.record_response(spend.VOICE, resp, model=self.model)
        return parse_intent(resp.output_text or "{}")


Classifier = Callable[[str], Awaitable[Optional[str]]]


def make_classifier(client: Any, timeout: float = TIMEOUT_SECS,
                    min_confidence: float = MIN_CONFIDENCE) -> Classifier:
    """Wrap a blocking ``classify(text) -> (label, conf)`` as an async
    ``text -> label | None``. A failure or a timeout is no intent."""

    async def classify(text: str) -> Optional[str]:
        if len(text.split()) > MAX_WORDS:
            return None
        try:
            label, conf = await asyncio.wait_for(asyncio.to_thread(client.classify, text), timeout=timeout)
        except Exception as e:  # noqa: BLE001
            log.info("intent: classifier skipped (%s: %s)", type(e).__name__, e)
            return None
        if label == NONE or conf < min_confidence:
            return None
        log.info("intent: %r → %s (%.2f)", text[:80], label, conf)
        return label

    return classify


def make_intent_classifier(environ: Any = None) -> Optional[Classifier]:
    """The real classifier, or None when there is no key (the phrase table still works)."""
    env = os.environ if environ is None else environ
    if (env.get("EVE_INTENT") or "1").strip().lower() in ("0", "false", "no", "off"):
        return None
    from . import openrouter

    key = openrouter.key(env)
    if not key:
        return None
    model = (env.get("EVE_INTENT_MODEL") or "").strip() or DEFAULT_MODEL
    try:
        return make_classifier(OpenAIIntentClient(model, api_key=key))
    except ImportError as e:
        log.warning("intent: openai SDK not importable (%s) — phrase table only", e)
        return None


def run_intent_test(pairs: list[tuple[str, str]]) -> int:
    """``eve-bridge intent-test --expect leave "..." ...``: the real classifier on each
    phrase (the phrase table is bypassed, so this measures the model). Prints ``intent-test ok``."""
    from . import openrouter

    env = os.environ
    key = openrouter.key(env)
    if not key:
        print("intent-test: OPENROUTER_API_KEY not set", file=sys.stderr)
        return 2
    model = (env.get("EVE_INTENT_MODEL") or "").strip() or DEFAULT_MODEL
    client = OpenAIIntentClient(model, api_key=key)
    bad = 0
    for want, text in pairs:
        want_label = NONE if want == "stay" else want
        try:
            label, conf = client.classify(text)
        except Exception as e:  # noqa: BLE001
            print(f"  ERROR {text!r}: {type(e).__name__}: {e}")
            bad += 1
            continue
        got = label if conf >= MIN_CONFIDENCE else NONE
        ok = got == want_label
        bad += 0 if ok else 1
        print(f"  {'ok ' if ok else 'BAD'} {text!r}: {label} ({conf:.2f}), wanted {want_label}")
    if bad:
        print(f"intent-test: {bad} wrong ({model})", file=sys.stderr)
        return 1
    print(f"intent-test ok ({model}, {len(pairs)} phrase(s))")
    return 0
