"""obsbot_usb.py: building the helper, reading its JSON, asleep by the parked tilt, waking, the turn's directions
(the camera's relative "+1" pans the other way, measured 2026-10-11), and finding an OBSBOT on the USB bus."""

from __future__ import annotations

import json
import subprocess
from pathlib import Path
from typing import Any

import pytest

from eve_bridge import obsbot_usb
from eve_bridge.obsbot_usb import UsbCamera


class Helper:
    """subprocess.run for the helper: records argv, answers with the JSON a Tiny 3 gave."""

    def __init__(self, tilt: float = 0.0, error: str = ""):
        self.argv: list[list[str]] = []
        self.tilt, self.error = tilt, error

    def __call__(self, argv: list[str], **kw: Any) -> subprocess.CompletedProcess:
        self.argv.append(argv[1:])
        if self.error:
            out = {"ok": False, "error": self.error}
        elif argv[1] == "get":
            out = {"ok": True, "name": "OBSBOT Tiny 3", "pan": 0.0, "tilt": self.tilt, "zoom": 0, "zoom_max": 100}
        else:
            out = {"ok": True}
        return subprocess.CompletedProcess(argv, 0, json.dumps(out) + "\n", "")


@pytest.fixture(autouse=True)
def quick(monkeypatch):
    monkeypatch.setattr(obsbot_usb.time, "sleep", lambda s: None)


def test_read_and_asleep_by_the_parked_tilt():
    awake = UsbCamera(runner=Helper(tilt=3.0), helper=Path("/x"))
    assert awake.read()["asleep"] is False
    asleep = UsbCamera(runner=Helper(tilt=-79.0), helper=Path("/x"))
    assert asleep.read()["asleep"] is True


def test_no_camera_is_a_plain_line():
    cam = UsbCamera(runner=Helper(error="no camera"), helper=Path("/x"))
    assert cam.read() is None and cam.why == obsbot_usb.NO_CAMERA_LINE


def test_turn_directions_and_speed():
    h = Helper()
    cam = UsbCamera(runner=h, helper=Path("/x"))
    for d in ("left", "right", "up", "down", "stop"):
        assert cam.turn(d)
    speed = str(obsbot_usb.TURN_SPEED)
    assert h.argv == [["turn", "-1", "0", speed], ["turn", "1", "0", speed], ["turn", "0", "1", speed],
                      ["turn", "0", "-1", speed], ["turn", "0", "0", speed]]


def test_point_and_zoom_arguments():
    h = Helper()
    cam = UsbCamera(runner=h, helper=Path("/x"))
    assert cam.point(20, -10.5) and cam.zoom(40)
    assert h.argv == [["pantilt", "20.00", "-10.50"], ["zoom", "40"]]


def test_wake_opens_the_video_only_when_asleep():
    opened: list[int] = []
    h = Helper(tilt=-79.0)

    def waker() -> None:
        opened.append(1)
        h.tilt = 0.0                                  # the gimbal unparks once the video opens

    asleep = UsbCamera(runner=h, waker=waker, helper=Path("/x"))
    assert asleep.wake() and opened == [1]
    awake = UsbCamera(runner=Helper(tilt=0.0), waker=lambda: opened.append(2), helper=Path("/x"))
    assert awake.wake() and opened == [1]


def test_wake_gives_up_when_the_camera_stays_parked(monkeypatch):
    clock = iter(range(0, 100))
    monkeypatch.setattr(obsbot_usb.time, "monotonic", lambda: next(clock))
    cam = UsbCamera(runner=Helper(tilt=-79.0), waker=lambda: None, helper=Path("/x"))
    assert cam.wake() is False


def test_build_compiles_once_per_source(tmp_path, monkeypatch):
    monkeypatch.setattr(obsbot_usb, "BIN_DIR", tmp_path)
    calls: list[list[str]] = []

    def clang(argv: list[str], **kw: Any) -> subprocess.CompletedProcess:
        calls.append(argv)
        Path(argv[argv.index("-o") + 1]).write_text("binary")
        return subprocess.CompletedProcess(argv, 0, "", "")

    (tmp_path / "obsbot-uvc-old").write_text("stale")
    path, why = obsbot_usb.build(clang)
    assert why == "" and path == obsbot_usb.helper_path() and path.exists()
    assert not (tmp_path / "obsbot-uvc-old").exists()                       # an older build is cleared
    assert obsbot_usb.build(clang)[0] == path and len(calls) == 1             # built once


def test_build_failure_is_a_reason(tmp_path, monkeypatch):
    monkeypatch.setattr(obsbot_usb, "BIN_DIR", tmp_path)

    def broken(argv: list[str], **kw: Any) -> subprocess.CompletedProcess:
        return subprocess.CompletedProcess(argv, 1, "", "error: no IOKit")

    path, why = obsbot_usb.build(broken)
    assert path is None and "clang failed" in why


def test_present_reads_the_usb_bus():
    def ioreg(found: bool):
        return lambda argv, **kw: subprocess.CompletedProcess(
            argv, 0, '"idVendor" = 13668\n' if found else '"idVendor" = 1452\n', "")

    assert obsbot_usb.present(ioreg(True)) is True
    assert obsbot_usb.present(ioreg(False)) is False


def test_the_helper_source_builds_with_this_macs_clang(tmp_path):
    """The real helper compiles (it is C against IOKit): a broken edit fails here, not on the owner's first move."""
    import shutil

    clang = shutil.which("clang")
    if clang is None:
        pytest.skip("no clang on this machine")
    out = subprocess.run([clang, "-O2", "-Wall", "-Werror", "-framework", "IOKit", "-framework", "CoreFoundation",
                          "-o", str(tmp_path / "h"), str(obsbot_usb.SOURCE)], capture_output=True, text=True)
    assert out.returncode == 0, out.stderr
    usage = subprocess.run([str(tmp_path / "h")], capture_output=True, text=True)
    assert json.loads(usage.stdout)["ok"] is False                           # no arguments: usage, never a crash
