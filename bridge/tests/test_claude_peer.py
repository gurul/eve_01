"""claude_peer: eve speaking Claude Code's cross-session messaging protocol, offline. A fake session (a Unix socket
server in this process, so the kernel names this test's pid at both ends) stands in for a real one; the live proof
against real sessions is in docs/claude-peer.md."""

from __future__ import annotations

import asyncio
import json
import os
import shutil
import socket
import stat
import subprocess
import tempfile
import uuid
from pathlib import Path
from typing import Any, Awaitable, Callable, Optional

import pytest

from eve_bridge import claude_peer
from eve_bridge.claude_peer import Delivery, Inbox, Peer


@pytest.fixture(autouse=True)
def _fresh(monkeypatch, tmp_path):
    """No Inbox, empty rate buckets, a private registry folder, and no 150 ms close delay."""
    monkeypatch.setattr(claude_peer, "_active", None)
    claude_peer._buckets.clear()
    registry = tmp_path / "sessions"
    registry.mkdir()
    monkeypatch.setattr(claude_peer, "SESSIONS_DIR", registry)
    monkeypatch.setattr(claude_peer, "END_DELAY", 0.0)
    yield
    claude_peer._buckets.clear()


@pytest.fixture
def sock_dir():
    """A short folder for sockets: a Unix socket path must stay under 104 bytes, and pytest's tmp_path does not."""
    d = tempfile.mkdtemp(prefix="cp-", dir="/tmp")
    os.chmod(d, 0o700)
    yield Path(d)
    shutil.rmtree(d, ignore_errors=True)


def peer_at(path: Any, *, pid: Optional[int] = None, sid: str = "sess-1", protocol: int = 1,
            status: str = "idle") -> Peer:
    return Peer(pid=os.getpid() if pid is None else pid, session_id=sid, cwd="/w", name="work-1", status=status,
                kind="interactive", socket_path=str(path), version="2.1.311", protocol=protocol,
                features=("notify_idle",))


def lines_of(raw: bytes) -> list[dict[str, Any]]:
    return [json.loads(line) for line in raw.decode("utf-8").splitlines() if line.strip()]


class FakeSession:
    """A session's inbox: reads each connection to its end, closes back, records what came, then (like a session)
    may answer a user message with a peer_message_status line to its ``from`` address."""

    def __init__(self, path: Path, answer: Optional[Callable[[dict[str, Any]], Awaitable[None]]] = None) -> None:
        self.path = str(path)
        self.answer = answer
        self.conns: list[bytes] = []
        self.server: Optional[asyncio.AbstractServer] = None

    async def __aenter__(self) -> "FakeSession":
        self.server = await asyncio.start_unix_server(self._serve, path=self.path)
        os.chmod(self.path, 0o600)
        return self

    async def __aexit__(self, *exc: Any) -> None:
        assert self.server is not None
        self.server.close()
        await asyncio.wait_for(self.server.wait_closed(), 2)

    async def _serve(self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
        data = await reader.read()
        self.conns.append(data)
        writer.close()
        for frame in lines_of(data):
            if frame.get("type") == "user" and self.answer is not None:
                await self.answer(frame)


async def write_to(path: str, *lines: Any, raw: bytes = b"") -> None:
    """Connect to ``path`` and write JSON lines (or raw bytes) the way a session's sender does, then close."""
    reader, writer = await asyncio.open_unix_connection(path)
    writer.write(raw + b"".join((json.dumps(x) + "\n").encode() for x in lines))
    await writer.drain()
    writer.write_eof()
    await asyncio.wait_for(reader.read(), 2)
    writer.close()


def receipt_to(status: str, **extra: Any) -> Callable[[dict[str, Any]], Awaitable[None]]:
    async def answer(frame: dict[str, Any]) -> None:
        line = {"type": "control", "action": "peer_message_status", "status": status, "reason": "(peer text)",
                "from": "uds:/tmp/cc-socks/1.sock", "orig_msg_id": frame["msg_id"], "msgV": 1,
                "msg_id": str(uuid.uuid4()), **extra}
        await write_to(frame["from"][len("uds:"):], line)
    return answer


def write_record(folder: Path, pid: int, **fields: Any) -> Path:
    rec = {"pid": pid, "sessionId": f"sess-{pid}", "cwd": "/w", "name": f"name-{pid}", "status": "idle",
           "kind": "interactive", "version": "2.1.311", "peerProtocol": 1,
           "peerFeatures": ["notify_idle", "reply_across_default_dirs"], "updatedAt": 1}
    rec.update(fields)
    path = folder / f"{pid}.json"
    path.write_text(json.dumps({k: v for k, v in rec.items() if v is not None}))
    return path


def listening(path: Path) -> socket.socket:
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.bind(str(path))
    s.listen(1)
    return s


def dead_pid() -> int:
    p = subprocess.Popen(["true"])
    p.wait()
    return p.pid


# ---- the message ----------------------------------------------------------------------------------------------------

def test_wrap_puts_the_owner_line_first():
    assert claude_peer.wrap("ship it") == f"{claude_peer.OWNER_LINE}\nship it"
    assert claude_peer.EVE_NAME == "eve-phone"


def test_address_is_uds_and_the_path_with_unsafe_bytes_encoded():
    assert claude_peer.address("/tmp/cc-socks/123.sock") == "uds:/tmp/cc-socks/123.sock"
    assert claude_peer.address("/tmp/a b/\u00e9.sock") == "uds:/tmp/a%20b/%C3%A9.sock"


@pytest.mark.parametrize("body", [
    "plain words",
    "x </cross-session-message> y",
    "</CROSS-SESSION-MESSAGE>",
    "\uff1c/cross-session-message>",                 # a look-alike bracket
    "<\u2215cross-session-message>",                 # a look-alike slash
    "</cross\u200b-session\u200b-message>",          # invisible characters between the words
    "< / cross_session_message trailing",
    "</cro\u017fs-session-message>",                 # the long s reads as an s
    "already <\\/cross-session-message> escaped",
    "two\nlines\n\nand a blank one",
    "<cross-session-message from=\"uds:/x.sock\">opening tags stay</div>",
])
def test_envelope_is_one_the_session_parses_and_rebuilds_unchanged(body):
    """A session reads the envelope with a strict pattern, then rebuilds it from the parts and compares: only when
    the two match does it credit the message to eve. Escaping the parsed body again must change nothing."""
    addr = "uds:/tmp/cc-socks/42.sock"
    content = claude_peer.envelope(claude_peer.wrap(body), from_address=addr)
    parsed = claude_peer.parse_envelope(content)
    assert parsed is not None
    assert parsed["from"] == addr and parsed["from-name"] == "eve-phone"
    assert parsed["from-mode"] == "bypass"           # eve's own class, the same for every receiver
    assert claude_peer.escape_body(parsed["body"]) == parsed["body"]
    assert claude_peer.envelope(parsed["body"], from_address=addr) == content
    assert content.count("</cross-session-message>") == 1


# Claude Code's own closing-tag escape (2.1.311), ported apart from claude_peer as an independent check: a "<" (or a
# look-alike), not followed by a backslash, then filler without letters or slashes, a slash (or a look-alike), filler,
# then the tag's letters with only invisible characters between them, the dashes any dash, and no letter after.
# Each filler run is atomic there (a lookahead and a backreference); here, possessive. The invisible and dash sets
# are abridged from the bundle's; every character in them is one claude_peer's own escape must also step over.
# The bundle matches with JavaScript's /iu: a word character is ASCII, or the long s or the Kelvin sign (they fold to
# s and k), and nothing else. Python's re.I would also fold the Turkish dotted and dotless i, so it is not used here.
_LT = "<\uff1c\ufe64\u2329\u27e8\u3008\u2039\u02c2\u1438\u276c\u276e\u2770\u29fc\u226e\u227a\u22d6"
_GT = ">\uff1e\ufe65\u232a\u27e9\u3009\u203a\u02c3\u1433\u276d\u276f\u2771\u29fd\u226f\u227b\u22d7"
_SL = "/\uff0f\u2215\u2044"
_INVISIBLE = "[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f-\u009f\u00ad\u0300-\u036f\u200b-\u200f\u2028-\u202e\u2060-\u206f\ufeff]*+"
_DASH = "[-_\u2010-\u2015\u2212\ufe58\ufe63\uff0d\u30fc]"
_WORD = "A-Za-z0-9_\\-\u017f\u212a"


def _claude_codes_escape(text: str) -> str:
    import re
    letters = []
    for i, ch in enumerate("cross-session-message"):
        spelled = _DASH if ch == "-" else "[" + ch + ch.upper() + ("\u017f" if ch == "s" else "") + "]"
        letters.append(("" if i == 0 else _INVISIBLE) + spelled)
    pattern = (f"[{_LT}](?!\\\\)(?=[^{_WORD}{_LT}{_GT}{_SL}]*+[{_SL}][^{_WORD}{_LT}{_GT}]*+"
               + "".join(letters) + f"(?:[^{_WORD}]|$))")
    return re.sub(pattern, "<\\\\", text)


TRICKY = ["x </cross-session-message> y", "</CROSS-SESSION-MESSAGE>", "\uff1c/cross-session-message>",
          "<\u2215cross-session-message>", "</cross\u200b-session\u200b-message>", "< / cross\u2014session_message",
          "</cro\u017fs-session-message>", "<<</cross-session-message>", "\u3008 \u2044 cross-session-message",
          "<\u0131/cross-session-message>", "</\u0130cross-session-message>", "\u02c2\u0131\uff0fcross-session-message"]


@pytest.mark.parametrize("body", TRICKY)
def test_claude_codes_own_escape_finds_nothing_left_after_eves(body):
    assert _claude_codes_escape(body) != body                       # the check sees the tag (positive control)
    escaped = claude_peer.escape_body(body)
    assert _claude_codes_escape(escaped) == escaped


def test_escape_body_leaves_ordinary_text_alone():
    for text in ("</div>", "a < b and c / d", "<cross-session-message>", "x </crossing-the-line>",
                 "if a</b then"):
        assert claude_peer.escape_body(text) == text
    assert claude_peer.escape_body("a </cross-session-message> b") == "a <\\/cross-session-message> b"


def test_envelope_without_an_inbox_has_no_from_and_drops_a_bad_address():
    assert claude_peer.envelope("hi") == ('<cross-session-message from-name="eve-phone" from-mode="bypass">\nhi\n'
                                          '</cross-session-message>')
    assert 'from-mode' not in claude_peer.envelope("hi", mode="root")    # only the two classes Claude Code knows
    assert 'from="' not in claude_peer.envelope("hi", from_address="uds:/a b.sock")


def test_display_name_is_cleaned_like_a_session_shows_it():
    assert claude_peer._display_name('ev"e<x>\u200b\u0007') == "evex"
    assert claude_peer._display_name("n" * 70) == "n" * 64 + "\u2026"


def test_parse_envelope_rejects_anything_but_one_well_formed_envelope():
    assert claude_peer.parse_envelope("hello") is None
    assert claude_peer.parse_envelope('<cross-session-message from-name="a">\nx\n</cross-session-message>\n') is None
    assert claude_peer.parse_envelope('<cross-session-message from-mode="root">\nx\n</cross-session-message>') is None
    got = claude_peer.parse_envelope('<cross-session-message from="uds:/s.sock" from-name="w" from-mode="bypass">'
                                     '\nPONG\n</cross-session-message>')
    assert got == {"from": "uds:/s.sock", "from-name": "w", "from-mode": "bypass", "body": "PONG"}


# ---- the registry ---------------------------------------------------------------------------------------------------

def test_peers_reads_a_live_interactive_session(sock_dir, tmp_path):
    pid = os.getpid()
    sock = listening(sock_dir / f"{pid}.sock")
    try:
        write_record(claude_peer.SESSIONS_DIR, pid, messagingSocketPath=str(sock_dir / f"{pid}.sock"),
                     sessionId="abc", name="eve-d9", status="busy")
        got = claude_peer.peers()
    finally:
        sock.close()
    assert got == {"abc": Peer(pid=pid, session_id="abc", cwd="/w", name="eve-d9", status="busy",
                               kind="interactive", socket_path=str(sock_dir / f"{pid}.sock"), version="2.1.311",
                               protocol=1, features=("notify_idle", "reply_across_default_dirs"))}


def test_peers_skips_what_is_not_a_live_session_that_takes_messages(sock_dir, monkeypatch):
    reg = claude_peer.SESSIONS_DIR
    me, parent, gone = os.getpid(), os.getppid(), dead_pid()
    good = sock_dir / "good.sock"
    keep = listening(good)
    (sock_dir / "plain.sock").write_text("not a socket")
    try:
        write_record(reg, me, messagingSocketPath=str(good), sessionId="ok")
        (reg / "11.json").write_text("{torn")
        (reg / "12.json").write_text("[1, 2]")
        (reg / "abc.json").write_text(json.dumps({"pid": 13}))
        (reg / f"{me}.1234abcd.key").write_text("{}")
        write_record(reg, gone, messagingSocketPath=str(good), sessionId="dead")
        mismatched = json.loads(write_record(reg, parent, messagingSocketPath=str(good), sessionId="wrong-pid")
                                .read_text())
        (reg / f"{parent}.json").write_text(json.dumps({**mismatched, "pid": parent + 1}))   # file name says otherwise
        rows = {
            "sdk": dict(kind="sdk"),
            "proto2": dict(peerProtocol=2),
            "noproto": dict(peerProtocol=None),
            "nosock": dict(messagingSocketPath=None),
            "filesock": dict(messagingSocketPath=str(sock_dir / "plain.sock")),
            "relative": dict(messagingSocketPath="good.sock"),
            "spare": dict(spare=True),
            "parked": dict(parkedJobId="j1"),
        }
        folders = {}
        for sid, extra in rows.items():
            folder = reg.parent / f"reg-{sid}"
            folder.mkdir()
            write_record(folder, me, **{"messagingSocketPath": str(good), "sessionId": sid, **extra})
            folders[sid] = folder
        assert set(claude_peer.peers()) == {"ok"}
        for sid, folder in folders.items():
            assert claude_peer.peers(folder) == {}, sid
        # live_sessions lists the live ones that cannot take messages, too
        assert claude_peer.live_sessions(folders["proto2"])["proto2"].protocol == 2
        assert claude_peer.live_sessions(folders["nosock"])["nosock"].socket_path == ""
        assert claude_peer.live_sessions(folders["filesock"])["filesock"].socket_path == ""
        assert claude_peer.live_sessions(folders["sdk"]) == {}
    finally:
        keep.close()


def test_a_recycled_pid_is_not_the_session(sock_dir, monkeypatch):
    good = sock_dir / "s.sock"
    keep = listening(good)
    try:
        write_record(claude_peer.SESSIONS_DIR, os.getpid(), messagingSocketPath=str(good), sessionId="old",
                     procStart="Thu Jan  1 00:00:00 1970")
        assert claude_peer.peers() == {}
        monkeypatch.setattr(claude_peer, "_start_times", lambda pids: {os.getpid(): "Thu Jan 1 00:00:00 1970"})
        assert set(claude_peer.peers()) == {"old"}          # same start, spaces aside: the same process
        monkeypatch.setattr(claude_peer, "_start_times", lambda pids: None)
        assert set(claude_peer.peers()) == {"old"}          # ps could not run: the check is skipped, not failed
    finally:
        keep.close()


def test_start_times_reads_this_process_like_claude_code_records_it():
    times = claude_peer._start_times([os.getpid()])
    assert times is not None and len(times[os.getpid()].split()) == 5     # "Tue Oct 27 04:24:15 2026"


def test_another_pid_namespace_is_skipped_on_macos(sock_dir, monkeypatch):
    monkeypatch.setattr(claude_peer, "_pid_domain", lambda: "darwin")
    good = sock_dir / "s.sock"
    keep = listening(good)
    try:
        write_record(claude_peer.SESSIONS_DIR, os.getpid(), messagingSocketPath=str(good), pidDomain="linux:4026")
        assert claude_peer.peers() == {}
        write_record(claude_peer.SESSIONS_DIR, os.getpid(), messagingSocketPath=str(good), pidDomain="darwin")
        assert len(claude_peer.peers()) == 1
    finally:
        keep.close()


def test_two_processes_on_one_session_id_keep_the_latest(sock_dir):
    a, b = sock_dir / "a.sock", sock_dir / "b.sock"
    keep = [listening(a), listening(b)]
    try:
        write_record(claude_peer.SESSIONS_DIR, os.getpid(), messagingSocketPath=str(a), sessionId="same",
                     updatedAt=5)
        write_record(claude_peer.SESSIONS_DIR, os.getppid(), messagingSocketPath=str(b), sessionId="same",
                     updatedAt=9)
        assert claude_peer.peers()["same"].pid == os.getppid()
    finally:
        for s in keep:
            s.close()


def test_a_missing_registry_is_no_sessions_and_an_unreadable_one_raises(tmp_path):
    assert claude_peer.peers(tmp_path / "nope") == {}
    locked = tmp_path / "locked"
    locked.mkdir()
    locked.chmod(0)
    try:
        if os.access(locked, os.R_OK):
            pytest.skip("running as a user who can read anything")
        with pytest.raises(OSError):
            claude_peer.peers(locked)
    finally:
        locked.chmod(0o700)


# ---- send -----------------------------------------------------------------------------------------------------------

def test_send_writes_the_auth_line_and_the_message_then_closes(sock_dir):
    path = sock_dir / "s.sock"
    peer = peer_at(path, sid="the-session")
    token = "0123456789abcdef" * 2
    key = claude_peer._key_path(peer, claude_peer.SESSIONS_DIR)
    key.write_text(json.dumps({"peerToken": token, "procStart": "x"}))

    async def run():
        async with FakeSession(path) as fake:
            got = await claude_peer.send(peer, "deploy the branch")
        return got, fake.conns

    got, conns = asyncio.run(run())
    assert got.state == "delivered" and got.ok and got.detail == claude_peer.DELIVERED
    assert uuid.UUID(got.msg_id)
    assert len(conns) == 1 and conns[0].endswith(b"\n")
    auth, msg = lines_of(conns[0])
    assert auth == {"type": "auth", "token": token}
    assert msg == {"msgV": 1, "msg_id": got.msg_id, "type": "user", "priority": "next",
                   "session_id": "the-session",
                   "message": {"role": "user", "content": claude_peer.envelope(claude_peer.wrap("deploy the branch"))}}


@pytest.mark.parametrize("key_text", [None, "{bad json", json.dumps({"peerToken": "short"}), json.dumps([1])])
def test_without_a_good_key_file_send_skips_the_auth_line(sock_dir, key_text):
    path = sock_dir / "s.sock"
    peer = peer_at(path)
    if key_text is not None:
        claude_peer._key_path(peer, claude_peer.SESSIONS_DIR).write_text(key_text)

    async def run():
        async with FakeSession(path) as fake:
            got = await claude_peer.send(peer, "hi")
        return got, fake.conns

    got, conns = asyncio.run(run())
    assert got.state == "delivered"
    assert [f["type"] for f in lines_of(conns[0])] == ["user"]


def test_with_an_inbox_the_message_names_eve_and_an_unanswered_one_is_delivered(sock_dir, monkeypatch):
    monkeypatch.setattr(claude_peer, "RECEIPT_WAIT", 0.2)
    path = sock_dir / "s.sock"

    async def run():
        box = Inbox(lambda *a: None, socket_dir=sock_dir / "socks")
        await box.start()
        try:
            async with FakeSession(path) as fake:
                got = await claude_peer.send(peer_at(path), "hi")
            return got, fake.conns, box.address
        finally:
            await box.close()

    got, conns, addr = asyncio.run(run())
    assert got.state == "delivered"
    (msg,) = lines_of(conns[0])
    assert msg["from"] == addr == f"uds:{sock_dir}/socks/{os.getpid()}.sock"
    assert claude_peer.parse_envelope(msg["message"]["content"])["from"] == addr


@pytest.mark.parametrize("status,extra,state,detail", [
    ("held", {}, "held", claude_peer.HELD),
    ("refused", {}, "refused", claude_peer.REFUSED),
    ("expired", {"status_detail": "refused"}, "refused", claude_peer.REFUSED),
    ("dropped", {"drop_reason": "duplicate", "dropped_msg_ids": []}, "error", claude_peer.DROPPED["duplicate"]),
    ("dropped", {"drop_reason": "rate-limited"}, "error", claude_peer.DROPPED["rate-limited"]),
])
def test_a_session_answer_becomes_the_delivery(sock_dir, status, extra, state, detail):
    path = sock_dir / "s.sock"

    async def run():
        box = Inbox(lambda *a: None, socket_dir=sock_dir / "socks")
        await box.start()
        try:
            async with FakeSession(path, receipt_to(status, **extra)):
                return await claude_peer.send(peer_at(path), "hi")
        finally:
            await box.close()

    got = asyncio.run(run())
    assert (got.state, got.detail) == (state, detail)
    assert got.ok is (state == "held")


def test_a_drop_named_only_in_dropped_msg_ids_still_finds_its_message(sock_dir):
    path = sock_dir / "s.sock"

    async def answer(frame):
        await receipt_to("dropped", drop_reason="queue-full", orig_msg_id=str(uuid.uuid4()),
                         dropped_msg_ids=[frame["msg_id"]])(frame)

    async def run():
        box = Inbox(lambda *a: None, socket_dir=sock_dir / "socks")
        await box.start()
        try:
            async with FakeSession(path, answer):
                return await claude_peer.send(peer_at(path), "hi")
        finally:
            await box.close()

    assert asyncio.run(run()).detail == claude_peer.DROPPED["queue-full"]


def test_an_answer_after_send_returns_goes_to_on_receipt(sock_dir, monkeypatch):
    """Held, then approved on the Mac later: send has returned "held", and the approval arrives on its own."""
    path = sock_dir / "s.sock"
    later: list[tuple[str, str, str]] = []

    async def answer(frame):
        await receipt_to("held")(frame)
        await asyncio.sleep(0.3)
        await receipt_to("delivered")(frame)
        await receipt_to("denied")(frame)

    async def run():
        box = Inbox(lambda *a: None, on_receipt=lambda *a: later.append(a), socket_dir=sock_dir / "socks")
        await box.start()
        try:
            async with FakeSession(path, answer):
                got = await claude_peer.send(peer_at(path), "hi")
                for _ in range(40):
                    if len(later) >= 2:
                        break
                    await asyncio.sleep(0.05)
            return got
        finally:
            await box.close()

    got = asyncio.run(run())
    assert got.state == "held"
    assert later == [(got.msg_id, "delivered", claude_peer.APPROVED), (got.msg_id, "refused", claude_peer.DENIED)]


def test_answers_about_other_messages_or_from_other_processes_are_ignored(sock_dir):
    seen: list[Any] = []

    async def run():
        box = Inbox(lambda *a: None, on_receipt=lambda *a: seen.append(a), socket_dir=sock_dir / "socks")
        await box.start()
        try:
            mine = str(uuid.uuid4())
            box._sent[mine] = 999_999                   # went to another pid than the one answering (this one)
            await write_to(box.socket_path, {"type": "control", "action": "peer_message_status", "status": "held",
                                             "orig_msg_id": mine})
            await write_to(box.socket_path, {"type": "control", "action": "peer_message_status", "status": "held",
                                             "orig_msg_id": str(uuid.uuid4())})          # never sent
            await write_to(box.socket_path, {"type": "control", "action": "rename", "name": "x"})
            await asyncio.sleep(0.1)
        finally:
            await box.close()

    asyncio.run(run())
    assert seen == []


def test_send_says_gone_when_the_session_is_not_there(sock_dir):
    async def run():
        missing = await claude_peer.send(peer_at(sock_dir / "missing.sock"), "hi")
        stale = sock_dir / "stale.sock"
        listening(stale).close()                         # a socket file nobody listens on
        refused = await claude_peer.send(peer_at(stale), "hi")
        dead = await claude_peer.send(peer_at(sock_dir / "x.sock", pid=dead_pid()), "hi")
        return missing, refused, dead

    missing, refused, dead = asyncio.run(run())
    assert (missing.state, missing.detail) == ("gone", claude_peer.STALE)
    assert (refused.state, refused.detail) == ("gone", claude_peer.STALE)
    assert (dead.state, dead.detail) == ("gone", claude_peer.GONE)
    assert missing.msg_id == refused.msg_id == dead.msg_id == ""


def test_send_says_gone_when_the_registry_names_another_session_or_socket(sock_dir):
    path = sock_dir / "s.sock"
    write_record(claude_peer.SESSIONS_DIR, os.getpid(), sessionId="after-clear", messagingSocketPath=str(path))

    async def run():
        async with FakeSession(path) as fake:
            moved = await claude_peer.send(peer_at(path, sid="before-clear"), "hi")
            write_record(claude_peer.SESSIONS_DIR, os.getpid(), sessionId="before-clear",
                         messagingSocketPath=str(sock_dir / "other.sock"))
            elsewhere = await claude_peer.send(peer_at(path, sid="before-clear"), "hi")
        return moved, elsewhere, fake.conns

    moved, elsewhere, conns = asyncio.run(run())
    assert (moved.state, moved.detail) == ("gone", claude_peer.MOVED)
    assert (elsewhere.state, elsewhere.detail) == ("gone", claude_peer.STALE)
    assert conns == []                                   # nothing was written to the wrong conversation


def test_send_never_writes_to_a_process_other_than_the_session(sock_dir):
    """The kernel names the process listening; the parent of this test is alive but is not it."""
    path = sock_dir / "s.sock"

    async def run():
        async with FakeSession(path) as fake:
            got = await claude_peer.send(peer_at(path, pid=os.getppid()), "hi")
        return got, fake.conns

    got, conns = asyncio.run(run())
    assert (got.state, got.detail) == ("gone", claude_peer.STALE)
    assert conns == [b""]


def test_send_refuses_a_socket_path_that_is_a_link(sock_dir):
    path = sock_dir / "s.sock"
    link = sock_dir / "link.sock"
    link.symlink_to(path)

    async def run():
        async with FakeSession(path) as fake:
            got = await claude_peer.send(peer_at(link), "hi")
        return got, fake.conns

    got, conns = asyncio.run(run())
    assert got.state == "error" and "link" in got.detail
    assert conns == []


def test_send_unsupported_empty_too_long(sock_dir):
    path = sock_dir / "s.sock"

    async def run():
        async with FakeSession(path) as fake:
            out = [await claude_peer.send(peer_at(path, protocol=2), "hi"),
                   await claude_peer.send(peer_at(""), "hi"),
                   await claude_peer.send(peer_at(path), "  \n "),
                   await claude_peer.send(peer_at(path), "x" * claude_peer.LINE_CAP)]
        return out, fake.conns

    (proto, nosock, empty, big), conns = asyncio.run(run())
    assert (proto.state, nosock.state) == ("unsupported", "unsupported")
    assert (empty.state, empty.detail) == ("error", claude_peer.EMPTY)
    assert (big.state, big.detail) == ("error", claude_peer.TOO_LONG)
    assert big.msg_id == ""                              # only a message that went out has an id
    assert conns == []


def test_send_times_out_as_an_error(sock_dir, monkeypatch):
    path = sock_dir / "s.sock"

    async def hang(*a, **k):
        await asyncio.sleep(30)

    async def run():
        async with FakeSession(path):
            monkeypatch.setattr(asyncio, "open_unix_connection", hang)
            return await claude_peer.send(peer_at(path), "hi", timeout=0.3)

    got = asyncio.run(run())
    assert (got.state, got.detail) == ("error", claude_peer.TIMEOUT)


def test_send_paces_itself_under_the_sessions_rate_limit(sock_dir, monkeypatch):
    monkeypatch.setattr(claude_peer, "PACE_BURST", 2)
    monkeypatch.setattr(claude_peer, "PACE_REFILL", 0.0001)
    path = sock_dir / "s.sock"

    async def run():
        async with FakeSession(path) as fake:
            out = [await claude_peer.send(peer_at(path), f"m{i}") for i in range(3)]
        return out, fake.conns

    out, conns = asyncio.run(run())
    assert [d.state for d in out] == ["delivered", "delivered", "error"]
    assert out[2].detail == claude_peer.PACED and len(conns) == 2


def test_a_send_that_never_went_out_does_not_count_against_the_rate(sock_dir, monkeypatch):
    monkeypatch.setattr(claude_peer, "PACE_BURST", 1)
    monkeypatch.setattr(claude_peer, "PACE_REFILL", 0.0001)
    path = sock_dir / "s.sock"

    async def run():
        listening(path).close()                          # stale: the first send fails after taking its token
        first = await claude_peer.send(peer_at(path), "hi")
        path.unlink()
        async with FakeSession(path):
            second = await claude_peer.send(peer_at(path), "hi")
        return first, second

    first, second = asyncio.run(run())
    assert (first.state, second.state) == ("gone", "delivered")


def test_send_never_raises(sock_dir, monkeypatch):
    def boom(peer):
        raise RuntimeError("disk on fire")

    monkeypatch.setattr(claude_peer, "_recheck", boom)
    got = asyncio.run(claude_peer.send(peer_at(sock_dir / "s.sock"), "hi"))
    assert got == Delivery("error", claude_peer.FAILED)
    assert asyncio.run(claude_peer.send(peer_at(sock_dir / "s.sock"), None)).state == "error"   # type: ignore[arg-type]


# ---- eve's inbox ----------------------------------------------------------------------------------------------------

def test_inbox_listens_privately_and_cleans_up(sock_dir):
    folder = sock_dir / "made"

    async def run():
        box = Inbox(lambda *a: None, socket_dir=folder)
        await box.start()
        st_dir, st_sock = os.lstat(folder), os.lstat(box.socket_path)
        state = (box.address, box.socket_path, claude_peer.active_inbox() is box)
        await box.close()
        return st_dir, st_sock, state, box

    st_dir, st_sock, (addr, path, active), box = asyncio.run(run())
    assert stat.S_IMODE(st_dir.st_mode) == 0o700 and stat.S_IMODE(st_sock.st_mode) == 0o600
    assert path == str(folder / f"{os.getpid()}.sock") and addr == f"uds:{path}" and active
    assert not os.path.exists(path) and box.address == "" and claude_peer._active is None


def test_inbox_refuses_a_folder_others_can_enter_or_a_link(sock_dir):
    open_dir = sock_dir / "open"
    open_dir.mkdir()
    open_dir.chmod(0o770)
    (sock_dir / "real").mkdir(mode=0o700)
    (sock_dir / "link").symlink_to(sock_dir / "real")
    for folder in (open_dir, sock_dir / "link"):
        with pytest.raises(OSError):
            asyncio.run(Inbox(lambda *a: None, socket_dir=folder).start())
        assert claude_peer._active is None


def test_inbox_replaces_a_dead_socket_and_steps_around_a_live_one(sock_dir):
    folder = sock_dir / "socks"
    folder.mkdir(mode=0o700)
    mine = folder / f"{os.getpid()}.sock"
    listening(mine).close()                              # left by a process that died

    async def start_close():
        box = Inbox(lambda *a: None, socket_dir=folder)
        await box.start()
        path = box.socket_path
        await box.close()
        return path

    assert asyncio.run(start_close()) == str(mine)
    other = listening(mine)                              # something live holds the name
    try:
        path = asyncio.run(start_close())
    finally:
        other.close()
    assert path != str(mine) and Path(path).name.startswith(f"{os.getpid()}-") and path.endswith(".sock")


def test_a_sessions_sendmessage_reaches_on_message(sock_dir):
    """A session answers with SendMessage: no auth line (eve publishes no key), one envelope, then it closes. The
    registry names the session behind the connecting pid, which beats the message's own claims."""
    got: list[tuple[str, str, str]] = []
    session_sock = sock_dir / "session.sock"
    keep = listening(session_sock)
    write_record(claude_peer.SESSIONS_DIR, os.getpid(), messagingSocketPath=str(session_sock), name="work-59")

    async def run():
        box = Inbox(lambda *a: got.append(a), socket_dir=sock_dir / "socks")
        await box.start()
        try:
            content = ('<cross-session-message from="uds:/tmp/cc-socks/1.sock" from-name="claims-to-be-x" '
                       'from-mode="bypass">\nPONG\n</cross-session-message>')
            await write_to(box.socket_path, {"type": "auth", "token": "f" * 32}, {"msgV": 1, "msg_id": str(uuid.uuid4()),
                           "type": "user", "message": {"role": "user", "content": content}, "priority": "next",
                           "from": "uds:/tmp/cc-socks/1.sock"}, raw=b"not json\n\n")
            for _ in range(40):
                if got:
                    break
                await asyncio.sleep(0.05)
        finally:
            await box.close()

    try:
        asyncio.run(run())
    finally:
        keep.close()
    assert got == [(claude_peer.address(str(session_sock)), "work-59", "PONG")]


def test_inbox_passes_a_message_from_an_unregistered_sender_as_it_came(sock_dir):
    got: list[tuple[str, str, str]] = []

    async def run():
        box = Inbox(lambda *a: got.append(a), socket_dir=sock_dir / "socks")
        await box.start()
        try:
            await write_to(box.socket_path, {"type": "user", "from": "uds:/tmp/cc-socks/7.sock",
                                             "message": {"role": "user", "content": "no envelope here"}})
            await write_to(box.socket_path, {"type": "user", "message": {"role": "user", "content": ""}})
            await write_to(box.socket_path, {"type": "user", "message": {"role": "user", "content": (
                '<cross-session-message from-name="w">\nhi\n</cross-session-message>')}})
            await asyncio.sleep(0.2)
        finally:
            await box.close()

    asyncio.run(run())
    assert got == [("uds:/tmp/cc-socks/7.sock", "", "no envelope here"), (f"pid:{os.getpid()}", "w", "hi")]


def test_a_failing_or_async_listener_does_not_break_the_inbox(sock_dir):
    got: list[str] = []
    calls = {"n": 0}

    async def listener(addr, name, text):
        calls["n"] += 1
        if calls["n"] == 1:
            raise RuntimeError("listener bug")
        got.append(text)

    def sync_boom(*a):
        raise ValueError("sync bug")

    async def run():
        box = Inbox(listener, socket_dir=sock_dir / "socks")
        await box.start()
        try:
            msg = lambda t: {"type": "user", "message": {"role": "user", "content": t}}   # noqa: E731
            await write_to(box.socket_path, msg("one"))
            await write_to(box.socket_path, msg("two"))
            box.on_message = sync_boom
            await write_to(box.socket_path, msg("three"))
            box.on_message = listener
            await write_to(box.socket_path, msg("four"))
            await asyncio.sleep(0.2)
        finally:
            await box.close()

    asyncio.run(run())
    assert got == ["two", "four"]


def test_inbox_drops_a_line_past_the_cap_and_keeps_listening(sock_dir, monkeypatch):
    monkeypatch.setattr(claude_peer, "LINE_CAP", 1000)
    got: list[str] = []

    async def run():
        box = Inbox(lambda a, n, t: got.append(t), socket_dir=sock_dir / "socks")
        await box.start()
        try:
            reader, writer = await asyncio.open_unix_connection(box.socket_path)
            writer.write(b"x" * 5000)
            await writer.drain()
            await asyncio.wait_for(reader.read(), 2)      # the inbox hangs up
            writer.close()
            await write_to(box.socket_path, {"type": "user", "message": {"role": "user", "content": "after"}})
            await asyncio.sleep(0.1)
        finally:
            await box.close()

    asyncio.run(run())
    assert got == ["after"]


def test_active_inbox_belongs_to_its_own_event_loop(sock_dir):
    box = Inbox(lambda *a: None, socket_dir=sock_dir / "socks")
    asyncio.run(box.start())
    try:
        assert claude_peer._active is box

        async def other_loop():
            return claude_peer.active_inbox()

        assert asyncio.run(other_loop()) is None          # a send in another loop names no inbox
    finally:
        box._unlink()
        claude_peer._active = None
