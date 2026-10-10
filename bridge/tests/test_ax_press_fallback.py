"""The lane's AXPress fallback (desktop_helpers._LaneAdapter._press_instead, after computah, 2026-10-06)."""

from __future__ import annotations

import json

import pytest
from test_desktop_helpers import Bench, _candidate

from eve_bridge.ax_candidates import Snapshot, snapshot_from_raw
from eve_bridge.desktop_helpers import _LaneAdapter

REF = object()                                   # stands for the live AXUIElement


def _live(**kw):
    base = {"role": "AXRadioButton", "title": "Week", "frame": (10, 10, 40, 20), "enabled": True, "pid": 42,
            "actions": ["AXPress"]}
    base.update(kw)
    return base


def _rig(monkeypatch, *, under=None, live=None, press_err=0, ref=REF, cand=None, approve=None):
    monkeypatch.delenv("EVE_AX_PRESS", raising=False)
    c = cand or _candidate("Week")
    b = Bench()
    b.h._element_at = lambda x, y: under
    pressed: list = []

    def press(el):
        pressed.append(el)
        return press_err
    b.h._element_read = lambda el: (live if live is not None else _live()) if el is REF else None
    b.h._element_press = press
    b.h.install({"pyautogui": b.gui})
    a = _LaneAdapter(b.h, approve)
    a._snapshot = Snapshot(seq=1, app="Calendar", bundle="com.apple.iCal", title="w", pid=42, elements=(c,),
                           _refs={c.id: ref} if ref is not None else {})
    return a, b, c, pressed


def test_a_hit_test_miss_presses_the_snapshots_element_and_sends_no_click(monkeypatch) -> None:
    a, b, c, pressed = _rig(monkeypatch, under=None)
    line = a.click_candidate(c)
    assert line.startswith("pressed AXRadioButton 'Week' with AXPress (the hit-test missed: nothing under")
    assert pressed == [REF] and b.gui.clicks == [] and b.h.acted is True


def test_an_overlay_on_the_point_is_a_miss_too(monkeypatch) -> None:
    over = {"role": "AXStaticText", "title": "tooltip", "frame": (5, 5, 90, 40), "app": "Calendar"}
    a, b, c, pressed = _rig(monkeypatch, under=over)
    assert a.click_candidate(c).startswith("pressed AXRadioButton 'Week' with AXPress") and b.gui.clicks == []


def test_a_hit_test_that_finds_the_control_clicks_and_never_also_presses(monkeypatch) -> None:
    hit = {"role": "AXRadioButton", "title": "Week", "frame": (10, 10, 40, 20), "app": "Calendar"}
    a, b, c, pressed = _rig(monkeypatch, under=hit)
    assert a.click_candidate(c).startswith("clicked") and b.gui.clicks == [(30, 20)] and pressed == []


def test_another_apps_window_over_the_point_is_still_refused(monkeypatch) -> None:
    over = {"role": "AXButton", "title": "OK", "frame": (0, 0, 100, 100), "app": "Mail"}
    a, b, c, pressed = _rig(monkeypatch, under=over)
    assert a.click_candidate(c).startswith("refused: (30,20) is in Mail") and pressed == []


@pytest.mark.parametrize("live", [
    _live(pid=7),                                # another process now
    _live(enabled=False),
    _live(actions=["AXShowMenu"]),               # nothing to press
    _live(frame=(10, 60, 40, 20)),               # moved more than 2 pt
    _live(frame=None),
    _live(title="Month"),                        # renamed: a different control in the same place
])
def test_an_element_that_is_no_longer_that_control_is_refused(monkeypatch, live) -> None:
    a, b, c, pressed = _rig(monkeypatch, under=None, live=live)
    assert a.click_candidate(c).startswith("refused: nothing under") and pressed == [] and b.h.acted is False


def test_a_stale_or_missing_element_is_refused(monkeypatch) -> None:
    a, b, c, pressed = _rig(monkeypatch, under=None, ref=None)
    assert a.click_candidate(c).startswith("refused:") and pressed == []
    a, b, c, pressed = _rig(monkeypatch, under=None, ref=object())          # reads as gone
    assert a.click_candidate(c).startswith("refused:") and pressed == []


def test_a_candidate_from_another_snapshot_is_refused(monkeypatch) -> None:
    a, b, c, pressed = _rig(monkeypatch, under=None)
    other = _candidate("Week", frame=(10, 10, 41, 20))                      # same id, not this snapshot's
    assert a.click_candidate(other).startswith("refused:") and pressed == []


def test_a_sensitive_control_needs_approval(monkeypatch) -> None:
    c = _candidate("Delete Event", "button")
    live = _live(role="AXButton", title="Delete Event")
    a, b, c, pressed = _rig(monkeypatch, under=None, live=live, cand=c)
    assert a.click_candidate(c) == "refused: 'Delete Event' is a sensitive control without approval" and pressed == []
    a, b, c, pressed = _rig(monkeypatch, under=None, live=live, cand=c, approve="delete event")
    assert a.click_candidate(c).startswith("pressed AXButton 'Delete Event' with AXPress") and pressed == [REF]


def test_an_error_from_the_press_is_an_unknown_effect_not_a_refusal(monkeypatch) -> None:
    a, b, c, pressed = _rig(monkeypatch, under=None, press_err=-25204)
    line = a.click_candidate(c)
    assert not line.startswith("refused:") and "effect is unknown" in line and b.h.acted is True


def test_the_switch_turns_it_off(monkeypatch) -> None:
    a, b, c, pressed = _rig(monkeypatch, under=None)
    monkeypatch.setenv("EVE_AX_PRESS", "0")
    assert a.click_candidate(c).startswith("refused: nothing under") and pressed == []


def test_refs_ride_the_snapshot_and_never_reach_its_dict() -> None:
    node = {"role": "AXButton", "title": "Week", "frame": [10, 10, 40, 20], "actions": ["AXPress"],
            "enabled": True, "children": []}
    root = {"role": "AXWindow", "title": "w", "frame": [0, 0, 500, 500], "children": [node]}
    raw = {"app": "Calendar", "bundle": "b", "pid": 42, "root": root}
    snap = snapshot_from_raw(raw, seq=1, refs={id(node): REF})
    (c,) = snap.elements
    assert snap.ref(c.id) is REF
    assert snap == snapshot_from_raw(raw, seq=1)                     # refs never change equality
    json.dumps(snap.to_dict())                                       # and never reach the serialised form
    assert snapshot_from_raw(raw, seq=1).ref(c.id) is None
