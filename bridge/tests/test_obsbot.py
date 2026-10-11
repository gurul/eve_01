"""obsbot.py: the camera over USB (a fake UsbCamera that behaves as the Tiny 3 measured on 2026-10-11) and OBSBOT's
own features over OSC (a fake OBSBOT Center on a free UDP port, answering the way the real one did)."""

from __future__ import annotations

import json
import socket
import threading
from typing import Any, Optional

import pytest

from eve_bridge import obsbot, obsbot_usb
from eve_bridge.obsbot import G, Obsbot, T, decode, encode


class FakeUsb:
    """UsbCamera's surface. Asleep, it reads tilt -79 and ignores moves, as the camera does."""

    def __init__(self, name: str = "OBSBOT Tiny 3"):
        self.name, self.pan, self.tilt, self.zoom_level = name, 0.0, 0.0, 0
        self.asleep, self.unplugged, self.why = False, False, ""
        self.calls: list[tuple] = []

    def read(self) -> Optional[dict[str, Any]]:
        if self.unplugged:
            self.why = obsbot_usb.NO_CAMERA_LINE
            return None
        return {"ok": True, "name": self.name, "pan": self.pan, "tilt": -79.0 if self.asleep else self.tilt,
                "zoom": self.zoom_level, "asleep": self.asleep}

    def wake(self) -> bool:
        self.calls.append(("wake",))
        self.asleep = False
        return not self.unplugged

    def point(self, pan: float, tilt: float) -> bool:
        self.calls.append(("point", pan, tilt))
        if not self.asleep:
            self.pan, self.tilt = pan, tilt
        return True

    def turn(self, direction: str, speed: int = obsbot_usb.TURN_SPEED) -> bool:
        self.calls.append(("turn", direction))
        if direction != "stop":
            d = {"left": (5, 0), "right": (-5, 0), "up": (0, 5), "down": (0, -5)}[direction]
            self.pan, self.tilt = self.pan + d[0], self.tilt + d[1]
        return True

    def zoom(self, level: int) -> bool:
        self.calls.append(("zoom", level))
        self.zoom_level = level
        return True


class FakeCenter:
    def __init__(self, usb: Optional[FakeUsb] = None):
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.sock.bind(("127.0.0.1", 0))
        self.port = self.sock.getsockname()[1]
        self.usb = usb
        self.got: list[tuple[str, list]] = []
        self._stop = False
        self.t = threading.Thread(target=self.run, daemon=True)
        self.t.start()

    def run(self):
        self.sock.settimeout(0.1)
        while not self._stop:
            try:
                data, src = self.sock.recvfrom(65536)
            except (socket.timeout, OSError):
                continue
            address, args = decode(data)
            self.got.append((address, args))
            name = address.rsplit("/", 1)[-1]
            if name == "Connected":
                self.sock.sendto(encode(G + "ConnectedResp", 1), src)
            elif name == "GetPresetPositionInfo" and len(args) == 1:
                self.sock.sendto(encode(T + "PresetPositionInfo", 1, "Desk", 0, "Add", 0, "Add"), src)
            elif name == "GetAiTrackingInfo" and len(args) == 1:
                self.sock.sendto(encode(T + "AiTrackingInfo", 0), src)
            elif name == "WakeSleep" and self.usb is not None:
                self.usb.asleep = not bool(args[1])

    def close(self):
        self._stop = True
        self.t.join()
        self.sock.close()


def arrives(center: FakeCenter, message: tuple, within: float = 1.0) -> bool:
    """A fire-and-forget command reaches the fake on its own thread; wait for it (time.sleep is patched out)."""
    import time
    end = time.monotonic() + within
    while time.monotonic() < end:
        if message in center.got:
            return True
    return False


@pytest.fixture
def usb():
    return FakeUsb()


@pytest.fixture
def cam(usb):
    o = Obsbot(port=1, app_running=lambda: False, usb=usb)                    # no OBSBOT Center
    yield o
    o.close()


@pytest.fixture
def center(usb):
    c = FakeCenter(usb)
    yield c
    c.close()


@pytest.fixture
def cam_app(usb, center):
    o = Obsbot(port=center.port, app_running=lambda: True, usb=usb)
    yield o
    o.close()


@pytest.fixture(autouse=True)
def quick(monkeypatch):
    monkeypatch.setattr(obsbot.time, "sleep", lambda s: None)


def test_osc_round_trip():
    data = encode("/OBSBOT/x", 3, -1.5, "Tiny 3")
    assert len(data) % 4 == 0
    assert decode(data) == ("/OBSBOT/x", [3, -1.5, "Tiny 3"])


def test_status_over_usb_alone(cam):
    assert cam.ensure() is None
    st = cam.status()
    assert st["ok"] and st["camera"] == "Tiny 3" and st["awake"] is True and st["app"] is False
    assert (st["pan"], st["pitch"], st["zoom"]) == (0, 0, 0)
    assert st["line"] == "Tiny 3 awake; pan 0°, pitch 0°; zoom 0%."


def test_status_asleep_and_unplugged(cam, usb):
    usb.asleep = True
    st = cam.status()
    assert st["awake"] is False and st["pan"] is None and st["line"] == "Tiny 3 asleep; zoom 0%."
    usb.unplugged = True
    assert cam.status() == {"ok": False, "line": obsbot_usb.NO_CAMERA_LINE}
    fresh = Obsbot(port=1, app_running=lambda: False, usb=usb)
    try:
        assert fresh.ensure() == obsbot_usb.NO_CAMERA_LINE
    finally:
        fresh.close()


def test_move_turns_pitch_into_tilt_and_reads_back(cam, usb):
    res = cam.move(45, 10)                      # pitch + is down: the camera's tilt -10
    assert res["ok"] and (res["pan"], res["pitch"]) == (45, 10)
    assert ("point", 45, -10) in usb.calls


def test_move_wakes_a_sleeping_camera_first(cam, usb):
    usb.asleep = True
    res = cam.move(20, 0)
    assert res["ok"] and usb.calls[:2] == [("wake",), ("point", 20, 0)]


def test_move_keeps_the_axis_not_given_and_clamps(cam, usb):
    usb.pan, usb.tilt = 20, 5
    assert cam.move(None, 15)["pan"] == 20
    cam.move(500, -500)
    assert ("point", 129, obsbot_usb.TILT_LIMIT) in usb.calls


def test_nudge_turns_then_stops(cam, usb):
    res = cam.nudge("left", seconds=0.5)
    assert usb.calls[-2:] == [("turn", "left"), ("turn", "stop")] and res["pan"] == 5
    assert not cam.nudge("sideways")["ok"]


def test_zoom_and_center(cam, usb):
    assert cam.zoom(60)["line"] == "Zoom 60%."
    usb.pan = 30
    assert cam.reset()["line"] == "Centered." and usb.pan == 0


def test_app_features_say_the_app_is_needed(cam, usb):
    for res in (cam.track(True), cam.mode("desk"), cam.preset(1), cam.wake(False), cam.fov(78), cam.mirror(True)):
        assert res == {"ok": False, "line": obsbot.APP_NEEDED_LINE}
    assert cam.wake(True)["line"] == "Tiny 3 awake."                           # waking is USB's: no app needed


def test_app_features_go_to_obsbot_center_when_it_runs(cam_app, center, usb):
    assert cam_app.track(True)["line"] == "Tracking on."
    assert arrives(center, (T + "ToggleAILock", [0, 1]))
    assert not cam_app.preset(2)["ok"]                                          # not saved
    assert cam_app.preset(1)["line"] == "Going to Desk."
    assert arrives(center, (T + "TriggerPreset", [0, 0]))
    assert cam_app.wake(False)["line"] == "Tiny 3 asleep." and usb.asleep


def test_ai_mode_refuses_another_model():
    other = FakeUsb(name="OBSBOT Tiny 2")
    o = Obsbot(port=1, app_running=lambda: True, usb=other)
    try:
        assert "Tiny 3" in o.mode("desk")["line"]
    finally:
        o.close()


def test_a_running_app_with_osc_off_is_named(usb):
    o = Obsbot(port=1, app_running=lambda: True, usb=usb)
    try:
        assert o.track(True)["line"] == obsbot.OFF_REASON
    finally:
        o.close()


def test_close_says_disconnected_only_to_an_app_it_reached(cam_app, center):
    cam_app.track(False)
    cam_app.close()
    assert arrives(center, (G + "Disconnected", [0]))


def maker(usb, port=1, running=False):
    def make(**kw):
        return Obsbot(port=port, app_running=lambda: running, usb=usb, **kw)
    return make


def test_cli_json_after_the_subcommand(usb):
    out: list[str] = []
    assert obsbot.cli(["status", "--json"], out=out.append, make=maker(usb)) == 0
    assert json.loads(out[0])["camera"] == "Tiny 3"


def test_cli_move_nudge_and_exit_codes(usb):
    out: list[str] = []
    assert obsbot.cli(["move", "--pan", "30", "--pitch", "-10"], out=out.append, make=maker(usb)) == 0
    assert out[-1] == "Pan 30°, pitch -10°."
    assert obsbot.cli(["track", "on"], out=out.append, make=maker(usb)) == 1
    assert out[-1] == obsbot.APP_NEEDED_LINE


def test_cli_raw_needs_an_obsbot_address_and_the_app(usb):
    out: list[str] = []
    assert obsbot.cli(["raw", "/other/thing"], out=out.append, make=maker(usb)) == 1
    assert obsbot.cli(["raw", "/OBSBOT/WebCam/General/GetZoomInfo", "0"], out=out.append, make=maker(usb)) == 1
    assert out[-1] == obsbot.APP_NEEDED_LINE


def test_setup_reads_the_camera_and_turns_osc_on_while_the_app_is_closed(tmp_path, monkeypatch, usb):
    ini = tmp_path / "global.ini"
    ini.write_text("[SoftSetting]\nOSC=false\nOSCHostIp=\nOSCReceivePort=16284\n")
    monkeypatch.setattr(obsbot, "APP_PATH", tmp_path)
    monkeypatch.setattr(obsbot, "_app_running", lambda: False)
    out: list[str] = []
    assert obsbot.setup(ini, out=out.append, usb=usb) == 0
    text = ini.read_text()
    assert "OSC=true" in text and "OSCHostIp=127.0.0.1" in text and "OSCConnectMethod=0" in text
    assert "answers over USB" in out[0]


def test_setup_leaves_a_running_app_alone(tmp_path, monkeypatch, usb):
    ini = tmp_path / "global.ini"
    ini.write_text("[SoftSetting]\nOSC=false\n")
    monkeypatch.setattr(obsbot, "APP_PATH", tmp_path)
    monkeypatch.setattr(obsbot, "_app_running", lambda: True)
    out: list[str] = []
    assert obsbot.setup(ini, out=out.append, usb=usb) == 0
    assert ini.read_text() == "[SoftSetting]\nOSC=false\n" and "turn on OSC in its settings" in out[-1]


def test_setup_without_a_camera_fails(tmp_path, usb):
    usb.unplugged = True
    out: list[str] = []
    assert obsbot.setup(tmp_path / "none.ini", out=out.append, usb=usb) == 1
    assert out == [obsbot_usb.NO_CAMERA_LINE]
