"""Every tool eve offers declares an approval policy (tool_gate.POLICY), and declaring them changed no decision.

CI is where an undeclared tool is caught, not the runtime: a tool missing from POLICY still runs unasked, as before,
so a lagging map never puts questions on the phone. This test is what keeps a new native send, delete or buy from
shipping without a decision about who approves it (the owner's rule, 2026-10-09: ask for sending, deleting and
buying, not for routine work; docs/plans 2026-10-26, "Do 4").

Names with "__" (the direct apps: Google, GitHub, Maps) and COMPOSIO_* names are not collected: apps_mcp's and
composio_tools.decide's own policies decide those, per call.
"""

from __future__ import annotations

import importlib
from typing import Any, Iterable, Mapping

import pytest

from eve_bridge import app_registry, tool_gate

# Every module-level list (or single tool) of tools eve offers a model: the chat brain's, the voice's, the / menu's,
# memory's, each feature module's, the Codex vault's and the Mac agent's. A tool built by a function is collected by
# calling it (DYNAMIC); each registry app's TOOLS are added from app_registry.
STATIC: tuple[tuple[str, str], ...] = (
    ("telegram", "TOOLS"), ("telegram", "RECENT_MESSAGES_TOOL"), ("websearch", "WEB_SEARCH_TOOL"),
    ("voice_agent", "TOOLS"), ("voice_agent", "MEET_TOOLS"),
    ("memory", "TOOLS"), ("memory", "VOICE_TOOLS"),
    ("menu_tools", "TOOLS"), ("menu_tools", "FILE_TOOLS"),
    ("canvas", "TOOLS"), ("find_image", "TOOLS"), ("meet", "TOOLS"), ("phone_location", "TOOLS"),
    ("places", "TOOLS"), ("science_search", "TOOLS"), ("second_brain", "SECOND_BRAIN_TOOLS"), ("share", "TOOLS"),
    ("watch", "TOOLS"), ("nyt", "TOOLS"), ("apps_maker", "MAKER_TOOLS"), ("vault_mcp", "TOOLS"),
    ("america", "TOOL"), ("events", "TOOL"), ("opentable", "TOOL"), ("computer_agent", "TOOLS"),
)


def _dynamic() -> dict[str, list[Any]]:
    from eve_bridge import agentcard, apps_mcp, chief

    return {
        "chief.tools": chief.tools(),
        "agentcard.AgentCard.tools": agentcard.AgentCard.tools(None),      # builds its list without self
        "apps_mcp.open_tool_spec": [apps_mcp.open_tool_spec([("an_app", "An app")])],
    }


def _names(value: Any) -> Iterable[str]:
    items = [value] if isinstance(value, Mapping) else list(value or ())
    for tool in items:
        if isinstance(tool, Mapping):
            name = tool.get("name") or (tool.get("function") or {}).get("name")
            if isinstance(name, str) and name:
                yield name


def _decided_elsewhere(name: str) -> bool:
    return "__" in name or name.startswith("COMPOSIO_")


def offered() -> dict[str, set[str]]:
    """Every native tool name eve offers, with where it is defined."""
    out: dict[str, set[str]] = {}

    def add(where: str, value: Any) -> None:
        for name in _names(value):
            if not _decided_elsewhere(name):
                out.setdefault(name, set()).add(where)

    for module, attr in STATIC:
        add(f"{module}.{attr}", getattr(importlib.import_module(f"eve_bridge.{module}"), attr))
    for where, tools in _dynamic().items():
        add(where, tools)
    for app in app_registry.with_tools():
        add(f"{app.module}.TOOLS", app.mod().TOOLS)
    return out


def unclassified() -> list[str]:
    return sorted(n for n in offered() if n not in tool_gate.POLICY)


# ---- coverage -------------------------------------------------------------------------------------------------------

def test_every_offered_tool_declares_an_approval_policy() -> None:
    missing = unclassified()
    assert missing == [], f"no tool_gate.POLICY entry for: {', '.join(missing)} (docs/adding-an-app.md, Safety)"


def test_the_collector_sees_every_source() -> None:
    names = offered()
    assert len(names) >= 90                                       # 92 + the dynamic ones on 2026-10-26
    for name in ("start_task", "recent_messages", "web_search", "end_conversation", "join_meeting", "remember",
                 "forget_apply", "claude_relay", "send_file", "canvas_due", "place_name", "share_file",
                 "vault_fill", "take_on", "agentcard_confirm", "open_app", "lights_set", "firetv_screenshot",
                 "camera_control", "spotify_control", "exec_py"):
        assert name in names, name


def test_a_new_undeclared_tool_is_reported(monkeypatch: pytest.MonkeyPatch) -> None:
    from eve_bridge import menu_tools

    assert unclassified() == []
    fake = {"type": "function", "name": "send_everything", "parameters": {"type": "object", "properties": {}}}
    monkeypatch.setattr(menu_tools, "TOOLS", [*menu_tools.TOOLS, fake])
    assert unclassified() == ["send_everything"]


def test_names_decided_by_their_own_policies_are_not_collected(monkeypatch: pytest.MonkeyPatch) -> None:
    from eve_bridge import menu_tools

    extra = [{"type": "function", "name": n} for n in ("gmail__send_message", "COMPOSIO_MULTI_EXECUTE_TOOL")]
    monkeypatch.setattr(menu_tools, "TOOLS", [*menu_tools.TOOLS, *extra])
    assert unclassified() == []


def test_policy_values_are_exactly_the_five() -> None:
    assert app_registry.POLICIES == ("read", "routine", "asks", "taint_asks", "executor_owned")
    assert set(tool_gate.POLICY.values()) <= set(app_registry.POLICIES)
    assert tool_gate.policy("place_name") == "routine" and tool_gate.policy("gmail__send_message") is None


def test_policy_agrees_with_provenance() -> None:
    """An acting tool is never declared a read; a declared read never asks, clean or tainted."""
    for name in tool_gate.ACTING:
        assert tool_gate.POLICY[name] != "read", name
    for name, kind in tool_gate.POLICY.items():
        if kind in ("read", "routine", "executor_owned"):
            for args in ARGS:
                assert not tool_gate.needs_confirmation(name, args, "a web search"), (name, args)


def test_the_places_writes_are_routine_acts_that_still_taint() -> None:
    for name in ("place_name", "thing_place"):
        assert tool_gate.acting(name, {}) and tool_gate.POLICY[name] == "routine"
        assert tool_gate.taints(name, {"action": "find"})         # a place's name can be the geocoder's words
        assert tool_gate.source(name) == "a tool result"          # the owner's question reads as before
        for taint in ("", "a web search"):
            assert not tool_gate.needs_confirmation(name, {"action": "forget"}, taint)


# ---- no decision changed --------------------------------------------------------------------------------------------

def _needs_confirmation_before(name: str, args: Mapping[str, Any], tainted: Any = "") -> bool:
    """tool_gate.needs_confirmation as it was before POLICY (main at 4907b3d, 2026-10-26), copied whole: the oracle
    the table-driven version must match for every name and argument below."""
    if name in {"forget_apply", "delete_app", "watch_remove"}:
        return True
    if name == "pair_eve_link":
        return bool(args.get("new_link"))
    if not tainted:
        return False
    if name in {"answer_question", "meet_join", "join_meeting"}:
        return True
    if name == "take_notes":
        return str(args.get("action") or "").strip().lower() == "start"
    return False


# The argument shapes the old rules looked at, and their near misses.
ARGS: tuple[dict[str, Any], ...] = (
    {}, {"new_link": True}, {"new_link": False}, {"new_link": "true"}, {"new_link": 1}, {"new_link": ""},
    {"action": "start"}, {"action": " START "}, {"action": "stop"}, {"action": "status"}, {"action": None},
    {"action": "start", "new_link": True},
)
TAINTS: tuple[Any, ...] = ("", None, "a web search", tool_gate.CARRIED)
# Every collected name, every declared one, and names nobody declares (a direct app's, Composio's, a stranger).
NAMES = sorted(set(offered()) | set(tool_gate.POLICY)
               | {"gmail__send_message", "COMPOSIO_MULTI_EXECUTE_TOOL", "a_tool_nobody_offers", ""})


@pytest.mark.parametrize("name", NAMES)
def test_no_confirmation_decision_changed(name: str) -> None:
    for args in ARGS:
        for taint in TAINTS:
            assert tool_gate.needs_confirmation(name, args, taint) is _needs_confirmation_before(name, args, taint), \
                (name, args, taint)


def test_the_oracle_still_asks_where_it_did() -> None:
    """The comparison is not vacuous: the old rules asked for these, and the new ones still do."""
    asked = {n for n in NAMES for a in ARGS for t in TAINTS if _needs_confirmation_before(n, a, t)}
    assert asked == {"forget_apply", "delete_app", "watch_remove", "pair_eve_link", "answer_question", "meet_join",
                     "join_meeting", "take_notes"}
