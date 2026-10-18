"""The chief's desk (chief_desk.py): the attention gate, backoff, expiry, and the briefs composed in code.

Gate G2.2 of the chief design (2026-09-29): at 23:00 an item is batched and at 10:00 it goes now; the third push of
a day is batched; backoff doubles; the owner's own action cancels queued nudges; an expired decision takes the
option that does not act; the push count survives a reload; with no breakpoint in 45 minutes the item moves to
the next slot; any error falls back to batch.

Gate G2.3 (``-k docs``): docs/stackchan/chief.md names every EVE_CHIEF* setting the code reads, and its
documented defaults equal the constants (the test_fastlane_docs pattern).
"""

from __future__ import annotations

import dataclasses
import re
from datetime import datetime
from pathlib import Path
from typing import Any

import pytest

from eve_bridge import chief, chief_replan
from eve_bridge import chief_card as cc
from eve_bridge import chief_desk as cd
from eve_bridge import chief_ledger as cl
from eve_bridge.chief_card import Nudges
from eve_bridge.chief_desk import Choice, Desk, Item, Mandate, Route

ROOT = Path(__file__).resolve().parents[2]
CHIEF_DOC = ROOT / "docs" / "stackchan" / "chief.md"
SRC = ROOT / "bridge" / "src" / "eve_bridge"


def at(hh: int, mm: int = 0, day: int = 29) -> float:
    return datetime(2026, 9, day, hh, mm).timestamp()


def desk(tmp_path: Path, mode: str = "on", **kw: Any) -> Desk:
    return Desk(tmp_path, mode=mode, **kw)


def urgent(t: float, card_id: str = "c1", **kw: Any) -> Item:
    """A firm commitment due within the hour: the one unprompted nudge the desk may send."""
    return Item("nudge", card_id, queued_at=t, deadline_at=t + 3600, firm=True, **kw)


# ---- quiet hours -------------------------------------------------------------------------------

def test_at_2300_an_item_is_batched_and_at_1000_it_goes_now(tmp_path: Path) -> None:
    d = desk(tmp_path)
    late, morning = at(23), at(10)
    d.breakpoint("task_end", late)
    r = d.offer(urgent(late), late)
    assert (r.route, r.rule) == ("batch", "quiet_hours") and r.at == at(8, 0, day=30)
    d.breakpoint("task_end", morning)
    assert d.offer(urgent(morning), morning) == Route("now", "urgent_firm")
    # a reminder the owner asked for and a Go wait out quiet hours too; a reply and a receipt do not
    assert d.offer(Item("reminder", "c2", queued_at=late), late).rule == "quiet_hours"
    assert d.offer(Item("go", "c3", queued_at=late), late).rule == "quiet_hours"
    assert d.offer(Item("reply"), late) == Route("now", "reply")
    assert d.offer(Item("receipt", "c3"), late) == Route("now", "receipt")


def test_quiet_hours_cross_midnight_and_a_daytime_window_works_too() -> None:
    m = Mandate()
    assert cd.in_quiet(m, at(22, 30)) and cd.in_quiet(m, at(3)) and cd.in_quiet(m, at(7, 59))
    assert not cd.in_quiet(m, at(8)) and not cd.in_quiet(m, at(22, 29))
    day = Mandate(quiet_start="13:00", quiet_end="14:00")
    assert cd.in_quiet(day, at(13, 30)) and not cd.in_quiet(day, at(14)) and not cd.in_quiet(day, at(23))
    assert cd.quiet_ends(m, at(3)) == at(8) and cd.quiet_ends(m, at(10)) == at(10)


# ---- the daily budget ------------------------------------------------------------------------

def test_the_third_push_of_a_day_is_batched(tmp_path: Path) -> None:
    d = desk(tmp_path)
    routes = []
    for i, hh in enumerate((10, 12, 14)):
        t = at(hh)
        d.breakpoint("relay_end", t)
        routes.append(d.offer(urgent(t, f"c{i + 1}"), t))
    assert [r.route for r in routes] == ["now", "now", "batch"] and routes[2].rule == "push_budget"
    # reminders the owner asked for do not count and are not counted
    t = at(15)
    d.breakpoint("task_end", t)
    assert d.offer(Item("reminder", "c9", queued_at=t), t) == Route("now", "asked_reminder")
    assert d.pushes_today(t) == 2
    # a new day, a new budget
    t = at(10, day=30)
    d.breakpoint("task_end", t)
    assert d.offer(urgent(t, "c4"), t).route == "now"


def test_in_shadow_the_would_pushes_are_silent_and_counted(tmp_path: Path) -> None:
    d = desk(tmp_path, mode="shadow")
    out = []
    for hh in (10, 12, 14):
        t = at(hh)
        d.breakpoint("task_end", t)
        out.append(d.offer(urgent(t), t))
    assert [(r.route, r.rule) for r in out] == [("silent", "urgent_firm"), ("silent", "urgent_firm"),
                                                ("batch", "push_budget")]
    off = desk(tmp_path / "off", mode="off")
    off.breakpoint("task_end", at(10))
    assert off.offer(urgent(at(10)), at(10)) == Route("batch", "push_off")


def test_the_push_count_survives_a_reload(tmp_path: Path) -> None:
    d = desk(tmp_path)
    for hh in (10, 11):
        d.breakpoint("task_end", at(hh))
        assert d.offer(urgent(at(hh)), at(hh)).route == "now"
    again = desk(tmp_path)
    assert again.pushes_today(at(12)) == 2
    again.breakpoint("task_end", at(12))
    assert again.offer(urgent(at(12)), at(12)) == Route("batch", "push_budget")


# ---- breakpoints -------------------------------------------------------------------------------

def test_unprompted_items_wait_for_a_breakpoint(tmp_path: Path) -> None:
    d = desk(tmp_path)
    t = at(10)
    assert d.offer(urgent(t), t) == Route("batch", "await_breakpoint")
    d.owner_said(t)
    assert not d.at_breakpoint(t + 30)
    assert d.at_breakpoint(t + cd.IDLE_SECS)                                # 90 s after the owner's last word
    assert not d.at_breakpoint(t + 3 * 3600)                                # hours later is not a moment
    d.breakpoint("wake_end", t + 4000)
    assert d.at_breakpoint(t + 4000 + 60) and not d.at_breakpoint(t + 4000 + cd.BREAKPOINT_WINDOW_SECS + 1)
    with pytest.raises(ValueError):
        d.breakpoint("lunch", t)


def test_with_no_breakpoint_in_45_minutes_the_reminder_is_sent_anyway(tmp_path: Path) -> None:
    # owner, 2026-10-06: "just send it at 45 min" (it went to the next morning's brief)
    d = desk(tmp_path)
    due = at(10)
    waiting = d.offer(Item("reminder", "c1", queued_at=due), due + 60)
    assert (waiting.route, waiting.rule) == ("batch", "await_breakpoint") and waiting.at == due + cd.REMINDER_WAIT_SECS
    late = d.offer(Item("reminder", "c1", queued_at=due), due + cd.REMINDER_WAIT_SECS)
    assert late == Route("now", "reminder_waited", None)
    # quiet hours still hold it, however long it waited
    night = at(23)
    assert d.offer(Item("reminder", "c1", queued_at=night), night + cd.REMINDER_WAIT_SECS).rule == "quiet_hours"
    # control: a breakpoint inside the 45 minutes sends it
    d.breakpoint("task_end", due + 600)
    assert d.offer(Item("reminder", "c1", queued_at=due), due + 610) == Route("now", "asked_reminder")


# ---- backoff, the owner's action, expiry ---------------------------------------------------------

def test_backoff_doubles_and_three_ignored_go_to_the_sheet(tmp_path: Path) -> None:
    t = at(10)
    n = Nudges()
    gaps = []
    for _ in range(3):
        n = Desk.ignored(n, t)
        gaps.append(n.next_at - t)
    assert gaps == [cd.NUDGE_GAP_SECS, 2 * cd.NUDGE_GAP_SECS, 4 * cd.NUDGE_GAP_SECS]
    assert n.ignored == 3 and n.level == "sheet"
    assert Desk.sent(Nudges(ignored=1), t).next_at == t + 2 * cd.NUDGE_GAP_SECS
    d = desk(tmp_path)
    d.breakpoint("task_end", t)
    assert d.offer(urgent(t, nudges=Nudges(ignored=1, next_at=t + 60)), t) == Route("batch", "backoff", t + 60)
    assert d.offer(urgent(t, nudges=n), t) == Route("batch", "sheet_only")


def test_the_owners_own_action_cancels_queued_nudges(tmp_path: Path) -> None:
    d = desk(tmp_path)
    t = at(10)
    d.breakpoint("task_end", t + 100)
    d.acted("c1", t + 50)
    assert d.offer(urgent(t, "c1"), t + 100) == Route("drop", "owner_acted")
    assert d.offer(Item("reminder", "c1", queued_at=t), t + 100) == Route("drop", "owner_acted")
    assert d.offer(urgent(t, "c2"), t + 100).route == "now"                 # control: another card goes
    assert d.offer(urgent(t + 60, "c1"), t + 100).route == "now"            # a nudge queued after the act goes
    assert desk(tmp_path).offer(urgent(t, "c1"), t + 100).rule == "owner_acted"    # survives a reload


def test_only_a_firm_commitment_due_soon_may_nudge(tmp_path: Path) -> None:
    d = desk(tmp_path)
    t = at(10)
    d.breakpoint("task_end", t)
    assert d.offer(Item("nudge", "c1", queued_at=t, deadline_at=t + 3600, firm=False), t).rule == "not_urgent"
    assert d.offer(Item("nudge", "c1", queued_at=t, deadline_at=t + 5 * 3600, firm=True), t).rule == "not_urgent"
    assert d.offer(Item("nudge", "c1", queued_at=t, firm=True), t).rule == "not_urgent"
    assert d.offer(Item("go", "c1", queued_at=t, card_open=False), t) == Route("drop", "card_closed")


def test_an_expired_decision_takes_the_option_that_does_not_act() -> None:
    go = [Choice("Yes, order it", acts=True), Choice("No", acts=False)]
    assert cd.on_expiry(go) == Choice("No", acts=False)
    assert cd.on_expiry(list(reversed(go))) == Choice("No", acts=False)
    assert cd.on_expiry([Choice("Order A", True), Choice("Order B", True)]) is None     # nothing, never an act


# ---- errors hold the message --------------------------------------------------------------------

def test_any_error_falls_back_to_batch(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    d = desk(tmp_path)
    t = at(10)
    d.breakpoint("task_end", t)
    assert d.offer(Item("telegram_blast", "c1"), t) == Route("batch", "error")          # an unknown kind

    def boom(*a: Any, **k: Any) -> bool:
        raise RuntimeError("clock broke")

    monkeypatch.setattr(d, "at_breakpoint", boom)
    assert d.offer(urgent(t), t) == Route("batch", "error")
    monkeypatch.undo()
    # a push that cannot be logged is not sent: the log is a folder now
    d.path.unlink()
    d.path.mkdir()
    assert d.offer(urgent(t), t) == Route("batch", "error")
    assert d.offer(Item("reply"), t) == Route("now", "reply")                # a reply is not a push


def test_a_mandate_that_does_not_parse_is_strict_and_said_once(tmp_path: Path) -> None:
    (tmp_path / cd.MANDATE_FILE).write_text('quiet_start = "21:00"\npushes_per_day = 1\n', encoding="utf-8")
    ok = Desk(tmp_path, mode="on")
    assert ok.mandate == Mandate(quiet_start="21:00", pushes_per_day=1) and ok.take_problem() == ""
    for bad in ('pushes_per_day = 99\n', 'quiet_start = "late"\n', 'volume = 3\n', 'quiet_start = \n',
                'quiet_start = "08:00"\nquiet_end = "08:00"\n'):
        (tmp_path / cd.MANDATE_FILE).write_text(bad, encoding="utf-8")
        d = Desk(tmp_path, mode="on")
        assert d.mandate == cd.STRICT and d.mandate.pushes_per_day == 0, bad
        assert "unprompted" in d.take_problem() and d.take_problem() == ""
    t = at(10)
    d.breakpoint("task_end", t)
    assert d.offer(urgent(t), t) == Route("batch", "push_budget")
    (tmp_path / cd.MANDATE_FILE).unlink()
    assert Desk(tmp_path, mode="on").mandate == Mandate()


def test_push_mode_reads_the_switch() -> None:
    assert cd.push_mode({}) == "shadow"
    assert cd.push_mode({"EVE_CHIEF_PUSH": "on"}) == "on"
    assert cd.push_mode({"EVE_CHIEF_PUSH": "1"}) == "on"
    assert cd.push_mode({"EVE_CHIEF_PUSH": "false"}) == "off"
    assert cd.push_mode({"EVE_CHIEF_PUSH": "loud"}) == "shadow"


def test_attention_lines_carry_ids_and_rules_never_text(tmp_path: Path) -> None:
    d = desk(tmp_path)
    d.offer(Item("reply", "c1"), at(10))
    line = d.path.read_text().splitlines()[0]
    assert set(__import__("json").loads(line)) == {"t", "kind", "id", "route", "rule", "mode", "unprompted"}


# ---- the briefs, composed in code -----------------------------------------------------------------

def _job(title: str, **over: Any) -> cc.Card:
    args = {"kind": "job", "title": title, "purpose": "", "end_state": "a desk is ordered",
            "done_checks": [{"kind": "ui_seen", "arg": "order placed"}], "never": [],
            "phases": [{"do": "research", "goal": "desks", "door": None},
                       {"do": "act", "goal": "order the picked desk", "door": None}],
            "deadline": None, "cue": None, "firm": False, "question": None}
    args.update(over)
    out = cc.parse(args, said="order a desk")
    assert isinstance(out, cc.Card), out
    return out


def _reminder(title: str, cue: str, deadline: str) -> cc.Card:
    out = cc.parse({"kind": "commitment", "title": title, "purpose": "", "end_state": "done", "done_checks": [],
                    "never": [], "phases": [], "deadline": deadline, "cue": {"at": cue, "next_action": title},
                    "firm": True, "question": None}, said="remind me")
    assert isinstance(out, cc.Card), out
    return out


def test_the_first_brief_is_three_lines_at_most_from_the_ledger(tmp_path: Path) -> None:
    clock = [at(9)]
    led = cl.Ledger(tmp_path, clock=lambda: clock[0])
    assert cd.first_brief(led, at(9)) == ""
    desk_job = led.new(_job("Standing desk"))
    led.update(desk_job.id, why="go asked", status="waiting", waiting_for="go")
    bom = led.new(_reminder("BOM export", "2026-09-29 10:00", "2026-09-30 17:00"))
    old = led.new(_job("Monitor arm"))
    led.update(old.id, why="closed", status="done")
    led.new(dataclasses.replace(_job("Desk lamp"), status="proposed"))
    brief = cd.first_brief(led, at(9, 30))
    assert brief.splitlines() == [
        f"Today's one thing: BOM export ({bom.id}), due 2026-09-30 17:00.",
        f"Waiting on you: {desk_job.id} your Go (order the picked desk).",
        f"Closed since yesterday: Monitor arm ({old.id}, done).",
    ]


def test_the_evening_close_and_the_weekly_sheet(tmp_path: Path) -> None:
    clock = [at(12, day=20)]
    led = cl.Ledger(tmp_path, clock=lambda: clock[0])
    stale = led.new(_job("Old errand"))
    clock[0] = at(12)
    done = led.new(_job("Standing desk"))
    led.update(done.id, why="receipt", status="done")
    slipped = led.new(_reminder("BOM export", "2026-09-28 10:00", "2026-09-29 17:00"))
    tomorrow = led.new(_reminder("Call the bank", "2026-09-30 09:00", "2026-09-30 12:00"))
    ignored = led.new(_reminder("File taxes", "2026-10-05 09:00", "2026-10-06 12:00"))
    led.update(ignored.id, why="ignored", nudges=Nudges(sent=3, ignored=3, level="sheet"))
    unsure = led.new(_job("Desk mat"))
    led.update(unsure.id, why="receipt", status="unverified")
    close = cd.evening_close(led, at(21))
    assert close.splitlines() == [
        f"Closed today: Standing desk ({done.id}: 'order placed' seen on screen).",
        f"Slipped: BOM export ({slipped.id}, due 2026-09-29 17:00).",
        f"Tomorrow first: Call the bank ({tomorrow.id})?",
    ]
    sheet = cd.weekly_sheet(led, at(18, day=29))
    assert sheet.ids == (stale.id, ignored.id, unsure.id)
    assert f"{stale.id} Old errand: untouched for 9 days" in sheet.text
    assert f"{ignored.id} File taxes: reminder ignored 3 times" in sheet.text
    assert f"{unsure.id} Desk mat: not confirmed" in sheet.text
    assert tomorrow.id not in sheet.ids


# ---- docs (G2.3) --------------------------------------------------------------------------------

def _knob_row(text: str, knob: str) -> str:
    match = re.search(rf"^\| `{knob}` \| `([^`]*)` \|", text, re.M)
    assert match, f"no Knobs row for {knob}"
    return match.group(1)


# Every EVE_CHIEF* setting the code reads, and the constant its documented default must equal. A setting
# the code reads that is missing here fails the docs test below: add its row to chief.md and its default here.
DOCUMENTED_DEFAULTS = {
    "EVE_CHIEF": chief.DEFAULT_MODE,
    "EVE_CHIEF_ASSESS_MODEL": chief.DEFAULT_ASSESS_MODEL,
    "EVE_CHIEF_ASSESS_EFFORT": chief.DEFAULT_ASSESS_EFFORT,
    "EVE_CHIEF_DIR": cl.DEFAULT_DIR,
    "EVE_CHIEF_PUSH": cd.DEFAULT_PUSH_MODE,
    "EVE_CHIEF_REPLAN": chief_replan.DEFAULT_MODE,
}


def test_docs_name_every_chief_setting_the_code_reads_with_its_default() -> None:
    read = set()
    for path in SRC.rglob("*.py"):
        read.update(re.findall(r"\bEVE_CHIEF[A-Z0-9_]*", path.read_text(encoding="utf-8")))
    assert read, "positive control: the chief's own modules read EVE_CHIEF settings"
    assert {"EVE_CHIEF_DIR", "EVE_CHIEF_PUSH"} <= read
    text = CHIEF_DOC.read_text(encoding="utf-8")
    for knob in sorted(read):
        assert knob in DOCUMENTED_DEFAULTS, f"{knob} is read by the code but has no documented default here"
        assert _knob_row(text, knob) == DOCUMENTED_DEFAULTS[knob], knob


def test_docs_mandate_defaults_equal_the_constants() -> None:
    text = CHIEF_DOC.read_text(encoding="utf-8")
    assert _knob_row(text, "quiet_start") == cd.QUIET_START == "22:30"
    assert _knob_row(text, "quiet_end") == cd.QUIET_END == "08:00"
    assert _knob_row(text, "pushes_per_day") == str(cd.PUSHES_PER_DAY) == "2"
    assert set(cd.MANDATE_KEYS) == {"quiet_start", "quiet_end", "pushes_per_day"}
    for word in ("$0.50 and 20 min", "$1 and 30 min", "21:00", "Sunday 18:00", "45 minutes", "90 seconds"):
        assert word in text, word
    assert cc.DEFAULT_BUDGET_READ_USD == 0.50 and cc.DEFAULT_BUDGET_READ_MIN == 20
    assert cc.DEFAULT_BUDGET_ACT_USD == 1.00 and cc.DEFAULT_BUDGET_ACT_MIN == 30
    assert cd.EVENING_CLOSE_AT == "21:00" and cd.WEEKLY_SHEET_AT == "18:00" and cd.WEEKLY_SHEET_DAY == 6


def test_docs_name_the_live_sets_and_carry_no_owner_data() -> None:
    text = CHIEF_DOC.read_text(encoding="utf-8")
    for kind in cc.DONE_KINDS_LIVE + cc.RESULT_STATUSES + cc.STATUSES:
        assert f"`{kind}" in text, kind
    for kind in cc.PHASE_KINDS_LIVE:
        assert f"`{kind}`" in text, kind
    for event in cl.EVENT_KINDS:
        assert event in text, event
    assert not re.search(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}|/Users/|\b[0-9]{9,}\b", text)
    # positive control: the same pattern finds a planted address
    assert re.search(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}", "write to someone@example.org")
    assert "Guru" not in text.replace("guru_says_done", "").replace("needs_guru", "")


# ---- the reviewer's attacks after P3 (2026-09-29): each was reproduced first, then fixed ----------------------

@pytest.mark.parametrize("row", ['{"kind":"acted","id":"c1","t":"soon"}', '{"kind":"acted","id":"c1","t":1e999}',
                                 '{"kind":"acted","id":"c1","t":[1]}', '{"t":"x","unprompted":true,"route":"now"}'])
def test_attack_a_bad_row_in_the_attention_log_is_skipped(tmp_path: Path, row: str) -> None:
    (tmp_path / cd.ATTENTION_FILE).write_text(row + '\n{"kind":"acted","id":"c2","t":5.0}\n')
    d = Desk(tmp_path, mode="on")
    assert d.pushes_today(at(10)) == 0
    assert d.offer(Item("reminder", "c2", queued_at=4.0), at(10)) == Route("drop", "owner_acted")    # control


def test_attack_a_budget_warning_is_unprompted(tmp_path: Path) -> None:
    late, morning = at(23), at(10)
    on = desk(tmp_path / "on")
    on.breakpoint("task_end", late)
    assert on.offer(Item("warn", "c1", queued_at=late), late).rule == "quiet_hours"
    assert desk(tmp_path / "off", mode="off").offer(Item("warn", "c1"), morning) == Route("batch", "push_off")
    on.breakpoint("task_end", morning)
    assert on.offer(Item("warn", "c1", queued_at=morning), morning) == Route("now", "budget_warning")
    assert on.pushes_today(morning) == 1                                     # it counts against the day's budget


def test_pushes_today_parses_only_new_attention_lines(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    """Review D6: pushes_today re-parsed all of attention.jsonl on every offer."""
    import json
    row = '{{"t":{t},"kind":"verdict","id":"{i}","route":"{r}","unprompted":true}}\n'
    (tmp_path / cd.ATTENTION_FILE).write_text(row.format(t=at(9), i="c1", r="now"))
    d = Desk(tmp_path, mode="on")
    assert d.pushes_today(at(10)) == 1
    calls = [0]
    real = json.loads

    def counted(*a: Any, **k: Any) -> Any:
        calls[0] += 1
        return real(*a, **k)
    monkeypatch.setattr(json, "loads", counted)
    assert d.pushes_today(at(10)) == 1 and calls[0] == 0
    with (tmp_path / cd.ATTENTION_FILE).open("a") as f:
        f.write(row.format(t=at(9, 30), i="c2", r="silent"))
    assert d.pushes_today(at(10)) == 2 and calls[0] == 1
