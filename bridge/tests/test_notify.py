"""Notifications through ntfy (notify.py): what Telegram would have pushed, coalesced, never in the way."""

from __future__ import annotations

import asyncio
import logging
from pathlib import Path

import pytest

from eve_bridge import cli, notify, telegram
from eve_bridge.telegram import MutedBotApi, configured

TOKEN = "a" * 40
OWNER = 42


class FakePost:
    def __init__(self, status: int = 200, error: Exception | None = None) -> None:
        self.calls: list[tuple[str, str, dict[str, str]]] = []
        self.status, self.error = status, error

    def __call__(self, url: str, body: bytes, headers: dict[str, str]) -> int:
        self.calls.append((url, body.decode(), headers))
        if self.error is not None:
            raise self.error
        return self.status


def quick(post: FakePost, **kw) -> notify.Notifier:
    return notify.Notifier(lambda: TOKEN, post=post, coalesce_secs=0.02, gap_secs=0.15, **kw)


# ---- the topic -----------------------------------------------------------------------------------------------

def test_the_topic_is_the_phones_and_differs_from_the_rendezvous():
    from eve_bridge import stick_link

    # The same vector is asserted against Swift's NotifyTopic in ios/EveLink/Tests/main.swift.
    assert notify.notify_topic(TOKEN) == "eve-312f09f108bc0795afe2c8447ddfbd6a"
    assert notify.notify_topic(TOKEN) != stick_link.rendezvous_topic(TOKEN)
    assert TOKEN not in notify.notify_topic(TOKEN)
    assert notify.notify_topic("b" * 40) != notify.notify_topic(TOKEN)


def test_the_switches():
    assert notify.configured({}) == notify.NotifyConfig(True, "https://ntfy.sh", True)
    assert not notify.configured({"EVE_NOTIFY": "0"}).enabled
    assert not notify.configured({"EVE_NOTIFY_PREVIEW": "off"}).preview
    assert notify.configured({"EVE_NOTIFY_URL": "https://ntfy.home/"}).base == "https://ntfy.home"
    assert notify.make_notifier({"EVE_NOTIFY": "0"}) is None


# ---- the words -----------------------------------------------------------------------------------------------

def test_plain_text_strips_html_markdown_and_openui_and_caps():
    html = '<b>Watch</b>\n<i>flights</i>\n\nThe fare is <b>$412</b> &amp; falling, <a href="https://e.com">see</a>'
    assert notify.plain_text(html) == "Watch\nflights\nThe fare is $412 & falling, see"
    assert notify.plain_text("**done**: the [report](https://x.y) is in `out.md`") == "done: the report is in out.md"
    assert notify.plain_text("Here:\n```openui-lang\nroot = Card()\n```") == (
        "Here:\n(A table or chart is here: open Eve Link to see it.)")
    long = notify.plain_text("word " * 200)
    assert len(long) == notify.MAX_CHARS and long.endswith("…")


# ---- the hook: MutedBotApi's sends ---------------------------------------------------------------------------

def test_muted_sends_make_exactly_one_coalesced_post():
    post = FakePost()
    n = quick(post)
    api = MutedBotApi(notifier=n)

    async def go():
        await api.send_message(OWNER, "first")
        await api.send_message(OWNER, "the price **dropped**")
        await api.send_inline(OWNER, "Go on the chief's job?", [[("Go", "k1", "success"), ("No", "k2", "")]])
        await asyncio.sleep(0.1)

    asyncio.run(go())
    assert len(post.calls) == 1 and n.posts == 1
    url, body, headers = post.calls[0]
    assert url == "https://ntfy.sh/" + notify.notify_topic(TOKEN)
    assert body == "Go on the chief's job? (+2 more)"
    assert headers == {"Title": "eve", "Click": "evelink://chat"}


def test_edits_typing_drafts_reactions_commands_and_progress_cards_do_not_notify():
    post = FakePost()
    api = MutedBotApi(notifier=quick(post))

    async def go():
        await api.edit_message(OWNER, 5, "edited")
        await api.drop_buttons(OWNER, 5)
        await api.typing(OWNER)
        await api.send_draft(OWNER, 1, "")
        await api.react(OWNER, 5, "")
        await api.answer_callback("q", "ok")
        await api.set_commands([("jobs", "your jobs")], OWNER)
        await api.send_inline(OWNER, "Working on it", [[(telegram.STOP_BUTTON, "s1", "danger")]])
        await asyncio.sleep(0.1)

    asyncio.run(go())
    assert post.calls == []


def test_a_picture_and_a_file_notify(tmp_path: Path):
    post = FakePost()
    pic = tmp_path / "p.jpg"
    pic.write_bytes(b"x")

    async def go():
        api = MutedBotApi(notifier=quick(post))
        await api.send_photo(OWNER, pic, "")
        await asyncio.sleep(0.1)
        await api.send_document(OWNER, pic, "the notes")
        await asyncio.sleep(0.3)

    asyncio.run(go())
    assert [c[1] for c in post.calls] == ["eve sent a picture", "the notes"]


def test_at_most_one_post_per_gap_the_rest_folded():
    post = FakePost()
    n = quick(post)

    async def go():
        n.offer("one")
        await asyncio.sleep(0.05)          # posted
        n.offer("two")
        await asyncio.sleep(0.05)          # inside the gap: held
        assert len(post.calls) == 1
        n.offer("three")
        await asyncio.sleep(0.2)           # the gap is over: one post with both

    asyncio.run(go())
    assert [c[1] for c in post.calls] == ["one", "three (+1 more)"]


def test_preview_off_sends_no_words():
    post = FakePost()
    n = quick(post, preview=False)

    async def go():
        n.offer("the secret code is 1234")
        n.offer("and another")
        await asyncio.sleep(0.1)

    asyncio.run(go())
    assert [c[1] for c in post.calls] == ["eve has 2 new messages"]
    assert "1234" not in post.calls[0][1]


def test_no_token_posts_nothing():
    post = FakePost()
    n = notify.Notifier(lambda: "", post=post, coalesce_secs=0.01)

    async def go():
        n.offer("hello")
        await asyncio.sleep(0.05)

    asyncio.run(go())
    assert post.calls == []


@pytest.mark.parametrize("post", [FakePost(error=OSError("https://ntfy.sh/eve-secret-topic down")),
                                  FakePost(error=ValueError("bad")), FakePost(status=500)])
def test_failures_never_raise_and_never_log_the_topic_or_text(post, caplog):
    caplog.set_level(logging.DEBUG)
    n = quick(post)
    api = MutedBotApi(notifier=n)

    async def go():
        sent = await api.send_message(OWNER, "private words")
        await asyncio.sleep(0.1)
        return sent

    assert asyncio.run(go()) is None
    assert n.posts == 0
    assert "eve-" not in caplog.text and "private words" not in caplog.text
    assert any(r.levelno == logging.WARNING for r in caplog.records)


def test_offer_never_blocks_without_a_loop_and_never_raises():
    n = quick(FakePost())
    n.offer("no loop here")                 # a debug line, nothing else


def test_close_cancels_a_pending_post():
    post = FakePost()

    async def go():
        api = MutedBotApi(notifier=notify.Notifier(lambda: TOKEN, post=post, coalesce_secs=5))
        await api.send_message(OWNER, "later")
        await api.close()

    asyncio.run(go())
    assert post.calls == []


def test_make_inlet_lends_the_notifier_in_app_mode_and_not_when_off(monkeypatch):
    monkeypatch.setenv("OPENROUTER_API_KEY", "sk-test")
    seen = {}

    class Inlet:
        def __init__(self, config, api, *a, **k):
            seen["api"] = api

    monkeypatch.setattr(telegram, "TelegramInlet", Inlet)
    monkeypatch.setattr(notify, "read_token", lambda path=None: "")
    cfg = configured({"EVE_TELEGRAM": "app", "EVE_TELEGRAM_TOKEN": "1:x",
                      "EVE_TELEGRAM_OWNER": str(OWNER)})
    monkeypatch.delenv("EVE_NOTIFY", raising=False)
    telegram.make_inlet(cfg)
    assert isinstance(seen["api"].notifier, notify.Notifier)
    monkeypatch.setenv("EVE_NOTIFY", "0")
    telegram.make_inlet(cfg)
    assert seen["api"].notifier is None


# ---- the CLI ---------------------------------------------------------------------------------------------------

def test_cli_topic_and_test(monkeypatch, capsys):
    monkeypatch.setattr(cli, "load_env_file", lambda *a, **k: None)
    monkeypatch.setattr(notify, "read_token", lambda path=None: TOKEN)
    post = FakePost()
    monkeypatch.setattr(notify, "_post", post)
    monkeypatch.setattr(notify.post_once, "__defaults__", (notify.NOTIFY_URL, post))
    monkeypatch.delenv("EVE_NOTIFY_URL", raising=False)
    assert cli.main(["notify", "topic"]) == 0
    out = capsys.readouterr().out
    topic = notify.notify_topic(TOKEN)
    assert topic in out and f"https://ntfy.sh/{topic}" in out and "App Store" in out
    assert TOKEN not in out
    assert cli.main(["notify", "test"]) == 0
    assert post.calls and post.calls[0][1] == notify.TEST_LINE


def test_cli_without_a_pairing(monkeypatch, capsys):
    monkeypatch.setattr(cli, "load_env_file", lambda *a, **k: None)
    monkeypatch.setattr(notify, "read_token", lambda path=None: "")
    assert cli.main(["notify", "topic"]) == 1
    assert "not paired" in capsys.readouterr().out


# ---- through the inlet: an app turn's reply goes to the app, not to a notification ---------------------------

def test_app_turn_replies_do_not_notify_but_unasked_lines_do():
    from test_telegram import FakeCreate, Rig

    post = FakePost()
    api = MutedBotApi(notifier=quick(post))
    rig = Rig(api, FakeCreate())
    rig.inlet._chat_id = OWNER
    to_app: list[str] = []
    reading = {"on": True}

    def app_out(turn: str, text: str, kind: str) -> bool:
        if reading["on"]:
            to_app.append(text)
        return reading["on"]

    rig.inlet.app_out = app_out

    async def in_app_turn(text: str) -> None:
        token = telegram._FROM_APP.set("turn-1")
        try:
            await rig.inlet._say(OWNER, text)
        finally:
            telegram._FROM_APP.reset(token)

    async def go():
        await in_app_turn("Here is your answer.")
        await asyncio.sleep(0.1)
        assert to_app == ["Here is your answer."] and post.calls == []      # in the app, no notification
        reading["on"] = False
        await in_app_turn("The task finished.")                              # the app hung up: notify
        await asyncio.sleep(0.1)
        assert [c[1] for c in post.calls] == ["The task finished."]
        assert await rig.inlet.tell_owner("Fare dropped to $412", about="watch: fare")   # a watch alert
        await asyncio.sleep(0.3)

    asyncio.run(go())
    assert len(post.calls) == 2 and post.calls[1][1].endswith("Fare dropped to $412")
