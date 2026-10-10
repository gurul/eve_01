"""link_reactions.py and its wiring: eve's receipts as reactions on the owner's messages in the Eve Link app
(owner, 2026-10-09: "reactions for when it get image or takes not or sees text … like telegram").

The store on its own (ids, Telegram's one-reaction semantics, the file); then the real TelegramInlet behind the real
link door: an app turn's receipts land in the app (never in Telegram, and with no fallback line), the round of only
receipt tools still ends the turn, 👀 sits on a photo while eve reads it, /api/link/messages carries the reaction
and a long poll wakes on a change. Telegram's own path is checked to be what it was."""
from __future__ import annotations

import asyncio
import json
import time
from pathlib import Path
from typing import Any

import pytest
from test_link_chat import FakeInlet, events, http, messages, real_rig, serve, store_at, turn
from test_link_uploads import jpeg, turn_with, upload
from test_telegram import OWNER, FakeApi, FakeCreate, Rig, call, say

from eve_bridge import link_chat, link_reactions, telegram
from eve_bridge.link_reactions import Reactions, clean
from eve_bridge.link_uploads import UploadStore

CID = "c-0123456789"           # a client_id the app sends: also the turn's id, so the page knows its bubble


# ---- the store -------------------------------------------------------------------------------------------

def test_one_reaction_per_message_a_new_one_replaces_it_and_empty_takes_it_off() -> None:
    r = Reactions(clock=lambda: 1000.0)
    heard: list[str] = []
    r.listeners.append(heard.append)
    mid = r.mint("turn-aaaaaaaa")
    assert mid < 0 and Reactions.is_app(mid) and not Reactions.is_app(5)
    assert r.mint("turn-aaaaaaaa") == mid and r.mint("turn-bbbbbbbb") != mid      # stable per message
    v0 = r.version
    r.set(mid, "👀")
    assert r.snapshot() == {"turn-aaaaaaaa": "👀"} and r.version == v0 + 1
    r.set(mid, "👍")                                                            # replaces, never adds
    assert r.snapshot() == {"turn-aaaaaaaa": "👍"} and r.get("turn-aaaaaaaa") == "👍"
    r.set(mid, "👍")                                                            # the same: no change, no wake
    assert r.version == v0 + 2 and heard == ["turn-aaaaaaaa"] * 2
    r.set(mid, "")
    assert r.snapshot() == {} and r.version == v0 + 3 and len(heard) == 3
    r.set(mid, "")                                                              # nothing to take off
    assert r.version == v0 + 3


def test_an_unknown_id_or_a_word_is_refused() -> None:
    r = Reactions()
    with pytest.raises(KeyError):
        r.set(-99, "👍")                                                        # never minted
    mid = r.mint("turn-aaaaaaaa")
    for bad in ("ok", "<b>", "👍 ", "1", "中", "👍" * 9):
        with pytest.raises(ValueError):
            r.set(mid, bad)
    with pytest.raises(ValueError):
        r.mint("../etc")
    assert r.snapshot() == {}


def test_clean_keeps_only_a_short_emoji() -> None:
    for good in ("👍", "✍", "🏆", "👀", "👍🏽", "❤️", "👨‍👩‍👧"):
        assert clean(good) == good
    for bad in ("", "a", "ok", " ", "👍 ", "<script>", "中", "é", None, 7, ["👍"], "👍" * 9):
        assert clean(bad) == ""


def test_reactions_are_kept_in_their_file_and_junk_in_it_is_dropped(tmp_path: Path) -> None:
    path = tmp_path / link_reactions.FILE_NAME
    r = Reactions(path)
    r.set(r.mint("turn-aaaaaaaa"), "🏆")
    assert json.loads(path.read_text()) == {"turn-aaaaaaaa": "🏆"}
    assert oct(path.stat().st_mode & 0o777) == "0o600"
    assert Reactions(path).snapshot() == {"turn-aaaaaaaa": "🏆"}                 # after a restart
    path.write_text(json.dumps({"turn-aaaaaaaa": "🏆", "../x": "👍", "turn-bbbbbbbb": "<b>hi</b>", "turn-c": 3}))
    assert Reactions(path).snapshot() == {"turn-aaaaaaaa": "🏆"}
    path.write_text("not json")
    assert Reactions(path).snapshot() == {}                                     # never fails the daemon


def test_eyes_are_never_kept_in_the_file_so_a_crash_cannot_leave_them_on(tmp_path: Path) -> None:
    """Review, 2026-10-09: a 👀 saved before a crash came back on restart with no id left to take it off."""
    assert telegram.SEEN_REACTION in link_reactions.TRANSIENT
    path = tmp_path / link_reactions.FILE_NAME
    r = Reactions(path)
    r.set(r.mint("turn-aaaaaaaa"), "🏆")
    r.set(r.mint("turn-bbbbbbbb"), telegram.SEEN_REACTION)
    assert r.snapshot() == {"turn-aaaaaaaa": "🏆", "turn-bbbbbbbb": "👀"}         # shown while it is true
    assert json.loads(path.read_text()) == {"turn-aaaaaaaa": "🏆"}               # never written
    assert Reactions(path).snapshot() == {"turn-aaaaaaaa": "🏆"}                 # the "crash": gone on restart
    # a file written before this fix, holding a 👀: dropped on load
    path.write_text(json.dumps({"turn-aaaaaaaa": "🏆", "turn-bbbbbbbb": "👀"}))
    assert Reactions(path).snapshot() == {"turn-aaaaaaaa": "🏆"}


def test_only_the_newest_reactions_are_kept() -> None:
    r = Reactions()
    for i in range(link_reactions.MAX_KEPT + 5):
        r.set(r.mint(f"turn-{i:08d}"), "👍")
    kept = r.snapshot()
    assert len(kept) == link_reactions.MAX_KEPT and "turn-00000000" not in kept
    assert f"turn-{link_reactions.MAX_KEPT + 4:08d}" in kept


# ---- the inlet: where a receipt goes ---------------------------------------------------------------------

def test_a_receipt_on_an_app_message_lands_in_the_app_not_in_telegram() -> None:
    api = FakeApi()
    rig = Rig(api, FakeCreate())
    mid = rig.inlet.app_reactions.mint("turn-aaaaaaaa")

    async def go() -> None:
        assert await rig.inlet._receipt(OWNER, mid, telegram.SEEN_REACTION)
        assert rig.inlet.app_reactions.snapshot() == {"turn-aaaaaaaa": "👀"}
        assert await rig.inlet._receipt(OWNER, mid, telegram.TYPED_REACTION, "Typed.")
        assert rig.inlet.app_reactions.snapshot() == {"turn-aaaaaaaa": "👍"}       # replaced
        await rig.inlet._unreact(OWNER, mid)
        assert rig.inlet.app_reactions.snapshot() == {}
        assert api.reactions == [] and api.sent == []                          # Telegram never saw any of it
        # an app id the store let go of: the line it stands for, as when Telegram refuses one
        assert not await rig.inlet._receipt(OWNER, -12345, telegram.TYPED_REACTION, "Typed.")
        assert api.sent == [(OWNER, "Typed.")]
        # positive control: a Telegram message's reaction still goes to Telegram
        assert await rig.inlet._receipt(OWNER, 7, telegram.STAR_REACTION)
        assert api.reactions == [(OWNER, 7, telegram.STAR_REACTION)]

    asyncio.run(go())


def test_an_app_message_id_is_never_a_telegram_reply_link() -> None:
    api = FakeApi()
    rig = Rig(api, FakeCreate())
    asyncio.run(rig.inlet._say(OWNER, "Back.", reply_to=-3))
    asyncio.run(rig.inlet._say(OWNER, "Here.", reply_to=9))
    assert api.sent == [(OWNER, "Back."), (OWNER, "Here.")] and api.replies == [("Here.", 9)]


def test_words_heard_from_the_app_get_an_id_and_telegram_words_do_not() -> None:
    rig = Rig(FakeApi(), FakeCreate())
    rig.inlet._chat_id = OWNER
    seen: list[telegram.Inbound] = []
    rig.inlet._handle = seen.append                                            # type: ignore[method-assign]
    rig.inlet.hear("hi", on_call=False, app="turn-aaaaaaaa")
    rig.inlet.hear("hi", on_call=False)
    rig.inlet.hear("stop", on_call=False, app="turn-bbbbbbbb")
    assert seen[0].message_id < 0 and seen[1].message_id == 0 and seen[2].message_id < 0
    assert seen[0].message_id == rig.inlet.app_reactions.mint("turn-aaaaaaaa")


# ---- the whole path: app turn → reaction → /api/link/messages ----------------------------------------------

def test_a_star_from_the_app_is_a_trophy_on_the_owners_bubble_with_no_second_model_call(tmp_path: Path) -> None:
    rig, memory = real_rig(tmp_path, call("remember", {"claim": "I like ramen"}))

    async def go() -> None:
        server, port, token = await serve(tmp_path, rig.inlet, settle_secs=0.01)
        status, _, body = await turn(port, token, "remember that I like ramen", CID)
        assert status == 200
        contents, done, _ = events(body)
        assert done and contents == []                                          # the 🏆 is the receipt, no line
        assert len(rig.create.requests) == 1                                   # and no second model call
        assert rig.api.reactions == [] and rig.api.sent == []                  # nothing in Telegram
        assert rig.inlet.app_reactions.snapshot() == {CID: telegram.STAR_REACTION}
        owner = [ln for ln in memory.said() if ln["who"] == "owner"]
        assert owner[0]["mid"] == CID and owner[0]["via"] == "app"
        page = await messages(port, token)
        first = page["messages"][0]
        assert first["role"] == "owner" and first["rid"] == CID and first["reaction"] == "🏆"
        assert page["reactions"] == {CID: "🏆"}
        assert page["cursor"].endswith(f"~{rig.inlet.app_reactions.version}")
        await server.close()

    asyncio.run(go())


def test_take_notes_starting_is_a_quill_in_the_app_and_still_the_models_answer_in_telegram(tmp_path: Path) -> None:
    class Taker:
        def __init__(self) -> None:
            self.started = 0

        def start(self) -> dict[str, Any]:
            self.started += 1
            return {"ok": True, "started": True}

    taker = Taker()
    rig, _ = real_rig(tmp_path, call("take_notes", {"action": "start"}), notes=lambda: taker)

    async def go() -> None:
        server, port, token = await serve(tmp_path, rig.inlet, settle_secs=0.01)
        _, _, body = await turn(port, token, "start taking notes", CID)
        assert events(body)[0] == [] and taker.started == 1 and len(rig.create.requests) == 1
        assert rig.inlet.app_reactions.snapshot() == {CID: telegram.VAULT_REACTION}
        await server.close()

    asyncio.run(go())
    # Telegram's set is unchanged: take_notes there is still news the model words
    api = FakeApi()
    tg = Rig(api, FakeCreate(call("take_notes", {"action": "start"}), say("Taking notes now.")), notes=lambda: Taker())
    asyncio.run(tg.inlet._turn(telegram.Inbound(OWNER, OWNER, "start taking notes", message_id=4)))
    assert api.reactions == [] and api.sent == [(OWNER, "Taking notes now.")] and len(tg.create.requests) == 2


def test_an_app_take_notes_start_while_already_running_is_said_not_a_silent_quill(tmp_path: Path) -> None:
    """Review, 2026-10-09: an "already taking notes" start is news, so the model words it and no ✍ goes on."""
    class Taker:
        def start(self) -> dict[str, Any]:
            return {"ok": True, "already": True, "minutes": 3}

    rig, _ = real_rig(tmp_path, call("take_notes", {"action": "start"}), say("Already taking notes, 3 minutes in."),
                      notes=lambda: Taker())

    async def go() -> None:
        server, port, token = await serve(tmp_path, rig.inlet, settle_secs=0.01)
        _, _, body = await turn(port, token, "take notes", CID)
        assert events(body)[0] == ["Already taking notes, 3 minutes in."] and len(rig.create.requests) == 2
        assert rig.inlet.app_reactions.snapshot() == {}
        await server.close()

    asyncio.run(go())


def test_an_app_take_notes_stop_or_status_is_not_a_receipt(tmp_path: Path) -> None:
    class Taker:
        async def stop(self, reason: str) -> dict[str, Any]:
            return {"ok": True, "file": "notes.md"}

    rig, _ = real_rig(tmp_path, call("take_notes", {"action": "stop"}), say("Saved the notes to notes.md."),
                      notes=lambda: Taker())

    async def go() -> None:
        server, port, token = await serve(tmp_path, rig.inlet, settle_secs=0.01)
        _, _, body = await turn(port, token, "stop taking notes", CID)
        assert events(body)[0] == ["Saved the notes to notes.md."] and len(rig.create.requests) == 2
        assert rig.inlet.app_reactions.snapshot() == {}
        await server.close()

    asyncio.run(go())


def test_a_photo_from_the_app_wears_eyes_while_eve_reads_it_and_they_go_when_the_turn_ends(tmp_path: Path) -> None:
    during: list[dict[str, str]] = []
    rig, _ = real_rig(tmp_path)

    async def reads() -> dict[str, Any]:
        during.append(rig.inlet.app_reactions.snapshot())
        return say("A red square.")

    rig.create.responses.append(reads)

    async def go() -> None:
        store = UploadStore(tmp_path / "up")
        server, port, token = await serve(tmp_path, rig.inlet, uploads=store, settle_secs=0.01)
        _, got = await upload(port, token, jpeg(), "red.jpg")
        status, body = await turn_with(port, token, "what is this?", [got["id"]], CID)
        assert status == 200 and events(body) == (["A red square."], True, 0)
        assert during == [{CID: telegram.SEEN_REACTION}]                        # 👀 while it was being read
        assert rig.inlet.app_reactions.snapshot() == {}                         # and off once answered
        assert rig.api.reactions == [] and rig.api.sent == []
        await server.close()

    asyncio.run(go())


def test_a_telegram_photo_for_eve_still_gets_no_eyes(tmp_path: Path) -> None:
    from eve_bridge import telegram_images as media

    class Photo(FakeApi):
        async def receive_image(self, item: Any) -> Any:
            return media.ReceivedImage(b"\xff\xd8\xff\xd9", "image/jpeg", "photo.jpg")

    api = Photo()
    rig = Rig(api, FakeCreate(say("A photo.")))
    asyncio.run(rig.inlet._image(telegram.Inbound(OWNER, OWNER, "look", media.Attachment("f"), message_id=5),
                                 "eve", 0))
    assert api.reactions == [] and api.sent == [(OWNER, "A photo.")]


# ---- the long poll -----------------------------------------------------------------------------------------

def test_a_reaction_wakes_the_long_poll_and_an_old_version_answers_at_once(tmp_path: Path) -> None:
    async def go() -> None:
        inlet = FakeInlet(store_at(tmp_path))
        inlet.app_reactions = Reactions()
        mid = inlet.app_reactions.mint(CID)
        server, port, token = await serve(tmp_path, inlet)
        first = await messages(port, token)
        assert first["reactions"] == {} and "~" in first["cursor"]

        async def later() -> None:
            await asyncio.sleep(0.3)
            inlet.app_reactions.set(mid, "👀")

        t0 = time.monotonic()
        got, _ = await asyncio.gather(messages(port, token, f"?after={first['cursor']}&wait=20"), later())
        assert time.monotonic() - t0 < 10                                      # woken, not timed out
        assert got["messages"] == [] and got["reactions"] == {CID: "👀"}
        # the app's cursor names the version it saw: a change it missed between polls answers at once
        inlet.app_reactions.set(mid, "👍")
        t0 = time.monotonic()
        again = await messages(port, token, f"?after={got['cursor']}&wait=20")
        assert time.monotonic() - t0 < 5 and again["reactions"] == {CID: "👍"}
        # nothing changed since: the poll waits as before
        quiet = await messages(port, token, f"?after={again['cursor']}&wait=0.4")
        assert quiet["messages"] == [] and quiet["cursor"] == again["cursor"]
        # a cursor this door never made is refused
        for bad in (f"{again['cursor']}~", f"{again['cursor'].split('~')[0]}~x", "2026-10-09:1~1~2"):
            assert (await http(port, "GET", f"{link_chat.MESSAGES_PATH}?after={bad}", token=token))[0] == 400
        await server.close()

    asyncio.run(go())


def test_the_app_client_id_is_the_turn_id_only_when_it_is_a_plain_unused_id() -> None:
    assert link_chat.turn_id(CID) == CID
    assert link_chat.turn_id(CID, {CID: object()}) != CID                     # in use: a fresh one
    for odd in ("", "short", "has space ok", "a/b/c/d/e/f", "x" * 65):
        got = link_chat.turn_id(odd)
        assert got != odd and len(got) == 16
    assert link_chat.split_cursor("2026-10-09:4~17") == ("2026-10-09:4", 17)
    assert link_chat.split_cursor("2026-10-09:-1") == ("2026-10-09:-1", None)
    assert link_chat.split_cursor(None) == (None, None)
