"""Who may use eve lite's web app: the Mac's own browser and each paired phone or laptop (docs/plans/eve-lite.md M4).

* **Cookies.** ``eve_mac`` on the Mac's own page and ``eve_dev`` on the LAN door. A cookie is scoped by host, not
  by port, so the two listeners need two names. Both are HttpOnly and SameSite=Strict and last 400 days (MAX_AGE,
  the most a browser keeps one). Only ``eve_dev`` is Secure: the Mac's page is plain http on loopback, where not
  every browser keeps a Secure cookie.
* **Devices** (``web-devices.json``, 0600). A cookie's value is a 256-bit token. The file keeps only its SHA-256,
  with a label, the host it was set on, the door it paired through (``local`` for the Mac's own page, ``lan`` for a
  phone or laptop), and when the device paired and was last seen. ``check`` is told which door is asking and refuses
  a token from the other one, so a phone's ``eve_dev`` token cannot stand in as the Mac page's ``eve_mac`` and reach
  a Mac-only route. A row with no door reads as ``lan`` (the lower-privilege door). Revoking returns the ids, so the
  daemon can close those devices' live peers. A device unseen for MAX_AGE is forgotten: its cookie is gone too.
* **The pairing window.** Only the signed-in Mac page opens it. It holds one 128-bit secret, which travels in the
  QR code's ``#p=`` fragment (a browser never sends a fragment to a server), and one 6-digit code to type on a
  laptop. It lasts 5 minutes and pairs one device. 5 wrong tries close it, and a new window replaces the old one.
* **The Mac login.** ``eve-bridge web open`` asks the daemon, over its 0600 socket, for a one-time 128-bit code
  that lives 2 minutes, and hands it to the browser in a 0600 file: never on the screen, in argv or in a log.

Secrets are compared in constant time, never logged, and never kept: tokens are hashed on disk, codes in memory.
"""

from __future__ import annotations

import contextlib
import hashlib
import hmac
import json
import logging
import os
import re
import secrets
import threading
import time
import unicodedata
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Callable, Optional

log = logging.getLogger(__name__)

COOKIE_MAC = "eve_mac"
COOKIE_DEV = "eve_dev"
MAX_AGE = 34_560_000                  # 400 days: browsers keep a cookie no longer (Chrome caps Max-Age there)

PAIR_SECS = 300
PAIR_TRIES = 5                        # wrong tries that close a pairing window
LOGIN_SECS = 120
LOGIN_KEEP_SECS = 60                  # a used login code still answers used() this long after it expired
MAX_LOGINS = 16                       # login codes waiting at once; past this the oldest goes
MAX_DEVICES = 64                      # paired browsers; past this the one seen longest ago is dropped
SEEN_SECS = 600                       # a device's last_seen reaches the file at most this often
TEXT_CHARS = 64                       # a stored label or host

DOORS = ("local", "lan")                           # the Mac's own page, and the LAN door a phone or laptop pairs on
DEFAULT_DOOR = "lan"                                # a row with no door (an older file) reads as the lower-privilege one
TOKEN = re.compile(r"[A-Za-z0-9_-]{16,256}")      # what secrets.token_urlsafe makes; cookie-safe as it is
DEVICE_ID = re.compile(r"[0-9a-f]{8}")
SHA256_HEX = re.compile(r"[0-9a-f]{64}")
CODE_SEPARATORS = re.compile(r"[\s-]")             # "123 456" and "123-456" are the code typed with a gap


# ---- cookies --------------------------------------------------------------------------------------------

def _flags(name: str) -> str:
    if name == COOKIE_DEV:
        return "HttpOnly; Secure; SameSite=Strict; Path=/"
    if name == COOKIE_MAC:
        return "HttpOnly; SameSite=Strict; Path=/"
    raise ValueError("not an eve lite cookie")


def set_cookie(name: str, token: str) -> str:
    """The Set-Cookie value that gives a browser its device token, with the exact flags for its listener."""
    flags = _flags(name)
    if not isinstance(token, str) or not TOKEN.fullmatch(token):
        raise ValueError("not a device token")
    return f"{name}={token}; {flags}; Max-Age={MAX_AGE}"


def clear_cookie(name: str) -> str:
    """The Set-Cookie value that removes the cookie (signing out)."""
    return f"{name}=; {_flags(name)}; Max-Age=0"


def cookie_value(header: Optional[str], name: str) -> str:
    """The value of cookie ``name`` in a request's Cookie header, or "". A malformed pair is skipped, never
    fatal (``http.cookies`` drops every cookie after one it cannot parse)."""
    for part in (header or "").split(";"):
        key, eq, value = part.partition("=")
        if not eq or key.strip() != name:
            continue
        value = value.strip()
        if len(value) >= 2 and value[0] == value[-1] == '"':
            value = value[1:-1]
        if value:
            return value
    return ""


# ---- devices --------------------------------------------------------------------------------------------

@dataclass(frozen=True)
class Device:
    """One paired browser."""

    id: str
    label: str
    host: str
    created: int
    last_seen: int


def _digest(secret: str) -> str:
    return hashlib.sha256(secret.encode()).hexdigest()


def _text(value: Any) -> str:
    """A label or host as it may be stored and shown: one line, no control or format characters, TEXT_CHARS."""
    if not isinstance(value, str):
        return ""
    kept = "".join(" " if c.isspace() else c for c in value if c.isspace() or unicodedata.category(c)[0] != "C")
    return " ".join(kept.split())[:TEXT_CHARS]


def _stamp(value: Any) -> Optional[int]:
    return value if isinstance(value, int) and not isinstance(value, bool) and value >= 0 else None


def _door(value: Any) -> str:
    """The door a row paired through, as it is stored: ``local`` or ``lan``; anything else (an older row with no
    door, a bad value) reads as ``lan``, the lower-privilege door, so a hand-written one cannot claim the Mac's."""
    return value if value in DOORS else DEFAULT_DOOR


def _row(raw: Any) -> Optional[dict[str, Any]]:
    """One device as the file holds it, checked; None for anything else (dropped, never trusted)."""
    if not isinstance(raw, dict):
        return None
    dev_id, digest = raw.get("id"), raw.get("sha256")
    created, seen = _stamp(raw.get("created")), _stamp(raw.get("last_seen"))
    if not (isinstance(dev_id, str) and DEVICE_ID.fullmatch(dev_id) and isinstance(digest, str)
            and SHA256_HEX.fullmatch(digest) and created is not None and seen is not None):
        return None
    return {"id": dev_id, "label": _text(raw.get("label")), "host": _text(raw.get("host")),
            "door": _door(raw.get("door")), "sha256": digest, "created": created, "last_seen": seen}


def _write_private(path: Path, data: bytes) -> None:
    """Write ``data`` to ``path`` atomically and owner-only (0600), whatever the umask."""
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_name(f".{path.name}.tmp")
    with contextlib.suppress(FileNotFoundError):
        tmp.unlink()
    fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0), 0o600)
    try:
        with os.fdopen(fd, "wb") as f:
            os.fchmod(f.fileno(), 0o600)
            f.write(data)
            f.flush()
            os.fsync(f.fileno())
        os.replace(tmp, path)
    except BaseException:
        with contextlib.suppress(OSError):
            tmp.unlink()
        raise


class Devices:
    """The paired browsers, in ``path`` (``~/.config/eve-bridge/web-devices.json``, 0600), tokens as SHA-256 only.
    Reads the file again whenever it changed on disk, so a revoke by another process counts at once. Thread-safe."""

    def __init__(self, path: Path | str, *, clock: Callable[[], float] = time.time) -> None:
        self.path = Path(path)
        self._clock = clock
        self._lock = threading.Lock()
        self._rows: list[dict[str, Any]] = []
        self._file: Optional[tuple[int, int, int]] = None      # (inode, size, mtime) when last read or written
        self._read_once = False

    def add(self, label: str, host: str, *, door: str = DEFAULT_DOOR) -> tuple[str, str]:
        """Pair a browser on ``door`` (``local`` for the Mac's own page, ``lan`` for a phone or laptop). Returns its
        id and the token for its cookie: the token is never stored or shown, only its hash is kept. Raises OSError
        when the list cannot be saved, and then nothing was paired."""
        token = secrets.token_urlsafe(32)
        with self._lock:
            self._sync()
            now = int(self._clock())
            taken = {row["id"] for row in self._rows}
            dev_id = secrets.token_hex(4)
            while dev_id in taken:
                dev_id = secrets.token_hex(4)
            row = {"id": dev_id, "label": _text(label), "host": _text(host), "door": _door(door),
                   "sha256": _digest(token), "created": now, "last_seen": now}
            rows = self._kept(self._rows + [row], now)
            self._write(rows)
            self._rows = rows
            paired = len(rows)
        log.info("web auth: paired a device (%d paired)", paired)
        return dev_id, token

    def check(self, token: Any, door: Optional[str] = None) -> Optional[Device]:
        """The device a cookie's token belongs to, or None: unknown, revoked, malformed, unseen for MAX_AGE, or —
        when ``door`` is given — paired through the other door. The door is the caller's listener (``local`` or
        ``lan``): a phone's ``eve_dev`` token presented as the Mac page's ``eve_mac`` is refused here, so it reaches
        no Mac-only route. Marks the device seen."""
        if not isinstance(token, str) or not TOKEN.fullmatch(token):
            return None
        digest = _digest(token)
        with self._lock:
            self._sync()
            now = int(self._clock())
            found: Optional[dict[str, Any]] = None
            for row in self._rows:                              # every row, each in constant time
                if hmac.compare_digest(row["sha256"], digest):
                    found = row
            if found is None or now - found["last_seen"] > MAX_AGE or (door is not None and found["door"] != door):
                return None
            if now - found["last_seen"] >= SEEN_SECS:
                found["last_seen"] = now
                try:
                    rows = self._kept(self._rows, now)
                    self._write(rows)
                    self._rows = rows
                except OSError as e:                            # a full disk must not sign anyone out
                    log.warning("web auth: could not save when a device was last seen (%s)", type(e).__name__)
            return Device(found["id"], found["label"], found["host"], found["created"], found["last_seen"])

    def revoke(self, id_or_all: str) -> list[str]:
        """Forget one device by its id, or every device with "all". Returns the ids revoked, so their live
        connections can be closed; [] when nothing matched. The tokens stop working at once; OSError when the list
        cannot be saved, because then the revoke would not outlive a restart."""
        want = id_or_all.strip().lower() if isinstance(id_or_all, str) else ""
        with self._lock:
            self._sync()
            gone = [row["id"] for row in self._rows if want and (want == "all" or row["id"] == want)]
            if gone:
                self._rows = [row for row in self._rows if row["id"] not in gone]
                log.info("web auth: revoked %d device(s)", len(gone))
                self._write(self._kept(self._rows, int(self._clock())))
        return gone

    def list(self) -> list[dict[str, Any]]:
        """Every paired device, oldest first, as {id, label, host, created, last_seen}. Never the hash."""
        with self._lock:
            self._sync()
            now = int(self._clock())
            rows = [{k: row[k] for k in ("id", "label", "host", "created", "last_seen")}
                    for row in self._rows if now - row["last_seen"] <= MAX_AGE]
        return sorted(rows, key=lambda row: (row["created"], row["id"]))

    # ---- the file ------------------------------------------------------------------------------------------

    def _stat(self) -> Optional[tuple[int, int, int]]:
        try:
            st = self.path.stat()
        except OSError:
            return None
        return st.st_ino, st.st_size, st.st_mtime_ns

    def _sync(self) -> None:
        """Read the file when this object has not yet, or it changed since this object last read or wrote it."""
        now_file = self._stat()
        if self._read_once and now_file == self._file:
            return
        self._rows = self._read()
        self._file = now_file
        self._read_once = True

    def _read(self) -> list[dict[str, Any]]:
        try:
            data = json.loads(self.path.read_text(encoding="utf-8"))
        except FileNotFoundError:
            return []
        except (OSError, ValueError) as e:
            log.warning("web auth: the device list could not be read (%s); every device pairs again", type(e).__name__)
            return []
        raw = data.get("devices") if isinstance(data, dict) else None
        rows = [row for row in map(_row, raw if isinstance(raw, list) else []) if row is not None]
        unique: dict[str, dict[str, Any]] = {}
        for row in rows:
            unique.setdefault(row["id"], row)
        return list(unique.values())

    def _kept(self, rows: list[dict[str, Any]], now: int) -> list[dict[str, Any]]:
        """``rows`` less the devices unseen for MAX_AGE and, past MAX_DEVICES, the ones seen longest ago."""
        rows = [row for row in rows if now - row["last_seen"] <= MAX_AGE]
        if len(rows) > MAX_DEVICES:
            oldest = sorted(rows, key=lambda row: (row["last_seen"], row["created"]))[: len(rows) - MAX_DEVICES]
            dropped = {row["id"] for row in oldest}
            rows = [row for row in rows if row["id"] not in dropped]
            log.info("web auth: forgot %d device(s) seen longest ago (more than %d paired)", len(dropped), MAX_DEVICES)
        return rows

    def _write(self, rows: list[dict[str, Any]]) -> None:
        body = json.dumps({"version": 1, "devices": rows}, indent=2) + "\n"
        _write_private(self.path, body.encode())
        self._file = self._stat()
        self._read_once = True


# ---- pairing a phone or laptop --------------------------------------------------------------------------

def _code_digits(code: Any) -> Optional[str]:
    """A 6-digit code as typed ("123456", "123 456", "123-456", or the number 123456), or None."""
    if isinstance(code, int) and not isinstance(code, bool):
        return f"{code:06d}" if 0 <= code < 1_000_000 else None
    if isinstance(code, str) and len(code) <= 32:
        digits = CODE_SEPARATORS.sub("", code)
        if re.fullmatch(r"[0-9]{6}", digits):
            return digits
    return None


class PairingWindow:
    """At most one open pairing at a time: a 128-bit secret for the QR code and a 6-digit code to type. Either
    pairs one device, once, before ``ttl`` runs out; PAIR_TRIES wrong tries close the window first. Thread-safe."""

    def __init__(self, *, clock: Callable[[], float] = time.monotonic) -> None:
        self._clock = clock
        self._lock = threading.Lock()
        self._secret = b""                    # SHA-256 of the secret and of the code, never the values
        self._code = b""
        self._until = 0.0
        self._tries = 0
        self._open = False

    def open(self, ttl: float = PAIR_SECS) -> tuple[str, str]:
        """A new window, replacing any open one: (secret for ``#p=``, 6-digit code). Only the signed-in Mac page
        may call this."""
        secret = secrets.token_urlsafe(16)
        code = f"{secrets.randbelow(1_000_000):06d}"
        with self._lock:
            self._secret = hashlib.sha256(secret.encode()).digest()
            self._code = hashlib.sha256(code.encode()).digest()
            self._until = self._clock() + max(float(ttl), 0.0)
            self._tries = 0
            self._open = True
        log.info("web auth: pairing window open for %d s", int(ttl))
        return secret, code

    def close(self) -> None:
        """Close the window now: the Mac page cancelled it, or the LAN door turned off."""
        with self._lock:
            self._shut()

    @property
    def is_open(self) -> bool:
        with self._lock:
            return self._live()

    @property
    def remaining(self) -> float:
        """Seconds until the window closes by itself, 0.0 when it is closed."""
        with self._lock:
            return max(self._until - self._clock(), 0.0) if self._live() else 0.0

    def pair(self, *, secret: Any = None, code: Any = None) -> bool:
        """True once: for the window's secret or its code, given alone, while the window is open. Any other try
        counts as wrong (both given, neither, malformed, not a match), and the PAIR_TRIES-th closes the window."""
        with self._lock:
            if not self._live():
                return False
            given: Optional[bytes] = None
            want = b""
            if secret is not None and code is None:
                if isinstance(secret, str) and len(secret) <= 64:
                    given, want = hashlib.sha256(secret.encode("utf-8", "replace")).digest(), self._secret
            elif code is not None and secret is None:
                digits = _code_digits(code)
                if digits is not None:
                    given, want = hashlib.sha256(digits.encode()).digest(), self._code
            if given is not None and hmac.compare_digest(given, want):
                self._shut()
                log.info("web auth: a device paired through the window (%s)", "secret" if secret is not None else "code")
                return True
            self._tries += 1
            if self._tries >= PAIR_TRIES:
                self._shut()
                log.warning("web auth: pairing window closed after %d wrong tries", self._tries)
            return False

    def _live(self) -> bool:
        if self._open and self._clock() >= self._until:
            self._shut()
        return self._open

    def _shut(self) -> None:
        self._open = False
        self._secret = self._code = b""


# ---- the Mac's own browser ------------------------------------------------------------------------------

@dataclass
class _Login:
    until: float
    keep: float
    used: bool = False


class MacLogin:
    """One-time codes that sign the Mac's own browser in. Each works once, before its ``ttl``; only its hash is
    kept. Thread-safe."""

    def __init__(self, *, clock: Callable[[], float] = time.monotonic) -> None:
        self._clock = clock
        self._lock = threading.Lock()
        self._codes: dict[str, _Login] = {}

    def issue(self, ttl: float = LOGIN_SECS) -> str:
        """A new 128-bit code, for ``http://localhost:<port>/#login=<code>``."""
        code = secrets.token_urlsafe(16)
        with self._lock:
            now = self._clock()
            self._prune(now)
            while len(self._codes) >= MAX_LOGINS:
                del self._codes[next(iter(self._codes))]       # the oldest: dicts keep insertion order
            until = now + max(float(ttl), 0.0)
            self._codes[_digest(code)] = _Login(until=until, keep=until + LOGIN_KEEP_SECS)
        return code

    def consume(self, code: Any) -> bool:
        """True once for a code this issued, before it expires; False for anything else."""
        if not isinstance(code, str) or not TOKEN.fullmatch(code):
            return False
        with self._lock:
            now = self._clock()
            self._prune(now)
            entry = self._codes.get(_digest(code))
            if entry is None or entry.used or now >= entry.until:
                return False
            entry.used = True
        log.info("web auth: the Mac's browser signed in")
        return True

    def used(self, code: Any) -> bool:
        """Has this code been consumed? ``web open`` asks, to delete its file as soon as the browser signed in."""
        if not isinstance(code, str) or not TOKEN.fullmatch(code):
            return False
        with self._lock:
            self._prune(self._clock())
            entry = self._codes.get(_digest(code))
            return entry is not None and entry.used

    def _prune(self, now: float) -> None:
        for key in [k for k, entry in self._codes.items() if now >= entry.keep]:
            del self._codes[key]
