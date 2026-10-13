"""USB CDC serial transport — drop-in replacement for EveBLE.

The claude-desktop-buddy firmware feeds the same newline-delimited JSON
parser from USB serial as from BLE NUS (see firmware data.h), so the only
difference is the pipe. On the ESP32-S3's native USB-Serial/JTAG port,
opening the port does NOT reset the sketch, but the host must assert DTR
or the firmware's HWCDC TX is silently dropped.

Duck-types EveBLE's surface used by Daemon: ``connected``,
``wait_connected()``, ``send(obj, codec=None)``, ``run()``, ``stop()``.

Threading model: run()'s reader thread is the SOLE owner of the port
handle's close. Event-loop-side paths (force_reconnect, pulse_reset,
send-failure, stop) never close the fd — closing it under the reader's
blocked read() raced (paired EBADF on every forced reconnect, and an
fd-reuse hazard). They set a drop reason and cancel_read() instead; the
reader notices, breaks, and closes in its finally.

Nothing that can block runs on the event loop: reads, writes and the port
enumeration (``_resolve_port``, whose ``list_ports.comports()`` takes 2-3 s
while the board is off the bus) all run on worker threads, because the loop
is shared with every other door (Telegram, the voice, the web page).
"""

from __future__ import annotations

import asyncio
import glob
import json
import logging
import os
import re
import time
from typing import Any, Awaitable, Callable, Mapping, Optional

import serial  # pyserial

from .protocol import encode

log = logging.getLogger(__name__)

IncomingHandler = Callable[[dict[str, Any]], Awaitable[None]]

RECONNECT_SECS = 3.0
# After an ack-loss forced reconnect, hold the port CLOSED this long before
# reopening. Field evidence: the only things that ever revived the deaf-board
# wedge short of a reflash were daemon restarts — i.e. tens of seconds of
# port-closed time — while 3s in-process reopens never did.
FORCED_HOLDOFF_SECS = 15.0
# A board that is enumerated-but-silent (fresh opens, zero bytes) this many
# times within the window gets an RTS hardware reset on the next open. The
# silent-board crash loops of 2026-08-05 ran 20+ minutes of reopen loops that
# could never help; the RTS pulse reboots the chip and ends it in ~3 minutes.
RX_TRIP_PULSE_COUNT = 3
RX_TRIP_WINDOW_SECS = 600.0
# ESP32-S3 native USB-Serial/JTAG. (TinyUSB CDC builds present 303a:0002 —
# this firmware uses the built-in peripheral.)
_ESP32S3_VID = 0x303A

# Board output worth surfacing at WARNING: ESP-IDF error lines, panic dumps,
# watchdog trips, the diag ring's died-in-phase line, and the ROM banner that
# means it just reset underneath us.
_IS_CRASH = re.compile(
    r"watchdog|Backtrace|Guru Meditation|abort\(\)|assert failed|DIED IN|"
    r"StoreProhibited|LoadProhibited|rst:0x|E \(\d+\)|CORRUPT HEAP|stack overflow",
    re.IGNORECASE,
)

# The board rebooted underneath an unbroken link. A CH340-bridged board reset
# does NOT re-enumerate USB, so no reconnect fires and the on-connect resync
# (time + forced heartbeat) never runs — the reborn board sits at "--:--" and
# "No Claude connected" until state happens to change. The ROM banner and the
# firmware's own boot line are the tell; the daemon hooks them via `on_boot`.
_IS_BOOT = re.compile(r"rst:0x|^\[boot\] ")

# What the board says when a host motion is admitted, reduced, refused or stopped
# (body.cpp / main.cpp). Promoted to INFO: see _on_line.
_IS_MOTION = re.compile(r"^\[body\] motion|^\[gaze\] host look")

# Treat the link as dead after this long with no bytes read at all.
#
# A USB re-enumeration — which every board reset causes, including the one
# esptool triggers on flash — leaves the host holding a file descriptor that no
# longer reaches the device. Writes succeed into the void, reads return empty,
# and nothing ever raises, so `connected` stays True forever: the daemon looks
# healthy, the log is clean, and the board sits there showing "No Claude
# connected". Only an exception used to trigger reconnect, and a stale fd never
# produces one.
#
# The firmware prints "[alive] ..." every 5s unconditionally WHILE loop() runs
# (main.cpp) — so silence this long means either a stale fd or a hung/crashed
# board (the USB peripheral is autonomous silicon and keeps enumerating while
# the CPU is stuck). Keep in sync with the firmware interval.
RX_SILENCE_SECS = 20.0

# Longest NDJSON line we will assemble before declaring the stream garbage.
# Camera frames are the big ones: 160x120 gray is 19.2 KB raw → 25.6 KB of
# base64 in one line; JPEG frames are a few KB. 256 KB leaves room for
# larger frame sizes without letting a newline-less firmware spew grow
# the buffer without bound.
MAX_LINE_BYTES = 256 * 1024


def _read_chunk(ser: serial.Serial) -> bytes:
    """Reader thread. One blocking read of whatever the board has sent.

    Not ``ser.readline()``: pyserial inherits io.IOBase's readline, which
    pulls ONE byte per read() — a select() plus a syscall each — so a 25 KB
    camera-frame line cost tens of thousands of round trips and the reader
    fell behind at 5 fps. Block up to the port timeout for the first byte,
    then drain what is already waiting. Returns b"" on the timeout (or a
    cancel_read), exactly like readline did.
    """
    first = ser.read(1)
    if not first:
        return b""
    waiting = ser.in_waiting
    return first + ser.read(waiting) if waiting else first


USB_SERIAL_PREFIX = "usbsn:"


def serial_skip(environ: Optional[Mapping[str, str]] = None) -> frozenset[str]:
    """``EVE_SERIAL_SKIP``: USB serial numbers, comma-separated, that the robot's glob never opens.

    Other ESP32-S3 boards share the robot's ``/dev/cu.usbmodem*`` glob. On 2026-09-30, with the StackChan
    unplugged, the daemon opened another board on its charging cable as the robot, sent it status polls and
    RTS-reset it every two minutes. A board named here is left alone."""
    env = os.environ if environ is None else environ
    return frozenset(s.strip().lower() for s in (env.get("EVE_SERIAL_SKIP") or "").split(",") if s.strip())


def _resolve_port(pattern: str, skip_serials: frozenset[str] = frozenset()) -> Optional[str]:
    """Expand a glob like /dev/cu.usbmodem* to a concrete port (macOS
    re-enumerates the number when the board changes USB sockets). With
    multiple matches, prefer the node whose USB VID is the ESP32-S3's, and
    say so — silently picking sorted()[0] grabbed whatever enumerated
    first.

    ``usbsn:<serial>`` names a board by its USB serial number (an ESP32-S3's
    is its MAC) instead: with two ESP32-S3 boards on the bus — the StackChan
    and the Voice PE controller (controller.py) — the VID alone cannot tell
    them apart. A glob skips the nodes whose serial is in ``skip_serials``."""
    if pattern.startswith(USB_SERIAL_PREFIX):
        want = pattern[len(USB_SERIAL_PREFIX):].strip().lower()
        try:
            from serial.tools import list_ports
            return next((p.device for p in list_ports.comports()
                         if (p.serial_number or "").lower() == want and "/cu." in p.device), None)
        except Exception:  # noqa: BLE001 - IOKit can throw mid-enumeration; try again on the next open
            return None
    if "*" not in pattern:
        return pattern
    hits = sorted(glob.glob(pattern))
    if not hits:
        return None
    try:
        from serial.tools import list_ports
        ports = [p for p in list_ports.comports() if p.device in hits]
        skip = {s.lower() for s in skip_serials}
        hits = [h for h in hits
                if not any(p.device == h and (p.serial_number or "").lower() in skip for p in ports)]
        if not hits:
            return None
        vids = {p.device: p.vid for p in ports if p.device in hits}
    except Exception:  # noqa: BLE001 - IOKit can throw mid-enumeration; fall back to the glob alone
        if skip_serials:
            # Without the serials there is no telling the robot from the controller (Eve/PortPick.lean):
            # wait for the next open rather than guess, which could open the controller as the robot.
            return None
        vids = {}
    s3 = sorted(d for d, vid in vids.items() if vid == _ESP32S3_VID)
    if s3:
        if len(hits) > 1:
            log.warning("serial: %d nodes match %s — preferring ESP32-S3 %s",
                        len(hits), pattern, s3[0])
        return s3[0]
    # Every match names a USB vendor and none is Espressif: the board is off the bus and what is left is
    # a camera or a dock (2026-09-23: with the robot power-cycling, the daemon opened a webcam's node,
    # wrote status polls into it and waited for acks). Wait for the board instead of guessing.
    if hits and all(vids.get(d) is not None for d in hits):
        return None
    if len(hits) > 1:
        log.warning("serial: %d nodes match %s — picking %s",
                    len(hits), pattern, hits[0])
    return hits[0]


class EveSerial:
    # A USB cable, not a radio link: the board's BLE "sec" flag says nothing about this link,
    # so the daemon ignores it here (docs/SETUP-ISSUES.md #10).
    wired = True

    def __init__(
        self,
        on_message: IncomingHandler,
        port: str,
        baud: int = 115200,
        skip_serials: frozenset[str] = frozenset(),
    ) -> None:
        self.on_message = on_message
        self.skip_serials = skip_serials         # USB serials a glob must not open (_resolve_port)
        # Optional async hook fired when the board's boot banner scrolls past
        # on an already-open link (see _IS_BOOT). Debounced: a boot emits
        # several matching lines back to back.
        self.on_boot: Optional[Callable[[], Awaitable[None]]] = None
        self._last_boot_hook = 0.0
        self.port_pattern = port
        self.baud = baud
        self._ser: Optional[serial.Serial] = None
        self._connected_evt = asyncio.Event()
        self._send_lock = asyncio.Lock()
        self._stop = asyncio.Event()
        self._drop_reason: Optional[str] = None  # reader-owned teardown request
        self._reopen_holdoff = 0.0               # extra close-time before next open
        self._rx_trips: list[tuple[float, str]] = []  # (monotonic, port) silence trips
        self._pulse_on_open = False              # RTS-reset the board on next open

    @property
    def connected(self) -> bool:
        return (self._ser is not None and self._ser.is_open
                and self._drop_reason is None)

    async def wait_connected(self) -> None:
        await self._connected_evt.wait()

    async def send(self, obj: dict[str, Any], codec: Optional[str] = None) -> bool:
        # Capture once: self._ser can be swapped/dropped across the await
        # suspension points below (the old TOCTOU produced NoneType bursts
        # during teardown windows).
        ser = self._ser
        if ser is None or not ser.is_open or self._drop_reason is not None:
            return False
        data = encode(obj, codec=codec)
        loop = asyncio.get_running_loop()
        try:
            async with self._send_lock:
                if self._ser is not ser or not ser.is_open or self._drop_reason is not None:
                    return False
                # write_timeout bounds this (set at open). No flush(): that was
                # termios.tcdrain, which no timeout covers — one board that
                # stopped draining the OUT endpoint would block a sender
                # forever while every other sender queued on the lock.
                fut = loop.run_in_executor(None, ser.write, data)
                await self._write_holding_lock(fut, ser)
            return True
        except Exception as e:  # noqa: BLE001
            log.warning("serial send failed: %s", e)
            # Only condemn the handle the failure belongs to — a write that
            # raises late must not tear down a fresh reopened connection.
            if self._ser is ser:
                self._note_deaf(ser, e)
                self._request_drop(f"send failed: {e}")
            return False

    async def _write_holding_lock(self, fut: asyncio.Future, ser: serial.Serial) -> None:
        """Await the executor write with _send_lock held until the thread's
        ser.write has RETURNED, even if the sender is cancelled meanwhile.

        A cancelled sender (every ``asyncio.wait_for(ble.send(...))`` that
        times out) used to leave the ``async with`` at once, freeing the lock
        while its thread was still inside ser.write, so the next sender's line
        went out alongside it and broke the NDJSON framing on the wire. The
        wait is bounded by the port's write_timeout. The cancellation is
        re-raised once the port is free (verification/Eve/Serial.lean, A).
        """
        cancelled = False
        while not fut.done():
            try:
                await asyncio.wait((fut,))  # never cancels fut, never raises its error
            except asyncio.CancelledError:
                cancelled = True
        if cancelled:
            err = None if fut.cancelled() else fut.exception()
            if err is not None:
                log.warning("serial send failed: %s", err)
                if self._ser is ser:
                    self._note_deaf(ser, err)
                    self._request_drop(f"send failed: {err}")
            raise asyncio.CancelledError
        fut.result()

    async def run(self) -> None:
        loop = asyncio.get_running_loop()
        missing_since: Optional[float] = None
        next_missing_warn = 30.0
        warn_at = 0.0
        while not self._stop.is_set():
            # Off the loop: with the board off the bus, IOKit's enumeration (list_ports.comports) takes 2-3 s,
            # and on the loop it froze every door, Telegram included (1,543+ stall reports, 2026-09-23/24).
            port = await asyncio.to_thread(_resolve_port, self.port_pattern, self.skip_serials)
            if port is None:
                # Device off the bus: say so, loudly then backing off. The
                # 52-minute silent log hole of 2026-08-05 04:43 was this path
                # having no log line at all.
                now = time.monotonic()
                if missing_since is None:
                    missing_since = now
                    next_missing_warn = 30.0
                    warn_at = now + next_missing_warn
                elif now >= warn_at:
                    # A deadline, not total time missing: compared with the capped gap, every
                    # pass after 600 s warned, every 3 s (two thirds of the log by 2026-10-13).
                    log.warning(
                        "serial: no port matching %s for %.0fs — board unplugged?",
                        self.port_pattern, now - missing_since)
                    next_missing_warn = min(next_missing_warn * 2, 600.0)
                    warn_at = now + next_missing_warn
                await asyncio.sleep(RECONNECT_SECS)
                continue
            if missing_since is not None:
                log.info("serial: device reappeared at %s after %.0fs away",
                         port, time.monotonic() - missing_since)
                missing_since = None
            try:
                ser = serial.Serial(port, self.baud, timeout=1,
                                    write_timeout=2, exclusive=True)
                ser.dtr = True  # firmware HWCDC drops TX without DTR
            except Exception as e:  # noqa: BLE001
                log.warning("serial open %s failed: %s", port, e)
                await asyncio.sleep(RECONNECT_SECS)
                continue

            if self._pulse_on_open:
                # Escalation for the enumerated-but-silent board: pulse RTS on
                # the FRESH handle (pulsing before reopen is a no-op — the old
                # handle is already gone). Logs unconditionally: this line's
                # absence from a log is proof the pulse never ran.
                self._pulse_on_open = False
                log.warning(
                    "serial: RTS-resetting the board — %d silent or deaf reopens of %s "
                    "within %.0f min", RX_TRIP_PULSE_COUNT, port,
                    RX_TRIP_WINDOW_SECS / 60)
                try:
                    ser.rts = True
                    time.sleep(0.1)
                    ser.rts = False
                    ser.dtr = True
                except Exception as e:  # noqa: BLE001
                    log.warning("serial: RTS pulse failed: %s", e)
            try:
                ser.reset_input_buffer()
            except Exception:  # noqa: BLE001
                pass

            self._drop_reason = None
            self._ser = ser
            self._connected_evt.set()
            log.info("serial connected: %s @ %d", port, self.baud)
            buf = b""
            # Grace period starts now: a board mid-boot hasn't printed yet.
            last_rx = time.monotonic()
            try:
                while not self._stop.is_set():
                    chunk = await loop.run_in_executor(None, _read_chunk, ser)
                    if self._drop_reason is not None:
                        log.warning("serial: dropping link — %s", self._drop_reason)
                        break
                    if not chunk:
                        # read returned on its 1s timeout (or a
                        # cancel_read). Silence past the watchdog means a stale
                        # fd or a hung board — drop and let the outer loop
                        # re-resolve the glob and reopen.
                        if time.monotonic() - last_rx > RX_SILENCE_SECS:
                            log.warning(
                                "serial: no data from board for %.0fs — assuming stale "
                                "handle (USB re-enumeration?), reconnecting",
                                RX_SILENCE_SECS,
                            )
                            self._note_rx_trip(port)
                            break
                        continue
                    last_rx = time.monotonic()
                    buf += chunk
                    # A chunk may hold several lines, or the tail of a long
                    # one (a camera frame spans many USB packets).
                    while True:
                        nl = buf.find(b"\n")
                        if nl < 0:
                            break
                        line, buf = buf[:nl], buf[nl + 1:]
                        self._dispatch_line(line)
                    if len(buf) > MAX_LINE_BYTES:
                        log.warning("serial: %d bytes without a newline — discarding", len(buf))
                        buf = b""
            except Exception as e:  # noqa: BLE001
                log.warning("serial read error (%s) — reconnecting", e)
            finally:
                self._close_current()
            holdoff = self._reopen_holdoff or RECONNECT_SECS
            self._reopen_holdoff = 0.0
            await asyncio.sleep(holdoff)

    def _dispatch_line(self, line: bytes) -> None:
        """Loop thread. One complete line off the wire: JSON to on_message,
        anything else to the log (crash output at WARNING, see _IS_CRASH)."""
        text = line.decode("utf-8", errors="replace").strip()
        if not text.startswith("{"):
            if text:
                # Crash output must survive at the default log level. A panic
                # backtrace logged at DEBUG is invisible exactly when it
                # matters, and asking someone to reproduce a freeze under a
                # hand-started DEBUG daemon is a bad trade.
                # The board's answer to a motion request is the only way to know
                # whether it admitted, reduced or refused it, and a bench run that
                # needs a DEBUG daemon to see that is a bench run nobody does. So
                # the motion echo rides at INFO while the rest of the board's
                # chatter stays at DEBUG.
                level = logging.DEBUG
                if _IS_CRASH.search(text):
                    level = logging.WARNING
                elif _IS_MOTION.search(text):
                    level = logging.INFO
                log.log(level, "stick: %s", text)
                if (self.on_boot is not None
                        and _IS_BOOT.search(text)
                        and time.monotonic() - self._last_boot_hook > 5.0):
                    self._last_boot_hook = time.monotonic()
                    asyncio.create_task(self.on_boot())
            return
        try:
            obj = json.loads(text)
        except json.JSONDecodeError:
            log.debug("serial: bad json: %r", text[:120])
            return
        asyncio.create_task(self.on_message(obj))

    def _close_current(self) -> None:
        """Close the current handle. Reader-side only (run()'s finally)."""
        self._connected_evt.clear()
        ser, self._ser = self._ser, None
        if ser is not None:
            try:
                ser.close()
            except Exception:  # noqa: BLE001
                pass

    def _request_drop(self, why: str) -> None:
        """Ask the reader to drop and reopen. Never closes the fd here —
        the reader owns the close (see module docstring)."""
        if self._drop_reason is None:
            self._drop_reason = why
        self._connected_evt.clear()
        ser = self._ser
        if ser is not None:
            try:
                ser.cancel_read()  # unblocks the reader's blocking read immediately
            except Exception:  # noqa: BLE001
                pass

    def _note_rx_trip(self, port: str) -> None:
        """Sliding-window counter of enumerated-but-silent episodes; arms the
        RTS pulse when the same port trips repeatedly."""
        now = time.monotonic()
        self._rx_trips = [
            (t, p) for (t, p) in self._rx_trips
            if now - t < RX_TRIP_WINDOW_SECS
        ]
        self._rx_trips.append((now, port))
        if sum(1 for (_, p) in self._rx_trips if p == port) >= RX_TRIP_PULSE_COUNT:
            self._pulse_on_open = True
            self._rx_trips.clear()

    def _note_deaf(self, ser: serial.Serial, err: BaseException) -> None:
        """A write timeout is the deaf half of the wedge: the board has stopped draining the OUT endpoint. It
        counts as a trip like a silent open, so repeated ones RTS-reset the board instead of reopening it every
        5 s forever (the Voice PE, 2026-10-01 to 10-03: ~35,000 reopens, every one timing out)."""
        if isinstance(err, serial.SerialTimeoutException) and ser.port:
            self._note_rx_trip(ser.port)

    def pulse_reset(self, why: str) -> None:
        """Hardware-reset the board via the RTS line, then reconnect.

        The escalation for a board-side USB wedge: the S3 keeps transmitting
        but stops receiving, and reopening the host port does nothing because
        the dead half is in the chip. The pet reboots (~5s, diag ring reports
        the reset as external-pin) — a short outage against an indefinitely
        deaf board. Pulses the LIVE handle first (ioctls under a concurrent
        select are safe; the fd must be open to pulse), then hands the drop
        to the reader.
        """
        ser = self._ser
        log.warning("serial: RTS-resetting the board — %s", why)
        if ser is not None:
            try:
                ser.dtr = False
                ser.rts = True
                time.sleep(0.1)
                ser.rts = False
            except Exception as e:  # noqa: BLE001
                log.warning("serial: RTS pulse failed: %s", e)
        self._request_drop(f"RTS reset: {why}")

    def force_reconnect(self, why: str) -> None:
        """Drop the handle so run() reopens the port — after a deliberate
        closed-port holdoff. Field evidence: daemon restarts (tens of seconds
        closed) revived the deaf wedge; 3s in-process reopens never did.

        For the half-dead link: writes silently go nowhere while reads keep
        working, so RX_SILENCE_SECS never trips and the daemon looks healthy
        while the board sits there showing "No Claude connected". Only the
        absence of *replies* reveals it — see the daemon's status-ack watchdog.
        """
        log.warning("serial: forcing reconnect (holding port closed %.0fs) — %s",
                    FORCED_HOLDOFF_SECS, why)
        self._reopen_holdoff = FORCED_HOLDOFF_SECS
        self._request_drop(why)

    async def stop(self) -> None:
        self._stop.set()
        self._request_drop("stopping")
