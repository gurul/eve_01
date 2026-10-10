"""Ordinary private edits run; communication, deletion, sharing and unknown acts still ask."""

from __future__ import annotations

import json

import httpx
import pytest

from eve_bridge import apps_mcp, composio_tools, google_rest


def direct(name, args, rule="auto"):
    app, raw = apps_mcp.split_name(name)
    bridge = apps_mcp.AppsBridge(apps_mcp.AppsConfig(enabled=True, policy={app: rule}))
    bridge._raw[name] = {"name": raw, "annotations": {"readOnlyHint": False}}
    return bridge.decide(name, args).action


@pytest.mark.parametrize("name,args", [
    ("drive__create_file", {"title": "Notes", "textContent": "A private draft"}),
    ("gmail__create_draft", {"to": ["reader@example.org"], "subject": "Draft", "body": "Unsent"}),
    ("calendar__create_event", {"summary": "Focus", "startTime": "2026-10-10T09:00:00Z"}),
    ("calendar__quick_add_event", {"text": "Focus tomorrow at 9"}),
    ("calendar__update_event", {"eventId": "event", "summary": "Focus", "notifyGuests": False}),
    ("github__create_repository", {"name": "private-notes", "private": True}),
])
def test_supported_private_changes_run_and_owner_overrides_still_apply(name, args):
    assert direct(name, args) == "run"
    assert direct(name, args, "write") == "run"
    assert direct(name, args, "ask") == "ask"
    assert direct(name, args, "read") == "refuse"
    assert direct(name, None) == "ask"  # a name without the actual arguments is insufficient


@pytest.mark.parametrize("name,args", [
    ("gmail__send_message", {"to": ["reader@example.org"], "body": "Hello"}),
    ("gmail__delete_draft", {"id": "draft"}),
    ("gmail__create_draft", {"body": "Hello", "send": True}),
    ("drive__delete_file", {"fileId": "file"}),
    ("drive__create_file", {"title": "Notes", "parents": ["shared-folder"]}),
    ("drive__create_file", {"title": "Notes", "permissions": [{"type": "anyone"}]}),
    ("drive__create_file", {"title": "Notes", "body": {"permissions": [{"type": "anyone"}]}}),
    ("calendar__delete_event", {"eventId": "event", "notifyGuests": False}),
    ("calendar__respond_to_event", {"eventId": "event", "responseStatus": "accepted"}),
    ("calendar__create_event", {"attendeeEmails": ["reader@example.org"], "notifyGuests": False}),
    ("calendar__create_event", {"notifyGuests": True}),
    ("calendar__create_event", {"calendarId": "shared-calendar"}),
    ("calendar__quick_add_event", {"text": "Meeting reader@example.org tomorrow"}),
    ("calendar__update_event", {"eventId": "event", "summary": "New time"}),
    ("calendar__update_event", {"eventId": "event", "notify_guests": False}),
    ("calendar__update_event", {"eventId": "event", "sendUpdates": "none"}),
    ("calendar__update_event", {"eventId": "event", "notifyGuests": False,
                               "addAttendeeEmails": ["reader@example.org"]}),
    ("github__create_repository", {"name": "public-notes", "private": False}),
    ("github__create_repository", {"name": "public-notes", "PRIVATE": True}),
    ("github__create_repository", {"name": "team-notes", "private": True, "organization": "team"}),
    ("github__create_issue", {"title": "Public message"}),
    ("github__push_files", {"files": []}),
    ("github__merge_pull_request", {"pullNumber": 3}),
    ("drive__frobnicate", {}),
])
def test_consequential_or_unclassified_writes_ask_even_with_write_policy(name, args):
    assert direct(name, args) == "ask"
    assert direct(name, args, "write") == "ask"
    assert direct(name, args, "read") == "refuse"


@pytest.mark.parametrize("name", ["get_and_purchase", "get_and_charge", "get_and_pay", "get_and_deploy"])
def test_consequential_names_cannot_hide_behind_a_read_hint(name):
    assert not apps_mcp.tool_reads({"name": name, "annotations": {"readOnlyHint": True}})
    assert not composio_tools.is_read_only("SHOP_" + name.upper())


def multi(*calls, policy=None):
    return composio_tools.decide(composio_tools.MULTI_EXECUTE, {"tools": [
        {"tool_slug": slug, "arguments": args, "account": "work"} for slug, args in calls]}, policy).action


def test_composio_checks_each_calls_actual_arguments_and_takes_the_strictest():
    routine = ("GOOGLECALENDAR_CREATE_EVENT", {"summary": "Focus"})
    invite = ("GOOGLECALENDAR_CREATE_EVENT", {"attendees": ["reader@example.org"]})
    assert multi(routine) == "run"
    assert multi(routine, invite) == "ask"  # same slug, different arguments cannot collapse to one verdict
    assert multi(("GOOGLECALENDAR_UPDATE_EVENT", {"event_id": "event", "send_updates": "none"})) == "run"
    assert multi(("GOOGLECALENDAR_UPDATE_EVENT", {"event_id": "event"})) == "ask"
    assert multi(("GOOGLEDRIVE_UPLOAD_FILE", {"file_to_upload": "notes.txt"})) == "run"
    assert multi(("GOOGLEDRIVE_UPLOAD_FILE", {"file_to_upload": "notes.txt", "folder_id": "shared"})) == "ask"
    assert multi(("GOOGLEDOCS_CREATE_DOCUMENT", {"title": "Private draft"})) == "run"
    assert multi(("SLACK_SEND_MESSAGE", {"channel": "channel", "text": "Hello"}),
                 policy={"slack": "write"}) == "ask"
    assert multi(("GMAIL_CREATE_DRAFT", {"body": "Unsent"})) == "refuse"  # explicit work-mail restriction
    assert multi(routine, policy={"googlecalendar": "ask"}) == "ask"
    assert multi(routine, policy={"googlecalendar": "read"}) == "refuse"


@pytest.mark.parametrize("tools", [None, "opaque", [{}], [None], [{"tool_slug": ""}]])
def test_uninspectable_composio_batches_do_not_run(tools):
    assert composio_tools.decide(composio_tools.MULTI_EXECUTE, {"tools": tools}).action == "ask"


def test_workbench_writes_need_review_when_their_arguments_are_not_known():
    code = "run_composio_tool('GOOGLEDRIVE_UPLOAD_FILE', arguments)"
    assert composio_tools.decide(composio_tools.WORKBENCH, {"code_to_execute": code}).action == "ask"
    assert composio_tools.decide(composio_tools.WORKBENCH, {"code_to_execute": "print(sum([1,2]))"}).action == "run"


def test_routine_calendar_decision_matches_real_rest_delivery_parameters():
    requests = []

    def respond(request):
        requests.append(request)
        # A pre-existing event has guests; the edit must not silently inherit delivery.
        return httpx.Response(200, json={"id": "event", "attendees": [{"email": "guest@example.org"}]})

    with httpx.Client(transport=httpx.MockTransport(respond)) as http:
        client = google_rest.GoogleRestClient("calendar", lambda: "test", http=http)
        create = {"summary": "Focus", "startTime": "2026-10-10T09:00:00Z", "endTime": "2026-10-10T10:00:00Z"}
        assert direct("calendar__create_event", create) == "run"
        client.call_tool("create_event", create)
        assert requests[-1].url.params["sendUpdates"] == "none"
        assert "attendees" not in json.loads(requests[-1].content)

        edit = {"eventId": "event", "summary": "Moved", "notifyGuests": False}
        assert direct("calendar__update_event", edit) == "run"
        requests.clear()
        client.call_tool("update_event", edit)
        assert len(requests) == 1 and requests[0].method == "PATCH"
        assert requests[0].url.params["sendUpdates"] == "none"

        # Positive control: omitting the real adapter key causes guest delivery and must ask.
        edit.pop("notifyGuests")
        assert direct("calendar__update_event", edit) == "ask"
        requests.clear()
        client.call_tool("update_event", edit)
        assert [r.method for r in requests] == ["GET", "PATCH"]
        assert requests[-1].url.params["sendUpdates"] == "all"


def test_ambiguous_aliases_or_nonmapping_arguments_cannot_make_a_write_routine():
    assert direct("calendar__create_event", {"notifyGuests": False, "notify_guests": True}) == "ask"
    assert direct("drive__create_file", ["file"]) == "ask"
    assert composio_tools.decide(composio_tools.MULTI_EXECUTE, None).action == "ask"


def calendar_bridge(client, rule="auto"):
    bridge = apps_mcp.AppsBridge(apps_mcp.AppsConfig(enabled=True, policy={"calendar": rule}))
    bridge._clients["calendar"] = client
    bridge._raw["calendar__update_event"] = {
        "name": "update_event", "annotations": {"readOnlyHint": False}}
    return bridge


@pytest.mark.parametrize("rule,original_flag", [("auto", {}), ("write", {"notifyGuests": None})])
def test_prepare_private_edit_pins_delivery_and_survives_a_guest_race(rule, original_flag):
    requests = []

    def respond(request):
        requests.append(request)
        if request.method == "GET":
            assert not request.url.params  # neither a field mask nor maxAttendees can hide guests
            assert request.url.path.endswith("/calendars/primary/events/event")
            return httpx.Response(200, json={"id": "event", "attendees": []})
        # Someone added a guest after the preflight read; execution must still explicitly suppress delivery.
        assert request.method == "PATCH" and request.url.params["sendUpdates"] == "none"
        assert json.loads(request.content) == {"summary": "Moved"}
        return httpx.Response(200, json={"id": "event", "attendees": [{"email": "guest@example.org"}]})

    with httpx.Client(transport=httpx.MockTransport(respond)) as http:
        bridge = calendar_bridge(google_rest.GoogleRestClient("calendar", lambda: "test", http=http), rule)
        args = {"eventId": "event", "summary": "Moved", **original_flag}
        before = dict(args)
        prepared = bridge.prepare("calendar__update_event", args)
        assert prepared is not args and args == before
        assert prepared == {**args, "notifyGuests": False}
        assert bridge.decide("calendar__update_event", prepared).action == "run"
        assert bridge.execute("calendar__update_event", prepared)["ok"]
        assert [r.method for r in requests] == ["GET", "PATCH"]


@pytest.mark.parametrize("event", [
    {"id": "event", "attendees": [{"email": "guest@example.org"}]},
    {"id": "event", "attendees": [{}]},
    {"id": "event", "attendees": [], "attendeesOmitted": True},
    {"id": "event", "attendeesOmitted": "false"},
    {"id": "event", "attendees": None},
    {"id": "event", "attendees": {}},
    {"id": "event", "status": "cancelled"},
    {"id": "different"},
    {},
    [],
])
def test_prepare_guests_or_incomplete_event_leave_arguments_and_confirmation_unchanged(event):
    with httpx.Client(transport=httpx.MockTransport(lambda r: httpx.Response(200, json=event))) as http:
        bridge = calendar_bridge(google_rest.GoogleRestClient("calendar", lambda: "test", http=http))
        args = {"eventId": "event", "summary": "Moved"}
        prepared = bridge.prepare("calendar__update_event", args)
        assert prepared == args and prepared is not args
        assert bridge.decide("calendar__update_event", prepared).action == "ask"


def test_prepare_complete_event_without_attendees_field_is_private():
    with httpx.Client(transport=httpx.MockTransport(lambda r: httpx.Response(200, json={"id": "event"}))) as http:
        bridge = calendar_bridge(google_rest.GoogleRestClient("calendar", lambda: "test", http=http))
        assert bridge.prepare("calendar__update_event", {"eventId": "event"})["notifyGuests"] is False


@pytest.mark.parametrize("failure", ["http", "transport"])
def test_prepare_failed_read_asks_and_does_not_log_event_content(failure, caplog):
    def respond(request):
        if failure == "transport":
            raise httpx.ReadTimeout("PRIVATE EVENT TEXT", request=request)
        return httpx.Response(403, json={"error": {"message": "PRIVATE EVENT TEXT"}})

    with httpx.Client(transport=httpx.MockTransport(respond)) as http:
        bridge = calendar_bridge(google_rest.GoogleRestClient("calendar", lambda: "test", http=http))
        args = {"eventId": "event", "summary": "Moved"}
        with caplog.at_level("INFO", logger="eve_bridge.apps_mcp"):
            prepared = bridge.prepare("calendar__update_event", args)
        assert prepared == args and bridge.decide("calendar__update_event", prepared).action == "ask"
        assert "preparation failed" in caplog.text and "PRIVATE EVENT TEXT" not in caplog.text


@pytest.mark.parametrize("name,args,rule", [
    ("calendar__update_event", {"eventId": "event"}, "read"),
    ("calendar__update_event", {"eventId": "event"}, "ask"),
    ("calendar__update_event", {"eventId": "event", "notifyGuests": True}, "auto"),
    ("calendar__update_event", {"eventId": "event", "notifyGuests": False}, "auto"),
    ("calendar__update_event", {"eventId": "event", "notify_guests": False}, "auto"),
    ("calendar__update_event", {"eventId": "event", "calendarId": "shared"}, "auto"),
    ("calendar__update_event", {"eventId": "event", "addAttendeeEmails": ["guest@example.org"]}, "auto"),
    ("calendar__update_event", {"summary": "Moved"}, "auto"),
    ("calendar__update_event", {"eventId": " "}, "auto"),
    ("calendar__update_event", {"eventId": 1}, "auto"),
    ("calendar__delete_event", {"eventId": "event"}, "auto"),
    ("calendar__create_event", {"summary": "Focus"}, "auto"),
])
def test_prepare_respects_controls_and_policy_without_an_extra_read(name, args, rule):
    requests = []
    with httpx.Client(transport=httpx.MockTransport(lambda r: requests.append(r) or httpx.Response(200, json={}))) as http:
        bridge = calendar_bridge(google_rest.GoogleRestClient("calendar", lambda: "test", http=http), rule)
        prepared = bridge.prepare(name, args)
        assert prepared == args and prepared is not args
        assert requests == []


def test_prepare_never_trusts_an_unknown_calendar_adapter():
    class UnknownCalendar:
        def _req(self, *args, **kwargs):
            pytest.fail("an unknown adapter must not be inspected")

    bridge = calendar_bridge(UnknownCalendar())
    args = {"eventId": "event", "summary": "Moved"}
    assert bridge.prepare("calendar__update_event", args) == args
    assert bridge.decide("calendar__update_event", args).action == "ask"
