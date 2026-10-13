"""Serial transport — receive-silence watchdog.

Regression cover for a link that dies without raising. When the USB device
re-enumerates (every board reset, including esptool's), the host keeps a file
descriptor that no longer reaches the board: writes succeed, reads return empty,
nothing raises, and the daemon reports itself healthy forever while the board
shows "No Claude connected". Only a read-silence timeout catches that.

Async tests follow the repo convention (see test_ble_radio_reset): an explicit
_run() around asyncio.run(), rather than a pytest-asyncio dependency.
"""

from __future__ import annotations

import asyncio
import types
from typing import Any, Coroutine, TypeVar

import pytest

from eve_bridge import serial_transport
from eve_bridge.serial_transport import EveSerial

_T = TypeVar("_T")


def _run(coro: Coroutine[Any, Any, _T]) -> _T:
    return asyncio.run(coro)


class FakeSerial:
    """Minimal pyserial stand-in.

    ``lines`` are returned one per read() call (the transport's _read_chunk
    asks for one byte and then whatever is waiting; here a whole line comes
    back at once and ``in_waiting`` stays 0). Once exhausted every subsequent
    call returns b"" — exactly what a stale handle looks like, since pyserial
    returns empty on its read timeout rather than raising.
    """

    def __init__(self, lines: list[bytes]) -> None:
        self._lines = list(lines)
        self.is_open = True
        self.dtr = False
        self.rts = False
        self.closed = False
        self.cancelled = False
        self.input_resets = 0
        self.written: list[bytes] = []

    in_waiting = 0

    def read(self, _n: int = 1) -> bytes:
        return self._lines.pop(0) if self._lines else b""

    def write(self, data: bytes) -> None:
        self.written.append(data)

    def cancel_read(self) -> None:
        self.cancelled = True

    def reset_input_buffer(self) -> None:
        self.input_resets += 1

    def close(self) -> None:
        self.closed = True
        self.is_open = False


@pytest.fixture
def fast_watchdog(monkeypatch: pytest.MonkeyPatch) -> None:
    """Shrink the timeouts so these run in milliseconds, not half a minute."""
    monkeypatch.setattr(serial_transport, "RX_SILENCE_SECS", 0.05)
    monkeypatch.setattr(serial_transport, "RECONNECT_SECS", 0.01)


async def _noop(obj: dict) -> None:
    pass


async def _drive(bs: EveSerial, seconds: float) -> None:
    """Run the transport loop for a bounded time, then shut it down cleanly."""
    task = asyncio.create_task(bs.run())
    await asyncio.sleep(seconds)
    await bs.stop()
    task.cancel()
    await asyncio.gather(task, return_exceptions=True)


def _patch_port(monkeypatch: pytest.MonkeyPatch, opener) -> None:
    monkeypatch.setattr(serial_transport.serial, "Serial", opener)
    monkeypatch.setattr(serial_transport, "_resolve_port", lambda p, skip=frozenset(): "/dev/fake")


def test_silence_triggers_reconnect(fast_watchdog: None, monkeypatch: pytest.MonkeyPatch) -> None:
    """A port that goes quiet is reopened rather than held forever."""
    opened: list[FakeSerial] = []

    def fake_open(port: str, baud: int, timeout: int = 1, **kw: object) -> FakeSerial:
        s = FakeSerial([b"[alive] up=1s\n"])  # one line, then permanent silence
        opened.append(s)
        return s

    _patch_port(monkeypatch, fake_open)

    bs = EveSerial(on_message=_noop, port="/dev/fake")
    _run(_drive(bs, 0.4))

    # Without the watchdog this stays at 1 forever.
    assert len(opened) > 1, "stale handle was never dropped"
    assert opened[0].closed, "the dead handle should be closed on teardown"


def test_traffic_keeps_link_open(fast_watchdog: None, monkeypatch: pytest.MonkeyPatch) -> None:
    """A chatty board is never reconnected — the watchdog must not flap."""
    opened: list[FakeSerial] = []

    def fake_open(port: str, baud: int, timeout: int = 1, **kw: object) -> FakeSerial:
        # Far more lines than the test will consume: the board keeps talking.
        s = FakeSerial([b"[alive] up=1s\n"] * 100_000)
        opened.append(s)
        return s

    _patch_port(monkeypatch, fake_open)

    bs = EveSerial(on_message=_noop, port="/dev/fake")
    _run(_drive(bs, 0.4))

    assert len(opened) == 1, "a live link was needlessly reconnected"


def test_json_lines_reach_the_handler(fast_watchdog: None, monkeypatch: pytest.MonkeyPatch) -> None:
    """Non-JSON boot chatter is skipped; JSON is parsed and dispatched."""
    seen: list[dict] = []

    async def capture(obj: dict) -> None:
        seen.append(obj)

    def fake_open(port: str, baud: int, timeout: int = 1, **kw: object) -> FakeSerial:
        return FakeSerial([
            b"[alive] up=1s\n",
            b'{"cmd":"permission","id":"abc","decision":"once"}\n',
        ])

    _patch_port(monkeypatch, fake_open)

    bs = EveSerial(on_message=capture, port="/dev/fake")
    _run(_drive(bs, 0.2))

    assert {"cmd": "permission", "id": "abc", "decision": "once"} in seen


def test_force_reconnect_defers_close_to_reader(fast_watchdog: None, monkeypatch: pytest.MonkeyPatch) -> None:
    """force_reconnect never closes the fd itself — it flags the reader,
    which closes and reopens. (Closing under the reader's blocked readline
    raced: paired EBADF after every forced reconnect, fd-reuse hazard.)"""
    monkeypatch.setattr(serial_transport, "FORCED_HOLDOFF_SECS", 0.01)
    opened: list[FakeSerial] = []

    def fake_open(port: str, baud: int, timeout: int = 1, **kw: object) -> FakeSerial:
        s = FakeSerial([b"[alive] up=1s\n"] * 100_000)
        opened.append(s)
        return s

    _patch_port(monkeypatch, fake_open)
    bs = EveSerial(on_message=_noop, port="/dev/fake")

    async def scenario() -> None:
        task = asyncio.create_task(bs.run())
        await asyncio.sleep(0.05)
        first = opened[0]
        bs.force_reconnect("test wedge")
        assert not first.closed, "event-loop side must not close the fd"
        assert first.cancelled, "reader must be kicked out of readline"
        await asyncio.sleep(0.2)
        assert first.closed, "reader should close the condemned handle"
        assert len(opened) > 1, "port should reopen after the holdoff"
        await bs.stop()
        task.cancel()
        await asyncio.gather(task, return_exceptions=True)

    _run(scenario())


def test_send_failure_does_not_condemn_fresh_handle(monkeypatch: pytest.MonkeyPatch) -> None:
    """A write that fails after a reconnect must not tear down the NEW handle."""
    stale = FakeSerial([])

    def bad_write(data: bytes) -> None:
        raise RuntimeError("late failure")

    stale.write = bad_write  # type: ignore[method-assign]

    bs = EveSerial(on_message=_noop, port="/dev/fake")
    bs._ser = stale

    async def scenario() -> None:
        ok = await bs.send({"a": 1})
        assert not ok
        assert bs._drop_reason is not None  # stale handle condemned
        # Simulate the reader reopening:
        fresh = FakeSerial([])
        bs._ser = fresh
        bs._drop_reason = None
        assert bs.connected

    _run(scenario())


def test_repeated_silence_arms_rts_pulse(fast_watchdog: None, monkeypatch: pytest.MonkeyPatch) -> None:
    """Enumerated-but-silent reopens of the same port arm and consume the RTS
    pulse instead of looping reopens forever."""
    opened: list[FakeSerial] = []

    def fake_open(port: str, baud: int, timeout: int = 1, **kw: object) -> FakeSerial:
        s = FakeSerial([])  # born silent
        opened.append(s)
        return s

    _patch_port(monkeypatch, fake_open)
    bs = EveSerial(on_message=_noop, port="/dev/fake")
    _run(_drive(bs, 0.8))

    assert len(opened) >= serial_transport.RX_TRIP_PULSE_COUNT + 1
    assert not bs._pulse_on_open  # armed then consumed, not left dangling


def _deaf_reopens(err: Exception, count: int) -> EveSerial:
    """``count`` fresh handles whose first write raises ``err``, as the reader's reopen loop would hand them over."""
    bs = EveSerial(on_message=_noop, port="/dev/fake")

    def bad_write(data: bytes) -> None:
        raise err

    async def scenario() -> None:
        for _ in range(count):
            fresh = FakeSerial([])
            fresh.port = "/dev/cu.usbmodem101"  # type: ignore[attr-defined]
            fresh.write = bad_write  # type: ignore[method-assign]
            bs._ser, bs._drop_reason = fresh, None
            assert not await bs.send({"time": 1})

    _run(scenario())
    return bs


def test_repeated_write_timeouts_arm_rts_pulse() -> None:
    """A board that stops draining its OUT endpoint (the Voice PE, 2026-10-01) gets the RTS reset
    too, instead of a reopen every 5 s forever: a write timeout counts as a trip."""
    import serial
    bs = _deaf_reopens(serial.SerialTimeoutException("Write timeout"), serial_transport.RX_TRIP_PULSE_COUNT)
    assert bs._pulse_on_open


def test_other_write_failures_do_not_arm_rts_pulse() -> None:
    """Control: only a timeout is the deaf board; a write that raises anything else is not counted."""
    bs = _deaf_reopens(RuntimeError("late failure"), serial_transport.RX_TRIP_PULSE_COUNT)
    assert not bs._pulse_on_open


# ---- port choice: the board, or nothing ------------------------------------------------------

class _Port:
    def __init__(self, device: str, vid: Any, serial_number: Any = None) -> None:
        self.device, self.vid, self.serial_number = device, vid, serial_number


def _ports(monkeypatch: pytest.MonkeyPatch, ports: list[_Port]) -> None:
    from serial.tools import list_ports
    monkeypatch.setattr(serial_transport.glob, "glob", lambda pattern: [p.device for p in ports])
    monkeypatch.setattr(list_ports, "comports", lambda: ports)


BOARD = _Port("/dev/cu.usbmodem101", 0x303A)
CAMERA = _Port("/dev/cu.usbmodem1105", 0x3564)
DOCK = _Port("/dev/cu.usbmodemSN1", 0x291A)


def test_the_board_is_chosen_among_other_usb_devices(monkeypatch: pytest.MonkeyPatch) -> None:
    _ports(monkeypatch, [CAMERA, BOARD, DOCK])
    assert serial_transport._resolve_port("/dev/cu.usbmodem*") == BOARD.device


ROBOT = _Port("/dev/cu.usbmodem31201", 0x303A, "0A:00:00:00:00:01")
CONTROLLER = _Port("/dev/cu.usbmodem101", 0x303A, "0A:00:00:00:00:02")


def test_two_esp32_boards_are_told_apart_by_usb_serial(monkeypatch: pytest.MonkeyPatch) -> None:
    """The StackChan and the Voice PE controller are both ESP32-S3s (2026-09-26): the robot's glob skips the
    controller's serial, and the controller is named by its serial, in either case."""
    _ports(monkeypatch, [CAMERA, CONTROLLER, ROBOT, DOCK])
    skip = frozenset({"0a:00:00:00:00:02"})
    assert serial_transport._resolve_port("/dev/cu.usbmodem*") == CONTROLLER.device   # sorted first
    assert serial_transport._resolve_port("/dev/cu.usbmodem*", skip) == ROBOT.device
    assert serial_transport._resolve_port("usbsn:0A:00:00:00:00:02") == CONTROLLER.device
    assert serial_transport._resolve_port("usbsn:0a:00:00:00:00:01") == ROBOT.device


def test_with_only_the_controller_on_the_bus_the_robot_waits(monkeypatch: pytest.MonkeyPatch) -> None:
    _ports(monkeypatch, [CAMERA, CONTROLLER])
    assert serial_transport._resolve_port("/dev/cu.usbmodem*", frozenset({"0A:00:00:00:00:02"})) is None
    assert serial_transport._resolve_port("usbsn:00:00:00:00:00:00") is None


def test_with_the_board_off_the_bus_a_camera_is_never_opened(monkeypatch: pytest.MonkeyPatch) -> None:
    """The robot power-cycling left a webcam and a dock matching the glob (2026-09-23); the daemon opened
    the webcam. Every match has a vendor id and none is Espressif: wait for the board."""
    _ports(monkeypatch, [CAMERA, DOCK])
    assert serial_transport._resolve_port("/dev/cu.usbmodem*") is None
    _ports(monkeypatch, [CAMERA])
    assert serial_transport._resolve_port("/dev/cu.usbmodem*") is None


def test_without_vendor_ids_the_glob_still_decides(monkeypatch: pytest.MonkeyPatch) -> None:
    """A board behind a hub or a USB-serial chip may report no vendor id: then the glob is all there is."""
    _ports(monkeypatch, [_Port("/dev/cu.usbmodem9", None)])
    assert serial_transport._resolve_port("/dev/cu.usbmodem*") == "/dev/cu.usbmodem9"


def test_an_enumeration_error_falls_back_to_the_glob(monkeypatch: pytest.MonkeyPatch) -> None:
    from serial.tools import list_ports
    monkeypatch.setattr(serial_transport.glob, "glob", lambda pattern: [BOARD.device])

    def boom() -> list[Any]:
        raise OSError("IOKit")
    monkeypatch.setattr(list_ports, "comports", boom)
    assert serial_transport._resolve_port("/dev/cu.usbmodem*") == BOARD.device


def test_port_enumeration_never_blocks_the_event_loop(fast_watchdog: None, monkeypatch: pytest.MonkeyPatch) -> None:
    """With the board off the bus, ``list_ports.comports()`` took 2-3 s on the daemon's own asyncio loop
    (1,543+ stall reports, 2026-09-23/24, last stack serial_transport.py:128) and froze every other door,
    Telegram included. The enumeration runs off the loop: it never runs on the loop's thread, and a slow
    enumerator does not hold back a concurrent loop task."""
    import threading
    import time as _time

    from serial.tools import list_ports

    monkeypatch.setattr(serial_transport.glob, "glob", lambda pattern: [CAMERA.device, DOCK.device])
    threads: list[threading.Thread] = []

    def slow_comports() -> list[_Port]:
        threads.append(threading.current_thread())
        _time.sleep(0.2)
        return [CAMERA, DOCK]                        # the board is off the bus: run() keeps waiting
    monkeypatch.setattr(list_ports, "comports", slow_comports)

    async def main() -> tuple[float, threading.Thread]:
        loop_thread = threading.current_thread()
        bs = EveSerial(on_message=_noop, port="/dev/cu.usbmodem*")
        runner = asyncio.create_task(bs.run())
        worst, last = 0.0, _time.perf_counter()
        for _ in range(40):                          # a concurrent loop task ticking every 10 ms
            await asyncio.sleep(0.01)
            now = _time.perf_counter()
            worst, last = max(worst, now - last), now
        await bs.stop()
        runner.cancel()
        await asyncio.gather(runner, return_exceptions=True)
        return worst, loop_thread

    worst, loop_thread = _run(main())
    assert threads, "the enumerator was never called"
    assert all(t is not loop_thread for t in threads), "comports() ran on the event loop's thread"
    assert worst < 0.15, f"a loop task was held back {worst:.2f} s by the port enumeration"


def test_when_the_usb_list_fails_a_skip_is_never_guessed_past(monkeypatch: pytest.MonkeyPatch) -> None:
    """Eve/PortPick.lean's gap: comports() raising skipped the skip filter and fell back to the glob's first
    node, which can be the controller. With a skip set, the robot waits for the next open instead."""
    from serial.tools import list_ports

    def broken() -> list[Any]:
        raise OSError("IOKit mid-enumeration")

    monkeypatch.setattr(serial_transport.glob, "glob", lambda pattern: [CONTROLLER.device, ROBOT.device])
    monkeypatch.setattr(list_ports, "comports", broken)
    assert serial_transport._resolve_port("/dev/cu.usbmodem*", frozenset({"0A:00:00:00:00:02"})) is None
    assert serial_transport._resolve_port("/dev/cu.usbmodem*") == CONTROLLER.device   # no skip: as before


def test_unplugged_board_warning_backs_off_to_ten_minutes(monkeypatch: pytest.MonkeyPatch,
                                                         caplog: pytest.LogCaptureFixture) -> None:
    """With the board off the bus for hours, the warning comes at most every 10 minutes. The back-off
    compared total time missing with the capped gap, so after 600 s every 3 s pass warned: 70k of the
    log's 106k lines by 2026-10-13."""
    clock = [0.0]
    passes = [0]
    bs = EveSerial(on_message=_noop, port="/dev/fake")

    def no_port(_pattern: str, _skip: object = frozenset()) -> None:
        passes[0] += 1
        clock[0] += 3.0                      # one RECONNECT_SECS pass of real time
        if clock[0] >= 6000:
            bs._stop.set()
        return None

    monkeypatch.setattr(serial_transport, "_resolve_port", no_port)
    # The module's own clock only: asyncio's loop reads time.monotonic too.
    monkeypatch.setattr(serial_transport, "time", types.SimpleNamespace(monotonic=lambda: clock[0]))
    monkeypatch.setattr(serial_transport, "RECONNECT_SECS", 0)
    with caplog.at_level("WARNING", logger=serial_transport.log.name):
        _run(asyncio.wait_for(bs.run(), 10))
    warned = [r.getMessage() for r in caplog.records if "no port matching" in r.getMessage()]
    assert passes[0] >= 1990
    # 30 s, then doubling to the 600 s cap: 30, 90, 210, 450, 930, 1530 … 5730 → 13 warnings.
    assert len(warned) == 13, warned
