"""Replays of the counterexamples in verification/Eve/Serial.lean against the real code.

Each test drives the real class or method with fakes (no port, no board, no daemon):

  A  EveSerial.send: a sender cancelled by wait_for while its write is still on an
     executor thread must keep the lock until that write returns, or the next
     sender's bytes go out alongside it (Serial.lean `aTrace`).
  B  Daemon._status_poller + the status-ack branch of _handle_ble: a link that
     answers the resync status and one poll after each reconnect, then goes deaf,
     must reach pulse_reset (Serial.lean `fieldCycle`).
  C  folder_push._send_expect + Daemon.wait_for_ack: an ack that arrives before the
     sender resumes must not be dropped, and a late chunk ack from an earlier
     request must not satisfy the next chunk's waiter (Serial.lean `C_current_*`).

Async tests use an explicit asyncio.run, the repo convention.
"""

from __future__ import annotations

import asyncio
import threading
from pathlib import Path
from types import MethodType, SimpleNamespace
from typing import Any

import pytest

from eve_bridge import daemon as daemon_mod
from eve_bridge import folder_push
from eve_bridge.daemon import Daemon
from eve_bridge.serial_transport import EveSerial


async def _noop(obj: dict) -> None:
    pass


# ---- A: one write on the port at a time -----------------------------------------------------


class _SlowSerial:
    """A port whose write blocks on an event, counting how many writes are in it at once."""

    def __init__(self) -> None:
        self.is_open = True
        self.release = threading.Event()
        self.entered = threading.Event()
        self._mu = threading.Lock()
        self.inflight = 0
        self.max_inflight = 0
        self.wire: list[bytes] = []

    def write(self, data: bytes) -> int:
        with self._mu:
            self.inflight += 1
            self.max_inflight = max(self.max_inflight, self.inflight)
        self.entered.set()
        try:
            self.release.wait(5.0)
            with self._mu:
                self.wire.append(data)
            return len(data)
        finally:
            with self._mu:
                self.inflight -= 1

    def cancel_read(self) -> None:
        pass


def test_a_cancelled_sender_keeps_the_port_until_its_write_returns() -> None:
    async def go() -> tuple[Any, Any, _SlowSerial]:
        bs = EveSerial(_noop, port="/dev/fake")
        port = _SlowSerial()
        bs._ser = port  # type: ignore[assignment]
        first = asyncio.create_task(asyncio.wait_for(bs.send({"first": 1}), timeout=0.05))
        await asyncio.to_thread(port.entered.wait, 2.0)
        await asyncio.sleep(0.15)            # wait_for has cancelled the first sender by now
        second = asyncio.create_task(bs.send({"second": 2}))
        await asyncio.sleep(0.15)            # give the second sender every chance to write
        port.release.set()
        r1 = (await asyncio.gather(first, return_exceptions=True))[0]
        r2 = await second
        return r1, r2, port

    r1, r2, port = asyncio.run(go())
    assert port.max_inflight == 1, f"{port.max_inflight} writes were on the port at once"
    assert isinstance(r1, (TimeoutError, asyncio.TimeoutError))   # the caller still sees its timeout
    assert r2 is True
    assert [w.split(b":")[0] for w in port.wire] == [b'{"first"', b'{"second"']


# ---- B: the watchdog reaches the RTS pulse on a flapping link -------------------------------


class _FlappyBle:
    """After every reconnect or pulse the board answers the resync's status and ONE
    watchdog poll, then goes deaf (the field pattern of 2026-09-25)."""

    def __init__(self, d: SimpleNamespace) -> None:
        self.d = d
        self.connected = True
        self.answers_left = 1
        self.reconnects = 0
        self.pulses = 0

    async def send(self, obj: dict, codec: Any = None) -> bool:
        if obj.get("cmd") == "status" and self.answers_left > 0:
            self.answers_left -= 1
            await Daemon._handle_ble(self.d, {"ack": "status", "ok": True, "n": 0, "data": {}})
        return True

    def _reconnected(self) -> None:
        self.answers_left = 1
        # _send_resync's {"cmd":"status"}: answered on every fresh link.
        asyncio.get_running_loop().create_task(
            Daemon._handle_ble(self.d, {"ack": "status", "ok": True, "n": 0, "data": {}}))

    def force_reconnect(self, why: str) -> None:
        self.reconnects += 1
        self._reconnected()

    def pulse_reset(self, why: str) -> None:
        self.pulses += 1
        self._reconnected()


class _TickingAsyncio:
    """Stands in for the asyncio module inside daemon.py so each POLL_INTERVAL passes at once."""

    def __init__(self, ticks: int) -> None:
        self._ticks = ticks
        self.TimeoutError = asyncio.TimeoutError

    async def wait_for(self, aw: Any, timeout: float) -> None:
        if hasattr(aw, "close"):
            aw.close()
        await asyncio.sleep(0)             # let the resync ack task run, as a real minute would
        await asyncio.sleep(0)
        if self._ticks <= 0:
            return None                    # "shutdown": the poller returns
        self._ticks -= 1
        raise asyncio.TimeoutError

    def __getattr__(self, name: str) -> Any:
        return getattr(asyncio, name)


def _watchdog_stub() -> SimpleNamespace:
    d = SimpleNamespace(
        _shutdown=asyncio.Event(),
        _status_sent_at=None,
        _status_missed=0,
        _clean_polls=0,
        _ack_escalation=0,
        _last_stick_sec=None,
        _last_stick_battery_pct=None,
        _ack_waiters=[],
    )
    d.ble = _FlappyBle(d)
    return d


def test_b_flapping_link_reaches_the_rts_pulse(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(daemon_mod, "asyncio", _TickingAsyncio(ticks=40))

    async def go() -> SimpleNamespace:
        d = _watchdog_stub()
        await Daemon._status_poller(d)
        return d

    d = asyncio.run(go())
    assert d.ble.reconnects >= 2, "the flap should have escalated more than once"
    assert d.ble.pulses >= 1, (
        f"{d.ble.reconnects} reconnects and no RTS pulse: every flap reset the escalation")
    # Serial.lean B_fixed_flap_pulses: every second escalation pulses.
    assert d.ble.pulses >= (d.ble.reconnects + d.ble.pulses) // 2


def test_b_healthy_link_after_one_reconnect_de_escalates(monkeypatch: pytest.MonkeyPatch) -> None:
    """The fix must not pulse a link that recovers: two answered polls in a row reset it."""
    monkeypatch.setattr(daemon_mod, "asyncio", _TickingAsyncio(ticks=12))

    async def go() -> SimpleNamespace:
        d = _watchdog_stub()
        d.ble.answers_left = 0                   # deaf until the first reconnect ...
        orig = d.ble._reconnected

        def recover() -> None:
            orig()
            d.ble.answers_left = 10 ** 6         # ... then healthy for good
        d.ble._reconnected = recover
        await Daemon._status_poller(d)
        return d

    d = asyncio.run(go())
    assert d.ble.reconnects == 1
    assert d.ble.pulses == 0
    assert d._ack_escalation == 0


# ---- C: acks are matched to their own request, and never lost ------------------------------


class _Board:
    """Answers folder-push commands like firmware xfer.h: chunk acks carry n, the
    file's cumulative byte count. ``sync`` delivers the ack inside send(), before the
    sender resumes; otherwise it is delivered a moment later."""

    def __init__(self, d: SimpleNamespace, *, sync: bool) -> None:
        self.d = d
        self.sync = sync
        self.written = 0
        self.queue_first: list[dict] = []     # acks to deliver before the next real one
        self.silent_chunks = 0                 # chunk commands to leave unanswered

    async def _deliver(self, ack: dict) -> None:
        if not self.sync:
            await asyncio.sleep(0.01)
        for extra in self.queue_first:
            await Daemon._handle_ble(self.d, extra)
        self.queue_first = []
        await Daemon._handle_ble(self.d, ack)

    async def send(self, obj: dict, codec: Any = None) -> bool:
        cmd = obj.get("cmd")
        if cmd == "file":
            self.written = 0
        n = 0
        if cmd == "chunk":
            self.written += len(folder_push.base64.b64decode(obj["d"]))
            n = self.written
            if self.silent_chunks > 0:
                self.silent_chunks -= 1
                return True
        ack = {"ack": cmd, "ok": True, "n": n}
        if self.sync:
            await self._deliver(ack)
        else:
            asyncio.get_running_loop().create_task(self._deliver(ack))
        return True


def _ack_stub(sync: bool) -> SimpleNamespace:
    d = SimpleNamespace(
        _status_sent_at=None, _status_missed=0, _clean_polls=0, _ack_escalation=0,
        _ack_waiters=[], matched=[],
    )
    for name in ("wait_for_ack", "expect_ack"):
        if hasattr(Daemon, name):
            setattr(d, name, MethodType(getattr(Daemon, name), d))
    real_wait = d.wait_for_ack

    async def spy(*a: Any, **kw: Any) -> dict:
        ack = await real_wait(*a, **kw)
        d.matched.append(ack)
        return ack
    d.wait_for_ack = spy
    d.ble = _Board(d, sync=sync)
    return d


def _pack(base: Path, size: int) -> Path:
    folder = base / "pack"
    folder.mkdir(parents=True)
    (folder / "a.gif").write_bytes(bytes(range(256)) * (size // 256) + bytes(size % 256))
    return folder


@pytest.fixture
def fast_acks(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(folder_push, "ACK_TIMEOUT_FAST", 0.3)
    monkeypatch.setattr(folder_push, "ACK_TIMEOUT_SLOW", 0.3)


def test_c_ack_before_the_sender_resumes_is_not_lost(tmp_path: Path, fast_acks: None) -> None:
    folder = _pack(tmp_path, 300)

    async def go() -> dict:
        return await folder_push.push_character(_ack_stub(sync=True), str(folder))

    out = asyncio.run(go())
    assert out["files"] == 1 and out["total_bytes"] == 300


def test_c_late_chunk_ack_does_not_satisfy_the_next_chunk(tmp_path: Path, fast_acks: None) -> None:
    first = _pack(tmp_path / "one", 300)
    second = _pack(tmp_path / "two", 300)

    async def go() -> list[int]:
        d = _ack_stub(sync=False)
        d.ble.silent_chunks = 1                      # push 1: the first chunk's ack never comes
        with pytest.raises((TimeoutError, asyncio.TimeoutError)):
            await folder_push.push_character(d, str(first))
        # Push 2: a late chunk ack from an earlier request turns up just before push 2's
        # own first chunk ack. Its n (300) cannot answer this chunk (n=200). A late ack
        # whose n happens to equal the expected one is indistinguishable on the wire.
        d.matched.clear()
        orig_send = d.ble.send

        async def send(obj: dict, codec: Any = None) -> bool:
            if obj.get("cmd") == "chunk" and not d.matched_chunk_sent:
                d.matched_chunk_sent = True
                d.ble.queue_first = [{"ack": "chunk", "ok": True, "n": 300}]
            return await orig_send(obj, codec)
        d.matched_chunk_sent = False
        d.ble.send = send
        await folder_push.push_character(d, str(second))
        return [a["n"] for a in d.matched if a.get("ack") == "chunk"]

    chunk_ns = asyncio.run(go())
    assert chunk_ns == [200, 300], f"chunk waiters received acks n={chunk_ns}"
