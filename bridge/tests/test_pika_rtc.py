"""pika_rtc.py: Pika's video over a WebRTC data channel, against a real aiortc peer on this Mac and the fake camera.

The peer plays the app: it offers one unordered ``video`` channel, sends the offer through the panel as the app
does, and reads frames as Shared/PikaDrive.swift does (8-byte stamp, then the JPEG)."""
from __future__ import annotations

import asyncio
import contextlib
import json
import time
from pathlib import Path
from typing import Any

import pytest
from test_link_chat import FakeInlet, serve, store_at
from test_pika import FakeCamera, jpeg, read_parts, settle

from eve_bridge import pika, pika_rtc
from eve_bridge.pika import Pika, PikaConfig

aiortc = pytest.importorskip("aiortc")


def car_with(camera: FakeCamera, **kw: Any) -> Pika:
    p = Pika(PikaConfig(host="127.0.0.1", video_port=camera.port), **kw)
    p.rtc = pika_rtc.PikaRtc(p, stun="")
    return p


class Phone:
    """The app's side: one peer, one unordered ``video`` channel, frames and lines as they arrive."""

    def __init__(self) -> None:
        self.pc = aiortc.RTCPeerConnection()
        self.channel = self.pc.createDataChannel("video", ordered=False, maxPacketLifeTime=150)
        self.frames: list[tuple[int, bytes]] = []
        self.lines: list[str] = []
        self.got = asyncio.Event()

        @self.channel.on("message")
        def _message(m: Any) -> None:
            if isinstance(m, bytes):
                self.frames.append(pika_rtc.parse_message(m))
            else:
                self.lines.append(m)
            self.got.set()

    async def connect(self, car: Pika) -> dict[str, Any]:
        await self.pc.setLocalDescription(await self.pc.createOffer())
        reply = await car.panel({"action": "rtc", "sdp": self.pc.localDescription.sdp})
        if reply.get("ok"):
            await self.pc.setRemoteDescription(aiortc.RTCSessionDescription(sdp=reply["sdp"], type="answer"))
        return reply

    async def wait_for(self, frames: int = 0, lines: int = 0, secs: float = 10.0) -> None:
        end = time.monotonic() + secs
        while len(self.frames) < frames or len(self.lines) < lines:
            self.got.clear()
            left = end - time.monotonic()
            assert left > 0, f"got {len(self.frames)} frames and {self.lines} in {secs} s"
            with contextlib.suppress(asyncio.TimeoutError):
                await asyncio.wait_for(self.got.wait(), left)

    async def close(self) -> None:
        await self.pc.close()


def test_frames_arrive_byte_for_byte_with_the_macs_stamp() -> None:
    async def go() -> None:
        camera = FakeCamera([jpeg(1), jpeg(2), jpeg(3)])
        await camera.start()
        car = car_with(camera)
        phone = Phone()
        before = pika.stamp_ms()
        reply = await phone.connect(car)
        assert reply["ok"] is True and reply["rtc"] is True and reply["sdp"].startswith("v=0")
        await phone.wait_for(frames=5)
        after = pika.stamp_ms()
        assert all(j in {jpeg(1), jpeg(2), jpeg(3)} for _, j in phone.frames)          # never re-encoded
        assert all(before <= s <= after for s, _ in phone.frames)
        assert car._video is not None and camera.viewers == 1
        await car.close()
        await phone.close()

    asyncio.run(go())


def test_the_slot_is_shared_with_the_tunnel_stream_and_the_newest_wins(tmp_path: Path) -> None:
    async def go() -> None:
        server, port, token = await serve(tmp_path, FakeInlet(store_at(tmp_path)))
        camera = FakeCamera([jpeg(1), jpeg(2)])
        await camera.start()
        car = car_with(camera)
        server.pika = car
        phone = Phone()
        await phone.connect(car)
        await phone.wait_for(frames=2)
        status, got, tunnel = await read_parts(port, token, 2)                     # the tunnel takes the camera
        assert status == 200 and len(got) >= 2
        await settle(0.2)
        assert not car.rtc.open and camera.ended == 1                              # the peer and its upstream ended
        again = Phone()
        await again.connect(car)                                                   # and WebRTC takes it back
        await again.wait_for(frames=2)
        await settle(0.2)
        assert camera.ended == 2 and car.rtc.open
        tunnel.close()
        await car.close()
        await phone.close()
        await again.close()
        await server.close()

    asyncio.run(go())


def test_close_ends_the_peer_and_the_camera_socket() -> None:
    async def go() -> None:
        camera = FakeCamera([jpeg(1)])
        await camera.start()
        car = car_with(camera)
        phone = Phone()
        await phone.connect(car)
        await phone.wait_for(frames=2)
        await car.panel({"action": "close"})                                       # the Pika screen closed
        await settle(0.3)
        assert not car.rtc.open and car._video is None and camera.ended == 1
        assert phone.pc.connectionState in ("closed", "failed", "disconnected") or \
            phone.channel.readyState in ("closing", "closed")
        await phone.close()

    asyncio.run(go())


def test_a_newer_offer_replaces_the_older_peer() -> None:
    async def go() -> None:
        camera = FakeCamera([jpeg(1)])
        await camera.start()
        car = car_with(camera)
        first, second = Phone(), Phone()
        await first.connect(car)
        await first.wait_for(frames=1)
        await second.connect(car)
        await second.wait_for(frames=2)
        n = len(first.frames)
        await settle(0.3)
        assert len(first.frames) == n                                              # the old peer gets nothing more
        await car.close()
        await first.close()
        await second.close()

    asyncio.run(go())


def test_a_camera_that_is_off_is_a_line_on_the_channel() -> None:
    async def go() -> None:
        async def refuse(host: str, p: int) -> Any:
            raise ConnectionRefusedError()

        car = Pika(PikaConfig(host="127.0.0.1"), opener=refuse)
        car.rtc = pika_rtc.PikaRtc(car, stun="")
        phone = Phone()
        assert (await phone.connect(car))["ok"] is True
        await phone.wait_for(lines=1)
        assert phone.lines[0] == pika.VIDEO_UNREACHABLE_LINE and phone.frames == []
        await car.close()
        await phone.close()

    asyncio.run(go())


def test_a_bad_offer_or_no_webrtc_sends_the_app_to_the_tunnel() -> None:
    async def go() -> None:
        car = Pika(PikaConfig(host="127.0.0.1"))
        assert await car.panel({"action": "rtc", "sdp": "v=0\r\n"}) == {"ok": False, "rtc": False, "line": ""}
        car.rtc = pika_rtc.PikaRtc(car, stun="")
        for sdp in (None, 42, "not sdp", "v=0" + "x" * 30000):
            assert await car.panel({"action": "rtc", "sdp": sdp}) == {"ok": False, "rtc": False, "line": ""}
        assert not car.rtc.open
        assert (await car.panel({"action": "status"}))["rtc"] is True

    asyncio.run(go())


def test_stats_are_logged_per_path_and_bad_ones_refused(caplog: pytest.LogCaptureFixture) -> None:
    async def go() -> None:
        car = Pika(PikaConfig(host="127.0.0.1"))
        caplog.set_level("INFO", logger="eve_bridge.pika")
        ok = {"action": "stats", "path": "webrtc", "n": 60, "p50": 41.4, "p95": 88, "fps": 11.2}
        assert await car.panel(ok) == {"ok": True, "line": ""}
        assert "pika: video stats webrtc n=60 p50=41 p95=88 fps=11.2" in caplog.text
        for bad in ({**ok, "path": "carrier pigeon"}, {**ok, "p50": "fast"}, {**ok, "n": True}, {**ok, "fps": 1e9}):
            assert (await car.panel(bad))["ok"] is False

    asyncio.run(go())


def test_the_config_turns_webrtc_off_and_checks_the_stun_server(tmp_path: Path) -> None:
    f = tmp_path / "pika.json"
    f.write_text(json.dumps({"host": "elegoo-car.local"}))
    cfg = pika.load(f)
    assert cfg is not None and cfg.rtc is True and cfg.stun == pika.DEFAULT_STUN
    f.write_text(json.dumps({"rtc": False, "stun": ""}))
    cfg = pika.load(f)
    assert cfg is not None and cfg.rtc is False and cfg.stun == ""
    f.write_text(json.dumps({"rtc": "yes", "stun": "turn:evil.example:3478?transport=tcp"}))
    cfg = pika.load(f)
    assert cfg is not None and cfg.rtc is True and cfg.stun == pika.DEFAULT_STUN
    f.write_text(json.dumps({"rtc": False}))
    assert pika.make_pika(f, {"EVE_PIKA": "1"}).rtc is None
    f.write_text(json.dumps({}))
    assert isinstance(pika.make_pika(f, {"EVE_PIKA": "1"}).rtc, pika_rtc.PikaRtc)


def test_the_frame_message_is_stamp_then_jpeg() -> None:
    m = pika_rtc.frame_message(1760100000123, jpeg(7))
    assert m[:8] == (1760100000123).to_bytes(8, "big") and m[8:] == jpeg(7)
    assert pika_rtc.parse_message(m) == (1760100000123, jpeg(7))
