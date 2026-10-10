"""Daemon wiring for the voice's eyes, head and mute: frames reach the scene
watcher only while a conversation is open, the echoed pose reaches the head,
board loss reads as camera loss, and the owner's mute choice is persisted,
sent on connect, re-sent on a status-ack mismatch and strips caption chirps.

No board, no network: the SimpleNamespace + MethodType stub test_daemon_touch.py uses.
"""

from __future__ import annotations

import asyncio
import base64
import json
from types import MethodType, SimpleNamespace

from eve_bridge.daemon import Daemon
from eve_bridge.head import Head
from eve_bridge.scene import SceneConfig, SceneWatcher
from eve_bridge.sound import SoundSetting


class _Ble:
    def __init__(self, connected: bool = True) -> None:
        self.connected = connected
        self.sent: list[dict] = []

    async def send(self, obj: dict, codec=None) -> bool:
        self.sent.append(obj)
        return True


class _Vision:
    enabled = True

    def __init__(self) -> None:
        self.frames: list[dict] = []

    async def on_frame(self, frame: dict) -> None:
        self.frames.append(frame)


class _NoClient:
    def describe(self, image, mime, previous):
        return ("A desk.", "")

    def locate(self, image, mime, target):
        return {"visible": False, "x": 0.0, "y": 0.0, "what": ""}


def _frame(seq: int, yaw: int = -30, pitch: int = 50) -> dict:
    return {"frame": {"seq": seq, "w": 160, "h": 120, "fmt": "jpeg",
                      "b64": base64.b64encode(b"\xff\xd8x").decode(), "yaw": yaw, "pitch": pitch}}


def _daemon(tmp_path, connected: bool = True) -> SimpleNamespace:
    ble = _Ble(connected)
    d = SimpleNamespace(
        ble=ble,
        _vision=_Vision(),
        _explorer=SimpleNamespace(active=False),
        _explore_raw_frame=None,
        _snap_waiter=None,
        _sound=SoundSetting(tmp_path / "sound.json"),
        _status_sent_at=None,
        _status_missed=0,
        _clean_polls=0,
        _ack_escalation=0,
        _last_stick_sec=False,
        _last_stick_battery_pct=None,
        _last_activity_at=0.0,
    )
    d._sound.load()
    d._scene = SceneWatcher(_NoClient(), SceneConfig(), camera_ok=lambda: ble.connected and d._vision.enabled)
    d._head = Head(ble.send, connected=lambda: ble.connected)
    for name in ("_handle_ble", "_on_caption", "_send_sound", "_set_sound", "_handle_ipc", "_note_activity"):
        setattr(d, name, MethodType(getattr(Daemon, name), d))
    return d


def test_frames_reach_the_scene_only_during_a_conversation_and_pose_reaches_the_head(tmp_path) -> None:
    async def go():
        d = _daemon(tmp_path)
        await d._handle_ble(_frame(1, yaw=-30, pitch=50))
        assert d._scene._frame is None                      # no conversation: nothing held
        assert (d._head.yaw, d._head.pitch) == (-30.0, 50.0)
        assert len(d._vision.frames) == 1                   # face tracking is unchanged
        d._scene.start(run_loop=False)
        await d._handle_ble(_frame(2, yaw=10))
        assert d._scene._frame is not None and d._scene._frame["seq"] == 2
        await d._scene.stop()
        assert d._scene._frame is None
    asyncio.run(go())


def test_board_loss_is_camera_loss(tmp_path) -> None:
    async def go():
        d = _daemon(tmp_path)
        d._scene.start(run_loop=False)
        await d._handle_ble(_frame(1))
        assert d._scene.camera_state(d._scene.clock()) == "ok"
        d.ble.connected = False
        assert d._scene.camera_state(d._scene.clock()) == "lost"
        assert (await d._head.move(10, 45))["ok"] is False  # nothing to move either
        await d._scene.stop()
    asyncio.run(go())


def test_sound_is_sent_on_connect_and_mute_persists(tmp_path) -> None:
    async def go():
        d = _daemon(tmp_path)
        await d._send_sound()
        assert d.ble.sent[-1] == {"cmd": "sound", "on": True}
        d._set_sound(False)
        await asyncio.sleep(0)
        assert d.ble.sent[-1] == {"cmd": "sound", "on": False}
        assert json.loads((tmp_path / "sound.json").read_text()) == {"muted": True}
        assert SoundSetting(tmp_path / "sound.json").load() is False     # a restart keeps it
    asyncio.run(go())


def test_mute_without_a_board_is_kept_for_the_next_connect(tmp_path) -> None:
    async def go():
        d = _daemon(tmp_path, connected=False)
        d._set_sound(False)
        await asyncio.sleep(0)
        assert d.ble.sent == [] and d._sound.muted
        d.ble.connected = True
        await d._send_sound()
        assert d.ble.sent == [{"cmd": "sound", "on": False}]
    asyncio.run(go())


def test_caption_chirps_are_dropped_while_muted(tmp_path) -> None:
    async def go():
        d = _daemon(tmp_path)
        page = {"cmd": "caption", "page": 0, "of": 1, "lines": ["hi"], "chirp": True}
        d._on_caption(page)
        await asyncio.sleep(0)
        assert d.ble.sent[-1]["chirp"] is True
        d._sound.set(False)
        d._on_caption(page)
        await asyncio.sleep(0)
        assert d.ble.sent[-1]["chirp"] is False and d.ble.sent[-1]["lines"] == ["hi"]
    asyncio.run(go())


def test_status_ack_mismatch_resends_the_owners_choice(tmp_path) -> None:
    async def go():
        d = _daemon(tmp_path)
        d._sound.set(False)
        await d._handle_ble({"ack": "status", "ok": True, "data": {"sec": False, "snd": True}})
        await asyncio.sleep(0)
        assert {"cmd": "sound", "on": False} in d.ble.sent
        d.ble.sent.clear()
        await d._handle_ble({"ack": "status", "ok": True, "data": {"sec": False, "snd": False}})
        await asyncio.sleep(0)
        assert d.ble.sent == []                              # in agreement: nothing sent
        await d._handle_ble({"ack": "status", "ok": True, "data": {"sec": False}})
        await asyncio.sleep(0)
        assert d.ble.sent == []                              # older firmware: no field, no resend loop
    asyncio.run(go())


def test_ipc_sound_command(tmp_path) -> None:
    async def go():
        d = _daemon(tmp_path)
        assert await d._handle_ipc({"evt": "sound", "action": "status"}) == {
            "ok": True, "sound": "on", "connected": True}
        assert (await d._handle_ipc({"evt": "sound", "action": "off"}))["sound"] == "off"
        await asyncio.sleep(0)
        assert d.ble.sent[-1] == {"cmd": "sound", "on": False}
        assert (await d._handle_ipc({"evt": "sound", "action": "on"}))["sound"] == "on"
    asyncio.run(go())


def test_status_ack_sec_ignored_on_a_wired_link(tmp_path, caplog) -> None:
    # SETUP-ISSUES #10: over USB the board's BLE "sec" flag is not this link's security.
    async def go():
        d = _daemon(tmp_path)
        d._last_stick_sec = None
        d.ble.wired = True
        with caplog.at_level("INFO", logger="eve_bridge.daemon"):
            await d._handle_ble({"ack": "status", "ok": True, "data": {"sec": False}})
        assert d._last_stick_sec is None
        assert "UNENCRYPTED" not in caplog.text
    asyncio.run(go())


def test_status_ack_sec_still_warns_over_ble(tmp_path, caplog) -> None:
    async def go():
        d = _daemon(tmp_path)
        d._last_stick_sec = None
        with caplog.at_level("INFO", logger="eve_bridge.daemon"):
            await d._handle_ble({"ack": "status", "ok": True, "data": {"sec": False}})
        assert d._last_stick_sec is False
        assert "UNENCRYPTED" in caplog.text
    asyncio.run(go())
