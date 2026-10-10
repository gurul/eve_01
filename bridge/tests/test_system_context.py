from datetime import datetime, timezone

import pytest

from eve_bridge import system_context, telegram, think, voice_agent


@pytest.mark.parametrize(("month", "offset", "hour"), [(1, "-08:00", "04"), (9, "-07:00", "05")])
def test_clock_uses_configured_timezone_and_daylight_saving(month, offset, hour):
    result = system_context.context(
        {"EVE_LOCATION": "Example Apartments, Seattle", "EVE_TIMEZONE": "America/Los_Angeles"},
        now=datetime(2026, month, 22, 12, 0, tzinfo=timezone.utc))
    assert f"T{hour}:00:00{offset}" in result
    assert "Timezone: America/Los_Angeles" in result
    assert "Owner-configured location: Example Apartments, Seattle" in result
    assert "not live GPS" in result and "snapshot, not a ticking clock" in result


def test_missing_location_and_invalid_timezone_use_local_clock():
    result = system_context.context({"EVE_TIMEZONE": "Missing/Zone"})
    assert "Owner-configured location" not in result
    assert "Missing/Zone" not in result
    assert "Local clock when this context was generated:" in result


def _telegram_context(body: dict) -> str:
    """The text of the developer item a Telegram request ends its input with (telegram.with_turn_context)."""
    last = body["input"][-1]
    assert last["type"] == "message" and last["role"] == "developer"
    return "".join(part["text"] for part in last["content"])


def test_context_reaches_each_brain_and_refreshes_for_new_requests(monkeypatch):
    monkeypatch.setenv("EVE_LOCATION", "Example Building, Portland, Oregon")
    monkeypatch.setenv("EVE_TIMEZONE", "America/Los_Angeles")
    session = voice_agent.session_config(voice_agent.configured())
    # Telegram keeps the clock out of its cached instructions (owner, 2026-09-23): it is the trailing developer
    # item of the turn's input, rebuilt every turn.
    prompts = [session["instructions"], session["delegation"]["responses"]["instructions"],
               _telegram_context(telegram.request(telegram.configured(), [])),
               think.request(think.configured(), [])["instructions"]]
    for prompt in prompts:
        assert "Example Building, Portland, Oregon" in prompt
        assert "Timezone: America/Los_Angeles" in prompt
        assert "Local clock when this context was generated:" in prompt
    monkeypatch.setattr(system_context, "context", lambda: "FRESH_CONTEXT")
    assert "FRESH_CONTEXT" in think.request(think.configured(), [])["instructions"]
    assert "FRESH_CONTEXT" in _telegram_context(telegram.request(telegram.configured(), []))
    assert "FRESH_CONTEXT" in voice_agent.session_config(voice_agent.configured())["instructions"]
