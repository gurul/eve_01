"""EVE_SERIAL_SKIP: another board on its charging cable is never opened as the robot (2026-09-30)."""
from __future__ import annotations

from typing import Any

import pytest

from eve_bridge import serial_transport


class _Port:
    def __init__(self, device: str, vid: Any, serial_number: Any = None) -> None:
        self.device, self.vid, self.serial_number = device, vid, serial_number


def _ports(monkeypatch: pytest.MonkeyPatch, ports: list[_Port]) -> None:
    from serial.tools import list_ports
    monkeypatch.setattr(serial_transport.glob, "glob", lambda pattern: [p.device for p in ports])
    monkeypatch.setattr(list_ports, "comports", lambda: ports)


CONTROLLER = _Port("/dev/cu.usbmodem101", 0x303A, "0A:00:00:00:00:02")
STICK = _Port("/dev/cu.usbmodem31201", 0x303A, "0A:00:00:00:00:03")
ROBOT = _Port("/dev/cu.usbmodem4101", 0x303A, "0A:00:00:00:00:01")
CAMERA = _Port("/dev/cu.usbmodem1105", 0x3564)


def test_the_setting_is_a_comma_list_of_serials_any_case() -> None:
    assert serial_transport.serial_skip({}) == frozenset()
    assert serial_transport.serial_skip({"EVE_SERIAL_SKIP": ""}) == frozenset()
    assert serial_transport.serial_skip({"EVE_SERIAL_SKIP": " 0A:00:00:00:00:03 , ,0a:00 "}) == \
        frozenset({"0a:00:00:00:00:03", "0a:00"})


def test_with_the_robot_unplugged_the_stick_is_not_opened(monkeypatch: pytest.MonkeyPatch) -> None:
    """What happened: the controller skipped, the stick the only other Espressif node, so it became the robot."""
    _ports(monkeypatch, [CAMERA, CONTROLLER, STICK])
    ctl_only = frozenset({"0a:00:00:00:00:02"})
    assert serial_transport._resolve_port("/dev/cu.usbmodem*", ctl_only) == STICK.device      # the bug (control)
    skip = ctl_only | serial_transport.serial_skip({"EVE_SERIAL_SKIP": "0A:00:00:00:00:03"})
    assert serial_transport._resolve_port("/dev/cu.usbmodem*", skip) is None                   # waits instead


def test_with_the_robot_plugged_in_it_is_still_found(monkeypatch: pytest.MonkeyPatch) -> None:
    _ports(monkeypatch, [CAMERA, CONTROLLER, STICK, ROBOT])
    skip = frozenset({"0a:00:00:00:00:02", "0a:00:00:00:00:03"})
    assert serial_transport._resolve_port("/dev/cu.usbmodem*", skip) == ROBOT.device


def test_the_daemon_joins_the_skip_list_with_the_controller(monkeypatch: pytest.MonkeyPatch) -> None:
    captured: dict[str, Any] = {}

    class FakeSerial:
        def __init__(self, **kw: Any) -> None:
            captured.update(kw)
            self.on_boot = None

    monkeypatch.setenv("EVE_SERIAL_SKIP", "0A:00:00:00:00:03")
    monkeypatch.setenv("EVE_CONTROLLER_SERIAL", "0A:00:00:00:00:02")
    monkeypatch.setattr(serial_transport, "EveSerial", FakeSerial)
    from eve_bridge import controller, daemon
    monkeypatch.setattr(controller, "Controller", lambda *a, **k: object())
    try:
        daemon.Daemon(serial_port="/dev/cu.usbmodem*")
    except Exception:  # noqa: BLE001 - only the construction of the robot link matters here
        pass
    assert captured.get("skip_serials") == frozenset({"0a:00:00:00:00:03", "0a:00:00:00:00:02"})
