"""EVE_MENU_TOOLS off is byte for byte origin/main (v2 correctness review, 2026-10-08): the whole request body
of a session, with every piece the switch touches lent (the chief's jobs_list, OpenTable, Ticketmaster's key), is
compared as JSON against origin/main's telegram.py; and the modules whose wording the switch widens (chief.py's
jobs_list, opentable.py) are compared against origin/main's own copies. No network: test_telegram's fakes."""

from __future__ import annotations

import json
import sys
from pathlib import Path
from typing import Any

import pytest
from test_telegram import FakeApi, FakeCreate, call, say, update
from test_telegram_chief import Rig, _origin, run_rig

from eve_bridge import chief, menu_tools, opentable, system_context, telegram

CLOCK_NOTE = "\n\nSystem context:\nCLOCK."


@pytest.fixture(autouse=True)
def _off(monkeypatch: pytest.MonkeyPatch):
    monkeypatch.setattr(system_context, "context", lambda: CLOCK_NOTE)
    monkeypatch.setenv("EVE_MENU_TOOLS", "off")
    monkeypatch.setenv("TICKETMASTER_API_KEY", "tm-test")          # events_search would be lent were the switch on
    monkeypatch.delenv("EVE_CHIEF", raising=False)
    yield
    telegram.unhide_tokens()


def _session(tmp: Path, module: Any) -> dict[str, Any]:
    api = FakeApi([update("hi", update_id=1)], [update("what are you working on for me", update_id=2)],
                  [update("book dinner for 2 at 7", update_id=3)], [update("open the calculator", update_id=4)])
    create = FakeCreate(say("Hello!"), say("Nothing right now."), say("Where would you like to eat?"),
                        call("start_task", {"goal": "open the calculator"}))
    rig = Rig(tmp, api, create, module=module, with_chief=True, opentable_enabled=True)
    run_rig(rig)
    return {"requests": create.requests, "sent": api.sent, "titled": api.titled, "buttons": api.buttons,
            "goals": [a.goal for a in rig.agents]}


def test_switch_off_every_request_body_is_byte_for_byte_origin_main(tmp_path: Path,
                                                                   monkeypatch: pytest.MonkeyPatch) -> None:
    old = _origin("telegram", monkeypatch)
    try:
        before = _session(tmp_path / "old", old)
    finally:
        sys.modules.pop("eve_bridge._origin_telegram", None)
    after = _session(tmp_path / "new", telegram)
    assert len(after["requests"]) == 4 and after["goals"] == ["open the calculator"]
    names = {t.get("name") for t in after["requests"][0]["tools"]}
    assert "jobs_list" in names and "opentable_search" in names            # what the switch would touch is lent
    assert json.dumps(after, sort_keys=True, default=str) == json.dumps(before, sort_keys=True, default=str)
    # positive control: the switch on changes the body, and the comparison sees it
    monkeypatch.setenv("EVE_MENU_TOOLS", "on")
    on = _session(tmp_path / "on", telegram)
    assert json.dumps(on, sort_keys=True, default=str) != json.dumps(before, sort_keys=True, default=str)


def test_switch_off_jobs_list_and_opentable_are_origin_main_s_wording(monkeypatch: pytest.MonkeyPatch) -> None:
    for name, mine in (("chief", chief), ("opentable", opentable)):
        old = _origin(name, monkeypatch)
        try:
            if name == "chief":
                assert json.dumps(mine.tools(mine.LIVE)) == json.dumps(old.tools(old.LIVE))
            else:
                assert mine.tools() == old.tools() and mine.instructions() == old.instructions()
        finally:
            sys.modules.pop(f"eve_bridge._origin_{name}", None)
    assert not menu_tools.enabled()
    jobs = next(t for t in chief.tools(chief.LIVE) if t["name"] == "jobs_list")
    assert jobs["description"] == chief.JOBS_LIST_NARROW
    monkeypatch.setenv("EVE_MENU_TOOLS", "on")                         # positive control
    jobs = next(t for t in chief.tools(chief.LIVE) if t["name"] == "jobs_list")
    assert jobs["description"] == chief.JOBS_LIST_WIDE != chief.JOBS_LIST_NARROW
