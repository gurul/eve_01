"""The toolset router wired into the Telegram turn (toolset_router.py, telegram._think; 2026-10-05).

A turn starts with the toolsets its words or Jev call for (keywords ∪ Jev), direct apps and Share and the
workspace alike; open_app opens the rest, and a call to a closed toolset's tool opens it and runs. With
EVE_TOOLSET_ROUTER=off the turn is the one before the router (test_telegram_chief pins it to origin/main's).
Everything here is faked: no Jev, no model, no file is sent, no message leaves the test.
"""

from __future__ import annotations

import asyncio
import json
import logging
import threading
import time
from pathlib import Path
from typing import Any, Callable

import pytest
from test_share import ANA, FakeRunner
from test_share import make as make_share
from test_telegram import (
    OWNER,
    FakeApi,
    FakeApps,
    FakeCreate,
    Rig,
    call,
    run_rig,
    say,
    scenario_apps,
    update,
)

from eve_bridge import apps_mcp, share, telegram, toolset_router

HAND_OFF = "get my resume over to ana before her interview"     # share, with no share word


class FakeRouter:
    """Jev's stand-in: answers each turn with the toolsets it was given, and records what it was asked."""

    def __init__(self, *answers: set[str], wait: float = 0.0) -> None:
        self.answers = [frozenset(a) for a in answers]
        self.asked: list[tuple[str, str]] = []
        self.wait = wait
        self.release = threading.Event()

    def __call__(self, text: str, previous: str = "") -> toolset_router.Route:
        self.asked.append((text, previous))
        if self.wait:
            self.release.wait(self.wait)
        found = self.answers.pop(0) if self.answers else frozenset()
        return toolset_router.Route(found, 180.0)


def run_until(rig: Rig, requests: int, sent: int = 1, limit_s: float = 5.0) -> None:
    """run_rig, waiting in real time (the router answers on a thread) until the model calls and replies are in."""
    async def during() -> None:
        deadline = time.monotonic() + limit_s
        while (len(rig.create.requests) < requests or len(rig.api.sent) < sent) and time.monotonic() < deadline:
            await asyncio.sleep(0.01)

    run_rig(rig, during)


def names(body: dict[str, Any]) -> list[str]:
    return [str(t.get("name")) for t in body["tools"]]


def open_enum(body: dict[str, Any]) -> list[str]:
    [tool] = [t for t in body["tools"] if t.get("name") == apps_mcp.OPEN_TOOL]
    return list(tool["parameters"]["properties"]["apps"]["items"]["enum"])


def a_share(tmp_path: Path, runner: FakeRunner | None = None) -> share.Share:
    home = tmp_path / "home"
    (home / "Documents").mkdir(parents=True)
    (home / "Documents" / "resume.pdf").write_bytes(b"%PDF-1.4 " + b"x" * 2000)
    return make_share(home, runner or FakeRunner([ANA]))


def routed(api: FakeApi, create: FakeCreate, router: Callable[..., Any] | None = None, **kw: Any) -> Rig:
    return Rig(api, create, toolset_mode="auto", toolset_route=router, **kw)


# ---- (a) a message with no keyword gets its toolset from Jev, in round 1 --------------------------------------

def test_a_hand_off_with_no_keyword_offers_share_from_round_one_when_jev_says_so(tmp_path: Path) -> None:
    assert toolset_router.keyword_toolsets(HAND_OFF) == set()          # the words alone open nothing
    router = FakeRouter({toolset_router.SHARE})
    rig = routed(FakeApi([update(HAND_OFF, update_id=1)]), FakeCreate(say("By text or email?")), router,
                 share=a_share(tmp_path / "on"))
    run_until(rig, requests=1)
    first = rig.create.requests[0]
    assert set(share.TOOL_NAMES) <= set(names(first))
    assert share.INSTRUCTIONS in first["instructions"]
    assert router.asked == [(HAND_OFF, "")]
    assert rig.api.sent == [(OWNER, "By text or email?")]
    # control: Jev says nothing, so Share stays closed, its instructions out, and open_app can open it
    rig = routed(FakeApi([update(HAND_OFF, update_id=1)]), FakeCreate(say("By text or email?")), FakeRouter(set()),
                 share=a_share(tmp_path / "off"))
    run_until(rig, requests=1)
    first = rig.create.requests[0]
    assert not set(share.TOOL_NAMES) & set(names(first)) and share.INSTRUCTIONS not in first["instructions"]
    assert open_enum(first) == [toolset_router.SHARE]


# ---- (b) a slow Jev costs nothing but its budget ----------------------------------------------------------------

def test_a_router_that_misses_its_budget_leaves_the_keywords_and_the_turn_still_answers(
        monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> None:
    monkeypatch.setattr(toolset_router, "TIMEOUT_S", 0.05)
    apps = FakeApps()
    slow = FakeRouter({toolset_router.SHARE}, wait=2.0)
    rig = routed(FakeApi([update("any slack dms", update_id=1)]), FakeCreate(say("Two from Priya.")), slow,
                 apps=apps, share=a_share(tmp_path))
    try:
        run_until(rig, requests=1)
    finally:
        slow.release.set()
    first = rig.create.requests[0]
    assert "COMPOSIO_SEARCH_TOOLS" in names(first)                     # the keywords' workspace
    assert not set(share.TOOL_NAMES) & set(names(first))               # the late answer added nothing
    assert rig.api.sent == [(OWNER, "Two from Priya.")]


# ---- (c) off: the turn before the router ------------------------------------------------------------------------

def test_off_never_asks_the_router_and_offers_every_toolset_as_before(tmp_path: Path) -> None:
    def never(*_a: Any) -> Any:
        raise AssertionError("the router was asked with the switch off")

    sh = a_share(tmp_path)
    rig = Rig(FakeApi([update("hi", update_id=1)]), FakeCreate(say("Hello!")), toolset_mode="off",
              toolset_route=never, apps=FakeApps(), share=sh)
    run_until(rig, requests=1)
    first = rig.create.requests[0]
    have = names(first)
    assert set(share.TOOL_NAMES) <= set(have)
    assert "COMPOSIO_SEARCH_TOOLS" in have and telegram.APPS_BLOCK in first["instructions"]
    assert apps_mcp.OPEN_TOOL not in have                               # no direct app, so nothing to open
    assert share.INSTRUCTIONS in first["instructions"]
    # and the body is exactly what the pre-router builder writes (no lent block, the watch slot as it was)
    parts, _allowed, _tools = asyncio.run(rig.inlet._turn_parts(0))
    assert "lent_block" not in parts


def test_the_off_switch_reads_from_the_environment(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("EVE_TOOLSET_ROUTER", "off")
    rig = Rig(FakeApi(), FakeCreate())
    assert rig.inlet._toolset_mode == "off" and rig.inlet._toolset_route is None
    monkeypatch.setenv("EVE_TOOLSET_ROUTER", "auto")                # no Jev key here (conftest): keywords
    rig = Rig(FakeApi(), FakeCreate())
    assert rig.inlet._toolset_mode == "auto" and rig.inlet._toolset_route is None


# ---- (d) a closed toolset's tool opens it and runs ----------------------------------------------------------------

def test_a_share_tool_called_while_closed_opens_share_and_runs(tmp_path: Path) -> None:
    runner = FakeRunner([ANA])
    rig = routed(FakeApi([update("hi there", update_id=1)]),
                 FakeCreate(call("find_contact", {"name": "Ana"}), say("Ana Lee, +1 (555) 010-0199.")), None,
                 share=a_share(tmp_path, runner))
    run_until(rig, requests=2)
    first, second = rig.create.requests
    assert "find_contact" not in names(first)
    assert runner.calls                                                 # it ran
    assert json.loads(second["input"][-1]["output"]).get("ok", True) is not False
    assert set(share.TOOL_NAMES) <= set(names(second)) and share.INSTRUCTIONS in second["instructions"]
    assert toolset_router.SHARE not in open_enum(second) if apps_mcp.OPEN_TOOL in names(second) else True
    assert telegram.FAILED_LINE not in [t for _, t in rig.api.sent]
    assert rig.api.sent == [(OWNER, "Ana Lee, +1 (555) 010-0199.")]


def test_a_composio_tool_called_while_the_workspace_is_closed_runs_and_brings_its_block() -> None:
    from test_telegram import multi

    apps = FakeApps()
    rig = routed(FakeApi([update("hi there", update_id=1)]),
                 FakeCreate(call("COMPOSIO_MULTI_EXECUTE_TOOL", multi("SLACK_FETCH_CONVERSATION_HISTORY")),
                            say("Nothing new.")), None, apps=apps)
    run_until(rig, requests=2)
    first, second = rig.create.requests
    assert not any(n.startswith("COMPOSIO") for n in names(first))
    assert telegram.APPS_BLOCK not in first["instructions"]           # its block rides only with its tools
    assert open_enum(first) == [toolset_router.WORKSPACE]
    assert [n for n, _ in apps.executed] == ["COMPOSIO_MULTI_EXECUTE_TOOL"]
    assert "COMPOSIO_SEARCH_TOOLS" in names(second) and telegram.APPS_BLOCK in second["instructions"]
    assert rig.api.sent == [(OWNER, "Nothing new.")]


# ---- (e) open_app opens a lent toolset ----------------------------------------------------------------------------

def test_open_app_share_offers_the_share_tools_on_the_next_round(tmp_path: Path) -> None:
    sh = a_share(tmp_path)
    apps = scenario_apps()
    rig = routed(FakeApi([update("hi there", update_id=1)]),
                 FakeCreate(call(apps_mcp.OPEN_TOOL, {"apps": ["share"]}), say("Ready.")), None, share=sh, direct=apps)
    run_until(rig, requests=2)
    first, second = rig.create.requests
    assert toolset_router.SHARE in open_enum(first) and "calendar" in open_enum(first)
    assert not set(share.TOOL_NAMES) & set(names(first))
    opened = json.loads(second["input"][-1]["output"])
    assert opened["ok"] and opened["opened"] == ["share"] and set(opened["tools"]) == set(share.TOOL_NAMES)
    assert set(share.TOOL_NAMES) <= set(names(second)) and toolset_router.SHARE not in open_enum(second)
    assert rig.api.sent == [(OWNER, "Ready.")]


def test_open_app_with_no_toolset_here_names_the_ones_that_are_and_not_the_asked_name(tmp_path: Path) -> None:
    rig = routed(FakeApi(), FakeCreate(), None, share=a_share(tmp_path))
    rig.inlet._routed = True
    out = rig.inlet._open_toolsets({"apps": ["zzq"]})
    assert out["ok"] is False and "zzq" not in out["reason"] and "share" in out["reason"]


# ---- the order: what varies goes last ------------------------------------------------------------------------------

def test_the_routed_toolsets_sit_last_and_open_app_is_the_very_last(tmp_path: Path) -> None:
    router = FakeRouter({toolset_router.SHARE, "calendar"})
    rig = routed(FakeApi([update(HAND_OFF, update_id=1)]), FakeCreate(say("ok")), router,
                 share=a_share(tmp_path), direct=scenario_apps())
    run_until(rig, requests=1)
    have = names(rig.create.requests[0])
    assert have[-1] == apps_mcp.OPEN_TOOL
    first_lent = min(have.index(n) for n in share.TOOL_NAMES)
    stable = [n for n in telegram.offered_names(telegram.own_tools())]
    assert all(have.index(n) < first_lent for n in stable)
    assert all(n not in share.TOOL_NAMES for n in have[:first_lent])
    assert have.index("calendar__list_events") < first_lent           # the direct apps, then the lent ones


# ---- (f) the calendar + maps turn is still two model calls ---------------------------------------------------------

def test_my_next_class_and_a_maps_link_is_still_two_model_calls_with_the_router_on() -> None:
    from test_telegram import MAPS_URL

    apps = scenario_apps()
    said = "look for my next class on my calendar and give me a maps link"
    router = FakeRouter({"calendar", "maps"})
    rig = routed(FakeApi([update(said, update_id=1)]),
                 FakeCreate(call("calendar__list_events", {"startTime": "now"}), say(f"CS 101, Room 101: {MAPS_URL}")),
                 router, direct=apps)
    run_until(rig, requests=2)
    first = names(rig.create.requests[0])
    assert "calendar__list_events" in first and "maps__compute_routes" in first
    assert len(rig.create.requests) == 2 and router.asked == [(said, "")]
    assert rig.api.sent == [(OWNER, f"CS 101, Room 101: {MAPS_URL}")]


# ---- (g) a follow-up reads the previous message -------------------------------------------------------------------

def test_and_send_it_to_ana_after_find_my_resume_in_drive_opens_drive_and_share(tmp_path: Path) -> None:
    apps = scenario_apps()
    router = FakeRouter({"drive"}, set())                               # Jev adds nothing on the follow-up
    api = FakeApi([update("find my resume in drive", update_id=1)], [update("and send it to ana", update_id=2)])
    rig = routed(api, FakeCreate(say("Found Resume.pdf."), say("Sending it to Ana?")), router, direct=apps,
                 share=a_share(tmp_path))
    run_until(rig, requests=2, sent=2)
    assert router.asked == [("find my resume in drive", ""), ("and send it to ana", "find my resume in drive")]
    second = rig.create.requests[1]
    assert set(share.TOOL_NAMES) <= set(names(second))
    assert "drive" not in open_enum(second) and toolset_router.SHARE not in open_enum(second)
    assert rig.inlet._direct_open == {"drive"} and rig.inlet._lent == {toolset_router.SHARE}


# ---- the warm-up and the log ---------------------------------------------------------------------------------------

def test_the_call_warm_up_opens_no_toolset_left_over_from_the_last_turn(tmp_path: Path) -> None:
    rig = routed(FakeApi(), FakeCreate(say("")), None, share=a_share(tmp_path), direct=scenario_apps())
    rig.inlet._routed = True
    rig.inlet._lent = {toolset_router.SHARE}                            # the last turn's
    rig.inlet._direct_open = {"calendar"}
    asyncio.run(rig.inlet._warm())
    [body] = rig.create.requests
    assert not set(share.TOOL_NAMES) & set(names(body)) and "calendar__list_events" not in names(body)
    assert toolset_router.SHARE in open_enum(body) and "calendar" in open_enum(body)
    assert share.INSTRUCTIONS not in body["instructions"]


def test_one_log_line_per_turn_names_the_toolsets_how_and_jevs_ms_never_the_words(
        caplog: pytest.LogCaptureFixture, tmp_path: Path) -> None:
    router = FakeRouter({toolset_router.SHARE, "calendar"})
    with caplog.at_level(logging.INFO, logger="eve_bridge.telegram"):
        rig = routed(FakeApi([update("whens my thing w the advisor, and " + HAND_OFF, update_id=1)]),
                     FakeCreate(say("ok")), router, share=a_share(tmp_path), direct=scenario_apps())
        run_until(rig, requests=1)
    lines = [r.getMessage() for r in caplog.records if r.getMessage().startswith("telegram: toolsets:")]
    assert lines == ["telegram: toolsets: calendar (jev), share (jev); jev 180 ms"]
    assert not any("advisor" in r.getMessage() or "resume" in r.getMessage() for r in caplog.records)


# ---- review, 2026-10-05: a rundown names no Share tool it does not offer ------------------------------------

@pytest.mark.parametrize("mode", ["off", "auto"])
def test_a_rundown_carries_no_share_block_since_it_offers_no_share_tool(tmp_path: Path, mode: str) -> None:
    api = FakeApi([update("/rundown")])
    rig = Rig(api, FakeCreate(say("Quiet day.")), toolset_mode=mode, toolset_route=None,
              share=a_share(tmp_path))
    run_rig(rig)
    body = rig.create.requests[0]
    offered = set(names(body))
    assert not offered & set(share.TOOL_NAMES)
    assert "share_file" not in body["instructions"]
    # control: an ordinary turn in off mode still lends it, block and tools
    api = FakeApi([update("hi")])
    rig = Rig(api, FakeCreate(say("Hi.")), toolset_mode="off", toolset_route=None,
              share=a_share(tmp_path / "again"))
    run_rig(rig)
    body = rig.create.requests[0]
    assert "share_file" in body["instructions"] and set(share.TOOL_NAMES) <= set(names(body))
