"""The account-change rule asks only for controls shaped like an account verb (GATES.md G13).

A result title, a shop label or a form button that merely starts with "Star", "Like", "Save", "Hide", "Flag" or
"Report" is a plain click: "Star Wars", "Like New", "Save 20%", "Hide and Seek", "Flag of France", "Save changes".
What still asks is verb-shaped: the bare verb, the verb and a count ("Like 12"), the verb and what it acts on
("Save post", "Follow @jane"). A link that goes to a page named after its own label is a title, not an action.
A native Electron app (Slack, Spotify) renders its chrome in an AXWebArea; that is the app, not a web page, so
its "Follow" stays unasked as the Mac lane's own "Save" does. And a link whose only tell is its href (HN's
unnamed vote arrow) reaches the lane's confirmation question, because the snapshot carries the href to the gate.
"""

from __future__ import annotations

import pytest

from eve_bridge.ax_candidates import (
    Candidate,
    Snapshot,
    is_account_change,
    needs_confirmation,
    rank_candidates,
)
from eve_bridge.fast_lane import Choice, MenuItem, Thresholds, gate, run_delegate


def cand(label: str, *, role: str = "link", app: str = "browser", in_content: bool = False,
         cid: str = "1") -> Candidate:
    return Candidate(id=cid, role=role, label=label, value="", frame=(10, 20 * int(cid), 60, 16), actions=("AXPress",),
                     enabled=True, app=app, in_content=in_content, in_dialog=False, secure=False, editable=False)


# ---- titles and shop labels are plain ---------------------------------------------------------------------------

PLAIN = [
    "Star Wars", "Star Wars: Andor", "Like New", "Save $5", "Save 20%", "Save Your Tears", "Hide and Seek",
    "Flag of France", "Save changes", "Save for later", "Like a Rolling Stone", "Star Trek", "Follow the Money",
    "Save 20 percent", "Hide & Seek",
]
# Not an account change either; "report" is also on main's sensitive word list (is_sensitive), which asks for it in
# every app and is outside this rule.
REPORT_TITLES = ["Report a problem", "Report: inflation cools", "Report card 2025"]


@pytest.mark.parametrize("label", PLAIN)
def test_titles_and_shop_labels_are_not_account_changes(label: str) -> None:
    assert not is_account_change(label)
    assert not is_account_change(label, role="button")
    assert not needs_confirmation(cand(label))                               # a link in eve's browser
    assert not needs_confirmation(cand(label, role="button"))                # a button in eve's browser
    assert not needs_confirmation(cand(label, app="Safari", in_content=True))


@pytest.mark.parametrize("label", REPORT_TITLES)
def test_report_titles_are_not_account_changes(label: str) -> None:
    assert not is_account_change(label) and not is_account_change(label, role="button")


VERB_SHAPED = [
    "upvote", "Like", "Follow", "Star", "Save", "hide", "flag", "Report", "Subscribe", "▲ upvote", "♥ Like",
    "Like 12", "Star 1.2k", "Follow 3,401", "Save post", "Save this", "flag comment", "Follow @jane", "Like this post",
    "Unfollow @jane_doe", "Report this", "Report post", "Upvote 12",
]


@pytest.mark.parametrize("label", VERB_SHAPED)
def test_verb_shaped_labels_still_ask(label: str) -> None:
    assert is_account_change(label)
    assert needs_confirmation(cand(label, role="button"))
    assert needs_confirmation(cand(label, app="Google Chrome", in_content=True))


def test_follow_a_named_person_asks_on_a_button_only() -> None:
    # "Follow Jane Doe" is the aria-label of a follow button; as a link it reads as a title ("Follow the Money").
    assert needs_confirmation(cand("Follow Jane Doe", role="button"))
    assert not needs_confirmation(cand("Follow Jane Doe", role="link"))
    assert not needs_confirmation(cand("Star Wars", role="button"))          # only follow-type verbs take a person


def test_a_link_titled_after_its_own_page_is_a_title() -> None:
    assert not is_account_change("Like", "https://en.wikipedia.org/wiki/Like", role="link")
    assert not is_account_change("Hide", "https://www.imdb.com/find/?q=hide", role="link")
    assert not is_account_change("Star", "https://en.wikipedia.org/wiki/Star", role="link")
    assert not is_account_change("Save Your Tears", "https://music.example.com/song/save-your-tears", role="link")
    # a verb link that goes somewhere else still asks, and an account endpoint always does
    assert is_account_change("upvote", "https://news.ycombinator.com/x", role="link")
    assert is_account_change("hide", "https://news.ycombinator.com/hide?id=4123&goto=news", role="link")
    assert is_account_change("Star", "https://github.com/owner/repo/star", role="link")


# ---- native Electron app chrome is the app, not a page --------------------------------------------------------

@pytest.mark.parametrize("app,label", [("Slack", "Save for later"), ("Slack", "Save"), ("Spotify", "Follow"),
                                       ("Spotify", "Like"), ("Discord", "Star"), ("Notion", "Follow")])
def test_electron_app_chrome_stays_unasked(app: str, label: str) -> None:
    assert not needs_confirmation(cand(label, role="button", app=app, in_content=True))


@pytest.mark.parametrize("app", ["Safari", "Google Chrome", "Arc", "Firefox", "Microsoft Edge", "Brave Browser"])
def test_web_content_in_a_real_browser_still_asks(app: str) -> None:
    assert needs_confirmation(cand("Follow", role="button", app=app, in_content=True))
    assert needs_confirmation(cand("", app=app, in_content=True), "https://news.ycombinator.com/vote?id=1&how=up")


def test_sensitive_labels_ask_in_every_app() -> None:
    assert needs_confirmation(cand("Delete message", role="button", app="Slack", in_content=True))
    assert needs_confirmation(cand("Send", role="button", app="Spotify", in_content=True))


# ---- F4: an href-only account link reaches the confirmation question --------------------------------------------

VOTE = "https://news.ycombinator.com/vote?id=1&how=up"


def hn_page(seq: int = 1) -> Snapshot:
    rows = [cand("Show HN: a thing", cid="0"), cand("", cid="1"), cand("vote arrow", cid="2"),
            cand("12 comments", cid="3")]
    return Snapshot(seq=seq, app="browser", bundle="eve.browser", title="Hacker News", pid=1, elements=tuple(rows),
                    context_lines=("title: Hacker News",), node_count=4,
                    _hrefs={"0": "https://news.ycombinator.com/item?id=1", "1": VOTE, "2": VOTE.replace("1&", "2&"),
                            "3": "https://news.ycombinator.com/item?id=1"})


def test_the_snapshot_carries_each_controls_href() -> None:
    s = hn_page()
    assert s.href("1") == VOTE and s.href("0").endswith("item?id=1") and s.href("9") == ""
    # a plain snapshot (fixtures, the Mac walk) has none, and an observed link destination is read when present
    bare = Snapshot(seq=1, app="browser", bundle="eve.browser", title="", pid=1, elements=(cand("★ 1.2k", cid="4"),),
                    context_lines=('link destination: {"id": "4", "url": "https://github.com/owner/repo/star"}',))
    assert bare.href("4") == "https://github.com/owner/repo/star" and bare.href("5") == ""
    assert needs_confirmation(bare.elements[0], bare.href("4"))


def test_rank_withholds_an_href_only_vote_link() -> None:
    kept, _ = rank_candidates(hn_page(), "", max_out=20)
    assert sorted(c.label for c in kept) == ["12 comments", "Show HN: a thing"]


def test_gate_asks_for_an_href_only_account_link() -> None:
    c = hn_page().get("2")
    assert c is not None
    item = MenuItem("1", "click", "link: vote arrow", c, href=VOTE)
    choice = Choice("1", p_top=1.0, margin=1.0, confidence=1.0, probabilities={"1": 1.0}, k=1)
    kw = dict(thresholds=Thresholds(), approve=None, approve_used=False, search_typed=False, dialog_open=False,
              reobserve_count=0)
    assert gate(choice, item, **kw) == ("confirm", "sensitive")
    assert gate(choice, MenuItem("1", "click", "link: vote arrow", c), **kw) == ("act", "")   # no href, no tell
    assert gate(choice, item, **{**kw, "approve": "vote arrow"}) == ("act", "")              # the human's yes


class _Senses:
    def __init__(self, s: Snapshot) -> None:
        self.s = s

    def snapshot(self) -> Snapshot:
        return self.s

    def text_visible(self, text: str) -> bool:
        return False

    def focused(self):
        return None

    def frontmost_pid(self) -> int:
        return 1

    def screen_changed(self):
        return True


class _Effectors:
    def __init__(self) -> None:
        self.clicks: list[str] = []

    def click_candidate(self, c: Candidate) -> str:
        self.clicks.append(c.label)
        return f"clicked {c.role} {c.label!r}"

    def settle(self, secs: float) -> float:
        return 0.0


def test_the_lane_confirms_an_href_only_vote_link_instead_of_clicking_it() -> None:
    eff = _Effectors()
    r = run_delegate("click the vote arrow", senses=_Senses(hn_page()), effectors=eff, decider=None,
                     decide="keyword", allow_page_links=True)
    assert r.status == "confirm" and r.line.startswith('confirm: "vote arrow"'), r.line
    assert eff.clicks == []
    # the same page, a reading link: clicked as before
    eff = _Effectors()
    r = run_delegate("open the comments", senses=_Senses(hn_page()), effectors=eff, decider=None, decide="keyword",
                     allow_page_links=True)
    assert eff.clicks == ["12 comments"], r.line


# ---- F8: the planner's outline marks what the lane will ask for, by the same rule ------------------------------

def test_the_outline_hint_uses_the_lanes_own_confirmation_rule(monkeypatch: pytest.MonkeyPatch) -> None:
    from types import SimpleNamespace

    from eve_bridge import ax_candidates
    from eve_bridge.desktop_helpers import Helpers

    rows = (cand("Follow", role="button", app="Safari", in_content=True, cid="0"),
            cand("Star Wars", role="link", app="Safari", in_content=True, cid="1"),
            cand("Delete", role="button", app="Safari", cid="2"),
            cand("Save", role="button", app="Safari", cid="3"))
    s = Snapshot(seq=1, app="Safari", bundle="com.apple.Safari", title="", pid=7, elements=rows)
    monkeypatch.setattr(ax_candidates, "ax_snapshot", lambda *a, **k: s)
    lines = Helpers.outline(SimpleNamespace(_size=lambda: (1440, 900)))["lines"]   # type: ignore[arg-type]
    asked = sorted(line.split(" (")[0] for line in lines if line.endswith("[asks the human first]"))
    assert asked == ["Delete", "Follow"], lines


# ---- F4 integration: eve's own browser snapshot carries the raw href ---------------------------------------------

def test_the_browser_lanes_snapshot_carries_query_bearing_hrefs_to_the_gate() -> None:
    from eve_bridge.browser_lane import snapshot_from_page

    raw = {"url": "https://news.ycombinator.com/", "title": "Hacker News", "elements": [
        {"id": 1, "role": "link", "name": "", "href": "https://news.ycombinator.com/vote?id=1&how=up&goto=news",
         "x": 5, "y": 40, "w": 10, "h": 10},
        {"id": 2, "role": "link", "name": "Show HN: a thing", "href": "https://example.com/thing",
         "x": 20, "y": 40, "w": 200, "h": 16},
        {"id": 3, "role": "button", "name": "Search", "x": 20, "y": 80, "w": 60, "h": 16},
    ]}
    snap = snapshot_from_page(raw, seq=1)
    assert snap.href("1") == "https://news.ycombinator.com/vote?id=1&how=up&goto=news"
    assert snap.href("3") == ""
    vote = next(c for c in snap.elements if c.id == "1")
    title = next(c for c in snap.elements if c.id == "2")
    assert needs_confirmation(vote, snap.href(vote.id))
    assert not needs_confirmation(title, snap.href(title.id))
