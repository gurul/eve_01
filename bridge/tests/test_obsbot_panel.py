"""obsbot_panel.py: the camera's Eve Link panel (hold-to-turn with a heartbeat, the reads, the link door) and its
picture (a fake ffmpeg that lists an OBSBOT camera and writes JPEGs, or is refused the camera)."""

from __future__ import annotations

import asyncio
import json
import stat
import sys
from pathlib import Path
from typing import Any

from test_link_chat import FakeInlet, http, serve, store_at

from eve_bridge import link_chat, obsbot, obsbot_panel, pika
from eve_bridge.obsbot_panel import ObsbotPanel


class FakeUsbTurns:
    """UsbCamera's turn and wake, recorded into the shared list."""

    def __init__(self, sent: list):
        self.sent = sent
        self.why = ""

    def wake(self) -> bool:
        self.sent.append(("wake", []))
        return True

    def turn(self, direction: str, speed: int = 0) -> bool:
        self.sent.append(("turn", [direction]))
        return True


class FakeCam:
    """Obsbot's surface: moves and reads answered, USB turns recorded."""

    def __init__(self, sent: list, ready: bool = True, **kw: Any):
        self.sent = sent
        self._ready = False
        self.answers = ready
        self.device = 0
        self.usb = FakeUsbTurns(sent)

    def ensure(self) -> Any:
        if not self.answers:
            return "No OBSBOT camera is plugged into this Mac."
        self._ready = True
        return None

    def status(self) -> dict:
        return {"ok": True, "line": "Tiny 3 awake; pan 0°, pitch 0°; zoom 0%.", "pan": 0}

    def zoom(self, level: int) -> dict:
        self.sent.append(("zoom", [level]))
        return {"ok": True, "line": f"Zoom {level}%.", "zoom": level}

    def move(self, pan: Any, pitch: Any) -> dict:
        self.sent.append(("move", [pan, pitch]))
        return {"ok": True, "line": f"Pan {pan:g}°, pitch {pitch:g}°.", "pan": pan, "pitch": pitch}

    def close(self) -> None:
        pass


def make_panel(sent: list, ready: bool = True, **kw: Any) -> ObsbotPanel:
    return ObsbotPanel(make=lambda **k: FakeCam(sent, ready, **k), **kw)


def turns(sent: list) -> list:
    return [a[0] for n, a in sent if n == "turn"]


def test_a_press_wakes_and_turns_once_and_a_stop_ends_it() -> None:
    async def go() -> None:
        sent: list = []
        p = make_panel(sent)
        for _ in range(3):
            assert await p.panel({"action": "hold", "dir": "left", "hold": "a"}) == {"ok": True, "line": ""}
        # the first beat wakes the camera and starts the USB turn; the rest only renew it
        assert [n for n, _ in sent] == ["wake", "turn"] and turns(sent) == ["left"]
        res = await p.panel({"action": "stop", "hold": "a"})
        assert res["stopped"] and turns(sent) == ["left", "stop"]
        late = await p.panel({"action": "hold", "dir": "left", "hold": "a"})        # a beat that crossed the stop
        assert late["stopped"] and not late["ok"] and turns(sent) == ["left", "stop"]
        await p.close()

    asyncio.run(go())


def test_a_new_direction_replaces_the_turn() -> None:
    async def go() -> None:
        sent: list = []
        p = make_panel(sent)
        await p.panel({"action": "hold", "dir": "up", "hold": "a"})
        await p.panel({"action": "hold", "dir": "right", "hold": "b"})
        assert turns(sent) == ["up", "right"]
        assert (await p.panel({"action": "hold", "dir": "up", "hold": "a"}))["stopped"]   # the replaced press
        await p.close()

    asyncio.run(go())


def test_no_beat_stops_the_gimbal() -> None:
    async def go() -> None:
        sent: list = []
        p = make_panel(sent)
        await p.panel({"action": "hold", "dir": "down", "hold": "a"})
        await asyncio.sleep(obsbot_panel.HEARTBEAT_SECS + 0.3)
        assert turns(sent) == ["down", "stop"] and p._hold is None
        assert (await p.panel({"action": "hold", "dir": "down", "hold": "a"}))["stopped"]
        await p.close()

    asyncio.run(go())


def test_beats_keep_it_turning_past_the_heartbeat() -> None:
    async def go() -> None:
        sent: list = []
        p = make_panel(sent)
        for _ in range(8):                                                          # 8 x 0.15 s > 0.45 s
            await p.panel({"action": "hold", "dir": "down", "hold": "a"})
            await asyncio.sleep(0.15)
        assert turns(sent) == ["down"]                                              # no stop between
        await p.panel({"action": "close"})
        assert turns(sent) == ["down", "stop"]
        await p.close()

    asyncio.run(go())


def test_reads_and_refusals() -> None:
    async def go() -> None:
        sent: list = []
        p = make_panel(sent)
        st = await p.panel({"action": "status"})
        assert st["ok"] and st["modes"] == list(obsbot.AI_MODES)
        assert (await p.panel({"action": "zoom", "level": "40"}))["line"] == "Zoom 40%."
        assert not (await p.panel({"action": "zoom", "level": True}))["ok"]
        assert (await p.panel({"action": "move", "pan": 10, "pitch": -5}))["pan"] == 10
        assert not (await p.panel({"action": "fly"}))["ok"]
        assert not (await p.panel({"action": "hold", "dir": "sideways", "hold": "x"}))["ok"]
        down = make_panel([], ready=False)
        res = await down.panel({"action": "status"})
        assert res == {"ok": False, "line": "No OBSBOT camera is plugged into this Mac.", "stopped": False}
        await p.close()
        await down.close()

    asyncio.run(go())


def test_it_is_listed_and_its_panel_is_behind_the_link_token(tmp_path: Path) -> None:
    async def go() -> None:
        server, port, token = await serve(tmp_path, FakeInlet(store_at(tmp_path)))
        listing = json.loads((await http(port, "GET", link_chat.CONTROLS_PATH, token=token))[2])
        assert listing["obsbot"] is False
        sent: list = []
        server.obsbot = make_panel(sent, environ={"EVE_OBSBOT": "1"})
        path = f"{link_chat.CONTROLS_PATH}/obsbot"
        assert (await http(port, "POST", path, {"action": "zoom", "level": 10}))[0] == 401
        assert (await http(port, "GET", link_chat.OBSBOT_STREAM_PATH))[0] == 401
        assert sent == []                                                           # nothing reached the camera
        listing = json.loads((await http(port, "GET", link_chat.CONTROLS_PATH, token=token))[2])
        assert listing["obsbot"] is obsbot.APP_PATH.exists()
        if obsbot.APP_PATH.exists():                                                # positive control
            status, _, out = await http(port, "POST", path, {"action": "zoom", "level": 10}, token=token)
            assert status == 200 and json.loads(out)["zoom"] == 10
        server.obsbot = make_panel(sent, environ={"EVE_OBSBOT": "0"})
        assert json.loads((await http(port, "GET", link_chat.CONTROLS_PATH, token=token))[2])["obsbot"] is False
        status, _, out = await http(port, "POST", path, {"action": "status"}, token=token)
        assert status == 404 and json.loads(out)["line"] == link_chat.NO_CONTROL["obsbot"]
        await server.close()

    asyncio.run(go())


# ---- the picture ---------------------------------------------------------------------------------------------

FAKE_FFMPEG = r'''#!{python}
import sys, time
args = sys.argv[1:]
mode = {mode!r}
if "-list_devices" in args:
    sys.stderr.write("[AVFoundation indev @ 0x1] AVFoundation video devices:\n"
                     "[AVFoundation indev @ 0x1] [0] MacBook Pro Camera\n"
                     + ("[AVFoundation indev @ 0x1] [1] OBSBOT Tiny 3 StreamCamera\n" if mode != "none" else "")
                     + "[AVFoundation indev @ 0x1] AVFoundation audio devices:\n"
                     "[AVFoundation indev @ 0x1] [0] OBSBOT Tiny 3 Microphone\n")
    sys.exit(1)
if mode == "denied":
    sys.stderr.write("Failed to create AV capture input device: Cannot use OBSBOT (not authorized to capture video)\n")
    sys.exit(1)
assert args[args.index("-i") + 1] == "1:none", args
n = 0
while True:
    n += 1
    sys.stdout.buffer.write(b"\xff\xd8" + bytes([n % 250]) * 50 + b"\xff\xd9")
    sys.stdout.flush()
    time.sleep(0.05)
'''


def fake_ffmpeg(tmp_path: Path, mode: str = "ok") -> str:
    f = tmp_path / f"ffmpeg-{mode}"
    f.write_text(FAKE_FFMPEG.format(python=sys.executable, mode=mode))
    f.chmod(f.stat().st_mode | stat.S_IEXEC)
    return str(f)


async def read_parts(port: int, token: str, want: int) -> tuple[int, list[bytes], asyncio.StreamWriter, Any]:
    reader, writer = await asyncio.open_connection("127.0.0.1", port)
    writer.write(f"GET {link_chat.OBSBOT_STREAM_PATH} HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {token}\r\n\r\n"
                 .encode())
    await writer.drain()
    head = await asyncio.wait_for(reader.readuntil(b"\r\n\r\n"), 10)
    assert b"multipart/x-mixed-replace; boundary=frame" in head and b"X-Stamp" not in head
    got: list[bytes] = []
    buf = b""
    async for data in pika._body(reader, chunked=True):
        buf += data
        found, buf = pika.split_jpegs(buf)
        got += found
        if len(got) >= want:
            break
    return int(head.split()[1]), got, writer, reader


def test_two_viewers_share_one_capture_and_ffmpeg_stops_after_both(tmp_path: Path, monkeypatch: Any) -> None:
    """Two open screens both get frames; neither ends the other (2026-10-11: a phone and a simulator took the camera
    from each other every 3 s under the old one-viewer rule)."""
    monkeypatch.setattr(obsbot_panel, "LINGER_SECS", 0.2)

    async def go() -> None:
        server, port, token = await serve(tmp_path, FakeInlet(store_at(tmp_path)))
        server.obsbot = make_panel([], environ={"EVE_OBSBOT": "1"}, ffmpeg=fake_ffmpeg(tmp_path))
        if not server.obsbot.available:                                            # no OBSBOT Center here
            await server.close()
            return
        status, got, first, first_reader = await read_parts(port, token, 3)
        assert status == 200 and len(got) >= 3 and all(g.startswith(b"\xff\xd8") for g in got)
        status, got, second, _ = await read_parts(port, token, 2)                  # a second screen opens
        assert status == 200 and len(got) >= 2
        cap = server.obsbot._capture
        assert cap.viewers == 2 and cap.running
        more = await asyncio.wait_for(first_reader.read(4096), 3)                  # the first still gets frames
        assert b"\xff\xd8" in more or len(more) > 0
        first.close()
        second.close()
        await server.obsbot.panel({"action": "close"})                             # stops turning, not the picture
        for _ in range(40):
            await asyncio.sleep(0.1)
            if not cap.running:
                break
        assert cap.viewers == 0 and not cap.running                                # ffmpeg ended after the linger
        await server.close()

    asyncio.run(go())


def test_the_capture_hands_every_viewer_the_newest_frame() -> None:
    async def go() -> None:
        made = 0

        async def frames():
            nonlocal made
            made += 1
            for n in range(1, 50):
                yield bytes([n])
                await asyncio.sleep(0.02)

        cap = obsbot_panel.Capture(frames)
        a, b = cap.watch(), cap.watch()
        got_a = [await anext(a) for _ in range(3)]
        got_b = [await anext(b) for _ in range(3)]
        assert made == 1                                                            # one ffmpeg for both
        assert got_a == sorted(got_a) and len(set(got_a)) == 3 and got_b == sorted(got_b)
        await a.aclose()
        await b.aclose()
        await cap.stop()

        async def broken():
            raise LookupError("no camera")
            yield b""                                                                # an async generator

        cap = obsbot_panel.Capture(broken)
        w = cap.watch()
        try:
            await anext(w)
            raise AssertionError("a frame came")
        except LookupError as e:
            assert str(e) == "no camera"
        await w.aclose()

    asyncio.run(go())


def test_the_picture_says_why_it_cant_start(tmp_path: Path) -> None:
    async def go() -> None:
        for mode, line in (("denied", obsbot_panel.VIDEO_DENIED_LINE), ("none", obsbot_panel.VIDEO_OFF_LINE)):
            p = make_panel([], ffmpeg=fake_ffmpeg(tmp_path, mode))
            frames = p.frames()
            try:
                await anext(frames)
                raise AssertionError("a frame came")
            except LookupError as e:
                assert str(e) == line, mode
            await p.close()

    asyncio.run(go())
