"""eve's apps, declared once (owner, 2026-10-12: "make it so that apps are variable ... these are just the apps i want").

An app is a module plus one line here. The line says what the module offers and where it reaches: the chat brain's
tools and block (telegram.py), the voice backend's (voice_agent.py), the code path that answers a plain command with
no model turn, the tool gate's sets (tool_gate.py), the phone's Apps list and sheet (link_chat.py), the CLI
(cli.py), and the intent eval's labels (tools/intent_eval.py). Every site loops over ``APPS``; none names an app.

The apps below are the owner's picks, not a fixed set. ``eve-bridge app new <name>`` writes a new one (a module, its
test, its doc and a line above the marker at the end of ``APPS``); docs/adding-an-app.md walks through it.

**Order is model bytes.** ``APPS`` is in tool order: each app's tools and instruction block reach the chat and
the voice models in this order, so moving a line changes what the models read (AGENTS.md rule 6). A new app goes
last, and it is off by default (``default_on=False``), so adding one changes no byte until its switch is on.
``CODE_ORDER`` is the code path's order and ``PHONE_ORDER`` the phone's; an app not named in them comes after the
named ones, in ``APPS`` order.

This module is import-light: strings and sets only. It never imports an app module at import time; a site gets the
module through ``app.mod()``.
"""

from __future__ import annotations

import importlib
import keyword
import math
import re
from dataclasses import dataclass
from types import MappingProxyType, ModuleType
from typing import Any, Iterable, Mapping, Optional

_EMPTY: Mapping[str, Any] = MappingProxyType({})


@dataclass(frozen=True)
class App:
    """One app. ``slot`` is its name everywhere: the chat's ``inlet._<slot>``, ``VoiceSession.<slot>``, the Mini App
    server's attribute, the ``/api/link/controls`` key and ``POST /api/link/controls/<slot>``."""

    slot: str
    module: str                        # eve_bridge.<module>
    make: str                          # the module's factory: the hub, or None when it is off or not set up
    switch: str                        # its env var, e.g. EVE_LIGHTS
    default_on: bool                   # True: on unless 0/false/no/off; False: on only when 1/true/yes/on
    title: str                         # the phone row: its title,
    symbol: str                        # its SF Symbol,
    detail: str                        # and its subtitle
    doc: str                           # its doc, repo-relative
    no_control: str                    # the phone's 404 line: the app is not on this Mac
    tools: bool = True                 # it gives the models tools: hub.tools()/instructions()/handle(),
                                       # module.TOOLS, TOOL_NAMES, OFF_REASON and INSTRUCTIONS (the intent eval
                                       # lends TOOLS and INSTRUCTIONS; INSTRUCTIONS_HEAD for an app whose block
                                       # lists the owner's devices, like the lights, which the eval names itself)
    label: str = ""                    # its intent-eval label
    tool_prefix: str = ""              # every tool name starts with it
    acting: frozenset[str] = frozenset()                         # tools that change something (tool_gate.ACTING)
    clean: frozenset[str] = frozenset()                          # reads that bring in no outside text (tool_gate.CLEAN)
    policy: Mapping[str, str] = _EMPTY                           # a tool's approval policy (tool_gate.POLICY), when
                                                                 # not what policies() derives: acting is routine,
                                                                 # clean and sources are read
    sources: Mapping[str, str] = _EMPTY                          # a read's name in the owner's question
    say: Mapping[str, str] = _EMPTY                              # an act's head out loud (tool_gate._SAY_HEADS)
    own_words: Mapping[str, frozenset[str]] = _EMPTY             # actions answering in eve's own words only
    voice_omit: frozenset[str] = frozenset()                     # tools not offered out loud
    voice_kwargs: Mapping[str, Any] = _EMPTY                     # hub.instructions(**voice_kwargs) out loud
    voice_note: str = ""                                         # a line after its voice block
    code: bool = False                 # a code path: the module's match(text) (else hub.match) and hub.run
    code_taint_tool: str = ""          # the code path's line is outside words when tool_gate.taints(this, ...)
    controls: bool = True              # hub.panel(body) answers POST /api/link/controls/<slot>
    stream: bool = False               # GET /api/link/controls/<slot>/stream
    listing: bool = False              # its legacy controls key is hub.listing() (a list), not a bool
    native_sheet: bool = False         # the phone has Swift for it; any other app gets the generic sheet
    cli: tuple[str, ...] = ()          # eve-bridge words that run cli_module.cli(argv)
    cli_module: str = ""               # "" means module
    cli_help: str = ""

    def policies(self) -> dict[str, str]:
        """Each declared tool's approval policy (one of POLICIES): ``clean`` and ``sources`` names are read, ``acting``
        names are routine, and ``policy`` overrides either. Import-light: it reads the declaration, never the module
        (``check`` asks that every one of the module's TOOL_NAMES has one)."""
        out = dict.fromkeys((*self.clean, *self.sources), "read")
        out.update(dict.fromkeys(self.acting, "routine"))
        out.update(self.policy)
        return out

    def mod(self) -> ModuleType:
        """The app's module, imported on first use (importlib caches it)."""
        return importlib.import_module(f"eve_bridge.{self.module}")

    def cli_mod(self) -> ModuleType:
        return importlib.import_module(f"eve_bridge.{self.cli_module or self.module}")


# Who approves a tool's call (tool_gate.POLICY, docs/adding-an-app.md "Safety"); the set is exactly these five.
#   read            changes nothing; never asks
#   routine         changes something the owner asked for; never asks (the owner's rule, 2026-10-09)
#   asks            a one-way act (deleting, sending, paying): code asks the owner first, every time
#   taint_asks      runs on the owner's own request; asks once the turn has read outside text
#   executor_owned  the tool's own handler asks the owner for the exact act (a purchase, a share)
POLICIES: tuple[str, ...] = ("read", "routine", "asks", "taint_asks", "executor_owned")


def _fs(*names: str) -> frozenset[str]:
    return frozenset(names)


def _map(**kv: Any) -> Mapping[str, Any]:
    return MappingProxyType(dict(kv))


# The owner's apps, in TOOL order (the order their tools and blocks reach the models).
APPS: tuple[App, ...] = (
    App(slot="lights", module="lights", make="make_lights", switch="EVE_LIGHTS", default_on=True,
        title="Lights", symbol="lightbulb", detail="", doc="docs/lights.md",
        no_control="eve has no lights on this computer.",
        label="lights", tool_prefix="lights_",
        acting=_fs("lights_set", "lights_match"), clean=_fs("lights_status"),
        say=_map(lights_set="change the lights", lights_match="change the lights"),
        code=True, listing=True, native_sheet=True, cli=("lights",),
        cli_help="The owner's lights (Govee, HappyLighting, Sylvania/Tuya): scan, list, name, set. "
                 "See docs/lights.md"),
    App(slot="spotify", module="spotify", make="make_spotify", switch="EVE_SPOTIFY", default_on=True,
        title="Music", symbol="music.note", detail="Spotify", doc="docs/spotify.md",
        no_control="eve has no Spotify login on this computer.",
        label="music", tool_prefix="spotify_",
        acting=_fs("spotify_control"),
        sources=_map(spotify_control="Spotify", spotify_now_playing="Spotify"),
        say=_map(spotify_control="change the music"),
        own_words=_map(spotify_control=_fs("pause", "volume", "transfer", "mode_on", "mode_off", "mode")),
        code=True, code_taint_tool="spotify_control", native_sheet=True, cli=("spotify",),
        cli_help="The owner's Spotify (spotKnob's powers): login, status, devices, play, pause, next, previous, "
                 "volume, transfer. See docs/spotify.md"),
    App(slot="firetv", module="firetv", make="make_firetv", switch="EVE_FIRETV", default_on=True,
        title="Remote", symbol="tv", detail="Fire TV", doc="docs/firetv.md",
        no_control="eve has no TV on this computer.",
        label="tv_control", tool_prefix="firetv_",
        acting=_fs("firetv_control"), clean=_fs("firetv_status"),
        policy=_map(firetv_screenshot="read"),           # a picture to the owner; changes nothing on the TV
        say=_map(firetv_control="use the TV"),
        # no screenshot out loud: the picture goes to a chat, and a voice conversation has none
        voice_omit=_fs("firetv_screenshot"), voice_kwargs=_map(screenshot=False),
        voice_note="Out loud you can't see the TV's screen: when you need to know what it shows, ask the owner.",
        code=True, native_sheet=True, cli=("firetv",),
        cli_help="The owner's Fire TV over network ADB: pair, status, on, off, key, volume, mute, open, apps, "
                 "youtube, type, screenshot, forget. See docs/firetv.md"),
    App(slot="obsbot", module="obsbot_panel", make="make_panel", switch="EVE_OBSBOT", default_on=True,
        title="Camera", symbol="web.camera", detail="OBSBOT webcam", doc="docs/obsbot.md",
        no_control="The camera is off on this Mac (no OBSBOT camera is plugged in, or EVE_OBSBOT=0).",
        label="camera_aim", tool_prefix="camera_",
        acting=_fs("camera_control"), clean=_fs("camera_status"),
        say=_map(camera_control="move the camera"),
        code=True, stream=True, native_sheet=True, cli=("obsbot", "cam"), cli_module="obsbot",
        cli_help="The OBSBOT Tiny 3 webcam through OBSBOT Center's OSC: setup, status, wake, sleep, move, nudge, "
                 "center, zoom, fov, preset, track, mode. See docs/obsbot.md"),
    App(slot="pika", module="pika", make="make_pika", switch="EVE_PIKA", default_on=False,
        title="Pika", symbol="car", detail="Drive the car", doc="docs/pika.md",
        no_control="Pika isn't set up on this Mac (EVE_PIKA and pika.json).",
        tools=False, stream=True, native_sheet=True),
    # eve-bridge app new: new apps go above this line
)

# The code path's order (TelegramInlet._handle): the TV first, so a light named "TV strip" cannot claim "turn on the
# tv", then the camera (every pattern names it), the lights, the music.
CODE_ORDER: tuple[str, ...] = ("firetv", "obsbot", "lights", "spotify")
# The phone's Apps list, after Notes (which is eve's own, never a registry app).
PHONE_ORDER: tuple[str, ...] = ("spotify", "lights", "firetv", "pika", "obsbot")
# Names a new app may not take: eve's own panels and the CLI word.
RESERVED: frozenset[str] = frozenset({"memory", "notes", "app"})
MARKER = "# eve-bridge app new: new apps go above this line"

_ON = ("1", "true", "yes", "on")
_OFF = ("0", "false", "no", "off")


def get(slot: str) -> Optional[App]:
    for app in APPS:
        if app.slot == slot:
            return app
    return None


def with_tools() -> tuple[App, ...]:
    """The apps that give the models tools, in tool order."""
    return tuple(a for a in APPS if a.tools)


def ordered(order: Iterable[str]) -> tuple[App, ...]:
    """The listed slots first, then every other app in APPS order: a new app lands last in every order."""
    named = [a for a in (get(s) for s in order) if a is not None]
    return tuple(named) + tuple(a for a in APPS if a not in named)


def by_code() -> tuple[App, ...]:
    return tuple(a for a in ordered(CODE_ORDER) if a.code)


def for_phone() -> tuple[App, ...]:
    return tuple(a for a in ordered(PHONE_ORDER) if a.controls)


def switched_on(app: App, environ: Optional[Mapping[str, str]] = None) -> bool:
    """The switch alone (a module may also need a login, a file or a device)."""
    import os

    env = os.environ if environ is None else environ
    raw = str(env.get(app.switch, "")).strip().lower()
    return raw not in _OFF if app.default_on else raw in _ON


def owner(name: str, *, voice: bool = False) -> Optional[App]:
    """The tools app whose TOOL_NAMES holds ``name``; out loud (``voice``) its voice_omit names are no one's."""
    for app in with_tools():
        if name in app.mod().TOOL_NAMES and not (voice and name in app.voice_omit):
            return app
    return None


def _union(field_name: str) -> frozenset[str]:
    out: set[str] = set()
    for app in APPS:
        out |= set(getattr(app, field_name))
    return frozenset(out)


def ACTING() -> frozenset[str]:  # noqa: N802 — named after the tool_gate set it feeds
    return _union("acting")


def CLEAN() -> frozenset[str]:  # noqa: N802
    return _union("clean")


def POLICY() -> dict[str, str]:  # noqa: N802
    return {k: v for app in APPS for k, v in app.policies().items()}


def SOURCES() -> dict[str, str]:  # noqa: N802
    return {k: v for app in APPS for k, v in app.sources.items()}


def SAY() -> dict[str, str]:  # noqa: N802
    return {k: v for app in APPS for k, v in app.say.items()}


def OWN_WORDS() -> dict[str, frozenset[str]]:  # noqa: N802
    return {k: frozenset(v) for app in APPS for k, v in app.own_words.items()}


def labels() -> tuple[str, ...]:
    return tuple(a.label for a in APPS if a.label)


def prefixes() -> tuple[tuple[str, str], ...]:
    return tuple((a.tool_prefix, a.label) for a in APPS if a.tool_prefix and a.label)


def check_hubs(hubs: Mapping[str, Any]) -> None:
    """A site's ``**app_hubs``: every key must be a tools app's slot."""
    known = {a.slot for a in with_tools()}
    bad = sorted(set(hubs) - known)
    if bad:
        raise TypeError(f"unexpected app hub(s): {', '.join(bad)} (not a tools app in app_registry.APPS)")


def check(app: App) -> list[str]:
    """The contract an app must meet; [] when it does. Imports the module (the test runs it for every app)."""
    problems: list[str] = []
    if not app.slot.isidentifier() or not app.slot.islower() or app.slot in RESERVED:
        problems.append(f"slot {app.slot!r} is not a lowercase name outside {sorted(RESERVED)}")
    if not app.switch.startswith("EVE_"):
        problems.append(f"switch {app.switch!r} does not start with EVE_")
    mod = app.mod()
    if not callable(getattr(mod, app.make, None)):
        problems.append(f"{app.module}.{app.make} is missing")
    if app.tools:
        names = getattr(mod, "TOOL_NAMES", None)
        if not names:
            problems.append(f"{app.module}.TOOL_NAMES is missing")
            names = ()
        if not isinstance(getattr(mod, "OFF_REASON", None), str):
            problems.append(f"{app.module}.OFF_REASON is missing")
        tools = getattr(mod, "TOOLS", None)
        if not isinstance(tools, list) or tuple(t.get("name") for t in tools if isinstance(t, dict)) != tuple(names):
            problems.append(f"{app.module}.TOOLS is missing, or its names are not TOOL_NAMES in order")
        if not isinstance(getattr(mod, "INSTRUCTIONS", getattr(mod, "INSTRUCTIONS_HEAD", None)), str):
            problems.append(f"{app.module}.INSTRUCTIONS is missing (the intent eval lends it)")
        for name in sorted(set(app.acting) | set(app.clean) | set(app.voice_omit) | set(app.sources)
                           | set(app.say) | set(app.own_words) | set(app.policy)):
            if name not in names:
                problems.append(f"{name} is not one of {app.module}.TOOL_NAMES")
        declared = app.policies()
        problems += [f"{n} has no approval policy (put it in acting, clean, sources or policy)" for n in names
                     if n not in declared]
        problems += [f"{n}'s policy {p!r} is not one of {', '.join(POLICIES)}" for n, p in sorted(declared.items())
                     if p not in POLICIES]
        problems += [f"{n} is in acting, so its policy cannot be read" for n in sorted(app.acting)
                     if declared.get(n) == "read"]
        if not app.tool_prefix:
            problems.append("a tools app needs a tool_prefix")
        problems += [f"{n} does not start with {app.tool_prefix}" for n in names
                     if app.tool_prefix and not n.startswith(app.tool_prefix)]
        if app.code_taint_tool and app.code_taint_tool not in names:
            problems.append(f"code_taint_tool {app.code_taint_tool} is not one of its tools")
    hub_cls = _hub_class(mod, app.make)
    if app.code:
        if not callable(getattr(mod, "match", None)) and not (hub_cls and callable(getattr(hub_cls, "match", None))):
            problems.append("a code app needs a match(text)")
        if not (hub_cls and callable(getattr(hub_cls, "run", None))):
            problems.append("a code app's hub needs run(cmd)")
    if app.controls and not (hub_cls and callable(getattr(hub_cls, "panel", None))):
        problems.append("a controls app's hub needs panel(body)")
    if app.stream and not (hub_cls and callable(getattr(hub_cls, "stream", None))):
        problems.append("a stream app's hub needs stream(writer)")
    if app.cli and not callable(getattr(app.cli_mod(), "cli", None)):
        problems.append(f"{app.cli_module or app.module}.cli is missing")
    return problems


def _hub_class(mod: ModuleType, make: str = "") -> Any:
    """The class an app's factory returns, from its ``Optional[X]`` annotation, else the module's one class with
    ``panel`` and ``close``: enough for check() to see the hub's methods without making one (no device is touched)."""
    import typing

    factory = getattr(mod, make, None) if make else None
    if factory is not None:
        try:
            ret = typing.get_type_hints(factory).get("return")
        except Exception:  # noqa: BLE001 — an annotation that does not resolve: fall back to the scan
            ret = None
        for cand in (ret, *typing.get_args(ret)):
            if isinstance(cand, type) and cand is not type(None):
                return cand
    found = [obj for obj in vars(mod).values()
             if isinstance(obj, type) and obj.__module__ == mod.__name__
             and callable(getattr(obj, "panel", None)) and callable(getattr(obj, "close", None))]
    return found[0] if len(found) == 1 else None


# ---- the generic phone sheet (docs/adding-an-app.md, "The phone") ---------------------------------------------------

UI_VERSION = 1
MAX_CONTROLS = 24
CONTROL_TYPES = ("button", "toggle", "slider")


def button(id: str, label: str, action: str, *, symbol: str = "", destructive: bool = False) -> dict[str, Any]:  # noqa: A002
    out: dict[str, Any] = {"type": "button", "id": id, "label": label, "action": action}
    if symbol:
        out["symbol"] = symbol
    if destructive:
        out["role"] = "destructive"
    return out


def toggle(id: str, label: str, on: bool, action: str) -> dict[str, Any]:  # noqa: A002
    return {"type": "toggle", "id": id, "label": label, "on": bool(on), "action": action}


def slider(id: str, label: str, value: float, lo: float, hi: float, action: str, *,  # noqa: A002
           step: Optional[float] = None) -> dict[str, Any]:
    out: dict[str, Any] = {"type": "slider", "id": id, "label": label, "value": value, "min": lo, "max": hi,
                           "action": action}
    if step is not None:
        out["step"] = step
    return out


def panel(*, status: str = "", controls: Optional[list[dict[str, Any]]] = None, line: str = "",
          ok: bool = True) -> dict[str, Any]:
    """A generic sheet's answer: ``status`` is the headline, ``line`` the muted said-line, ``controls`` redraw the
    sheet (None keeps the last ones on the phone)."""
    out: dict[str, Any] = {"ok": ok, "ui": UI_VERSION}
    if line:
        out["line"] = line
    if status:
        out["status"] = status
    if controls is not None:
        out["controls"] = list(controls)
    return out


def check_panel(obj: Any) -> list[str]:
    """What the phone's generic sheet would refuse or drop in a panel answer; [] when it is all shown."""
    if not isinstance(obj, dict):
        return ["the answer is not a JSON object"]
    problems: list[str] = []
    if not isinstance(obj.get("ok"), bool):
        problems.append("ok must be true or false")
    for key in ("line", "status"):
        if key in obj and not isinstance(obj[key], str):
            problems.append(f"{key} must be a string")
    if obj.get("ok") is False:
        return problems
    if obj.get("ui") != UI_VERSION:
        problems.append(f"ui must be {UI_VERSION}")
    controls = obj.get("controls")
    if controls is None:
        return problems
    if not isinstance(controls, list):
        return problems + ["controls must be a list"]
    if len(controls) > MAX_CONTROLS:
        problems.append(f"more than {MAX_CONTROLS} controls")
    ids: set[str] = set()
    for i, c in enumerate(controls):
        where = f"controls[{i}]"
        if not isinstance(c, dict):
            problems.append(f"{where} is not an object")
            continue
        kind = c.get("type")
        if kind not in CONTROL_TYPES:
            problems.append(f"{where}: type {kind!r} is not one of {', '.join(CONTROL_TYPES)}")
            continue
        for key in ("id", "label", "action"):
            if not isinstance(c.get(key), str) or not c.get(key):
                problems.append(f"{where}: {key} must be a non-empty string")
        if c.get("id") in ids:
            problems.append(f"{where}: id {c.get('id')!r} is used twice")
        ids.add(str(c.get("id")))
        if kind == "toggle" and not isinstance(c.get("on"), bool):
            problems.append(f"{where}: a toggle needs on")
        if kind == "slider":
            nums = [c.get(k) for k in ("value", "min", "max")]
            if not all(_number(n) for n in nums):
                problems.append(f"{where}: a slider needs numeric value, min and max")
            elif not nums[1] < nums[2]:
                problems.append(f"{where}: a slider needs min < max")
            if "step" in c and not (_number(c["step"]) and c["step"] > 0):
                problems.append(f"{where}: step must be a positive number")
        if kind == "button" and c.get("role") not in (None, "destructive"):
            problems.append(f"{where}: role is destructive or absent")
    return problems


def _number(v: Any) -> bool:
    return isinstance(v, (int, float)) and not isinstance(v, bool) and math.isfinite(v)


# ---- names a new app may take (app_template.scaffold) ---------------------------------------------------------------

# Where a hub is kept under its slot's name: the chat inlet and the daemon as ``_<slot>``, the voice session and the
# Mini App server as ``<slot>``. A new slot must not be a name these already use for something else.
SITES = ("telegram.py", "voice_agent.py", "miniapp.py", "daemon.py", "link_chat.py")


def site_names() -> frozenset[str]:
    """Every attribute and member name the wiring sites already use (``self.x``, ``server.x``, ``def x``, a class
    field): read from their source, so nothing is imported and no device is touched."""
    from pathlib import Path

    here = Path(__file__).resolve().parent
    out: set[str] = set()
    for name in SITES:
        path = here / name
        if not path.is_file():
            continue
        text = path.read_text(encoding="utf-8")
        out |= set(re.findall(r"\b(?:self|server|cls|inlet|session|daemon)\.([A-Za-z_][A-Za-z0-9_]*)", text))
        out |= set(re.findall(r"^\s*(?:async\s+)?def\s+([A-Za-z_][A-Za-z0-9_]*)", text, re.M))
        out |= set(re.findall(r"^    ([A-Za-z_][A-Za-z0-9_]*)\s*(?::[^=\n]*)?=", text, re.M))
    return frozenset(out)


def name_problem(name: str, own_prefixes: Iterable[str] = (), labels: Iterable[str] = ()) -> str:
    """Why ``name`` cannot be a new app's slot; "" when it can. ``own_prefixes``: eve's own tools' first words;
    ``labels``: names already in use as an intent-eval label or one of eve's own tools (the scaffold reads them from
    bridge/tools/intent_eval.py and the tool gate)."""
    if not re.fullmatch(r"[a-z][a-z0-9]{1,15}", name):
        return "a name is 2 to 16 lowercase letters or digits, starting with a letter"
    if keyword.iskeyword(name) or keyword.issoftkeyword(name):
        return f"{name} is a Python keyword"
    if name in RESERVED or get(name) is not None:
        return f"{name} is taken"
    prefix = name + "_"
    for own in own_prefixes:
        if own.startswith(prefix) or prefix.startswith(own):
            return f"{name}_ would collide with eve's own tools ({own}…)"
    for app in with_tools():
        mine = app.tool_prefix
        if mine and (mine.startswith(prefix) or prefix.startswith(mine)):
            return f"{name}_ would collide with the {app.slot} app's tools ({mine}…)"
        clash = [n for n in app.mod().TOOL_NAMES if n == name or n.startswith(prefix)]
        if clash:
            return f"{name}_ would collide with the {app.slot} app's tool {clash[0]}"
    if name in set(labels) | set(a.label for a in APPS):
        return f"{name} is already an intent-eval label or one of eve's tools"
    used = site_names()
    for attr in (name, "_" + name):
        if attr in used:
            return f"{name} would clash with {attr}, a name eve's wiring already uses"
    return ""
