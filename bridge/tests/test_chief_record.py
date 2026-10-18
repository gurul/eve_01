"""The chief's record (chief_card.py, chief_ledger.py): the strict parse, the add-only door, and the store.

Gate G1.1 of the chief design (2026-09-29): one bad phase refuses the whole card; "order the picked desk" is one_way
and "compare desks" two_way; a revision cannot lower a door; a failed os.replace leaves the old file; an unreadable
file moves to .bad-<ts>; ids are never reused; the ledger is in a temporary folder during tests (positive control:
unset, it resolves to ~/.config/eve-bridge/chief).
"""

from __future__ import annotations

import dataclasses
import json
import os
from datetime import datetime
from pathlib import Path
from typing import Any

import pytest

from eve_bridge import chief_card as cc
from eve_bridge import chief_ledger as cl
from eve_bridge.chief_card import Approval, Card, Evidence, Live, PhaseResult, Refusal

SAID = "find me a standing desk under $600 and order it"


def desk_args(**over: Any) -> dict[str, Any]:
    args: dict[str, Any] = {
        "kind": "job", "title": "Standing desk", "purpose": "a desk for the office",
        "end_state": "an order confirmation for one desk under $600",
        "done_checks": [{"kind": "read_links", "arg": 3}, {"kind": "cited_pick", "arg": None},
                        {"kind": "ui_seen", "arg": "order confirmed"}],
        "never": ["over $600"],
        "phases": [{"do": "research", "goal": "standing desks under $600 with reviews", "door": None},
                   {"do": "assess", "goal": "pick one", "door": None},
                   {"do": "act", "goal": "order the picked desk", "door": None}],
        "deadline": None, "cue": None, "firm": False, "question": None}
    args.update(over)
    return args


def reminder_args(**over: Any) -> dict[str, Any]:
    args: dict[str, Any] = {
        "kind": "commitment", "title": "BOM export", "purpose": "the board order", "end_state": "the BOM is exported",
        "done_checks": [], "never": [], "phases": [], "deadline": "2026-10-02 17:00",
        "cue": {"at": "2026-10-01 10:00", "next_action": "export the BOM"}, "firm": True, "question": None}
    args.update(over)
    return args


def card(**over: Any) -> Card:
    out = cc.parse(desk_args(**over), said=SAID, said_ref="2026-09-29 10:02")
    assert isinstance(out, Card), out
    return out


# ---- the parse ---------------------------------------------------------------------------------

def test_a_whole_desk_job_parses_with_its_doors_budget_and_backbrief() -> None:
    c = card()
    assert c.id == "" and c.rev == 1 and c.status == "active" and c.waiting_for is None
    assert [(p.n, p.do, p.door) for p in c.phases] == [(1, "research", "two_way"), (2, "assess", "two_way"),
                                                       (3, "act", "one_way")]
    assert c.phases[2].grounded is True                          # the owner said "order"
    assert c.budget == cc.Budget(cc.DEFAULT_BUDGET_ACT_USD, cc.DEFAULT_BUDGET_ACT_MIN)
    assert [x.kind for x in c.done_checks] == ["read_links", "cited_pick", "ui_seen"]
    assert c.done_checks[0].arg == "3" and c.source.said_ref == "2026-09-29 10:02"
    brief = cc.backbrief(c)
    assert brief.startswith("On it: Standing desk. I intend to research standing desks under $600 with reviews")
    assert "order the picked desk after your Go" in brief and "Up to $1 and 30 min." in brief
    assert "Done when an order confirmation for one desk under $600." in brief


def test_the_card_keeps_a_reference_to_the_words_never_the_words() -> None:
    c = card()
    assert "standing desk under $600 and order it" not in json.dumps(cc.to_json(c))


def test_one_bad_phase_refuses_the_whole_card() -> None:
    phases = desk_args()["phases"] + [{"do": "teleport", "goal": "x", "door": None}]
    out = cc.parse(desk_args(phases=phases), said=SAID)
    assert isinstance(out, Refusal) and "phase 4" in out.reason
    # control: without the bad phase the same card parses
    assert isinstance(cc.parse(desk_args(), said=SAID), Card)


@pytest.mark.parametrize("over, words", [
    ({"kind": "chore"}, "kind"),
    ({"title": "  "}, "title"),
    ({"phases": []}, "at least one phase"),
    ({"phases": [{"do": "research", "goal": "", "door": None}]}, "goal"),
    ({"phases": [{"do": "research", "goal": "desks", "door": "sideways"}]}, "door"),
    ({"phases": [{"do": "research", "goal": "desks", "door": None, "extra": 1}]}, "unknown field"),
    ({"phases": [{"do": "research", "goal": f"desks {i}", "door": None} for i in range(7)]}, "at most 6"),
    ({"done_checks": [{"kind": "tests_green", "arg": ""}]}, "kind must be one of"),
    ({"done_checks": [{"kind": "read_links", "arg": 0}]}, "count"),
    ({"done_checks": [{"kind": "read_links", "arg": "three"}]}, "count"),
    ({"done_checks": [{"kind": "ui_seen", "arg": ""}]}, "ui_seen text"),
    ({"done_checks": [{"kind": "watch_armed", "arg": None}]}, "needs a watch phase"),
    ({"done_checks": [{"kind": "file_exists", "arg": "/etc/passwd"}]}, "home folder"),
    ({"done_checks": [{"kind": "file_exists", "arg": "~/../../etc/passwd"}]}, "home folder"),
    ({"deadline": "next friday"}, "not a date"),
    ({"cue": {"at": None, "next_action": "buy"}}, "a job has no cue"),
    ({"firm": "yes"}, "firm"),
    ({"surprise": True}, "unknown field"),
])
def test_the_parse_is_strict(over: dict[str, Any], words: str) -> None:
    out = cc.parse(desk_args(**over), said=SAID)
    assert isinstance(out, Refusal), out
    assert words in out.reason


def test_a_card_is_made_only_in_a_turn_with_the_owners_words() -> None:
    assert isinstance(cc.parse(desk_args(), said=""), Refusal)
    assert isinstance(cc.parse(desk_args(), said="   "), Refusal)


def test_a_commitment_has_no_phases_a_cue_and_only_the_owners_checks() -> None:
    c = cc.parse(reminder_args(), said="remind me Thursday to export the BOM, it's due Friday")
    assert isinstance(c, Card) and c.kind == "commitment" and c.phases == () and c.firm
    assert [x.kind for x in c.done_checks] == ["guru_says_done"]             # the default check
    assert c.cue == cc.Cue("2026-10-01 10:00", "export the BOM") and c.deadline == "2026-10-02 17:00"
    assert cc.backbrief(c) == ("Holding it: BOM export, due 2026-10-02 17:00. I'll remind you at your first break "
                               "after 2026-10-01 10:00, 10:45 at the latest.")
    assert isinstance(cc.parse(reminder_args(phases=[{"do": "research", "goal": "x", "door": None}]), said="x"), Refusal)
    assert isinstance(cc.parse(reminder_args(cue=None), said="x"), Refusal)
    assert isinstance(cc.parse(reminder_args(done_checks=[{"kind": "ui_seen", "arg": "x"}]), said="x"), Refusal)


def test_a_missing_end_state_or_a_question_makes_the_card_wait() -> None:
    c = card(end_state="")
    assert c.status == "waiting" and c.waiting_for == "question" and cc.missing(c) == ["end_state"]
    q = card(question="Which room is it for?")
    assert q.status == "waiting" and cc.missing(q) == ["question"] and cc.backbrief(q).endswith("Which room is it for?")


def test_the_live_set_is_the_whole_offer() -> None:
    narrow = Live(phases=("research", "assess"), checks=("read_links", "cited_pick", "guru_says_done"))
    out = cc.parse(desk_args(), said=SAID, live=narrow)
    assert isinstance(out, Refusal) and "phase 3" in out.reason
    ok = cc.parse(desk_args(phases=desk_args()["phases"][:2], done_checks=desk_args()["done_checks"][:2]),
                  said=SAID, live=narrow)
    assert isinstance(ok, Card)
    # only live names in the sets the code offers
    assert cc.PHASE_KINDS_LIVE == ("research", "assess", "act", "watch")
    assert cc.DONE_KINDS_LIVE == ("read_links", "cited_pick", "ui_seen", "watch_armed", "file_exists",
                                  "guru_says_done")


def test_file_exists_takes_a_home_path(tmp_path: Path) -> None:
    ok = cc.parse(desk_args(done_checks=[{"kind": "file_exists", "arg": "~/Desktop/bom.csv"}]), said=SAID,
                  home=tmp_path)
    assert isinstance(ok, Card)
    assert cc.home_path("~/Desktop/bom.csv", tmp_path) == (tmp_path / "Desktop" / "bom.csv").resolve()
    assert cc.home_path("/tmp/x", tmp_path) is None and cc.home_path("~/../x", tmp_path) is None
    outside = tmp_path.parent / "elsewhere"
    outside.mkdir(exist_ok=True)
    (tmp_path / "link").symlink_to(outside)
    assert cc.home_path("~/link/x", tmp_path) is None                         # a symlink out is outside


# ---- the door ----------------------------------------------------------------------------------

def test_order_the_picked_desk_is_one_way_and_compare_desks_is_two_way() -> None:
    assert cc.door("order the picked desk") == "one_way"
    assert cc.door("compare desks") == "two_way"
    assert cc.door("compare desks", slugs=["GMAIL_SEND_EMAIL"]) == "one_way"      # a writing Composio call
    assert cc.door("compare desks", can_ask=False) == "one_way"                   # an executor that cannot ask
    assert cc.door("compare desks", raised=True) == "one_way"                     # the brain may raise


def test_the_brain_can_raise_a_door_but_never_lower_one() -> None:
    lowered = card(phases=[{"do": "act", "goal": "order the picked desk", "door": "two_way"}],
                   done_checks=[{"kind": "ui_seen", "arg": "order confirmed"}])
    assert lowered.phases[0].door == "one_way"
    raised = card(phases=[{"do": "act", "goal": "open the desk page", "door": "one_way"}],
                  done_checks=[{"kind": "ui_seen", "arg": "desk page"}])
    assert raised.phases[0].door == "one_way" and raised.phases[0].grounded is False   # nothing the owner said


def test_grounded_needs_the_consequential_word_in_the_owners_words() -> None:
    assert cc.grounded("order the picked desk", SAID) is True
    assert cc.grounded("order the picked desk", "find me a standing desk under $600") is False
    assert cc.grounded("compare desks", SAID) is False                            # no verb names an act
    c = cc.parse(desk_args(), said="find me a standing desk under $600")
    assert isinstance(c, Card) and c.phases[2].door == "one_way" and c.phases[2].grounded is False


def test_the_door_is_add_only_and_a_revision_cannot_lower_it() -> None:
    c = dataclasses.replace(card(), id="c1")
    # the owner rewords the act so that no consequential word is left: the door stays one_way
    patch = {"phases": [{"do": "research", "goal": "standing desks under $600 with reviews", "door": None},
                        {"do": "assess", "goal": "pick one", "door": None},
                        {"do": "act", "goal": "get the picked desk", "door": "two_way"}]}
    new = cc.revise(c, patch, said="change c1: get the picked desk")
    assert isinstance(new, Card) and new.rev == 2
    assert new.phases[2].door == "one_way"
    # a phase moved into a one-way place inherits the door
    moved = cc.revise(c, {"phases": [{"do": "research", "goal": "desks", "door": None},
                                     {"do": "assess", "goal": "pick one", "door": None},
                                     {"do": "research", "goal": "more desks", "door": None}],
                          "done_checks": [{"kind": "read_links", "arg": 3}]}, said="change c1")
    assert isinstance(moved, Card) and moved.phases[2].door == "one_way"
    # control: a phase that was never one-way is still two-way after the same revision
    assert new.phases[0].door == "two_way" and moved.phases[0].door == "two_way"


def test_a_revision_cancels_approvals_keeps_state_and_never_changes_the_kind() -> None:
    c = dataclasses.replace(card(), id="c1")
    ran = dataclasses.replace(c.phases[0], status="ok", executor="search_router",
                              result=PhaseResult("done", (Evidence("links", "https://a.example/1"),)))
    approved = dataclasses.replace(c.phases[2], approval=Approval("tok", 1, 5.0))
    c = dataclasses.replace(c, phases=(ran, c.phases[1], approved), status="waiting", waiting_for="go")
    new = cc.revise(c, {"never": ["over $550"]}, said="change c1 never over $550", now=9.0)
    assert isinstance(new, Card) and new.rev == 2 and new.never == ("over $550",)
    assert new.phases[0].status == "ok" and new.phases[0].result == ran.result
    assert new.phases[2].approval is None                                 # the yes was for rev 1
    assert new.status == "waiting" and new.waiting_for == "go"
    assert isinstance(cc.revise(c, {"kind": "commitment"}, said="x"), Refusal)
    assert isinstance(cc.revise(dataclasses.replace(c, status="done"), {"never": []}, said="x"), Refusal)
    answered = cc.revise(dataclasses.replace(card(question="Which room?"), id="c2"), {"question": None}, said="office")
    assert isinstance(answered, Card) and answered.status == "active" and answered.waiting_for is None


def test_a_phase_inserted_at_the_front_keeps_every_phase_that_ran() -> None:
    """Phases used to be matched by number: one inserted at the front put each later phase "in a new place",
    and the research that had run went back to queued with its result gone (an act that ran could run again)."""
    c = dataclasses.replace(card(), id="c1")
    ran = dataclasses.replace(c.phases[0], status="ok", executor="search_router",
                              result=PhaseResult("done", (Evidence("links", "https://a.example/1"),)))
    acted = dataclasses.replace(c.phases[2], status="ok", executor="browser",
                                result=PhaseResult("done", (Evidence("ui_seen", "order confirmed"),)))
    c = dataclasses.replace(c, phases=(ran, c.phases[1], acted))
    front = {"do": "research", "goal": "measure the office wall", "door": None}
    patch = {"phases": [front] + [{"do": p.do, "goal": p.goal, "door": None} for p in c.phases]}
    new = cc.revise(c, patch, said="change c1: measure the wall first, then order the picked desk")
    assert isinstance(new, Card)
    assert [p.n for p in new.phases] == [1, 2, 3, 4]
    assert new.phases[0].goal == front["goal"] and new.phases[0].status == "queued" and new.phases[0].result is None
    assert (new.phases[1].goal, new.phases[1].status, new.phases[1].result) == (ran.goal, "ok", ran.result)
    assert new.phases[2].status == "queued"                                   # control: it never ran
    assert (new.phases[3].goal, new.phases[3].status, new.phases[3].result) == (acted.goal, "ok", acted.result)
    assert new.phases[3].door == "one_way" and new.phases[3].executor == "browser"
    # a phase deleted before them: the later ones keep their state and take the new numbers
    gone = cc.revise(c, {"phases": [{"do": p.do, "goal": p.goal, "door": None} for p in (c.phases[0], c.phases[2])],
                         "done_checks": [{"kind": "read_links", "arg": 3}, {"kind": "ui_seen", "arg": "order confirmed"}]},
                     said="change c1: skip the pick, order the picked desk")
    assert isinstance(gone, Card) and [(p.n, p.status) for p in gone.phases] == [(1, "ok"), (2, "ok")]
    assert gone.phases[1].result == acted.result
    # a running phase's result comes back by its number: a revision never moves it while it runs
    running = dataclasses.replace(c, phases=(ran, dataclasses.replace(c.phases[1], status="running"), acted))
    assert isinstance(cc.revise(running, patch, said="change c1: measure the wall first"), Refusal)
    assert isinstance(cc.revise(running, {"never": ["over $550"]}, said="change c1 never over $550"), Card)


def test_a_stored_card_round_trips_and_a_bad_one_is_refused() -> None:
    c = dataclasses.replace(card(), id="c7")
    assert cc.from_json(cc.to_json(c)) == c
    raw = cc.to_json(c)
    for key, bad in (("id", "7"), ("status", "finished"), ("rev", 0), ("done_checks", [])):
        with pytest.raises(cc.CardError):
            cc.from_json({**raw, key: bad})
    raw["phases"][0]["result"] = {"status": "done", "evidence": [{"kind": "rumour", "ref": "x"}]}
    with pytest.raises(cc.CardError):
        cc.from_json(raw)


# ---- the ledger --------------------------------------------------------------------------------

class Clock:
    def __init__(self, t: float = 1_790_000_000.0) -> None:
        self.t = t

    def __call__(self) -> float:
        return self.t


def test_the_ledger_is_in_the_temp_folder_during_tests(_chief_ledger_in_tmp: Path) -> None:
    assert cl.chief_dir() == _chief_ledger_in_tmp
    assert cl.Ledger().dir == _chief_ledger_in_tmp
    assert str(Path.home() / ".config") not in str(cl.chief_dir())


def test_positive_control_unset_the_ledger_resolves_to_the_config_folder(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("EVE_CHIEF_DIR", raising=False)
    assert cl.chief_dir() == Path.home() / ".config" / "eve-bridge" / "chief"
    assert cl.chief_dir({}) == Path.home() / ".config" / "eve-bridge" / "chief"
    assert cl.chief_dir({"EVE_CHIEF_DIR": "/x/y"}) == Path("/x/y")


def test_new_files_the_card_under_a_fresh_id_and_logs_created(tmp_path: Path) -> None:
    led = cl.Ledger(tmp_path, clock=Clock())
    a = led.new(card())
    b = led.new(card(title="Monitor arm"))
    assert (a.id, b.id) == ("c1", "c2") and a.created == 1_790_000_000.0
    assert oct(os.stat(led.cards_path).st_mode & 0o777) == "0o600"
    assert oct(os.stat(led.events_path).st_mode & 0o777) == "0o600"
    again = cl.Ledger(tmp_path)
    assert again.get("c1") == a and again.get("c2") == b
    created = again.events("c1")
    assert created[0]["e"] == "created" and created[0]["doors"] == ["two_way", "two_way", "one_way"]
    with pytest.raises(ValueError):
        led.new(a)                                                  # already filed


def test_ids_are_never_reused(tmp_path: Path) -> None:
    led = cl.Ledger(tmp_path)
    for _ in range(3):
        led.new(card())
    assert led.redact(["c3"]) == 1
    assert cl.Ledger(tmp_path).new(card()).id == "c4"               # c3 is gone, and never comes back
    # the cards file lost entirely: the events still name c1..c4, so the next id is c5
    led.cards_path.unlink()
    assert cl.Ledger(tmp_path).new(card()).id == "c5"


def test_a_failed_replace_leaves_the_old_file_and_memory_as_they_were(tmp_path: Path,
                                                                       monkeypatch: pytest.MonkeyPatch) -> None:
    led = cl.Ledger(tmp_path)
    led.new(card())
    before = led.cards_path.read_bytes()

    def boom(src: Any, dst: Any) -> None:
        raise OSError("disk full")

    monkeypatch.setattr(cl.os, "replace", boom)
    with pytest.raises(cl.LedgerError):
        led.new(card(title="Second"))
    with pytest.raises(cl.LedgerError):
        led.update("c1", why="test", status="dropped")
    assert led.cards_path.read_bytes() == before
    assert led.get("c2") is None and led.get("c1").status == "active"
    assert not [p for p in tmp_path.iterdir() if p.name.startswith(".cards-")]   # no temp file left
    monkeypatch.undo()
    assert led.new(card(title="Second")).id == "c2"


def test_an_unreadable_file_moves_to_bad_and_is_reported_once(tmp_path: Path) -> None:
    led = cl.Ledger(tmp_path, clock=Clock())
    led.new(card())
    led.new(card())
    led.cards_path.write_text('{"version": 1, "last_id": 2, "cards": [{"id": "c2", oops', encoding="utf-8")
    led.events_path.unlink()
    later = 1_790_000_500
    again = cl.Ledger(tmp_path, clock=Clock(float(later)))
    aside = tmp_path / f"cards.json.bad-{later}"
    assert aside.exists() and not again.cards_path.exists()
    assert again.all() == []
    problem = again.take_problem()
    assert f"cards.json.bad-{later}" in problem and again.take_problem() == ""
    assert again.new(card()).id == "c3"                              # ids go on past the bad file's highest


def test_one_card_that_does_not_load_is_skipped_not_the_whole_list(tmp_path: Path) -> None:
    led = cl.Ledger(tmp_path)
    led.new(card())
    led.new(card())
    raw = json.loads(led.cards_path.read_text())
    raw["cards"][0]["status"] = "finished"
    led.cards_path.write_text(json.dumps(raw))
    again = cl.Ledger(tmp_path)
    assert [c.id for c in again.all()] == ["c2"]
    assert "(c1)" in again.take_problem() and again.take_problem() == ""       # said once (it was "" before 2026-09-29)


def test_state_changes_do_not_bump_rev_and_a_revision_does(tmp_path: Path) -> None:
    led = cl.Ledger(tmp_path)
    c = led.new(card())
    led.update_phase(c.id, 3, why="go asked", approval=Approval("tok", 1, 1.0))
    s = led.update(c.id, why="go asked", status="waiting", waiting_for="go")
    assert s.rev == 1 and s.phase(3).approval == Approval("tok", 1, 1.0)
    r = led.revise(c.id, {"never": ["over $500"]}, "owner changed the cap", said="change c1 never over $500")
    assert isinstance(r, Card) and r.rev == 2 and r.phase(3).approval is None
    kinds = [e["e"] for e in led.events(c.id)]
    assert kinds == ["created", "state", "state", "revised"]
    with pytest.raises(ValueError):
        led.update(c.id, why="x", title="new title")                 # not a state field
    with pytest.raises(ValueError):
        led.update(c.id, why="x", status="finished")
    with pytest.raises(ValueError):
        led.update_phase(c.id, 1, why="x", door="two_way")            # a door is never a state change
    led.update(c.id, why="done", status="done")
    with pytest.raises(ValueError):
        led.update(c.id, why="x", status="active")                    # a finished card is reopened, not moved
    assert led.reopen(c.id, why="owner reopened").status == "active" and led.get(c.id).rev == 3
    assert isinstance(led.revise("c99", {}, "x", said="x"), Refusal)


def test_an_event_that_cannot_be_written_says_so(tmp_path: Path) -> None:
    led = cl.Ledger(tmp_path)
    assert led.event("dispatched", "c1", phase=3, executor="codex") is True
    led.events_path.unlink()
    led.events_path.mkdir()                                          # the path is a folder now: the write fails
    assert led.event("dispatched", "c1", phase=3) is False
    with pytest.raises(ValueError):
        led.event("teleported", "c1")


def test_events_carry_scalars_capped(tmp_path: Path) -> None:
    led = cl.Ledger(tmp_path)
    led.event("result", "c1", why="x" * 500, nested={"a": 1}, doors=["one_way"])
    row = led.events("c1")[0]
    assert len(row["why"]) == cl.MAX_EVENT_TEXT and isinstance(row["nested"], str) and row["doors"] == ["one_way"]


def test_due_finds_cues_and_nudges_that_have_come(tmp_path: Path) -> None:
    led = cl.Ledger(tmp_path)
    c = led.new(cc.parse(reminder_args(), said="remind me"))
    cue = datetime(2026, 10, 1, 10, 0).timestamp()
    assert led.due(cue - 60) == [] and [x.id for x in led.due(cue + 60)] == [c.id]
    led.update(c.id, why="nudged", nudges=cc.Nudges(sent=1, next_at=cue + 3600))
    assert led.due(cue + 60) == [] and [x.id for x in led.due(cue + 3601)] == [c.id]
    led.update(c.id, why="done", status="done")
    assert led.due(cue + 99999) == []


def test_for_turn_is_one_short_line(tmp_path: Path) -> None:
    led = cl.Ledger(tmp_path)
    assert led.for_turn(0) == ""
    a = led.new(card())
    led.update(a.id, why="go", status="waiting", waiting_for="go")
    led.new(card(title="Monitor arm"))
    r = led.new(cc.parse(reminder_args(), said="remind me"))
    now = datetime(2026, 10, 1, 9, 0).timestamp()
    line = led.for_turn(now)
    assert line == ("Open jobs: 2 (c1 waiting on your Go: order the picked desk; c2 researching: Monitor arm). "
                    f"Due today: BOM export ({r.id}).")
    for i in range(20):
        led.new(card(title=f"Job number {i}"))
    assert len(led.for_turn(now)) <= cl.TURN_LINE_CHARS


def test_matching_finds_the_card_the_owners_words_are_about(tmp_path: Path) -> None:
    led = cl.Ledger(tmp_path)
    r = led.new(cc.parse(reminder_args(), said="remind me"))
    led.new(card())
    assert [c.id for c in led.matching("exporting now: the BOM export for the board")] == [r.id]
    assert led.matching("what's the weather") == []


def test_redact_removes_the_cards_and_their_events(tmp_path: Path) -> None:
    led = cl.Ledger(tmp_path)
    a = led.new(card())
    b = led.new(card(title="Monitor arm"))
    assert led.redact([a.id]) == 1
    assert led.get(a.id) is None and led.get(b.id) is not None
    assert all(e.get("id") != a.id for e in led.events())
    assert [e["e"] for e in led.events(b.id)] == ["created"]


# ---- the reviewer's attacks after P3 (2026-09-29): each was reproduced first, then fixed ----------------------

@pytest.mark.parametrize("field, value", [(("budget", "wall_min"), "1e999"), (("spent", "usd"), "1e999"),
                                          (("nudges", "sent"), "-1e999"), (("created",), "1e999")])
def test_attack_a_number_too_big_sets_the_card_aside_instead_of_crashing(tmp_path: Path, field: tuple[str, ...],
                                                                         value: str) -> None:
    led = cl.Ledger(tmp_path)
    led.new(card())
    led.new(card())
    raw = json.loads(led.cards_path.read_text())
    target = raw["cards"][0]
    for key in field[:-1]:
        target = target[key]
    target[field[-1]] = "SENTINEL"
    led.cards_path.write_text(json.dumps(raw).replace('"SENTINEL"', value))
    again = cl.Ledger(tmp_path)
    assert [c.id for c in again.all()] == ["c2"]
    assert "c1" in again.take_problem()


def test_attack_a_card_that_does_not_load_is_kept_aside_and_reported(tmp_path: Path) -> None:
    led = cl.Ledger(tmp_path, clock=Clock())
    led.new(card())
    raw = json.loads(led.cards_path.read_text())
    raw["cards"][0]["status"] = "Active"                  # a hand-edit's typo
    led.cards_path.write_text(json.dumps(raw))
    again = cl.Ledger(tmp_path, clock=Clock())
    problem = again.take_problem()
    assert "1 job" in problem and "cards.json.bad-" in problem
    again.new(card())                                     # the next save writes only what loaded ...
    ids = [x["id"] for x in json.loads(again.cards_path.read_text())["cards"]]
    assert ids == ["c2"]
    (aside,) = list(tmp_path.glob("cards.json.bad-*"))    # ... and the skipped card is still on disk
    assert '"Active"' in aside.read_text()


@pytest.mark.parametrize("text", ['[{"id": "c1"}]', '"cards"', '{"version": 1, "last_id": 3, "cards": {"c1": 1}}'])
def test_attack_a_file_of_the_wrong_shape_is_moved_aside_not_overwritten(tmp_path: Path, text: str) -> None:
    (tmp_path / "cards.json").write_text(text)
    led = cl.Ledger(tmp_path, clock=Clock())
    assert led.all() == [] and "couldn't read my list" in led.take_problem()
    (aside,) = list(tmp_path.glob("cards.json.bad-*"))
    assert aside.read_text() == text
    led.new(card())
    assert aside.read_text() == text


# ---- the reviewer's attacks after P4 (2026-09-29): each was reproduced first, then fixed ----------------------

def test_routine_acts_use_the_executors_live_confirmation_boundary() -> None:
    """Codex can stop for a consequential action; merely using the Mac no longer needs a Go."""
    routine = card(phases=[{"do": "act", "goal": "organize the desk notes", "door": None}],
                   done_checks=[{"kind": "guru_says_done", "arg": None}])
    assert routine.phases[0].door == "two_way"
    dangerous = card(phases=[{"do": "act", "goal": "delete the desk notes", "door": None}],
                     done_checks=[{"kind": "guru_says_done", "arg": None}])
    assert dangerous.phases[0].door == "one_way"
    raised = card(phases=[{"do": "act", "goal": "open the desk page", "door": "one_way"}],
                  done_checks=[{"kind": "ui_seen", "arg": "desk page"}])
    assert raised.phases[0].door == "one_way" and raised.phases[0].grounded is False


def test_a_stored_routine_two_way_act_keeps_its_door() -> None:
    """Loading preserves routine two-way work and never lowers a stored one-way door."""
    c = dataclasses.replace(card(phases=[{"do": "research", "goal": "desks", "door": None},
                                         {"do": "act", "goal": "open the desk page", "door": None}],
                                 done_checks=[{"kind": "guru_says_done", "arg": None}]), id="c1")
    raw = cc.to_json(c)
    raw["phases"][1]["door"] = "two_way"
    loaded = cc.from_json(json.loads(json.dumps(raw)))
    assert [p.door for p in loaded.phases] == ["two_way", "two_way"]
    raw["phases"][1]["door"] = "one_way"
    assert cc.from_json(raw).phases[1].door == "one_way"


def _counting_loads(monkeypatch: pytest.MonkeyPatch) -> list[int]:
    calls = [0]
    real = json.loads

    def counted(*a: Any, **k: Any) -> Any:
        calls[0] += 1
        return real(*a, **k)
    monkeypatch.setattr(json, "loads", counted)
    return calls


def test_events_parse_only_the_lines_appended_since_the_last_read(tmp_path: Path,
                                                                  monkeypatch: pytest.MonkeyPatch) -> None:
    """Review D6: events() re-read and parsed the whole append-only file on every call, many times a tick."""
    led = cl.Ledger(tmp_path)
    a = led.new(card())
    led.event("checked", a.id, note="one")
    first = led.events()
    calls = _counting_loads(monkeypatch)
    assert led.events() == first and calls[0] == 0              # nothing new: nothing parsed
    led.event("checked", a.id, note="two")
    now = led.events()
    assert now[:-1] == first and now[-1]["note"] == "two" and calls[0] == 1
    other = cl.Ledger(tmp_path)                                 # another writer appends: still seen
    other.event("checked", a.id, note="three")
    assert [e.get("note") for e in led.events(a.id)][-3:] == ["one", "two", "three"]
    with (tmp_path / cl.EVENTS_FILE).open("a") as f:            # a torn tail is skipped, then read once whole
        f.write('{"t":1,"e":"checked","id":"' + a.id + '","note":"fo')
    assert [e.get("note") for e in led.events(a.id)][-1] == "three"
    with (tmp_path / cl.EVENTS_FILE).open("a") as f:
        f.write('ur"}\n')
    assert [e.get("note") for e in led.events(a.id)][-1] == "four"
    b = led.new(card(title="Monitor arm"))
    assert led.redact([a.id]) == 1                              # a rewrite replaces the file: read afresh
    assert all(e.get("id") != a.id for e in led.events())
    assert [e["e"] for e in led.events(b.id)] == ["created"]
