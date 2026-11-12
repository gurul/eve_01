"""The app router (app_router.py): whether a turn's request is for one of the owner's devices.

Two halves. The first is pure: the questions and the state, the answer's parsing, the rule, the fit and the eval
tool, with a fake ``predict``. The second is the chat brain's turn on test_telegram's fakes: the incident ("Change TV
input to HDMI 1." on a call), a claim that leads to the device's tool, and the promise that no claim, an error and a
timeout leave the request byte for byte what it was. Nothing here reaches Jev, a model or a device.
"""

from __future__ import annotations

import asyncio
import json
import logging
import sys
import threading
import time
from pathlib import Path
from typing import Any, Iterable, Optional

import pytest
from test_telegram import OWNER, FakeApi, FakeCreate, Rig, _FakeTV, call, say, settle

from eve_bridge import app_registry, app_router, profile, system_context, telegram
from eve_bridge import app_router as ar

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "tools"))
import app_route_eval as ev  # noqa: E402

INCIDENT = "Change TV input to HDMI 1."
CLOCK_NOTE = "\n\nSystem context:\nCLOCK."
TV_TOOLS = ("firetv_control", "firetv_status", "firetv_screenshot")
GATES = dict.fromkeys(ar.KEYS, ar.Gate(noul=0.7, choice=0.5, lead=0.0, can=0.5))


def answer(which: str, p: float = 0.95, *, noul: Optional[float] = None, mac: float = 0.05, can: float = 0.9,
           how: str = "", offered: Iterable[str] = ar.ALL_TOOLS, ms: float = 180.0, error: str = "") -> ar.AppAnswer:
    """An AppAnswer as ``ask`` builds it: the Choice picks ``which`` at ``p``; that device's Noul is ``noul`` (``p``
    when not given), every other device's 0.02."""
    things = ar.live(offered)
    keys = tuple(t.key for t in things)
    rest = round((1.0 - p) / (len(keys) + 1), 4)
    probs = {k: (p if k == which else rest) for k in (ar.OTHER, ar.MAC, *keys)}
    nouls = {ar.MAC: mac, **{k: ((p if noul is None else noul) if k == which else 0.02) for k in keys}}
    hows = {t.key: ((how if t.key == which and how else t.methods[0].key), 0.9) for t in things}
    cans = {k: (can if k == which else 0.03) for k in keys}
    return ar.AppAnswer(keys, which, probs, nouls, hows, cans, ms, error)


def jev_body(a: ar.AppAnswer, drop: tuple[str, ...] = ()) -> dict[str, Any]:
    """Jev's envelope for ``a``: what ``predict`` returns."""
    answers: dict[str, Any] = {"which": {"choice": a.which, "probabilities": dict(a.probs), "confidence": 0.9},
                               "for_mac": {"noul": a.nouls[ar.MAC]}}
    for k in a.asked:
        method, p = a.how[k]
        answers[f"for_{k}"] = {"noul": a.nouls[k]}
        answers[f"how_{k}"] = {"choice": method, "probabilities": {method: p}}
        answers[f"can_{k}"] = {"noul": a.can[k]}
    return {"answers": {k: v for k, v in answers.items() if k not in drop}}


class FakePredict:
    def __init__(self, reply: Any) -> None:
        self.reply, self.calls = reply, []

    def __call__(self, state: Any, questions: dict[str, Any]) -> dict[str, Any]:
        self.calls.append((state, questions))
        if isinstance(self.reply, Exception):
            raise self.reply
        return self.reply(state) if callable(self.reply) else self.reply


# ---- the devices come from the real tool schemas --------------------------------------------------------------------

def _actions(tools: list[dict[str, Any]], name: str) -> set[str]:
    tool = next(t for t in tools if t["name"] == name)
    return set(tool["parameters"]["properties"]["action"]["enum"])


def test_every_schema_action_of_every_device_is_one_of_its_methods() -> None:
    from eve_bridge import firetv, lights, obsbot_panel, spotify

    covers = {t.key: {m.tool: set() for m in t.methods} for t in ar.THINGS}
    for t in ar.THINGS:
        for m in t.methods:
            assert not covers[t.key][m.tool] & set(m.covers), f"{t.key}: an action is in two methods"
            covers[t.key][m.tool] |= set(m.covers)
    assert covers["tv"]["firetv_control"] == _actions(firetv.TOOLS, "firetv_control") == set(firetv.ACTIONS)
    assert covers["music"]["spotify_control"] == _actions(spotify.TOOLS, "spotify_control")
    assert covers["camera"]["camera_control"] == _actions(obsbot_panel.TOOLS, "camera_control")
    set_light = next(t for t in lights.TOOLS if t["name"] == "lights_set")
    assert covers["lights"]["lights_set"] == set(set_light["parameters"]["properties"]) - {"target"}
    # every tool the app's module offers is some method's, and the one tool that is not the module's is eve's own
    mods = {"tv": firetv, "lights": lights, "music": spotify, "camera": obsbot_panel}
    own = {t["name"] for t in telegram.TOOLS}
    for t in ar.THINGS:
        assert set(mods[t.key].TOOL_NAMES) <= set(t.tools)
        assert set(t.tools) - set(mods[t.key].TOOL_NAMES) <= {"take_photo"} & own
        app = app_registry.get(t.slot)
        assert app is not None and app.tools and app.mod() is mods[t.key]
        assert len({m.key for m in t.methods}) == len(t.methods)
    assert ar.START_TASK in own and ar.START_TASK not in ar.ALL_TOOLS


def test_the_questions_and_the_state_name_no_tool_model_or_app() -> None:
    things = ar.live(ar.ALL_TOOLS)
    read = json.dumps([ar.state("x", "y", things), ar.questions(things)]).lower()
    names = {n for a in app_registry.with_tools() for n in a.mod().TOOL_NAMES} | {t["name"] for t in telegram.TOOLS}
    for name in sorted(names | {"codex", "jev", "gpt", "claude", "fire tv", "firetv", "spotify", "obsbot", "govee",
                                "computer use", "agent"}):
        assert name.lower() not in read, name
    # the state carries one capability line per live device, so every Noul can see it
    lines = ar.state("x", "y", things)["things"].split("\n")
    assert len(lines) == len(ar.THINGS) and all("can do exactly these things and nothing else" in ln for ln in lines)
    for t in things:
        assert t.line in ar.questions(things)[f"can_{t.key}"]["instructions"]


def test_one_request_carries_both_layers_and_the_safe_option_comes_first() -> None:
    q = ar.questions(ar.live(ar.ALL_TOOLS))
    assert list(q["which"]["criteria"]) == [ar.OTHER, ar.MAC, "tv", "lights", "music", "camera"]
    assert {k for k, v in q.items() if v["type"] == "noul"} == {"for_mac", *(f"for_{k}" for k in ar.KEYS),
                                                               *(f"can_{k}" for k in ar.KEYS)}
    assert {k for k, v in q.items() if v["type"] == "choice"} == {"which", *(f"how_{k}" for k in ar.KEYS)}
    assert list(q["how_tv"]["criteria"]) == [m.key for m in ar.thing("tv").methods]
    st = ar.state("  a\n b " + "x" * 600, "p " * 400, ar.live(ar.ALL_TOOLS))
    assert st["request"].startswith("a b x") and len(st["request"]) == 500 and len(st["previous"]) == 500


def test_a_mac_without_a_device_never_asks_about_it() -> None:
    no_tv = ar.ALL_TOOLS - set(TV_TOOLS)
    q = ar.questions(ar.live(no_tv))
    assert "tv" not in q["which"]["criteria"] and not any(k.endswith("_tv") for k in q)
    assert "TV" not in json.dumps([q, ar.state("x", "", ar.live(no_tv))])
    # the Mac's camera alone: the camera is there, with the one thing it can do
    (cam,) = ar.live({"take_photo", "start_task", "web_search"})
    assert cam.key == "camera" and [m.key for m in cam.methods] == ["photo"] and cam.tools == ("take_photo",)
    assert "turn it" not in cam.line and "take a picture" in cam.line
    # a TV with no screenshot (as the voice has it): the method goes with its tool
    tv = ar.live({"firetv_control", "firetv_status"})[0]
    assert "picture" not in [m.key for m in tv.methods] and "screen shows" not in tv.line
    assert ar.fingerprint(ar.live(no_tv)) != ar.fingerprint(ar.live(ar.ALL_TOOLS))
    # nothing live: Jev is not called at all
    predict = FakePredict({})
    assert ar.ask(predict, "hi", "", {"web_search"}).error == "no device" and predict.calls == []


# ---- the answer -----------------------------------------------------------------------------------------------------

def test_ask_reads_every_typed_answer_and_round_trips_through_json() -> None:
    want = answer("tv", 0.93, can=0.12, how="keys", mac=0.04)
    ticks = iter([10.0, 10.25])
    predict = FakePredict(jev_body(want))
    got = ar.ask(predict, INCIDENT, "", ar.ALL_TOOLS, lambda: next(ticks))
    assert got.which == "tv" and got.probs["tv"] == 0.93 and got.nouls["tv"] == 0.93 and got.nouls[ar.MAC] == 0.04
    assert got.how["tv"] == ("keys", 0.9) and got.can["tv"] == 0.12 and got.asked == ar.KEYS
    assert got.ms == pytest.approx(250.0) and got.error == ""
    state, questions = predict.calls[0]
    assert state["request"] == INCIDENT and set(state) == {"about", "things", "previous", "request"}
    assert questions == ar.questions(ar.live(ar.ALL_TOOLS))
    assert ar.AppAnswer.from_json(json.loads(json.dumps(got.to_json()))) == got           # the eval's cache


@pytest.mark.parametrize("drop", ["which", "for_mac", "for_tv", "how_lights", "can_camera"])
def test_a_missing_answer_is_an_error_that_claims_nothing(drop: str) -> None:
    got = ar.ask(FakePredict(jev_body(answer("tv"), drop=(drop,))), INCIDENT, "", ar.ALL_TOOLS)
    assert got.error == f"missing: {drop}" and not ar.decide(got, GATES)


def test_a_failed_or_malformed_call_claims_nothing() -> None:
    boom = ar.ask(FakePredict(RuntimeError("HTTP 503 from the route")), INCIDENT, "", ar.ALL_TOOLS)
    assert boom.error.startswith("RuntimeError") and not ar.decide(boom, GATES) and ar.decide(boom, GATES).asked
    for body in ({}, {"answers": None}, {"answers": []}, {"answers": {"which": "tv"}}):
        assert ar.ask(FakePredict(body), INCIDENT, "", ar.ALL_TOOLS).error
    # a Choice that names something that was not an option, or carries no probability for its pick
    bad = jev_body(answer("tv"))
    bad["answers"]["which"] = {"choice": "fridge", "probabilities": {"fridge": 0.99}}
    assert ar.ask(FakePredict(bad), INCIDENT, "", ar.ALL_TOOLS).error == "missing: which"
    bad["answers"]["which"] = {"choice": "tv", "probabilities": {"mac": 0.2}}
    assert ar.ask(FakePredict(bad), INCIDENT, "", ar.ALL_TOOLS).error == "missing: which"
    bad = jev_body(answer("tv"))
    bad["answers"]["for_tv"] = {"noul": True}                      # a bool is not a probability
    assert ar.ask(FakePredict(bad), INCIDENT, "", ar.ALL_TOOLS).error == "missing: for_tv"


# ---- the rule -------------------------------------------------------------------------------------------------------

def test_a_device_is_claimed_only_when_all_three_edges_clear() -> None:
    got = ar.decide(answer("tv", 0.93, noul=0.81, how="open_app"), GATES)
    assert (got.app, got.can, got.method, got.asked, got.error) == ("tv", True, "open_app", True, "")
    assert got.p_app == pytest.approx(0.81)                         # the weakest edge, never a product
    # the Choice alone is not a gate: it picks the TV, the TV's own Noul says no
    assert not ar.decide(answer("tv", 0.99, noul=0.6), GATES)
    # the Noul alone is not either: the Choice picked the TV with too little
    assert not ar.decide(answer("tv", 0.45, noul=0.95), GATES)
    # nor both, when the Mac's Noul is ahead
    assert not ar.decide(answer("tv", 0.9, noul=0.8, mac=0.85), GATES)
    assert ar.decide(answer("tv", 0.9, noul=0.8, mac=0.8), GATES)
    lead = {**GATES, "tv": ar.Gate(0.7, 0.5, 0.3, 0.5)}
    assert not ar.decide(answer("tv", 0.9, noul=0.8, mac=0.6), lead) and ar.decide(answer("tv", 0.9, noul=0.8, mac=0.5), lead)
    # the two options that claim nothing never claim, however sure
    for which in (ar.MAC, ar.OTHER):
        got = ar.decide(answer(which, 0.99), GATES)
        assert not got and got.leaning == which and got.asked
    # a device with no gate, an error, and an answer slower than the turn waits
    assert not ar.decide(answer("tv"), {})
    assert not ar.decide(answer("tv", error="HTTPError"), GATES)
    slow = ar.decide(answer("tv", ms=900.0), GATES, budget_ms=800.0)
    assert not slow and slow.error == "slow"


def test_layer_two_is_read_for_the_claimed_device_only() -> None:
    cant = ar.decide(answer("tv", can=0.12, how="keys"), GATES)
    assert cant.app == "tv" and cant.can is False and cant.method == ""    # no method hint for what it can't do
    a = answer("lights", how="colour")
    a = ar.AppAnswer(a.asked, a.which, a.probs, a.nouls, {**a.how, "tv": ("power", 0.99)}, {**a.can, "tv": 0.99},
                     a.ms)
    got = ar.decide(a, GATES)
    assert (got.app, got.method, got.can) == ("lights", "colour", True)


def test_what_a_claim_tells_the_turn_names_only_tools_the_turn_offers() -> None:
    offered = {"firetv_control", "firetv_status", "start_task", "web_search"}
    can = ar.AppRoute("tv", True, "open_app", 0.9, asked=True)
    assert ar.note_line(can, offered) == (
        "This request is for the TV. Its tools this turn: firetv_control, firetv_status. It reads as a request to "
        "open an app on it by name (firetv_control). Use them now; when they can't do what was asked, tell the owner "
        "so in one line.")
    cant = ar.AppRoute("tv", False, "", 0.9, asked=True)
    assert ar.note_line(cant, offered) == (
        "This request is for the TV. Its tools this turn: firetv_control, firetv_status. It reads as something the "
        "TV can't do: check it against what those tools take, and when it can't be done with them, tell the owner so "
        "in one line.")
    assert ar.refusal(cant, offered) == (
        "not started: this request is for the TV, which you work with firetv_control, firetv_status. Use one of "
        "those this turn; when they can't do what the owner asked, tell the owner so in one line.")
    # a method whose tool is not offered is no hint; a device with no tool offered is no claim at all
    shot = ar.AppRoute("tv", True, "picture", 0.9, asked=True)
    assert "It reads as" not in ar.note_line(shot, offered) and "firetv_screenshot" not in ar.note_line(shot, offered)
    assert ar.note_line(can, {"start_task"}) == "" and ar.refusal(can, {"start_task"}) == ""
    assert ar.note_line(ar.NO_CLAIM, ar.ALL_TOOLS) == "" and ar.refusal(ar.NO_CLAIM, ar.ALL_TOOLS) == ""
    for route in (can, cant, shot):
        for text in (ar.note_line(route, ar.ALL_TOOLS | {"start_task"}), ar.refusal(route, ar.ALL_TOOLS)):
            assert ar.START_TASK not in text and "Mac" not in text and "agent" not in text


def test_the_log_line_is_names_and_numbers() -> None:
    assert ar.NO_CLAIM.log_line() == "none; jev off"
    assert ar.decide(answer("tv", 0.93, noul=0.83, can=0.1), GATES).log_line() == "tv (cannot, p 0.83); jev 180 ms"
    assert ar.decide(answer("music", how="play"), GATES).log_line() == "music (can, play, p 0.95); jev 180 ms"
    assert ar.decide(answer(ar.MAC, 0.88), GATES).log_line() == "none (leaning mac 0.88); jev 180 ms"
    assert ar.AppRoute(ms=801.0, error="timeout", asked=True).log_line() == "none; jev 801 ms (timeout)"


# ---- the switch, the gates file, the turn's call ---------------------------------------------------------------------

@pytest.mark.parametrize("raw,want", [("", "auto"), ("auto", "auto"), ("ON", "on"), ("1", "on"), ("true", "on"),
                                      ("off", "off"), ("0", "off"), ("no", "off"), ("sometimes", "auto")])
def test_the_switch_parses_as_the_toolset_routers_does(raw: str, want: str) -> None:
    assert ar.mode({"EVE_APP_ROUTER": raw}) == want
    assert ar.mode({}) == "auto"


def test_the_router_is_made_only_when_it_is_on_and_jev_has_a_route(monkeypatch: pytest.MonkeyPatch) -> None:
    key = {"OPENROUTER_API_KEY": "k-test", "EVE_JEV_ROUTE": "openrouter"}
    monkeypatch.setattr(ar, "_JEV", {})
    assert ar.SHIPPED is False                                      # until the blind holdout says otherwise
    assert ar.make_router({**key, "EVE_APP_ROUTER": "off"}) is None
    assert ar.make_router({**key}) is None                          # auto, and not SHIPPED
    assert ar.make_router({"EVE_APP_ROUTER": "on", "EVE_JEV_ROUTE": "openrouter"}) is None      # no key
    on = ar.make_router({**key, "EVE_APP_ROUTER": "on"})
    assert on is not None and ar.make_router({**key, "EVE_APP_ROUTER": "on"}) is on            # once a process
    assert ar.make_router({**key, "OPENROUTER_API_KEY": "k-new", "EVE_APP_ROUTER": "on"}) is not on
    monkeypatch.setattr(ar, "SHIPPED", True)
    assert ar.make_router({**key, "OPENROUTER_API_KEY": "k-new"}) is not None


def test_eve_lite_forces_the_switch_off() -> None:
    assert profile.LITE_FORCED["EVE_APP_ROUTER"] == "off" and "EVE_APP_ROUTER" in profile.LITE_CLASSIFIED
    assert ar.mode(profile.LITE_FORCED) == "off"


def test_the_gates_file_is_beside_the_module_and_packaged(tmp_path: Path) -> None:
    assert ar.GATES_FILE.parent == Path(ar.__file__).parent and ar.GATES_FILE.exists()
    assert '"app_gates.json"' in (Path(ar.__file__).parents[2] / "pyproject.toml").read_text(encoding="utf-8")
    loaded = ar.load_gates()
    assert set(loaded) == set(ar.KEYS) and loaded == ar.GATES                              # read at import
    raw = json.loads(ar.GATES_FILE.read_text(encoding="utf-8"))
    assert set(raw["gates"]) == set(ar.KEYS)
    for g in loaded.values():
        assert 0.0 < g.noul < 1.0 and 0.0 < g.choice < 1.0 and -1.0 <= g.lead < 1.0 and 0.0 < g.can < 1.0
    # a missing or broken file: the starting gates, for every device
    assert ar.load_gates(tmp_path / "none.json") == dict.fromkeys(ar.KEYS, ar.DEFAULT_GATE)
    (tmp_path / "bad.json").write_text('{"gates": {"tv": {"noul": 0.5}}}', encoding="utf-8")
    assert ar.load_gates(tmp_path / "bad.json") == dict.fromkeys(ar.KEYS, ar.DEFAULT_GATE)


class FakeAppRouter:
    """Jev's stand-in for a turn: answers with what it was given, and records what it was asked."""

    def __init__(self, *answers: Any, wait: float = 0.0) -> None:
        self.answers = list(answers)
        self.asked: list[tuple[str, str, frozenset[str]]] = []
        self.wait = wait
        self.release = threading.Event()

    def __call__(self, text: str, previous: str, offered: frozenset[str]) -> ar.AppAnswer:
        self.asked.append((text, previous, offered))
        if self.wait:
            self.release.wait(self.wait)
        reply = self.answers.pop(0) if self.answers else answer(ar.OTHER, offered=offered)
        if isinstance(reply, Exception):
            raise reply
        return reply(offered) if callable(reply) else reply


def test_route_turn_never_lets_jev_hurt_the_turn() -> None:
    assert ar.route_turn(INCIDENT, "", None, ar.ALL_TOOLS, GATES) is ar.NO_CLAIM
    router = FakeAppRouter()
    assert ar.route_turn("hi", "", router, {"web_search", "start_task"}, GATES) is ar.NO_CLAIM and router.asked == []
    got = ar.route_turn(INCIDENT, "before", FakeAppRouter(answer("tv", can=0.1)), ar.ALL_TOOLS | {"x"}, GATES)
    assert (got.app, got.can, got.asked) == ("tv", False, True)
    boom = ar.route_turn(INCIDENT, "", FakeAppRouter(RuntimeError("no")), ar.ALL_TOOLS, GATES)
    assert not boom and boom.error == "RuntimeError" and boom.asked
    odd = ar.route_turn(INCIDENT, "", FakeAppRouter("tv"), ar.ALL_TOOLS, GATES)
    assert not odd and odd.error == "not an answer"
    # Jev claims a device this turn has no tool for (it was never asked about it): no claim
    stray = ar.route_turn(INCIDENT, "", FakeAppRouter(answer("tv")), {"take_photo"}, GATES)
    assert not stray and stray.error == "not an answer"
    slow = FakeAppRouter(answer("tv"), wait=5.0)
    t0 = time.perf_counter()
    late = ar.route_turn(INCIDENT, "", slow, ar.ALL_TOOLS, GATES, timeout_s=0.05)
    slow.release.set()
    assert not late and late.error == "timeout" and late.asked and time.perf_counter() - t0 < 2.0
    # the module's gates when none are given
    assert ar.route_turn(INCIDENT, "", FakeAppRouter(answer("tv", 0.99)), ar.ALL_TOOLS).asked


# ---- the fit --------------------------------------------------------------------------------------------------------

# Twelve made-up cases in the datasets' schema, with the answers a fake Jev gives them.
CASES: list[dict[str, Any]] = [
    {"id": "t1", "door": "call", "previous": "", "text": "switch the telly to the games console input", "app": "tv",
     "can": False, "why": "the TV, and it has no such method"},
    {"id": "t2", "door": "app", "previous": "tv on", "text": "open the cartoons app", "app": "tv", "can": True,
     "method": "open_app", "why": "a follow-up to the TV"},
    {"id": "t3", "door": "telegram", "previous": "", "text": "tv volume to 20", "app": "tv", "can": True,
     "method": "volume", "why": "the TV's volume"},
    {"id": "l1", "door": "call", "previous": "", "text": "make the desk lamp teal", "app": "lights", "can": True,
     "method": "colour", "why": "a colour"},
    {"id": "l2", "door": "app", "previous": "", "text": "pulse the lights to the beat", "app": "lights", "can": False,
     "why": "no moving effect"},
    {"id": "m1", "door": "call", "previous": "", "text": "play something mellow", "app": "music", "can": True,
     "method": "play", "why": "a mood"},
    {"id": "c1", "door": "app", "previous": "", "text": "snap a picture of my desk", "app": "camera", "can": True,
     "method": "photo", "why": "a photo now"},
    {"id": "k1", "door": "app", "previous": "", "text": "open the slides file and export a pdf", "app": "mac",
     "can": None, "why": "the computer's own screen"},
    {"id": "k2", "door": "call", "previous": "", "text": "open the music app on my computer and sign in",
     "app": "mac", "can": None, "why": "an app's window, not the music"},
    {"id": "k3", "door": "telegram", "previous": "", "text": "turn my camera on in the video call", "app": "mac",
     "can": None, "why": "a setting inside an app"},
    {"id": "n1", "door": "app", "previous": "", "text": "what's a good film about lighthouses", "app": "none",
     "can": None, "why": "a question"},
    {"id": "n2", "door": "call", "previous": "", "text": "remind me to buy lamp oil", "app": "none", "can": None,
     "why": "a reminder that only mentions a lamp"},
]
JEV: dict[str, ar.AppAnswer] = {
    "t1": answer("tv", 0.97, noul=0.95, can=0.14, how="keys"),
    "t2": answer("tv", 0.99, noul=0.96, can=0.92, how="open_app"),
    "t3": answer("tv", 0.90, noul=0.88, can=0.95, how="volume"),
    "l1": answer("lights", 0.99, noul=0.96, can=0.97, how="colour"),
    "l2": answer("lights", 0.85, noul=0.80, can=0.20, how="power"),
    "m1": answer("music", 0.99, noul=0.97, can=0.90, how="play"),
    "c1": answer("camera", 0.98, noul=0.96, can=0.97, how="photo"),
    "k1": answer(ar.MAC, 0.90, mac=0.85),
    "k2": answer("music", 0.55, noul=0.60, mac=0.80, can=0.3),      # the Choice leans to the music; the Mac leads
    "k3": answer("camera", 0.70, noul=0.50, mac=0.55, can=0.25),
    "n1": answer("tv", 0.40, noul=0.30, mac=0.10, can=0.1),         # a film: the Choice has to pick something
    "n2": answer(ar.OTHER, 0.95),
}


def _fit() -> tuple[dict[str, ar.Gate], dict[str, dict[str, Any]]]:
    return ar.fit([JEV[c["id"]] for c in CASES], [c["app"] for c in CASES], [c["can"] for c in CASES])


def test_the_fit_claims_no_case_of_another_truth_and_keeps_its_own() -> None:
    gates, seen = _fit()
    routes = {c["id"]: ar.decide(JEV[c["id"]], gates) for c in CASES}
    for c in CASES:
        want = c["app"] if c["app"] in ar.KEYS else ""
        assert routes[c["id"]].app == want, c["id"]
    assert all(info["wrong"] == 0 and info["claimed"] == info["own"] for info in seen.values())
    # halfway between the strictest gates and the loosest each could go alone, on the strict side: the TV's Noul gate
    # sits between the film (0.30) and its own weakest case (0.88), nearer neither
    tv = gates["tv"]
    assert seen["tv"]["strictest"][0] == 0.85 and seen["tv"]["loosest"][0] <= 0.35
    assert 0.35 < tv.noul < 0.85 and tv.noul in ar.GRID and tv.choice in ar.GRID and tv.lead in ar.LEAD_GRID
    # the music's lead gate keeps "open the music app on my computer" (the Mac ahead by 0.2) out
    assert not ar.claims(JEV["k2"], "music", gates["music"]) and ar.claims(JEV["m1"], "music", gates["music"])


def test_the_fit_prefers_no_wrong_claim_to_recall() -> None:
    # a Mac case that reads exactly like the TV's own weakest case: the TV case is given up, the Mac case never claimed
    answers = [answer("tv", 0.9, noul=0.9), answer("tv", 0.8, noul=0.8, mac=0.1), answer("tv", 0.8, noul=0.8, mac=0.1)]
    gate, info = ar.fit_claim(answers, ["tv", "tv", ar.MAC], "tv")
    assert (info["claimed"], info["own"], info["wrong"]) == (1, 2, 0)
    assert ar.claims(answers[0], "tv", gate) and not ar.claims(answers[2], "tv", gate)
    # no gates separate them at all: the device is never claimed, and the fit says so
    same = [answer("tv", 0.9), answer("tv", 0.9)]
    gate, info = ar.fit_claim(same, ["tv", "none"], "tv")
    assert info["claimed"] == 0 and "never claimed" in info["note"] and not ar.claims(same[0], "tv", gate)
    # an error or a slow answer is never a claim, so it never counts for or against
    gate, info = ar.fit_claim([answer("tv", error="x"), answer("tv", ms=5000.0), answer("tv")], ["tv"] * 3, "tv")
    assert (info["claimed"], info["own"]) == (1, 3)


def test_the_can_gate_never_calls_a_doable_tuning_case_undoable() -> None:
    gates, _ = _fit()
    assert gates["tv"].can == 0.5                  # halfway between 0.14 (can't) and 0.92 (can), on the low side
    assert gates["lights"].can == 0.55
    assert gates["music"].can == 0.5 and gates["camera"].can == 0.5     # only doable cases: the starting gate, at most
    # overlap: a doable case reads lower than one that can't be done; the doable one still reads as doable
    a = [answer("tv", can=0.3), answer("tv", can=0.6)]
    assert ar.fit_can(a, ["tv", "tv"], [True, False], "tv") == 0.3
    assert ar.fit_can(a, ["tv", "tv"], [None, None], "tv") == ar.DEFAULT_GATE.can
    assert ar.fit_can([answer("tv", can=0.2)], ["tv"], [False], "tv") == 0.6


# ---- the eval tool --------------------------------------------------------------------------------------------------

def _dataset(folder: Path, name: str, cases: list[dict[str, Any]]) -> None:
    folder.mkdir(parents=True, exist_ok=True)
    (folder / name).write_text(json.dumps({"about": "made up for the tests", "cases": cases}), encoding="utf-8")


def _predict_by_text(cases: list[dict[str, Any]], slow: float = 0.0) -> FakePredict:
    by_text = {" ".join(c["text"].split()): jev_body(JEV[c["id"]]) for c in cases}
    return FakePredict(lambda state: by_text[state["request"]])


def test_the_eval_fits_caches_by_question_and_scores_the_holdout_once(tmp_path: Path, monkeypatch: pytest.MonkeyPatch,
                                                                      capsys: pytest.CaptureFixture[str]) -> None:
    data, gates_file = tmp_path / "routes", tmp_path / "app_gates.json"
    _dataset(data, ev.TUNING, CASES)
    _dataset(data, ev.HOLDOUT, CASES)
    # before anything is fitted or scored: the report fails closed, and the holdout refuses to run on starting gates
    assert ev.report_main(data, gates_file) is False and ev.FAILED in capsys.readouterr().out
    gates_file.write_text(ar.GATES_FILE.read_text(encoding="utf-8"), encoding="utf-8")
    predict = _predict_by_text(CASES)
    if json.loads(gates_file.read_text(encoding="utf-8"))["fitted"] is None:
        with pytest.raises(SystemExit, match="fit the gates first"):
            ev.holdout_main(predict, data, gates_file)
    assert predict.calls == []
    # --fit: Jev once per case, the gates written with the questions they were fitted on
    gates = ev.fit_main(predict, data, gates_file)
    assert len(predict.calls) == len(CASES) and "APP_ROUTER_FIT_COMPLETE" in capsys.readouterr().out
    assert ar.load_gates(gates_file) == gates == _fit()[0]
    assert json.loads(gates_file.read_text(encoding="utf-8"))["questions"] == ar.fingerprint(ar.live(ar.ALL_TOOLS))
    ev.fit_main(predict, data, gates_file)                           # again: from the cache
    assert len(predict.calls) == len(CASES)
    # an edited case is asked again; the others are not
    edited = [dict(c) for c in CASES]
    edited[0]["previous"] = "tv on"
    _dataset(data, ev.TUNING, edited)
    ev.fit_main(predict, data, gates_file)
    assert len(predict.calls) == len(CASES) + 1
    # a reworded question: every cached answer is stale, and none is read
    monkeypatch.setattr(ar, "WHICH", ar.WHICH + " ")
    ev.fit_main(predict, data, gates_file)
    assert len(predict.calls) == 2 * len(CASES) + 1
    # the holdout refuses gates fitted on other questions
    monkeypatch.undo()
    with pytest.raises(SystemExit, match="questions changed"):
        ev.holdout_main(predict, data, gates_file)
    ev.fit_main(predict, data, gates_file)
    capsys.readouterr()
    # --holdout: once
    asked = len(predict.calls)
    assert ev.holdout_main(predict, data, gates_file) is True
    out = capsys.readouterr().out
    assert len(predict.calls) == asked + len(CASES) and out.rstrip().endswith(ev.OK)
    assert "(1) device cases named right 7/7 = 100.0%" in out and "(2) Mac cases claimed for a device 0/3" in out
    assert "(3) neither cases claimed for a device 0/2" in out and "(4) Jev p95 0 ms" in out
    assert "can / can't, on the device cases named right: 7/7; of the ones that can't: 2/2" in out
    assert "method, on the named cases that carry one: 5/5" in out and "confusion" in out
    with pytest.raises(SystemExit, match="already scored"):
        ev.holdout_main(predict, data, gates_file)
    assert len(predict.calls) == asked + len(CASES)
    # --report: no Jev, the same verdict
    assert ev.report_main(data, gates_file) is True and len(predict.calls) == asked + len(CASES)
    assert capsys.readouterr().out.rstrip().endswith(ev.OK)
    # a refit after the holdout fails the report, whatever the numbers
    refit = json.loads(gates_file.read_text(encoding="utf-8"))
    refit["gates"]["tv"]["noul"] = 0.05
    gates_file.write_text(json.dumps(refit), encoding="utf-8")
    assert ev.report_main(data, gates_file) is False
    out = capsys.readouterr().out
    assert "a refit after the holdout" in out and out.rstrip().endswith(ev.FAILED)


def test_the_eval_bar_fails_on_a_claimed_mac_case_a_miss_or_a_slow_jev(tmp_path: Path) -> None:
    gates = _fit()[0]
    good = ev.score(CASES, [JEV[c["id"]] for c in CASES], gates)
    assert ev.bar(good) == {"1": True, "2": True, "3": True, "4": True, "shipped": True}
    # a Mac case claimed: line 2
    wrong = {**JEV, "k1": answer("tv", 0.99, noul=0.97)}
    s = ev.score(CASES, [wrong[c["id"]] for c in CASES], gates)
    assert s["mac_claimed"] == ["k1"] and ev.bar(s)["2"] is False and ev.bar(s)["shipped"] is False
    # a device case taken for another device is not "named right": line 1
    crossed = {**JEV, "t1": answer("lights", 0.99, noul=0.97), "t2": answer(ar.OTHER, 0.9)}
    s = ev.score(CASES, [crossed[c["id"]] for c in CASES], gates)
    assert s["device_right"] == 5 and s["missed"] == ["t1", "t2"] and ev.bar(s)["1"] is False
    assert s["confusion"]["tv"] == {"lights": 1, "no claim": 1, "tv": 1}
    # a slow Jev claims nothing and fails line 4
    slow = [ar.AppAnswer.from_json({**JEV[c["id"]].to_json(), "ms": 950.0}) for c in CASES]
    s = ev.score(CASES, slow, gates)
    assert s["device_right"] == 0 and s["slow"] == len(CASES) and ev.bar(s)["4"] is False
    # no case of a kind: that line does not hold (a bar over nothing proves nothing)
    devices = [c for c in CASES if c["app"] in ar.KEYS]
    s = ev.score(devices, [JEV[c["id"]] for c in devices], gates)
    assert ev.bar(s)["1"] is True and ev.bar(s)["2"] is False and ev.bar(s)["3"] is False
    # a bad label stops the run instead of being scored as a wrong answer
    _dataset(tmp_path, ev.TUNING, [{**CASES[0], "app": "fridge"}])
    with pytest.raises(SystemExit, match="BAD_CASES"):
        ev.cases(tmp_path / ev.TUNING)
    with pytest.raises(SystemExit, match="NO_CASES"):
        ev.cases(tmp_path / "missing.json")


def test_a_holdout_jev_never_answered_is_not_spent(tmp_path: Path) -> None:
    data, gates_file = tmp_path / "routes", tmp_path / "app_gates.json"
    _dataset(data, ev.TUNING, CASES)
    _dataset(data, ev.HOLDOUT, CASES)
    ev.fit_main(_predict_by_text(CASES), data, gates_file)
    with pytest.raises(SystemExit, match="did not answer"):
        ev.holdout_main(FakePredict(RuntimeError("down")), data, gates_file)
    assert not (data / ev.RESULT).exists() and not (data / ev.HOLDOUT_ANSWERS).exists()
    assert ev.holdout_main(_predict_by_text(CASES), data, gates_file) is True


# ---- the turn (telegram._think), on test_telegram's fakes -----------------------------------------------------------

@pytest.fixture
def clock(monkeypatch: pytest.MonkeyPatch) -> None:
    """One clock for every request, so two turns' bodies can be compared byte for byte."""
    monkeypatch.setattr(system_context, "context", lambda: CLOCK_NOTE)
    monkeypatch.delenv("EVE_PROFILE", raising=False)


def tv_answer(**kw: Any) -> Any:
    return lambda offered: answer("tv", offered=offered, **kw)


def turn(text: str, *responses: Any, app_route: Any = None, **lent: Any) -> Rig:
    """One typed turn of a fresh chat, run to its end (the router's thread included)."""
    rig = Rig(FakeApi(), FakeCreate(*responses), app_route=app_route, small_talk_ask=None, **lent)
    asyncio.run(rig.inlet._turn(telegram.Inbound(OWNER, OWNER, text, message_id=9)))
    return rig


def call_turn(text: str, *responses: Any, app_route: Any = None, **lent: Any) -> tuple[Rig, list[str]]:
    """One push-to-talk call turn: the warm-up first (script ``say("ok")`` for it), then the owner's words."""
    rig = Rig(FakeApi(), FakeCreate(*responses), app_route=app_route, **lent)
    rig.inlet._chat_id = OWNER
    read: list[str] = []

    async def go() -> None:
        assert rig.inlet.listen(read.append)
        await settle()
        rig.inlet.hear(text)
        deadline = time.monotonic() + 5.0
        while rig.create.responses and time.monotonic() < deadline:     # the router answers on a thread
            await asyncio.sleep(0.01)
        for _ in range(20):
            await settle()
            await asyncio.sleep(0.01)

    asyncio.run(go())
    return rig, read


def note_of(body: dict[str, Any]) -> str:
    return "\n".join(c["text"] for i in body["input"] if i.get("role") == "developer" for c in i["content"])


def outputs(body: dict[str, Any]) -> list[dict[str, Any]]:
    return [json.loads(i["output"]) for i in body["input"] if i.get("type") == "function_call_output"]


def test_the_incident_on_a_call_the_tv_is_claimed_and_the_mac_agent_is_refused(
        clock: None, caplog: pytest.LogCaptureFixture) -> None:
    tv, router = _FakeTV(), FakeAppRouter(tv_answer(can=0.14, how="keys"))
    assert tv.match(INCIDENT) is None                               # no code path takes it: it reaches the brain
    caplog.set_level(logging.INFO)
    rig, read = call_turn(INCIDENT, say("ok"), call("start_task", {"goal": "Change TV input to HDMI 1"}),
                          say("The TV can't switch HDMI inputs."), app_route=router, firetv=tv)
    warm, first, second = rig.create.requests
    # Jev was asked once, with the owner's words and the tools that are a device's this turn
    assert router.asked == [(INCIDENT, "", frozenset({*TV_TOOLS, "take_photo"}))]
    # the guard: the Mac agent never started, nothing reached the TV, and the model got another round
    assert rig.agents == [] and tv.handled == [] and tv.ran == []
    claim = ar.AppRoute("tv", False, "", 0.95, 180.0, asked=True)
    assert outputs(second) == [{"ok": False, "reason": ar.refusal(claim, TV_TOOLS)}]
    assert (rig.inlet._app_claim.app, rig.inlet._app_claim.can, rig.inlet._app_refused) == ("tv", False, 1)
    # the owner heard one line, with no "On it." before it
    assert read == ["The TV can't switch HDMI inputs."] and telegram.CALL_ON_IT not in read
    # the note: the call's, then the claim's line, in every round; the cached prefix is the warm-up's
    line = ar.note_line(claim, TV_TOOLS)
    assert "It reads as something the TV can't do" in line and "firetv_control" in line
    for body in (first, second):
        assert note_of(body).endswith(telegram.CALL_NOTE + "\n\n" + line)
        assert body["tools"] == warm["tools"] and body["instructions"] == warm["instructions"]
    assert line not in note_of(warm)
    # the log: names and numbers, never what was said
    logged = "\n".join(r.getMessage() for r in caplog.records)
    assert "app router: tv (cannot, p 0.95); jev 180 ms" in logged
    assert "start_task refused: the request is for a device (tv)" in logged
    assert "HDMI" not in logged and "Change" not in logged


def test_a_claim_the_tv_can_do_leads_to_its_own_tool(clock: None) -> None:
    tv = _FakeTV()
    args = {"action": "open_app", "app": "Prime Video", "query": None, "text": None, "volume": None,
            "volume_change": None, "repeat": None}
    rig = turn("open prime", call("firetv_control", args), say("Opened Prime Video."),
               app_route=FakeAppRouter(tv_answer(how="open_app")), firetv=tv)
    first, _second = rig.create.requests
    assert tv.handled == [("firetv_control", args)] and rig.agents == [] and rig.inlet._app_refused == 0
    assert note_of(first).endswith(
        "This request is for the TV. Its tools this turn: firetv_control, firetv_status, firetv_screenshot. It "
        "reads as a request to open an app on it by name (firetv_control). Use them now; when they can't do what "
        "was asked, tell the owner so in one line.")
    assert rig.api.sent[-1] == (OWNER, "Opened Prime Video.")


def test_the_guard_holds_for_every_round_of_the_turn_and_only_for_start_task(clock: None) -> None:
    tv = _FakeTV()
    rig = turn("put the tv on the other input", call("start_task", {"goal": "switch the input"}),
               call("start_task", {"goal": "switch the input in settings"}, "call_2"),
               call("firetv_status", {}, "call_3"), say("It can't switch inputs."),
               app_route=FakeAppRouter(tv_answer(can=0.1)), firetv=tv)
    assert rig.agents == [] and rig.inlet._app_refused == 2 and tv.handled == [("firetv_status", {})]
    assert rig.api.sent[-1] == (OWNER, "It can't switch inputs.")
    last = outputs(rig.create.requests[-1])
    assert [o.get("ok") for o in last] == [False, False, True]


def _bodies(rig: Rig) -> str:
    return json.dumps(rig.create.requests, sort_keys=True, ensure_ascii=False)


def test_no_claim_an_error_and_a_timeout_leave_the_request_bytes_as_they_were(
        clock: None, monkeypatch: pytest.MonkeyPatch) -> None:
    def run(app_route: Any) -> Rig:
        return turn("open the calculator", call("start_task", {"goal": "open the calculator"}), app_route=app_route,
                    firetv=_FakeTV())

    off = run(None)
    assert len(off.create.requests) == 1 and off.agents[0].goal == "open the calculator"
    want = _bodies(off)
    monkeypatch.setattr(app_router, "TIMEOUT_S", 0.05)
    slow = FakeAppRouter(tv_answer(), wait=5.0)
    routers = {
        "mac": FakeAppRouter(lambda offered: answer(ar.MAC, 0.9, mac=0.85, offered=offered)),
        "other": FakeAppRouter(),
        "below the gates": FakeAppRouter(tv_answer(p=0.99, noul=0.3)),
        "error": FakeAppRouter(lambda offered: answer("tv", offered=offered, error="HTTP 503")),
        "raises": FakeAppRouter(RuntimeError("down")),
        "not an answer": FakeAppRouter({"app": "tv"}),
        "timeout": slow,
    }
    for name, router in routers.items():
        rig = run(router)
        assert _bodies(rig) == want, name
        assert len(router.asked) == 1 and rig.agents[0].goal == "open the calculator", name      # never refused
        assert not rig.inlet._app_claim and rig.inlet._app_refused == 0, name
    slow.release.set()
    # and on a call: the warm-up and the turn, with the call's note
    quiet, _ = call_turn(INCIDENT, say("ok"), say("It can't."), firetv=_FakeTV())
    asked, _ = call_turn(INCIDENT, say("ok"), say("It can't."), app_route=FakeAppRouter(), firetv=_FakeTV())
    assert _bodies(asked) == _bodies(quiet) and note_of(quiet.create.requests[1]).endswith(telegram.CALL_NOTE)


def test_the_switch_off_builds_no_router_and_the_turn_is_the_one_before(
        clock: None, monkeypatch: pytest.MonkeyPatch) -> None:
    want = _bodies(turn("hi there", say("Hello."), firetv=_FakeTV()))
    for raw in ("off", "auto", "on"):       # on, with no Jev key in the tests' environment: still no router
        monkeypatch.setenv("EVE_APP_ROUTER", raw)
        rig = Rig(FakeApi(), FakeCreate(say("Hello.")), small_talk_ask=None, firetv=_FakeTV())
        assert rig.inlet._app_route is None, raw
        asyncio.run(rig.inlet._turn(telegram.Inbound(OWNER, OWNER, "hi there", message_id=9)))
        assert _bodies(rig) == want, raw


def test_a_mac_request_is_never_refused_and_the_mac_camera_is_a_device(clock: None) -> None:
    # no device app on this Mac at all: the camera is still there, through take_photo
    router = FakeAppRouter(lambda offered: answer(ar.MAC, 0.92, mac=0.9, offered=offered))
    rig = turn("open the budget sheet and sort it", call("start_task", {"goal": "open the budget sheet"}),
               app_route=router)
    assert rig.agents[0].goal == "open the budget sheet" and router.asked[0][2] == frozenset({"take_photo"})
    photo = FakeAppRouter(lambda offered: answer("camera", how="photo", offered=offered))
    rig = turn("take a picture from my mac", call("start_task", {"goal": "open the camera app"}),
               say("No camera right now."), app_route=photo)
    assert rig.agents == [] and rig.inlet._app_claim.app == "camera"
    assert "Its tools this turn: take_photo." in note_of(rig.create.requests[0])
    assert outputs(rig.create.requests[1])[0]["reason"].startswith(
        "not started: this request is for the camera, which you work with take_photo.")


def test_a_follow_up_is_asked_with_the_previous_message(clock: None) -> None:
    router = FakeAppRouter()
    rig = Rig(FakeApi(), FakeCreate(say("ok"), say("ok")), app_route=router, small_talk_ask=None, firetv=_FakeTV())

    async def go() -> None:
        await rig.inlet._turn(telegram.Inbound(OWNER, OWNER, "is anything good on tonight", message_id=1))
        await rig.inlet._turn(telegram.Inbound(OWNER, OWNER, "open prime", message_id=2))

    asyncio.run(go())
    assert [(t, p) for t, p, _ in router.asked] == [("is anything good on tonight", ""),
                                                   ("open prime", "is anything good on tonight")]


def test_a_rundown_turn_is_never_routed(clock: None) -> None:
    from eve_bridge import rundown

    words = next(w for w in ("daily rundown", "rundown", "/rundown", "morning rundown") if rundown.matches(w))
    router = FakeAppRouter(tv_answer())
    rig = turn(words, say("Nothing today."), app_route=router, firetv=_FakeTV())
    assert router.asked == [] and rig.create.requests and not rig.inlet._app_claim


def test_eve_lite_never_calls_it(clock: None, monkeypatch: pytest.MonkeyPatch) -> None:
    lite_env = {"EVE_PROFILE": "lite", "EVE_TELEGRAM": "app", "EVE_TOOLSET_ROUTER": "off", "EVE_GRADE_ROUTER": "off",
                "EVE_SEARCH_ROUTER": "off", "EVE_MENU_TOOLS": "off", "EVE_APPS": "0", "EVE_COMPOSIO": "0",
                "EVE_CHIEF": "off", "EVE_ROBOT": "0", "EVE_COMPUTER_CONTROL": "0"}
    for key, value in lite_env.items():
        monkeypatch.setenv(key, value)
    # even switched on by hand, with a Jev key: lite's inlet builds none
    monkeypatch.setenv("EVE_APP_ROUTER", "on")
    monkeypatch.setenv("OPENROUTER_API_KEY", "k-test")
    monkeypatch.setenv("EVE_JEV_ROUTE", "openrouter")
    monkeypatch.setattr(ar, "_JEV", {})
    assert ar.make_router() is not None and telegram._lite()
    lent: dict[str, Any] = dict(agent_factory=None, agent_enabled=False, small_talk_ask=None)
    rig = Rig(FakeApi(), FakeCreate(say("Hello.")), **lent)
    assert rig.inlet._app_route is None
    # and one lent by hand is never asked
    router = FakeAppRouter(tv_answer())
    rig = Rig(FakeApi(), FakeCreate(say("Hello.")), app_route=router, **lent)
    asyncio.run(rig.inlet._turn(telegram.Inbound(OWNER, OWNER, "take a photo", message_id=9)))
    assert router.asked == [] and rig.create.requests and not rig.inlet._app_claim


def test_the_turns_log_line_has_no_message_text(clock: None, caplog: pytest.LogCaptureFixture) -> None:
    secret = "zebra crossing at marmalade o'clock"
    caplog.set_level(logging.DEBUG)
    turn(secret, say("ok"), app_route=FakeAppRouter(tv_answer(how="volume")), firetv=_FakeTV())
    turn(secret, say("ok"), app_route=FakeAppRouter(RuntimeError(secret)), firetv=_FakeTV())
    logged = "\n".join(r.getMessage() for r in caplog.records)
    assert "telegram: app router: tv (can, volume, p 0.95); jev 180 ms" in logged
    assert "telegram: app router: none; jev" in logged and "(RuntimeError)" in logged
    for word in ("zebra", "marmalade"):
        assert word not in logged
