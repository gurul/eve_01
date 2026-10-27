"""Talk to any running Claude Code session on this Mac through Claude Code's own cross-session messaging.

Every interactive Claude Code session (in Warp, Terminal, an IDE...) registers itself in
``~/.claude/sessions/<pid>.json`` (``sessionId``, ``cwd``, ``name``, ``status``, ``kind``, ``peerProtocol``,
``peerFeatures``, ``messagingSocketPath``) and listens on that socket. A message sent there reaches the session's
Claude as a ``<cross-session-message>``: an idle session wakes and acts on it, a busy one takes it at its next tool
round. This is how the phone's Work tab talks to a session without forking it or opening a window.

The wire (protocol 1, read out of Claude Code 2.1.311; docs/claude-peer.md quotes the code and the live proof):

- A session listens on a Unix socket, ``/tmp/cc-socks/<pid>.sock`` (0600, in a 0700 folder). A sender connects,
  writes newline-delimited JSON, waits 150 ms, closes its side, and the session closes back. Nothing else comes back
  on that connection.
- The first line may be ``{"type":"auth","token":...}``, the token from the session's key file
  ``~/.claude/sessions/<pid>.<sha256 of the socket path>.key``. Only Windows requires it; Claude Code sends it
  whenever the key file is there, and so does eve.
- Then the message: ``{"msgV":1,"msg_id":<uuid>,"type":"user","message":{"role":"user","content":<envelope>},
  "priority":"next","from":<eve's address>,"session_id":<the session's id>}``. The envelope is
  ``<cross-session-message from="uds:..." from-name="eve-phone">`` + newline + the words + newline + the end tag.
  The session drops a message whose ``session_id`` is not its own, so the words never land in another conversation.
- An accepted message gets no answer at all. A held, refused or dropped one gets a ``peer_message_status`` line, sent
  to the ``from`` address in a new connection: that is what eve's ``Inbox`` is for, and why ``send`` waits a moment
  (``RECEIPT_WAIT``) before it says "delivered".

eve says who it is and how it runs: ``from-name`` is ``EVE_NAME`` and ``from-mode`` is ``EVE_MODE``, "bypass", the
class of the Work sessions eve itself launches with ``--dangerously-skip-permissions`` (owner, 2026-10-26: phone
messages go straight in). A session that runs without asking takes them at once; one that prompts holds them for its
user's approval on the Mac ("mode-mismatch"), and the phone says so. The mode is fixed: eve never picks it to match
the receiver.
"""

from __future__ import annotations

import asyncio
import atexit
import contextlib
import errno
import hashlib
import inspect
import json
import logging
import os
import re
import secrets
import socket
import stat
import struct
import subprocess
import sys
import time
import unicodedata
import uuid
from collections import OrderedDict
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Callable, Optional

log = logging.getLogger(__name__)

SESSIONS_DIR = Path.home() / ".claude" / "sessions"
EVE_NAME = "eve-phone"          # the from-name a session sees on eve's messages
EVE_MODE = "bypass"             # the from-mode: eve's permission class, never chosen per receiver
# The first line of every message eve relays: whose words these are. The session's Claude is told the message came
# from another session, so this line says it is the owner typing on the phone.
OWNER_LINE = "From the owner, typed on their phone in eve's Work tab:"

PROTOCOLS = (1,)                 # the peerProtocol versions this module speaks
SOCKETS_DIR = Path("/tmp/cc-socks")   # where Claude Code's sessions listen, and eve's Inbox with them
TAG = "cross-session-message"
LINE_CAP = 1_048_576             # a session drops a connection whose unread text passes this many characters
KEY_CAP = 4096                   # the largest key file Claude Code reads
RECORD_CAP = 65_536              # the largest registry file read here (a real one is under 1 KB)
END_DELAY = 0.15                 # Claude Code's sender waits this long on macOS before closing its side
RECEIPT_WAIT = 1.0               # how long send waits for a held / refused / dropped answer
FIRST_LINE_WAIT = 30.0           # the Inbox closes a connection that sends no line in this time
PACE_BURST = 30                  # messages to one session in a burst, then PACE_REFILL a second (its rate limit)
PACE_REFILL = 0.5

_AUTH_LINE_LEN = len(json.dumps({"type": "auth", "token": "0" * 32}, separators=(",", ":"))) + 1
_TOKEN = re.compile(r"^[0-9a-f]{32}$")
_RECORD_NAME = re.compile(r"^(\d+)\.json$")
_MSG_ID = re.compile(r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$", re.I)
_ADDRESS = re.compile(r"^uds:[A-Za-z0-9%:_/.\\\-]{1,200}$")
_PATH_UNSAFE = re.compile(r"[^A-Za-z0-9:_/.\\\-]")

# The envelope exactly as a session parses it (attributes in this order, nothing else), so the session credits the
# message to its sender only when it is one well-formed envelope.
_ENVELOPE = re.compile(
    r'^<cross-session-message(?: from="([A-Za-z0-9%:_/.\\\-]+)")?(?: from-session="([A-Za-z0-9_-]{1,80})")?'
    r'(?: hop-chain="([0-9a-f]{24}(?:,[0-9a-f]{24}){0,31})")?(?: from-name="([^"<>\n\r]+)")?'
    r'(?: from-mode="(bypass|prompting)")?(?: from-plugin="([^"<>\n\r]+)")?>\n([\s\S]*)\n</cross-session-message>\Z')

# A closing tag hidden in the words would end the envelope early. Claude Code turns the "<" of anything that reads
# as </cross-session-message> (look-alike brackets and slashes, invisible characters between the letters, any case)
# into "<\", then checks that doing it again changes nothing. This catches at least every case it does (more is
# harmless: an escaped "<" is never escaped twice), so that check holds.
# Its pattern is JavaScript's case-insensitive /iu, where a letter of the tag is that ASCII letter in either case, or
# the long s (\u017f) for an s, and nothing else. Python's re.I would also read the Turkish dotted and dotless i as
# letters and stop at them where Claude Code steps over them, so the letters are spelled out and re.I is not used.
_OPENERS = "<\uff1c\ufe64\u2329\u27e8\u3008\u2039\u02c2\u1438\u276c\u276e\u2770\u29fc\u226e\u227a\u22d6"
_SLASHES = "/\uff0f\u2215\u2044"
_GAP = "[^A-Za-z0-9\u017f]*+"   # never a letter of the tag


def _tag_letter(c: str) -> str:
    return f"[{c}{c.upper()}\u017f]" if c == "s" else f"[{c}{c.upper()}]"


_CLOSING = re.compile(
    f"[{_OPENERS}](?!\\\\)(?=[^A-Za-z0-9\u017f{_SLASHES}]*+[{_SLASHES}]{_GAP}"
    + _GAP.join(_tag_letter(c) for c in "crosssessionmessage") + ")")


@dataclass(frozen=True)
class Peer:
    pid: int
    session_id: str
    cwd: str
    name: str
    status: str                   # "busy" | "idle" | whatever the registry says ("shell": working too, a tool runs;
                                  # "waiting": on its user)
    kind: str                     # "interactive" | ...
    socket_path: str
    version: str
    protocol: int
    features: tuple[str, ...] = ()


@dataclass(frozen=True)
class Delivery:
    # "delivered": in the session's inbox (it wakes, or takes it at its next tool round)
    # "held": the session holds it for approval on the Mac   "refused": the session refused it
    # "gone": no such live session / stale socket   "unsupported": this session cannot take messages
    # "error": anything else (timeout, too large...)
    state: str
    detail: str = ""             # one plain sentence the phone can show
    msg_id: str = ""             # the id of a message that went out, which a later answer (on_receipt) names

    @property
    def ok(self) -> bool:
        return self.state in ("delivered", "held")


# ---- what the phone is told -----------------------------------------------------------------------------------------

DELIVERED = "Sent. The session reads it now, or at its next step if it is working."
HELD = ("Waiting for approval on the Mac: this session holds messages from other programs until you allow them "
        "there.")
REFUSED = "This session does not take messages from other programs (a setting there refuses them)."
DENIED = "Declined on the Mac, so the session did not get it."
EXPIRED = "It waited for approval on the Mac and expired, so the session did not get it."
APPROVED = "Approved on the Mac: the session has it now."
GONE = "That session is not running any more."
STALE = "That session no longer listens at its address."
MOVED = "That terminal runs a different session now."
UNSUPPORTED = "This session's Claude Code cannot take messages from eve."
TIMEOUT = "The session did not answer in time."
BUSY = "The session's inbox is busy; try again in a moment."
TOO_LONG = "Too long to send as one message."
PACED = "Too many messages to this session just now; wait a few seconds and send again."
EMPTY = "Nothing to send."
FAILED = "Could not reach the session."
DROPPED = {
    "duplicate": "The session dropped it as a repeat of the message just before it.",
    "rate-limited": "Too many messages to this session just now; wait a little and send again.",
    "queue-full": "The session has too many unread messages from other programs, so it dropped this one.",
    "hop-loop": "The session dropped it as a message loop.",
    "hop-runaway": "The session dropped it as a message loop.",
}


def _receipt(status: str, drop_reason: Any = None) -> Optional[tuple[str, str]]:
    """A peer_message_status line's status as (Delivery state, the phone's sentence); None for an unknown status."""
    if status == "held":
        return "held", HELD
    if status == "delivered":
        return "delivered", APPROVED
    if status == "refused":
        return "refused", REFUSED
    if status == "denied":
        return "refused", DENIED
    if status == "expired":
        return "error", EXPIRED
    if status == "dropped":
        return "error", DROPPED.get(drop_reason if isinstance(drop_reason, str) else "",
                                    "The session dropped it unread.")
    return None


# ---- the registry ---------------------------------------------------------------------------------------------------

def address(socket_path: str) -> str:
    """How a session names a socket in ``from="..."`` and SendMessage's ``to``: ``uds:`` and the path, any byte
    outside ``A-Za-z0-9:_/.\\-`` percent-encoded."""
    return "uds:" + _PATH_UNSAFE.sub(
        lambda m: "".join(f"%{b:02X}" for b in m.group(0).encode("utf-8", "surrogatepass")), socket_path)


def _alive(pid: int) -> bool:
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True              # someone else's process: alive, though not one of ours
    except (OverflowError, OSError):
        return False
    return True


def _start_times(pids: list[int]) -> Optional[dict[int, str]]:
    """Each live pid's start time as Claude Code records it (``LC_ALL=C TZ=UTC ps -o lstart=``), one ps for all.
    None when ps cannot run, so the check is skipped rather than every session dropped."""
    if not pids:
        return {}
    env = {**os.environ, "LC_ALL": "C", "TZ": "UTC"}
    try:
        out = subprocess.run(["ps", "-o", "pid=,lstart=", "-p", ",".join(str(p) for p in pids)],
                             capture_output=True, text=True, timeout=3, env=env)
    except (OSError, subprocess.SubprocessError):
        return None
    times: dict[int, str] = {}
    for line in out.stdout.splitlines():
        parts = line.split(None, 1)
        if len(parts) == 2 and parts[0].isdigit():
            times[int(parts[0])] = " ".join(parts[1].split())
    if not times and out.returncode != 0:
        return None
    return times


def _pid_domain() -> Optional[str]:
    return "darwin" if sys.platform == "darwin" else None


def _read_record(path: Path) -> Optional[dict[str, Any]]:
    """One registry file as a dict, or None when it is not one (torn, too big, a link, not JSON)."""
    try:
        st = os.lstat(path)
        if not stat.S_ISREG(st.st_mode) or st.st_size > RECORD_CAP:
            return None
        with open(path, "rb") as f:
            obj = json.loads(f.read(RECORD_CAP + 1))
    except (OSError, ValueError):
        return None
    return obj if isinstance(obj, dict) else None


def _is_socket(path: str) -> bool:
    try:
        return stat.S_ISSOCK(os.lstat(path).st_mode)
    except OSError:
        return False


def _peer_from(pid: int, rec: dict[str, Any]) -> Optional[Peer]:
    sid = rec.get("sessionId")
    if not isinstance(sid, str) or not sid:
        return None
    sock = rec.get("messagingSocketPath")
    sock = sock if isinstance(sock, str) and sock.startswith("/") else ""
    protocol = rec.get("peerProtocol")
    features = rec.get("peerFeatures")

    def text(key: str) -> str:
        value = rec.get(key)
        return value if isinstance(value, str) else ""

    return Peer(pid=pid, session_id=sid, cwd=text("cwd"), name=text("name"), status=text("status"),
                kind=text("kind"), socket_path=sock if sock and _is_socket(sock) else "", version=text("version"),
                protocol=protocol if isinstance(protocol, int) and not isinstance(protocol, bool) else 0,
                features=tuple(f for f in features if isinstance(f, str))[:16] if isinstance(features, list) else ())


def live_sessions(sessions_dir: Optional[Path] = None) -> dict[str, Peer]:
    """Every live interactive session in Claude Code's registry, by session id, whether or not it can take messages
    (``socket_path`` is "" when it has no socket; ``protocol`` 0 when it declares none). Live: its pid runs, and was
    started when the record says (a reused pid is not the session). Two live processes on one session id: the most
    recently updated one. A folder that does not exist is no sessions; one that cannot be listed raises OSError,
    because that is "cannot tell", never "none". A headless run (``claude -p``, entrypoint ``sdk-cli``) registers as
    interactive too, and is included."""
    folder = Path(sessions_dir) if sessions_dir is not None else SESSIONS_DIR
    try:
        names = os.listdir(folder)
    except FileNotFoundError:
        return {}
    found: list[tuple[int, dict[str, Any]]] = []
    domain = _pid_domain()
    for name in names:
        m = _RECORD_NAME.match(name)
        if not m:
            continue
        pid = int(m.group(1))
        rec = _read_record(folder / name)
        if rec is None or rec.get("pid") != pid or pid <= 1 or rec.get("kind") != "interactive":
            continue
        if rec.get("spare") is True or rec.get("parkedJobId") is not None:
            continue             # a pre-started spare or a parked job: not a conversation anyone is in
        if domain is not None and isinstance(rec.get("pidDomain"), str) and rec["pidDomain"] != domain:
            continue             # recorded in another pid namespace (a container): its pid means nothing here
        if _alive(pid):
            found.append((pid, rec))
    starts = _start_times([pid for pid, rec in found if isinstance(rec.get("procStart"), str)])
    out: dict[str, Peer] = {}
    stamps: dict[str, float] = {}
    for pid, rec in found:
        recorded = rec.get("procStart")
        if isinstance(recorded, str) and starts is not None and starts.get(pid) != " ".join(recorded.split()):
            continue             # the pid is gone, or now belongs to another process
        peer = _peer_from(pid, rec)
        if peer is None:
            continue
        stamp = rec.get("updatedAt")
        stamp = float(stamp) if isinstance(stamp, (int, float)) and not isinstance(stamp, bool) else 0.0
        if peer.session_id in out and stamps[peer.session_id] >= stamp:
            continue
        out[peer.session_id] = peer
        stamps[peer.session_id] = stamp
    return out


def peers(sessions_dir: Optional[Path] = None) -> dict[str, Peer]:
    """Live interactive sessions that can take messages, by session id. A registry file whose pid is gone is
    skipped."""
    return {sid: p for sid, p in live_sessions(sessions_dir).items()
            if p.protocol in PROTOCOLS and p.socket_path}


# ---- the message ----------------------------------------------------------------------------------------------------

def wrap(text: str) -> str:
    """The message body eve sends: ``OWNER_LINE``, a newline, then the owner's words."""
    return f"{OWNER_LINE}\n{text}"


def _display_name(name: str) -> str:
    """A from-name as a session shows it: no quotes or angle brackets, no control or invisible characters, at most
    64 characters."""
    name = "".join(c for c in name.replace('"', "").replace("<", "").replace(">", "")
                   if unicodedata.category(c) not in ("Cc", "Cf", "Cs", "Zl", "Zp")).strip()
    return name[:64] + "\u2026" if len(name) > 64 else name


def escape_body(body: str) -> str:
    """The words with every closing envelope tag in them made harmless (``<`` becomes ``<\\``)."""
    return _CLOSING.sub("<\\\\", body)


def envelope(body: str, *, from_address: str = "", name: str = EVE_NAME, mode: str = EVE_MODE) -> str:
    """The message's content as Claude Code's own sender writes it."""
    attrs = []
    if from_address and _ADDRESS.match(from_address):
        attrs.append(f'from="{from_address}"')
    shown = _display_name(name)
    if shown:
        attrs.append(f'from-name="{shown}"')
    if mode in ("bypass", "prompting"):
        attrs.append(f'from-mode="{mode}"')
    head = (" " + " ".join(attrs)) if attrs else ""
    return f"<{TAG}{head}>\n{escape_body(body)}\n</{TAG}>"


def parse_envelope(content: str) -> Optional[dict[str, str]]:
    """``from``, ``from-name``... and ``body`` of a message that is exactly one well-formed envelope, as a session
    reads one; None otherwise."""
    m = _ENVELOPE.match(content) if isinstance(content, str) else None
    if m is None:
        return None
    keys = ("from", "from-session", "hop-chain", "from-name", "from-mode", "from-plugin")
    out = {k: v for k, v in zip(keys, m.groups()[:6], strict=True) if v is not None}
    out["body"] = m.group(7)
    return out


def _clean(text: str) -> str:
    return text.encode("utf-8", "replace").decode("utf-8")    # a lone surrogate cannot go on the wire


def _frame(peer: Peer, body: str, msg_id: str, from_address: str) -> str:
    frame: dict[str, Any] = {"msgV": 1, "msg_id": msg_id, "type": "user",
                             "message": {"role": "user", "content": envelope(body, from_address=from_address)},
                             "priority": "next"}
    if from_address:
        frame["from"] = from_address
    frame["session_id"] = peer.session_id
    return json.dumps(frame, ensure_ascii=False, separators=(",", ":"))


def _key_path(peer: Peer, sessions_dir: Path) -> Path:
    digest = hashlib.sha256(os.path.abspath(peer.socket_path).encode("utf-8")).hexdigest()
    return sessions_dir / f"{peer.pid}.{digest}.key"


def _auth_line(peer: Peer, sessions_dir: Path) -> str:
    """The session's auth line when its key file is there and well-formed, else "". The token is never logged."""
    try:
        path = _key_path(peer, sessions_dir)
        st = os.lstat(path)
        if not stat.S_ISREG(st.st_mode) or st.st_size > KEY_CAP:
            return ""
        with open(path, "rb") as f:
            obj = json.loads(f.read(KEY_CAP + 1))
    except (OSError, ValueError):
        return ""
    token = obj.get("peerToken") if isinstance(obj, dict) else None
    if not isinstance(token, str) or not _TOKEN.match(token):
        return ""
    return json.dumps({"type": "auth", "token": token}, separators=(",", ":")) + "\n"


def _utf16_len(s: str) -> int:
    return len(s.encode("utf-16-le", "surrogatepass")) // 2


# ---- who is on the other end ----------------------------------------------------------------------------------------

def _peer_pid(sock: Any) -> Optional[int]:
    """The pid of the process at the other end of a connected Unix socket (macOS LOCAL_PEERPID, Linux SO_PEERCRED)."""
    try:
        if sys.platform == "darwin":
            pid = sock.getsockopt(0, 2)
        elif hasattr(socket, "SO_PEERCRED"):
            pid = struct.unpack("3i", sock.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, struct.calcsize("3i")))[0]
        else:
            return None
    except (OSError, struct.error, AttributeError):
        return None
    return pid if isinstance(pid, int) and pid > 0 else None


def _peer_uid(sock: Any) -> Optional[int]:
    """The uid at the other end (macOS LOCAL_PEERCRED's struct xucred, Linux SO_PEERCRED)."""
    try:
        if sys.platform == "darwin":
            raw = sock.getsockopt(0, 1, 76)
            return struct.unpack_from("I", raw, 4)[0] if len(raw) >= 8 else None
        if hasattr(socket, "SO_PEERCRED"):
            return struct.unpack("3i", sock.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED,
                                                       struct.calcsize("3i")))[1]
    except (OSError, struct.error, AttributeError):
        return None
    return None


# ---- pacing: the session's own rate limit, kept on eve's side so it never has to drop one ---------------------------

_buckets: "OrderedDict[str, tuple[float, float]]" = OrderedDict()


def _take_token(target: str, now: Optional[float] = None) -> bool:
    """One message's worth of ``target``'s bucket; False when it is empty."""
    now = time.monotonic() if now is None else now
    tokens, at = _buckets.pop(target, (float(PACE_BURST), now))
    tokens = min(float(PACE_BURST), tokens + (now - at) * PACE_REFILL)
    if tokens < 1:
        _buckets[target] = (tokens, now)
        return False
    _buckets[target] = (tokens - 1, now)
    while len(_buckets) > 256:
        _buckets.popitem(last=False)
    return True


def _give_back(target: str) -> None:
    """A message that never went out does not count against the session's rate."""
    if target in _buckets:
        tokens, at = _buckets[target]
        _buckets[target] = (min(float(PACE_BURST), tokens + 1), at)


# ---- send -----------------------------------------------------------------------------------------------------------

_active: Optional["Inbox"] = None


def active_inbox() -> Optional["Inbox"]:
    """The Inbox ``send`` names as eve's address, if one is listening in this event loop."""
    inbox = _active
    if inbox is None or not inbox.address:
        return None
    try:
        running = asyncio.get_running_loop()
    except RuntimeError:
        return None
    return inbox if inbox._loop is running else None


def _recheck(peer: Peer) -> Optional[Delivery]:
    """The session is still the one at that pid and socket, by the registry and the process table; a Delivery when
    not."""
    if not _alive(peer.pid):
        return Delivery("gone", GONE)
    rec = _read_record(SESSIONS_DIR / f"{peer.pid}.json")
    if rec is not None and rec.get("pid") == peer.pid:
        if rec.get("sessionId") != peer.session_id:
            return Delivery("gone", MOVED)
        if rec.get("messagingSocketPath") not in (None, peer.socket_path):
            return Delivery("gone", STALE)
    try:
        st = os.lstat(peer.socket_path)
    except FileNotFoundError:
        return Delivery("gone", STALE)
    if stat.S_ISLNK(st.st_mode):
        return Delivery("error", "Refusing to send: the session's socket path is a link.")
    if not stat.S_ISSOCK(st.st_mode):
        return Delivery("gone", STALE)
    return None


async def _deliver(peer: Peer, payload: bytes, deadline: float) -> Optional[Delivery]:
    """Connect, check who answers, write, close our side, and wait for the session to close back. None when the
    session took the bytes; a Delivery when it did not."""
    loop = asyncio.get_running_loop()
    try:
        reader, writer = await asyncio.wait_for(asyncio.open_unix_connection(peer.socket_path),
                                                max(0.01, deadline - loop.time()))
    except (FileNotFoundError, ConnectionRefusedError):
        return Delivery("gone", STALE)
    except (asyncio.TimeoutError, TimeoutError):
        return Delivery("error", TIMEOUT)
    except BlockingIOError:
        return Delivery("error", BUSY)
    except PermissionError:
        return Delivery("error", "Not allowed to open the session's socket.")
    except OSError as e:
        return Delivery("error", BUSY if e.errno in (errno.EAGAIN, errno.EWOULDBLOCK, errno.EBUSY) else FAILED)
    try:
        sock = writer.get_extra_info("socket")
        pid, uid = _peer_pid(sock), _peer_uid(sock)
        if pid is None or uid is None:
            return Delivery("error", "Could not check which process answers at the session's socket.")
        if pid != peer.pid:
            return Delivery("gone", STALE)        # another process listens there now
        if uid != os.getuid():
            return Delivery("error", "Refusing to send: the session's socket belongs to another user.")
        writer.write(payload)
        await asyncio.wait_for(writer.drain(), max(0.01, deadline - loop.time()))
        await asyncio.sleep(END_DELAY)
        if writer.can_write_eof():
            writer.write_eof()
        # The session closes back once it has read everything. Bytes already in its socket are its to read even if
        # that takes longer, so running out of time here is not a failure.
        with contextlib.suppress(asyncio.TimeoutError, TimeoutError, OSError, ValueError):
            await asyncio.wait_for(reader.read(), max(0.01, deadline - loop.time()))
        return None
    except (asyncio.TimeoutError, TimeoutError):
        return Delivery("error", TIMEOUT)
    except (BrokenPipeError, ConnectionResetError):
        return Delivery("gone", STALE)
    except OSError:
        return Delivery("error", FAILED)
    finally:
        writer.close()
        with contextlib.suppress(Exception):
            await asyncio.wait_for(writer.wait_closed(), 0.5)


async def send(peer: Peer, text: str, *, timeout: float = 5.0) -> Delivery:
    """Deliver ``wrap(text)`` to the session. Never raises: every failure is a Delivery."""
    try:
        return await _send(peer, text, timeout)
    except Exception as e:  # noqa: BLE001 — the contract: a failure is a Delivery, never an exception
        log.warning("claude_peer: send failed unexpectedly (%s)", type(e).__name__)
        return Delivery("error", FAILED)


async def _send(peer: Peer, text: str, timeout: float) -> Delivery:
    if not isinstance(text, str) or not text.strip():
        return Delivery("error", EMPTY)
    if peer.protocol not in PROTOCOLS or not peer.socket_path:
        return Delivery("unsupported", UNSUPPORTED)
    loop = asyncio.get_running_loop()
    deadline = loop.time() + max(0.1, float(timeout))
    problem = await asyncio.to_thread(_recheck, peer)
    if problem is not None:
        return problem
    inbox = active_inbox()
    from_address = inbox.address if inbox is not None else ""
    msg_id = str(uuid.uuid4())
    line = _frame(peer, wrap(_clean(text)), msg_id, from_address)
    if _AUTH_LINE_LEN + _utf16_len(line) + 1 > LINE_CAP:
        return Delivery("error", TOO_LONG)
    target = os.path.abspath(peer.socket_path)
    if not _take_token(target):
        return Delivery("error", PACED)
    auth = await asyncio.to_thread(_auth_line, peer, SESSIONS_DIR)
    answer = inbox._expect(msg_id, peer.pid) if inbox is not None else None
    failed = await _deliver(peer, (auth + line + "\n").encode("utf-8"), deadline)
    if failed is not None:
        _give_back(target)
        if inbox is not None:
            inbox._forget(msg_id)
        log.info("claude_peer: not delivered to pid %d (%s)", peer.pid, failed.state)
        return failed
    if answer is not None:
        # Held, refused and dropped messages are answered at once; an accepted one never is.
        wait = min(RECEIPT_WAIT, max(0.0, deadline - loop.time()))
        try:
            got = await asyncio.wait_for(asyncio.shield(answer), wait)
        except (asyncio.TimeoutError, TimeoutError):
            got = None
        inbox._settle(msg_id)
        if got is not None:
            return Delivery(got[0], got[1], msg_id)
    log.info("claude_peer: delivered to pid %d (%d characters)", peer.pid, len(text))
    return Delivery("delivered", DELIVERED, msg_id)


# ---- eve's inbox ----------------------------------------------------------------------------------------------------

def _private_dir(folder: Path) -> None:
    """``folder`` exists as a real directory of ours that no one else can enter (0700); made so when missing.
    OSError when it is a link, someone else's, or open to others: eve does not listen there then."""
    try:
        st = os.lstat(folder)
    except FileNotFoundError:
        os.makedirs(folder, mode=0o700, exist_ok=True)
        st = os.lstat(folder)
        if stat.S_ISDIR(st.st_mode) and st.st_uid == os.getuid() and (st.st_mode & 0o777) != 0o700:
            os.chmod(folder, 0o700)
            st = os.lstat(folder)
    if not stat.S_ISDIR(st.st_mode):
        raise OSError(f"{folder} is not a directory")
    if st.st_uid != os.getuid():
        raise OSError(f"{folder} belongs to another user")
    if st.st_mode & 0o077:
        raise OSError(f"{folder} is open to other users (mode {st.st_mode & 0o777:o})")


def _answers(path: Path) -> bool:
    with contextlib.closing(socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)) as s:
        s.settimeout(0.25)
        try:
            s.connect(str(path))
        except OSError:
            return False
        return True


def _inbox_path(folder: Path) -> Path:
    """``<folder>/<eve's pid>.sock``, as a session names its own. A dead socket left there is removed; a live one (a
    pid shared across namespaces) is left alone and eve takes ``<pid>-<8 hex>.sock``, a name sessions also accept."""
    _private_dir(folder)
    path = folder / f"{os.getpid()}.sock"
    for _ in range(4):
        try:
            st = os.lstat(path)
        except FileNotFoundError:
            return path
        if stat.S_ISSOCK(st.st_mode) and not _answers(path):
            with contextlib.suppress(FileNotFoundError):
                os.unlink(path)
            return path
        path = folder / f"{os.getpid()}-{secrets.token_hex(4)}.sock"
    raise OSError("no free socket name for eve's inbox")


class Inbox:
    """eve's own socket, so a session that answers with SendMessage reaches eve instead of failing.
    ``on_message(from_address, from_name, text)`` is called for each message; eve never acts on one, it only shows
    it. ``address`` is how receivers see eve in ``from="..."``.

    It listens at ``/tmp/cc-socks/<eve's pid>.sock`` (``socket_dir`` moves it), beside the sessions' own sockets: a
    session sends its answers about a message (held, refused, dropped) only to an address there, and verifies by pid
    that the process listening is the one that sent. Nothing is written to ``~/.claude``. Those answers go to
    ``send`` while it waits, and later ones (approved, declined or expired on the Mac) to ``on_receipt(msg_id, state,
    detail)``. ``send`` uses the Inbox that started last in its event loop."""

    def __init__(self, on_message: Callable[[str, str, str], None], *,
                 on_receipt: Optional[Callable[[str, str, str], None]] = None,
                 socket_dir: Optional[Path] = None) -> None:
        self.on_message = on_message
        self.on_receipt = on_receipt
        self.address = ""
        self.socket_path = ""
        self._dir = Path(socket_dir) if socket_dir is not None else SOCKETS_DIR
        self._server: Optional[asyncio.AbstractServer] = None
        self._loop: Optional[asyncio.AbstractEventLoop] = None
        self._inode: Optional[tuple[int, int]] = None
        self._waiting: dict[str, tuple[asyncio.Future, int]] = {}
        self._sent: "OrderedDict[str, int]" = OrderedDict()      # msg_id -> the pid it went to, for later answers
        self._tasks: set[asyncio.Task] = set()

    async def start(self) -> None:
        global _active
        if self._server is not None:
            return
        path = await asyncio.to_thread(_inbox_path, self._dir)
        # LINE_CAP counts characters; a character is up to three bytes of UTF-8.
        server = await asyncio.start_unix_server(self._serve, path=str(path), limit=3 * LINE_CAP + 2)
        try:
            os.chmod(path, 0o600)
            st = os.lstat(path)
        except OSError:
            server.close()
            raise
        self._server, self._loop = server, asyncio.get_running_loop()
        self._inode = (st.st_dev, st.st_ino)
        self.socket_path = str(path)
        self.address = address(str(path))
        _active = self
        atexit.register(self._unlink)
        log.info("claude_peer: eve's inbox listens at %s", self.address)

    async def close(self) -> None:
        global _active
        if _active is self:
            _active = None
        server, self._server = self._server, None
        if server is not None:
            server.close()
            with contextlib.suppress(Exception):
                await asyncio.wait_for(server.wait_closed(), 1.0)
        for task in list(self._tasks):
            task.cancel()
        for fut, _ in self._waiting.values():
            if not fut.done():
                fut.set_result(None)         # send stops waiting: no answer
        self._waiting.clear()
        self._unlink()
        with contextlib.suppress(Exception):
            atexit.unregister(self._unlink)
        self.address = ""

    def _unlink(self) -> None:
        """Remove eve's socket file, only if it is still the one this Inbox made."""
        if not self.socket_path or self._inode is None:
            return
        with contextlib.suppress(OSError):
            st = os.lstat(self.socket_path)
            if (st.st_dev, st.st_ino) == self._inode:
                os.unlink(self.socket_path)
        self._inode = None

    # -- send's side --
    def _expect(self, msg_id: str, pid: int) -> asyncio.Future:
        fut = asyncio.get_running_loop().create_future()
        self._waiting[msg_id] = (fut, pid)
        self._sent[msg_id] = pid
        while len(self._sent) > 256:
            self._sent.popitem(last=False)
        return fut

    def _settle(self, msg_id: str) -> None:
        """send stopped waiting: a later answer about this message goes to on_receipt."""
        self._waiting.pop(msg_id, None)

    def _forget(self, msg_id: str) -> None:
        self._settle(msg_id)
        self._sent.pop(msg_id, None)

    # -- the socket's side --
    async def _serve(self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
        sock = writer.get_extra_info("socket")
        pid, uid = _peer_pid(sock), _peer_uid(sock)
        try:
            if uid is not None and uid != os.getuid():
                return
            while True:
                try:
                    line = await asyncio.wait_for(reader.readline(), FIRST_LINE_WAIT)
                except (asyncio.TimeoutError, TimeoutError, ValueError, asyncio.LimitOverrunError, OSError):
                    return               # silent, too long, or broken: dropped, as a session drops one
                if not line:
                    return
                await self._line(line, pid)
        finally:
            writer.close()
            with contextlib.suppress(Exception):
                await asyncio.wait_for(writer.wait_closed(), 0.5)

    async def _line(self, raw: bytes, pid: Optional[int]) -> None:
        try:
            frame = json.loads(raw.decode("utf-8"))
        except (UnicodeDecodeError, ValueError):
            return
        if not isinstance(frame, dict) or not isinstance(frame.get("type"), str):
            return
        if frame["type"] == "control" and frame.get("action") == "peer_message_status":
            self._status(frame, pid)
        elif frame["type"] == "user":
            await self._message(frame, pid)

    def _status(self, frame: dict[str, Any], pid: Optional[int]) -> None:
        status = frame.get("status")
        if status == "expired" and frame.get("status_detail") == "refused":
            status = "refused"
        mapped = _receipt(status if isinstance(status, str) else "", frame.get("drop_reason"))
        if mapped is None:
            return
        ids = [frame.get("orig_msg_id")]
        if isinstance(frame.get("dropped_msg_ids"), list):
            ids += frame["dropped_msg_ids"][:256]
        for msg_id in dict.fromkeys(i for i in ids if isinstance(i, str) and _MSG_ID.match(i)):
            sent_to = self._sent.get(msg_id)
            if sent_to is None or (pid is not None and pid != sent_to):
                continue             # not a message eve sent, or not the session it went to
            entry = self._waiting.pop(msg_id, None)
            if entry is not None and not entry[0].done():
                entry[0].set_result(mapped)
            elif self.on_receipt is not None:
                self._call(self.on_receipt, msg_id, *mapped)

    async def _message(self, frame: dict[str, Any], pid: Optional[int]) -> None:
        message = frame.get("message")
        content = message.get("content") if isinstance(message, dict) else None
        if not isinstance(content, str) or not content:
            return
        parsed = parse_envelope(content) or {}
        text = parsed.get("body", content)
        asserted = frame.get("from") if isinstance(frame.get("from"), str) else parsed.get("from", "")
        from_address = asserted if asserted and _ADDRESS.match(asserted) else ""
        from_name = _display_name(parsed.get("from-name", ""))
        if pid is not None:
            # The kernel says which process wrote; the registry says which session that is. Trust those over the
            # message's own words about its sender.
            with contextlib.suppress(Exception):
                for peer in (await asyncio.to_thread(live_sessions)).values():
                    if peer.pid == pid:
                        from_address = address(peer.socket_path) if peer.socket_path else from_address
                        from_name = _display_name(peer.name) or from_name
                        break
        if not from_address and pid is not None:
            from_address = f"pid:{pid}"
        log.info("claude_peer: a message for eve (%d characters)", len(text))
        self._call(self.on_message, from_address, from_name, text)

    def _call(self, fn: Callable[..., Any], *args: Any) -> None:
        try:
            result = fn(*args)
        except Exception as e:  # noqa: BLE001 — a listener's failure must not close eve's inbox
            log.warning("claude_peer: inbox listener failed (%s)", type(e).__name__)
            return
        if inspect.isawaitable(result):
            task = asyncio.ensure_future(result)
            self._tasks.add(task)
            task.add_done_callback(self._finished)

    def _finished(self, task: asyncio.Task) -> None:
        self._tasks.discard(task)
        if not task.cancelled() and task.exception() is not None:
            log.warning("claude_peer: inbox listener failed (%s)", type(task.exception()).__name__)
