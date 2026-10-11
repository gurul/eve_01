"""obsbot.py against a fake OBSBOT Center: a UDP OSC server on a free port that answers the way the real one did
on 2026-10-11 (queries answered only with the device index alone; a fast move that drops short; pan/pitch reply)."""

from __future__ import annotations

import json
import socket
import threading

import pytest

from eve_bridge import obsbot
from eve_bridge.obsbot import G, Obsbot, T, decode, encode


class FakeCenter:
    def __init__(self, name: str = "Tiny 3"):
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.sock.bind(("127.0.0.1", 0))
        self.port = self.sock.getsockname()[1]
        self.name, self.awake, self.pan, self.pitch, self.zoom = name, True, 0, 0, 0
        self.drop_fast_moves = False
        self.got: list[tuple[str, list]] = []
        self._stop = False
        self.t = threading.Thread(target=self.run, daemon=True)
        self.t.start()

    def reply(self, to, address, *args):
        self.sock.sendto(encode(address, *args), to)

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
                self.reply(src, G + "ConnectedResp", 1)
                self.reply(src, G + "ZoomInfo", self.zoom, -1)          # the push that follows, unasked
            elif name == "GetDeviceInfo" and len(args) == 1:
                self.reply(src, G + "DeviceInfo", 1, self.name, 0, "", 0, "", 0, "", 0, int(self.awake), 18)
            elif name == "GetGimbalPosInfo" and len(args) == 1:
                self.reply(src, G + "GetGimbalPosInfoResp", self.pan, self.pitch)
            elif name == "GetZoomInfo" and len(args) == 1:
                self.reply(src, G + "ZoomInfo", self.zoom, -1)
            elif name == "GetPresetPositionInfo" and len(args) == 1:
                self.reply(src, T + "PresetPositionInfo", 1, "Desk", 0, "Add", 0, "Add")
            elif name == "GetAiTrackingInfo" and len(args) == 1:
                self.reply(src, T + "AiTrackingInfo", 0)
            elif name == "SetGimMotorDegree":
                _, speed, pan, pitch = args
                if not (self.drop_fast_moves and speed > 20):
                    self.pan, self.pitch = pan, pitch
            elif name == "SetZoom":
                self.zoom = args[1]
            elif name == "WakeSleep":
                self.awake = bool(args[1])

    def close(self):
        self._stop = True
        self.t.join()
        self.sock.close()


def arrives(center, message, within=1.0):
    """A fire-and-forget command reaches the fake on its own thread; wait for it (time.sleep is patched out)."""
    import time
    end = time.monotonic() + within
    while time.monotonic() < end:
        if message in center.got:
            return True
    return False


@pytest.fixture
def center():
    c = FakeCenter()
    yield c
    c.close()


@pytest.fixture
def cam(center):
    o = Obsbot(port=center.port, launch=False, app_running=lambda: True, launch_app=lambda: None)
    yield o
    o.close()


@pytest.fixture(autouse=True)
def quick(monkeypatch):
    monkeypatch.setattr(obsbot.time, "sleep", lambda s: None)


def test_osc_round_trip():
    data = encode("/OBSBOT/x", 3, -1.5, "Tiny 3")
    assert len(data) % 4 == 0
    assert decode(data) == ("/OBSBOT/x", [3, -1.5, "Tiny 3"])


def test_status_reads_everything(cam):
    assert cam.ensure() is None
    st = cam.status()
    assert st["ok"] and st["camera"] == "Tiny 3" and st["awake"] is True
    assert (st["pan"], st["pitch"], st["zoom"]) == (0, 0, 0)
    assert st["line"] == "Tiny 3 awake; pan 0°, pitch 0°; zoom 0%; presets: Desk."


def test_queries_send_the_device_index_alone(cam, center):
    cam.ensure()
    cam.position()
    sent = [a for addr, a in center.got if addr.endswith("GetGimbalPosInfo")]
    assert sent == [[0]]


def test_move_lands_and_reads_back(cam, center):
    cam.ensure()
    res = cam.move(45, 10)
    assert res["ok"] and (res["pan"], res["pitch"]) == (45, 10)
    assert ("/OBSBOT/WebCam/General/SetGimMotorDegree", [0, 30, 45, 10]) in center.got


def test_move_dropped_at_speed_is_resent_at_20(cam, center):
    center.drop_fast_moves = True
    cam.ensure()
    res = cam.move(10, 10)
    speeds = [a[1] for addr, a in center.got if addr.endswith("SetGimMotorDegree")]
    assert speeds == [30, 20] and res["ok"]


def test_move_keeps_the_axis_not_given(cam, center):
    center.pan, center.pitch = 20, -5
    cam.ensure()
    assert cam.move(None, 15)["pan"] == 20


def test_move_clamps_to_the_gimbal_range(cam, center):
    cam.ensure()
    cam.move(500, -500)
    assert ("/OBSBOT/WebCam/General/SetGimMotorDegree", [0, 30, 129, -59]) in center.got


def test_zoom_and_sleep(cam, center):
    cam.ensure()
    assert cam.zoom(60)["line"] == "Zoom 60%."
    assert cam.wake(False)["line"] == "Tiny 3 asleep."
    assert center.awake is False


def test_preset_that_is_not_saved_is_refused(cam, center):
    cam.ensure()
    assert not cam.preset(2)["ok"]
    assert cam.preset(1)["line"] == "Going to Desk."
    assert arrives(center, ("/OBSBOT/WebCam/Tiny/TriggerPreset", [0, 0]))


def test_ai_mode_refuses_another_model(center):
    center.name = "Tiny 2"
    o = Obsbot(port=center.port, launch=False, app_running=lambda: True)
    try:
        o.ensure()
        assert "Tiny 3" in o.mode("desk")["line"]
        assert not any(a.endswith("SetAiMode") for a, _ in center.got)
    finally:
        o.close()


def test_silent_app_is_reported():
    o = Obsbot(port=1, launch=False, app_running=lambda: True)
    try:
        assert o.ensure() == obsbot.OFF_REASON
    finally:
        o.close()


def maker(center):
    def make(**kw):
        return Obsbot(port=center.port, app_running=lambda: True, **kw)
    return make


def test_cli_json_after_the_subcommand(center, monkeypatch):
    out: list[str] = []
    make = maker(center)
    assert obsbot.cli(["status", "--json"], out=out.append, make=make) == 0
    assert json.loads(out[0])["camera"] == "Tiny 3"


def test_cli_raw_only_sends_obsbot_addresses(center):
    out: list[str] = []
    make = maker(center)
    assert obsbot.cli(["raw", "/other/thing"], out=out.append, make=make) == 1


def test_setup_turns_osc_on(tmp_path, monkeypatch):
    ini = tmp_path / "global.ini"
    ini.write_text("[SoftSetting]\nOSC=false\nOSCHostIp=\nOSCReceivePort=16284\n")
    monkeypatch.setattr(obsbot, "_app_running", lambda: False)
    monkeypatch.setattr(obsbot, "_launch_app", lambda: None)
    monkeypatch.setattr(obsbot.Obsbot, "ensure", lambda self: None)
    assert obsbot.setup(ini, out=lambda s: None) == 0
    text = ini.read_text()
    assert "OSC=true" in text and "OSCHostIp=127.0.0.1" in text and "OSCConnectMethod=0" in text
