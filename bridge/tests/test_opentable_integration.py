"""OpenTable's real inlet routing with offline model/agent boundaries."""
from __future__ import annotations

import asyncio
import dataclasses
import json
import time
from pathlib import Path
from typing import Any

import pytest
from jsonschema import Draft202012Validator
from test_telegram import CFG, OWNER, STRANGER, FakeAgent, FakeApi, FakeCreate, Rig, call, update

from eve_bridge import opentable, telegram
from eve_bridge.daemon import Daemon

QUERY = "Seattle on October 5, 2026 at 7 pm for 2 people"


async def until(predicate: Any) -> None:
    deadline = time.monotonic() + 3
    while not predicate():
        assert time.monotonic() < deadline, "OpenTable did not reach the expected state"
        await asyncio.sleep(0.001)


def rig_with_agent(**kwargs: Any) -> tuple[Rig, list[str | None]]:
    floors: list[str | None] = []
    rig = Rig(FakeApi(), FakeCreate(), **kwargs)

    def factory(on_event: Any, ask: Any, *, floor: str | None = None) -> FakeAgent:
        floors.append(floor)
        agent = FakeAgent(on_event, ask)
        rig.agents.append(agent)
        return agent

    rig.inlet._agent_factory = factory
    return rig, floors


@pytest.mark.parametrize("enabled,control,factory", [(False, True, True), (True, False, True), (True, True, False), (True, True, True)])
def test_feature_menu_and_shared_tools_require_all_capabilities(enabled: bool, control: bool, factory: bool) -> None:
    async def run() -> None:
        rig = Rig(FakeApi(), FakeCreate(), opentable_enabled=enabled, agent_enabled=control,
                  **({} if factory else {"agent_factory": None}))
        await rig.inlet._set_commands()
        parts, names, _ = await rig.inlet._turn_parts(0)
        live = enabled and control and factory
        assert ("opentable_search" in names) is live
        assert (opentable.tools()[0] in parts["watch_tools"]) is live
        assert sum(word == "opentable" for word, _ in rig.api.commands[0][0]) == int(live)
        assert rig.api.commands[0][1] == OWNER
        assert rig.api.commands[0][0][-1][0] == "stop"
        assert not rig.create.requests and not rig.agents
        await rig.inlet._shutdown()
    asyncio.run(run())


@pytest.mark.parametrize("relay", ["none", "claude", "codex"])
def test_command_routes_directly_through_codex_without_consuming_questions(relay: str) -> None:
    async def run() -> None:
        rig, floors = rig_with_agent(opentable_enabled=True)
        inlet = rig.inlet
        inlet.claude = relay == "claude"
        inlet._codex_chat = OWNER if relay == "codex" else None
        prior = inlet._open_question(asyncio.get_running_loop().create_future(), OWNER,
                                     strict=True, words=frozenset({"yes", "no"}), firm=True)
        inlet._dispatch(update("/OPENTABLE@EveBot " + QUERY))
        await until(lambda: bool(rig.agents) and rig.agents[0].running)
        assert floors == ["codex"] and QUERY in rig.agents[0].goal
        assert opentable.OPENTABLE_URL in rig.agents[0].goal
        assert not prior.future.done() and not inlet._held
        assert not rig.create.requests
        rig.agents[0].release.set()
        await until(lambda: not inlet.task_running)
        await until(lambda: (OWNER, rig.agents[0].final_text) in rig.api.sent)
        assert any(title == telegram.TASK_DONE_TITLE and subtitle == "OpenTable: " + QUERY
                   for title, subtitle, _ in rig.api.titled)
        prior.future.cancel()
        await inlet._shutdown()
    asyncio.run(run())


@pytest.mark.parametrize("on_call", [False, True])
def test_chat_and_call_tools_start_one_task_without_second_model_round(on_call: bool) -> None:
    async def run() -> None:
        rig, floors = rig_with_agent(opentable_enabled=True)
        rig.inlet._create = rig.create = FakeCreate(call("opentable_search", {"request": QUERY}))
        rig.inlet._chat_id = OWNER
        spoken: list[str] = []
        if on_call:
            rig.inlet._call_say = spoken.append
            telegram._FROM_CALL.set(spoken.append)  # the call's own work: only that is read on it (CallScope.lean)
        text, rounds = await rig.inlet._think([{"role": "user", "content": [{"type": "input_text", "text": QUERY}]}], OWNER)
        await until(lambda: bool(rig.agents) and rig.agents[0].running)
        assert rounds == 1 and text == "" and len(rig.create.requests) == 1
        assert floors == ["codex"]
        assert opentable.tools()[0] in rig.create.requests[0]["tools"]
        if on_call:
            assert telegram.CALL_ON_IT in spoken
        rig.agents[0].release.set()
        await until(lambda: not rig.inlet.task_running)
        await rig.inlet._shutdown()
    asyncio.run(run())


def test_receipt_is_started_not_booked_and_busy_has_no_fallback() -> None:
    async def run() -> None:
        rig, floors = rig_with_agent(opentable_enabled=True)
        result = await rig.inlet._tool("opentable_search", {"request": QUERY}, OWNER)
        assert result["ok"] and result["status"] == "started"
        assert result["provider"] == "opentable_browser" and result["reservation_submitted"] is False
        assert "goal" not in result and "times" not in result and "reservation_id" not in result
        artifact = json.loads((Path(__file__).resolve().parents[2] / "docs/contracts/opentable.json").read_text())
        receipts = Draft202012Validator(artifact)
        receipts.validate(result)
        await until(lambda: bool(rig.agents) and rig.agents[0].running)
        busy = await rig.inlet._tool("opentable_search", {"request": QUERY}, OWNER)
        assert not busy["ok"] and "already running" in busy["reason"]
        receipts.validate(busy)
        assert floors == ["codex"] and not rig.create.requests
        rig.agents[0].release.set()
        await until(lambda: not rig.inlet.task_running)
        await rig.inlet._shutdown()
    asyncio.run(run())


@pytest.mark.parametrize("args", [{}, {"request": ""}, {"request": 42}, {"request": "x" * 4001}, {"request": QUERY, "book": True}])
def test_invalid_tool_requests_never_start_or_fall_through(args: dict[str, Any]) -> None:
    async def run() -> None:
        rig, floors = rig_with_agent(opentable_enabled=True)
        result = await rig.inlet._tool("opentable_search", args, OWNER)
        assert not result["ok"] and result["status"] == "invalid_request"
        assert not rig.agents and not floors and not rig.create.requests
        await rig.inlet._shutdown()
    asyncio.run(run())


def test_disabled_stranger_bare_and_attachment_paths_do_no_work() -> None:
    async def run() -> None:
        off = Rig(FakeApi(), FakeCreate())
        off.inlet._dispatch(update("/opentable " + QUERY))
        await until(lambda: bool(off.api.sent))
        assert opentable.OFF_REASON in off.api.sent[-1][1]
        result = await off.inlet._tool("opentable_search", {"request": QUERY}, OWNER)
        assert not result["ok"] and result["status"] == "unavailable"
        on, floors = rig_with_agent(opentable_enabled=True)
        result = await on.inlet._tool("opentable_search", {"request": QUERY}, STRANGER)
        assert not result["ok"] and "owner" in result["reason"]
        on.inlet._dispatch(update("/opentable " + QUERY, uid=STRANGER))
        on.inlet._dispatch(update("/opentable"))
        await until(lambda: bool(on.api.sent))
        assert on.api.sent[-1][1] == opentable.HELP_LINE
        await on.inlet._opentable_command(telegram.Inbound(OWNER, OWNER, "/opentable", image=object()), QUERY)
        assert "without attachments" in on.api.sent[-1][1]
        assert not on.agents and not floors and not on.create.requests and not off.create.requests
        await off.inlet._shutdown()
        await on.inlet._shutdown()
    asyncio.run(run())


@pytest.mark.parametrize("entry", ["tool", "command"])
def test_factory_failure_is_static_unavailable_and_clears_progress(entry: str) -> None:
    async def run() -> None:
        rig, _ = rig_with_agent(opentable_enabled=True)
        def broken(*args: Any, **kwargs: Any) -> None:
            raise RuntimeError("sensitive launch details")
        rig.inlet._agent_factory = broken
        if entry == "tool":
            result = await rig.inlet._tool("opentable_search", {"request": QUERY}, OWNER)
            assert not result["ok"] and result["status"] == "unavailable"
            assert "sensitive" not in str(result) and result["reservation_submitted"] is False
        else:
            await rig.inlet._opentable_command(telegram.Inbound(OWNER, OWNER, "/opentable " + QUERY), QUERY)
            assert "could not start" in rig.api.sent[-1][1] and "sensitive" not in rig.api.sent[-1][1]
        assert rig.inlet._task_progress is None and not rig.inlet.task_running
        assert not rig.create.requests
        await rig.inlet._shutdown()
    asyncio.run(run())


def test_another_configured_owner_cannot_cancel_the_first_owners_search() -> None:
    async def run() -> None:
        rig, _ = rig_with_agent(opentable_enabled=True,
                               config=dataclasses.replace(CFG, owner_ids=frozenset({OWNER, STRANGER})))
        await rig.inlet._tool("opentable_search", {"request": QUERY}, OWNER)
        await until(lambda: bool(rig.agents) and rig.agents[0].running)
        rig.inlet._dispatch(update("/opentable cancel", uid=STRANGER))
        await until(lambda: bool(rig.api.sent) and rig.api.sent[-1][0] == STRANGER)
        assert rig.api.sent[-1][1] == "No /opentable search is running."
        assert rig.agents[0].cancel_reason is None
        rig.agents[0].release.set()
        await until(lambda: not rig.inlet.task_running)
        await rig.inlet._shutdown()
    asyncio.run(run())


def test_cancel_stops_only_matching_search_and_leaves_purchase_and_other_prompt() -> None:
    async def run() -> None:
        rig, floors = rig_with_agent(opentable_enabled=True)
        inlet = rig.inlet
        prior = inlet._open_question(asyncio.get_running_loop().create_future(), OWNER,
                                     strict=True, words=frozenset({"yes", "no"}), firm=True)
        class Purchase:
            def active(self, chat: int) -> bool:
                return True

            def stop(self, chat: int) -> None:
                raise AssertionError("OpenTable cancel touched purchase consent")
        inlet._purchases = Purchase()
        await inlet._tool("opentable_search", {"request": QUERY}, OWNER)
        await until(lambda: bool(rig.agents) and rig.agents[0].running)
        inlet._dispatch(update("/opentable cancel"))
        await until(lambda: rig.agents[0].cancel_reason is not None)
        assert not prior.future.done() and floors == ["codex"]
        await until(lambda: not inlet.task_running)
        inlet._purchases = None
        prior.future.cancel()
        # A normal computer task is a positive control: /opentable cancel must leave it running.
        assert inlet._start_task("open Calculator", OWNER)["ok"]
        await until(lambda: len(rig.agents) == 2 and rig.agents[1].running)
        inlet._dispatch(update("/opentable cancel", update_id=3))
        await until(lambda: any("No /opentable" in text for _, text in rig.api.sent))
        assert rig.agents[1].cancel_reason is None
        rig.agents[1].release.set()
        await until(lambda: not inlet.task_running)
        await inlet._shutdown()
    asyncio.run(run())


def test_daemon_passes_opt_in_without_network_or_partner_setup(monkeypatch: pytest.MonkeyPatch) -> None:
    from types import SimpleNamespace

    from eve_bridge import daemon as daemon_mod
    monkeypatch.setenv("EVE_OPENTABLE", "1")
    monkeypatch.delenv("OPENROUTER_API_KEY", raising=False)
    monkeypatch.setattr(daemon_mod.telegram_mod, "configured", lambda: CFG)
    for method in ("_make_apps", "_make_direct", "_make_meeter", "_lights_hub", "_spotify_hub", "_firetv_hub", "_make_canvas",
                   "_make_share", "_make_chief"):
        monkeypatch.setattr(Daemon, method, lambda *a, **k: None)
    from eve_bridge import watch
    monkeypatch.setattr(watch, "make_watcher", lambda: None)
    captured: dict[str, Any] = {}
    monkeypatch.setattr(daemon_mod.telegram_mod, "make_inlet", lambda cfg, **kw: captured.update(kw))
    daemon = SimpleNamespace(_agent_cfg=SimpleNamespace(enabled=True), _make_agent=lambda *a: None,
                             _recall_cfg=None, _photo_for_owner=None, _thinker=None, _on_agent_state=None,
                             _scene=None, _head=None, _set_sound=None, _on_caption=None)
    Daemon._make_telegram(daemon)
    assert captured["opentable_enabled"] is True and captured["agent_enabled"] is True
    assert captured["agent_factory"] is daemon._make_agent
    monkeypatch.setenv("EVE_OPENTABLE", "0")
    Daemon._make_telegram(daemon)
    assert captured["opentable_enabled"] is False
