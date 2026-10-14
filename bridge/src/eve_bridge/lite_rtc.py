"""eve lite's WebRTC: the web app's chat and push-to-talk voice, from a phone or laptop on the same Wi-Fi straight to
this Mac (docs/plans/eve-lite.md, M6).

The owner, 2026-10-13: "make the eve app a web app so that it can just be accessed from same wifi through web rtc".
The page (ios/EveLink/Web/src/web) offers one peer per device through ``POST /api/web/rtc`` (lite_web.py, behind
the device cookie), and ``RtcHub.answer`` answers it with every candidate in the answer, no trickle (as
pika_rtc.py does). There is no STUN or TURN server: ``RTCConfiguration(iceServers=[])``, because ``None`` makes
aiortc add Google's, and both ends are on the same LAN. One peer per device, the newest offer wins, at most
``MAX_PEERS``; a peer that has not connected ``CONNECT_SECS`` after its answer is closed.

* **Candidates.** A browser hides its address behind an mDNS name (``<uuid>.local``), and aioice resolves each with a
  single one-second query and drops the candidate when it gets no answer (aioice/mdns.py, ice.py); with no pair to
  check, ICE fails at once. So before the offer is applied, each such host candidate is pointed at the signaling
  request's own peer address when that is IPv4 and not loopback (``rewrite_local_candidates``). Peer-reflexive
  discovery stays as the backstop.
* **The ``link`` channel** carries the unchanged LinkChat (link_chat.py) as HTTP over messages (``LinkChannel``).
  The page sends ``{"t":"req","id":<u32>,"m":"GET|POST","p":<path>,"q":<query>,"h":{...},"b":<body>}`` and
  ``{"t":"cancel","id":n}``. The Mac answers with ``{"t":"head","id":n,"s":<status>,"h":{...}}``, binary frames
  (the id as 4 bytes big-endian, then at most ``BODY_FRAME_BYTES`` of the de-chunked body) and ``{"t":"end","id":n}``,
  or ``{"t":"err","id":n}`` when the answer broke off. Only ``/api/link/turn`` and ``/api/link/messages`` pass;
  every other path is a 404 here (uploads and files go over HTTPS). A request's body is at most ``MAX_BODY_BYTES``
  (an SCTP message is at most 64 KiB), and at most ``MAX_REQUESTS`` run at once on a channel. The door drops the
  page's ``Origin`` (LinkChat refuses any) and sets ``Authorization: Bearer <the peer's secret>``. The body is fed
  to a StreamReader that reaches EOF only on a cancel or when the channel closes: LinkChat reads EOF as the client
  hanging up, and a turn's lines nobody reads go elsewhere.
* **The ``call`` channel** and the peer's audio carry a push-to-talk call into the unchanged PhoneCalls
  (phone_call.py, ``serve_socket``), through ``RtcCallSocket``. Its hello is built here (``{"token": <the peer's
  secret>, "can": []}``), so a later call on the same peer needs no fresh token. The page's microphone track is
  resampled to 24 kHz mono 16-bit PCM and passed on in 100 ms blocks, only between ``talk`` and ``done``, with the
  300 ms before ``talk`` replayed after it (``talk`` can arrive after the first words, since SCTP and RTP race) and
  ``done`` held back 250 ms (SCTP can overtake the last RTP packets). eve's speech plays on the peer's audio track
  (``VoiceTrack``): 20 ms frames in real time, silence when nothing is queued. A ``said`` caption waits in the audio
  queue and goes out when its sentence starts to play; ``flush`` empties the queue; everything else goes at once.

aiortc is imported only when an offer comes, as in pika_rtc.py: without it the daemon starts, every offer is refused
with ``RtcError`` and the page chats over HTTPS. Logs carry counts and types, never what was said.
"""

from __future__ import annotations

import asyncio
import contextlib
import fractions
import hmac
import ipaddress
import json
import logging
import re
import secrets
import struct
import time
from collections import deque
from dataclasses import dataclass
from typing import Any, Awaitable, Callable, Optional, Union

from . import link_chat, phone_call
from .phone_call import OP_BIN, OP_TEXT, Closed

log = logging.getLogger(__name__)

# ---- signaling and peers --------------------------------------------------------------------------------------

MAX_SDP_CHARS = 20000                 # an offer longer than this is refused before aiortc reads it
MAX_PEERS = 4                         # one per device; past this the oldest peer is closed for the new one
CONNECT_SECS = 30.0                   # a peer not connected this long after its answer is closed
CLOSE_SECS = 5.0                      # how long a closing peer waits for aiortc, and for its call to end
LINK_LABEL = "link"
CALL_LABEL = "call"
NO_AUDIO = "This page connected without audio. Reload it to talk."

# ---- the link channel ----------------------------------------------------------------------------------------

LINK_PATHS = (link_chat.TURN_PATH, link_chat.MESSAGES_PATH)
MAX_REQUESTS = 8                      # requests running at once on one channel
MAX_BODY_BYTES = 60_000               # a request's body, UTF-8; the whole message must fit SCTP's 64 KiB
MAX_MESSAGE_CHARS = 1 << 20           # a message longer than this is not read at all
MAX_QUERY_CHARS = 2048
MAX_HEADERS = 32
MAX_HEADER_CHARS = 2048
MAX_HEAD_BYTES = 16 * 1024            # LinkChat's heads are a few hundred bytes
BODY_FRAME_BYTES = 16_384             # body bytes in one binary frame, after the 4-byte id
BUFFER_HIGH = 256 * 1024              # a writer's drain waits while more than this waits in the channel
DRAIN_POLL_SECS = 0.5                 # and looks again this often, woken or not
MAX_ID = 0xFFFFFFFF
HEADER_NAME_RE = re.compile(r"^[a-z0-9][a-z0-9-]{0,63}$")
# The page's headers that never reach LinkChat: the door sets its own authorization and length, and a browser's
# Origin would be refused (link_chat.py: any Origin is a web page).
DROPPED_REQUEST_HEADERS = frozenset({"origin", "authorization", "cookie", "host", "content-length",
                                     "transfer-encoding", "connection", "upgrade", "x-filename"})
DROPPED_RESPONSE_HEADERS = frozenset({"transfer-encoding", "connection", "keep-alive"})   # the body is de-chunked

# ---- the call --------------------------------------------------------------------------------------------------

SAMPLE_RATE = phone_call.SAMPLE_RATE                       # 24 kHz mono 16-bit, both ways, as phone_call.Call
VOICE_FRAME_SAMPLES = SAMPLE_RATE // 50                    # 20 ms: 480 samples
VOICE_FRAME_BYTES = VOICE_FRAME_SAMPLES * 2
BLOCK_BYTES = SAMPLE_RATE // 10 * 2                        # 100 ms of the microphone per binary message: 4800
PREROLL_BYTES = SAMPLE_RATE * 3 // 10 * 2                  # 300 ms kept before each talk, replayed after it
POSTROLL_SECS = 0.25                                       # how long a done waits for the last RTP packets
MAX_PRESS_BYTES = int(phone_call.MAX_PRESS_SECS * phone_call.BYTES_PER_SEC) + BLOCK_BYTES
MAX_VOICE_BYTES = 600 * phone_call.BYTES_PER_SEC           # speech queued past ten minutes is dropped
MAX_LAG_SECS = 0.2                                         # a voice track this far behind starts again from now
MAX_CONTROL_CHARS = 4096                                   # a call message longer than this is not a control


class RtcError(ValueError):
    """An offer this Mac will not or cannot answer. lite_web.py answers 400, and the page chats over HTTPS."""


def available() -> bool:
    """Whether aiortc loads. Not only ImportError: a native library of PyAV's that fails to load is an OSError, and
    either way the page chats over HTTPS instead."""
    try:
        import aiortc  # noqa: F401
    except Exception as e:  # noqa: BLE001
        log.warning("lite rtc: aiortc does not load (%s); the web app chats over HTTPS", type(e).__name__)
        return False
    return True


def configuration() -> Any:
    """The peers' configuration: no ICE server at all. ``iceServers=None`` would make aiortc add
    stun.l.google.com (RTCIceGatherer.getDefaultIceServers), and the phone and the Mac share a LAN."""
    from aiortc import RTCConfiguration

    return RTCConfiguration(iceServers=[])


# ---- candidates ------------------------------------------------------------------------------------------------

def peer_ipv4(addr: str) -> Optional[str]:
    """The signaling request's peer as a plain IPv4 address (an IPv4-mapped IPv6 one unwrapped), or None for an IPv6
    peer, a loopback one, or anything that is not an address."""
    try:
        ip: Any = ipaddress.ip_address(str(addr or "").strip().strip("[]").split("%", 1)[0])
    except ValueError:
        return None
    if isinstance(ip, ipaddress.IPv6Address):
        if ip.ipv4_mapped is None:
            return None
        ip = ip.ipv4_mapped
    if ip.is_loopback or ip.is_unspecified or ip.is_multicast:
        return None
    return str(ip)


def rewrite_local_candidates(sdp: str, peer_addr: str) -> tuple[str, int]:
    """The offer with each host candidate whose address is an mDNS name (``….local``) pointed at ``peer_addr``, the
    signaling request's peer, when that is IPv4 and not loopback → (the SDP, how many were rewritten). The port stays:
    it is the browser's own socket. Every other line, and each line ending, is kept as it was."""
    ip = peer_ipv4(peer_addr)
    if ip is None or ".local" not in sdp.lower():
        return sdp, 0
    lines = sdp.split("\n")
    rewritten = 0
    for i, line in enumerate(lines):
        if not line.startswith("a=candidate:"):
            continue
        body, end = (line[:-1], "\r") if line.endswith("\r") else (line, "")
        # a=candidate:<foundation> <component> <transport> <priority> <address> <port> typ <type> ...
        bits = body.split(" ")
        if len(bits) >= 8 and bits[6] == "typ" and bits[7] == "host" and bits[4].lower().endswith(".local"):
            bits[4] = ip
            lines[i] = " ".join(bits) + end
            rewritten += 1
    return "\n".join(lines), rewritten


def reliable(channel: Any) -> bool:
    """An ordered channel that retransmits until delivered: both channels' messages depend on their order."""
    return (bool(getattr(channel, "ordered", False)) and getattr(channel, "maxPacketLifeTime", None) is None
            and getattr(channel, "maxRetransmits", None) is None)


def _send(channel: Any, data: Union[str, bytes]) -> bool:
    """One message on a data channel → whether it was queued. A closed channel is False, never an error."""
    if getattr(channel, "readyState", "") != "open":
        return False
    try:
        channel.send(data)
    except Exception:  # noqa: BLE001 — aiortc's InvalidStateError: the channel closed under us
        return False
    return True


def _text(obj: dict[str, Any]) -> str:
    return json.dumps(obj, separators=(",", ":"))


# ---- the link channel: LinkChat's HTTP as messages -------------------------------------------------------------------

class _BadResponse(Exception):
    """What LinkChat wrote is not an HTTP/1.1 response this writer can frame."""


_HEAD, _LENGTH, _CHUNK_SIZE, _CHUNK_DATA, _CHUNK_END, _TRAILER, _UNTIL_CLOSE, _DONE = range(8)


class ChannelWriter:
    """What LinkChat writes one request's answer to. It calls only ``write`` and ``drain`` (link_chat.py), with
    HTTP/1.1: the status line and headers, then a body framed by Content-Length or chunked. Parsed as it comes: the
    head goes out as one ``head`` message, the body de-chunked as binary frames (the request's id, then at most
    ``BODY_FRAME_BYTES``), and its end as ``end``. A body that breaks off is ``err``. After a cancel or a closed
    channel every write raises ConnectionResetError, which LinkChat's stream takes as a hang-up."""

    def __init__(self, channel: "LinkChannel", rid: int) -> None:
        self.channel, self.id = channel, rid
        self._prefix = struct.pack(">I", rid)
        self.dead = False                 # cancelled, or the channel closed: nothing more goes out
        self.done = False                 # end or err went out
        self.status = 0
        self._buf = bytearray()
        self._state = _HEAD
        self._left = 0

    # -- what LinkChat calls --
    def write(self, data: bytes) -> None:
        if self.dead:
            raise ConnectionResetError("the page cancelled this request")
        if self.done:
            return                                          # nothing follows the end of an answer
        self._buf += data
        try:
            self._parse()
        except _BadResponse:
            log.warning("lite rtc: a link answer could not be framed; the request ends in err")
            self._finish("err")
            self.dead = True                                # and LinkChat's next write ends its stream
            raise ConnectionResetError("the answer could not be framed") from None

    async def drain(self) -> None:
        if self.dead:
            raise ConnectionResetError("the page cancelled this request")
        await self.channel.drained()
        if self.dead:
            raise ConnectionResetError("the page cancelled this request")

    # asyncio.StreamWriter's other calls, so a LinkChat that one day closes its writer still ends the answer
    def is_closing(self) -> bool:
        return self.dead or self.done

    def close(self) -> None:
        self.finish()
        self.dead = True

    async def wait_closed(self) -> None:
        return None

    def get_extra_info(self, name: str, default: Any = None) -> Any:
        return default

    # -- the request's end, seen from LinkChannel --
    def finish(self) -> None:
        """LinkChat returned: an answer whose length is the connection's ends now; one cut short is ``err``."""
        if self.done or self.dead:
            return
        if self._state == _UNTIL_CLOSE:
            self._finish("end")
        else:
            self._finish("err")

    def fail(self) -> None:
        if not self.done and not self.dead:
            self._finish("err")

    # -- the parser --
    def _parse(self) -> None:
        body = bytearray()
        while True:
            state = self._state
            if state == _HEAD:
                end = self._buf.find(b"\r\n\r\n")
                if end < 0:
                    if len(self._buf) > MAX_HEAD_BYTES:
                        raise _BadResponse("head too long")
                    break
                head = bytes(self._buf[:end])
                del self._buf[:end + 4]
                self._head(head)
            elif state in (_LENGTH, _CHUNK_DATA):
                take = min(self._left, len(self._buf))
                body += self._buf[:take]
                del self._buf[:take]
                self._left -= take
                if self._left:
                    break
                self._state = _DONE if state == _LENGTH else _CHUNK_END
            elif state == _CHUNK_SIZE:
                end = self._buf.find(b"\r\n")
                if end < 0:
                    if len(self._buf) > 1024:
                        raise _BadResponse("chunk size line too long")
                    break
                line = bytes(self._buf[:end]).split(b";", 1)[0].strip()
                del self._buf[:end + 2]
                try:
                    size = int(line, 16)
                except ValueError:
                    raise _BadResponse("bad chunk size") from None
                if size < 0:
                    raise _BadResponse("bad chunk size")
                self._left = size
                self._state = _CHUNK_DATA if size else _TRAILER
            elif state == _CHUNK_END:
                if len(self._buf) < 2:
                    break
                if self._buf[:2] != b"\r\n":
                    raise _BadResponse("a chunk without its CRLF")
                del self._buf[:2]
                self._state = _CHUNK_SIZE
            elif state == _TRAILER:
                end = self._buf.find(b"\r\n")
                if end < 0:
                    if len(self._buf) > MAX_HEAD_BYTES:
                        raise _BadResponse("trailer too long")
                    break
                line = bytes(self._buf[:end])
                del self._buf[:end + 2]
                if not line:
                    self._state = _DONE
            elif state == _UNTIL_CLOSE:
                body += self._buf
                self._buf.clear()
                break
            else:                                           # _DONE: anything after the answer is not part of it
                self._buf.clear()
                break
        if body:
            self._frames(body)
        if self._state == _DONE and not self.done:
            self._finish("end")

    def _head(self, raw: bytes) -> None:
        lines = raw.decode("latin-1").split("\r\n")
        top = lines[0].split(" ", 2)
        if len(top) < 2 or not top[0].startswith("HTTP/"):
            raise _BadResponse("no status line")
        try:
            status = int(top[1])
        except ValueError:
            raise _BadResponse("bad status") from None
        headers: dict[str, str] = {}
        for line in lines[1:]:
            name, sep, value = line.partition(":")
            if not sep:
                raise _BadResponse("bad header line")
            name, value = name.strip().lower(), value.strip()
            headers[name] = f"{headers[name]}, {value}" if name in headers else value
        chunked = "chunked" in headers.get("transfer-encoding", "").lower()
        if chunked:
            headers.pop("content-length", None)             # chunked wins (RFC 9112 6.3); the frames carry it all
        self.status = status
        self._send(_text({"t": "head", "id": self.id, "s": status,
                          "h": {k: v for k, v in headers.items() if k not in DROPPED_RESPONSE_HEADERS}}))
        if 100 <= status < 200 or status in (204, 304):
            self._state = _DONE
        elif chunked:
            self._state = _CHUNK_SIZE
        elif "content-length" in headers:
            try:
                self._left = int(headers["content-length"])
            except ValueError:
                raise _BadResponse("bad content-length") from None
            if self._left < 0:
                raise _BadResponse("bad content-length")
            self._state = _LENGTH if self._left else _DONE
        else:
            self._state = _UNTIL_CLOSE

    def _frames(self, body: bytearray) -> None:
        for i in range(0, len(body), BODY_FRAME_BYTES):
            self._send(self._prefix + bytes(body[i:i + BODY_FRAME_BYTES]))

    def _finish(self, kind: str) -> None:
        self.done = True
        self.channel.send(_text({"t": kind, "id": self.id}))

    def _send(self, data: Union[str, bytes]) -> None:
        """One message for this request. The channel gone: the request is dead and LinkChat's write raises."""
        if not self.channel.send(data):
            self.dead = True
            raise ConnectionResetError("the link channel closed")


@dataclass(eq=False)
class _Request:
    id: int
    reader: asyncio.StreamReader
    writer: ChannelWriter
    task: Optional[asyncio.Task[Any]] = None


def _request_id(raw: Any) -> Optional[int]:
    return raw if isinstance(raw, int) and not isinstance(raw, bool) and 0 <= raw <= MAX_ID else None


def _request_headers(raw: Any) -> Optional[dict[str, str]]:
    """The page's headers, lowercased, without the ones the door replaces or drops; None when malformed."""
    if raw is None:
        return {}
    if not isinstance(raw, dict) or len(raw) > MAX_HEADERS:
        return None
    out: dict[str, str] = {}
    for name, value in raw.items():
        if not isinstance(name, str) or not isinstance(value, str):
            return None
        name = name.strip().lower()
        if (not HEADER_NAME_RE.match(name) or len(value) > MAX_HEADER_CHARS
                or "\r" in value or "\n" in value or "\x00" in value):
            return None
        if name not in DROPPED_REQUEST_HEADERS:
            out[name] = value.strip()
    return out


class LinkChannel:
    """One ``link`` data channel: the page's HTTP requests, answered by the unchanged LinkChat (module docstring).
    ``token`` is the peer's secret: each request reaches LinkChat with ``Authorization: Bearer <token>`` and that as
    its only accepted token. ``served(path)`` is told of each request that reaches LinkChat (RtcHub's counts)."""

    def __init__(self, channel: Any, link: Any, token: str, *,
                 served: Optional[Callable[[str], None]] = None, max_requests: int = MAX_REQUESTS) -> None:
        self.channel, self.link = channel, link
        self._token = token.encode("utf-8")
        self._bearer = f"Bearer {token}"
        self.served = served
        self.max_requests = max_requests
        self.requests: dict[int, _Request] = {}
        self.closed = False
        self._low = asyncio.Event()
        with contextlib.suppress(Exception):
            channel.bufferedAmountLowThreshold = BUFFER_HIGH
        channel.on("bufferedamountlow", self._on_low)
        channel.on("message", self._on_message)
        channel.on("close", self.close)

    def check(self, token: str) -> bool:
        """LinkChat's token check: the peer's secret, compared in constant time."""
        return hmac.compare_digest(str(token).encode("utf-8"), self._token)

    # -- sending --
    def send(self, data: Union[str, bytes]) -> bool:
        return not self.closed and _send(self.channel, data)

    def _on_low(self) -> None:
        old, self._low = self._low, asyncio.Event()
        old.set()

    async def drained(self) -> None:
        """Wait while more than ``BUFFER_HIGH`` waits in the channel: a slow page holds LinkChat's writes back."""
        while not self.closed and getattr(self.channel, "bufferedAmount", 0) > BUFFER_HIGH:
            low = self._low
            with contextlib.suppress(asyncio.TimeoutError):
                await asyncio.wait_for(low.wait(), DRAIN_POLL_SECS)

    def _refuse(self, rid: int, status: int, error: str) -> None:
        """An answer of the door's own, in LinkChat's shape (a JSON body), for a request that never reaches it."""
        body = json.dumps({"ok": False, "error": error}).encode()
        if self.send(_text({"t": "head", "id": rid, "s": status,
                            "h": {"content-type": "application/json", "content-length": str(len(body)),
                                  "cache-control": "no-store"}})):
            self.send(struct.pack(">I", rid) + body)
            self.send(_text({"t": "end", "id": rid}))

    # -- receiving --
    def _on_message(self, message: Any) -> None:
        try:
            self._message(message)
        except Exception as e:  # noqa: BLE001 — never into aiortc's receive loop
            log.warning("lite rtc: a link message failed (%s)", type(e).__name__)

    def _message(self, message: Any) -> None:
        if self.closed:
            return
        if not isinstance(message, str) or len(message) > MAX_MESSAGE_CHARS:
            log.info("lite rtc: a link message that is not a request was dropped")
            return
        try:
            msg = json.loads(message)
        except ValueError:
            log.info("lite rtc: a link message that is not JSON was dropped")
            return
        if not isinstance(msg, dict):
            return
        if msg.get("t") == "req":
            self._request(msg)
        elif msg.get("t") == "cancel":
            rid = _request_id(msg.get("id"))
            if rid is not None:
                self.cancel(rid)

    def _request(self, msg: dict[str, Any]) -> None:
        rid = _request_id(msg.get("id"))
        if rid is None:
            log.info("lite rtc: a link request without an id was dropped")
            return
        if rid in self.requests:
            log.info("lite rtc: a link request reused a running id; dropped")
            return
        method, path, query = msg.get("m"), msg.get("p"), msg.get("q")
        if query is None:
            query = ""
        if not isinstance(path, str) or not isinstance(query, str):
            self._refuse(rid, 400, "p and q must be strings")
            return
        if "?" in path and not query:
            path, query = path.split("?", 1)                # the query in the path, as a URL has it
        query = query.removeprefix("?")
        if path not in LINK_PATHS:
            self._refuse(rid, 404, "not found")
            return
        method = method.upper() if isinstance(method, str) else ""
        if method not in ("GET", "POST"):
            self._refuse(rid, 405, "wrong method")
            return
        if len(query) > MAX_QUERY_CHARS:
            self._refuse(rid, 400, "the query is too long")
            return
        headers = _request_headers(msg.get("h"))
        if headers is None:
            self._refuse(rid, 400, f"h must be at most {MAX_HEADERS} string headers")
            return
        body = msg.get("b")
        if body is None or method == "GET":
            body = ""
        if not isinstance(body, str):
            self._refuse(rid, 400, "b must be a string")
            return
        data = body.encode("utf-8")
        if len(data) > MAX_BODY_BYTES:
            self._refuse(rid, 413, f"the body must be at most {MAX_BODY_BYTES} bytes here")
            return
        if len(self.requests) >= self.max_requests:
            self._refuse(rid, 429, f"at most {self.max_requests} requests at once on this channel")
            return
        headers["authorization"] = self._bearer
        if data:
            headers["content-length"] = str(len(data))
        reader = asyncio.StreamReader()
        if data:
            reader.feed_data(data)                          # and no EOF: that means the page hung up
        req = _Request(rid, reader, ChannelWriter(self, rid))
        self.requests[rid] = req
        req.task = asyncio.ensure_future(self._run(req, method, path, query, headers))

    async def _run(self, req: _Request, method: str, path: str, query: str, headers: dict[str, str]) -> None:
        if self.served is not None:
            self.served(path)
        try:
            await self.link.serve(method, path, query, headers, req.reader, req.writer, self.check)
        except Exception as e:  # noqa: BLE001 — the type only
            if not req.writer.dead:
                log.warning("lite rtc: a link request failed (%s)", type(e).__name__)
            req.writer.fail()
        else:
            req.writer.finish()
        finally:
            if self.requests.get(req.id) is req:
                del self.requests[req.id]

    def cancel(self, rid: int) -> None:
        """The page gave up on a request: LinkChat sees its client hang up (EOF), its writes stop, and the slot is
        free at once. Nothing more is sent for it."""
        req = self.requests.pop(rid, None)
        if req is not None:
            self._drop(req)

    @staticmethod
    def _drop(req: _Request) -> None:
        req.writer.dead = True
        req.reader.feed_eof()
        if req.task is not None and not req.task.done():
            req.task.cancel()

    def close(self) -> None:
        """The channel closed (or its peer did): every request is cancelled."""
        if self.closed:
            return
        self.closed = True
        requests, self.requests = list(self.requests.values()), {}
        for req in requests:
            self._drop(req)
        self._on_low()                                      # a drain waiting on the channel lets go
        with contextlib.suppress(Exception):
            self.channel.close()


# ---- eve's voice: the outbound audio track ----------------------------------------------------------------------

Marker = Callable[[], Any]


class VoiceTrack:
    """eve's voice on one peer: the call's 24 kHz mono PCM (phone_call.Call.read_out, through
    RtcCallSocket.send_bytes) as 20 ms frames of ``VOICE_FRAME_SAMPLES``, paced in real time, and silence while
    nothing is queued. A marker (``mark``) waits in the queue between the audio and fires when the audio after it is
    taken for a frame, that is when that audio starts to leave the Mac: speech is made faster than it plays, so a
    ``said`` caption sent with it would run ahead of the voice. ``flush`` drops the audio and the markers. aiortc's
    Opus encoder resamples the frames to 48 kHz stereo itself; ``media_track`` wraps this as aiortc's track."""

    def __init__(self, *, clock: Callable[[], float] = time.monotonic,
                 sleep: Callable[[float], Awaitable[Any]] = asyncio.sleep, max_bytes: int = MAX_VOICE_BYTES) -> None:
        self.clock, self.sleep, self.max_bytes = clock, sleep, max_bytes
        self._items: deque[Union[bytes, Marker]] = deque()
        self._offset = 0                  # how much of the first queued audio is already in a frame
        self._carry = b""                 # an odd byte, waiting for the rest of its sample
        self.queued = 0                   # bytes of audio waiting
        self.dropped = 0                  # bytes refused past max_bytes
        self.frames = 0
        self._pts: Optional[int] = None
        self._start = 0.0

    def push(self, pcm: bytes) -> None:
        data = self._carry + bytes(pcm)
        cut = len(data) - len(data) % 2
        self._carry = data[cut:]
        if not cut:
            return
        if self.queued + cut > self.max_bytes:
            if not self.dropped:
                log.warning("lite rtc: eve's speech is queued past %d s; the rest is dropped",
                            self.max_bytes // phone_call.BYTES_PER_SEC)
            self.dropped += cut
            return
        self._items.append(data[:cut])
        self.queued += cut

    def mark(self, fire: Marker) -> None:
        self._items.append(fire)

    def flush(self) -> None:
        self._items.clear()
        self._offset, self._carry, self.queued = 0, b"", 0

    def take(self) -> bytes:
        """The next frame's PCM: queued audio, then silence; each marker passed on the way fires."""
        out = bytearray()
        while len(out) < VOICE_FRAME_BYTES and self._items:
            item = self._items[0]
            if not isinstance(item, bytes):
                self._items.popleft()
                try:
                    item()
                except Exception as e:  # noqa: BLE001 — a caption that cannot go must not stop the voice
                    log.warning("lite rtc: a caption could not be sent (%s)", type(e).__name__)
                continue
            n = min(len(item) - self._offset, VOICE_FRAME_BYTES - len(out))
            out += item[self._offset:self._offset + n]
            self._offset += n
            self.queued -= n
            if self._offset == len(item):
                self._items.popleft()
                self._offset = 0
        if len(out) < VOICE_FRAME_BYTES:
            out += bytes(VOICE_FRAME_BYTES - len(out))
        return bytes(out)

    async def recv(self) -> Any:
        """The next frame (an av.AudioFrame), when it is due: the first at once, then one every 20 ms."""
        if self._pts is None:
            self._pts, self._start = 0, self.clock()
        else:
            self._pts += VOICE_FRAME_SAMPLES
            wait = self._start + self._pts / SAMPLE_RATE - self.clock()
            if wait > 0:
                await self.sleep(wait)
            elif wait < -MAX_LAG_SECS:
                # far behind (the loop stalled): from now, not a burst the page's jitter buffer would drop
                self._start = self.clock() - self._pts / SAMPLE_RATE
        self.frames += 1
        return self.frame(self.take(), self._pts)

    @staticmethod
    def frame(pcm: bytes, pts: int) -> Any:
        import av
        import numpy as np

        frame = av.AudioFrame.from_ndarray(np.frombuffer(pcm, dtype=np.int16).reshape(1, -1), format="s16",
                                           layout="mono")
        frame.sample_rate = SAMPLE_RATE
        frame.pts = pts
        frame.time_base = fractions.Fraction(1, SAMPLE_RATE)
        return frame


_MEDIA_TRACK: Optional[type] = None


def media_track(voice: VoiceTrack) -> Any:
    """``voice`` as aiortc's audio track, for ``RTCPeerConnection.addTrack``. aiortc is imported here, on the first
    offer, not when this module is."""
    global _MEDIA_TRACK
    if _MEDIA_TRACK is None:
        from aiortc.mediastreams import MediaStreamError, MediaStreamTrack

        class VoiceMediaTrack(MediaStreamTrack):
            kind = "audio"

            def __init__(self, source: VoiceTrack) -> None:
                super().__init__()
                self.source = source

            async def recv(self) -> Any:
                if self.readyState != "live":
                    raise MediaStreamError
                return await self.source.recv()

        _MEDIA_TRACK = VoiceMediaTrack
    return _MEDIA_TRACK(voice)


# ---- the page's microphone: the inbound audio track ---------------------------------------------------------------

class InboundAudio:
    """The page's microphone as the call hears it: each decoded frame of the peer's audio track (aiortc's Opus
    decoder gives 48 kHz stereo) through ``av.AudioResampler(format="s16", layout="mono", rate=24000)``, as raw
    PCM, the format phone_call.Call takes."""

    def __init__(self) -> None:
        self._resampler: Any = None

    def convert(self, frame: Any) -> bytes:
        import av

        if self._resampler is None:
            self._resampler = av.AudioResampler(format="s16", layout="mono", rate=SAMPLE_RATE)
        try:
            out = self._resampler.resample(frame)
        except ValueError:
            # the track changed format (another codec): a fresh resampler from this frame on
            self._resampler = av.AudioResampler(format="s16", layout="mono", rate=SAMPLE_RATE)
            out = self._resampler.resample(frame)
        return b"".join(f.to_ndarray().tobytes() for f in out if f.samples)


# ---- the call channel: PhoneCalls' socket ----------------------------------------------------------------------

class RtcCallSocket:
    """One call on a peer: the ``call`` data channel and the peer's audio, as the WebSocket that
    PhoneCalls.serve_socket drives (``recv``, ``send_json``, ``send_bytes``, ``close``, ``closed``).

    * ``recv``: first the hello, built here (``{"token": <the peer's secret>, "can": []}``); then the page's
      messages (``talk``, ``done``, ``end``, ``ping``) as text, and the microphone (``audio``, from the peer's
      track) as ``BLOCK_BYTES`` binary blocks, only between ``talk`` and ``done``. The ``PREROLL_BYTES`` heard
      just before a ``talk`` follow it; a ``done`` waits ``POSTROLL_SECS`` with the audio still arriving, then the
      press's last part block, then the ``done``.
    * ``send_json``: ``said`` waits in eve's voice queue as a marker (``VoiceTrack.mark``) and goes out when its
      sentence starts to play; ``flush`` empties that queue and goes out at once; everything else (``heard``,
      ``state``, ``ended``) goes out at once.
    * ``send_bytes``: eve's speech, queued on the peer's VoiceTrack.
    """

    def __init__(self, channel: Any, voice: VoiceTrack, token: str, *,
                 on_close: Optional[Callable[["RtcCallSocket"], None]] = None,
                 postroll_secs: float = POSTROLL_SECS) -> None:
        self.channel, self.voice = channel, voice
        self.on_close = on_close
        self.postroll_secs = postroll_secs
        self.closed = False
        self._hello: Optional[bytes] = _text({"token": token, "can": []}).encode("utf-8")
        self._inbox: asyncio.Queue[Optional[tuple[int, bytes]]] = asyncio.Queue()
        self._eof = False
        self.talking = False
        self._ring = bytearray()          # the last PREROLL_BYTES heard while not talking
        self._block = bytearray()         # the press's audio not yet a whole block
        self._press_bytes = 0
        self._done: Optional[bytes] = None
        self._post: Optional[asyncio.TimerHandle] = None
        channel.on("message", self._on_message)
        channel.on("close", self._on_channel_close)

    # -- the WebSocket PhoneCalls drives --
    async def recv(self) -> tuple[int, bytes]:
        if self._hello is not None:
            hello, self._hello = self._hello, None
            return OP_TEXT, hello
        if self._eof:
            raise Closed("the call channel closed")
        item = await self._inbox.get()
        if item is None:
            self._eof = True
            raise Closed("the call channel closed")
        return item

    async def send_json(self, obj: dict[str, Any]) -> None:
        if self.closed:
            raise Closed("the call channel closed")
        kind = obj.get("type")
        text = _text(obj)
        if kind == "said":
            self.voice.mark(lambda: self._send(text))      # with its sentence's first sound
            return
        if kind == "flush":
            self.voice.flush()
        if not self._send(text):
            raise Closed("the call channel closed")

    async def send_bytes(self, data: bytes) -> None:
        if self.closed:
            raise Closed("the call channel closed")
        self.voice.push(data)

    async def close(self, code: int = 1000) -> None:
        self.shut()

    # -- the page's side --
    def audio(self, pcm: bytes) -> None:
        """Microphone PCM from the peer's track (24 kHz mono 16-bit): the press's while talking, else the ring."""
        if self.closed or not pcm:
            return
        if not self.talking:
            self._ring += pcm
            over = len(self._ring) - PREROLL_BYTES
            if over > 0:
                del self._ring[:over]
            return
        self._block += pcm
        while len(self._block) >= BLOCK_BYTES:
            block = bytes(self._block[:BLOCK_BYTES])
            del self._block[:BLOCK_BYTES]
            if self._press_bytes < MAX_PRESS_BYTES:       # the call keeps MAX_PRESS_SECS of a press; so does this
                self._press_bytes += BLOCK_BYTES
                self._put(OP_BIN, block)

    def _on_message(self, message: Any) -> None:
        try:
            self._message(message)
        except Exception as e:  # noqa: BLE001 — never into aiortc's receive loop
            log.warning("lite rtc: a call message failed (%s)", type(e).__name__)

    def _message(self, message: Any) -> None:
        if self.closed or not isinstance(message, str) or len(message) > MAX_CONTROL_CHARS:
            return                                          # the microphone is the audio track, never a message
        raw = message.encode("utf-8")
        try:
            msg = json.loads(message)
        except ValueError:
            msg = None
        kind = msg.get("type") if isinstance(msg, dict) else None
        if kind == "talk":
            self._release()                                 # a done still in its post-roll is the last press's end
            self._put(OP_TEXT, raw)
            if not self.talking:
                self.talking, self._press_bytes = True, 0
                pre, self._ring = self._ring, bytearray()
                self.audio(bytes(pre))                      # the pre-roll: what was heard just before the press
        elif kind == "done":
            if not self.talking:
                self._put(OP_TEXT, raw)                     # a stray done: the call ignores it, as before
            elif self._post is None:
                self._done = raw
                self._post = asyncio.get_running_loop().call_later(self.postroll_secs, self._release)
        elif kind == "end":
            self._release()
            self._put(OP_TEXT, raw)
        else:
            self._put(OP_TEXT, raw)                         # ping and anything else, as the page sent it

    def _release(self) -> None:
        """The post-roll is over (or cut short): the press's last part block, then its done."""
        if self._post is None:
            return
        self._post.cancel()
        self._post = None
        tail = bytes(self._block[:len(self._block) - len(self._block) % 2])
        self._block = bytearray()
        if tail and self._press_bytes < MAX_PRESS_BYTES:
            self._put(OP_BIN, tail)
        self.talking = False
        done, self._done = self._done, None
        if done is not None:
            self._put(OP_TEXT, done)

    def _put(self, op: int, data: bytes) -> None:
        if not self.closed:
            self._inbox.put_nowait((op, data))

    def _send(self, text: str) -> bool:
        return not self.closed and _send(self.channel, text)

    # -- the end --
    def shut(self) -> None:
        """This side ends the call's socket: what the page sent and the call did not read yet is dropped."""
        if self._end(keep=False):
            with contextlib.suppress(Exception):
                self.channel.close()

    def _on_channel_close(self) -> None:
        self._end(keep=True)                                # the page closed it: what it sent first is still read

    def _end(self, keep: bool) -> bool:
        if self.closed:
            return False
        self.closed = True
        if self._post is not None:
            self._post.cancel()
            self._post = None
        self.voice.flush()
        if not keep:
            while not self._inbox.empty():
                self._inbox.get_nowait()
        self._inbox.put_nowait(None)
        if self.on_close is not None:
            self.on_close(self)
        return True


# ---- one peer, and the hub ---------------------------------------------------------------------------------------

class _Peer:
    """One device's WebRTC peer: its connection, its secret, its ``link`` and ``call`` channels (the newest of each
    wins), eve's voice on its audio track, and the pump that hears its microphone."""

    def __init__(self, hub: "RtcHub", device_id: str) -> None:
        self.hub, self.device_id = hub, device_id
        self.secret = secrets.token_urlsafe(32)
        self.pc: Any = None
        self.voice = VoiceTrack()
        self.voice_on = False
        self.connected = False
        self.closed = False
        self.link: Optional[LinkChannel] = None
        self.call: Optional[RtcCallSocket] = None
        self._tasks: set[asyncio.Task[Any]] = set()       # the microphone pump and the connect deadline
        self._calls: set[asyncio.Task[Any]] = set()       # PhoneCalls.serve_socket, one per call channel

    def check(self, token: str) -> bool:
        return hmac.compare_digest(str(token).encode("utf-8"), self.secret.encode("utf-8"))

    def _spawn(self, coro: Awaitable[Any], into: set[asyncio.Task[Any]]) -> None:
        task = asyncio.ensure_future(coro)
        into.add(task)
        task.add_done_callback(into.discard)

    async def start(self, offer: str) -> str:
        """Apply the offer and answer it, with every candidate in the answer."""
        from aiortc import RTCPeerConnection, RTCSessionDescription

        pc = self.pc = RTCPeerConnection(configuration())
        pc.on("datachannel", self._on_channel)
        pc.on("connectionstatechange", self._on_state)
        await pc.setRemoteDescription(RTCSessionDescription(sdp=offer, type="offer"))
        if pc.sctp is None:
            # the page makes its link channel before the offer; aiortc would answer anything, even no media at all
            raise RtcError("the offer has no data channel")
        audio = next((t for t in pc.getTransceivers() if t.kind == "audio"), None)
        for t in pc.getTransceivers():
            if t is not audio:
                t.direction = "inactive"                    # only the voice's audio flows, and never video
        if audio is not None and self.hub.voice_ready():
            pc.addTrack(media_track(self.voice))            # onto the page's sendrecv transceiver: no renegotiation
            self.voice_on = True
            mic = audio.receiver.track
            if mic is not None:
                self._spawn(self._hear(mic), self._tasks)
        elif audio is not None:
            audio.direction = "inactive"
        await pc.setLocalDescription(await pc.createAnswer())     # gathers every candidate first
        self._spawn(self._deadline(), self._tasks)
        return pc.localDescription.sdp

    async def _hear(self, track: Any) -> None:
        """Read the page's microphone for as long as the track lives, so nothing piles up in aiortc's queue; a call
        socket gets it as 24 kHz PCM."""
        inbound = InboundAudio()
        while True:
            try:
                frame = await track.recv()
            except Exception:  # noqa: BLE001 — MediaStreamError: the track ended
                return
            call = self.call
            if call is None or call.closed:
                continue
            try:
                pcm = inbound.convert(frame)
            except Exception as e:  # noqa: BLE001 — one frame lost, never the pump
                log.info("lite rtc: a microphone frame could not be read (%s)", type(e).__name__)
                continue
            call.audio(pcm)

    async def _deadline(self) -> None:
        await asyncio.sleep(self.hub.connect_secs)
        if not self.closed and not self.connected:
            self.hub._forget(self, "it did not connect")

    def _on_state(self) -> None:
        pc = self.pc
        if pc is None or self.closed:
            return
        state = pc.connectionState
        if state == "connected" and not self.connected:
            self.connected = True
            log.info("lite rtc: a peer connected (voice %s)", "on" if self.voice_on else "off")
        elif state in ("failed", "closed"):
            self.hub._forget(self, f"its connection {state}")

    def _on_channel(self, channel: Any) -> None:
        try:
            self._channel(channel)
        except Exception as e:  # noqa: BLE001 — never into aiortc's receive loop
            log.warning("lite rtc: a data channel could not be taken (%s)", type(e).__name__)
            with contextlib.suppress(Exception):
                channel.close()

    def _channel(self, channel: Any) -> None:
        label = getattr(channel, "label", "")
        if self.closed or label not in (LINK_LABEL, CALL_LABEL) or not reliable(channel):
            log.info("lite rtc: a data channel was refused (%s)",
                     label if label in (LINK_LABEL, CALL_LABEL) else "an unknown label")
            channel.close()
            return
        if label == LINK_LABEL:
            old, self.link = self.link, LinkChannel(channel, self.hub.link, self.secret, served=self.hub._served)
            if old is not None:
                old.close()
            return
        calls = self.hub.calls
        if calls is None:
            channel.close()
            return
        old_call = self.call
        if old_call is not None:
            old_call.shut()                                 # a new call on this peer: the old one ends first
        if not self.voice_on and self.hub.voice_ready():
            _send(channel, _text({"type": "ended", "reason": NO_AUDIO}))
            channel.close()
            return
        sock = self.call = RtcCallSocket(channel, self.voice, self.secret, on_close=self._call_closed)
        self._spawn(self._serve_call(calls, sock), self._calls)

    async def _serve_call(self, calls: Any, sock: RtcCallSocket) -> None:
        self.hub.counts["calls"] += 1
        log.info("lite rtc: a call opened on a peer")
        try:
            await calls.serve_socket(sock, self.check)
        except Exception as e:  # noqa: BLE001 — the type only
            log.warning("lite rtc: a call ended with an error (%s)", type(e).__name__)
        finally:
            sock.shut()

    def _call_closed(self, sock: RtcCallSocket) -> None:
        if self.call is sock:
            self.call = None

    async def close(self) -> None:
        """End everything on this peer: requests are cancelled, the call ends (PhoneCalls sees it drop), the
        connection closes."""
        if self.closed:
            return
        self.closed = True
        if self.link is not None:
            self.link.close()
        if self.call is not None:
            self.call.shut()
        self.voice.flush()
        me = asyncio.current_task()
        for task in self._tasks:
            if task is not me:
                task.cancel()
        pc, self.pc = self.pc, None
        if pc is not None:
            with contextlib.suppress(Exception):
                await asyncio.wait_for(pc.close(), CLOSE_SECS)
        pending = [t for t in (*self._tasks, *self._calls) if t is not me and not t.done()]
        if pending:
            _, late = await asyncio.wait(pending, timeout=CLOSE_SECS)
            for task in late:
                task.cancel()


class RtcHub:
    """Every device's peer (M11: ``RtcHub(link, calls)``). ``link`` is the daemon's one LinkChat; ``calls`` its
    PhoneCalls, or None when calls are off. lite_web.py answers ``POST /api/web/rtc`` with ``answer`` and closes a
    revoked device's peer with ``close_device``; the daemon calls ``close`` when it stops."""

    def __init__(self, link: Any, calls: Any, *, max_peers: int = MAX_PEERS,
                 connect_secs: float = CONNECT_SECS) -> None:
        self.link, self.calls = link, calls
        self.max_peers, self.connect_secs = max_peers, connect_secs
        self.peers: dict[str, _Peer] = {}                 # by device id, oldest first
        self.closed = False
        self.counts: dict[str, int] = {"answers": 0, "link_requests": 0, "link_turns": 0, "calls": 0}
        self._lock = asyncio.Lock()
        self._closing: set[asyncio.Task[Any]] = set()

    def voice_ready(self) -> bool:
        """Whether this Mac can take a call: PhoneCalls with the chat and a voice (an OpenRouter key)."""
        calls = self.calls
        return (calls is not None and getattr(calls, "voice", None) is not None
                and getattr(calls, "brain", None) is not None)

    def stats(self) -> dict[str, int]:
        """Counts for status pages and tests: peers, connected peers, answers, link requests and turns, calls."""
        return {"peers": len(self.peers), "connected": sum(1 for p in self.peers.values() if p.connected),
                **self.counts}

    def _served(self, path: str) -> None:
        self.counts["link_requests"] += 1
        if path == link_chat.TURN_PATH:
            self.counts["link_turns"] += 1

    async def answer(self, device_id: str, sdp: str, peer_addr: str) -> dict[str, Any]:
        """``POST /api/web/rtc``: the answer to a device's offer → ``{"sdp": <answer>, "voice": <whether the call
        can be used>}``. The device's previous peer is closed (the newest offer wins), and the oldest other peer
        when ``max_peers`` are open. ``peer_addr`` is the signaling request's peer address (the candidate
        rewrite). Raises RtcError for an offer that is not answered."""
        if not isinstance(device_id, str) or not device_id:
            raise RtcError("no device")
        if not isinstance(sdp, str) or not sdp.strip() or len(sdp) > MAX_SDP_CHARS:
            raise RtcError(f"the offer must be SDP of at most {MAX_SDP_CHARS} characters")
        if not available():
            raise RtcError("WebRTC is not available on this Mac")
        offer, rewritten = rewrite_local_candidates(sdp, peer_addr)
        async with self._lock:
            if self.closed:
                raise RtcError("eve is stopping")
            old = self.peers.pop(device_id, None)
            if old is not None:
                self._retire(old)
            while len(self.peers) >= self.max_peers:
                self._retire(self.peers.pop(next(iter(self.peers))))
            peer = self.peers[device_id] = _Peer(self, device_id)
            try:
                answer = await peer.start(offer)
            except BaseException as e:
                if self.peers.get(device_id) is peer:
                    del self.peers[device_id]
                self._retire(peer)
                if not isinstance(e, Exception):
                    raise
                log.info("lite rtc: an offer could not be answered (%s)", type(e).__name__)
                if isinstance(e, RtcError):
                    raise
                raise RtcError("that offer could not be answered") from e
        self.counts["answers"] += 1
        log.info("lite rtc: answered a device's offer (%d of %d peers, voice %s, %d mDNS candidate(s) pointed at "
                 "its address)", len(self.peers), self.max_peers, "on" if peer.voice_on else "off", rewritten)
        return {"sdp": answer, "voice": peer.voice_on}

    async def close_device(self, device_id: str) -> None:
        """A revoked device: its peer closes now, with its requests and its call."""
        async with self._lock:
            peer = self.peers.pop(device_id, None)
        if peer is not None:
            await peer.close()

    async def close(self) -> None:
        """The daemon is stopping: every peer closes."""
        self.closed = True
        peers, self.peers = list(self.peers.values()), {}
        closing = [p.close() for p in peers]
        await asyncio.gather(*closing, *self._closing, return_exceptions=True)

    def _forget(self, peer: _Peer, why: str) -> None:
        """A peer that failed, closed or never connected leaves (if it is still its device's)."""
        if self.peers.get(peer.device_id) is peer:
            del self.peers[peer.device_id]
        if not peer.closed:
            log.info("lite rtc: a peer closed: %s", why)
        self._retire(peer)

    def _retire(self, peer: _Peer) -> None:
        """Close a peer in the background: a new offer is not kept waiting on an old connection's goodbye."""
        task = asyncio.ensure_future(peer.close())
        self._closing.add(task)
        task.add_done_callback(self._closing.discard)
