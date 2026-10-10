"""'hey eve, take a picture': the daemon side of the voice's take_photo tool."""

from __future__ import annotations

import asyncio
from pathlib import Path
from types import MethodType, SimpleNamespace

from eve_bridge import mac_camera
from eve_bridge.daemon import Daemon
from eve_bridge.vision import Frame

JPEG = b"\xff\xd8" + b"\x00" * 64 + b"\xff\xd9"


def _frame() -> Frame:
    return Frame(seq=1, w=320, h=240, fmt="jpeg", data=JPEG, snap=True)


class _Notes:
    def __init__(self) -> None:
        self.kept: list[tuple[str, int, int]] = []

    async def keep(self, frame, said="", yaw=0, pitch=45):
        self.kept.append((said, yaw, pitch))
        return SimpleNamespace(photo="photos/2026-09-13/120000-3.jpg", caption="the red bike by the door")


def _daemon(tmp_path: Path, connected=True, snap=None, newest=None, notes="diary") -> SimpleNamespace:
    async def take_snapshot():
        return snap

    d = SimpleNamespace(
        ble=SimpleNamespace(connected=connected),
        _take_snapshot=take_snapshot,
        _scene=SimpleNamespace(newest_frame=lambda: newest),
        _head=SimpleNamespace(yaw=19.6, pitch=40.2),
        _notes=_Notes() if notes == "diary" else None,
        _explore_cfg=SimpleNamespace(notes_dir=tmp_path / "notes"),
        _note_activity=lambda: None,
    )
    d._photo_for_owner = MethodType(Daemon._photo_for_owner, d)
    d._mac_photo = MethodType(Daemon._mac_photo, d)
    return d


def test_a_snap_from_the_board_is_kept_with_the_owners_words_and_pose(tmp_path) -> None:
    d = _daemon(tmp_path, snap=_frame())
    r = asyncio.run(d._photo_for_owner("my bike"))
    assert r["ok"] and r["caption"] == "the red bike by the door"
    assert r["path"] == str(tmp_path / "notes" / "photos/2026-09-13/120000-3.jpg")
    assert r["answer"] == "Kept it: the red bike by the door"
    assert d._notes.kept == [("my bike", 20, 40)]


def test_the_streamed_frame_stands_in_when_the_board_cannot_snap(tmp_path) -> None:
    raw = {"seq": 9, "w": 160, "h": 120, "fmt": "jpeg", "b64": __import__("base64").b64encode(JPEG).decode()}
    d = _daemon(tmp_path, snap=None, newest=raw)
    r = asyncio.run(d._photo_for_owner(""))
    assert r["ok"] and d._notes.kept == [("", 20, 40)]


def test_no_frame_from_the_board_is_a_reason_not_a_photo(tmp_path) -> None:
    r = asyncio.run(_daemon(tmp_path, snap=None, newest=None)._photo_for_owner("x"))
    assert r["ok"] is False and "no picture" in r["reason"]


def test_without_a_diary_model_the_file_and_a_plain_line_are_still_kept(tmp_path) -> None:
    d = _daemon(tmp_path, snap=_frame(), notes=None)
    r = asyncio.run(d._photo_for_owner("the plant"))
    assert r["ok"] and r["caption"] == "the plant"
    photo = Path(r["path"])
    assert photo.read_bytes() == JPEG and photo.parent.parent.name == "photos"
    day = next((tmp_path / "notes").glob("*.md")).read_text()
    assert "A picture you asked for: the plant" in day and f"![](photos/{photo.parent.name}/{photo.name})" in day


# ---- the Mac's camera, without the robot (2026-10-06) -----------------------------------------------------------
# "Take a photo from my Mac and send it to me" went to start_task twice (10-05 16:58, 10-06 13:02): take_photo said
# "the robot is not connected", so Codex's Computer Use took it in about 70 s and it came back as a file.

def _mac(monkeypatch, result) -> list:
    calls: list = []

    async def snap(*a, **k):
        calls.append(1)
        return result
    monkeypatch.setattr(mac_camera, "snap", snap)
    return calls


def test_without_the_robot_the_macs_camera_takes_the_photo(tmp_path, monkeypatch) -> None:
    calls = _mac(monkeypatch, {"ok": True, "jpeg": JPEG})
    d = _daemon(tmp_path, connected=False)
    r = asyncio.run(d._photo_for_owner("my desk"))
    assert calls == [1] and r["ok"] and r["caption"] == "my desk"
    photo = Path(r["path"])
    assert photo.read_bytes() == JPEG and photo.parent.parent.name == "photos"
    assert d._notes.kept == []                          # not the robot's diary: nothing was posed by a head
    assert not list((tmp_path / "notes").glob("*.md"))


def test_the_robot_still_takes_it_when_it_is_connected(tmp_path, monkeypatch) -> None:
    calls = _mac(monkeypatch, {"ok": True, "jpeg": JPEG})
    r = asyncio.run(_daemon(tmp_path, snap=_frame())._photo_for_owner("my bike"))
    assert r["ok"] and calls == []


def test_a_mac_camera_that_gives_nothing_is_its_reason(tmp_path, monkeypatch) -> None:
    _mac(monkeypatch, {"ok": False, "reason": "the Mac's camera did not answer in time"})
    r = asyncio.run(_daemon(tmp_path, connected=False)._photo_for_owner(""))
    assert r == {"ok": False, "reason": "the Mac's camera did not answer in time"}


def test_the_camera_command_reads_one_settled_frame_without_a_microphone(monkeypatch) -> None:
    cmd = mac_camera.command("/x/ffmpeg", "default")
    assert cmd[0] == "/x/ffmpeg" and cmd[cmd.index("-i") + 1] == "default:none"
    assert f"gte(n\\,{mac_camera.WARMUP_FRAMES})" in cmd[cmd.index("-vf") + 1]
    assert cmd[cmd.index("-frames:v") + 1] == "1" and cmd[-1] == "pipe:1"
    assert mac_camera.device({}) == "default" and mac_camera.device({"EVE_MAC_CAMERA": "2"}) == "2"
    assert mac_camera.device({"EVE_MAC_CAMERA": "off"}) is None


def test_a_switched_off_camera_never_starts_ffmpeg(monkeypatch) -> None:
    started: list = []

    async def boom(*a, **k):
        started.append(a)
        raise AssertionError("ffmpeg started")
    monkeypatch.setattr(mac_camera.asyncio, "create_subprocess_exec", boom)
    r = asyncio.run(mac_camera.snap({"EVE_MAC_CAMERA": "off"}))
    assert r["ok"] is False and "switched off" in r["reason"] and started == []
