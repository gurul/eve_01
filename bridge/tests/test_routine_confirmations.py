"""User policy: routine work proceeds; dangerous native/app actions keep concrete approval.

Exercise the chat/voice dispatch paths, not only the classification table. Outside-content tracking is
independent of permission, and a delegated routine act can still ask through its actual runtime callback.
"""
from __future__ import annotations

import asyncio
from pathlib import Path

import httpx
import pytest
from test_chief import DESK_JOB
from test_telegram import OWNER, FakeApi, FakeCreate, FakeMemory, Rig, call, say
from test_tool_gate import _answer
from test_voice_agent import FakeAgent, FakeConnection, _session, _tool_call
from test_voice_taint_gate import _until

from eve_bridge import apps_mcp, chief_card, google_rest, telegram, tool_gate


@pytest.mark.parametrize("name,args", [
    ("start_task", {"goal": "make a spreadsheet"}), ("start_coding_session", {}),
    ("make_app", {}), ("change_app", {}), ("rename_app", {}), ("restore_app", {}),
    ("remember", {"claim": "I prefer matinees"}), ("capture_note", {}), ("edit_note", {}),
    ("lights_set", {}), ("spotify_control", {"action": "play"}), ("set_sound", {"on": False}),
    ("set_stealth", {"on": True}), ("watch_add", {}), ("watch_pause", {}),
    ("send_file", {"path": "chart.png"}), ("take_photo", {}), ("screenshot", {}),
    ("take_on", {}), ("take_notes", {"action": "stop"}), ("pair_eve_link", {"new_link": False}),
    ("agentcard_buy", {}), ("agentcard_confirm", {}), ("share_file", {}),
    ("take_notes", {"action": "start"}), ("meet_join", {"url": "https://meet.example.test/a"}),
    ("join_meeting", {}), ("answer_question", {"answer": "yes"}),
])
def test_routine_or_internally_confirmed_actions_have_no_outer_gate(name, args) -> None:
    assert not tool_gate.needs_confirmation(name, args)
    if name != "share_file":
        assert tool_gate.acting(name, args)       # granting routine work must not erase provenance


@pytest.mark.parametrize("name,args", [
    ("forget_apply", {"token": "preview-1"}), ("delete_app", {"app_id": "budget"}),
    ("watch_remove", {"id": "w1"}),
    ("pair_eve_link", {"new_link": True}), ("pair_eve_link", {"new_link": "true"}),
    ("pair_eve_link", {"new_link": 1}),
])
def test_native_consequential_actions_keep_the_code_gate(name, args) -> None:
    assert tool_gate.needs_confirmation(name, args)
    assert tool_gate.needs_confirmation(name, args, "a web search")


@pytest.mark.parametrize("name,args", [
    ("take_notes", {"action": "start"}), ("meet_join", {"url": "https://meet.example.test/a"}),
    ("join_meeting", {}), ("answer_question", {"answer": "yes"}),
])
def test_recording_joining_and_answering_ask_only_after_outside_text(name, args) -> None:
    assert not tool_gate.needs_confirmation(name, args)
    assert tool_gate.needs_confirmation(name, args, "a web search")


@pytest.mark.parametrize("taint", ["", "a web search"])
@pytest.mark.parametrize("decision,executes", [("Yes", True), ("No", False)])
def test_native_deletion_dispatch_needs_real_approval_even_in_clean_chat(tmp_path, taint, decision, executes) -> None:
    memory = FakeMemory(tmp_path)
    rig = Rig(FakeApi(), FakeCreate(call("forget_apply", {"token": "preview-1"}), say("Handled.")), memory=memory)

    async def go():
        task = asyncio.create_task(rig.inlet._think([telegram.message_item("user", "forget this")], OWNER,
                                                   taint=taint))
        await _answer(rig, decision)
        await task

    asyncio.run(go())
    calls = [(name, args) for name, args, _ in memory.calls]
    assert calls == ([("forget_apply", {"token": "preview-1"})] if executes else [])
    asks = [item for item in rig.api.titled if item[0] == telegram.GATE_TITLE]
    assert len(asks) == 1 and "preview-1" in asks[0][2]


def test_tainted_voice_routine_settings_and_task_start_do_not_ask() -> None:
    agent = FakeAgent(None, None)
    conn = FakeConnection([_tool_call("set_sound", "c1", on=False),
                           _tool_call("start_task", "c2", goal="make a chart")])
    session, _, _ = _session(conn, [agent])
    session._taint = "a web search"

    async def go():
        task = asyncio.create_task(session.run())
        await _until(lambda: len(conn.tool_outputs()) >= 2)
        assert conn.tool_outputs() == [{"ok": True, "sound": "off"}, {"ok": True, "goal": "make a chart"}]
        assert session.task_running and session._taint == "a web search"
        assert not any(tool_gate.SAY_EXACTLY in c for c in conn.commentary())
        agent.release.set()
        conn.feed(_tool_call("end_conversation", "end"), None)
        await task

    asyncio.run(go())


def test_routine_chief_act_has_live_runtime_question_callback(tmp_path: Path) -> None:
    from test_telegram_chief import Rig as ChiefRig

    rig = ChiefRig(tmp_path, FakeApi(), FakeCreate(), with_chief=True, hold=True)
    routine = {**DESK_JOB, "title": "Sort downloads", "end_state": "Files are organized", "never": [],
               "done_checks": [{"kind": "guru_says_done", "arg": None}],
               "phases": [{"do": "act", "goal": "organize downloads into dated folders", "door": None}]}

    async def go():
        rig.inlet._chat_id = OWNER
        card_id = await rig.take(routine, "organize my downloads")
        await rig.chief.tick()
        await _until(lambda: bool(rig.agents))
        assert rig.chief.ledger.get(card_id).phase(1).door == "two_way"
        assert rig.floors == ["codex"] and rig.inlet._live_question() is None
        assert not [e for e in rig.chief.ledger.events(card_id) if e["e"] == "asked"]
        # The executor encounters a concrete dangerous step later and can still stop for the owner.
        question = "Delete report.pdf permanently? yes / no"
        waiting = asyncio.create_task(rig.agents[0].ask_user(question))
        await _until(lambda: rig.inlet._live_question() is not None)
        assert question in rig.inlet._live_question().text
        rig.inlet.hear("no")
        assert await waiting == "no"
        rig.agents[0].release.set()
        await rig.inlet._shutdown()

    asyncio.run(go())


def test_loading_chief_phase_preserves_stored_door_and_reapplies_dangerous_goal_floor() -> None:
    raw = {"n": 1, "do": "act", "goal": "organize downloads", "door": "two_way", "status": "queued"}
    assert chief_card._phase_from(raw).door == "two_way"
    assert chief_card._phase_from({**raw, "goal": "delete the downloads"}).door == "one_way"
    assert chief_card._phase_from({**raw, "door": "one_way"}).door == "one_way"


@pytest.mark.parametrize("has_guests", [False, True])
def test_chat_prepares_private_calendar_edit_and_keeps_guest_edit_approval(has_guests) -> None:
    requests, questions = [], []

    def respond(request):
        requests.append(request)
        return httpx.Response(200, json={"id": "event", "attendees": (
            [{"email": "guest@example.org"}] if has_guests else [])})

    async def deny(question, *_args, **_kwargs):
        questions.append(question)
        return "no"

    with httpx.Client(transport=httpx.MockTransport(respond)) as http:
        client = google_rest.GoogleRestClient("calendar", lambda: "test", http=http)
        direct = apps_mcp.AppsBridge(apps_mcp.AppsConfig(enabled=True))
        direct._clients["calendar"] = client
        direct.paths["calendar"] = "rest"
        direct._raw["calendar__update_event"] = {
            "name": "update_event", "annotations": {"readOnlyHint": False}}
        rig = Rig(FakeApi(), FakeCreate())
        rig.inlet._direct = direct
        rig.inlet._taint = "a calendar event"
        rig.inlet._ask_user = deny
        args = {"eventId": "event", "summary": "Quiet reading"}
        result = asyncio.run(rig.inlet._direct_tool("calendar__update_event", args, OWNER))

    assert args == {"eventId": "event", "summary": "Quiet reading"}
    assert bool(questions) is has_guests
    assert result["ok"] is not has_guests
    assert [r.method for r in requests] == (["GET"] if has_guests else ["GET", "PATCH"])
    if not has_guests:
        assert requests[-1].url.params["sendUpdates"] == "none"
