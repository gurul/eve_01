"""Host vision — frame decode, face geometry, the drop governor, the daemon edge.

No Vision framework here: the detector is a plain callable, and the daemon
tests drive Daemon methods against a stub transport the way test_listen_key
does.
"""

from __future__ import annotations

import asyncio
import base64
from pathlib import Path
from types import SimpleNamespace

import pytest

from eve_bridge import vision
from eve_bridge.vision import (
    FaceResult,
    FaceTracker,
    Frame,
    FrameRateGovernor,
    Rect,
    build_cam_cmd,
    build_face_cmd,
    configured_save_dir,
    decode_frame,
    encode_gray_png,
    pick_face,
)

# A real 4x2 baseline JPEG (488 bytes, written by ImageIO) so the jpeg
# branch is exercised with genuine bytes without a PIL dependency.
TINY_JPEG_B64 = (
    "/9j/4AAQSkZJRgABAQAASABIAAD/4QBARXhpZgAATU0AKgAAAAgAAYdpAAQAAAABAAAAGgAAAAAAAqACAAQAAAABAAAABKAD"
    "AAQAAAABAAAAAgAAAAD/7QA4UGhvdG9zaG9wIDMuMAA4QklNBAQAAAAAAAA4QklNBCUAAAAAABDUHYzZjwCyBOmACZjs+EJ+"
    "/8AACwgAAgAEAQERAP/EAB8AAAEFAQEBAQEBAAAAAAAAAAABAgMEBQYHCAkKC//EALUQAAIBAwMCBAMFBQQEAAABfQECAwAE"
    "EQUSITFBBhNRYQcicRQygZGhCCNCscEVUtHwJDNicoIJChYXGBkaJSYnKCkqNDU2Nzg5OkNERUZHSElKU1RVVldYWVpjZGVm"
    "Z2hpanN0dXZ3eHl6g4SFhoeIiYqSk5SVlpeYmZqio6Slpqeoqaqys7S1tre4ubrCw8TFxsfIycrS09TV1tfY2drh4uPk5ebn"
    "6Onq8fLz9PX29/j5+v/bAEMABgYGBgYGCgYGCg4KCgoOEg4ODg4SFxISEhISFxwXFxcXFxccHBwcHBwcHCIiIiIiIicnJycn"
    "LCwsLCwsLCwsLP/dAAQAAf/aAAgBAQAAPwCz4Tvr210eNLaeSIHGQjlQdqqg6HsqgD2AHQV//9k="
)
TINY_JPEG = base64.b64decode(TINY_JPEG_B64)

W, H = 160, 120


def _gray_obj(seq: int = 1, w: int = W, h: int = H, **extra) -> dict:
    obj = {"seq": seq, "w": w, "h": h, "fmt": "gray",
           "b64": base64.b64encode(bytes(w * h)).decode()}
    obj.update(extra)
    return obj


def _jpeg_obj(seq: int = 1, **extra) -> dict:
    obj = {"seq": seq, "w": 4, "h": 2, "fmt": "jpeg", "b64": TINY_JPEG_B64}
    obj.update(extra)
    return obj


def _frame(seq: int = 1) -> Frame:
    return Frame(seq=seq, w=W, h=H, fmt="gray", data=bytes(W * H))


# ---- decode_frame -----------------------------------------------------------

def test_decode_gray() -> None:
    f = decode_frame(_gray_obj(seq=7, yaw=12.5, pitch=-3))
    assert (f.seq, f.w, f.h, f.fmt) == (7, W, H, "gray")
    assert len(f.data) == W * H
    assert (f.yaw, f.pitch) == (12.5, -3.0)


def test_decode_jpeg_keeps_file_bytes() -> None:
    f = decode_frame(_jpeg_obj())
    assert f.fmt == "jpeg"
    assert f.data == TINY_JPEG
    assert f.data[:2] == b"\xff\xd8" and f.data[-2:] == b"\xff\xd9"
    assert f.yaw is None and f.pitch is None


def test_decode_yaw_pitch_ignore_junk() -> None:
    f = decode_frame(_gray_obj(yaw="12", pitch=True))
    assert f.yaw is None and f.pitch is None


@pytest.mark.parametrize("bad", [
    {**_gray_obj(), "fmt": "rgb"},
    {**_gray_obj(), "b64": "not base64!"},
    {**_gray_obj(), "b64": ""},
    {**_gray_obj(), "w": 0},
    {**_gray_obj(w=10, h=10), "w": 11},                     # gray length mismatch
    {**_jpeg_obj(), "b64": base64.b64encode(b"PNG.....").decode()},   # no SOI
    {k: v for k, v in _gray_obj().items() if k != "seq"},
])
def test_decode_rejects_malformed(bad: dict) -> None:
    with pytest.raises(ValueError):
        decode_frame(bad)


# ---- pick_face geometry -----------------------------------------------------

def test_pick_face_centre() -> None:
    r = pick_face([Rect(60, 40, 40, 40, conf=0.9)], W, H)   # centre (80, 60)
    assert r == FaceResult(bx=0, by=0, size=25, conf=90)


def test_pick_face_right_is_positive_bx() -> None:
    r = pick_face([Rect(120, 40, 40, 40)], W, H)            # centre x = 140
    assert r is not None and r.bx == 75 and r.by == 0


def test_pick_face_down_is_positive_by() -> None:
    r = pick_face([Rect(60, 90, 40, 30)], W, H)             # centre y = 105
    assert r is not None and r.by == 75 and r.bx == 0


def test_pick_face_top_left_is_negative() -> None:
    r = pick_face([Rect(0, 0, 16, 12)], W, H)               # centre (8, 6)
    assert r is not None and r.bx == -90 and r.by == -90 and r.size == 10


def test_pick_face_largest_wins() -> None:
    small = Rect(0, 0, 10, 10, conf=0.99)
    big = Rect(100, 20, 50, 50, conf=0.5)
    r = pick_face([small, big], W, H)
    assert r == FaceResult(bx=56, by=-25, size=31, conf=50)


def test_pick_face_clamps_out_of_frame() -> None:
    r = pick_face([Rect(150, 110, 60, 60)], W, H)
    assert r is not None and r.bx == 100 and r.by == 100 and r.size == 38


def test_pick_face_none_without_rects() -> None:
    assert pick_face([], W, H) is None


# ---- wire shapes ------------------------------------------------------------

def test_face_cmd_with_face_echoes_pose() -> None:
    cmd = build_face_cmd(42, FaceResult(bx=10, by=-20, size=30, conf=95), yaw=15.0, pitch=-5.0)
    assert cmd == {"cmd": "face", "seq": 42, "bx": 10, "by": -20, "size": 30, "conf": 95,
                   "yaw": 15.0, "pitch": -5.0, "who": "unknown"}


def test_face_cmd_without_pose_omits_it() -> None:
    cmd = build_face_cmd(1, FaceResult(bx=0, by=0, size=5, conf=50))
    assert "yaw" not in cmd and "pitch" not in cmd
    assert cmd["who"] == "unknown"


def test_face_cmd_no_face_is_conf_zero() -> None:
    assert build_face_cmd(9, None, yaw=1.0, pitch=2.0) == {
        "cmd": "face", "seq": 9, "conf": 0, "who": "unknown"}


def test_face_cmd_who_override() -> None:
    assert build_face_cmd(1, None, who="owner")["who"] == "owner"


def test_cam_cmds() -> None:
    assert build_cam_cmd(True) == {"cmd": "cam", "on": True, "fps": 5, "w": 160, "h": 120}
    assert build_cam_cmd(False) == {"cmd": "cam", "on": False}


# ---- governor ---------------------------------------------------------------

def test_governor_runs_first_frame_immediately() -> None:
    g = FrameRateGovernor()
    f = _frame(1)
    assert g.offer(f) is f
    assert g.busy
    assert g.done() is None
    assert not g.busy
    assert (g.received, g.processed, g.dropped) == (1, 1, 0)


def test_governor_keeps_only_newest_while_busy() -> None:
    g = FrameRateGovernor()
    assert g.offer(_frame(1)) is not None
    assert g.offer(_frame(2)) is None       # parked
    assert g.offer(_frame(3)) is None       # displaces 2
    assert g.offer(_frame(4)) is None       # displaces 3
    assert g.dropped == 2
    nxt = g.done()
    assert nxt is not None and nxt.seq == 4
    assert g.busy                            # still running (frame 4)
    assert g.done() is None
    assert not g.busy
    assert (g.received, g.processed, g.dropped) == (4, 2, 2)


# ---- FaceTracker ------------------------------------------------------------

class _Sent:
    def __init__(self) -> None:
        self.items: list[dict] = []

    async def __call__(self, obj: dict) -> bool:
        self.items.append(obj)
        return True


def _tracker(detect, **kw) -> tuple[FaceTracker, _Sent]:
    sent = _Sent()
    return FaceTracker(detect=detect, send=sent, **kw), sent


def test_tracker_replies_face_for_each_processed_frame() -> None:
    def detect(frame: Frame) -> list[Rect]:
        return [Rect(120, 40, 40, 40, conf=0.8)] if frame.seq % 2 else []

    async def go() -> list[dict]:
        t, sent = _tracker(detect)
        await t.on_frame(_gray_obj(seq=1, yaw=10, pitch=2))
        await asyncio.sleep(0.05)
        await t.on_frame(_gray_obj(seq=2))
        await asyncio.sleep(0.05)
        t.stop()
        return sent.items

    sent = asyncio.run(go())
    assert sent == [
        {"cmd": "face", "seq": 1, "bx": 75, "by": 0, "size": 25, "conf": 80,
         "yaw": 10.0, "pitch": 2.0, "who": "unknown"},
        {"cmd": "face", "seq": 2, "conf": 0, "who": "unknown"},
    ]


def test_tracker_drops_to_newest_while_detector_busy() -> None:
    import threading

    gate = threading.Event()
    seen: list[int] = []

    def slow_detect(frame: Frame) -> list[Rect]:
        seen.append(frame.seq)
        gate.wait(timeout=2.0)
        return []

    async def go() -> tuple[list[int], list[int], int]:
        t, sent = _tracker(slow_detect)
        for seq in (1, 2, 3, 4):
            await t.on_frame(_gray_obj(seq=seq))
        await asyncio.sleep(0.05)       # frame 1 is inside the detector
        gate.set()
        await asyncio.sleep(0.2)
        t.stop()
        return seen, [o["seq"] for o in sent.items], t.governor.dropped

    seen_seqs, replied, dropped = asyncio.run(go())
    assert seen_seqs == [1, 4]           # 2 and 3 were never queued
    assert replied == [1, 4]
    assert dropped == 2


def test_tracker_disabled_ignores_frames() -> None:
    async def go() -> tuple[list[dict], int]:
        t, sent = _tracker(None)
        assert not t.enabled
        await t.on_frame(_gray_obj())
        await asyncio.sleep(0.01)
        return sent.items, t.governor.received

    assert asyncio.run(go()) == ([], 0)


def test_tracker_counts_bad_frames_without_replying() -> None:
    async def go() -> tuple[int, list[dict]]:
        t, sent = _tracker(lambda f: [])
        await t.on_frame({**_gray_obj(), "fmt": "rgb"})
        return t.bad_frames, sent.items

    assert asyncio.run(go()) == (1, [])


def test_tracker_stats_line_and_window_reset() -> None:
    now = [100.0]

    def detect(frame: Frame) -> list[Rect]:
        now[0] += 0.02                   # 20 ms per detect on the fake clock
        return [Rect(0, 0, 10, 10)] if frame.seq == 1 else []

    async def go() -> tuple[str | None, str | None]:
        t, _ = _tracker(detect, clock=lambda: now[0])
        assert t.stats_line() is None    # nothing yet
        for seq in (1, 2):
            await t.on_frame(_gray_obj(seq=seq))
            await asyncio.sleep(0.05)
        now[0] += 10.0
        line = t.stats_line()
        again = t.stats_line()           # window reset: nothing new
        t.stop()
        return line, again

    line, again = asyncio.run(go())
    assert line == "vision: 2 frames, 0 dropped, 0.2 fps, 20 ms/detect, faces 50%"
    assert again is None


# ---- frame dump -------------------------------------------------------------

def test_gray_png_is_valid_png() -> None:
    png = encode_gray_png(3, 2, bytes([0, 128, 255, 10, 20, 30]))
    assert png.startswith(b"\x89PNG\r\n\x1a\n")
    assert b"IHDR" in png and b"IDAT" in png and png.endswith(b"IEND\xaeB`\x82")
    with pytest.raises(ValueError):
        encode_gray_png(3, 2, b"short")


def test_tracker_saves_at_most_one_frame_per_second(tmp_path: Path) -> None:
    now = [0.0]

    async def go() -> list[str]:
        t, _ = _tracker(lambda f: [], save_dir=tmp_path, clock=lambda: now[0])
        await t.on_frame(_gray_obj(seq=1))
        now[0] += 0.5
        await t.on_frame(_jpeg_obj(seq=2))          # inside the 1 s window: skipped
        now[0] += 0.6
        await t.on_frame(_jpeg_obj(seq=3))
        await asyncio.sleep(0.05)
        t.stop()
        return sorted(p.name for p in tmp_path.iterdir())

    assert asyncio.run(go()) == ["frame_000001.png", "frame_000003.jpg"]
    assert (tmp_path / "frame_000003.jpg").read_bytes() == TINY_JPEG


def test_configured_save_dir(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("EVE_SAVE_FRAMES", raising=False)
    assert configured_save_dir(None) is None
    monkeypatch.setenv("EVE_SAVE_FRAMES", "/tmp/frames ")
    assert configured_save_dir(None) == Path("/tmp/frames")
    assert configured_save_dir("/cli/wins") == Path("/cli/wins")


# ---- adapter availability ----------------------------------------------------

def test_make_detector_none_off_darwin(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(vision.sys, "platform", "linux")
    assert vision.make_detector() is None


# ---- daemon wire edge -------------------------------------------------------

class _StubBle:
    def __init__(self, connected: bool = True) -> None:
        self.connected = connected
        self.sent: list[dict] = []

    async def send(self, obj: dict, codec=None) -> bool:
        self.sent.append(obj)
        return True

    async def wait_connected(self) -> None:
        return None


def _daemon(detect, connected: bool = True) -> SimpleNamespace:
    from types import MethodType

    from eve_bridge.daemon import Daemon
    from eve_bridge.scene import SceneWatcher

    ble = _StubBle(connected)
    # _explorer/_note_activity: the idle explorer's hooks in _handle_ble (explore.py).
    # _head/_scene/_sound: the frame's head pose, the voice's eyes (inert outside a
    # conversation) and the owner's mute choice, re-sent on every resync.
    d = SimpleNamespace(ble=ble, _listen_sent=None, _shutdown=asyncio.Event(),
                        _explorer=SimpleNamespace(active=False), _note_activity=lambda: None,
                        _agent_state="idle", _head=SimpleNamespace(observe=lambda yaw, pitch: None),
                        _scene=SceneWatcher(None), _sound=SimpleNamespace(on=True, muted=False))
    d._vision = FaceTracker(detect=detect, send=ble.send)
    for name in ("_reset_listen", "_resync_agent", "_send_cam", "_send_sound"):
        setattr(d, name, MethodType(getattr(Daemon, name), d))
    return d


def test_daemon_resync_sends_cam_on_after_time_sync() -> None:
    from eve_bridge.daemon import Daemon

    async def go() -> list[dict]:
        d = _daemon(lambda f: [])
        d._push_heartbeat = lambda force=False: asyncio.sleep(0)
        await Daemon._resync_board(d)
        return d.ble.sent

    sent = asyncio.run(go())
    assert "time" in sent[0]
    cam = sent.index({"cmd": "cam", "on": True, "fps": 5, "w": 160, "h": 120})
    assert sent.index({"cmd": "listen", "on": False}) < cam
    # ... and the owner's mute choice goes out last, so a rebooted board comes back with it
    assert sent[-1] == {"cmd": "sound", "on": True} and cam == len(sent) - 2


def test_daemon_no_cam_on_without_detector() -> None:
    from eve_bridge.daemon import Daemon

    async def go() -> list[dict]:
        d = _daemon(None)
        await Daemon._send_cam(d, True)
        await Daemon._send_cam(d, False)
        return d.ble.sent

    assert asyncio.run(go()) == []


def test_daemon_no_cam_while_disconnected() -> None:
    from eve_bridge.daemon import Daemon

    async def go() -> list[dict]:
        d = _daemon(lambda f: [], connected=False)
        await Daemon._send_cam(d, True)
        return d.ble.sent

    assert asyncio.run(go()) == []


def test_daemon_frame_object_gets_a_face_reply() -> None:
    from eve_bridge.daemon import Daemon

    async def go() -> list[dict]:
        d = _daemon(lambda f: [Rect(60, 40, 40, 40, conf=1.0)])
        await Daemon._handle_ble(d, {"frame": _gray_obj(seq=5, yaw=0, pitch=0)})
        await asyncio.sleep(0.05)
        d._vision.stop()
        return d.ble.sent

    assert asyncio.run(go()) == [
        {"cmd": "face", "seq": 5, "bx": 0, "by": 0, "size": 25, "conf": 100,
         "yaw": 0.0, "pitch": 0.0, "who": "unknown"},
    ]
