"""Replays of verification/Eve/RoutedTurn.lean's counterexamples against the real Python (2026-10-05).

Each test runs the trace `current_violates` names on the code as it is and asserts the property the model proves
of the fix (`fixed_invariant`). Every test here failed on origin/main a35b043 before the fix.

- P4 (the router's toolsets always include the keyword toolsets, whatever Jev does): `route_turn` read
  ``box["route"]`` after the worker, so a router that raised a BaseException (the worker caught only Exception) left
  the box empty and route_turn raised KeyError; and it built ``frozenset(r.toolsets)`` unguarded, so a Route whose
  toolsets is not a set of names raised TypeError. Either way the turn failed with FAILED_LINE and the keywords
  were lost.
- P1 (no tool call ends a routed turn with the failure line because of its name): parse_response(keep_unknown=True)
  still raised on a call whose name is empty or not a string; and the out-of-rounds summary parsed without
  keep_unknown, so a call the model made despite tool_choice "none" failed the whole turn.

Everything is faked: no Jev, no model, no message leaves the test.
"""

from __future__ import annotations

import json
import threading

from test_telegram import OWNER, FakeApi, FakeCreate, call, update
from test_telegram_toolsets import routed, run_until

from eve_bridge import telegram, toolset_router

ASKED = "when is the dentist on my calendar"    # the keywords alone open the calendar; not a rundown


class _Gone(BaseException):
    """Not an Exception: what the worker's ``except Exception`` lets through."""


def test_a_router_that_raises_a_base_exception_still_leaves_the_keyword_toolsets() -> None:
    assert toolset_router.keyword_toolsets(ASKED) == {toolset_router.CALENDAR}

    def router(text: str, previous: str = "") -> toolset_router.Route:
        raise _Gone()

    hook, threading.excepthook = threading.excepthook, (lambda args: None)   # the worker's end is not the test's
    try:
        r = toolset_router.route_turn(ASKED, "", router, timeout_s=2.0)
    finally:
        threading.excepthook = hook
    assert toolset_router.CALENDAR in r.toolsets
    assert r.asked and r.error


def test_a_route_whose_toolsets_is_not_a_set_of_names_still_leaves_the_keyword_toolsets() -> None:
    def router(text: str, previous: str = "") -> toolset_router.Route:
        return toolset_router.Route(None, 120.0)          # type: ignore[arg-type]

    r = toolset_router.route_turn(ASKED, "", router, timeout_s=2.0)
    assert toolset_router.CALENDAR in r.toolsets
    assert r.asked and r.error


def test_a_malformed_route_does_not_fail_the_turn() -> None:
    def router(text: str, previous: str = "") -> toolset_router.Route:
        return toolset_router.Route(None, 120.0)          # type: ignore[arg-type]

    rig = routed(FakeApi([update(ASKED, update_id=1)]), FakeCreate(telegram_say("Nothing on today.")), router)
    run_until(rig, requests=1)
    assert rig.api.sent == [(OWNER, "Nothing on today.")]


def telegram_say(text: str) -> dict:
    return {"id": "resp", "output": [{"type": "message", "role": "assistant",
                                      "content": [{"type": "output_text", "text": text}]}]}


def test_a_call_with_an_empty_name_is_refused_and_the_turn_goes_on() -> None:
    rig = routed(FakeApi([update("hi there", update_id=1)]), FakeCreate(call("", {}), telegram_say("Hi!")), None)
    run_until(rig, requests=2)
    assert telegram.FAILED_LINE not in [t for _, t in rig.api.sent]
    assert rig.api.sent == [(OWNER, "Hi!")]
    second = rig.create.requests[1]
    assert json.loads(second["input"][-1]["output"])["ok"] is False


def test_a_call_in_the_out_of_rounds_summary_does_not_fail_the_turn() -> None:
    wrong = [call("no_such_tool", {}, call_id=f"c{i}") for i in range(telegram.MAX_TOOL_ROUNDS)]
    summary = call("no_such_tool", {}, call_id="last")
    summary["output"].append({"type": "message", "role": "assistant",
                              "content": [{"type": "output_text", "text": "Here is what I found."}]})
    rig = routed(FakeApi([update("hi there", update_id=1)]), FakeCreate(*wrong, summary), None)
    run_until(rig, requests=telegram.MAX_TOOL_ROUNDS + 1)
    assert telegram.FAILED_LINE not in [t for _, t in rig.api.sent]
    assert rig.api.sent == [(OWNER, "Here is what I found.")]


def test_a_turn_without_the_robot_body_names_none_of_its_tools(monkeypatch) -> None:
    """P5, the owner's prompt rule: every tool name in a turn's instructions is offered that turn. The robot body's
    tools are offered only with EVE_ROBOT=1 (own_tools), but INSTRUCTIONS named them on every turn."""
    import re

    monkeypatch.delenv("EVE_ROBOT", raising=False)
    rig = routed(FakeApi([update("hi there", update_id=1)]), FakeCreate(telegram_say("Hi!")), None)
    run_until(rig, requests=1)
    first = rig.create.requests[0]
    offered = {str(t.get("name")) for t in first["tools"]}
    body = {n for n in telegram.ROBOT_BODY_TOOLS if "_" in n}      # look / find are plain words too
    assert body and not body & offered                               # the robot is off: none of them is offered
    named = {n for n in body if re.search(rf"\b{n}\b", first["instructions"])}
    assert named == set(), sorted(named)


# ---- found by the adversarial check of RoutedTurn.lean (2026-10-05) -----------------------------------------------

def test_a_route_whose_ms_is_not_a_number_does_not_fail_the_turn() -> None:
    """P4: route_turn passes Jev's ``r.ms`` through unchecked, and _think formats it in ``route.log_line()``
    (``{self.ms:.0f}``) after the gather. A Route whose ms is None raised TypeError there: FAILED_LINE, and the
    keyword toolsets were lost. A guard around ``frozenset(r.toolsets)`` alone does not reach it."""
    def router(text: str, previous: str = "") -> toolset_router.Route:
        return toolset_router.Route(frozenset(), None)    # type: ignore[arg-type]

    rig = routed(FakeApi([update(ASKED, update_id=1)]), FakeCreate(telegram_say("Nothing on today.")), router)
    run_until(rig, requests=1, limit_s=2.0)
    assert rig.api.sent == [(OWNER, "Nothing on today.")]


def test_a_routed_turn_with_web_search_off_does_not_name_web_search() -> None:
    """P5: APPS_BLOCK_REST names web_search whenever the workspace's Composio tools ride, but
    websearch.tools_for offers nothing with EVE_WEB_SEARCH=off."""
    import dataclasses

    from test_telegram import CFG, FakeApps

    from eve_bridge import websearch

    cfg = dataclasses.replace(CFG, search=websearch.SearchConfig(engine="off"))
    rig = routed(FakeApi([update("any slack dms", update_id=1)]), FakeCreate(telegram_say("Two from Priya.")), None,
                 apps=FakeApps(), config=cfg)
    run_until(rig, requests=1)
    first = rig.create.requests[0]
    offered = {str(t.get("name")) for t in first["tools"]}
    assert "COMPOSIO_SEARCH_TOOLS" in offered                    # the workspace is open: its block rides
    assert "web_search" not in offered                           # the search is off
    assert "web_search" not in first["instructions"]


def test_an_unrouted_turn_never_names_composio_while_its_session_is_down(tmp_path) -> None:
    """P5 with the router off: _turn_parts writes accounts_note whenever app_tools is non-empty, not only while
    Composio's session is up. ComposioBridge.start lists the owner's accounts (``_accounts``) before
    ``sessions.create``; when that create fails (no network at boot, or still starting) the note names
    COMPOSIO_MULTI_EXECUTE_TOOL on every turn with a direct app up, and no Composio tool is offered."""
    import re
    from types import SimpleNamespace

    from test_telegram import Rig, fake_apps, run_rig

    from eve_bridge import composio_tools

    def fail(*_a: object, **_k: object) -> None:
        raise ConnectionError("down")

    class Client:
        def __init__(self) -> None:
            self.connected_accounts = SimpleNamespace(list=lambda user_ids: SimpleNamespace(items=[
                {"toolkit": {"slug": "gmail"}, "status": "ACTIVE", "alias": "work", "id": "a1"}]))
            self.sessions = SimpleNamespace(use=fail, create=fail)

    bridge = composio_tools.ComposioBridge(
        composio_tools.ComposioConfig(enabled=True, user_id="u", state_path=tmp_path / "state.json"),
        client_factory=Client)
    try:
        bridge.start()
    except ConnectionError:
        pass                                                      # the daemon logs it and keeps the bridge
    assert not bridge.started
    rig = Rig(FakeApi([update("any mail from Sam?", update_id=1)]), FakeCreate(telegram_say("Two from Sam.")),
              apps=bridge, direct=fake_apps(), toolset_mode="off")
    run_rig(rig)
    first = rig.create.requests[0]
    offered = {str(t.get("name")) for t in first["tools"]}
    named = set(re.findall(r"\bCOMPOSIO_[A-Z_]+\b", first["instructions"]))
    assert named <= offered, sorted(named - offered)
