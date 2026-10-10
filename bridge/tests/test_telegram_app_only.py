"""EVE_TELEGRAM=app: the chat's brain stays (the Eve Link app, calls, the watcher), Telegram is never reached.
And a call's ears drop words in another script when the owner's language is English."""

from __future__ import annotations

import asyncio
from types import SimpleNamespace

import pytest

from eve_bridge import telegram
from eve_bridge.phone_call import foreign
from eve_bridge.telegram import BotApi, MutedBotApi, TelegramInlet, configured

TOKEN = "123456:test-token-not-real"
OWNER = 42


def test_app_turns_on_the_brain_without_the_door():
    cfg = configured({"EVE_TELEGRAM": "app", "EVE_TELEGRAM_TOKEN": TOKEN,
                      "EVE_TELEGRAM_OWNER": str(OWNER)})
    assert cfg.enabled and cfg.app_only
    on = configured({"EVE_TELEGRAM": "1", "EVE_TELEGRAM_TOKEN": TOKEN,
                     "EVE_TELEGRAM_OWNER": str(OWNER)})
    assert on.enabled and not on.app_only
    # still needs an owner: the brain answers one person
    assert not configured({"EVE_TELEGRAM": "app", "EVE_TELEGRAM_TOKEN": TOKEN}).enabled


def test_the_muted_api_sends_nothing_and_every_send_looks_sent():
    api = MutedBotApi()
    assert isinstance(api, BotApi)

    async def go():
        await api.send_message(OWNER, "the receipt for c1")
        msg = await api.send_inline(OWNER, "go?", [[("Go", "k1", "")]])
        await api.typing(OWNER)
        await api.set_commands([("jobs", "your jobs")], OWNER)
        await api.close()
        return msg, await api.get_updates(None), await api.get_me()

    msg, updates, me = asyncio.run(go())
    assert msg == 0 and updates == [] and me["username"] == "muted"
    assert api.dropped >= 3
    assert not hasattr(api, "_client")             # no HTTP client: nothing can leave the Mac


def test_make_inlet_lends_the_muted_api_in_app_mode(monkeypatch):
    monkeypatch.setenv("OPENROUTER_API_KEY", "sk-test")
    seen = {}

    class Inlet:
        def __init__(self, config, api, *a, **k):
            seen["api"] = api

    monkeypatch.setattr(telegram, "TelegramInlet", Inlet)
    cfg = configured({"EVE_TELEGRAM": "app", "EVE_TELEGRAM_TOKEN": TOKEN,
                      "EVE_TELEGRAM_OWNER": str(OWNER)})
    telegram.make_inlet(cfg)
    assert isinstance(seen["api"], MutedBotApi)


def test_app_only_run_never_polls_and_shuts_down_cleanly():
    shut = []

    class NoPoll:
        async def get_updates(self, *a, **k):
            raise AssertionError("polled Telegram in app-only mode")

    async def shutdown():
        shut.append(True)

    stub = SimpleNamespace(config=SimpleNamespace(app_only=True, model="m", effort="low"), api=NoPoll(),
                           _shutdown=shutdown)

    async def go():
        task = asyncio.ensure_future(TelegramInlet.run(stub))
        await asyncio.sleep(0.05)
        assert not task.done()
        task.cancel()
        with pytest.raises(asyncio.CancelledError):
            await task

    asyncio.run(go())
    assert shut == [True]


@pytest.mark.parametrize("text", ["あ。", "ご視聴ありがとうございました", "Продолжение следует", "谢谢"])
def test_english_calls_drop_words_in_another_script(text):
    assert foreign(text, "en")


@pytest.mark.parametrize("text", ["Who is the president?", "Play Beyoncé", "café at 5", "ok", "...", "42", ""])
def test_english_and_punctuation_are_kept(text):
    assert not foreign(text, "en")


def test_no_language_or_another_language_keeps_everything():
    assert not foreign("あ。", "")
    assert not foreign("あ。", "ja")


def test_app_only_still_keeps_the_token_out_of_logs():
    import logging

    MutedBotApi(TOKEN)
    try:
        record = logging.getLogRecordFactory()("httpx", logging.INFO, __file__, 1,
                                               f"POST https://api.telegram.org/bot{TOKEN}/sendMessage", None, None)
        assert TOKEN not in record.getMessage()
    finally:
        telegram.unhide_tokens()


def test_the_mini_apps_pin_never_reaches_telegram_in_app_mode():
    from eve_bridge.miniapp import bot_caller

    bot = bot_caller(TOKEN, environ={"EVE_TELEGRAM": "app"})
    try:
        reply = asyncio.run(bot("setChatMenuButton", {"chat_id": OWNER}))
    finally:
        telegram.unhide_tokens()
    assert reply["ok"] is False and "EVE_TELEGRAM=app" in reply["description"]
