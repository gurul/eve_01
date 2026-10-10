"""start_task never sends email while Gmail may: the model is told to use Gmail's own tool (telegram.MAIL_SEND)."""
from __future__ import annotations

from types import SimpleNamespace

import pytest

from eve_bridge.telegram import MAIL_SEND, MAIL_SEND_REASON, TelegramInlet


def _inlet(apps=("gmail", "calendar"), gmail="ask", started=True):
    direct = SimpleNamespace(started=started, apps=list(apps), policy={"gmail": gmail})
    return SimpleNamespace(_direct=direct)


@pytest.mark.parametrize("goal", [
    "Email me a test saying hi", "email Sam that I'm running late", "Send an email to ana@example.com",
    "write a mail to my landlord about the heater", "reply to Sam's email saying noon works",
    "draft and send an email to the team", "e-mail them the notes",
])
def test_sending_email_is_refused_as_a_mac_task(goal: str) -> None:
    assert TelegramInlet._mail_task(_inlet(), {"goal": goal})


@pytest.mark.parametrize("goal", [
    "open my email", "check my inbox in Chrome", "open Mail.app", "take a screenshot of the email settings page",
    "open the email from Sam in Chrome", "rename the file emails.csv",
])
def test_other_screen_tasks_still_start(goal: str) -> None:
    assert not TelegramInlet._mail_task(_inlet(), {"goal": goal})


def test_only_while_gmail_is_up_and_may_send() -> None:
    goal = {"goal": "Email me a test saying hi"}
    assert not TelegramInlet._mail_task(_inlet(apps=("calendar",)), goal)
    assert not TelegramInlet._mail_task(_inlet(gmail="read"), goal)
    assert not TelegramInlet._mail_task(_inlet(started=False), goal)
    assert not TelegramInlet._mail_task(SimpleNamespace(_direct=None), goal)


def test_the_reason_names_the_tool_that_sends() -> None:
    assert "gmail__send_message" in MAIL_SEND_REASON and MAIL_SEND.pattern
