"""chrome_consent.ConsentBroker: the owner's Telegram yes/no answers Chrome's "Allow remote debugging?"."""

from __future__ import annotations

import asyncio

import pytest

from eve_bridge.chrome_consent import PRESS_TRIES, QUESTION, ConsentBroker


def rig(reply=None, *, showing_after: int = 0, never_shows: bool = False, no_telegram: bool = False,
        ask_raises: bool = False):
    pressed: list[str] = []
    asked: list[str] = []
    polls = {"n": 0}

    async def showing() -> bool:
        polls["n"] += 1
        return not never_shows and not pressed and polls["n"] > showing_after   # a press dismisses it

    async def pressing(label: str) -> bool:
        pressed.append(label)
        return True

    async def ask(q: str) -> str:
        asked.append(q)
        if ask_raises:
            raise RuntimeError("telegram down")
        if reply is None:
            await asyncio.sleep(10)
        return reply

    broker = ConsentBroker(None if no_telegram else ask, showing=showing, pressing=pressing, appear_secs=0.05,
                           ask_timeout_secs=0.05, poll_secs=0.01, settle_secs=0.001)
    return broker, pressed, asked


@pytest.mark.parametrize("reply", ["yes", "Yes please", "ok", "allow", "go ahead"])
def test_a_clear_yes_presses_allow(reply: str) -> None:
    broker, pressed, asked = rig(reply, showing_after=2)
    assert asyncio.run(broker.answer_own_connection()) == "allowed"
    assert pressed == ["Allow"] and asked == [QUESTION]


@pytest.mark.parametrize("reply", ["no", "nope", "yeah no", "ok wait", "maybe", ""])
def test_anything_else_presses_cancel(reply: str) -> None:
    broker, pressed, _ = rig(reply)
    assert asyncio.run(broker.answer_own_connection()) == "declined" and pressed == ["Cancel"]


def test_silence_and_a_broken_telegram_press_cancel() -> None:
    broker, pressed, _ = rig(None)
    assert asyncio.run(broker.answer_own_connection()) == "unanswered" and pressed == ["Cancel"]
    broker, pressed, _ = rig("yes", ask_raises=True)
    assert asyncio.run(broker.answer_own_connection()) == "unanswered" and pressed == ["Cancel"]


def test_no_way_to_ask_presses_cancel_without_asking() -> None:
    broker, pressed, asked = rig("yes", no_telegram=True)
    assert asyncio.run(broker.answer_own_connection()) == "no_way_to_ask" and pressed == ["Cancel"] and asked == []


def test_no_dialog_means_nothing_is_pressed_or_asked() -> None:
    broker, pressed, asked = rig("yes", never_shows=True)
    assert asyncio.run(broker.answer_own_connection()) == "no_dialog" and pressed == [] and asked == []


def test_only_allow_and_cancel_can_be_pressed() -> None:
    from eve_bridge.chrome_consent import press

    with pytest.raises(ValueError):
        asyncio.run(press("Turn off in settings"))


def _broker(showing_seq, reply="yes", press_ok=True, slow_reply=False):
    told: list[str] = []
    pressed: list[str] = []
    seq = list(showing_seq)

    async def showing() -> bool:
        if pressed and press_ok:
            return False                                   # a press that landed dismisses the dialog
        return seq.pop(0) if len(seq) > 1 else seq[0]

    async def pressing(label: str) -> bool:
        pressed.append(label)
        return press_ok

    async def ask(q: str) -> str:
        if slow_reply:
            await asyncio.sleep(10)
        return reply

    async def tell(text: str) -> None:
        told.append(text)

    b = ConsentBroker(ask, tell_owner=tell, showing=showing, pressing=pressing, appear_secs=1,
                      ask_timeout_secs=2, poll_secs=0.01, settle_secs=0.001)
    return b, pressed, told


def test_a_connection_that_finishes_first_withdraws_the_phone_question() -> None:
    """BrowserLane.connect cancels the broker when the connection is done; the CancelledError skipped the
    cleanup, the Telegram question stayed pending, and the owner's next message was swallowed as its answer."""
    async def go() -> None:
        asked, withdrawn = asyncio.Event(), asyncio.Event()

        async def showing() -> bool:
            return True

        async def pressing(label: str) -> bool:
            return True

        async def ask(q: str) -> str:
            asked.set()
            try:
                await asyncio.Event().wait()
            except asyncio.CancelledError:
                withdrawn.set()
                raise
            return ""

        b = ConsentBroker(ask, showing=showing, pressing=pressing, appear_secs=1, ask_timeout_secs=60,
                          poll_secs=0.001, settle_secs=0.001)
        answering = asyncio.ensure_future(b.answer_own_connection())
        await asked.wait()
        answering.cancel()                                # as connect() does once the connection is up
        await asyncio.gather(answering, return_exceptions=True)
        assert answering.cancelled() and withdrawn.is_set()

    asyncio.run(go())


def test_the_dialog_is_found_in_whichever_chrome_shows_it(monkeypatch: pytest.MonkeyPatch) -> None:
    """With two Chromes running, only the first pgrep PID was searched: the dialog in the other was never seen."""
    import subprocess
    from types import SimpleNamespace

    from eve_bridge import chrome_consent

    heading = {"AXRole": "AXHeading", "AXTitle": chrome_consent.DIALOG_TEXT}
    allow = {"AXRole": "AXButton", "AXTitle": "Allow"}
    apps = {111: {"AXWindows": [{"AXRole": "AXWindow", "AXChildren": []}]},                 # a Chrome with no dialog
            222: {"AXWindows": [{"AXRole": "AXWindow", "AXChildren": [heading, allow]}]}}  # the one that asks
    monkeypatch.setattr(subprocess, "run", lambda *a, **k: SimpleNamespace(stdout="111\n222\n"))
    monkeypatch.setattr(chrome_consent, "_ax", lambda: (lambda pid: apps[pid],
                                                        lambda el, name, _x: (0, el.get(name)),
                                                        lambda el, action: 0))
    assert chrome_consent._find_dialog("Allow") == (True, allow)


def test_answered_at_the_mac_withdraws_the_phone_question() -> None:
    from eve_bridge.chrome_consent import GONE_LINE

    b, pressed, told = _broker([True, True, False], slow_reply=True)     # the dialog vanishes while we wait
    assert asyncio.run(b.answer_own_connection()) == "answered_at_mac"
    assert pressed == [] and told == [GONE_LINE]


def test_a_press_that_fails_while_the_dialog_is_still_up_says_why() -> None:
    from eve_bridge.chrome_consent import PRESS_FAILED_LINE

    b, pressed, told = _broker([True], reply="yes", press_ok=False)
    assert asyncio.run(b.answer_own_connection()) == "press_failed"
    assert pressed == ["Allow"] * PRESS_TRIES and told == [PRESS_FAILED_LINE]


def test_a_press_chrome_ignored_is_repeated_until_the_dialog_is_gone() -> None:
    """Measured 2026-09-24: AXPress in the dialog's first moments reports success and dismisses nothing. The
    daemon logged auto_allowed while the dialog stayed up. Only a dialog that has gone counts as pressed."""
    pressed: list[str] = []

    async def showing() -> bool:
        return len(pressed) < 2                             # the first press does nothing; the second lands

    async def pressing(label: str) -> bool:
        pressed.append(label)
        return True                                         # Chrome says "done" both times

    b = ConsentBroker(None, showing=showing, pressing=pressing, appear_secs=0.05, poll_secs=0.01,
                      settle_secs=0.001, auto_allow=True)
    assert asyncio.run(b.answer_own_connection()) == "auto_allowed" and pressed == ["Allow", "Allow"]

    pressed.clear()

    async def ask(q: str) -> str:
        return "yes"

    b = ConsentBroker(ask, showing=showing, pressing=pressing, appear_secs=0.05, ask_timeout_secs=1,
                      poll_secs=0.01, settle_secs=0.001)
    assert asyncio.run(b.answer_own_connection()) == "allowed" and pressed == ["Allow", "Allow"]


def test_the_phone_wait_ends_before_the_connection_gives_up() -> None:
    from eve_bridge import browser_lane, chrome_consent

    assert chrome_consent.ASK_TIMEOUT_SECS * 1000 < browser_lane.CONSENT_TIMEOUT_MS


def test_a_waiting_question_is_never_overwritten_by_the_chrome_question() -> None:
    from types import SimpleNamespace

    from eve_bridge.daemon import Daemon

    asked = []

    async def ask_user(q, chat, title=""):
        asked.append(q)
        return "yes"

    busy = SimpleNamespace(_telegram=SimpleNamespace(_chat_id=1, _ask_user=ask_user, _awaiting_answer=True))
    with pytest.raises(RuntimeError, match="already waiting"):
        asyncio.run(Daemon._ask_owner_on_phone(busy, "Allow?"))
    assert asked == []


def test_the_standing_yes_presses_allow_without_asking() -> None:
    pressed: list[str] = []
    asked: list[str] = []

    async def showing() -> bool:
        return not pressed

    async def pressing(label: str) -> bool:
        pressed.append(label)
        return True

    async def ask(q: str) -> str:
        asked.append(q)
        return "no"

    for ask_owner in (ask, None):                      # with or without Telegram
        pressed.clear()
        b = ConsentBroker(ask_owner, showing=showing, pressing=pressing, appear_secs=0.05, poll_secs=0.01,
                          settle_secs=0.001, auto_allow=True)
        assert asyncio.run(b.answer_own_connection()) == "auto_allowed"
        assert pressed == ["Allow"] and asked == []


def test_the_standing_yes_retries_a_lagging_button_and_never_presses_cancel() -> None:
    pressed: list[str] = []
    landed = {"n": 0}

    async def showing() -> bool:
        return landed["n"] == 0

    async def pressing(label: str) -> bool:
        pressed.append(label)
        if len(pressed) >= 3:                           # the button appears on the third look
            landed["n"] += 1
        return len(pressed) >= 3

    b = ConsentBroker(None, showing=showing, pressing=pressing, appear_secs=0.05, poll_secs=0.01,
                      settle_secs=0.001, auto_allow=True)
    assert asyncio.run(b.answer_own_connection()) == "auto_allowed" and pressed == ["Allow"] * 3

    pressed.clear()
    landed["n"] = 0                                     # a fresh dialog

    async def never(label: str) -> bool:
        pressed.append(label)
        return False

    b = ConsentBroker(None, showing=showing, pressing=never, appear_secs=0.05, poll_secs=0.01,
                      settle_secs=0.001, auto_allow=True)
    assert asyncio.run(b.answer_own_connection()) == "press_failed" and "Cancel" not in pressed
    assert pressed == ["Allow"] * PRESS_TRIES


def test_the_standing_yes_still_waits_for_chromes_own_dialog() -> None:
    pressed: list[str] = []

    async def showing() -> bool:
        return False

    async def pressing(label: str) -> bool:
        pressed.append(label)
        return True

    b = ConsentBroker(None, showing=showing, pressing=pressing, appear_secs=0.05, poll_secs=0.01,
                      settle_secs=0.001, auto_allow=True)
    assert asyncio.run(b.answer_own_connection()) == "no_dialog" and pressed == []


@pytest.mark.parametrize("value,expect", [("allow", "allow"), (" Allow ", "allow"), ("ask", "ask"), ("", "ask"),
                                          ("yes", "ask"), (None, "ask")])
def test_the_access_preference_is_allow_only_when_spelled_allow(value, expect) -> None:
    from eve_bridge.chrome_consent import access_preference

    env = {} if value is None else {"EVE_CHROME_ACCESS": value}
    assert access_preference(env) == expect
