"""app_reflex.py: a bare app launch is `open -a`, everything else is the wrapped agent's."""

from __future__ import annotations

import asyncio
from typing import Any

from eve_bridge import app_reflex
from eve_bridge.app_reflex import ReflexFirstAgent

APPS = ("Spotify", "Notes", "Safari", "Warp")


class Inner:
    provider = "codex"

    def __init__(self) -> None:
        self.running = False
        self.goals: list[str] = []
        self.cancelled = ""

    async def run(self, goal: str) -> str:
        self.goals.append(goal)
        return "codex did it"

    def cancel(self, reason: str = "") -> None:
        self.cancelled = reason

    def status(self) -> dict[str, Any]:
        return {"running": self.running, "provider": self.provider}


def rig(*, ok: bool = True, asker: Any = None, enabled: bool = True):
    events: list[tuple[str, str]] = []
    opened: list[str] = []

    async def opener(app: str) -> tuple[bool, str]:
        opened.append(app)
        return ok, "" if ok else "Unable to find application"

    inner = Inner()
    agent = ReflexFirstAgent(lambda: inner, lambda ev: events.append((ev.kind, ev.text)), apps=lambda: APPS,
                             asker=asker, opener=opener, enabled=enabled,
                             on_done=lambda: events.append(("done", "")))
    return agent, inner, opened, events


def test_bare_launch_is_opened_without_the_inner_agent() -> None:
    agent, inner, opened, events = rig()
    assert asyncio.run(agent.run("open Spotify")) == "Opened Spotify."
    assert opened == ["Spotify"] and inner.goals == [] and agent.reflexed == "Spotify"
    assert events == [("started", "open Spotify"), ("final", "Opened Spotify.")]


def test_launch_then_more_goes_to_the_inner_agent_whole() -> None:
    agent, inner, opened, _ = rig()
    assert asyncio.run(agent.run("open Spotify and play my discover weekly")) == "codex did it"
    assert opened == [] and inner.goals == ["open Spotify and play my discover weekly"]


def test_consequential_and_uninstalled_are_the_inner_agents() -> None:
    agent, inner, opened, _ = rig()
    asyncio.run(agent.run("force quit Spotify"))
    asyncio.run(agent.run("open Photoshop"))
    assert opened == [] and inner.goals == ["force quit Spotify", "open Photoshop"]


def test_failed_open_falls_back_to_the_inner_agent() -> None:
    agent, inner, opened, _ = rig(ok=False)
    assert asyncio.run(agent.run("open Notes")) == "codex did it"
    assert opened == ["Notes"] and inner.goals == ["open Notes"]


def test_jev_answers_wording_the_rules_do_not_know() -> None:
    asked: list[str] = []

    def asker(goal: str, apps: list[str]) -> str:
        asked.append(goal)
        return "Notes"

    agent, inner, opened, _ = rig(asker=asker)
    assert asyncio.run(agent.run("Notes, please.")) == "Opened Notes."
    assert asked and opened == ["Notes"] and inner.goals == []


def test_jev_naming_an_uninstalled_app_is_ignored_and_errors_abstain() -> None:
    agent, inner, opened, _ = rig(asker=lambda goal, apps: "Photoshop")
    asyncio.run(agent.run("Notes, please."))

    def boom(goal: str, apps: list[str]) -> str:
        raise TimeoutError

    agent2, inner2, opened2, _ = rig(asker=boom)
    asyncio.run(agent2.run("Notes, please."))
    assert opened == opened2 == [] and inner.goals == inner2.goals == ["Notes, please."]


def test_disabled_passes_everything_through() -> None:
    agent, inner, opened, _ = rig(enabled=False)
    asyncio.run(agent.run("open Spotify"))
    assert opened == [] and inner.goals == ["open Spotify"]


def test_the_inner_agent_is_built_only_when_the_reflex_declines() -> None:
    built: list[int] = []
    inner = Inner()

    def make() -> Inner:
        built.append(1)
        return inner

    agent = ReflexFirstAgent(make, lambda ev: None, apps=lambda: APPS, opener=lambda app: _ok())
    asyncio.run(agent.run("open Spotify"))
    assert built == []                                  # a launch never spends the warm Codex agent
    asyncio.run(agent.run("open Spotify and play jazz"))
    assert built == [1]


async def _ok() -> tuple[bool, str]:
    return True, ""


def test_on_done_follows_a_handoff_only() -> None:
    agent, _, _, events = rig()
    asyncio.run(agent.run("open Spotify"))
    assert ("done", "") not in events
    asyncio.run(agent.run("play jazz on spotify"))
    assert events[-1] == ("done", "")


def test_cancel_before_the_handoff_never_starts_it_and_after_is_forwarded() -> None:
    agent, inner, _, events = rig()
    agent.cancel(reason="stop")
    assert asyncio.run(agent.run("play jazz on spotify")) == "Stopped." and inner.goals == []
    agent2, inner2, _, _ = rig()
    asyncio.run(agent2.run("play jazz on spotify"))
    agent2.cancel(reason="stop")
    assert inner2.cancelled == "stop" and agent2.provider == "codex" and agent2.running is False
    assert agent2.status() is not None


def test_switches() -> None:
    assert app_reflex.reflexes_on({"EVE_REFLEXES": "0"}) is False
    assert app_reflex.reflexes_on({}) is True
    assert app_reflex.jev_asker({"EVE_ROUTER_MODEL": "off"}) is None
    assert app_reflex.jev_asker({"EVE_ROUTER_MODEL": "jev", "EVE_JEV_ROUTE": "openrouter"}) is None  # no key


def test_agent_event_has_one_definition() -> None:
    from eve_bridge import agent_contract, computer_agent

    assert computer_agent.AgentEvent is agent_contract.AgentEvent


# ---- a request that refers back to a link from an earlier message ----------------------------------------

MAP = "https://maps.app.goo.gl/AbCdEf123"


def test_open_it_after_a_shared_link_carries_the_link() -> None:
    """Production, 2026-09-24: the owner shared a Google Maps link, then "Use Google search to open it up"; the
    goal was the owner's words for that request only, so the link was not in it."""
    recent = [f"here {MAP}", "Use Google search to open it up"]
    goal = app_reflex.with_referenced_links("Use Google search to open it up", recent)
    assert goal.startswith("Use Google search to open it up") and MAP in goal
    from eve_bridge.browser_lane import is_web_goal
    assert is_web_goal(goal)


def test_the_newest_links_come_first_and_only_the_recent_messages_count() -> None:
    old, mid, new = "https://example.org/old", "https://example.org/mid", "https://example.org/new."
    recent = [old, "a", "b", mid, new]                     # `old` is five messages back: out of reach
    goal = app_reflex.with_referenced_links("open that link", recent)
    assert goal.index("example.org/new") < goal.index("example.org/mid") and old not in goal
    assert "example.org/new." not in goal                   # trailing punctuation is not part of a link


def test_a_goal_that_does_not_refer_back_or_has_its_own_link_is_unchanged() -> None:
    recent = [f"here {MAP}"]
    for goal in ("play some jazz on spotify", "open https://example.org/x", "what's the weather", ""):
        assert app_reflex.with_referenced_links(goal, recent) == goal
    assert app_reflex.with_referenced_links("open it", ["no links here"]) == "open it"


def test_the_body_logs_the_goals_shape_never_its_words(caplog: Any) -> None:
    import logging

    agent, inner, _, _ = rig(enabled=False)
    with caplog.at_level(logging.INFO, logger="eve_bridge.app_reflex"):
        asyncio.run(agent.run(f"please open this secretive place {MAP}"))
    line = next(r.getMessage() for r in caplog.records if "gets a goal of" in r.getMessage())
    assert "6 words, 1 link(s) (maps.app.goo.gl)" in line and "secretive" not in line


def _routed_rig() -> tuple[ReflexFirstAgent, Inner, Inner, list[str]]:
    """The daemon's shape: Codex inside, the Chrome lane beside it, the Chrome lane's own rule choosing."""
    from eve_bridge import browser_lane

    opened: list[str] = []

    async def opener(app: str) -> tuple[bool, str]:
        opened.append(app)
        return True, ""

    async def route_body(goal: str) -> str:
        return "chrome" if browser_lane.is_web_goal(goal) else "codex"

    codex, chrome = Inner(), Inner()
    chrome.provider = "chrome-lane"
    agent = ReflexFirstAgent(lambda: codex, lambda ev: None, apps=lambda: APPS + ("Photo Booth",), opener=opener,
                             make_auto=lambda: chrome, route_body=route_body)
    return agent, codex, chrome, opened


def test_the_door_note_never_steers_the_route() -> None:
    """Production 2026-09-26: Telegram's file hint says "open it in a browser", and read with the goal it sent
    every Telegram task to the Chrome lane and made "open Spotify" too long to be a launch. The note now goes
    beside the goal: the reflex and the router read the owner's words, the body gets both."""
    from eve_bridge.telegram import TASK_FILE_HINT

    agent, codex, chrome, opened = _routed_rig()
    assert asyncio.run(agent.run("Open up Spotify.", note=TASK_FILE_HINT)) == "Opened Spotify."
    assert opened == ["Spotify"] and codex.goals == chrome.goals == []

    agent, codex, chrome, opened = _routed_rig()
    goal = "Send the picture I just took on Photo Booth to me"
    asyncio.run(agent.run(goal, note=TASK_FILE_HINT))
    assert chrome.goals == [] and codex.goals == [goal + TASK_FILE_HINT] and opened == []

    agent, codex, chrome, opened = _routed_rig()
    asyncio.run(agent.run("open youtube.com in chrome", note=TASK_FILE_HINT))
    assert codex.goals == [] and chrome.goals == ["open youtube.com in chrome" + TASK_FILE_HINT]


def test_a_stop_while_the_body_router_thinks_never_starts_the_body() -> None:
    """A Stop during the router's await (Jev, about 2 s) used to be recorded and then ignored: the body ran."""
    async def go() -> None:
        events: list[tuple[str, str]] = []
        codex, chrome = Inner(), Inner()
        discarded: list[str] = []

        async def discard() -> None:
            discarded.append("codex")
        codex.discard = discard                          # type: ignore[attr-defined]  # a warm agent's close
        asked, release = asyncio.Event(), asyncio.Event()

        async def route_body(goal: str) -> str:
            asked.set()
            await release.wait()
            return "codex"

        agent = ReflexFirstAgent(lambda: codex, lambda ev: events.append((ev.kind, ev.text)), apps=lambda: APPS,
                                 opener=lambda app: _ok(), make_auto=lambda: chrome, route_body=route_body,
                                 on_done=lambda: events.append(("done", "")))
        task = asyncio.create_task(agent.run("play jazz on spotify"))
        await asked.wait()
        agent.cancel(reason="stop")
        release.set()
        assert await task == "Stopped."
        assert codex.goals == chrome.goals == [] and ("cancelled", "Stopped.") in events
        assert discarded == ["codex"] and ("done", "") in events      # the warm agent closed, the next one warmed

    asyncio.run(go())
