"""Account-changing page controls need the human's yes: a short "upvote" / "Like" / "Follow" / "Star" /
"hide" / "flag" / "Save" link in eve's signed-in browser, or a link whose endpoint is /vote, /follow, /star,
/like, /flag or /hide, goes the same way as a sensitive label. Navigation that only reads ("Saved articles",
a "Follow-up" heading, "Hide sidebar", "Stories you might like") stays a plain click, and the desktop lane's
"Save" stays unasked (confirm only dangerous acts)."""

from __future__ import annotations

import pytest

from eve_bridge.ax_candidates import Candidate, Snapshot, is_account_change, needs_confirmation, rank_candidates
from eve_bridge.browser_lane import snapshot_from_page


def page(*items: tuple[str, str, str]) -> Snapshot:
    """A browser snapshot from (role, name, href) rows, the shape the page collector returns."""
    elements = [{"id": str(i), "role": role, "name": name, "href": href, "x": 10, "y": 20 * i, "w": 40, "h": 16}
                for i, (role, name, href) in enumerate(items)]
    return snapshot_from_page({"elements": elements, "title": "Hacker News", "url": "https://news.ycombinator.com/"}, seq=1)


def desktop(label: str, *, app: str = "TextEdit", in_content: bool = False) -> Candidate:
    return Candidate(id="1", role="button", label=label, value="", frame=(0, 0, 40, 20), actions=("AXPress",),
                     enabled=True, app=app, in_content=in_content, in_dialog=False, secure=False, editable=False)


ACCOUNT_LABELS = [
    "upvote", "downvote", "Like", "Unlike", "Follow", "Unfollow", "Star", "Unstar", "hide", "flag", "Report",
    "Save", "Subscribe", "▲ upvote", "Like 12", "Follow @jane", "Star 1.2k", "Save post", "flag comment",
    "Follow Jane Doe", "  UPVOTE  ", "♥ Like",
]
PLAIN_LABELS = [
    "Saved articles", "Follow-up", "Follow-up questions", "Follow up", "Hide sidebar", "Hide toolbar",
    "Hide details", "Show more", "Stories you might like", "People to follow this week", "Starred repositories",
    "Liked videos", "Things I like", "Stargazers", "Followers", "Hidden", "Saved", "Flagged", "Report card 2025 results",
    "Home", "",
]


@pytest.mark.parametrize("label", ACCOUNT_LABELS)
def test_short_account_change_labels_are_caught(label: str) -> None:
    assert is_account_change(label)


@pytest.mark.parametrize("label", PLAIN_LABELS)
def test_navigation_headings_and_ui_toggles_are_not(label: str) -> None:
    assert not is_account_change(label)


@pytest.mark.parametrize("href", [
    "https://news.ycombinator.com/vote?id=4123&how=up&auth=abc", "https://news.ycombinator.com/hide?id=4123&goto=news",
    "https://news.ycombinator.com/flag?id=4123", "https://github.com/owner/repo/star", "https://example.com/api/upvote",
    "https://example.com/users/jane/follow", "https://example.com/post/9/like", "https://example.com/like.php?id=9",
    "https://example.com/item?id=9&action=vote", "/vote?id=1&how=up",
])
def test_account_change_endpoints_are_caught_whatever_the_label(href: str) -> None:
    assert is_account_change("", href)
    assert is_account_change("story title that reads like anything", href)


@pytest.mark.parametrize("href", [
    "https://news.ycombinator.com/item?id=4123", "https://github.com/owner/repo/stargazers",
    "https://example.com/blog/follow-us", "https://en.wikipedia.org/wiki/Star", "https://example.com/search?q=like",
    "https://example.com/saved", "https://example.com/followers", "https://example.com/hidden-gems", "", "not a url",
    "https://example.com/votes/results",
])
def test_reading_links_are_not(href: str) -> None:
    assert not is_account_change("Read more", href)


def test_non_strings_fail_closed() -> None:
    assert is_account_change(None) and is_account_change(42)            # type: ignore[arg-type]
    assert is_account_change("Read more", None) is False                 # no href is not a href


def test_browser_candidates_need_confirmation_by_label_and_by_href() -> None:
    s = page(("link", "upvote", "https://news.ycombinator.com/vote?id=1&how=up"),
             ("link", "Show HN: a thing", "https://news.ycombinator.com/item?id=1"),
             ("link", "hide", "https://news.ycombinator.com/hide?id=1"),
             ("link", "", "https://news.ycombinator.com/vote?id=2&how=up"),    # the arrow has no name
             ("link", "Saved articles", "https://news.ycombinator.com/favorites"),
             ("button", "Delete account", ""))
    hrefs = {"0": "https://news.ycombinator.com/vote?id=1&how=up", "2": "https://news.ycombinator.com/hide?id=1",
             "3": "https://news.ycombinator.com/vote?id=2&how=up", "4": "https://news.ycombinator.com/favorites"}
    asked = [c.label for c in s.elements if needs_confirmation(c, hrefs.get(c.id, ""))]
    assert asked == ["upvote", "hide", "", "Delete account"]


def test_desktop_save_and_hide_stay_unasked_but_sensitive_verbs_still_ask() -> None:
    # confirm-only-dangerous: the Mac lane's "Save" / "Hide Toolbar" / Mail's "Flag" are not account changes.
    for label in ("Save", "Hide", "Flag", "Like", "Star"):
        assert not needs_confirmation(desktop(label)), label
    assert needs_confirmation(desktop("Delete"))
    assert needs_confirmation(desktop("Send"))
    # Web content under an AXWebArea in a desktop browser is a page: the same rule as eve's own browser.
    assert needs_confirmation(desktop("upvote", app="Safari", in_content=True))
    assert needs_confirmation(desktop("Like", app="Safari", in_content=True), "")


def test_rank_drops_account_changes_from_the_browser_menu_unless_allowed() -> None:
    s = page(("link", "upvote", ""), ("link", "Show HN: a thing", ""), ("link", "Follow", ""),
             ("link", "Saved articles", ""), ("button", "Hide sidebar", ""))
    kept, _ = rank_candidates(s, "", max_out=20)
    assert sorted(c.label for c in kept) == ["Hide sidebar", "Saved articles", "Show HN: a thing"]   # role prior orders
    kept, _ = rank_candidates(s, "", max_out=20, allow_sensitive=True)
    assert sorted(c.label for c in kept) == ["Follow", "Hide sidebar", "Saved articles", "Show HN: a thing", "upvote"]


# ---- the browser's own click: the effector asks for the same controls, href included ---------------------------

def test_the_browser_click_refuses_an_upvote_link_without_a_yes() -> None:
    from browser_fakes import FakePage, link

    from eve_bridge import browser_lane as bl

    page_ = FakePage("https://news.ycombinator.com/", "Hacker News", "Top stories",
                     [link("0", "Show HN: a thing", "https://news.ycombinator.com/item?id=1"),
                      link("1", "", "https://news.ycombinator.com/vote?id=1&how=up"),
                      link("2", "upvote", "https://news.ycombinator.com/x")])
    p = bl._Page(page_)
    snap = p.snapshot()
    by_id = {c.id: c for c in snap.elements}
    for eid in ("1", "2"):
        if eid in by_id:
            assert p.click_candidate(by_id[eid]).startswith("refused:"), eid
    assert page_.clicks == []
    assert p.click_candidate(by_id["0"]).startswith("clicked ")
    p.approve = " ".join(by_id["2"].label.split()).casefold()   # the human's yes covers that one click
    assert p.click_candidate(by_id["2"]).startswith("clicked ")
