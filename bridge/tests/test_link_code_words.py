"""eve's code words typed in the Eve Link app's chat: the app has no / menu (owner, 2026-10-10: "i dont think we need
/ commands anymore since it can parse my natural language commands pretty easily"), but a "/apps" the owner types is
an ordinary turn, which the inlet dispatches as Telegram's do. Every word Telegram's menu lists is accepted on that
path, as test_telegram.py holds for Telegram; and Telegram's menu is set from TelegramInlet.menu_commands."""
from __future__ import annotations

import asyncio

import pytest
from test_telegram import MENU_CHECKS, OWNER, FakeApi, FakeCreate, Rig, _menu_rig, say, settle

from eve_bridge import telegram


def test_telegrams_menu_is_set_from_the_same_list() -> None:
    rig = Rig(FakeApi(), FakeCreate())
    rig.inlet.chief = object()
    asyncio.run(rig.inlet._set_commands())
    assert rig.api.commands and all(menu == rig.inlet.menu_commands() for menu, _ in rig.api.commands)


@pytest.mark.parametrize("command", sorted(MENU_CHECKS))
def test_each_menu_word_is_accepted_typed_in_the_app(command: str) -> None:
    """The app sends a typed word as its turn's text, "/apps", through ``hear(app=<turn id>)``. With no app stream
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
