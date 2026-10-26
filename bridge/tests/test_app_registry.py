"""eve's apps, declared once (app_registry.py): the registry is complete, every wiring site reads it, the order the
models see is today's, an app switched off is gone everywhere, and `eve-bridge app new` writes an app that passes its
own tests and registers cleanly (docs/app-registry-gates.md, A1)."""

from __future__ import annotations

import asyncio
import hashlib
import importlib.util
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import pytest
from test_telegram import OWNER, FakeApi, FakeCreate, Rig, say

from eve_bridge import app_registry, link_chat, telegram, tool_gate
from eve_bridge.app_registry import APPS

REPO = Path(__file__).resolve().parents[2]
SRC = REPO / "bridge" / "src" / "eve_bridge"

# ---- frozen copies of today's values (2026-10-12, before the registry) ---------------------------------------------

TODAY_ACTING = frozenset({
    "agentcard_buy", "agentcard_confirm", "answer_question", "camera_control", "capture_note", "change_app",
    "claude_relay", "close_card", "codex_relay", "delete_app", "edit_note", "file_note", "firetv_control",
    "forget_apply", "go_explore", "join_meeting", "leave_meeting", "lights_match", "lights_set", "make_app",
    "meet_join", "meet_leave", "opentable_search", "pair_eve_link", "remember", "rename_app", "restore_app",
    "screenshot", "send_file", "set_sound", "set_stealth", "spotify_control", "start_coding_session", "start_task",
    "steer_task", "stop_task", "take_notes", "take_on", "take_photo", "undo_app", "undo_note", "watch_add",
    "watch_pause", "watch_remove", "watch_resume", "watch_set_end",
    # 2026-10-26: the places tools write the places history (acting for provenance; routine, so they never ask, and
    # in ACTING_READS, so they still taint as before)
    "place_name", "thing_place"})
TODAY_CLEAN = frozenset({"camera_status", "firetv_status", "forget_preview", "lights_status", "move_head", "open_app",
                         "open_apps", "spend_report"})
TODAY_APP_SOURCES = {"spotify_control": "Spotify", "spotify_now_playing": "Spotify"}
TODAY_APP_SAY = {"spotify_control": "change the music", "lights_set": "change the lights",
                 "lights_match": "change the lights", "firetv_control": "use the TV", "camera_control": "move the camera"}
TODAY_OWN_WORDS = {"spotify_control": frozenset({"pause", "volume", "transfer", "mode_on", "mode_off", "mode"})}
TODAY_LABELS = ("lights", "music", "tv_control", "camera_aim")
TODAY_PREFIXES = (("lights_", "lights"), ("spotify_", "music"), ("firetv_", "tv_control"), ("camera_", "camera_aim"))
# The apps' tools in the order the chat model reads them, and out loud (no TV screenshot)
CHAT_TOOLS = ["lights_set", "lights_match", "lights_status", "spotify_now_playing", "spotify_control",
              "firetv_status", "firetv_control", "firetv_screenshot", "camera_status", "camera_control"]
VOICE_TOOLS = [n for n in CHAT_TOOLS if n != "firetv_screenshot"]
TV_NOTE = "Out loud you can't see the TV's screen: when you need to know what it shows, ask the owner."
SWITCHES = {"lights": "EVE_LIGHTS", "spotify": "EVE_SPOTIFY", "firetv": "EVE_FIRETV", "obsbot": "EVE_OBSBOT",
            "pika": "EVE_PIKA"}
# The owner's apps come first in APPS, in this order. An app added after them (`eve-bridge app new`, here or in
# someone's own checkout) comes after: every test below holds today's values for the owner's apps and lets the rest
# through, so a new app passes the whole suite with no edit here.
OWNERS = ("lights", "spotify", "firetv", "obsbot", "pika")
NEWER = tuple(a for a in APPS if a.slot not in OWNERS)
# A plain command each owner app's code path takes ("lights off"); a newer app's code path is its own test's.
CODE_WORDS = {"lights": "lights off", "spotify": "pause the music", "firetv": "turn on the tv", "obsbot": "camera left"}


def _newer(field_name: str) -> set[str]:
    return {n for a in NEWER for n in getattr(a, field_name)}


# ---- the registry itself --------------------------------------------------------------------------------------------

def test_every_app_meets_the_contract_once() -> None:
    assert [a.slot for a in APPS][:len(OWNERS)] == list(OWNERS)                  # tool order is model bytes
    assert len({a.slot for a in APPS}) == len(APPS)
    for app in APPS:
        assert app_registry.check(app) == [], app.slot
        assert app.switch == SWITCHES.get(app.slot, f"EVE_{app.slot.upper()}")
    for slot in (*app_registry.CODE_ORDER, *app_registry.PHONE_ORDER):
        assert app_registry.get(slot) is not None, slot
    assert [a.slot for a in app_registry.by_code()][:4] == ["firetv", "obsbot", "lights", "spotify"]
    assert [a.slot for a in app_registry.for_phone()][:5] == ["spotify", "lights", "firetv", "pika", "obsbot"]
    assert not app_registry.get("pika").default_on and not app_registry.get("pika").tools


def test_ordered_puts_unnamed_apps_last() -> None:
    assert [a.slot for a in app_registry.ordered(("pika",))] == ["pika", "lights", "spotify", "firetv", "obsbot",
                                                                  *(a.slot for a in NEWER)]


def test_switched_on_reads_the_switch_both_ways() -> None:
    lights, pika = app_registry.get("lights"), app_registry.get("pika")
    assert app_registry.switched_on(lights, {}) and not app_registry.switched_on(lights, {"EVE_LIGHTS": "off"})
    assert not app_registry.switched_on(pika, {}) and app_registry.switched_on(pika, {"EVE_PIKA": "yes"})


def test_a_hub_for_no_app_is_refused() -> None:
    app_registry.check_hubs({"lights": None, "obsbot": None})
    with pytest.raises(TypeError):
        app_registry.check_hubs({"toaster": object()})
    with pytest.raises(TypeError):
        app_registry.check_hubs({"pika": object()})                  # pika gives the models no tools


def test_owner_finds_the_app_and_out_loud_skips_voice_omit() -> None:
    assert app_registry.owner("lights_set").slot == "lights"
    assert app_registry.owner("firetv_screenshot").slot == "firetv"
    assert app_registry.owner("firetv_screenshot", voice=True) is None
    assert app_registry.owner("firetv_control", voice=True).slot == "firetv"
    assert app_registry.owner("web_search") is None


def test_the_tool_gate_sets_are_todays() -> None:
    assert tool_gate.ACTING == TODAY_ACTING | _newer("acting")
    assert tool_gate.CLEAN == TODAY_CLEAN | _newer("clean")
    assert {k: v for k, v in tool_gate._SOURCES.items() if k in TODAY_APP_SOURCES} == TODAY_APP_SOURCES
    assert {k: v for k, v in tool_gate._SAY_HEADS.items() if k in TODAY_APP_SAY} == TODAY_APP_SAY
    assert tool_gate._OWN_WORDS_ACTIONS == {**TODAY_OWN_WORDS, **{k: frozenset(v) for a in NEWER
                                                                  for k, v in a.own_words.items()}}
    owners_only = [a for a in APPS if a.slot in OWNERS]
    assert {k: v for a in owners_only for k, v in a.sources.items()} == TODAY_APP_SOURCES
    assert {k: v for a in owners_only for k, v in a.say.items()} == TODAY_APP_SAY


# The owner's apps' approval policies (tool_gate.POLICY), 2026-10-26: every tool declared; none asks.
TODAY_APP_POLICY = {"lights_set": "routine", "lights_match": "routine", "lights_status": "read",
                    "spotify_control": "routine", "spotify_now_playing": "read",
                    "firetv_control": "routine", "firetv_status": "read", "firetv_screenshot": "read",
                    "camera_control": "routine", "camera_status": "read"}


def test_every_app_tool_declares_an_approval_policy() -> None:
    owners_only = {k: v for a in APPS if a.slot in OWNERS for k, v in a.policies().items()}
    assert owners_only == TODAY_APP_POLICY
    for app in app_registry.with_tools():
        assert set(app.mod().TOOL_NAMES) <= set(app.policies()), app.slot
    assert {k: tool_gate.POLICY[k] for k in app_registry.POLICY()} == app_registry.POLICY()


def test_check_refuses_an_undeclared_or_wrong_policy() -> None:
    from dataclasses import replace

    firetv = app_registry.get("firetv")
    bare = replace(firetv, policy={})
    assert "firetv_screenshot has no approval policy (put it in acting, clean, sources or policy)" in \
        app_registry.check(bare)
    wrong = replace(firetv, policy={"firetv_screenshot": "maybe"})
    assert any("firetv_screenshot's policy 'maybe'" in p for p in app_registry.check(wrong))
    read_act = replace(firetv, policy={"firetv_screenshot": "read", "firetv_control": "read"})
    assert "firetv_control is in acting, so its policy cannot be read" in app_registry.check(read_act)
    stray = replace(firetv, policy={"firetv_screenshot": "read", "firetv_nothing": "read"})
    assert "firetv_nothing is not one of firetv.TOOL_NAMES" in app_registry.check(stray)
    asking = replace(firetv, policy={"firetv_screenshot": "read", "firetv_control": "asks"})
    assert app_registry.check(asking) == [] and asking.policies()["firetv_control"] == "asks"


def test_the_eval_labels_and_prefixes_are_todays() -> None:
    import intent_eval

    assert app_registry.labels()[:4] == TODAY_LABELS and app_registry.prefixes()[:4] == TODAY_PREFIXES
    i = intent_eval.LABELS.index("meet")
    n = len(app_registry.labels())
    assert intent_eval.LABELS[i:i + n + 2] == ("meet", *app_registry.labels(), "gov_legal_lookup")
    assert len(set(intent_eval.LABELS)) == len(intent_eval.LABELS)                  # no app's label is taken twice
    assert intent_eval._PREFIXES == (("watch_", "price_watch"), ("meet_", "meet"), *app_registry.prefixes())
    assert {"tv_control", "camera_aim", "lights", "music"} <= intent_eval.CODE_ACTING


# ---- what the models see, with every app here -----------------------------------------------------------------------

class Hub:
    """An app's hub as the chat and the voice read it: the module's real tools, a block of its own."""

    def __init__(self, slot: str) -> None:
        self.slot = slot
        self.asked: list[dict[str, Any]] = []

    def tools(self) -> list[dict[str, Any]]:
        return json.loads(json.dumps(app_registry.get(self.slot).mod().TOOLS))

    def match(self, text: str) -> Any:              # the lights match their own names; these have none
        return None

    def instructions(self, **kw: Any) -> str:
        self.asked.append(kw)
        return f"<{self.slot} block{' ' + json.dumps(kw) if kw else ''}>"


def hubs(*off: str) -> dict[str, Any]:
    """The owner's tools apps present (less ``off``); a newer app is absent, as it is until its switch is on."""
    return {a.slot: (None if a.slot in off or a.slot not in OWNERS else Hub(a.slot)) for a in app_registry.with_tools()}


def chat_request(app_hubs: dict[str, Any]) -> dict[str, Any]:
    rig = Rig(FakeApi(), FakeCreate(say("ok")), **app_hubs)
    asyncio.run(rig.inlet._turn(telegram.Inbound(OWNER, OWNER, "hi", message_id=10)))
    return rig.create.requests[0]


def app_names(tools: list[dict[str, Any]]) -> list[str]:
    prefixes = tuple(p for p, _ in app_registry.prefixes())
    return [str(t["name"]) for t in tools if str(t.get("name", "")).startswith(prefixes)]


def voice_config(app_hubs: dict[str, Any]) -> dict[str, Any]:
    from eve_bridge.voice_agent import VoiceConfig, session_config

    return session_config(VoiceConfig(output="audio"), **app_hubs)["delegation"]["responses"]


def test_the_chat_reads_the_apps_in_todays_order() -> None:
    req = chat_request(hubs())
    names = [str(t.get("name")) for t in req["tools"]]
    assert app_names(req["tools"]) == CHAT_TOOLS
    first = names.index(CHAT_TOOLS[0])
    assert names[first:first + len(CHAT_TOOLS)] == CHAT_TOOLS                    # one slot, together
    assert "<lights block>\n\n<spotify block>\n\n<firetv block>\n\n<obsbot block>" in req["instructions"]


def test_the_voice_reads_the_apps_in_todays_order() -> None:
    h = hubs()
    cfg = voice_config(h)
    assert app_names(cfg["tools"]) == VOICE_TOOLS
    assert cfg["instructions"].endswith(
        '\n\n<lights block>\n\n<spotify block>\n\n<firetv block {"screenshot": false}>\n' + TV_NOTE
        + "\n\n<obsbot block>")
    assert h["firetv"].asked == [{"screenshot": False}]
    assert list(voice_config(hubs(*h))["tools"]) == [t for t in cfg["tools"] if t.get("name") not in VOICE_TOOLS]


def test_a_tool_reaches_its_apps_hub_or_its_off_reason() -> None:
    class Doing(Hub):
        async def handle(self, name: str, args: dict[str, Any]) -> dict[str, Any]:
            return {"ok": True, "by": self.slot, "name": name}

    rig = Rig(FakeApi(), FakeCreate(), lights=Doing("lights"))
    assert asyncio.run(rig.inlet._tool("lights_set", {}, OWNER)) == {"ok": True, "by": "lights", "name": "lights_set"}
    from eve_bridge import spotify

    assert asyncio.run(rig.inlet._tool("spotify_control", {}, OWNER)) == {"ok": False, "reason": spotify.OFF_REASON}


# ---- switched off, an app is gone everywhere ------------------------------------------------------------------------

@pytest.mark.parametrize("app", APPS, ids=lambda a: a.slot)
def test_an_app_switched_off_is_gone_everywhere(app: app_registry.App, monkeypatch: pytest.MonkeyPatch) -> None:
    import intent_eval

    from eve_bridge.daemon import Daemon

    monkeypatch.setenv(app.switch, "0")
    # the daemon makes no hub, so no site gets one
    assert getattr(app.mod(), app.make)() is None
    assert Daemon._app_hub(SimpleNamespace(), app) is None
    # the phone: its legacy key is empty and it is not in the Apps list
    present = {a.slot: (Hub(a.slot) if a.slot != app.slot else None) for a in APPS}
    for hub in present.values():
        if hub is not None:
            hub.listing = lambda: [{"name": "desk lamp"}]
    listing = link_chat.controls_listing(lambda n: present.get(n))
    assert not listing.get(app.slot)
    assert app.slot not in [row["id"] for row in listing["apps"]]
    assert len(listing["apps"]) == len(app_registry.for_phone()) - (1 if app.controls else 0)
    # the eval lends every tools app whatever this shell says, so its request bytes never depend on the environment
    assert (app.slot in intent_eval.lend_apps({"lights": "x"})) == app.tools
    if not app.tools:
        return
    # the chat and the voice: none of its tools, no block
    req = chat_request(hubs(app.slot))
    names = app_names(req["tools"])
    assert names and not any(n in app.mod().TOOL_NAMES for n in names)
    assert f"<{app.slot} block" not in req["instructions"]
    cfg = voice_config(hubs(app.slot))
    assert not any(n in app.mod().TOOL_NAMES for n in app_names(cfg["tools"]))
    assert f"<{app.slot} block" not in cfg["instructions"]
    # no code path: a plain command for it reaches the brain
    words = CODE_WORDS.get(app.slot)
    if words is None:
        return
    inlet = SimpleNamespace(**{"_" + a.slot: (Hub(a.slot) if a.slot != app.slot else None) for a in APPS},
                            _chief_word=None, _quick=None)
    assert intent_eval.code_route(words, inlet) != app.label
    assert not app.mod().enabled()                  # so _handle says no "not set up" line and the brain answers


def test_a_tv_command_with_the_tv_off_goes_to_the_brain(monkeypatch: pytest.MonkeyPatch) -> None:
    from test_telegram import run_rig, update

    monkeypatch.setenv("EVE_FIRETV", "0")
    rig = Rig(FakeApi([update("turn on the tv")]), FakeCreate(say("ok")), **hubs("firetv"))
    run_rig(rig)
    assert rig.create.requests                                                   # a model turn, no "not set up" line
    from eve_bridge import firetv

    assert all(firetv.NOT_SET_UP_LINE not in text for _, text in rig.api.sent)


# ---- the phone's contract -------------------------------------------------------------------------------------------

def test_the_panel_contract_example_passes_and_bad_controls_do_not() -> None:
    example = json.loads((REPO / "bridge" / "tests" / "fixtures" / "app_panel_example.json").read_text())
    assert app_registry.check_panel(example) == []
    built = app_registry.panel(status="On · speed 3", line="The fan is on.", controls=[
        app_registry.toggle("power", "Power", True, "power"),
        app_registry.slider("speed", "Speed", 3, 1, 5, "speed", step=1),
        app_registry.button("boost", "Boost", "boost", symbol="bolt"),
        app_registry.button("off", "Turn off", "off", destructive=True)])
    assert built == example
    assert app_registry.check_panel({**example, "controls": [{"type": "slider", "id": "s", "label": "S",
                                                                "value": 1, "min": 5, "max": 5, "action": "s"}]})
    assert app_registry.check_panel({**example, "controls": [{"type": "dial", "id": "d"}]})
    assert app_registry.check_panel({**example, "controls": [example["controls"][0]] * 25})
    assert app_registry.check_panel({"ok": False, "line": "No."}) == []


def test_the_mini_app_keeps_its_three_cards() -> None:
    page = (SRC / "miniapp.py").read_text(encoding="utf-8")
    assert all(f'"/api/{card}"' in page for card in ("lights", "spotify", "firetv"))


def test_a_new_slot_is_a_control_path() -> None:
    assert link_chat.CONTROL_RE.match("/api/link/controls/fan2").group(1) == "fan2"
    assert link_chat.CONTROL_RE.match("/api/link/controls/2fan") is None
    assert set(link_chat.CONTROLS) == {a.slot for a in APPS} | {"memory", "notes"}
    assert link_chat.STREAM_PATHS == {link_chat.PIKA_STREAM_PATH: "pika", link_chat.OBSBOT_STREAM_PATH: "obsbot",
                                      **{f"{link_chat.CONTROLS_PATH}/{a.slot}/stream": a.slot for a in NEWER if a.stream}}


# ---- the docs -------------------------------------------------------------------------------------------------------

def test_agents_md_names_every_switch_and_the_walkthrough_exists() -> None:
    agents = (REPO / "AGENTS.md").read_text(encoding="utf-8")
    for app in APPS:
        assert (REPO / app.doc).is_file(), app.doc
        if app.slot in OWNERS:                     # the shipped apps' table; an app of your own need not be in it
            assert app.switch in agents, app.switch
    assert (REPO / "docs" / "adding-an-app.md").is_file()
    assert "docs/adding-an-app.md" in agents


# ---- the scaffold ---------------------------------------------------------------------------------------------------

def _digest(root: Path) -> str:
    h = hashlib.sha256()
    for p in sorted(root.rglob("*")):
        if p.is_file() and "__pycache__" not in p.parts:
            h.update(str(p.relative_to(root)).encode() + p.read_bytes())
    return h.hexdigest()


@pytest.mark.xdist_group("app_scaffold")
def test_a_scaffolded_app_passes_its_own_tests_and_registers(tmp_path: Path) -> None:
    from eve_bridge import app_template

    shutil.copytree(SRC, tmp_path / "bridge" / "src" / "eve_bridge",
                    ignore=shutil.ignore_patterns("__pycache__", "*.pyc"))
    (tmp_path / "bridge" / "tests").mkdir(parents=True)
    shutil.copy(REPO / "bridge" / "tests" / "conftest.py", tmp_path / "bridge" / "tests" / "conftest.py")
    (tmp_path / "bridge" / "pytest.ini").write_text("[pytest]\n", encoding="utf-8")
    (tmp_path / "docs").mkdir()

    before = _digest(tmp_path)
    would = app_template.scaffold("demo", tmp_path, dry_run=True)
    assert _digest(tmp_path) == before                                           # --dry-run writes nothing
    wrote = app_template.scaffold("demo", tmp_path)
    assert sorted(would) == sorted(wrote) and all(p.is_file() for p in wrote)

    env = {**os.environ, "PYTHONPATH": str(tmp_path / "bridge" / "src")}
    env.pop("EVE_DEMO", None)
    run = subprocess.run([sys.executable, "-m", "pytest", "-q", "-p", "no:cacheprovider", "-p", "no:xdist",
                          "-c", str(tmp_path / "bridge" / "pytest.ini"), "--rootdir", str(tmp_path / "bridge"),
                          str(tmp_path / "bridge" / "tests" / "test_demo.py")],
                         cwd=tmp_path, env=env, capture_output=True, text=True, timeout=180)
    assert run.returncode == 0, run.stdout + run.stderr
    assert "7 passed" in run.stdout.splitlines()[-1]

    if importlib.util.find_spec("ruff") is not None:
        lint = subprocess.run([sys.executable, "-m", "ruff", "check", "--config", str(REPO / "bridge" / "pyproject.toml"),
                               *(str(p) for p in wrote if p.suffix == ".py")],
                              cwd=tmp_path, capture_output=True, text=True, timeout=120)
        assert lint.returncode == 0, lint.stdout + lint.stderr

    probe = subprocess.run([sys.executable, "-c", "from eve_bridge import app_registry as r; "
                            "print(r.APPS[-1].slot, r.labels()[:4], r.check(r.APPS[-1]))"],
                           cwd=tmp_path, env=env, capture_output=True, text=True, timeout=60)
    assert probe.stdout.split("\n")[0] == f"demo {TODAY_LABELS} []", probe.stdout + probe.stderr

    after = _digest(tmp_path)
    with pytest.raises(app_template.Refused):
        app_template.scaffold("demo", tmp_path)                                  # a second time: refused
    for bad in ("lights", "memory", "app", "class", "watch", "Fan", "f", "camera", "music", "quick", "link",
                "screenshot"):
        with pytest.raises(app_template.Refused):
            app_template.scaffold(bad, tmp_path)
    assert _digest(tmp_path) == after                                            # and nothing changed


def test_a_new_name_may_not_take_a_tool_prefix_a_label_or_a_wiring_attribute() -> None:
    from eve_bridge import app_template

    taken = app_template.eval_labels(REPO) | app_template.own_names()
    assert {"screenshot", "stealth", "camera_photo", "meet"} <= taken
    problem = lambda name: app_registry.name_problem(name, app_template.own_prefixes(), taken)  # noqa: E731
    assert "obsbot" in problem("camera")                       # camera_ is the OBSBOT's prefix
    assert "label" in problem("music") and "label" in problem("screenshot") and "label" in problem("stealth")
    for name in ("quick", "today", "config", "gate", "brief", "conn", "link", "calls", "port"):
        assert "wiring" in problem(name), name                 # the inlet, the voice, the server or the daemon has it
    for name in ("fan", "kettle", "blinds", "coin"):
        assert problem(name) == "", name
