"""pika.py: the Pika car's hold-to-drive panel and its video, against a fake car on loopback.

The fake car speaks what github.com/gurul/pika's camera board does (firmware/camera_s3/camera_s3.ino): one TCP client
at a time, ``{Heartbeat}`` from the board, ``{ok}`` from the UNO for a stop, and nothing at all to a second client
while the first is connected. Its video is ESP-IDF httpd's chunked multipart (app_httpd.cpp ``stream_handler``)."""
from __future__ import annotations

import asyncio
import json
from pathlib import Path
from typing import Any, Optional

import pytest
from test_link_chat import FakeInlet, http, serve, store_at

from eve_bridge import link_chat, pika
from eve_bridge.pika import Pika, PikaConfig

# The car repo's own examples (README "Wire protocol"), with eve's pulse length: the bytes must be these exactly.
README_FORWARD = b'{"H":"4","N":2,"D1":3,"D2":120,"T":800}'
README_STOP = b'{"H":"6","N":100}'


class FakeCar:
    """The camera board's TCP relay on a loopback port. ``busy``: it accepts and never answers (another controller
    has the slot). Records every byte each client sent, and whether each connection is still open."""

    def __init__(self, busy: bool = False, beat: bool = False) -> None:
        self.busy = busy
        self.beat = beat
        self.clients: list[bytearray] = []
        self.open: list[bool] = []
        self.writers: list[asyncio.StreamWriter] = []
        self.port = 0
        self._server: Optional[asyncio.base_events.Server] = None

    async def start(self) -> int:
        self._server = await asyncio.start_server(self._client, "127.0.0.1", 0)
        self.port = self._server.sockets[0].getsockname()[1]
        return self.port

    async def _client(self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
        got = bytearray()
        i = len(self.clients)
        self.clients.append(got)
        self.open.append(True)
        self.writers.append(writer)
        if self.beat and not self.busy:
            writer.write(b"{Heartbeat}")
        buf = b""
        try:
            while True:
                data = await reader.read(1024)
                if not data:
                    break
                got.extend(data)
                buf += data
                while b"}" in buf:
                    f, buf = buf.split(b"}", 1)
                    f += b"}"
                    if not self.busy and b'"N":100' in f:
                        writer.write(b"{ok}")                 # the UNO's answer to a stop (no tag)
        except (ConnectionError, OSError):
            pass
        self.open[i] = False
        writer.close()

    def frames(self, i: int = 0) -> list[bytes]:
        out, rest = [], bytes(self.clients[i])
        while b"}" in rest:
            f, rest = rest.split(b"}", 1)
            out.append(f + b"}")
        return out

    def hang_up(self, i: int = 0) -> None:
        self.writers[i].close()

    async def close(self) -> None:
        if self._server is not None:
            self._server.close()
            for w in self.writers:
                w.close()


def make(car: FakeCar, **kw: Any) -> Pika:
    cfg = PikaConfig(host="127.0.0.1", port=car.port, video_port=kw.pop("video_port", 1))
    kw.setdefault("heartbeat_secs", 0.3)
    kw.setdefault("idle_secs", 30.0)
    kw.setdefault("hello_secs", 0.5)
    kw.setdefault("tick_secs", 0.02)
    return Pika(cfg, **kw)


def drive(p: Pika, d: str, hold: str) -> Any:
    return p.panel({"action": "drive", "dir": d, "hold": hold})


async def settle(secs: float = 0.05) -> None:
    await asyncio.sleep(secs)


# ---- the wire ------------------------------------------------------------------------------------------------

def test_frames_are_the_car_repos_bytes_exactly() -> None:
    assert pika.frame(4, N=2, D1=3, D2=120, T=800) == README_FORWARD
    assert pika.stop_frame(6) == README_STOP
    assert pika.drive_frame(4, 3, 120) == b'{"H":"4","N":2,"D1":3,"D2":120,"T":450}'
    assert pika.DIRS == {"left": 1, "right": 2, "forward": 3, "back": 4}             # tools/car.py DIR
    assert pika.PULSE_MS <= 450                                                       # tools/safety.py MAX_PULSE_MS
    assert pika.split_frames('{Heartbeat}{3_ok}{T_1') == (["{Heartbeat}", "{3_ok}"], "{T_1")


def test_a_held_drive_then_release_is_hello_drive_stop_byte_for_byte() -> None:
    async def go() -> None:
        car = FakeCar()
        await car.start()
        p = make(car)
        assert (await drive(p, "forward", "h1")) == {"ok": True, "line": ""}
        assert (await p.panel({"action": "stop", "hold": "h1"}))["ok"]
        await settle()
        assert car.frames() == [b'{"H":"1","N":100}',                            # the hello
                                b'{"H":"2","N":2,"D1":3,"D2":120,"T":450}',
                                b'{"H":"3","N":100}']
        assert p.stops == ["release"]
        # a turn uses the turn speed; back the drive speed
        assert (await drive(p, "left", "h2"))["ok"] and (await drive(p, "back", "h2"))["ok"]
        await p.panel({"action": "stop", "hold": "h2"})
        await settle()
        assert car.frames()[3:] == [b'{"H":"4","N":2,"D1":1,"D2":140,"T":450}',
                                    b'{"H":"5","N":2,"D1":4,"D2":120,"T":450}', b'{"H":"6","N":100}']
        await p.close()
        await car.close()

    asyncio.run(go())


def test_beats_renew_the_timed_move_but_not_faster_than_resend() -> None:
    async def go() -> None:
        car = FakeCar()
        await car.start()
        p = make(car)
        for _ in range(4):                                  # four beats in one burst: one frame
            assert (await drive(p, "forward", "h"))["ok"]
        await settle(pika.RESEND_SECS + 0.05)
        assert (await drive(p, "forward", "h"))["ok"]       # a beat after RESEND_SECS: renewed
        await settle()
        drives = [f for f in car.frames() if b'"N":2' in f]
        assert len(drives) == 2
        await p.close()
        await car.close()

    asyncio.run(go())


# ---- the stops -----------------------------------------------------------------------------------------------

def test_a_missed_heartbeat_stops_and_the_late_beat_never_restarts_it() -> None:
    async def go() -> None:
        car = FakeCar()
        await car.start()
        p = make(car, heartbeat_secs=0.15)
        assert (await drive(p, "forward", "h"))["ok"]
        await settle(0.35)                                  # no beat for longer than the heartbeat
        assert p.stops == ["heartbeat"]
        assert car.frames()[-1] == b'{"H":"3","N":100}'
        late = await drive(p, "forward", "h")
        assert late == {"ok": False, "stopped": True, "line": pika.LATE_LINE}
        await settle()
        assert [f for f in car.frames() if b'"N":2' in f] == [b'{"H":"2","N":2,"D1":3,"D2":120,"T":450}']
        assert (await drive(p, "forward", "h2"))["ok"]      # a new press drives again
        await p.close()
        await car.close()

    asyncio.run(go())


def test_a_beat_that_crossed_the_release_in_the_tunnel_is_refused() -> None:
    async def go() -> None:
        car = FakeCar()
        await car.start()
        p = make(car)
        assert (await drive(p, "right", "h"))["ok"]
        await p.panel({"action": "stop", "hold": "h"})
        assert (await drive(p, "right", "h"))["stopped"] is True
        await settle()
        assert sum(b'"N":2' in f for f in car.frames()) == 1
        await p.close()
        await car.close()

    asyncio.run(go())


def test_a_release_while_connecting_means_no_drive_frame_at_all() -> None:
    async def go() -> None:
        car = FakeCar()
        await car.start()
        gate = asyncio.Event()

        async def slow_open(host: str, port: int) -> Any:
            await gate.wait()
            return await asyncio.open_connection(host, port)

        p = make(car, opener=slow_open)
        pending = asyncio.ensure_future(drive(p, "forward", "h"))
        await settle()
        assert (await p.panel({"action": "stop", "hold": "h"}))["ok"]   # answered at once, nothing to wait on
        gate.set()
        assert (await pending)["stopped"] is True
        await settle()
        assert not any(b'"N":2' in f for f in car.frames())
        assert not asyncio.iscoroutinefunction(Pika.stop)                # a stop never awaits anything
        await p.close()
        await car.close()

    asyncio.run(go())


def test_the_car_hanging_up_ends_the_press() -> None:
    async def go() -> None:
        car = FakeCar()
        await car.start()
        p = make(car)
        assert (await drive(p, "forward", "h"))["ok"]
        car.hang_up()                                       # the camera board stops the UNO itself on a disconnect
        await settle(0.1)
        assert not p.connected
        assert (await drive(p, "forward", "h")) == {"ok": False, "stopped": True, "line": pika.LOST_LINE}
        assert (await drive(p, "forward", "h2"))["ok"]      # a new press connects again
        assert len(car.clients) == 2
        await p.close()
        await car.close()

    asyncio.run(go())


def test_an_error_mid_drive_sends_a_stop(monkeypatch: pytest.MonkeyPatch) -> None:
    async def go() -> None:
        car = FakeCar()
        await car.start()
        p = make(car)
        assert (await drive(p, "forward", "h"))["ok"]
        real = pika.drive_frame

        def broken(*a: Any, **k: Any) -> bytes:
            raise OSError("boom")

        monkeypatch.setattr(pika, "drive_frame", broken)
        await settle(pika.RESEND_SECS + 0.02)
        out = await drive(p, "left", "h")
        assert out == {"ok": False, "stopped": True, "line": pika.LOST_LINE}
        assert p.stops == ["error"]
        await settle()
        assert car.frames()[1:] == [b'{"H":"2","N":2,"D1":3,"D2":120,"T":450}', b'{"H":"4","N":100}']  # tag 3 failed
        assert not p.connected
        monkeypatch.setattr(pika, "drive_frame", real)
        await p.close()
        await car.close()

    asyncio.run(go())


def test_close_stops_and_lets_go_of_the_slot() -> None:
    async def go() -> None:
        car = FakeCar()
        await car.start()
        p = make(car)
        assert (await drive(p, "back", "h"))["ok"]
        assert (await p.panel({"action": "close", "hold": "h"}))["ok"]
        await settle(0.1)
        assert car.frames()[-1] == b'{"H":"3","N":100}' and p.stops == ["panel closed"]
        assert car.open == [False] and not p.connected
        await car.close()

    asyncio.run(go())


# ---- the one slot --------------------------------------------------------------------------------------------

def test_the_slot_is_let_go_after_idle() -> None:
    async def go() -> None:
        car = FakeCar(beat=True)
        await car.start()
        p = make(car, idle_secs=0.2)
        assert (await drive(p, "forward", "h"))["ok"]
        await p.panel({"action": "stop", "hold": "h"})
        await settle(0.1)
        assert car.open == [True]
        assert b"{Heartbeat}" in car.clients[0]             # eve answered the board's heartbeat
        await settle(0.4)
        assert car.open == [False] and not p.connected
        assert p.stops == ["release", "idle"]
        await car.close()

    asyncio.run(go())


def test_a_busy_car_says_so_in_one_line_and_lets_go() -> None:
    async def go() -> None:
        car = FakeCar(busy=True)
        await car.start()
        p = make(car)
        out = await drive(p, "forward", "h")
        assert out == {"ok": False, "stopped": True, "line": pika.BUSY_LINE}
        assert "\n" not in pika.BUSY_LINE
        await settle(0.1)
        assert car.open == [False] and not p.connected
        assert not any(b'"N":2' in f for f in car.frames())
        await car.close()

    asyncio.run(go())


def test_an_unreachable_car_says_so() -> None:
    async def go() -> None:
        async def refuse(host: str, port: int) -> Any:
            raise ConnectionRefusedError()

        p = Pika(PikaConfig(host="127.0.0.1", port=9), opener=refuse)
        assert await drive(p, "forward", "h") == {"ok": False, "stopped": True, "line": pika.UNREACHABLE_LINE}
        assert (await p.panel({"action": "status"})) == {"ok": True, "connected": False, "line": ""}
        assert (await p.panel({"action": "drive", "dir": "up", "hold": "x"}))["ok"] is False
        assert (await p.panel({"action": "spin"}))["ok"] is False

    asyncio.run(go())


# ---- the switch and the config -------------------------------------------------------------------------------

def test_off_by_default_and_without_config(tmp_path: Path) -> None:
    cfg = tmp_path / "pika.json"
    assert pika.make_pika(cfg, environ={}) is None                                   # off by default
    assert pika.make_pika(cfg, environ={"EVE_PIKA": "1"}) is None                     # on, but no config
    cfg.write_text(json.dumps({"host": "192.168.1.50"}))
    assert pika.make_pika(cfg, environ={"EVE_PIKA": "0"}) is None
    made = pika.make_pika(cfg, environ={"EVE_PIKA": "1"})
    assert made is not None and made.cfg == PikaConfig(host="192.168.1.50")
    cfg.write_text(json.dumps({"host": "bad host!", "port": "x", "speed": 999, "turn_speed": 90, "video_port": 8081}))
    assert pika.load(cfg) == PikaConfig(turn_speed=90, video_port=8081)
    cfg.write_text("[]")
    assert pika.load(cfg) is None


# ---- through the link door: auth, listing, video -------------------------------------------------------------

class FakeCamera:
    """The camera's MJPEG server: ESP-IDF httpd, chunked, one viewer at a time (a second waits, unanswered)."""

    def __init__(self, frames: list[bytes]) -> None:
        self.frames = frames
        self.viewers = 0
        self.ended = 0
        self.port = 0
        self._busy = asyncio.Lock()

    async def start(self) -> int:
        server = await asyncio.start_server(self._client, "127.0.0.1", 0)
        self.port = server.sockets[0].getsockname()[1]
        return self.port

    async def _client(self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
        await reader.readuntil(b"\r\n\r\n")
        async with self._busy:
            self.viewers += 1
            writer.write(b"HTTP/1.1 200 OK\r\nContent-Type: multipart/x-mixed-replace;boundary=123456789000000000000987654321"
                         b"\r\nTransfer-Encoding: chunked\r\n\r\n")
            try:
                i = 0
                while True:
                    jpeg = self.frames[i % len(self.frames)]
                    part = (b"\r\n--123456789000000000000987654321\r\nContent-Type: image/jpeg\r\nContent-Length: %d"
                            b"\r\n\r\n" % len(jpeg) + jpeg)
                    for piece in (part[:7], part[7:]):          # chunk edges inside a part
                        writer.write(b"%x\r\n" % len(piece) + piece + b"\r\n")
                    await writer.drain()
                    i += 1
                    await asyncio.sleep(0.02)
            except (ConnectionError, OSError):
                pass
            finally:
                self.ended += 1
                writer.close()


def jpeg(n: int) -> bytes:
    return b"\xff\xd8" + bytes([n]) * 40 + b"\xff\xd9"


async def read_parts(port: int, token: str, want: int) -> tuple[int, list[bytes], asyncio.StreamWriter]:
    reader, writer = await asyncio.open_connection("127.0.0.1", port)
    writer.write(f"GET {link_chat.PIKA_STREAM_PATH} HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {token}\r\n\r\n".encode())
    await writer.drain()
    head = await asyncio.wait_for(reader.readuntil(b"\r\n\r\n"), 5)
    status = int(head.split()[1])
    assert b"multipart/x-mixed-replace; boundary=frame" in head
    got: list[bytes] = []
    buf = b""
    async for data in pika._body(reader, chunked=True):
        buf += data
        found, buf = pika.split_jpegs(buf)
        got += found
        if len(got) >= want:
            break
    return status, got, writer


def test_the_control_and_the_video_need_the_link_token(tmp_path: Path) -> None:
    async def go() -> None:
        server, port, token = await serve(tmp_path, FakeInlet(store_at(tmp_path)))
        car = FakeCar()
        await car.start()
        server.pika = make(car)
        doors = (("POST", f"{link_chat.CONTROLS_PATH}/pika"), ("GET", link_chat.PIKA_STREAM_PATH))
        body = {"action": "drive", "dir": "forward", "hold": "h"}
        for method, path in doors:
            assert (await http(port, method, path, body))[0] == 401, path
            assert (await http(port, method, path, body, token=token[:-1]))[0] == 401, path
            assert (await http(port, method, path, body, token=token, origin="https://evil.example"))[0] == 403, path
        assert car.clients == []                                                     # nothing reached the car
        status, _, out = await http(port, "POST", f"{link_chat.CONTROLS_PATH}/pika", body, token=token)  # positive control
        assert status == 200 and json.loads(out) == {"ok": True, "line": ""}
        await server.pika.close()
        await car.close()
        await server.close()

    asyncio.run(go())


def test_pika_is_listed_only_when_it_is_on(tmp_path: Path) -> None:
    async def go() -> None:
        server, port, token = await serve(tmp_path, FakeInlet(store_at(tmp_path)))
        listing = json.loads((await http(port, "GET", link_chat.CONTROLS_PATH, token=token))[2])
        assert listing["pika"] is False
        for method, path in (("POST", f"{link_chat.CONTROLS_PATH}/pika"), ("GET", link_chat.PIKA_STREAM_PATH)):
            status, _, out = await http(port, method, path, {"action": "status"}, token=token)
            assert status == 404 and json.loads(out)["line"] == link_chat.NO_CONTROL["pika"], path
        server.pika = Pika(PikaConfig(host="127.0.0.1"))
        assert json.loads((await http(port, "GET", link_chat.CONTROLS_PATH, token=token))[2])["pika"] is True
        await server.close()

    asyncio.run(go())


def test_the_video_is_relayed_one_viewer_at_a_time(tmp_path: Path) -> None:
    async def go() -> None:
        server, port, token = await serve(tmp_path, FakeInlet(store_at(tmp_path)))
        camera = FakeCamera([jpeg(1), jpeg(2), jpeg(3)])
        await camera.start()
        car = FakeCar()
        await car.start()
        server.pika = make(car, video_port=camera.port)
        status, got, first = await read_parts(port, token, 3)
        assert status == 200 and all(g in {jpeg(1), jpeg(2), jpeg(3)} for g in got)
        status, got, second = await read_parts(port, token, 2)                     # the newest viewer wins
        assert status == 200 and len(got) >= 2
        await settle(0.1)
        assert camera.ended == 1                                                     # the first upstream was let go
        await server.pika.panel({"action": "close"})                                 # the panel closed
        await settle(0.1)
        assert camera.ended == 2 and server.pika._video is None
        first.close()
        second.close()
        await car.close()
        await server.close()

    asyncio.run(go())


def test_a_camera_that_is_off_is_one_plain_line(tmp_path: Path) -> None:
    async def go() -> None:
        server, port, token = await serve(tmp_path, FakeInlet(store_at(tmp_path)))

        async def refuse(host: str, p: int) -> Any:
            raise ConnectionRefusedError()

        server.pika = Pika(PikaConfig(host="127.0.0.1"), opener=refuse)
        status, _, out = await http(port, "GET", link_chat.PIKA_STREAM_PATH, token=token)
        assert status == 503 and json.loads(out) == {"ok": False, "line": pika.VIDEO_UNREACHABLE_LINE}
        await server.close()

    asyncio.run(go())


def test_split_jpegs_keeps_a_partial_frame() -> None:
    a, b = jpeg(1), jpeg(2)
    found, rest = pika.split_jpegs(b"junk" + a + b"--x" + b[:10])
    assert found == [a] and rest == b[:10]
    found, rest = pika.split_jpegs(rest + b[10:])
    assert found == [b] and rest == b""[-1:]
    assert pika.video_part(a).startswith(b"--frame\r\nContent-Type: image/jpeg\r\nContent-Length: 44\r\n\r\n\xff\xd8")
