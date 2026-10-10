"""fast_lane.py with fake senses, effectors and decider: the code gates of PLAN §3.3
step 5 (sensitive labels, dialogs, System Settings, return into a message, hit-test
miss, focus mismatch, no-repeat, approve), every result-line template with its
payload states, the done_when oracles, the stall rules, the caps, gate(), and the
keyword gate that decides a step without the model when one offered control
uniquely shares the most objective tokens.

Objectives in these tests are chosen for the oracle under test: "switch to week view"
uniquely matches Week, so the keyword gate decides; "show the next seven days" matches
nothing, so the model (the fake Decider) is asked."""

from __future__ import annotations

import pytest

from eve_bridge import fast_lane as fl
from eve_bridge.ax_candidates import Candidate, Snapshot
from eve_bridge.decider import Choice
from eve_bridge.fast_lane import (
    SYSTEM_SETTINGS_BUNDLE,
    DelegateResult,
    MenuItem,
    Thresholds,
    describe_pick,
    format_line,
    gate,
    run_delegate,
)

# ---- fakes -------------------------------------------------------------------------


def cand(cid: str, role: str, label: str, value: str = "", *, frame=None, in_dialog=False, secure=False,
         editable=None, actions=("AXPress",), enabled=True, in_content=False, app="Calendar") -> Candidate:
    if frame is None:
        frame = (100 + 70 * int(cid), 100, 60, 30)
    if editable is None:
        editable = role in ("text field", "search field", "text area", "combo box")
    if editable and actions == ("AXPress",):
        actions = ()
    return Candidate(id=cid, role=role, label=label, value=value, frame=tuple(frame), actions=tuple(actions),
                     enabled=enabled, app=app, in_content=in_content, in_dialog=in_dialog, secure=secure,
                     editable=editable)


_SEQ = [0]


def snap(*cands: Candidate, app="Calendar", bundle="com.apple.iCal", title="September 2026", pid=42,
         dialog_text="", truncated=False, node_count=None, focused=None) -> Snapshot:
    _SEQ[0] += 1
    count = node_count if node_count is not None else len(cands) + 3
    return Snapshot(seq=_SEQ[0], app=app, bundle=bundle, title=title, pid=pid, elements=tuple(cands),
                    context_lines=(f"title: {title}",), focused=focused, dialog_text=dialog_text,
                    node_count=count, truncated=truncated, secs=0.05)


def calendar(week_selected: bool = False) -> Snapshot:
    return snap(cand("0", "button", "Add Event"),
                cand("1", "radio button", "Day"),
                cand("2", "radio button", "Week", "selected" if week_selected else ""),
                cand("3", "radio button", "Month", "" if week_selected else "selected"),
                cand("4", "button", "Today"),
                cand("5", "button", "Delete Event"),
                cand("6", "search field", "Search"))


class Senses:
    def __init__(self, snapshots: list[Snapshot], *, visible: list[set[str]] | None = None,
                 changed: list | None = None, pid: int = 42) -> None:
        self.snapshots = list(snapshots)
        self.visible = list(visible or [set()])
        self.changed = list(changed or [None])
        self.pid = pid
        self.calls = 0

    def snapshot(self) -> Snapshot:
        s = self.snapshots[min(self.calls, len(self.snapshots) - 1)]
        self.calls += 1
        return s

    def text_visible(self, text: str) -> bool:
        # the visibility set that goes with the LATEST snapshot handed out
        idx = min(max(self.calls - 1, 0), len(self.visible) - 1)
        return text in self.visible[idx]

    def focused(self):
        return None

    def frontmost_pid(self) -> int:
        return self.pid

    def screen_changed(self):
        idx = min(max(self.calls - 2, 0), len(self.changed) - 1)
        return self.changed[idx]


class Effectors:
    def __init__(self, *, refuse_click: bool = False, refuse_type: bool = False) -> None:
        self.clicks: list[str] = []
        self.typed: list[tuple[str, str]] = []
        self.presses: list[str] = []
        self.settles: list[float] = []
        self.refuse_click = refuse_click
        self.refuse_type = refuse_type

    def click_candidate(self, c: Candidate) -> str:
        if self.refuse_click:
            return f"refused: nothing pressable at the centre of {c.label!r}"
        self.clicks.append(c.label)
        return f"clicked {c.role} {c.label!r}"

    def focus_and_type(self, c: Candidate, text: str) -> str:
        if self.refuse_type:
            return "refused: the focused element is not the field"
        self.typed.append((c.label, text))
        return f"typed {len(text)} characters into {c.label!r}"

    def press(self, key: str) -> str:
        self.presses.append(key)
        return f"pressed {key}"

    def settle(self, secs: float) -> float:
        self.settles.append(secs)
        return 0.3


class Decider:
    """Picks by option-text substring ("Week"), or a reserved key, or "error"; records every menu."""

    def __init__(self, picks: list[str], *, p_top: float = 0.7, margin: float = 0.4, available: bool = True) -> None:
        self.picks = list(picks)
        self.p_top, self.margin = p_top, margin
        self.available = available
        self.menus: list[dict[str, str]] = []
        self.calls: list[dict] = []

    def choose(self, objective: str, *, app: str, context: str, options: dict[str, str], recent=()) -> Choice:
        self.menus.append(dict(options))
        self.calls.append({"objective": objective, "app": app, "context": context, "recent": list(recent)})
        pick = self.picks.pop(0) if self.picks else "abstain"
        probs = {k: 0.0 for k in options}
        if pick == "error":
            return Choice("", error="predict raised RuntimeError: boom", k=len(options))
        if pick in ("reobserve", "abstain"):
            key = pick
        else:
            key = next((k for k, v in options.items() if pick.casefold() in v.casefold()), None)
            if key is None:
                raise AssertionError(f"{pick!r} is not in the menu {list(options.values())}")
        probs[key] = self.p_top
        return Choice(key, p_top=self.p_top, margin=self.margin, confidence=0.3, probabilities=probs, ms=9.0,
                      input_tokens=200, k=len(options))


class Clock:
    def __init__(self) -> None:
        self.t = 0.0

    def __call__(self) -> float:
        self.t += 0.001
        return self.t


def run(objective: str, senses: Senses, decider: Decider, effectors: Effectors | None = None, **kw) -> tuple[
        DelegateResult, Effectors]:
    eff = effectors or Effectors()
    # These tests pin the lane's gates on the path where a model answers; the keyword-only
    # default (fast_lane.DEFAULT_DECIDE) has its own file, tests/test_lane_first.py.
    kw.setdefault("decide", "model")
    result = run_delegate(objective, senses=senses, effectors=eff, decider=decider, clock=Clock(), **kw)
    return result, eff


# ---- the safety gates (GATES.md G8) ----------------------------------------------------


def test_sensitive_candidate_not_offered() -> None:
    d = Decider(["Week"])
    r, eff = run("show the next seven days", Senses([calendar(), calendar(True)]), d, done_when="Week")
    assert r.status == "done"
    [menu] = d.menus                                                 # the model was asked: no keyword match
    assert not any("Delete Event" in v for v in menu.values())
    assert any("Week" in v for v in menu.values()) and eff.clicks == ["Week"]
    assert r.steps[0].via == "model" and r.steps[0].offered == tuple(menu[k] for k in menu if k not in ("reobserve", "abstain"))
    # the keyword path builds the same menu: the sensitive control is absent there too
    d2 = Decider([])
    r2, eff2 = run("switch to week view", Senses([calendar(), calendar(True)]), d2, done_when="Week")
    assert r2.status == "done" and d2.menus == [] and eff2.clicks == ["Week"]
    assert r2.steps[0].via == "keyword" and not any("Delete Event" in t for t in r2.steps[0].offered)


def test_sensitive_pick_confirms_without_clicking() -> None:
    compose = snap(cand("0", "button", "Send"), cand("1", "button", "Attach"), cand("2", "button", "Emoji"),
                   app="Mail", bundle="com.apple.mail", title="New Message")
    d = Decider(["Send"])
    r, eff = run("send the email", Senses([compose]), d)
    assert r.status == "confirm"
    assert r.line == ('confirm: "Send" needs the human\'s yes; app="Mail"; action=click; steps=0; last=none; '
                      "text=none; key=none")
    assert eff.clicks == [] and d.menus == []                      # never offered, never clicked
    assert r.log_lines[-1].startswith("delegate confirm:")


def test_sheet_button_never_offered_escalates_dialog_open() -> None:
    sheet = snap(cand("0", "radio button", "Week"), cand("1", "button", "Today"),
                 cand("2", "button", "OK", in_dialog=True), cand("3", "button", "Cancel", in_dialog=True),
                 dialog_text="Delete this event?")
    d = Decider(["OK"])
    r, eff = run("switch to week view", Senses([sheet]), d, done_when="Week")
    assert r.status == "escalate" and 'reason=dialog_open: "Delete this event?"' in r.line
    assert d.menus == [] and eff.clicks == []


def test_system_settings_toggle_confirms() -> None:
    settings = snap(cand("0", "row", "General"), cand("1", "row", "Appearance"), cand("2", "radio button", "Light",
                    "selected"), cand("3", "radio button", "Dark"), cand("4", "button", "Back"),
                    app="System Settings", bundle=SYSTEM_SETTINGS_BUNDLE, title="Appearance")
    d = Decider(["Dark"])
    r, eff = run("turn on dark mode", Senses([settings]), d)
    assert r.status == "confirm" and r.line.startswith('confirm: "Dark" needs the human\'s yes; app="System Settings"')
    assert eff.clicks == [] and d.menus == []
    # navigation in the same pane is still offered: rows, tabs, links and Back
    d2 = Decider(["Back"])
    r2, eff2 = run("go back to the previous pane", Senses([settings, settings]), d2)
    assert r2.status == "stopped" and eff2.clicks == ["Back"]
    assert d2.menus == [] and r2.steps[0].via == "keyword"        # "back" uniquely matches: no model call
    assert not any("Dark" in t or "Light" in t for t in r2.steps[0].offered)
    # the value change is withheld from the model path just the same
    d3 = Decider(["Appearance"])
    r3, _ = run("show me the look and feel pane", Senses([settings, settings]), d3)
    [menu] = d3.menus
    assert not any("Dark" in v or "Light" in v for v in menu.values()) and r3.steps[0].via == "model"


def message_compose() -> Snapshot:
    return snap(cand("0", "button", "Emoji"), cand("1", "button", "Audio"), cand("2", "text area", "Message"),
                app="Messages", bundle="com.apple.MobileSMS", title="Maya Patel")


def test_return_after_message_area_confirms() -> None:
    d = Decider(["type into text area: Message", "press return"])
    r, eff = run("enter the message and send it", Senses([message_compose(), message_compose()]), d,
                 text="I'm downstairs.", key="return", done_when="Delivered")
    assert r.status == "confirm"
    assert r.line == ('confirm: "return" needs the human\'s yes; app="Messages"; action=press; steps=1; '
                      "last=type 15 chars into text area: Message; text=applied; key=pending")
    assert eff.typed == [("Message", "I'm downstairs.")] and eff.presses == []
    assert "I'm downstairs" not in r.line                             # the typed text never reaches a log line


def notes_search() -> Snapshot:
    return snap(cand("0", "button", "New Note"), cand("1", "button", "Folders"), cand("2", "search field", "Search"),
                app="Notes", bundle="com.apple.Notes", title="Notes")


def test_return_after_search_field_presses_once() -> None:
    d = Decider(["type into search field: Search", "press return"])
    senses = Senses([notes_search(), notes_search(), notes_search()], visible=[set(), set(), {"Results"}])
    r, eff = run("search my notes", senses, d, text="blue adapter", key="return", done_when="Results")
    assert r.status == "done", r.line
    assert eff.typed == [("Search", "blue adapter")] and eff.presses == ["return"]
    assert r.line == 'done: matched="Results" via=ocr; steps=2; last=press return; text=applied; key=applied'
    # a press option is offered only once the text has been applied
    assert not any("press return" in v for v in d.menus[0].values())
    assert any("press return" in v for v in d.menus[1].values())


def test_hit_test_miss_zero_input() -> None:
    d = Decider(["Week"])
    r, eff = run("switch to week view", Senses([calendar()]), d, Effectors(refuse_click=True), done_when="Week")
    assert r.status == "escalate" and "reason=hit_test_failed" in r.line and "steps=0; last=none" in r.line
    assert eff.clicks == [] and r.steps[-1].reason == "hit_test_failed"


def test_focus_mismatch_zero_paste() -> None:
    d = Decider(["type into search field: Search"])
    r, eff = run("search my notes", Senses([notes_search()]), d, Effectors(refuse_type=True), text="blue adapter",
                 done_when="Results")
    assert r.status == "escalate" and "reason=focus_changed" in r.line
    assert eff.typed == [] and "text=pending" in r.line


def test_no_repeat_after_unchanged() -> None:
    d = Decider(["abstain"])
    senses = Senses([calendar(), calendar(), calendar()], changed=[False, False])
    r, eff = run("switch to week view", senses, d, done_when="Never")
    assert eff.clicks == ["Week"]
    assert any("Week" in t for t in r.steps[0].offered) and r.steps[0].via == "keyword"
    [menu] = d.menus                                                 # step 2 went to the model
    assert not any("Week" in v for v in menu.values())               # not re-offered after an unchanged step
    assert not any("Week" in t for t in r.steps[1].offered)
    assert r.status == "escalate" and "reason=abstain" in r.line and r.steps[0].changed is False


def test_approve_allows_one_sensitive_click() -> None:
    events = snap(cand("0", "button", "Delete Event"), cand("1", "button", "Discard Draft"), cand("2", "button", "Today"),
                  cand("3", "button", "Inspector"))
    d = Decider(["abstain"])
    senses = Senses([events, events, events], changed=[True, True])
    r, eff = run("delete the selected event", senses, d, approve="Delete Event", done_when="Never", max_steps=3)
    assert eff.clicks == ["Delete Event"] and r.steps[0].via == "keyword"   # approved AND the unique match
    assert any("Delete Event" in t for t in r.steps[0].offered)
    assert not any("Discard" in t for t in r.steps[0].offered)        # only the approved label, once
    [menu] = d.menus                                                  # step 2: nothing matches, the model is asked
    assert not any("Delete" in v or "Discard" in v for v in menu.values())   # approval consumed: neither offered
    assert r.status == "escalate" and "steps=1; last=click button: Delete Event" in r.line
    # with no safe match left after the approved click, the next sensitive match is a confirm, never a click
    only = snap(cand("0", "button", "Delete Event"), cand("1", "button", "Delete All"), cand("2", "button", "Today"))
    d2 = Decider([])
    r2, eff2 = run("delete the selected event", Senses([only, only], changed=[True]), d2,
                   approve="Delete Event", done_when="Never", max_steps=3)
    assert eff2.clicks == ["Delete Event"] and d2.menus == []
    assert r2.status == "confirm" and r2.line.startswith('confirm: "Delete All" needs the human\'s yes')
    assert "steps=1; last=click button: Delete Event" in r2.line
    # the model path honours approve identically: the objective shares no token, the fake picks it
    d3 = Decider(["Delete Event"])
    r3, eff3 = run("get rid of the highlighted appointment", Senses([events, events], changed=[True]), d3,
                   approve="Delete Event", done_when="Never", max_steps=1)
    assert eff3.clicks == ["Delete Event"] and r3.steps[0].via == "model"


def test_week_radio_clicked_once() -> None:
    d = Decider([])
    r, eff = run("switch to week view", Senses([calendar(), calendar(True)]), d, done_when="Week")
    assert r.status == "done"
    assert r.line == 'done: matched="Week" via=ax; steps=1; last=click radio button: Week; text=none; key=none'
    assert eff.clicks == ["Week"] and eff.settles == [1.0]
    [step] = r.steps
    assert step.verdict == "act" and step.changed is True and step.n == 1 and step.k == 7   # 5 offered + 2 reserved
    assert step.via == "keyword" and step.kind == "click" and step.p_top == 1.0 and d.menus == []
    assert step.description == "Week (radio button)" and "Month (radio button, selected)" in step.offered
    assert r.log_lines[0].startswith("delegate step 1: Week (radio button) (keyword) act")
    assert r.log_lines[-1].startswith("delegate done:") and all(len(ln) <= 80 for ln in r.log_lines)
    # the model path, positive control: no keyword match, the fake picks Week
    d3 = Decider(["Week"])
    r3, eff3 = run("show the next seven days", Senses([calendar(), calendar(True)]), d3, done_when="Week")
    assert r3.status == "done" and eff3.clicks == ["Week"] and r3.steps[0].via == "model"
    assert r3.log_lines[0].startswith("delegate step 1: Week (radio button) (p 0.70) act")
    # negative control: already in week view → done with zero clicks, before any step
    d2 = Decider(["Week"])
    r2, eff2 = run("switch to week view", Senses([calendar(True)]), d2, done_when="Week")
    assert r2.status == "done" and eff2.clicks == [] and d2.menus == [] and "steps=0; last=none" in r2.line


# ---- the other statuses and reasons --------------------------------------------------------


def test_one_step_without_done_when() -> None:
    d = Decider(["Today"])
    r, eff = run("jump to today", Senses([calendar(), calendar()]), d)
    assert r.status == "stopped" and eff.clicks == ["Today"]
    assert r.line == ('stopped: reason=one_step; steps=1; last=click button: Today; pick=4 button "Today"; '
                      "text=none; key=none")


def test_dry_run_reports_the_pick_and_touches_nothing() -> None:
    d = Decider(["Week"])
    r, eff = run("switch to week view", Senses([calendar()]), d, done_when="Week", dry_run=True)
    assert r.status == "stopped" and eff.clicks == [] and eff.settles == []
    assert r.line == ('stopped: reason=dry_run; steps=0; last=none; pick=2 radio button "Week"; text=none; '
                      "key=none")
    assert r.steps[0].verdict == "stopped" and r.steps[0].reason == "dry_run"


def test_reobserve_once_then_stalled() -> None:
    d = Decider(["reobserve", "Week"])
    senses = Senses([calendar(), calendar(), calendar(True)])
    r, eff = run("show the next seven days", senses, d, done_when="Week")
    assert r.status == "done" and eff.clicks == ["Week"] and senses.calls == 3
    assert [s.verdict for s in r.steps] == ["reobserve", "act"] and [s.n for s in r.steps] == [1, 1]
    d2 = Decider(["reobserve", "reobserve"])
    r2, eff2 = run("show the next seven days", Senses([calendar()]), d2, done_when="Week")
    assert r2.status == "escalate" and "reason=stalled;" in r2.line and eff2.clicks == []


def test_two_unchanged_steps_escalate_stalled_2() -> None:
    d = Decider(["Today"])                                            # step 1 is the keyword gate's
    senses = Senses([calendar(), calendar(), calendar()], changed=[False, False])
    r, eff = run("switch to week view", senses, d, done_when="Never", max_steps=4)
    assert r.status == "escalate" and "reason=stalled_2" in r.line
    assert eff.clicks == ["Week", "Today"] and "steps=2; last=click button: Today" in r.line


def test_ambiguous_target_escalates_on_low_probability_or_margin() -> None:
    r, eff = run("show the next seven days", Senses([calendar()]), Decider(["Week"], p_top=0.2, margin=0.1),
                 done_when="Week")
    assert r.status == "escalate" and "reason=ambiguous_target" in r.line and eff.clicks == []
    r, eff = run("show the next seven days", Senses([calendar()]), Decider(["Week"], p_top=0.5, margin=0.01),
                 done_when="Week")
    assert r.status == "escalate" and "reason=ambiguous_target" in r.line and eff.clicks == []
    assert r.line.startswith('escalate: app="Calendar"; reason=ambiguous_target; top=[')
    assert 'top=[0 button "Add Event", ' in r.line                    # no match: menu order is reading order
    # a keyword pick is never ambiguous: the same low-probability fake is not even asked
    r, eff = run("switch to week view", Senses([calendar(), calendar(True)]),
                 Decider(["Week"], p_top=0.2, margin=0.1), done_when="Week")
    assert r.status == "done" and eff.clicks == ["Week"]


def test_model_error_and_abstain_escalate() -> None:
    r, eff = run("show the next seven days", Senses([calendar()]), Decider(["error"]), done_when="Week")
    assert r.status == "escalate" and "reason=model_invalid" in r.line and eff.clicks == []
    assert any("predict raised" in ln for ln in r.log_lines)
    r, eff = run("show the next seven days", Senses([calendar()]), Decider(["abstain"]), done_when="Week")
    assert r.status == "escalate" and "reason=abstain" in r.line and eff.clicks == []


def test_no_window_and_no_candidate_and_truncated() -> None:
    empty = snap(app="Calendar", node_count=0)
    r, _ = run("switch to week view", Senses([empty]), Decider(["Week"]), done_when="Week")
    assert r.status == "escalate" and "reason=no_window" in r.line
    bare = snap(cand("0", "button", "Today"))                        # one pressable, nothing editable
    senses = Senses([bare, bare])
    r, _ = run("switch to week view", senses, Decider(["Today"]), done_when="Week")
    assert r.status == "escalate" and "reason=no_candidate" in r.line and senses.calls == 2   # one re-snapshot
    cut = snap(cand("0", "button", "Today"), cand("1", "button", "Inspector"), truncated=True)
    r, _ = run("switch to week view", Senses([cut]), Decider(["Today"]), done_when="Week")
    assert r.status == "escalate" and "reason=truncated" in r.line
    cut2 = snap(cand("0", "button", "Today"), cand("1", "radio button", "Week"), truncated=True)
    r, eff = run("switch to week view", Senses([cut2, calendar(True)]), Decider(["Week"]), done_when="Week")
    assert r.status == "escalate" and "reason=truncated" in r.line and not eff.clicks
    assert not r.steps                                               # a matching label cannot prove uniqueness
    complete = snap(cand("0", "button", "Today"), cand("1", "radio button", "Week"))
    r, eff = run("switch to week view", Senses([complete, calendar(True)]), Decider(["Week"]), done_when="Week")
    assert r.status == "done" and eff.clicks == ["Week"]              # complete positive control still acts


def test_truncated_after_observation_retains_input_without_claiming_done() -> None:
    from dataclasses import replace

    after = replace(calendar(True), truncated=True)
    r, eff = run("switch to week view", Senses([calendar(), after]), Decider(["Week"]), done_when="Week")
    assert r.status == "escalate" and "reason=truncated" in r.line
    assert eff.clicks == ["Week"] and "steps=1" in r.line


def test_step_cap_escalates_after_rechecking_done_when() -> None:
    d = Decider(["Today"])                                            # step 1: keyword; step 2: the model
    senses = Senses([calendar(), calendar(), calendar()], changed=[True, True])
    r, eff = run("switch to week view", senses, d, done_when="Never", max_steps=2)
    assert r.status == "escalate" and "reason=step_cap" in r.line and eff.clicks == ["Week", "Today"]
    assert [s.via for s in r.steps] == ["keyword", "model"]
    d = Decider(["Today"])
    senses = Senses([calendar(), calendar(), calendar()], changed=[True, True], visible=[set(), set(), {"Bingo"}])
    r, _ = run("switch to week view", senses, d, done_when="Bingo", max_steps=2)
    assert r.status == "done" and "via=ocr" in r.line                # the last settle is checked first


def test_done_via_title_only_when_it_was_absent_at_step_0() -> None:
    before = snap(cand("0", "row", "Inbox"), cand("1", "row", "Travel"), app="Mail", title="Inbox")
    after = snap(cand("0", "row", "Inbox"), cand("1", "row", "Travel"), app="Mail", title="Travel — 3 messages")
    r, eff = run("open the travel mailbox", Senses([before, after]), Decider(["Travel"]), done_when="Travel")
    assert r.status == "done" and "via=title" in r.line and eff.clicks == ["Travel"]
    already = snap(cand("0", "row", "Inbox"), cand("1", "row", "Travel"), app="Mail", title="Travel — 3 messages")
    r, eff = run("open the travel mailbox", Senses([already, already]), Decider(["Travel"]), done_when="Travel",
                 max_steps=1)
    assert r.status == "escalate" and "reason=step_cap" in r.line     # present at step 0: not evidence


def test_unavailable_decider_means_zero_input() -> None:
    senses = Senses([calendar()])
    r, eff = run("switch to week view", senses, Decider(["Week"], available=False), done_when="Week")
    assert r.status == "unavailable" and r.line == "unavailable: the decider is not loaded"
    assert eff.clicks == [] and senses.calls == 0
    r, eff = run("switch to week view", senses, None, done_when="Week")
    assert r.status == "unavailable" and eff.clicks == []
    r, _ = run("", senses, Decider(["Week"]))
    assert r.status == "unavailable" and "objective" in r.line


def test_key_combos_are_unavailable() -> None:
    for key in ("command+l", "cmd+w", "ctrl+c", "shift tab"):
        r, eff = run("close the tab", Senses([calendar()]), Decider(["Week"]), key=key)
        assert r.status == "unavailable" and r.line == "unavailable: key combos are not supported"
        assert eff.presses == [] and eff.clicks == []


def test_other_keys_press_without_confirm_and_escape_after_type_is_fine() -> None:
    d = Decider(["press escape"])
    r, eff = run("dismiss the popover", Senses([notes_search(), notes_search()]), d, key="escape")
    assert r.status == "stopped" and eff.presses == ["escape"] and "key=applied" in r.line
    assert r.line.endswith("last=press escape; pick=none; text=none; key=applied")


def test_type_options_are_capped_and_never_secure() -> None:
    form = snap(cand("0", "button", "Continue"), cand("1", "text field", "Name"), cand("2", "text field", "Email"),
                cand("3", "text field", "Password", secure=True), cand("4", "text field", "Postcode"),
                cand("5", "text field", "City"), app="Safari", bundle="com.apple.Safari", title="Form")
    d = Decider(["type into text field: Name"])
    r, eff = run("fill in the name", Senses([form, form]), d, text="Maya")
    [menu] = d.menus
    typed = [v for v in menu.values() if v.startswith("type into")]
    assert len(typed) == 3 and not any("Password" in v for v in typed)
    assert typed[0] == "type into text field: Name"                  # the objective's match ranks first
    assert eff.typed == [("Name", "Maya")] and r.status == "stopped" and "text=applied" in r.line


def test_max_steps_is_clamped_and_menu_is_bounded() -> None:
    many = snap(*[cand(str(i), "button", f"Button {i}") for i in range(20)])
    d = Decider(["Button 0", "Button 1"] * 4)                          # the previous pick is never re-offered
    senses = Senses([many] * 8, changed=[True] * 8)
    r, eff = run("press button 0", senses, d, done_when="Never", max_steps=99)
    assert len(eff.clicks) == 6 and "reason=step_cap" in r.line       # clamped to 6
    assert all(len(m) <= 12 for m in d.menus) and r.steps[0].dropped_cap == 10


def test_focus_change_before_acting_escalates() -> None:
    senses = Senses([calendar()], pid=99)                             # another app took the front
    r, eff = run("switch to week view", senses, Decider(["Week"]), done_when="Week")
    assert r.status == "escalate" and "reason=focus_changed" in r.line and eff.clicks == []


def test_decider_receives_context_and_recent_actions() -> None:
    d = Decider(["Week", "Today"])
    senses = Senses([calendar(), calendar(), calendar()], changed=[True, True])
    run("show the next seven days", senses, d, done_when="Never", max_steps=2)
    assert d.calls[0]["app"] == "Calendar" and d.calls[0]["context"] == "title: September 2026"
    assert d.calls[0]["recent"] == [] and d.calls[1]["recent"] == ["click radio button: Week"]
    assert set(d.menus[0]) >= {"reobserve", "abstain", "1"}
    assert d.menus[0]["reobserve"] == fl.REOBSERVE_TEXT and d.menus[0]["abstain"] == fl.ABSTAIN_TEXT
    assert d.menus[0]["1"] == "Add Event (button)" and "Month (radio button, selected)" in d.menus[0].values()
    focused = snap(cand("0", "button", "Today"), cand("1", "radio button", "Week"), focused=cand("9", "search field", "Search"))
    d2 = Decider(["Week"])
    run("show the next seven days", Senses([focused, focused]), d2)
    assert d2.calls[0]["context"] == "title: September 2026; focused: search field Search"
    assert fl.lane_context(snap(title="  A   B ")) == "title: A B"


# ---- the keyword gate ---------------------------------------------------------------------


def _items(*cands: Candidate) -> list[MenuItem]:
    return [MenuItem(str(i + 1), "click", fl.render_option(c), c) for i, c in enumerate(cands)]


def test_keyword_pick_unique_maximum_only() -> None:
    week, today, month = cand("1", "radio button", "Week"), cand("2", "button", "Today"), cand("3", "radio button", "Month")
    items = _items(week, today, month)
    assert fl.keyword_pick(items, "switch to week view").candidate is week
    assert fl.keyword_pick(items, "go to the next seven days") is None                 # zero overlap
    assert fl.keyword_pick(items, "week or month") is None                             # a tie
    assert fl.keyword_pick(items, "") is None
    two = _items(cand("1", "row", "Week calendar"), cand("2", "radio button", "Week"))
    assert fl.keyword_pick(two, "show the week calendar").candidate.label == "Week calendar"   # two tokens beat one
    # the value counts: "the selected one" matches a selected control; type and press items never do
    sel = _items(cand("1", "radio button", "Month", "selected"), cand("2", "radio button", "Week"))
    assert fl.keyword_pick(sel, "open the selected one").candidate.label == "Month"
    typed = [MenuItem("1", "type", "type into search field: Search", cand("5", "search field", "Search"), "x"),
             MenuItem("2", "press", "press return", None, "return")]
    assert fl.keyword_pick(typed, "search return") is None


def test_keyword_pick_is_still_gated_by_every_code_oracle() -> None:
    # hit-test refusal: the keyword pick is refused by the effector and nothing is applied
    r, eff = run("switch to week view", Senses([calendar()]), Decider([]), Effectors(refuse_click=True),
                 done_when="Week")
    assert r.status == "escalate" and "reason=hit_test_failed" in r.line and eff.clicks == []
    assert r.steps[0].via == "keyword"
    # a sensitive control is withheld before the gate ever sees it: "delete" cannot be keyword-picked,
    # and the look-alike "Add Event" (one shared token against Delete Event's two) is never clicked instead
    r, eff = run("delete the event", Senses([calendar()]), Decider([]))
    assert r.status == "confirm" and r.line.startswith('confirm: "Delete Event"') and eff.clicks == []
    r, eff = run("delete the event", Senses([calendar()]), Decider(["Add Event"]))
    assert r.status == "confirm" and eff.clicks == []
    # an offered control that matches strictly better than any withheld one is picked as usual
    r, eff = run("add a new event", Senses([calendar(), calendar()]), Decider([]))
    assert r.status == "stopped" and eff.clicks == ["Add Event"] and r.steps[0].via == "keyword"
    # System Settings: a value control is withheld; the navigation row is keyword-picked
    settings = snap(cand("0", "row", "General"), cand("1", "row", "Appearance"), cand("2", "button", "Dark"),
                    app="System Settings", bundle=SYSTEM_SETTINGS_BUNDLE, title="General")
    r, eff = run("open the appearance pane", Senses([settings, settings]), Decider([]))
    assert r.status == "stopped" and eff.clicks == ["Appearance"] and r.steps[0].via == "keyword"
    r, eff = run("turn on dark mode", Senses([settings]), Decider([]))
    assert r.status == "confirm" and eff.clicks == []
    # a dialog stops the run before any pick
    sheet = snap(cand("0", "radio button", "Week"), cand("1", "button", "OK", in_dialog=True), dialog_text="Sure?")
    r, eff = run("switch to week view", Senses([sheet]), Decider([]), done_when="Week")
    assert r.status == "escalate" and "dialog_open" in r.line and eff.clicks == []
    # the focus check and the no-repeat rule apply too
    r, eff = run("switch to week view", Senses([calendar()], pid=7), Decider([]), done_when="Week")
    assert r.status == "escalate" and "reason=focus_changed" in r.line and eff.clicks == []
    d = Decider(["abstain"])
    r, eff = run("switch to week view", Senses([calendar()] * 3, changed=[True, True]), d, done_when="Never")
    assert eff.clicks == ["Week"] and not any("Week" in t for t in r.steps[1].offered) and r.steps[1].via == "model"


# ---- format_line and gate() ----------------------------------------------------------------


def test_format_line_templates_and_escaping() -> None:
    week = cand("2", "radio button", 'Say "hi"\nnow')
    assert describe_pick(week) == '2 radio button "Say \\"hi\\" now"'
    assert format_line("unavailable", reason="x") == "unavailable: x"
    assert format_line("done", done_when='We"ek', via="ax", steps=1, last="click radio button: Week") == \
        'done: matched="We\\"ek" via=ax; steps=1; last=click radio button: Week; text=none; key=none'
    assert format_line("escalate", app="Cal", reason="abstain", top=(week, week, week, week), steps=0) == \
        'escalate: app="Cal"; reason=abstain; top=[2 radio button "Say \\"hi\\" now", 2 radio button "Say \\"hi\\" now", ' \
        '2 radio button "Say \\"hi\\" now"]; steps=0; last=none; text=none; key=none'
    assert format_line("confirm", label="Send", app="Mail", action="click", text_state="applied", key_state="pending") \
        == 'confirm: "Send" needs the human\'s yes; app="Mail"; action=click; steps=0; last=none; text=applied; key=pending'
    with pytest.raises(ValueError):
        format_line("maybe")


def test_gate_table() -> None:
    th = Thresholds(p_min=0.35, margin_min=0.05)
    week = MenuItem("1", "click", "click radio button: Week", cand("2", "radio button", "Week"))
    delete = MenuItem("2", "click", "click button: Delete Event", cand("5", "button", "Delete Event"))
    ret = MenuItem("3", "press", "press return", None, "return")
    tab = MenuItem("4", "press", "press tab", None, "tab")
    ok = dict(thresholds=th, approve=None, approve_used=False, search_typed=False, dialog_open=False, reobserve_count=0)

    def choice(cid: str, p: float = 0.7, m: float = 0.4, error: str = "") -> Choice:
        return Choice(cid, p_top=p, margin=m, error=error)

    assert gate(choice(""), week, **ok) == ("escalate", "model_invalid")
    assert gate(choice("1", error="x"), week, **ok) == ("escalate", "model_invalid")
    assert gate(choice("abstain"), None, **ok) == ("escalate", "abstain")
    assert gate(choice("reobserve"), None, **ok) == ("reobserve", "")
    assert gate(choice("reobserve"), None, **{**ok, "reobserve_count": 1}) == ("escalate", "stalled")
    assert gate(choice("1", p=0.3), week, **ok) == ("escalate", "ambiguous_target")
    assert gate(choice("1", m=0.04), week, **ok) == ("escalate", "ambiguous_target")
    assert gate(choice("9"), None, **ok) == ("escalate", "model_invalid")
    assert gate(choice("1"), week, **ok) == ("act", "")
    assert gate(choice("2"), delete, **ok) == ("confirm", "sensitive")
    assert gate(choice("2"), delete, **{**ok, "approve": "delete event"}) == ("act", "")
    assert gate(choice("2"), delete, **{**ok, "approve": "delete event", "approve_used": True}) == ("confirm", "sensitive")
    assert gate(choice("3"), ret, **ok) == ("confirm", "return")
    assert gate(choice("3"), ret, **{**ok, "search_typed": True}) == ("act", "")
    assert gate(choice("3"), ret, **{**ok, "search_typed": True, "dialog_open": True}) == ("confirm", "return")
    assert gate(choice("4"), tab, **ok) == ("act", "")



# ---- review fixes (2026-09-21) --------------------------------------------------------------

def test_semicolons_in_labels_cannot_forge_result_fields() -> None:
    from eve_bridge.fast_lane import _q

    assert _q('Send; steps=9; last=none') == "Send, steps=9, last=none"
    assert _q('say "hi"\nthere') == 'say \\"hi\\" there'


def test_content_fields_are_not_offered_for_typing_unless_page_links_allowed() -> None:
    from eve_bridge.fast_lane import editable_pool

    s = snap(cand("1", "text field", "Part name", in_content=True), cand("2", "search field", "Search"))
    assert [c.id for c in editable_pool(s)] == ["2"]
    assert [c.id for c in editable_pool(s, allow_page_links=True)] == ["1", "2"]


def test_dialog_open_beats_a_satisfied_done_when() -> None:
    """A sheet with the target already selected: the lane reports the dialog, never 'done'."""
    s = snap(cand("1", "radio button", "Week", "selected"), cand("2", "button", "OK", in_dialog=True),
             dialog_text="Delete this event?")
    result, eff = run("switch to week view", Senses([s]), Decider([]), done_when="Week")
    assert result.status == "escalate" and 'dialog_open: "Delete this event?"' in result.line
    assert eff.clicks == [] and eff.presses == []


def test_a_control_named_exactly_is_not_mistaken_for_a_withheld_look_alike() -> None:
    # browser_model_eval contact_form, 2026-09-24: the plan clicked the "Your message" field and the lane asked the
    # human about "Send message" instead (one shared word each), which a yes would then have pressed.
    form = snap(cand("0", "text field", "Your message", actions=("AXPress", "AXFocus")), cand("1", "button", "Send message"),
                app="browser", bundle="eve.browser", title="Contact us")
    r, eff = run("Your message", Senses([form, form]), Decider([]))
    assert r.status == "stopped" and eff.clicks == ["Your message"], r.line
    # a looser objective still gets the withheld control's confirm, and the withheld control is never clicked
    r, eff = run("the message", Senses([form]), Decider([]))
    assert r.status == "confirm" and r.line.startswith('confirm: "Send message"') and eff.clicks == []
