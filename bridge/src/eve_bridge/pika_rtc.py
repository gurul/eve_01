"""Pika's video over WebRTC: the car's JPEGs straight from this Mac to the phone, past the tunnel (docs/pika.md).

Owner, 2026-10-10: "can we setup webrtc so that the camera stream is lower latency". Measured that day from the Mac,
the tunnel stream's first frame took 0.79 s against 0.14 s from the camera direct, its frames came in bursts (gap p95
564 ms against 209 ms) and a request's round trip through it was 145 ms median. On the home Wi-Fi WebRTC connects the
phone to this Mac directly; off it, through each side's public address (STUN). It never relays through a server: when
neither works the app falls back to the tunnel stream (pika.Pika.stream).

How. The app offers one data channel labelled ``video``, unordered and with a short packet lifetime, so a lost frame
is skipped rather than holding up the ones behind it. The offer goes through the link's own authenticated control
(``{"action": "rtc", "sdp": <offer>}``, pika.Pika.panel), and the answer comes back in the reply with every candidate
in it (no trickle). Each binary message is one frame: the Mac's send stamp in ms (8 bytes, big-endian) and the
camera's JPEG exactly as it came, never re-encoded, so WebRTC adds no encoder delay. A text message is a line for the
owner (the camera is off, or busy). Frames are dropped, never queued, while the channel's buffer is behind.

The camera serves one viewer. This takes the same slot as the tunnel stream (newest wins), and gives it back when
the channel or the peer closes, when the panel closes, or when a tunnel stream or a newer offer takes over. While the
channel is open and the camera can't be reached, it tries again every RETRY_SECS, so a car switched on later
appears by itself.

aiortc is imported only when an offer comes; without it the daemon starts and the app uses the tunnel stream.
"""

from __future__ import annotations

import asyncio
import contextlib
import logging
import struct
from typing import Any, Optional

from . import pika as pika_mod

log = logging.getLogger(__name__)

HEADER = struct.Struct(">Q")          # the Mac's send stamp, ms since the epoch
MAX_MESSAGE = 65536                   # aiortc's SCTP maximum message size (RTCSctpTransport.getCapabilities)
MAX_JPEG = MAX_MESSAGE - HEADER.size
BUFFER_LIMIT = 96 * 1024              # bytes waiting in the channel: past this, a frame is dropped
OPEN_SECS = 10.0                      # the channel must open this long after the answer
RETRY_SECS = 3.0                      # the camera could not be reached: try again after this
MAX_SDP_CHARS = 20000


def frame_message(stamp: int, jpeg: bytes) -> bytes:
    return HEADER.pack(stamp) + jpeg


def parse_message(data: bytes) -> tuple[int, bytes]:
    """The stamp and the JPEG of one frame message (the tests read frames with it, as the app does)."""
    return HEADER.unpack_from(data)[0], data[HEADER.size:]


def available() -> bool:
    try:
        import aiortc  # noqa: F401
    except ImportError:
        return False
    return True


class PikaRtc:
    """One WebRTC peer at a time for the Pika screen's video. Lives on the daemon's event loop."""

    def __init__(self, pika: "pika_mod.Pika", stun: str = "") -> None:
        self.pika = pika
        self.stun = stun
        self._pc: Any = None
        self._sender: Optional[asyncio.Task] = None
        self.sent = 0
        self.dropped = 0

    @property
    def open(self) -> bool:
        return self._pc is not None

    async def answer(self, offer_sdp: str) -> str:
        """Answer the app's offer. Ends the peer and the video before it: the newest wins."""
        from aiortc import RTCConfiguration, RTCIceServer, RTCPeerConnection, RTCSessionDescription

        await self.close()
        servers = [RTCIceServer(urls=self.stun)] if self.stun else []
        pc = RTCPeerConnection(RTCConfiguration(iceServers=servers))
        self._pc = pc

        @pc.on("datachannel")
        def _channel(channel: Any) -> None:
            if channel.label != "video" or self._pc is not pc:
                channel.close()
                return
            if self._sender is not None:
                self._sender.cancel()
            self._sender = asyncio.ensure_future(self._send(pc, channel))

        @pc.on("connectionstatechange")
        async def _state() -> None:
            log.info("pika: webrtc %s", pc.connectionState)
            if pc.connectionState in ("failed", "closed") and self._pc is pc:
                await self.close()

        try:
            await pc.setRemoteDescription(RTCSessionDescription(sdp=offer_sdp, type="offer"))
            await pc.setLocalDescription(await pc.createAnswer())     # gathers every candidate first
        except Exception:
            if self._pc is pc:
                await self.close()
            raise
        return pc.localDescription.sdp

    async def _send(self, pc: Any, channel: Any) -> None:
        """The camera's frames into the channel until it closes."""
        if channel.readyState != "open":
            opened = asyncio.Event()
            channel.on("open", opened.set)
            try:
                await asyncio.wait_for(opened.wait(), OPEN_SECS)
            except asyncio.TimeoutError:
                log.info("pika: webrtc channel never opened")
                return
        await self.pika._take_video(keep_rtc=True)
        log.info("pika: video on (webrtc)")
        try:
            while channel.readyState == "open" and self._pc is pc:
                frames = self.pika.frames()
                try:
                    async for jpeg in frames:
                        if channel.readyState != "open":
                            break
                        if len(jpeg) > MAX_JPEG or channel.bufferedAmount > BUFFER_LIMIT:
                            self.dropped += 1
                            continue
                        channel.send(frame_message(pika_mod.stamp_ms(), jpeg))
                        self.sent += 1
                except pika_mod.PikaError as e:
                    if channel.readyState == "open":
                        channel.send(str(e))
                finally:
                    await frames.aclose()
                if channel.readyState == "open":
                    await asyncio.sleep(RETRY_SECS)
        finally:
            self.pika._left_video()

    async def close(self) -> None:
        """End the peer and give back the camera."""
        pc, self._pc = self._pc, None
        sender, self._sender = self._sender, None
        if sender is not None and sender is not asyncio.current_task() and not sender.done():
            sender.cancel()
            with contextlib.suppress(BaseException):
                await sender
        if pc is not None:
            with contextlib.suppress(Exception):
                await pc.close()
