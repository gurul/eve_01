"""ax_candidates.py on hand-rolled raw walks (no pyobjc): the sensitive-label
table, snapshot derivation (labels, values, tags, viewport, reading order,
dialogs, truncation, no window), the rank drops and scoring, describe and
context_summary, the dict round trip and the raw re-derivation."""

from __future__ import annotations

import json

import pytest

from eve_bridge import ax_candidates as ax
from eve_bridge.ax_candidates import (
    SENSITIVE_LABEL,
    Candidate,
    Snapshot,
    ax_snapshot,
    context_summary,
    describe,
    is_pressable,
    is_sensitive,
    objective_tokens,
    pressable,
    rank_candidates,
    snapshot_from_dict,
    snapshot_from_raw,
)

# ---- fakes -------------------------------------------------------------------------


def node(role: str, title: str = "", *, frame=(0, 0, 10, 10), children=(), actions=(), subrole="", value=None,
         description="", enabled=True, focused=False) -> dict:
    """One raw node in the documented walk shape (ancestor_roles filled by `raw`)."""
    return {"role": role, "subrole": subrole, "title": title, "description": description, "value": value,
            "frame": list(frame) if frame is not None else None, "actions": list(actions), "enabled": enabled,
            "focused": focused, "children": list(children), "ancestor_roles": []}


def _fill_ancestors(n: dict, ancestors: list[str]) -> None:
    n["ancestor_roles"] = list(ancestors)
    for c in n["children"]:
        _fill_ancestors(c, ancestors + [n["role"]])


def raw(*children: dict, app="Calendar", bundle="com.apple.iCal", title="Calendar", subrole="AXStandardWindow",
        node_count=None, truncated=False, secs=0.05, fullscreen=False, root=None) -> dict:
    if root is None:
        root = node("AXWindow", title, frame=(0, 0, 900, 600), subrole=subrole, children=children)
    _fill_ancestors(root, [])
    count = node_count if node_count is not None else _count(root)
    return {"app": app, "bundle": bundle, "pid": 42, "title": root["title"], "fullscreen": fullscreen,
            "node_count": count, "truncated": truncated, "secs": secs, "root": root}


def _count(n: dict) -> int:
    return 1 + sum(_count(c) for c in n["children"])


def snap(*children: dict, screen=(1440, 900), **kw) -> Snapshot:
    return ax_snapshot(42, walk=lambda pid, **_k: raw(*children, **kw), screen=screen)


def labels(cands) -> list[str]:
    return [c.label for c in cands]


def toolbar() -> list[dict]:
    """Calendar's view switcher and navigation, as the real walk shows them."""
    return [
        node("AXButton", "Add Event", frame=(471, 98, 44, 52), actions=["AXPress"]),
        node("AXRadioButton", "Day", frame=(636, 106, 60, 36), actions=["AXPress"], value=0),
        node("AXRadioButton", "Week", frame=(696, 106, 60, 36), actions=["AXPress"], value=0),
        node("AXRadioButton", "Month", frame=(758, 106, 60, 36), actions=["AXPress"], value=1),
        node("AXRadioButton", "Year", frame=(818, 106, 60, 36), actions=["AXPress"], value=0),
        node("AXButton", "previous month", frame=(1086, 165, 27, 27), actions=["AXPress"]),
        node("AXButton", "Today", frame=(1115, 165, 68, 27), actions=["AXPress"]),
        node("AXButton", "next month", frame=(1185, 165, 27, 27), actions=["AXPress"]),
        node("AXButton", "Delete Event", frame=(300, 165, 60, 27), actions=["AXPress"]),
        node("AXStaticText", value="September 2026", frame=(300, 140, 200, 20)),
    ]


# ---- the sensitive-label table -----------------------------------------------------------

LONG_LABEL = "Keep every message from this sender in its current mailbox folder and " + "archive the rest now"
assert len(LONG_LABEL) == 90 and LONG_LABEL.index("archive") > 60

POSITIVES = ["Delete Event", "Don't Save", "Don’t Save", "Replace", "OK", "Remove filter", "Export as PDF",
             "Send", "Pay now", "Sign in", "Log Out", "Move to Trash", "Close Window", "Share…", "Continue",
             "Apply", "Yes", "Empty Trash", "System Settings…", "Install now", "Discard changes",
             "Allow", "Upload", "Report", "Block sender", "Accept", "Agree", "Quit Calendar", "Restart",
             LONG_LABEL,
             # 2026-09-21: every one of these read as harmless before the plan executor's audit
             "Place Order", "Place your order", "Order Now", "Confirm", "Book now", "Publish", "Transfer",
             "Clear History", "Cancel Subscription", "Merge pull request", "Format Disk", "Turn Off",
             "Turn On FileVault", "Approve", "Kill", "Add to Cart", "Mark as Spam", "Revert", "Sign Up",
             "Create Account"]
NEGATIVES = ["Postcode", "Composer", "Dispatcher", "Week", "Accent color", "Reading List", "Sidebar",
             "Downloads", "Today", "Month", "Search Wikipedia", "Notes", "Calls", "Applications",
             "Blocked", "Replay", "Recents", "Sender", "Payments history", "Trashcan icon", "Continuous",
             "Bookmarks", "Books", "Orderly", "Sort Order", "Forward", "Format", "Formatting", "Transfers",
             "Confirmation", "Skills", "Turntable", "Publisher", "Merge All Windows"]


def test_sensitive_label_table_positives() -> None:
    assert len(POSITIVES) >= 12
    wrong = [p for p in POSITIVES if not is_sensitive(p)]
    assert wrong == [], wrong
    assert is_sensitive(LONG_LABEL) and LONG_LABEL.casefold().index("archive") > 60
    assert SENSITIVE_LABEL.search("please delete this") is not None
    # fail-closed: a label that is not text counts as sensitive
    assert is_sensitive(None) and is_sensitive(42)


def test_sensitive_label_table_negatives() -> None:
    assert len(NEGATIVES) >= 12
    wrong = [n for n in NEGATIVES if is_sensitive(n)]
    assert wrong == [], wrong
    assert not is_sensitive("") and not is_sensitive("   ")


def test_sensitive_phrases_match_across_whitespace_and_case() -> None:
    assert is_sensitive("DON'T  SAVE") and is_sensitive("sign\tin") and is_sensitive("move  to   trash")
    assert is_sensitive("check out") and is_sensitive("Checkout") and is_sensitive("shut down")


# ---- derivation: labels, values, roles, tags ---------------------------------------------

def test_reading_order_ids_and_role_names() -> None:
    s = snap(node("AXButton", "B", frame=(50, 20, 10, 10)), node("AXButton", "A", frame=(10, 20, 10, 10)),
             node("AXButton", "C", frame=(0, 40, 10, 10)), node("AXPopUpButton", "P", frame=(0, 0, 10, 10)))
    assert labels(s.elements) == ["P", "A", "B", "C"]
    assert [c.id for c in s.elements] == ["0", "1", "2", "3"]
    assert s.elements[0].role == "pop up button" and s.elements[1].role == "button"
    assert s.app == "Calendar" and s.bundle == "com.apple.iCal" and s.title == "Calendar" and s.pid == 42
    assert s.seq >= 1 and s.node_count == 5 and not s.truncated and s.secs == 0.05
    assert s.get("2").label == "B" and s.get("9") is None


def test_values_selected_checked_expanded_and_text() -> None:
    s = snap(node("AXRadioButton", "Month", value=1), node("AXRadioButton", "Week", value=0, frame=(20, 0, 10, 10)),
             node("AXCheckBox", "Bold", value=1, frame=(40, 0, 10, 10)),
             node("AXCheckBox", "Wide", value=0, frame=(60, 0, 10, 10)),
             node("AXCheckBox", "Dark", value=1, subrole="AXToggle", frame=(80, 0, 10, 10)),
             node("AXDisclosureTriangle", "More", value=0, frame=(100, 0, 10, 10)),
             node("AXTextField", "Name", value="x" * 60, frame=(120, 0, 10, 10)),
             node("AXStaticText", value="A heading", frame=(140, 0, 10, 10)),
             node("AXRadioButton", "Tab 1", subrole="AXTabButton", value=1, frame=(160, 0, 10, 10)))
    by = {c.label: c for c in s.elements}
    assert by["Month"].value == "selected" and by["Week"].value == ""
    assert by["Bold"].value == "checked" and by["Wide"].value == "unchecked"
    assert by["Dark"].value == "checked" and by["Dark"].role == "toggle"
    assert by["More"].value == "collapsed" and by["More"].role == "disclosure triangle"
    assert by["Name"].value == "x" * 39 + "…" and by["Name"].editable and by["Name"].role == "text field"
    assert by["A heading"].role == "static text" and by["A heading"].value == ""
    assert by["Tab 1"].role == "tab" and by["Tab 1"].value == "selected"


def test_descendant_static_text_labels_rows_cells_and_links() -> None:
    s = snap(
        node("AXRow", frame=(0, 0, 100, 20), children=[
            node("AXCell", frame=(0, 0, 100, 20), children=[node("AXStaticText", value="Appearance", frame=(2, 2, 60, 16))])]),
        node("AXLink", frame=(0, 30, 100, 20), children=[node("AXStaticText", value="Go back", frame=(2, 32, 60, 16))]),
        node("AXCell", frame=(0, 60, 100, 20), children=[node("AXStaticText", value="Storage", frame=(2, 62, 60, 16))]),
        node("AXButton", frame=(0, 90, 20, 20), children=[node("AXImage", "gear", frame=(0, 90, 20, 20))]),
    )
    by = {(c.role, c.label) for c in s.elements}
    assert ("row", "Appearance") in by and ("link", "Go back") in by and ("cell", "Storage") in by
    assert ("button", "gear") in by
    # the cell that only repeats its row's label is the same control: only the row is offered
    assert ("cell", "Appearance") not in by
    assert ("static text", "Appearance") in by            # the text itself stays, for done_when checks


def test_description_is_a_label_and_untitled_buttons_are_not_candidates() -> None:
    s = snap(node("AXButton", description="Reload this page", actions=["AXPress"]),
             node("AXButton", frame=(20, 0, 16, 16), actions=["AXPress"]),         # a traffic light
             node("AXGroup", frame=(40, 0, 16, 16)))
    assert labels(s.elements) == ["Reload this page"]


def test_untitled_editable_field_is_a_candidate_and_secure_is_tagged() -> None:
    s = snap(node("AXTextField", frame=(0, 0, 100, 20)), node("AXTextField", "PIN", subrole="AXSecureTextField",
                                                                frame=(0, 30, 100, 20)),
             node("AXSecureTextField", "Password", frame=(0, 60, 100, 20)),
             node("AXTextField", "Find", subrole="AXSearchField", frame=(0, 90, 100, 20)),
             node("AXTextArea", "Message", frame=(0, 120, 100, 20)), node("AXComboBox", "Font", frame=(0, 150, 100, 20)))
    by = {c.label: c for c in s.elements}
    assert by[""].editable and by[""].role == "text field"
    assert by["PIN"].secure and by["Password"].secure and not by["Find"].secure
    assert by["Find"].role == "search field" and by["Message"].role == "text area" and by["Font"].role == "combo box"
    assert all(by[k].editable for k in ("Find", "Message", "Font"))


def test_content_and_dialog_tags() -> None:
    s = snap(
        node("AXToolbar", frame=(0, 0, 900, 40), children=[node("AXButton", "New Tab", actions=["AXPress"])]),
        node("AXScrollArea", frame=(0, 40, 900, 500), children=[
            node("AXWebArea", frame=(0, 40, 900, 500), children=[
                node("AXLink", "Search", frame=(10, 50, 50, 20)),
                node("AXButton", "Search page", frame=(10, 80, 50, 20), actions=["AXPress"])])]),
        node("AXSheet", frame=(100, 100, 400, 200), children=[
            node("AXStaticText", value="Do you want to save the changes?", frame=(110, 110, 300, 20)),
            node("AXButton", "Don't Save", frame=(110, 250, 80, 20), actions=["AXPress"]),
            node("AXButton", "Cancel", frame=(200, 250, 80, 20), actions=["AXPress"])]),
    )
    by = {c.label: c for c in s.elements}
    assert not by["New Tab"].in_content and by["Search"].in_content and by["Search page"].in_content
    assert by["Don't Save"].in_dialog and by["Cancel"].in_dialog and not by["New Tab"].in_dialog
    assert s.dialog_text == "Do you want to save the changes?"


def test_dialog_window_tags_everything_and_caps_the_text() -> None:
    long = "x" * 120
    s = snap(node("AXStaticText", value=long, frame=(0, 0, 300, 20)), node("AXButton", "OK", frame=(0, 30, 40, 20)),
             subrole="AXDialog")
    assert all(c.in_dialog for c in s.elements) and s.dialog_text == "x" * 80
    assert rank_candidates(s, "press ok", max_out=5)[0] == []


def test_focused_candidate_and_context_lines() -> None:
    s = snap(node("AXStaticText", value="September 2026", frame=(300, 140, 200, 20)),
             node("AXStaticText", value="Calendar", frame=(0, 0, 50, 10)),          # equals the title: skipped
             node("AXStaticText", value="September 2026", frame=(0, 300, 50, 10)),  # duplicate: skipped
             node("AXStaticText", value="y" * 61, frame=(0, 320, 50, 10)),          # too long: skipped
             node("AXRadioButton", "Month", value=1, focused=True, frame=(758, 106, 60, 36)),
             *[node("AXStaticText", value=f"Event {i}", frame=(0, 400 + i * 10, 50, 10)) for i in range(9)])
    assert s.focused is not None and s.focused.label == "Month"
    assert s.context_lines[:2] == ("title: Calendar", "focused: radio button Month")
    assert s.context_lines[2] == "September 2026"
    assert len(s.context_lines) <= 8 and "Calendar" not in s.context_lines[2:]
    assert "y" * 61 not in s.context_lines


def test_viewport_filter_and_bad_frames() -> None:
    s = snap(node("AXButton", "On", frame=(890, 590, 20, 20), actions=["AXPress"]),      # overlaps the corner
             node("AXButton", "Right", frame=(900, 10, 20, 20), actions=["AXPress"]),
             node("AXButton", "Below", frame=(10, 600, 20, 20), actions=["AXPress"]),
             node("AXButton", "Left", frame=(-30, 10, 20, 20), actions=["AXPress"]),
             node("AXButton", "Above", frame=(10, -20, 20, 20), actions=["AXPress"]),
             node("AXButton", "Flat", frame=(10, 10, 0, 20), actions=["AXPress"]),
             node("AXButton", "Nowhere", frame=None, actions=["AXPress"]),
             node("AXButton", "Odd", frame=(1, 2, 3), actions=["AXPress"]), screen=(900, 600))
    assert labels(s.elements) == ["On"]
    # no screen given: no viewport filter, only the frame sanity checks
    s = snap(node("AXButton", "Right", frame=(900, 10, 20, 20)), node("AXButton", "Flat", frame=(10, 10, 0, 20)),
             screen=None)
    assert labels(s.elements) == ["Right"]


def test_no_window_snapshot() -> None:
    s = ax_snapshot(7, walk=lambda pid, **_k: {"app": "Slack", "bundle": "com.tinyspeck.slackmacgap", "pid": 7,
                                                "title": "", "fullscreen": False, "node_count": 0, "truncated": False,
                                                "secs": 0.001, "root": None})
    assert s.elements == () and not s.truncated and s.node_count == 0 and s.app == "Slack"
    assert s.focused is None and s.dialog_text == "" and s.context_lines == ()
    assert rank_candidates(s, "anything", max_out=5) == ([], 0)


def test_truncation_flags_pass_through_and_disabled_is_kept_but_tagged() -> None:
    s = snap(node("AXButton", "Go", enabled=False, actions=["AXPress"]), truncated=True, node_count=800)
    assert s.truncated and s.node_count == 800
    assert s.elements[0].enabled is False
    assert rank_candidates(s, "go", max_out=5)[0] == []


def test_ax_snapshot_passes_the_budget_to_the_walk_and_counts_seq() -> None:
    seen: list[dict] = []

    def walk(pid, **kw):
        seen.append({"pid": pid, **kw})
        return raw()

    a = ax_snapshot(5, max_nodes=800, budget_secs=0.2, walk=walk)
    b = ax_snapshot(5, walk=walk)
    assert seen[0] == {"pid": 5, "max_nodes": 800, "budget_secs": 0.2}
    assert seen[1] == {"pid": 5, "max_nodes": 4000, "budget_secs": 0.5}
    assert b.seq == a.seq + 1


def test_fullscreen_flag_and_skipped_scrollbar_subtrees() -> None:
    s = snap(node("AXScrollBar", frame=(0, 0, 10, 100), children=[node("AXButton", "Up", actions=["AXPress"])]),
             node("AXButton", "Ok fine", frame=(20, 0, 10, 10), actions=["AXPress"]), fullscreen=True)
    assert s.fullscreen and labels(s.elements) == ["Ok fine"]


# ---- the walker's budget (fake clock, fake AX) -------------------------------------------

def test_walk_truncates_at_max_nodes_and_at_budget(monkeypatch) -> None:
    """Drive ax_walk with a fake ApplicationServices module: a wide tree, a clock we control."""
    import sys
    import types

    class El:
        def __init__(self, name: str, kids: list) -> None:
            self.name, self.kids = name, kids

    leaves = [El(f"leaf{i}", []) for i in range(50)]
    window = El("window", leaves)
    app = El("app", [window])
    cursor = {"t": 0.0}

    def clock() -> float:
        cursor["t"] += 0.01
        return cursor["t"]

    fake = types.SimpleNamespace(
        kAXValueAXErrorType=5, kAXValueCGPointType=1, kAXValueCGSizeType=2,
        AXUIElementCreateApplication=lambda pid: app,
        AXUIElementCopyAttributeValue=lambda el, attr, _n: (0, window) if attr == "AXFocusedWindow" else (
            0, False) if attr == "AXFullScreen" else (-25205, None),
        AXUIElementCopyMultipleAttributeValues=lambda el, attrs, opt, _n: (0, [
            "AXWindow" if el is window else "AXStaticText", "", el.name, "", None, None, None, True, False,
            el.kids]),                                   # static texts: the walker must ask for their actions
        AXUIElementCopyActionNames=lambda el, _n: (0, ["AXPress"]),
        AXValueGetType=lambda v: 0,
        AXValueGetValue=lambda v, t, _n: (False, None),
    )
    monkeypatch.setitem(sys.modules, "ApplicationServices", fake)
    monkeypatch.setitem(sys.modules, "objc", types.SimpleNamespace(lookUpClass=lambda name: type(name, (), {})))
    monkeypatch.setattr(ax, "_app_identity", lambda pid: ("Fake", "com.fake"))
    r = ax.ax_walk(1, max_nodes=10, budget_secs=100, clock=clock)
    assert r["truncated"] and r["node_count"] == 10 and len(r["root"]["children"]) == 9
    assert r["app"] == "Fake" and r["title"] == "window" and r["root"]["ancestor_roles"] == []
    assert r["root"]["children"][0]["ancestor_roles"] == ["AXWindow"]
    cursor["t"] = 0.0
    r = ax.ax_walk(1, max_nodes=1000, budget_secs=0.15, clock=clock)
    assert r["truncated"] and 1 < r["node_count"] < 51 and r["secs"] > 0.15
    cursor["t"] = 0.0
    r = ax.ax_walk(1, max_nodes=1000, budget_secs=100, clock=clock)
    assert not r["truncated"] and r["node_count"] == 51 and json.dumps(r)   # JSON-serialisable
    assert r["root"]["children"][0]["actions"] == ["AXPress"]
    assert snapshot_from_raw(r).elements == ()                   # the fake has no frames: nothing to click


# ---- rank_candidates: drops, scoring, cap ----------------------------------------------

def test_rank_drops_disabled_unlabelled_dialog_secure_page_links_and_sensitive() -> None:
    s = snap(
        node("AXButton", "Disabled", enabled=False, actions=["AXPress"]),
        node("AXButton", "", frame=(20, 0, 10, 10), actions=["AXPress"]),
        node("AXSheet", frame=(100, 100, 400, 200), children=[node("AXButton", "Cancel", actions=["AXPress"])]),
        node("AXTextField", "Password", subrole="AXSecureTextField", frame=(0, 30, 100, 20)),
        node("AXWebArea", frame=(0, 60, 900, 400), children=[
            node("AXLink", "Search", frame=(10, 70, 50, 20)), node("AXImage", "Logo", frame=(10, 100, 50, 20), actions=["AXPress"]),
            node("AXButton", "Play", frame=(10, 130, 50, 20), actions=["AXPress"])]),
        node("AXButton", "Delete Event", frame=(0, 500, 60, 20), actions=["AXPress"]),
        node("AXButton", "Week", frame=(0, 530, 60, 20), actions=["AXPress"]),
    )
    kept, dropped = rank_candidates(s, "", max_out=20)
    assert labels(kept) == ["Play", "Week"] and dropped == 0
    kept, _ = rank_candidates(s, "", max_out=20, allow_page_links=True)
    assert labels(kept) == ["Play", "Week", "Search", "Logo"]
    kept, _ = rank_candidates(s, "", max_out=20, allow_sensitive=True)
    assert labels(kept) == ["Play", "Delete Event", "Week"]                 # reading order on equal scores
    kept, _ = rank_candidates(s, "", max_out=20, kinds="editable")
    assert kept == []                                       # the only field is secure
    with pytest.raises(ValueError):
        rank_candidates(s, "", max_out=5, kinds="anything")


def test_rank_scores_objective_tokens_over_role_prior_and_keeps_reading_order_on_ties() -> None:
    s = snap(*toolbar())
    kept, dropped = rank_candidates(s, "switch to week view", max_out=12)
    assert labels(kept)[0] == "Week"
    # ties keep reading order (y, then x): the buttons and radio buttons (prior 3) in their rows, then
    # "September 2026" (static text, prior 1) last
    assert labels(kept)[1:] == ["Add Event", "Day", "Month", "Year", "previous month", "Today", "next month"]
    assert "Delete Event" not in labels(kept) and dropped == 0
    assert "September 2026" not in labels(kept)              # static text without AXPress is not pressable
    # a distractor that shares a token with the goal ranks with the target: the model, not the code, decides
    kept, _ = rank_candidates(s, "go back one month", max_out=3)
    assert labels(kept) == ["Month", "previous month", "next month"]       # all +4 for "month", reading order
    assert describe(kept[1], verb="click") == "click button: previous month"


def test_rank_matches_tokens_in_values_and_caps_with_a_count() -> None:
    s = snap(*toolbar())
    kept, _ = rank_candidates(s, "the selected view", max_out=12)
    assert labels(kept)[0] == "Month"                       # "selected" is in its value
    kept, dropped = rank_candidates(s, "", max_out=3)
    assert len(kept) == 3 and dropped == 5
    kept, dropped = rank_candidates(s, "", max_out=0)
    assert kept == [] and dropped == 8


def test_objective_tokens_drop_stop_words_and_short_words() -> None:
    assert objective_tokens("Switch to the Week view") == ["week"]
    assert objective_tokens("go back a page, please") == ["back", "page"]
    assert objective_tokens("show the whole month") == ["whole", "month"]
    assert objective_tokens("add a new event") == ["add", "new", "event"]
    assert objective_tokens("month month MONTH") == ["month"]


def test_pressable_by_action_or_role() -> None:
    s = snap(node("AXStaticText", value="Lunch", actions=["AXPress"]),
             node("AXStaticText", value="Header", frame=(0, 20, 10, 10)),
             node("AXRow", "Wi-Fi", frame=(0, 40, 10, 10)), node("AXSlider", "Volume", frame=(0, 60, 10, 10)),
             node("AXImage", "Cover", frame=(0, 80, 10, 10)))
    by = {c.label: c for c in s.elements}
    assert is_pressable(by["Lunch"]) and not is_pressable(by["Header"]) and is_pressable(by["Wi-Fi"])
    assert not is_pressable(by["Volume"]) and not is_pressable(by["Cover"])
    assert labels(pressable(s)) == ["Lunch", "Wi-Fi"]


# ---- describe / context_summary --------------------------------------------------------

def _cand(**kw) -> Candidate:
    base = dict(id="1", role="button", label="Today", value="", frame=(1, 2, 3, 4), actions=("AXPress",),
                enabled=True, app="Calendar", in_content=False, in_dialog=False, secure=False, editable=False)
    base.update(kw)
    return Candidate(**base)


def test_describe_strips_urls_collapses_space_and_caps() -> None:
    assert describe(_cand(role="radio button", label="Week", value="selected"), verb="click") == \
        "click radio button: Week (selected)"
    assert describe(_cand(role="link", label="Visit  https://en.wikipedia.org/wiki/Foo  now"), verb="click") == \
        "click link: Visit now"
    assert describe(_cand(role="link", label="www.example.com"), verb="click") == "click link"
    long = "Add AppleCare Coverage, There are 52 days left to protect your AirPods against accidental damage."
    d = describe(_cand(role="row", label=long), verb="click", max_chars=40)
    assert d == "click row: Add AppleCare Coverage, There are 52 da…" and len(d) <= 11 + 40
    assert describe(_cand(role="text field", label="Search", value="x" * 40), verb="type into") == \
        "type into text field: Search (" + "x" * 23 + "…)"


def test_context_summary_lists_title_focused_dialog_and_texts_capped() -> None:
    s = snap(node("AXStaticText", value="September 2026", frame=(300, 140, 200, 20)),
             node("AXRadioButton", "Month", value=1, focused=True, frame=(758, 106, 60, 36)),
             node("AXStaticText", value="Labor Day", frame=(0, 300, 50, 10)))
    assert context_summary(s) == ("app: Calendar; title: Calendar; focused: radio button Month; "
                                  "text: September 2026 | Labor Day")
    assert context_summary(s, max_chars=30) == "app: Calendar; title: Calenda…"
    d = snap(node("AXSheet", frame=(0, 0, 400, 200), children=[
        node("AXStaticText", value="Save changes?", frame=(10, 10, 100, 20))]))
    assert "dialog: Save changes?" in context_summary(d)


# ---- dict round trip and raw re-derivation -----------------------------------------------

def test_to_dict_from_dict_round_trip_is_json_safe() -> None:
    s = snap(*toolbar(), node("AXRadioButton", "Month", value=1, focused=True, frame=(0, 0, 10, 10)))
    d = json.loads(json.dumps(s.to_dict()))
    back = snapshot_from_dict(d)
    assert back == s and back.focused == s.focused and back.elements[3].frame == s.elements[3].frame
    assert isinstance(back.elements[0].actions, tuple) and isinstance(back.context_lines, tuple)


def test_snapshot_from_raw_is_deterministic_and_matches_ax_snapshot() -> None:
    r = raw(*toolbar())
    a = snapshot_from_raw(r, seq=3, screen=(900, 600))
    b = snapshot_from_raw(json.loads(json.dumps(r)), seq=3, screen=(900, 600))
    assert a == b and a.seq == 3
    c = ax_snapshot(42, walk=lambda pid, **_k: r, screen=(900, 600))
    assert c.elements == a.elements and c.context_lines == a.context_lines and c.focused == a.focused
    # the raw dump's own title wins over the root's when both are present
    r["title"] = "Other"
    assert snapshot_from_raw(r).title == "Other"


def test_candidate_from_dict_tolerates_missing_optional_fields() -> None:
    c = Candidate.from_dict({"id": 3, "role": "button", "frame": [1, 2, 3, 4]})
    assert c.id == "3" and c.label == "" and c.actions == () and c.enabled and not c.secure and c.frame == (1, 2, 3, 4)
