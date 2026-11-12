"""Whether a turn's request belongs to one of the owner's devices: Jev's judgment, in two layers, in one request.

Why (2026-11-11): on a call the owner said "Change TV input to HDMI 1." The chat brain had the TV's tools and
start_task on one flat list, the TV's block said inputs can't be switched without saying what to do instead, and the
model started the Mac agent. The logs show the same boundary failing 9 times in 58 Mac-agent starts (the TV, the
Mac's camera). The toolset router (toolset_router.py) never decides it: its seven toolsets are the owner's accounts,
and a device's tools ride every turn.

What this decides, before the chat model runs: is the request for one of the devices eve drives (the TV, the lights,
the music, the camera), which of that device's methods it asks for, and whether the device can do it. A claim never
changes the cached prefix (the tool list and the instruction blocks stay byte for byte): telegram._think adds one
line to the turn's developer note (``note_line``) and refuses a start_task call ONCE, with a reason (``refusal``),
so the model gets another round and either uses the device's own tools or tells the owner in one line that the
device can't do it. The guard is a nudge, not a wall: it refuses at most one call a turn, and it lifts as soon as
one of the claimed device's own tools has run ("turn off the lights and open the spreadsheet" gets both), when the
owner types more into the turn, or when they withdraw a gated question with new words.

One Jev request carries both layers (docs.typesafe.ai's function-calling pattern: every question in one request,
the tree lives in code and reads only the chosen branch). It is its OWN call, beside the toolset router's and the
grade router's: a question added to either would change the call their gates were fitted on.

* The state is the message, the owner's previous message and one line saying what the text is, as toolset_router
  builds them, plus ``things``: one line per live device saying in plain words what it can do, built from that
  device's real tool schemas (``THINGS``; test_app_router.py checks every schema action is covered). A Noul sees
  only the state, never another question's options (typed_ask.py l.597-599), so the lines must be there.
* Layer 1: one Choice ("which of these is the request for") over "something else", the Mac's own screen and the
  live devices, in that order (Jev leans to the first option, so the option that claims nothing is first); one
  absolute Noul per live device ("is the owner asking for something to happen on their TV ..."); and one Noul for
  the Mac's own screen. A Choice always picks something and a relative pick is not a gate (typed_ask.py
  l.229-237), so a device is CLAIMED only when the Choice picks it with a probability over its fitted gate, AND
  its own Noul clears its fitted gate, AND its Noul leads the Mac's Noul by the fitted margin.
* Layer 2, per live device, in the same request: a Choice over that device's methods, and a Noul "is what is asked
  one of the things it can do" (its capability line again). Code reads only the claimed device's two answers.
* An error, a timeout, a missing key or a missing answer is no claim: the turn is today's, byte for byte. Edges are
  combined with the minimum, never a product.

Which devices are asked about is read from the tools the turn offers (``live``): a Mac without a TV sends no
question about one. The camera is live when its own tools are offered or when take_photo is (the Mac's camera).
Every question is in the owner's words for things and names none of eve's tools, no model and no device's brand
name (the owner's rule: a name in a prompt is an option the model may fill a slot with). One app is named, a right
name: the TV's method words say "search YouTube on it", the one app its search works in.

Fitting (tools/app_route_eval.py, the house recipe of tools/toolset_eval.py): live Jev runs once over the tuning
set (app_tuning.json; answers cached by case id and by a hash of the questions) and ``fit`` picks, per device, the
gates that claim NO tuning case of another truth (a Mac case, a "neither" case, another device's case) and then as
many of the device's own cases as that allows; between the strictest such gates and the loosest each gate could go
alone, it takes the halfway point rounded to the strict side. Then live Jev runs ONCE over the blind holdout
(app_holdout.json) against the bar below. Nothing is refitted after the holdout.

After round 1 (2026-11-11, a review of this branch): a floor under the fitted gates (``FLOOR``, applied by
``load_gates``), the nudge-once guard, three rules in ``mac_app_only`` and new wording for a request the device
can't do. Round 1's sets are seen data now, so SHIPPED is False until a second blind round is scored with all of
that in place (tools/app_route_eval.py --holdout2); docs/app-router.md, "Round 2".

PRE-REGISTERED BAR (written 2026-11-11 by the orchestrator, before any tuning or holdout result existed), verbatim:
  Router level, scored ONCE on the blind holdout (writer B, 120 cases, never seen by the builder), gates fitted on
  the tuning set only:
  1. On holdout cases whose truth is a device app: the router names that app (right app, whether or not it can do
     it) on >= 0.90 of them.
  2. On holdout cases whose truth is the Mac: the router claims a device app (which would refuse the Mac agent) on
     <= 0.03 of them.
  3. On holdout cases whose truth is neither: the router claims a device app on <= 0.05 of them.
  4. Jev latency p95 <= 800 ms over the holdout run; an error or timeout yields no claim.
  SHIPPED is True only if all four hold. Reported, not gated: can/can't accuracy, method accuracy, per-app confusion.

``EVE_APP_ROUTER`` = auto (the default: on when SHIPPED) | on (a live trial before or despite the bar) | off.
eve lite forces it off (profile.LITE_FORCED). Jev is a third party: with the router on, the owner's message and the
previous one leave the Mac on every routed turn, as they already do for the toolset router.
"""

from __future__ import annotations

import hashlib
import json
import logging
import os
import threading
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Callable, Iterable, Mapping, Optional, Sequence

log = logging.getLogger(__name__)

Predict = Callable[[Any, dict[str, Any]], dict[str, Any]]

MODES = ("auto", "on", "off")
# Jev's whole budget at a turn's start, the toolset router's: past it the turn has no claim, which is today's turn.
TIMEOUT_S = 0.8
GRID = tuple(round(0.05 * i, 2) for i in range(1, 20))                    # 0.05 .. 0.95: a Noul's or the Choice's gate
# How far a device's Noul must lead the Mac's: -1.0 never binds; 0.0 is "at least level with the Mac's".
LEAD_GRID = (-1.0,) + tuple(round(0.1 * i, 1) for i in range(-5, 10))     # -1.0, -0.5 .. 0.9
# Beside this module and in package-data, so an installed eve carries it (grade_gate.json's lesson: under
# tests/fixtures a wheel had no file and fell back without a word). tools/app_route_eval.py --fit writes it.
GATES_FILE = Path(__file__).resolve().parent / "app_gates.json"
# Round 2 decides it; the orchestrator flips it. Round 1 (2026-11-11) held every line of the bar (80/80 device cases
# named, 0/24 Mac and 0/16 neither cases claimed, Jev p95 418 ms), but the gate floor, the nudge-once guard, the
# mac_app_only rules and the can't wording were all changed after it, on its data. A rule changed on seen data has
# no blind verdict, so this is False again until tools/app_route_eval.py --holdout2 and the end-to-end arms are
# scored on sets nobody here has read (docs/app-router.md, "Round 2").
SHIPPED = False

MAC, OTHER = "mac", "other"            # the Choice's two options that claim nothing
START_TASK = "start_task"              # the one call a claim refuses (telegram._think)


# ---- the devices, from their real tool schemas ----------------------------------------------------------------------

@dataclass(frozen=True)
class Method:
    """One thing a device can do: the owner's words for it (the capability line and the layer-2 option), the tool
    that does it, and the schema members it stands for (the tool's ``action`` values, or its parameters), which
    test_app_router.py checks against the module's own schema so a new action cannot go unlisted."""
    key: str
    words: str
    tool: str
    covers: tuple[str, ...] = ()


@dataclass(frozen=True)
class Thing:
    key: str                           # the claim's name, and the datasets' label: tv, lights, music, camera
    slot: str                          # its app_registry slot
    name: str                          # the owner's words, in a sentence: "the TV"
    full: str                          # how its capability line starts: "The camera on the computer"
    option: str                        # its option in layer 1's Choice
    noul: str                          # layer 1's absolute question
    methods: tuple[Method, ...]

    @property
    def tools(self) -> tuple[str, ...]:
        """Every tool that is this device's, in method order, once each."""
        return tuple(dict.fromkeys(m.tool for m in self.methods))


_ALSO = "Yes also when it is something {it} may not be able to do."

THINGS: tuple[Thing, ...] = (
    Thing("tv", "firetv", "the TV", "The TV",
          "their TV: its power, picture, sound, inputs, apps or what it plays",
          "Is the owner asking for something to happen on their TV, or to be told something about their TV: its "
          "power, its picture or sound, its inputs, its apps, or what it shows or plays? "
          + _ALSO.format(it="the TV") + " No when it is for the computer's own screen, a phone or another device.",
          (Method("power", "turn it on or off", "firetv_control", ("power_on", "power_off")),
           Method("volume", "set its volume, mute or unmute it", "firetv_control", ("volume", "mute", "unmute")),
           Method("keys", "press its remote's keys (up, down, left, right, select, back, home, menu)",
                  "firetv_control", ("up", "down", "left", "right", "select", "back", "home", "menu")),
           Method("playback", "play, pause, skip, rewind or fast-forward what is on", "firetv_control",
                  ("play_pause", "play", "pause", "next", "previous", "rewind", "fast_forward")),
           Method("open_app", "open an app on it by name", "firetv_control", ("open_app",)),
           Method("youtube", "search YouTube on it", "firetv_control", ("youtube_search",)),
           Method("typing", "type into a search box on it", "firetv_control", ("type_text",)),
           Method("status", "tell whether it is on, which app is in front and its volume", "firetv_status"),
           Method("picture", "send a picture of what its screen shows", "firetv_screenshot"))),
    Thing("lights", "lights", "the lights", "The lights in the home",
          "the lights in their home",
          "Is the owner asking for something to happen with the lights in their home, or to be told something "
          "about those lights? " + _ALSO.format(it="a light")
          + " No for a screen's brightness, and no for a light that is part of another device.",
          (Method("power", "turn them on or off", "lights_set", ("power",)),
           Method("brightness", "make them brighter or dimmer", "lights_set", ("brightness",)),
           Method("colour", "set a colour or a white (warm, cool, daylight)", "lights_set", ("color",)),
           Method("match", "make lights match another light", "lights_match"),
           Method("status", "tell whether they are on, how bright and what colour", "lights_status"))),
    Thing("music", "spotify", "the music", "The music player",
          "their music: what plays, how loud and on which speaker",
          "Is the owner asking for music or other audio to be played, paused, skipped, made louder or quieter or "
          "moved to another speaker, or to be told what is playing? " + _ALSO.format(it="the music player")
          + " No for a video, a film or a show, and no for another device's own volume.",
          (Method("play", "play or resume music, or play a song, an artist, an album, a playlist or a mood by "
                          "name", "spotify_control", ("play",)),
           Method("pause", "pause the music", "spotify_control", ("pause",)),
           Method("skip", "skip to the next track or go back to the previous one", "spotify_control",
                  ("next", "previous")),
           Method("volume", "set the music's volume", "spotify_control", ("volume",)),
           Method("transfer", "move the music to another speaker or device", "spotify_control", ("transfer",)),
           Method("mode", "turn the desk speaker's music mode on or off", "spotify_control",
                  ("mode_on", "mode_off")),
           Method("status", "tell what is playing and on which device", "spotify_now_playing"))),
    Thing("camera", "obsbot", "the camera", "The camera on the computer",
          "their camera, the one on the computer: a photo taken now, or where the camera points",
          "Is the owner asking for something to be done with their camera (the camera on their computer is that "
          "camera): a photo or a video taken now, or the camera turned, zoomed, made to follow someone, woken or "
          "put to sleep, or to be told where it points? " + _ALSO.format(it="the camera")
          + " No for a picture of the computer's screen, no for photos that already exist, and no for the camera "
          "setting inside a video call or another app.",
          (Method("photo", "take a picture with it now and send it", "take_photo"),
           Method("aim", "turn it, point it or center it", "camera_control", ("turn", "point", "center")),
           Method("zoom", "zoom in or out", "camera_control", ("zoom",)),
           Method("power", "wake it or put it to sleep", "camera_control", ("wake", "sleep")),
           Method("tracking", "follow a person or stop following, and set how it follows", "camera_control",
                  ("track_on", "track_off", "mode")),
           Method("preset", "go to a saved position", "camera_control", ("preset",)),
           Method("status", "tell where it points, its zoom and whether it is awake", "camera_status"))),
)
KEYS: tuple[str, ...] = tuple(t.key for t in THINGS)
# Every tool a device may own; the eval asks with all of them offered (every device live, as on the owner's Mac).
ALL_TOOLS: frozenset[str] = frozenset(tool for t in THINGS for tool in t.tools)


def thing(key: str) -> Optional[Thing]:
    return next((t for t in THINGS if t.key == key), None)


@dataclass(frozen=True)
class Live:
    """A device as this turn has it: the methods whose tool the turn offers, and those tools in order."""
    thing: Thing
    methods: tuple[Method, ...]

    @property
    def key(self) -> str:
        return self.thing.key

    @property
    def tools(self) -> tuple[str, ...]:
        return tuple(dict.fromkeys(m.tool for m in self.methods))

    @property
    def line(self) -> str:
        """Its capability line: a positive list, and the statement that the list is whole."""
        return (f"{self.thing.full} can do exactly these things and nothing else: "
                + "; ".join(m.words for m in self.methods) + ".")


def live(offered: Iterable[str]) -> tuple[Live, ...]:
    """The devices this turn can work, in THINGS order: each with the methods whose tool is among ``offered``. A
    device none of whose tools is offered is not there (its questions are never sent)."""
    have = set(offered)
    out = []
    for t in THINGS:
        methods = tuple(m for m in t.methods if m.tool in have)
        if methods:
            out.append(Live(t, methods))
    return tuple(out)


# ---- the question ---------------------------------------------------------------------------------------------------

ABOUT = ("A message the owner said or typed to their personal assistant, which works their computer and the "
         "devices listed under things. When a previous message is given, the new one may be a follow-up to it: "
         "judge what the new message asks for, reading the previous one only as context.")
WHICH = "Which of these is the owner's request for?"
OTHER_OPTION = ("something else: a question, a chat, a search, their mail, calendar, notes or files, or anything "
                "that is for none of the others")
MAC_OPTION = "the computer itself: an app, a website, a window or a setting on its own screen"
MAC_NOUL = ("Is the owner asking for something to be done on the computer itself: opening or using an app, a "
            "website, a window or a setting on its own screen, a video call included? No when what they ask for "
            "happens on one of the devices listed under things, and no for a question that needs no screen.")
HOW = "Which of these is the owner asking {name} to do?"
CAN = "{line} Is what the owner asks of {name} one of those things?"


def _q(kind: str, key: str) -> str:
    """A question's id: ``for_tv``, ``how_tv``, ``can_tv``."""
    return f"{kind}_{key}"


def state(text: str, previous: str, things: Sequence[Live]) -> dict[str, str]:
    """What Jev reads: toolset_router's three fields, cut the same way, plus one capability line per live device."""
    return {"about": ABOUT, "things": "\n".join(t.line for t in things),
            "previous": " ".join(str(previous or "").split())[:500],
            "request": " ".join(str(text or "").split())[:500]}


def questions(things: Sequence[Live]) -> dict[str, Any]:
    """Both layers for the live devices, in one request. Layer 1: ``which`` (the option that claims nothing first),
    ``for_mac`` and one ``for_<thing>``. Layer 2, per device: ``how_<thing>`` and ``can_<thing>``."""
    out: dict[str, Any] = {
        "which": {"type": "choice", "instructions": WHICH,
                  "criteria": {OTHER: OTHER_OPTION, MAC: MAC_OPTION, **{t.key: t.thing.option for t in things}}},
        _q("for", MAC): {"type": "noul", "instructions": MAC_NOUL},
    }
    for t in things:
        out[_q("for", t.key)] = {"type": "noul", "instructions": t.thing.noul}
    for t in things:
        out[_q("how", t.key)] = {"type": "choice", "instructions": HOW.format(name=t.thing.name),
                                 "criteria": {m.key: m.words for m in t.methods}}
        out[_q("can", t.key)] = {"type": "noul", "instructions": CAN.format(line=t.line, name=t.thing.name)}
    return out


def fingerprint(things: Sequence[Live]) -> str:
    """A hash of everything Jev is asked besides the message: the about line, the capability lines and every
    question. The eval caches answers under it, so a reworded question never reads an old answer."""
    blob = json.dumps({"state": state("", "", things), "questions": questions(things)}, sort_keys=True)
    return hashlib.sha256(blob.encode("utf-8")).hexdigest()[:16]


# ---- Jev's answer ---------------------------------------------------------------------------------------------------

@dataclass(frozen=True)
class AppAnswer:
    """Jev's answers to one message, as numbers. ``asked``: the devices the request asked about."""
    asked: tuple[str, ...] = ()
    which: str = ""                                              # the Choice's pick: a device, "mac" or "other"
    probs: Mapping[str, float] = field(default_factory=dict)     # the Choice's probability per option
    nouls: Mapping[str, float] = field(default_factory=dict)     # "mac" and each device -> its layer-1 Noul
    how: Mapping[str, tuple[str, float]] = field(default_factory=dict)   # device -> (method, its probability)
    can: Mapping[str, float] = field(default_factory=dict)       # device -> "it can do this" Noul
    ms: float = 0.0
    error: str = ""

    def to_json(self) -> dict[str, Any]:
        r = lambda v: round(float(v), 4)  # noqa: E731
        return {"asked": list(self.asked), "which": self.which, "probs": {k: r(v) for k, v in self.probs.items()},
                "nouls": {k: r(v) for k, v in self.nouls.items()},
                "how": {k: [m, r(p)] for k, (m, p) in self.how.items()},
                "can": {k: r(v) for k, v in self.can.items()}, "ms": round(self.ms, 1), "error": self.error}

    @staticmethod
    def from_json(raw: Mapping[str, Any]) -> "AppAnswer":
        f = lambda d: {str(k): float(v) for k, v in (d or {}).items()}  # noqa: E731
        return AppAnswer(tuple(str(k) for k in raw.get("asked") or ()), str(raw.get("which") or ""),
                         f(raw.get("probs")), f(raw.get("nouls")),
                         {str(k): (str(v[0]), float(v[1])) for k, v in (raw.get("how") or {}).items()},
                         f(raw.get("can")), float(raw.get("ms") or 0.0), str(raw.get("error") or ""))


def _noul(answer: Any) -> Optional[float]:
    v = answer.get("noul") if isinstance(answer, dict) else None
    return float(v) if isinstance(v, (int, float)) and not isinstance(v, bool) else None


def _choice(answer: Any, options: Iterable[str]) -> Optional[tuple[str, dict[str, float]]]:
    """(the pick, each option's probability), or None unless the pick is one of ``options`` and has a probability."""
    if not isinstance(answer, dict):
        return None
    key = answer.get("choice")
    raw = answer.get("probabilities")
    probs = {str(k): float(v) for k, v in raw.items()
             if isinstance(v, (int, float)) and not isinstance(v, bool)} if isinstance(raw, dict) else {}
    if not isinstance(key, str) or key not in set(options) or key not in probs:
        return None
    return key, probs


def ask(predict: Predict, text: str, previous: str, offered: Iterable[str],
        clock: Callable[[], float] = time.perf_counter) -> AppAnswer:
    """Jev's answers for one message, with the devices ``offered`` makes live. A failure, or any question that
    came back without its typed answer, is an answer with ``error`` set: it claims nothing. No live device: no
    call at all."""
    things = live(offered)
    asked = tuple(t.key for t in things)
    if not things:
        return AppAnswer(error="no device")
    t0 = clock()
    try:
        answers = predict(state(text, previous, things), questions(things)).get("answers") or {}
    except Exception as e:  # noqa: BLE001 — a model that fails abstains: the turn is today's
        return AppAnswer(asked, ms=(clock() - t0) * 1000.0, error=f"{type(e).__name__}: {e}"[:160])
    ms = (clock() - t0) * 1000.0
    if not isinstance(answers, dict):
        return AppAnswer(asked, ms=ms, error="no answers")
    picked = _choice(answers.get("which"), (OTHER, MAC, *asked))
    nouls = {key: _noul(answers.get(_q("for", key))) for key in (MAC, *asked)}
    how = {t.key: _choice(answers.get(_q("how", t.key)), (m.key for m in t.methods)) for t in things}
    can = {key: _noul(answers.get(_q("can", key))) for key in asked}
    missing = (["which"] if picked is None else []) + [_q("for", k) for k, v in nouls.items() if v is None] + [
        _q("how", k) for k, v in how.items() if v is None] + [_q("can", k) for k, v in can.items() if v is None]
    if missing:
        return AppAnswer(asked, ms=ms, error="missing: " + ", ".join(missing)[:140])
    assert picked is not None
    return AppAnswer(asked, picked[0], picked[1], {k: float(v or 0.0) for k, v in nouls.items()},
                     {k: (v[0], v[1][v[0]]) for k, v in how.items() if v is not None},
                     {k: float(v or 0.0) for k, v in can.items()}, ms)


# ---- the rule -------------------------------------------------------------------------------------------------------

@dataclass(frozen=True)
class Gate:
    """One device's fitted cut-offs. It is claimed when the Choice picks it at ``choice`` or more, its own Noul is
    ``noul`` or more, and that Noul leads the Mac's by ``lead`` or more. ``can``: at or over it, it can do it."""
    noul: float
    choice: float
    lead: float
    can: float

    def to_json(self) -> dict[str, float]:
        return {"noul": self.noul, "choice": self.choice, "lead": self.lead, "can": self.can}


# Used only until app_gates.json is fitted (tools/app_route_eval.py --fit): Jev's own documented starting point for
# a yes (0.7), the Choice better than even, the device's Noul not behind the Mac's.
DEFAULT_GATE = Gate(noul=0.7, choice=0.5, lead=0.0, can=0.5)
# A claim's gates are never looser than this, whatever app_gates.json says (``floored``, applied by ``load_gates``):
# the device's own Noul at least even, the Choice at least even, the device's Noul not behind the Mac's. DEFAULT_GATE's
# choice and lead, and an even Noul. ``can`` has no floor: it is read after the claim and is left as fitted.
# Why: on the tuning set the Choice never picked the lights, the music or the camera for a Mac or a "neither" case,
# so nothing held their fitted gates up and each landed "halfway to the grid floor" (music: Noul 0.15, Choice 0.35,
# lead -0.5). Gates that low are the Choice alone deciding, and this module's own lesson is that a relative Choice is
# not a gate. The fit found no negative; that is not evidence that there is none.
FLOOR = Gate(noul=0.5, choice=0.5, lead=0.0, can=0.0)


@dataclass(frozen=True)
class AppRoute:
    """One turn's verdict. ``app`` is the claimed device's key ("tv", "lights", "music", "camera"), or "" for no
    claim; the empty value is ``NO_CLAIM``. ``can`` and ``method`` are layer 2's, read only for the claimed device.
    ``p_app`` is the weakest of the claim's edges (the minimum, never a product)."""
    app: str = ""
    can: bool = True
    method: str = ""
    p_app: float = 0.0
    ms: float = 0.0                    # Jev's time; 0 when it was not asked
    error: str = ""                    # Jev's error, or "timeout"
    asked: bool = False                # a router was there to ask, and a device was live
    leaning: str = ""                  # the Choice's pick when nothing was claimed, for the log line

    def __bool__(self) -> bool:
        return bool(self.app)

    def log_line(self) -> str:
        """Names and numbers only, never the message: "tv (cannot, p 0.83); jev 212 ms"."""
        if not self.asked:
            return "none; jev off"
        if self.app:
            what = f"{self.app} ({'can' if self.can else 'cannot'}" + (f", {self.method}" if self.method else "") \
                   + f", p {self.p_app:.2f})"
        else:
            what = "none" + (f" (leaning {self.leaning} {self.p_app:.2f})" if self.leaning else "")
        return f"{what}; jev {self.ms:.0f} ms" + (f" ({self.error})" if self.error else "")


NO_CLAIM = AppRoute()


def edges(a: AppAnswer, key: str) -> tuple[float, float, float]:
    """A device's three edges in one answer: its Noul, the Choice's probability for it (0 unless the Choice picked
    it: a device the Choice did not pick is never claimed), and its Noul's lead over the Mac's."""
    n = float(a.nouls.get(key, 0.0))
    c = float(a.probs.get(key, 0.0)) if a.which == key else 0.0
    return n, c, n - float(a.nouls.get(MAC, 0.0))


def claims(a: AppAnswer, key: str, gate: Gate, budget_ms: float = float("inf")) -> bool:
    """Whether ``a`` claims the device ``key`` under ``gate``. Never on an error or an answer slower than the turn
    waits; never a device the request did not ask about."""
    if a.error or a.ms > budget_ms or key not in a.asked:
        return False
    n, c, lead = edges(a, key)
    return c >= gate.choice and n >= gate.noul and lead >= gate.lead


def decide(a: AppAnswer, gates: Mapping[str, Gate], budget_ms: float = float("inf")) -> AppRoute:
    """The claim in one answer, or no claim. A device with no gate is never claimed."""
    if a.error or a.ms > budget_ms:
        return AppRoute(ms=a.ms, error=a.error or "slow", asked=bool(a.asked))
    key = a.which
    if key in a.asked:
        n, c, _lead = edges(a, key)
        weakest = min(n, c)
    else:
        weakest = float(a.probs.get(key, 0.0))
    gate = gates.get(key)
    if gate is None or not claims(a, key, gate, budget_ms):
        return AppRoute(p_app=weakest, ms=a.ms, asked=True, leaning=key)
    can = float(a.can.get(key, 0.0)) >= gate.can
    method = a.how.get(key, ("", 0.0))[0]
    return AppRoute(app=key, can=can, method=method if can else "", p_app=weakest, ms=a.ms, asked=True)


# ---- what a claim tells the turn ------------------------------------------------------------------------------------

def owned(route: AppRoute, offered: Iterable[str]) -> tuple[str, ...]:
    """The claimed device's tools that this turn really offers, in order; () with no claim."""
    t = thing(route.app)
    have = set(offered)
    return tuple(name for name in t.tools if name in have) if t is not None else ()


def note_line(route: AppRoute, offered: Iterable[str]) -> str:
    """The line a claim adds to the turn's developer note, or "": which device the request is for, the tools that
    are that device's this turn, the method read as a hint, and, when it reads as something the device can't do,
    that the answer is one line saying so (with no invitation to call a tool just to check). Built by code from
    the claim; positive wording only."""
    t = thing(route.app)
    tools = owned(route, offered)
    if t is None or not tools:
        return ""
    head = f"This request is for {t.name}. Its tools this turn: {', '.join(tools)}."
    if not route.can:
        return (f"{head} It reads as something {t.name} can't do. If that is so, tell the owner in one line that "
                f"{t.name} can't do it.")
    m = next((m for m in t.methods if m.key == route.method and m.tool in tools), None)
    hint = f" It reads as a request to {m.words} ({m.tool})." if m is not None else ""
    return f"{head}{hint} Use them now; when they can't do what was asked, tell the owner so in one line."


def refusal(route: AppRoute, offered: Iterable[str]) -> str:
    """Why a start_task call is refused while a device is claimed, or "" when nothing is. It branches as
    ``note_line`` does. When the device can do it: the tools that are that device's, and what to do when they can't
    do it. When it reads as something the device can't do: only that, and to tell the owner so in one line; no
    tool is named, so nothing invites a call on a device judged unable. Never repeats a wrong name."""
    t = thing(route.app)
    tools = owned(route, offered)
    if t is None or not tools:
        return ""
    if not route.can:
        return (f"not started: this request is for {t.name}, and it reads as something {t.name} can't do. Tell the "
                f"owner so in one line.")
    return (f"not started: this request is for {t.name}, which you work with {', '.join(tools)}. Use one of those "
            f"this turn; when they can't do what the owner asked, tell the owner so in one line.")


# ---- the fit --------------------------------------------------------------------------------------------------------

def _strict_side(lo: float, hi: float, grid: Sequence[float]) -> float:
    """The grid value halfway between ``lo`` and ``hi``, rounded to the strict (higher) side."""
    mid = (lo + hi) / 2.0
    return next((g for g in grid if g >= mid - 1e-9), grid[-1])


def fit_claim(answers: Sequence[AppAnswer], truths: Sequence[str], key: str,
              budget_ms: float = TIMEOUT_S * 1000.0) -> tuple[Gate, dict[str, Any]]:
    """One device's three claim gates from the tuning answers, and what the fit saw.

    1. A gate triple is allowed only if it claims NO tuning case whose truth is something else (the Mac, neither,
       or another device).
    2. Of the allowed triples, keep those that claim the most of the device's own cases.
    3. Of those, take the strictest (the highest gates): S.
    4. For each gate alone, find how far below S it could go with the other two at S and rule 1 still holding;
       take the grid point halfway, rounded to the strict side. If the three halfway gates together break rule 1,
       keep S.
    No allowed triple claims any own case: the top of each grid (it claims nothing), and the eval says so."""
    rows = [(edges(a, key), truth == key) for a, truth in zip(answers, truths, strict=True)
            if not a.error and a.ms <= budget_ms and a.which == key and key in a.asked]
    own_total = sum(1 for t in truths if t == key)

    def count(n: float, c: float, lead: float) -> tuple[int, int]:
        """(own cases claimed, other cases claimed)."""
        own = wrong = 0
        for (rn, rc, rl), mine in rows:
            if rn >= n and rc >= c and rl >= lead:
                own, wrong = own + mine, wrong + (not mine)
        return own, wrong

    best: Optional[tuple[int, float, tuple[float, float, float]]] = None
    for n in GRID:
        for c in GRID:
            for lead in LEAD_GRID:
                own, wrong = count(n, c, lead)
                if wrong:
                    continue
                cand = (own, n + c + lead, (n, c, lead))
                if best is None or cand > best:
                    best = cand
    if best is None or best[0] == 0:
        gate = Gate(GRID[-1], GRID[-1], LEAD_GRID[-1], DEFAULT_GATE.can)
        return gate, {"own": own_total, "claimed": 0, "wrong": 0, "note": "no gates claim an own case with zero "
                                                                         "wrong claims; this device is never claimed"}
    top, strict = best[0], best[2]
    grids = (GRID, GRID, LEAD_GRID)
    loose = list(strict)
    for d in range(3):
        for g in grids[d]:                                  # ascending: the first that keeps rule 1 is the loosest
            trial = list(strict)
            trial[d] = g
            if g <= strict[d] and count(*trial)[1] == 0:
                loose[d] = g
                break
    half = tuple(_strict_side(loose[d], strict[d], grids[d]) for d in range(3))
    chosen = half if count(*half) == (top, 0) else strict
    own, wrong = count(*chosen)
    return (Gate(chosen[0], chosen[1], chosen[2], DEFAULT_GATE.can),
            {"own": own_total, "claimed": own, "wrong": wrong, "strictest": list(strict), "loosest": loose})


def fit_can(answers: Sequence[AppAnswer], truths: Sequence[str], cans: Sequence[Optional[bool]], key: str) -> float:
    """One device's "it can do this" gate, from its own tuning cases that say whether it can. Calling a doable
    request undoable is the worse error, so: the gate never goes above the lowest Noul of a case that CAN be done;
    under that limit it sits halfway to the highest Noul of a case that can't, rounded to the low side. No labelled
    case: DEFAULT_GATE's."""
    yes = [float(a.can.get(key, 0.0)) for a, t, c in zip(answers, truths, cans, strict=True)
           if t == key and c is True and not a.error and key in a.asked]
    no = [float(a.can.get(key, 0.0)) for a, t, c in zip(answers, truths, cans, strict=True)
          if t == key and c is False and not a.error and key in a.asked]
    if not yes and not no:
        return DEFAULT_GATE.can
    limit = max((g for g in GRID if g <= min(yes) + 1e-9), default=GRID[0]) if yes else GRID[-1]
    if not no:
        return min(limit, DEFAULT_GATE.can)
    mid = (max(no) + (min(yes) if yes else 1.0)) / 2.0
    under = max((g for g in GRID if g <= mid + 1e-9), default=GRID[0])
    return min(limit, under)


def fit(answers: Sequence[AppAnswer], truths: Sequence[str], cans: Sequence[Optional[bool]],
        budget_ms: float = TIMEOUT_S * 1000.0) -> tuple[dict[str, Gate], dict[str, dict[str, Any]]]:
    """Every device's gates (fit_claim, fit_can) and what each fit saw. The devices are independent: the Choice
    picks one, so one device's gates never change another's claims."""
    gates: dict[str, Gate] = {}
    seen: dict[str, dict[str, Any]] = {}
    for key in KEYS:
        gate, info = fit_claim(answers, truths, key, budget_ms)
        gates[key] = Gate(gate.noul, gate.choice, gate.lead, fit_can(answers, truths, cans, key))
        seen[key] = info
    return gates, seen


def fitted_gates(path: Path = GATES_FILE) -> dict[str, Gate]:
    """app_gates.json's gates exactly as the fit wrote them (history: the turn never reads these, ``load_gates``
    does); DEFAULT_GATE for every device when the file is missing or unreadable."""
    try:
        raw = json.loads(path.read_text(encoding="utf-8"))["gates"]
        return {key: Gate(float(raw[key]["noul"]), float(raw[key]["choice"]), float(raw[key]["lead"]),
                          float(raw[key]["can"])) for key in KEYS}
    except Exception:  # noqa: BLE001
        return dict.fromkeys(KEYS, DEFAULT_GATE)


def floored(gates: Mapping[str, Gate]) -> dict[str, Gate]:
    """``gates`` with each claim gate (noul, choice, lead) raised to FLOOR's where the fit left it looser; ``can``
    as fitted. A gate already at or over the floor is unchanged, so the floor can only remove claims."""
    return {key: Gate(max(g.noul, FLOOR.noul), max(g.choice, FLOOR.choice), max(g.lead, FLOOR.lead), g.can)
            for key, g in gates.items()}


def load_gates(path: Path = GATES_FILE) -> dict[str, Gate]:
    """The gates a turn is decided with: app_gates.json's, never looser than FLOOR (the max per claim field, in
    code, whatever the file says). The file keeps what the fit wrote. DEFAULT_GATE for every device when the file
    is missing or unreadable."""
    return floored(fitted_gates(path))


GATES: dict[str, Gate] = load_gates()          # read once, at import: the file changes only on a refit


# ---- the router -----------------------------------------------------------------------------------------------------

Router = Callable[[str, str, frozenset[str]], AppAnswer]


def mode(environ: Optional[Mapping[str, str]] = None) -> str:
    """EVE_APP_ROUTER = auto (the default: on when SHIPPED) | on (a live trial before or despite the bar) | off
    (no call, no note, no guard: the turn as it was). Parsed as EVE_TOOLSET_ROUTER is."""
    env = os.environ if environ is None else environ
    raw = (env.get("EVE_APP_ROUTER") or "auto").strip().lower()
    if raw in ("1", "true", "yes"):
        raw = "on"
    if raw in ("0", "false", "no"):
        raw = "off"
    if raw not in MODES:
        log.warning("app router: EVE_APP_ROUTER=%r is not one of %s; auto", raw, MODES)
        raw = "auto"
    return raw


_JEV: dict[str, Any] = {}
_JEV_LOCK = threading.Lock()


def make_router(environ: Optional[Mapping[str, str]] = None) -> Optional[Router]:
    """``router(text, previous, offered) -> AppAnswer`` over EVE_JEV_ROUTE (TIMEOUT_S), made once a process, or
    None: the mode is off (or auto while not SHIPPED), or no Jev route is configured. None means no claim, ever."""
    from . import jev

    env = os.environ if environ is None else environ
    m = mode(env)
    if m == "off" or (m == "auto" and not SHIPPED):
        return None
    try:
        url, key, model = jev.route_config(env)
    except Exception:  # noqa: BLE001
        return None
    # the key is part of the route (a rotated key makes a new router), kept only as a fingerprint
    route = (url, model, hashlib.sha256(str(key).encode()).hexdigest()[:16])
    with _JEV_LOCK:
        cached = _JEV.get("router")
        if cached is None or _JEV.get("route") != route:
            predict = jev.make_predict(url, key, model, timeout_s=TIMEOUT_S)
            cached = lambda text, previous, offered: ask(predict, text, previous, offered)  # noqa: E731
            _JEV.update(router=cached, route=route)
        return cached


# Installed apps whose name is itself a word for one of the devices or for the home (on a Mac: TV, Music, Home).
# "open the tv" or "start the music" may be the device, so mac_app_only does not answer for them: Jev is asked.
DEVICE_WORD_APPS = frozenset({"tv", "music", "home"})


def mac_app_only(text: str, apps: Optional[Iterable[str]] = None) -> bool:
    """True when the message is NOTHING BUT opening or quitting an app installed on this Mac ("open Spotify",
    "quit Slack"): code answers before Jev is asked and no device claims it. Jev reads a bare "open Spotify" as the
    TV's (seen live 2026-11-11, after the holdout). Three rules:

    1. Nothing but: a launch with anything after it ("open Spotify and play something") is not one, and neither is
       a message that names where ("open Spotify on the tv"). Those are asked.
    2. The wording is the planner's: task_router.bare_launch_or_quit is ``classify``'s own verdict, so the polite and
       spoken lead-ins it knows count too ("can you open Spotify", "okay, open Spotify.", "could you quit Slack?").
    3. Not for an app whose name is a word for a device or the home (DEVICE_WORD_APPS): "open the tv" is asked.

    ``apps`` is the installed apps (task_router.installed_apps() when None, read once a process)."""
    from . import task_router

    try:
        names = task_router.installed_apps() if apps is None else tuple(apps)
        app = task_router.bare_launch_or_quit(text or "", names)
        return bool(app) and app.casefold() not in DEVICE_WORD_APPS
    except Exception:  # noqa: BLE001 — a rule that fails decides nothing: Jev is asked as before
        return False


def route_turn(text: str, previous: str = "", router: Optional[Router] = None, offered: Iterable[str] = (),
               gates: Optional[Mapping[str, Gate]] = None, timeout_s: float = TIMEOUT_S,
               mac_apps: Optional[Iterable[str]] = None) -> AppRoute:
    """One turn's claim. Blocking (call it with asyncio.to_thread). Jev runs in a worker waited on for
    ``timeout_s`` at most; a router that is None, raises, errs, answers with anything else or is slower than that
    claims nothing, and so does a turn that offers no device's tool (then Jev is not asked at all)."""
    tools = frozenset(str(n) for n in offered) & ALL_TOOLS
    if router is None or not live(tools):
        return NO_CLAIM
    if mac_app_only(text, mac_apps):
        return NO_CLAIM
    box: dict[str, Any] = {}
    done = threading.Event()
    t0 = time.perf_counter()

    def run() -> None:
        try:
            box["a"] = router(text, previous, tools)
        except Exception as e:  # noqa: BLE001 — a router that fails claims nothing
            box["a"] = AppAnswer(error=type(e).__name__)
        finally:
            done.set()

    threading.Thread(target=run, name="app-router", daemon=True).start()
    if not done.wait(max(0.0, timeout_s)):
        return AppRoute(ms=(time.perf_counter() - t0) * 1000.0, error="timeout", asked=True)
    a = box.get("a")
    try:
        if not isinstance(a, AppAnswer):
            raise TypeError("not an answer")
        route = decide(a, GATES if gates is None else gates, timeout_s * 1000.0)
        # a claim must be for a device this turn can work: what Jev sent cannot name another
        if route.app and not owned(route, tools):
            raise ValueError("a device this turn does not have")
    except Exception:  # noqa: BLE001 — what Jev sent cannot hurt the turn
        return AppRoute(ms=(time.perf_counter() - t0) * 1000.0, error="not an answer", asked=True)
    return route if route.asked else AppRoute(ms=route.ms, error=route.error, asked=True)
