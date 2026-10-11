"""The camera for eve's models (obsbot_panel.py): strict tools, the code path's matcher and what it leaves to the
model, and handle/run over a real Obsbot with a fake USB camera (test_obsbot.FakeUsb)."""

from __future__ import annotations

import asyncio
from typing import Any

import pytest
from test_obsbot import FakeUsb

from eve_bridge import obsbot, obsbot_panel
from eve_bridge.obsbot import Obsbot
from eve_bridge.obsbot_panel import Command, ObsbotPanel, match


@pytest.fixture(autouse=True)
def quick(monkeypatch):
    monkeypatch.setattr(obsbot.time, "sleep", lambda s: None)


def panel(usb: FakeUsb) -> ObsbotPanel:
    return ObsbotPanel(make=lambda **k: Obsbot(port=1, app_running=lambda: False, usb=usb))


def control(**kw: Any) -> dict[str, Any]:
    args = dict.fromkeys(("action", "direction", "degrees", "pan", "tilt", "zoom", "zoom_change", "mode", "preset"))
    args.update(kw)
    return args


def test_tools_are_strict_and_complete():
    names = {t["name"] for t in obsbot_panel.TOOLS}
    assert names == set(obsbot_panel.TOOL_NAMES) == {"camera_status", "camera_control"}
    for t in obsbot_panel.TOOLS:
        params = t["parameters"]
        assert t["strict"] is True and params["additionalProperties"] is False
        assert set(params["required"]) == set(params["properties"])
    ctl = next(t for t in obsbot_panel.TOOLS if t["name"] == "camera_control")["parameters"]["properties"]
    assert ctl["action"]["enum"] == list(obsbot_panel.ACTIONS)
    assert set(ctl["mode"]["enum"]) == {*obsbot.AI_MODES, None}


def test_instructions_name_the_limits():
    text = obsbot_panel.INSTRUCTIONS
    assert "take_photo" in text and "OBSBOT Center" in text and "tracking" in text


@pytest.mark.parametrize("text,cmd", [
    ("camera left", Command("turn", direction="left", degrees=obsbot_panel.TURN_DEGREES)),
    ("turn the camera left a bit", Command("turn", direction="left", degrees=obsbot_panel.BIT_DEGREES)),
    ("pan the webcam right 30 degrees", Command("turn", direction="right", degrees=30)),
    ("camera to the left", Command("turn", direction="left", degrees=obsbot_panel.TURN_DEGREES)),
    ("camera up a lot", Command("turn", direction="up", degrees=obsbot_panel.LOT_DEGREES)),
    ("hey eve can you turn the camera right please", Command("turn", direction="right", degrees=20)),
    ("center the camera", Command("center")),
    ("camera straight ahead", Command("center")),
    ("zoom the camera in", Command("zoom", zoom_change=obsbot_panel.ZOOM_STEP)),
    ("camera zoom out a bit", Command("zoom", zoom_change=-(obsbot_panel.ZOOM_STEP // 2))),
    ("zoom in all the way on the camera", Command("zoom", zoom=100)),
    ("camera zoom 50%", Command("zoom", zoom=50)),
    ("wake the camera", Command("wake")),
    ("camera off", Command("sleep")),
    ("put the camera to sleep", Command("sleep")),
    ("where is the camera pointing", Command("status")),
    ("is my webcam awake", Command("status")),
    ("camera track me", Command("track_on")),
    ("camera stop tracking", Command("track_off")),
    ("camera preset 2", Command("preset", preset=2)),
])
def test_match(text, cmd):
    assert match(text) == cmd


@pytest.mark.parametrize("text", [
    "take a photo with the camera",       # take_photo's
    "point the camera at the door",       # needs eyes: the model
    "turn left",                          # names no camera
    "zoom in",
    "camera left in ten minutes",         # words over
    "camera zoom 300%",                   # out of range
    "camera left 500 degrees",
    "turn on the tv",
    "lights off",
    "what's the best camera for vlogging",
])
def test_match_leaves_the_rest_to_the_model(text):
    assert match(text) is None


def test_handle_status_turn_point_zoom_center():
    async def go() -> None:
        usb = FakeUsb()
        p = panel(usb)
        st = await p.handle("camera_status", {})
        assert st["ok"] and st["tilt"] == 0 and st["line"] == "Tiny 3 is awake, pointing straight ahead, zoom 0%."
        res = await p.handle("camera_control", control(action="turn", direction="left", degrees=15))
        assert res == {"ok": True, "pan": 15, "tilt": 0, "line": "Camera pointing 15° left."}
        res = await p.handle("camera_control", control(action="point", pan=-30, tilt=15))
        assert ("point", -30, 15) in usb.calls and res["line"] == "Camera pointing 30° right, 15° up."
        res = await p.handle("camera_control", control(action="turn", direction="down", degrees=None))
        assert res["tilt"] == 15 - obsbot_panel.TURN_DEGREES
        res = await p.handle("camera_control", control(action="zoom", zoom_change=25))
        assert res["zoom"] == 25
        res = await p.handle("camera_control", control(action="zoom", zoom_change=-50))
        assert res["zoom"] == 0                                                       # clamped
        res = await p.handle("camera_control", control(action="center"))
        assert res["line"] == "Camera pointing straight ahead."
        await p.close()

    asyncio.run(go())


def test_handle_refusals():
    async def go() -> None:
        usb = FakeUsb()
        p = panel(usb)
        assert not (await p.handle("camera_control", control(action="turn")))["ok"]          # no direction
        assert not (await p.handle("camera_control", control(action="point")))["ok"]         # no angles
        assert not (await p.handle("camera_control", control(action="zoom")))["ok"]
        assert (await p.handle("camera_fly", {}))["reason"] == "unknown tool camera_fly"
        track = await p.handle("camera_control", control(action="track_on"))
        assert track["ok"] is False and track["line"] == obsbot.APP_NEEDED_LINE
        usb.unplugged = True
        gone = panel(usb)
        assert (await gone.handle("camera_status", {}))["reason"] == usb.why
        await p.close()
        await gone.close()

    asyncio.run(go())


def test_run_is_the_code_path_line():
    async def go() -> None:
        usb = FakeUsb()
        p = panel(usb)
        assert await p.run(match("camera left a bit")) == "Camera pointing 10° left."
        assert await p.run(match("camera zoom in")) == "Zoom 25%."
        assert await p.run(match("where is the camera pointing")) == (
            "Tiny 3 is awake, pointing 10° left, zoom 25%.")
        assert await p.run(match("camera track me")) == obsbot.APP_NEEDED_LINE
        await p.close()

    asyncio.run(go())


def test_where_reads_in_the_owners_words():
    assert obsbot_panel.where(0, 0) == "straight ahead"
    assert obsbot_panel.where(20, -10) == "20° left, 10° up"
    assert obsbot_panel.where(-5, 30) == "5° right, 30° down"
    assert obsbot_panel.where(None, 0) == "somewhere I couldn't read"
