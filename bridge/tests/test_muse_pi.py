"""Muse on the owner's Raspberry Pi (muse_pi.py): the client, the chat's "muse on" switch, and the app's
/api/link/muse door. The Pi itself is never reached: ``muse_pi.ask`` is replaced, or ssh is a fake script."""

from __future__ import annotations

import asyncio
import json
import os
import stat
from pathlib import Path
from typing import Any

import pytest

from eve_bridge import muse_pi, telegram
from test_telegram import OWNER, FakeApi, FakeCreate, Rig, run_rig, say, settle, update

HOST = "pi@example.invalid"


@pytest.fixture
def muse(monkeypatch: pytest.MonkeyPatch) -> list[str]:
    """Muse set up, answering every message with "Muse heard: <text>"; the list is what Muse was asked."""
    asked: list[str] = []

    async def ask(text: str, **_: Any) -> muse_pi.Answer:
        asked.append(text)
        return muse_pi.Answer(True, f"Muse heard: {text}")

    monkeypatch.setenv(muse_pi.ENV, HOST)
    monkeypatch.setattr(muse_pi, "ask", ask)
    return asked


# ---- the client ----------------------------------------------------------------------------------------

def test_target_takes_only_a_plain_user_at_host(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv(muse_pi.ENV, raising=False)
    assert muse_pi.target() == "" and not muse_pi.available()
    for bad in ("pi", "-oProxyCommand=x@h", "a@b c", "a@b;rm"):
        monkeypatch.setenv(muse_pi.ENV, bad)
        assert muse_pi.target() == "", bad
    monkeypatch.setenv(muse_pi.ENV, HOST)
    assert muse_pi.target() == HOST and muse_pi.available()


def test_the_session_is_made_once_and_kept(tmp_path: Path) -> None:
    first = muse_pi.session_id(tmp_path)
    assert muse_pi.session_id(tmp_path) == first
    (tmp_path / muse_pi.SESSION_FILE).write_text("not-a-uuid\n")
    assert muse_pi.session_id(tmp_path) != "not-a-uuid"


def test_parse_reads_the_reply_and_never_shows_raw_errors() -> None:
    assert muse_pi.parse(b'{"ok": true, "reply": " PONG "}\n') == muse_pi.Answer(True, "PONG")
    assert muse_pi.parse(b"") == muse_pi.Answer(False, muse_pi.UNREACHABLE_LINE)
    assert muse_pi.parse(b"garbage") == muse_pi.Answer(False, muse_pi.UNREACHABLE_LINE)
    assert not muse_pi.parse(b'{"ok": false, "error": "not connected to the Muse"}').ok
    assert "Traceback" not in muse_pi.parse(b'{"ok": false, "error": "Traceback boom"}').text
    late = muse_pi.parse(b'{"ok": false, "error": "Muse did not finish answering in time", "reply": "half"}')
    assert late == muse_pi.Answer(False, "half")


def test_ask_runs_ssh_with_the_message_on_stdin(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    fake = tmp_path / "ssh"
    log = tmp_path / "args"
    fake.write_text("#!/bin/sh\n"
                    f'printf "%s\\n" "$@" > "{log}"\n'
                    'msg=$(cat)\n'
                    'printf \'{"ok": true, "reply": "got %s"}\\n\' "$msg"\n')
    fake.chmod(fake.stat().st_mode | stat.S_IEXEC)
    monkeypatch.setenv("PATH", f"{tmp_path}{os.pathsep}{os.environ['PATH']}")
    answer = asyncio.run(muse_pi.ask("hello", host=HOST, session="s1"))
    assert answer == muse_pi.Answer(True, "got hello")
    args = log.read_text().split("\n")
    assert "BatchMode=yes" in args and HOST in args and f"{muse_pi.ASK} --session-id s1" in args


def test_ask_is_off_without_a_pi(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv(muse_pi.ENV, raising=False)
    assert not asyncio.run(muse_pi.ask("hi")).ok


# ---- the chat's switch -------------------------------------------------------------------------------------

def test_muse_on_sends_words_to_muse_titled_muse_until_muse_off(muse: list[str]) -> None:
    api = FakeApi([update("muse on", update_id=1)])
    rig = Rig(api, FakeCreate(say("eve here.")))

    async def during() -> None:
        inlet = rig.inlet
        await settle()
        assert inlet.muse is True and api.sent[-1] == (OWNER, telegram.MUSE_ON_LINE)
        api.feed(update("what's the capital of France?", update_id=2))
        await settle()
        assert muse == ["what's the capital of France?"]
        assert api.titled[-1] == (telegram.MUSE_TITLE, None, "Muse heard: what's the capital of France?")
        api.feed(update("eve: hello", update_id=3))                    # "eve: ..." still reaches eve
        await settle()
        assert muse == ["what's the capital of France?"] and len(rig.create.requests) == 1
        api.feed(update("muse off", update_id=4))
        await settle()
        assert inlet.muse is False and api.sent[-1] == (OWNER, telegram.MUSE_OFF_LINE)
        api.feed(update("and now?", update_id=5))
        await settle()
        assert muse == ["what's the capital of France?"] and len(rig.create.requests) == 2

    run_rig(rig, during)


def test_muse_on_without_a_pi_says_so_and_stays_off(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv(muse_pi.ENV, raising=False)
    api = FakeApi([update("muse on", update_id=1)])
    rig = Rig(api, FakeCreate())

    async def during() -> None:
        await settle()
        assert rig.inlet.muse is False and api.sent[-1] == (OWNER, telegram.MUSE_UNAVAILABLE_LINE)

    run_rig(rig, during)


def test_one_relay_at_a_time(muse: list[str]) -> None:
    api = FakeApi([update("claude on", update_id=1)])
    rig = Rig(api, FakeCreate(), claude_sessions=lambda: ["/Users/g/repo"])

    async def during() -> None:
        inlet = rig.inlet
        await settle()
        assert inlet.claude is True
        api.feed(update("muse on", update_id=2))
        await settle()
        assert inlet.muse is True and inlet.claude is False             # Muse on turned Claude off
        api.feed(update("claude on", update_id=3))
        await settle()
        assert inlet.claude is True and inlet.muse is False             # and Claude on turns Muse off

    run_rig(rig, during)


def test_a_failed_ask_is_said_plainly(monkeypatch: pytest.MonkeyPatch) -> None:
    async def ask(text: str, **_: Any) -> muse_pi.Answer:
        return muse_pi.Answer(False, muse_pi.UNREACHABLE_LINE)

    monkeypatch.setenv(muse_pi.ENV, HOST)
    monkeypatch.setattr(muse_pi, "ask", ask)
    api = FakeApi([update("muse on", update_id=1), update("hello", update_id=2)])
    rig = Rig(api, FakeCreate())

    async def during() -> None:
        await settle()
        assert api.titled[-1] == (telegram.MUSE_TITLE, None, muse_pi.UNREACHABLE_LINE)

    run_rig(rig, during)


def test_the_app_switch_is_quiet(muse: list[str]) -> None:
    api = FakeApi([])
    rig = Rig(api, FakeCreate())

    async def during() -> None:
        inlet = rig.inlet
        assert inlet.set_muse(True, tell=False) is True and inlet.muse is True
        assert inlet.set_muse(False, tell=False) is False and inlet.muse is False
        await settle()
        assert api.sent == []

    run_rig(rig, during)


# ---- the app's door ------------------------------------------------------------------------------------

def test_the_link_door_reads_and_flips_muse(muse: list[str]) -> None:
    from eve_bridge import link_chat

    class Inlet:
        muse = False
        _chat_id = OWNER

        def set_muse(self, on: bool, *, tell: bool = True) -> bool:
            assert tell is False
            self.muse = on
            return on

    class Writer:
        def __init__(self) -> None:
            self.data = b""

        def write(self, b: bytes) -> None:
            self.data += b

        async def drain(self) -> None:
            pass

        def close(self) -> None:
            pass

        async def wait_closed(self) -> None:
            pass

    def body(writer: Writer) -> dict[str, Any]:
        return json.loads(writer.data.split(b"\r\n\r\n", 1)[1])

    chat = link_chat.LinkChat.__new__(link_chat.LinkChat)
    inlet = Inlet()

    async def go() -> None:
        w = Writer()
        await chat._muse(inlet, "GET", {}, asyncio.StreamReader(), w)
        assert body(w) == {"ok": True, "available": True, "on": False}
        reader = asyncio.StreamReader()
        payload = json.dumps({"on": True}).encode()
        reader.feed_data(payload)
        reader.feed_eof()
        w = Writer()
        await chat._muse(inlet, "POST", {"content-length": str(len(payload)),
                                         "content-type": "application/json"}, reader, w)
        assert body(w) == {"ok": True, "available": True, "on": True} and inlet.muse is True

    asyncio.run(go())
