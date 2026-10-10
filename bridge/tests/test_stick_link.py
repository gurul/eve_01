"""stick_link.py: the Eve Link app's door into a call, its token, its pairing page and the pinned Update button.

The real Mini App server on a loopback port and a real WebSocket client standing in for the app, as in
test_phone_call.py (whose fake chat and voice this reuses)."""
from __future__ import annotations

import asyncio
import http.client
import json
import os
import stat
import urllib.request
from pathlib import Path
from typing import Any
from urllib.parse import urlsplit

import pytest
from test_phone_call import OWNER, TOKEN, Brain, Voice, is_, press, signed, until
from websockets.asyncio.client import connect
from websockets.exceptions import InvalidStatus

from eve_bridge import miniapp, phone_call, stick_link
from eve_bridge.miniapp import MiniAppConfig, MiniAppServer, SpendLedger, check_init_data

# ---- the token ------------------------------------------------------------------------------------------

def test_no_token_until_pairing_and_nothing_is_accepted_then(tmp_path: Path) -> None:
    t = stick_link.LinkToken(tmp_path / "stick-link.json")
    assert t.current() == ""
    assert not t.check("") and not t.check("anything")


def test_the_token_is_private_checked_exactly_and_a_new_one_cuts_off_the_old(tmp_path: Path) -> None:
    t = stick_link.LinkToken(tmp_path / "stick-link.json")
    first = t.ensure()
    assert len(first) >= 40 and t.ensure() == first                      # ensure keeps it
    assert stat.S_IMODE(os.stat(t.path).st_mode) == 0o600
    assert t.check(first)                                                 # positive control
    assert not t.check(first[:-1]) and not t.check(first + "x") and not t.check("")
    second = t.rotate()
    assert second != first and t.check(second) and not t.check(first)


def test_a_damaged_token_file_counts_as_unpaired(tmp_path: Path) -> None:
    path = tmp_path / "stick-link.json"
    for text in ("not json", json.dumps({"token": 5}), json.dumps({"token": "short"}), json.dumps([1])):
        path.write_text(text)
        assert stick_link.LinkToken(path).current() == ""


def test_the_pairing_link_keeps_the_token_out_of_what_the_server_sees() -> None:
    url = stick_link.pair_url("https://a-b.trycloudflare.com/", "tok_en-123")
    parts = urlsplit(url)
    assert parts.path == stick_link.PAIR_PATH and parts.query == ""       # the request line carries neither
    assert parts.fragment == "t=tok_en-123"


# ---- the door -------------------------------------------------------------------------------------------

async def serve(tmp_path: Path, brain: Any, paired: bool = True) -> tuple[MiniAppServer, str, str, str]:
    cfg = MiniAppConfig(enabled=True, token=TOKEN, owner_ids=frozenset({OWNER}), ledger_path=tmp_path / "l.json")
    server = MiniAppServer(cfg, SpendLedger(cfg.ledger_path), page=b"home")
    server.calls = phone_call.PhoneCalls(lambda d: check_init_data(d, TOKEN, cfg.owner_ids), brain, Voice())
    server.stick = stick_link.LinkToken(tmp_path / "stick-link.json")
    token = server.stick.ensure() if paired else ""
    port = await server.start()
    return server, f"ws://127.0.0.1:{port}", f"http://127.0.0.1:{port}", token


def test_the_app_calls_with_its_token_and_a_press_is_heard(tmp_path: Path) -> None:
    brain = Brain()

    async def go() -> list[Any]:
        server, ws_base, _, token = await serve(tmp_path, brain)
        async with connect(ws_base + stick_link.CALL_PATH) as ws:        # no Origin, as the app
            await ws.send(json.dumps({"token": token}))
            await until(ws, is_("state", state="connected"))
            await press(ws, 1.0)
            got = await until(ws, is_("state", state="listening"))
        await server.close()
        return got

    got = asyncio.run(go())
    assert {"type": "heard", "text": "what's on my calendar"} in got
    assert brain.heard == ["what's on my calendar"]
    assert any(isinstance(m, bytes) for m in got)                         # the reply's audio came back


@pytest.mark.parametrize("hello", [{"token": "wrong"}, {"token": ""}, {}, {"initData": "x"}, "not json"])
def test_a_wrong_or_missing_token_ends_the_call_before_it_starts(tmp_path: Path, hello: Any) -> None:
    brain = Brain()

    async def go() -> None:
        server, ws_base, _, _ = await serve(tmp_path, brain)
        async with connect(ws_base + stick_link.CALL_PATH) as ws:
            await ws.send(hello if isinstance(hello, str) else json.dumps(hello))
            assert (await until(ws, is_("ended")))[-1]["reason"] == "Only eve's owner can call."
        await server.close()

    asyncio.run(go())
    assert brain.say is None and brain.heard == []


def test_telegrams_initdata_does_not_open_the_apps_door_nor_the_token_the_mini_apps(tmp_path: Path) -> None:
    async def go() -> None:
        server, ws_base, http, token = await serve(tmp_path, Brain())
        async with connect(ws_base + stick_link.CALL_PATH) as ws:
            await ws.send(json.dumps({"initData": signed()}))
            assert (await until(ws, is_("ended")))[-1]["reason"] == "Only eve's owner can call."
        async with connect(ws_base + "/api/call", origin=http) as ws:
            await ws.send(json.dumps({"token": token}))
            assert (await until(ws, is_("ended")))[-1]["reason"] == "Only eve's owner can call."
        async with connect(ws_base + "/api/call", origin=http) as ws:  # control: the Mini App still gets in
            await ws.send(json.dumps({"initData": signed()}))
            await until(ws, is_("state", state="connected"))
        await server.close()

    asyncio.run(go())


def test_a_browser_cannot_use_the_apps_door_and_it_is_shut_until_paired(tmp_path: Path) -> None:
    async def go() -> None:
        server, ws_base, http, _ = await serve(tmp_path, Brain())
        for origin in (http, "https://evil.example", "null"):
            with pytest.raises(InvalidStatus) as e:
                async with connect(ws_base + stick_link.CALL_PATH, origin=origin):
                    pass
            assert e.value.response.status_code == 403
        await server.close()
        server, ws_base, _, _ = await serve(tmp_path / "unpaired", Brain(), paired=False)
        async with connect(ws_base + stick_link.CALL_PATH) as ws:        # open door, but no token to match
            await ws.send(json.dumps({"token": ""}))
            assert (await until(ws, is_("ended")))[-1]["reason"] == "Only eve's owner can call."
        server.stick = None
        with pytest.raises(InvalidStatus) as e:
            async with connect(ws_base + stick_link.CALL_PATH):
                pass
        assert e.value.response.status_code == 404
        await server.close()

    asyncio.run(go())


def test_the_pairing_page_is_static_strict_and_hands_the_fragment_to_the_app(tmp_path: Path) -> None:
    def fetch(url: str) -> tuple[int, dict[str, str], str]:
        with urllib.request.urlopen(url) as r:                              # noqa: S310 - loopback
            return r.status, dict(r.headers), r.read().decode()

    async def go() -> tuple[int, dict[str, str], str]:
        server, _, http, token = await serve(tmp_path, Brain())
        out = await asyncio.to_thread(fetch, http + stick_link.PAIR_PATH)  # off the loop the server runs on
        await server.close()
        assert token not in out[2]
        return out

    status, headers, body = asyncio.run(go())
    assert status == 200
    assert "default-src 'none'" in headers["Content-Security-Policy"]
    assert headers["Referrer-Policy"] == "no-referrer" and headers["Cache-Control"] == "no-store"
    assert "location.hash" in body and "evelink://pair?u=" in body
    assert "history.replaceState" in body                                 # the token leaves the address bar


# ---- the pinned message and /stick ------------------------------------------------------------------------

def test_the_pinned_message_gets_the_update_button_only_once_paired() -> None:
    plain = miniapp.open_button("https://x.trycloudflare.com/")
    assert len(plain["inline_keyboard"]) == 1
    both = miniapp.open_button("https://x.trycloudflare.com/", "https://x.trycloudflare.com/stick#t=abc")
    assert both["inline_keyboard"][1] == [{"text": stick_link.UPDATE_TEXT,
                                           "url": "https://x.trycloudflare.com/stick#t=abc"}]


class FakeBot:
    def __init__(self) -> None:
        self.calls: list[tuple[str, dict]] = []

    async def __call__(self, method: str, data: dict) -> Any:
        self.calls.append((method, data))
        return {"ok": True, "result": {"message_id": 77}}


def test_stick_pairs_rotates_and_repoints_the_pin_at_the_live_address(tmp_path: Path) -> None:
    bot = FakeBot()
    cfg = MiniAppConfig(enabled=True, token=TOKEN, owner_ids=frozenset({OWNER}), ledger_path=tmp_path / "l.json")
    app = miniapp.MiniApp(cfg, bot=bot, tunnel_factory=lambda: None, pins_path=tmp_path / "pin.json")

    async def go() -> tuple[str, str, str]:
        down = await app.stick_link()                                      # no tunnel yet
        app.url = "https://live.trycloudflare.com"
        first = await app.stick_link()
        second = await app.stick_link(rotate=True)
        return down, first, second

    down, first, second = asyncio.run(go())
    assert down == "" and app.server.stick.current() == urlsplit(second).fragment[2:]
    assert first.startswith("https://live.trycloudflare.com/stick#t=") and first != second
    markups = [d["reply_markup"] for m, d in bot.calls if "reply_markup" in d]
    assert markups and markups[-1]["inline_keyboard"][1][0]["url"] == second


def test_pairing_without_a_tunnel_keeps_no_token(tmp_path: Path) -> None:
    cfg = MiniAppConfig(enabled=True, token=TOKEN, owner_ids=frozenset({OWNER}), ledger_path=tmp_path / "l.json")
    app = miniapp.MiniApp(cfg, bot=FakeBot(), tunnel_factory=lambda: None, pins_path=tmp_path / "pin.json")
    assert asyncio.run(app.stick_link()) == ""
    assert not (tmp_path / "stick-link.json").exists()


# ---- the rendezvous: the phone finds the new address by itself (2026-10-04) -----------------------------------

def test_the_rendezvous_topic_is_the_phones_and_says_nothing_about_the_token() -> None:
    token = "a" * 40
    # the same vector ios/EveLink/Tests/main.swift checks Rendezvous.topic against
    assert stick_link.rendezvous_topic(token) == "evelink-a7c4bc0e3e702a6abc8c7afe80e62b5d"
    assert token not in stick_link.rendezvous_topic(token)
    assert stick_link.rendezvous_topic("b" * 40) != stick_link.rendezvous_topic(token)


def test_publish_posts_only_the_address_to_the_tokens_topic() -> None:
    sent: list[tuple[str, bytes]] = []
    ok = stick_link.publish_address("https://new-name.trycloudflare.com/", "a" * 40, "https://ntfy.example",
                                    post=lambda url, body: sent.append((url, body)) or 200)
    assert ok
    assert sent == [("https://ntfy.example/evelink-a7c4bc0e3e702a6abc8c7afe80e62b5d",
                     b"https://new-name.trycloudflare.com")]


def test_publish_failures_are_quiet_falses() -> None:
    def boom(url: str, body: bytes) -> int:
        raise OSError("no network")
    assert not stick_link.publish_address("https://x.trycloudflare.com", "a" * 40, post=boom)
    assert not stick_link.publish_address("https://x.trycloudflare.com", "a" * 40, post=lambda u, b: 500)
    assert not stick_link.publish_address("", "a" * 40, post=lambda u, b: 200)
    assert not stick_link.publish_address("https://x.trycloudflare.com", "", post=lambda u, b: 200)


def test_the_rendezvous_switch_and_server() -> None:
    assert stick_link.rendezvous_base({}) == "https://ntfy.sh"
    assert stick_link.rendezvous_base({"EVE_LINK_RENDEZVOUS": "0"}) == ""
    assert stick_link.rendezvous_base({"EVE_LINK_RENDEZVOUS_URL": "https://ntfy.home/"}) == "https://ntfy.home"


def test_the_miniapp_posts_a_new_address_once_and_again_after_the_repost_time(monkeypatch, tmp_path: Path) -> None:
    posts: list[tuple[str, str]] = []
    monkeypatch.setattr(stick_link, "publish_address",
                        lambda url, token, base: posts.append((url, token)) or True)
    monkeypatch.delenv("EVE_LINK_RENDEZVOUS", raising=False)
    clock = [1000.0]                     # the loop's own clock: patching time.monotonic would stop asyncio's

    class Server:
        stick = stick_link.LinkToken(tmp_path / "stick-link.json")

    app = miniapp.MiniApp.__new__(miniapp.MiniApp)
    app.server = Server()
    app.url = "https://first.trycloudflare.com"
    token = app.server.stick.ensure()

    async def go() -> None:
        task = asyncio.create_task(app._announce_loop(every=0.01, clock=lambda: clock[0]))
        await asyncio.sleep(0.05)
        assert posts == [("https://first.trycloudflare.com", token)]          # once, not every tick
        app.url = "https://second.trycloudflare.com"
        await asyncio.sleep(0.05)
        assert posts[-1] == ("https://second.trycloudflare.com", token)
        n = len(posts)
        clock[0] += stick_link.REPOST_SECS + 1                                # ntfy forgets after 12 h
        await asyncio.sleep(0.05)
        assert len(posts) == n + 1
        task.cancel()

    asyncio.run(go())


def _announcer(tmp_path: Path, url: str) -> tuple[Any, str]:
    class Server:
        stick = stick_link.LinkToken(tmp_path / "stick-link.json")

    app = miniapp.MiniApp.__new__(miniapp.MiniApp)
    app.server = Server()
    app.url = url
    return app, app.server.stick.ensure()


def test_the_announcer_records_the_address_it_posted_not_the_one_after(monkeypatch, tmp_path: Path) -> None:
    # verification/Eve/Rendezvous.lean `trace` (current_violates): address A is posted; while the post is in flight
    # the tunnel restarts as B; the post returns True. The topic says A, so the next poll must post B. Before the
    # fix `sent` recorded B and nothing went out until REPOST_SECS (current_posts_only_at_repost).
    monkeypatch.delenv("EVE_LINK_RENDEZVOUS", raising=False)
    app, token = _announcer(tmp_path, "https://a.trycloudflare.com")
    posts: list[tuple[str, str]] = []

    def publish(url: str, tok: str, base: str) -> bool:
        posts.append((url, tok))
        if url == "https://a.trycloudflare.com":
            app.url = ""                                    # the tunnel exits mid-post...
            app.url = "https://b.trycloudflare.com"         # ...and a new one starts
        return True
    monkeypatch.setattr(stick_link, "publish_address", publish)

    async def go() -> None:
        task = asyncio.create_task(app._announce_loop(every=0.01, clock=lambda: 1000.0))   # the clock stands still
        await asyncio.sleep(0.1)
        task.cancel()

    asyncio.run(go())
    assert posts == [("https://a.trycloudflare.com", token), ("https://b.trycloudflare.com", token)]


def test_a_raising_post_is_a_failed_post_and_the_announcer_goes_on(monkeypatch, tmp_path: Path) -> None:
    # Rendezvous.lean `trace2` (current_dies, current_dead_forever): the post raises; before the fix the exception
    # ended the fire-and-forget task and nothing was ever posted again. Now it is a failed post, retried next poll.
    monkeypatch.delenv("EVE_LINK_RENDEZVOUS", raising=False)
    app, token = _announcer(tmp_path, "https://a.trycloudflare.com")
    posts: list[str] = []

    def publish(url: str, tok: str, base: str) -> bool:
        posts.append(url)
        if len(posts) == 1:
            raise http.client.BadStatusLine("garbage")      # an exception publish_address did not catch
        return True
    monkeypatch.setattr(stick_link, "publish_address", publish)

    async def go() -> None:
        task = asyncio.create_task(app._announce_loop(every=0.01, clock=lambda: 1000.0))
        await asyncio.sleep(0.05)
        assert not task.done()
        app.url = "https://b.trycloudflare.com"
        await asyncio.sleep(0.05)
        assert not task.done()
        task.cancel()

    asyncio.run(go())
    assert posts == ["https://a.trycloudflare.com", "https://a.trycloudflare.com", "https://b.trycloudflare.com"]


@pytest.mark.parametrize("exc", [http.client.BadStatusLine("x"), ValueError("unknown url type: 'ntfy.sh/t'")])
def test_publish_never_raises(exc: Exception) -> None:
    # Rendezvous.lean finding 2: a reply that breaks http.client, or a base with no scheme, raised out of publish
    def boom(url: str, body: bytes) -> int:
        raise exc
    assert stick_link.publish_address("https://x.trycloudflare.com", "a" * 40, "ntfy.sh", post=boom) is False


def test_the_rendezvous_off_posts_nothing(monkeypatch, tmp_path: Path) -> None:
    posts: list[Any] = []
    monkeypatch.setattr(stick_link, "publish_address", lambda *a: posts.append(a) or True)
    monkeypatch.setenv("EVE_LINK_RENDEZVOUS", "0")
    app = miniapp.MiniApp.__new__(miniapp.MiniApp)

    class Server:
        stick = stick_link.LinkToken(tmp_path / "stick-link.json")
    app.server = Server()
    app.url = "https://x.trycloudflare.com"
    app.server.stick.ensure()
    asyncio.run(asyncio.wait_for(app._announce_loop(every=0.01), 1))   # returns at once
    assert posts == []
