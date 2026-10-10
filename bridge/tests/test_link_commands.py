"""link_chat.py ``GET /api/link/commands``: the / menu the Eve Link app's composer offers.

Its door (the link token, no Origin, GET only), that its list is the inlet's own (TelegramInlet.menu_commands, the
list Telegram's menu gets: /jobs only while the chief is on), and that every word it lists is accepted when the app
sends it as a turn, as test_telegram.py holds for the words Telegram's menu sends."""
from __future__ import annotations

import asyncio
import json
import re
from pathlib import Path

import pytest
from test_link_chat import http, serve
from test_telegram import MENU_CHECKS, OWNER, FakeApi, FakeCreate, Rig, _menu_rig, say, settle

from eve_bridge import link_chat, telegram

PATH = link_chat.COMMANDS_PATH


async def commands(port: int, token: str) -> list[dict[str, str]]:
    status, _, body = await http(port, "GET", PATH, token=token)
    assert status == 200, body
    return json.loads(body)["commands"]


def test_the_menu_needs_the_link_token_and_no_origin(tmp_path: Path) -> None:
    rig = Rig(FakeApi(), FakeCreate())

    async def go() -> None:
        server, port, token = await serve(tmp_path, rig.inlet)
        assert (await http(port, "GET", PATH))[0] == 401                                  # no token
        assert (await http(port, "GET", PATH, token=token[:-1]))[0] == 401                 # nearly right
        assert (await http(port, "GET", PATH, token=token, origin="https://evil.example"))[0] == 403
        assert (await http(port, "GET", PATH, token=token, origin="null"))[0] == 403
        assert (await http(port, "POST", PATH, {"x": 1}, token=token))[0] == 405          # read-only
        assert (await http(port, "GET", PATH, token=token))[0] == 200                      # positive control
        await server.close()

    asyncio.run(go())


def test_the_menu_is_the_inlets_own_list(tmp_path: Path) -> None:
    rig = Rig(FakeApi(), FakeCreate())

    async def go() -> None:
        server, port, token = await serve(tmp_path, rig.inlet)
        got = await commands(port, token)
        assert [(c["command"], c["description"]) for c in got] == list(telegram.BOT_COMMANDS)
        assert list(rig.inlet.menu_commands()) == list(telegram.BOT_COMMANDS)
        await server.close()

    asyncio.run(go())


def test_the_menu_offers_jobs_only_while_the_chief_is_on(tmp_path: Path) -> None:
    on = Rig(FakeApi(), FakeCreate())
    on.inlet.chief = object()                                     # menu_commands only asks whether there is one

    async def go() -> None:
        server, port, token = await serve(tmp_path, on.inlet)
        got = [(c["command"], c["description"]) for c in await commands(port, token)]
        assert got[-2:] == [telegram.JOBS_MENU, telegram.BOT_COMMANDS[-1]]
        assert len(got) == len(telegram.BOT_COMMANDS) + 1
        assert tuple(got) == on.inlet.menu_commands()             # the same list Telegram's menu is set from
        await server.close()

    asyncio.run(go())


def test_telegrams_menu_is_set_from_the_same_list(tmp_path: Path) -> None:
    rig = Rig(FakeApi(), FakeCreate())
    rig.inlet.chief = object()
    asyncio.run(rig.inlet._set_commands())
    assert rig.api.commands and all(menu == rig.inlet.menu_commands() for menu, _ in rig.api.commands)


def test_no_inlet_is_the_no_chat_503(tmp_path: Path) -> None:
    async def go() -> None:
        server, port, token = await serve(tmp_path, None)
        status, _, body = await http(port, "GET", PATH, token=token)
        assert status == 503 and json.loads(body)["error"] == link_chat.NO_CHAT
        await server.close()

    asyncio.run(go())


def test_the_menu_does_not_wait_on_an_owner_chat(tmp_path: Path) -> None:
    """Before any owner has texted (no ``_chat_id``) the turn and messages doors are 503, but the menu only names
    words, so the app has it from the first launch."""
    rig = Rig(FakeApi(), FakeCreate())
    rig.inlet._chat_id = None

    async def go() -> None:
        server, port, token = await serve(tmp_path, rig.inlet)
        assert len(await commands(port, token)) == len(telegram.BOT_COMMANDS)
        assert (await http(port, "GET", link_chat.MESSAGES_PATH, token=token))[0] == 503   # the contrast
        await server.close()

    asyncio.run(go())


@pytest.mark.parametrize("command", sorted(MENU_CHECKS))
def test_each_menu_word_is_accepted_as_the_app_sends_it(command: str) -> None:
    """The app sends a tapped row as its turn's text, "/apps", through ``hear(app=<turn id>)``. With no app stream
    reading (``app_out`` unset), what the word says goes to Telegram, so test_telegram.py's checks apply as-is."""
    before, check = MENU_CHECKS[command]
    api = FakeApi()
    rig = _menu_rig(api, FakeCreate(say("Your day.")) if command == "rundown" else None)
    rig.inlet._chat_id = OWNER

    async def go() -> None:
        for i, text in enumerate(before + [f"/{command}"]):
            rig.inlet.hear(text, on_call=False, app=f"turn-{i}")
            await settle()
            await asyncio.gather(*list(rig.inlet._jobs))
        await settle()
        check(rig)
        if command != "rundown":
            assert rig.create.requests == []                              # code words: no model call

    asyncio.run(go())


def test_every_listed_word_is_one_the_app_path_was_checked_with() -> None:
    assert sorted(c for c, _ in telegram.BOT_COMMANDS) == sorted(MENU_CHECKS)


def test_the_apps_built_in_menu_is_telegrams_base_menu() -> None:
    """The app falls back to its own copy of the base words when the list cannot be read (an older eve): that copy
    must be BOT_COMMANDS, word for word, or the fallback offers a word eve does not know."""
    swift = (Path(__file__).resolve().parents[2] / "ios/EveLink/Shared/SlashCommands.swift").read_text()
    block = swift.split("static let builtIn", 1)[1].split("\n    ]\n", 1)[0]
    pairs = re.findall(r'SlashCommand\(command: "([^"]+)", description: "([^"]*)"\)', block)
    assert tuple(pairs) == telegram.BOT_COMMANDS
