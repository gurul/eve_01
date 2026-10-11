"""tools/intent_eval.py, the tool-choice eval harness: its labels, its code-word path and its recorder. No network:
the model is FakeCreate."""

from __future__ import annotations

import asyncio
import json
import sys
from pathlib import Path
from typing import Any

import pytest
from test_telegram import FakeApi, FakeCreate, Rig, call, say

from eve_bridge import menu_tools

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "tools"))

import intent_eval as ev  # noqa: E402

DEV_SET = Path(__file__).parent / "fixtures" / "intent_dev.json"


@pytest.mark.parametrize("name,args,lab", [
    ("start_task", {}, "computer_task"), ("take_photo", {}, "camera_photo"), ("screenshot", {}, "screenshot"),
    ("send_file", {}, "send_file_to_owner"), ("list_files", {}, "mac_files_read"), ("take_notes", {}, "room_notes"),
    ("remember", {}, "memory_write"), ("start_coding_session", {}, "coding_session"),
    ("think_hard", {}, "deep_reasoning"), ("recent_messages", {}, "chat_history_recall"),
    ("memory_search", {}, "memory_recall"), ("forget_preview", {}, "memory_forget"), ("web_search", {}, "web_search"),
    ("capture_note", {}, "notes"), ("list_todos", {}, "todos_read"), ("watch_add", {}, "price_watch"),
    ("meet_join", {}, "meet"), ("lights_set", {}, "lights"), ("spotify_control", {}, "music"),
    ("firetv_control", {}, "tv_control"), ("camera_control", {}, "camera_aim"), ("camera_status", {}, "camera_aim"),
    ("america_ask", {}, "gov_legal_lookup"),
    ("opentable_search", {}, "restaurant_booking"), ("events_search", {}, "events"),
    ("jobs_list", {}, "jobs_and_reminders"), ("take_on", {}, "jobs_and_reminders"), ("make_app", {}, "app_builder"),
    ("open_apps", {}, "app_builder"), ("calendar__list_events", {}, "calendar_read"),
    ("calendar__create_event", {}, "calendar_write"), ("gmail__search_messages", {}, "email_read"),
    ("find_contact", {}, "contacts_read"), ("maps__compute_routes", {}, "maps_directions"),
    ("github__list_issues", {}, "github"), ("open_app", {"apps": ["calendar"]}, "calendar_read"),
    ("COMPOSIO_MULTI_EXECUTE_TOOL", {"tools": [{"tool_slug": "SLACK_FETCH_CONVERSATION_HISTORY"}]}, "slack"),
    ("COMPOSIO_SEARCH_TOOLS", {"queries": [{"use_case": "find my linear tickets"}]}, "linear"),
    ("COMPOSIO_SEARCH_TOOLS", {"queries": [{"use_case": "the notion page about rent"}]}, "notion"),
    ("COMPOSIO_MULTI_EXECUTE_TOOL", {"tools": [{"tool_slug": "GMAIL_SEND_EMAIL"}]}, "email_send"),
    ("claude_relay", {"action": "on"}, "claude_relay"), ("codex_relay", {"action": "on"}, "codex"),
    ("spend_report", {}, "spend_report"), ("set_stealth", {"on": True}, "stealth"), ("stop_task", {}, "stop_task"),
    ("pair_eve_link", {"new_link": False}, "phone_pairing"), ("", {}, "none"),
])
def test_each_first_tool_maps_to_its_label(name: str, args: dict[str, Any], lab: str) -> None:
    assert ev.label(name, args) == lab
    assert lab in ev.LABELS


def test_every_label_is_reachable() -> None:
    reached = set(ev.TOOL_LABELS.values()) | {lab for _, lab in ev._PREFIXES} | {
        "notes", "app_builder", "calendar_read", "calendar_write", "email_read", "email_send", "maps_directions",
        "github", "slack", "notion", "linear", "ask", "none"}
    assert set(ev.LABELS) <= reached


@pytest.mark.parametrize("text,lab", [
    ("Sure, a theater or the TV at home?", "ask"), ("Here are three shows.", "none"), ("", "none"),
    ("Done. Anything else?", "ask"), ("I can't do that right now.", "none"),
])
def test_a_reply_with_no_tool_is_ask_or_none(text: str, lab: str) -> None:
    assert ev.reply_label(text) == lab


@pytest.mark.parametrize("text,lab", [
    ("/spend", "spend_report"), ("stealth", "stealth"), ("claude on", "claude_relay"), ("/rundown", "daily_rundown"),
    ("/meet 3pm", "meet"), ("codex on", "codex"), ("stop", "stop_task"), ("new claude", "coding_session"),
    ("/jobs", "jobs_and_reminders"), ("what did you spend today", ""), ("any concerts in seattle", ""),
])
def test_code_words_are_scored_by_their_code_path(text: str, lab: str) -> None:
    assert ev.code_route(text) == lab


@pytest.mark.parametrize("text,lab", [
    # v2 correctness review: the matchers _handle runs before any model call (firetv, lights, spotify, quick answers,
    # the chief's words) were not run, so the harness scored the brain on words production never sends it
    ("turn on the tv", "tv_control"), ("lights off", "lights"), ("pause the music", "music"),
    ("what time is it", "none"), ("/jobs", "jobs_and_reminders"), ("go c12", "jobs_and_reminders"),
    ("codex: fix the tests", "codex"), ("claude: run the tests", "claude_relay"), ("/america visa rules", "gov_legal_lookup"),
    ("i want to watch a movie tonight", ""), ("book dinner for 4 fri at 7", ""),
    ("camera left", "camera_aim"), ("zoom the camera in", "camera_aim"), ("point the camera at the door", ""),
])
def test_the_inlet_s_own_matchers_take_what_production_takes_by_code(text: str, lab: str) -> None:
    from eve_bridge import lights

    rig = Rig(FakeApi(), FakeCreate(), firetv=object(), spotify=object(), obsbot=object())
    rig.inlet.chief = type("Chief", (), {"live": None})()
    rig.inlet._eval_lights = lights.Lights([lights.Light(name="desk lamp", kind="govee", room="office")])
    assert ev.code_route(text, rig.inlet) == lab


def test_a_slash_watch_reaches_the_brain_as_production_rewrites_it() -> None:
    assert ev.code_route("/watch the price of a standing desk") == ""
    assert ev.brain_text("/watch the price of a standing desk") == "Watch the price of a standing desk"
    assert ev.brain_text("track that desk") == "track that desk"


@pytest.mark.parametrize("args,text,lab", [
    ({"apps": ["workspace"]}, "move ERA-1234 to in progress in linear", "linear"),
    ({"apps": ["workspace"]}, "what's on my notion page about rent", "notion"),
    ({"apps": ["workspace"]}, "catch me up on slack", "slack"),
    ({"apps": ["workspace", "github"]}, "whats the status of my latest pr on github", "github"),
    ({"apps": ["gmail", "calendar"]}, "anything on my calendar today", "calendar_read"),
    ({"apps": ["gmail"]}, "did my order ship", "email_read"),
])
def test_open_app_is_labelled_by_the_app_the_owner_meant(args: dict[str, Any], text: str, lab: str) -> None:
    assert ev.label("open_app", args, text=text) == lab


def test_open_app_runs_and_the_call_it_leads_to_is_scored() -> None:
    create = FakeCreate(call("open_app", {"apps": ["workspace"]}), call("web_search", {"query": "x"}))
    rig = Rig(FakeApi(), create, direct=ev._direct_apps(), apps=ev._Workspace())
    row = asyncio.run(ev._ask(rig.inlet, {"id": "o", "text": "check linear", "primary": "web_search"}, 0, set()))
    assert row.tool == "web_search" and row.got == "web_search" and len(create.requests) == 2
    create = FakeCreate(call("open_app", {"apps": ["workspace"]}), say("Which board?"))
    rig = Rig(FakeApi(), create, direct=ev._direct_apps(), apps=ev._Workspace())
    row = asyncio.run(ev._ask(rig.inlet, {"id": "p", "text": "check linear", "primary": "linear"}, 0, set()))
    assert row.tool == "open_app" and row.got == "linear"


def _row(ident: str, prim: str, accept: tuple[str, ...], *steps: Any, group: str = "", error: str = "") -> Any:
    return ev.Row(ident, prim, accept, tuple(ev.Step(*st) for st in steps), "brain", group, error)


def test_hard_negatives_that_act_are_reported(capsys: pytest.CaptureFixture[str]) -> None:
    rows = [_row("a", "none", ("none", "ask"), ("meet_join", "meet", True)),
            _row("b", "none", ("none",), ("meet_status", "meet", False)),
            _row("c", "events", ("events",), ("events_search", "events", False)),
            _row("d", "events", ("events",), error="APIConnectionError")]
    assert [r.passed for r in rows] == [False, False, True, False]
    ev.report(rows)
    out = capsys.readouterr().out
    assert "HARD_NEGATIVE_ACTED 1: a->meet_join" in out and "TOTAL 1/4 (25.0%)" in out
    assert "PROVIDER_ERRORS 1: d APIConnectionError" in out and "   none: 0/2" in out


# ---- the v4 scorer --------------------------------------------------------------------------------------------

def test_the_prep_set_is_exactly_the_registered_lookups() -> None:
    assert ev.PREP == {"list_files", "calendar__list_events", "calendar__search_events", "gmail__search_threads",
                       "gmail__get_thread", "memory_search", "watch_list", "jobs_list", "search_notes"}
    assert not any(ev.acting(n, {}) for n in ev.PREP)


def test_a_first_action_in_expect_passes() -> None:
    assert ev.score(("events",), [ev.Step("events_search", "events")]) == (True, "events")
    # a lookup that is itself acceptable passes on (a), whatever follows
    assert ev.score(("mac_files_read",), [ev.Step("list_files", "mac_files_read"),
                                          ev.Step("", "none", round=2)])[0]


def test_a_lookup_then_the_right_tool_passes() -> None:
    steps = [ev.Step("list_files", "mac_files_read", round=1), ev.Step("send_file", "send_file_to_owner", True, 2)]
    assert ev.score(("send_file_to_owner",), steps) == (True, "mac_files_read>send_file_to_owner")
    steps = [ev.Step("calendar__list_events", "calendar_read", round=1),
             ev.Step("calendar__update_event", "calendar_write", True, 2)]
    assert ev.score(("calendar_write",), steps)[0]


def test_positive_control_a_lookup_then_the_wrong_tool_fails() -> None:
    steps = [ev.Step("list_files", "mac_files_read", round=1), ev.Step("start_task", "computer_task", True, 2)]
    assert ev.score(("send_file_to_owner",), steps) == (False, "mac_files_read>computer_task")


def test_positive_control_a_lookup_then_nothing_fails_on_a_case_that_wants_a_tool() -> None:
    assert ev.score(("calendar_write",), [ev.Step("calendar__list_events", "calendar_read")]) == (
        False, "calendar_read>(stopped)")
    steps = [ev.Step("calendar__list_events", "calendar_read"), ev.Step("", "none", round=2)]
    assert not ev.score(("calendar_write",), steps)[0]
    steps = [ev.Step("memory_search", "memory_recall"), ev.Step("", "ask", round=2)]
    assert not ev.score(("memory_forget",), steps)[0]


def test_positive_control_a_non_prep_first_action_gets_no_second_chance() -> None:
    steps = [ev.Step("recent_messages", "chat_history_recall"), ev.Step("claude_relay", "claude_relay", True, 2)]
    assert ev.score(("claude_relay",), steps) == (False, "chat_history_recall")
    steps = [ev.Step("web_search", "web_search"), ev.Step("events_search", "events", False, 2)]
    assert not ev.score(("events",), steps)[0]


def test_positive_control_a_call_beside_the_lookup_in_its_own_round_is_not_a_second_action() -> None:
    steps = [ev.Step("list_files", "mac_files_read", round=1), ev.Step("send_file", "send_file_to_owner", True, 1)]
    assert not ev.score(("send_file_to_owner",), steps)[0]


def test_no_tool_reply_is_none_on_a_none_case_question_or_not() -> None:
    assert ev.reply_label("lol what happened?", "none") == "none"
    assert ev.reply_label("lol what happened?", "web_search") == "ask"
    assert ev.reply_label("lol what happened?", "ask") == "ask"
    assert ev.reply_label("Sounds fun.", "ask") == "none"


def test_positive_control_an_acting_call_at_step_two_on_a_hard_negative_is_counted(
        capsys: pytest.CaptureFixture[str]) -> None:
    steps = (("memory_search", "memory_recall", False, 1), ("remember", "memory_write", True, 2))
    rows = [_row("hn", "web_search", ("web_search",), *steps, group="hard_negative"),
            _row("no", "none", ("none",), ("memory_search", "memory_recall", False, 1), ("", "none", False, 2)),
            _row("cap", "memory_write", ("memory_write",), *steps)]
    assert rows[0].hard_negative and rows[0].acted and not rows[0].passed
    assert rows[1].passed and not rows[1].acted                    # a lookup, then a plain reply: (b) holds
    assert rows[2].passed and not rows[2].hard_negative
    ev.report(rows)
    assert "HARD_NEGATIVE_ACTED 1: hn->remember" in capsys.readouterr().out


def test_a_code_path_that_acts_counts_as_acting() -> None:
    rig = Rig(FakeApi(), FakeCreate(), firetv=object())
    row = asyncio.run(ev._ask(rig.inlet, {"id": "tv", "text": "turn on the tv", "primary": "none",
                                          "group": "hard_negative"}, 0, set()))
    assert row.via == "code" and row.got == "tv_control" and row.acted and row.hard_negative and not row.passed


def test_prep_stubs_are_short_neutral_and_name_the_request() -> None:
    out = ev.prep_stub("list_files", {"path": "~/Desktop"}, "send me the tax pdf thats on my desktop")
    assert out["files"][0]["name"] == "tax.pdf" and out["folder"] == "~/Desktop"
    assert ev.prep_stub("list_files", {}, "whats in the notes.txt on my desktop")["files"][0]["name"] == "notes.txt"
    assert ev.prep_stub("memory_search", {"query": "x"}, "forget my old address") == {"ok": True, "results": []}
    cal = ev.prep_stub("calendar__list_events", {}, "cancel the gym event tmrw")
    assert len(cal["events"]) == 1 and "gym" in cal["events"][0]["summary"]
    assert len(json.dumps(ev.prep_stub("gmail__search_threads", {}, "reply to the landlord email"))) < 400


def test_a_lookup_gets_its_stub_and_the_next_round_is_scored() -> None:
    """End to end through a real inlet: list_files returns the stub, a second round runs, send_file is recorded
    and never runs; the stub is what the second request carries."""
    create = FakeCreate(call("list_files", {"path": "~/Desktop"}), call("send_file", {"path": "~/Desktop/tax.pdf"}))
    rig = Rig(FakeApi(), create)
    row = asyncio.run(ev._ask(rig.inlet, {"id": "f", "text": "send me the tax pdf on my desktop",
                                          "primary": "send_file_to_owner"}, 0, set()))
    assert row.passed and row.tool == "list_files>send_file" and row.acted and len(create.requests) == 2
    assert "tax.pdf" in json.dumps(create.requests[1]) and rig.api.sent == []


def test_a_lookup_then_a_reply_is_scored_on_the_reply() -> None:
    create = FakeCreate(call("memory_search", {"query": "old address"}), say("Which address do you mean?"))
    rig = Rig(FakeApi(), create, memory=ev._Memory())
    row = asyncio.run(ev._ask(rig.inlet, {"id": "g", "text": "forget my old address", "primary": "memory_forget"},
                              0, set()))
    assert not row.passed and row.got == "memory_recall>ask"


def test_positive_control_a_call_to_a_tool_not_offered_is_an_action_and_stops_the_turn() -> None:
    # no memory lent: memory_search is not offered, so production refuses it; it is not given a repair round
    create = FakeCreate(call("memory_search", {"query": "a"}), call("remember", {"fact": "x"}))
    rig = Rig(FakeApi(), create)
    row = asyncio.run(ev._ask(rig.inlet, {"id": "u", "text": "note: i like aisle seats", "primary": "memory_write"},
                              0, set()))
    assert not row.passed and row.got == ev.UNKNOWN and len(create.requests) == 1


def test_a_second_lookup_is_not_given_a_third_round() -> None:
    create = FakeCreate(call("memory_search", {"query": "a"}), call("memory_search", {"query": "b"}),
                        call("remember", {"fact": "x"}))
    rig = Rig(FakeApi(), create, memory=ev._Memory())
    row = asyncio.run(ev._ask(rig.inlet, {"id": "h", "text": "note: i like aisle seats", "primary": "memory_write"},
                              0, set()))
    assert not row.passed and len(create.requests) == 2


def test_the_dev_set_loads_with_known_labels(tmp_path: Path) -> None:
    cases = ev.load_cases(DEV_SET)
    assert len(cases) == 15 and len({c["id"] for c in cases}) == 15
    assert all(set(ev.acceptable(c)) <= set(ev.LABELS) for c in cases)
    bad = tmp_path / "bad.json"
    bad.write_text(json.dumps([{"text": "hi"}]), encoding="utf-8")
    with pytest.raises(SystemExit):
        ev.load_cases(bad)


def test_the_harness_records_the_first_call_and_runs_nothing(monkeypatch: pytest.MonkeyPatch) -> None:
    """_ask through a real inlet with a scripted model: the first call is recorded and never runs, and an app
    turn's block rides the request."""
    monkeypatch.setenv("EVE_MENU_TOOLS", "on")
    create = FakeCreate(call(menu_tools.CLAUDE, {"action": "on", "session": ""}))
    rig = Rig(FakeApi(), create, claude_sessions=lambda: ["/Users/g/repo"])
    row = asyncio.run(ev._ask(rig.inlet, {"id": "t", "text": "patch me into claude", "primary": "claude_relay"}, 0,
                              set()))
    assert row.passed and row.tool == "claude_relay" and row.via == "brain" and row.acted
    assert rig.inlet.claude is False and rig.api.sent == []                      # nothing executed
    from eve_bridge.link_chat import app_block

    assert app_block().strip()[:40] in json.dumps(create.requests[0], ensure_ascii=False).replace("\\n", "\n")


def test_a_question_back_is_scored_ask() -> None:
    rig = Rig(FakeApi(), FakeCreate(say("A theater, or the TV at home?")))
    row = asyncio.run(ev._ask(rig.inlet, {"id": "m", "text": "i want to watch a movie", "primary": "ask"}, 0, set()))
    assert row.passed and row.got == "ask" and not row.acted


# ---- round-4 review: the harness's acting() missed app writes (only stricter; no re-score) ---------------------

@pytest.mark.parametrize("name,args", [
    ("calendar__quick_add_event", {"text": "gym tomorrow 7am"}),
    ("calendar__respond_to_event", {"eventId": "e1", "response": "accepted"}),
    ("drive__create_file", {"name": "notes.txt"}),
    ("github__create_issue", {"title": "x"}), ("github__merge_pull_request", {"pullNumber": 3}),
    ("github__push_files", {"files": []}), ("github__create_or_update_file", {"path": "a"}),
    ("github__add_issue_comment", {"body": "x"}), ("github__fork_repository", {}),
    ("COMPOSIO_MULTI_EXECUTE_TOOL", {"tools": [{"tool_slug": "SLACK_SEND_MESSAGE", "arguments": {}}]}),
    ("COMPOSIO_MULTI_EXECUTE_TOOL", {"tools": [{"tool_slug": "SLACK_FETCH_CONVERSATION_HISTORY", "arguments": {}},
                                               {"tool_slug": "NOTION_CREATE_PAGE", "arguments": {}}]}),
    ("COMPOSIO_MULTI_EXECUTE_TOOL", {"tools": [{"tool_slug": "LINEAR_CREATE_LINEAR_ISSUE", "arguments": {}}]}),
])
def test_app_writes_count_as_acting(name: str, args: dict[str, Any]) -> None:
    assert ev.acting(name, args), name
    assert ev.acting(name, args, set()), name                    # by its words, whatever the writes set says


@pytest.mark.parametrize("name,args", [
    ("calendar__list_events", {}), ("gmail__search_threads", {}), ("github__get_pull_request", {}),
    ("github__list_pull_requests", {}), ("drive__search_files", {}), ("gmail__get_label", {}),
    ("COMPOSIO_MULTI_EXECUTE_TOOL", {"tools": [{"tool_slug": "SLACK_FETCH_CONVERSATION_HISTORY", "arguments": {}}]}),
    ("COMPOSIO_SEARCH_TOOLS", {"queries": [{"use_case": "send a slack message"}]}),
])
def test_app_reads_do_not_count_as_acting(name: str, args: dict[str, Any]) -> None:
    assert not ev.acting(name, args, set()), name


def test_the_writes_set_makes_a_direct_tool_acting() -> None:
    assert not ev.acting("calendar__frobnicate_event", {}, set())
    assert ev.acting("calendar__frobnicate_event", {}, {"calendar__frobnicate_event"})


def test_the_recorder_passes_the_writes_set_to_acting(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(ev, "_direct_write", lambda name: False)       # only the writes set can say it writes
    create = FakeCreate(call("open_app", {"apps": ["github"]}), call("github__star_repository", {"query": "x"}))
    rig = Rig(FakeApi(), create, direct=ev._direct_apps(), apps=ev._Workspace())
    row = asyncio.run(ev._ask(rig.inlet, {"id": "w", "text": "star my repo", "primary": "none",
                                          "group": "hard_negative"}, 0, {"github__star_repository"}))
    assert row.tool == "github__star_repository" and row.acted and row.hard_negative


# ---- the case set's own rules (2026-10-11: two blind camera hard negatives accepted computer_task) -------------

def _write_cases(tmp_path: Path, cases: list[dict[str, Any]]) -> Path:
    p = tmp_path / "cases.json"
    p.write_text(json.dumps({"cases": cases}))
    return p


def test_a_hard_negative_that_accepts_an_acting_only_label_is_refused(tmp_path: Path) -> None:
    assert {"computer_task", "camera_photo"} <= ev.acting_only_labels()
    assert not {"camera_aim", "jobs_and_reminders", "web_search", "none"} & ev.acting_only_labels()
    clash = {"id": "c1", "text": "track my package", "primary": "web_search", "expect": ["computer_task"],
             "group": "hard_negative"}
    with pytest.raises(SystemExit, match="CONFLICT: .*c1"):
        ev.load_cases(_write_cases(tmp_path, [clash]))
    fine = {**clash, "expect": ["none"], "never": ["camera_aim"]}
    assert ev.load_cases(_write_cases(tmp_path, [fine])) == [fine]
    with pytest.raises(SystemExit, match="BAD_CASES"):
        ev.load_cases(_write_cases(tmp_path, [{**fine, "never": ["not_a_label"]}]))


def test_never_labels_are_reported_and_miss_the_bar(capsys: pytest.CaptureFixture[str]) -> None:
    moved = ev.Row("n1", "web_search", ("web_search", "computer_task"),
                   (ev.Step("camera_control", "camera_aim", True),), "brain", never=("camera_aim",))
    searched = ev.Row("n2", "web_search", ("web_search",), (ev.Step("web_search", "web_search"),), "brain",
                      never=("camera_aim",))
    assert moved.never_fired == ["camera_aim"] and searched.never_fired == []
    ev.report([moved, searched])
    assert "NEVER_FIRED 1: n1->camera_aim" in capsys.readouterr().out
    assert ev.bars([moved, searched], ["web_search=0"]) == ["NEVER_FIRED n1"]
    assert ev.bars([searched], ["web_search=100"]) == []
