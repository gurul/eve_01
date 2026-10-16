"""The url_query check accepts the searches a person would call right (plan_contract.py, review F2).

Before this the check wanted the request's query, character for character, on the named site or a general
search engine. That rejected good searches: a search typed into YouTube's own box when YouTube was already open
and no site was named, Wikipedia's redirect of an exact title to the article, a site the table does not know,
a query that kept "please" or "and tell me…", and Hacker News' rewrite of ?q= to ?query=. It still refuses a
different query, a dropped word that is not filler, the wrong site and a lookalike host.
"""

from __future__ import annotations

from typing import Optional

import pytest
from test_fast_lane import Effectors
from test_plan_success_guard import Page, go, outcome, raw

from eve_bridge import plan_contract as pc


def ok(url: str, host: Optional[str], query: str, here: str = "") -> bool:
    return pc.url_query_matches(url, pc.url_query_expect(host, query, here=here))


# ---- a search on the site already open ------------------------------------------------------------------


def test_no_site_named_accepts_a_search_on_the_page_already_open() -> None:
    url = "https://www.youtube.com/results?search_query=lofi+beats"
    assert ok(url, None, "lofi beats", here="youtube.com")
    assert ok(url, None, "lofi beats", here="www.youtube.com")
    assert not ok(url, None, "lofi beats")                                  # not open before: a site, not an engine
    assert not ok(url, None, "lofi beats", here="github.com")                # a different site was open
    assert ok("https://www.google.com/search?q=lofi+beats", None, "lofi beats", here="youtube.com")  # engines still
    # A site the table does not know, open already: its search under any usual query key.
    assert ok("https://www.etsy.com/search?q=blue+mugs", None, "blue mugs", here="etsy.com")
    assert not ok("https://www.etsy.com/search?q=red+mugs", None, "blue mugs", here="etsy.com")


def test_an_observed_page_that_is_not_a_hostname_is_ignored() -> None:
    assert pc.url_query_expect(None, "cats", here="about:blank").here == ""
    assert pc.url_query_expect(None, "cats", here="evil example").here == ""


def test_the_executor_reads_the_page_open_before_step_1() -> None:
    p = pc.parse_plan(raw({"kind": "open_url", "target": "https://www.youtube.com/results?search_query=lofi+beats"},
                          app="browser"), "search for lofi beats", intent=lambda r: outcome("search", None, "lofi beats"))
    page = Page(url="https://www.youtube.com/")
    r = go(p, page, Effectors(), page.desk, planned_for="browser")
    assert r.status == "complete", r.to_dict()


def test_the_plan_dict_carries_the_page_it_was_planned_on() -> None:
    # computer_agent sends {"here": <the outline's host>} beside "app", so a resume still knows it.
    p = pc.parse_plan(raw({"kind": "open_url", "target": "https://www.youtube.com/"}, app="browser",
                          here="www.youtube.com"), "search for lofi beats",
                      intent=lambda r: outcome("search", None, "lofi beats"))
    assert p.success is not None and p.success.here == "youtube.com"
    page = Page(url="https://www.youtube.com/results?search_query=lofi+beats")
    r = go(p, page, Effectors(), page.desk, planned_for="browser", start=1)  # resumed past the only step
    assert r.status == "complete", r.to_dict()


# ---- Wikipedia's exact-title redirect, Hacker News' rewrite ---------------------------------------------


@pytest.mark.parametrize("url, query, want", [
    ("https://en.wikipedia.org/wiki/Alan_Turing", "alan turing", True),
    ("https://en.wikipedia.org/wiki/Alan%20Turing", "Alan Turing", True),
    ("https://en.m.wikipedia.org/wiki/Alan_Turing#Early_life", "alan turing", True),
    ("https://en.wikipedia.org/wiki/Alan_Turing_(film)", "alan turing", False),
    ("https://en.wikipedia.org/wiki/Main_Page", "alan turing", False),
    ("https://en.wikipedia.org/wiki/Special:Search", "special search", False),
    ("https://en.wikipedia.org.evil.example/wiki/Alan_Turing", "alan turing", False),
])
def test_a_wikipedia_article_whose_title_is_the_query_is_the_search(url: str, query: str, want: bool) -> None:
    assert ok(url, "wikipedia.org", query) is want


def test_an_article_page_is_not_a_search_on_some_other_site() -> None:
    assert not ok("https://github.com/wiki/Alan_Turing", "github.com", "alan turing")


def test_hacker_news_rewrites_q_to_query() -> None:
    url = ("https://hn.algolia.com/?dateRange=all&page=0&prefix=false&query=laya%20browse&sort=byPopularity"
           "&type=story")
    assert ok(url, "news.ycombinator.com", "laya browse")
    assert ok("https://hn.algolia.com/?q=laya+browse", "news.ycombinator.com", "laya browse")
    assert not ok("https://www.google.com/search?query=laya+browse", "duckduckgo.com", "laya browse")


# ---- filler words ----------------------------------------------------------------------------------------


@pytest.mark.parametrize("query, url_q", [
    ("cats please", "cats"),
    ("please cats", "cats"),
    ("the beatles", "beatles"),
    ("beatles", "the beatles"),
    ("cats and tell me what you find", "cats"),
    ("cats, then show me the top one", "cats"),
    ("cats and let me know", "cats"),
    ("um cats for me", "cats"),
    ("Cats!", "cats"),
    ("alan turing's paper", "Alan Turing's paper"),
])
def test_filler_around_the_query_is_not_part_of_it(query: str, url_q: str) -> None:
    from urllib.parse import quote_plus

    assert ok(f"https://duckduckgo.com/?q={quote_plus(url_q)}", "duckduckgo.com", query)


@pytest.mark.parametrize("query, url_q", [
    ("cats and dogs", "cats"),             # a dropped word that is not filler
    ("cats", "cats and dogs"),             # an added word
    ("red shoes", "shoes"),
    ("vitamin a", "vitamin"),              # an article that is part of the query, not before it
    ("cats", "dogs"),
    ("show and tell ideas", "ideas"),
])
def test_a_different_query_is_still_refused(query: str, url_q: str) -> None:
    from urllib.parse import quote_plus

    assert not ok(f"https://duckduckgo.com/?q={quote_plus(url_q)}", "duckduckgo.com", query)


# ---- a site the table does not know --------------------------------------------------------------------


def test_a_site_named_at_the_end_of_the_query_is_where_it_was_searched() -> None:
    # "search for blue mugs on etsy": code reads no site (etsy is not in the table) and the query keeps "on etsy".
    assert ok("https://www.etsy.com/search?q=blue+mugs", None, "blue mugs on etsy")
    assert ok("https://www.google.com/search?q=blue+mugs+on+etsy", None, "blue mugs on etsy")
    assert not ok("https://www.google.com/search?q=blue+mugs", None, "blue mugs on etsy")
    assert not ok("https://www.etsy.com.evil.example/search?q=blue+mugs", None, "blue mugs on etsy")
    assert not ok("https://www.amazon.com/s?k=blue+mugs", None, "blue mugs on etsy")
    assert not ok("https://www.etsy.com/search?q=red+mugs", None, "blue mugs on etsy")


def test_the_old_refusals_still_hold() -> None:
    assert not ok("https://www.google.com/search?q=cats", "duckduckgo.com", "cats")
    assert not ok("https://duckduckgo.com.evil.example/?q=cats", "duckduckgo.com", "cats")
    assert not ok("https://duckduckgo.com/?x=cats", "duckduckgo.com", "cats")
    assert not ok("https://www.youtube.com/results?search_query=cats", None, "cats")
    assert not ok("about:blank", None, "cats")


def test_the_agent_sends_the_host_of_the_page_it_planned_on(tmp_path) -> None:
    import asyncio

    from test_agent_browser_lane import WEB_DONE, WEB_PLAN, FakeLane, _cfg
    from test_agent_plan_once import PlanWorker, _plan_response
    from test_computer_agent import FakeClient, _agent

    class OnYouTube(FakeLane):
        async def outline(self):
            return {**await super().outline(), "url": "https://www.youtube.com/watch?v=x"}

    lane = OnYouTube([WEB_DONE])
    a, _ = _agent(FakeClient([_plan_response(WEB_PLAN)]), PlanWorker([]), tmp_path, config=_cfg(tmp_path),
                  browser=lane)
    asyncio.run(a.run("open google news and show me the top headline"))
    assert lane.runs and lane.runs[0]["plan"]["here"] == "youtube.com", lane.runs


def test_no_site_named_accepts_every_general_search_engine_in_the_table() -> None:
    # The site table lists Bing and Yahoo as engines (browser_intent); a plain "search for" may land on either.
    assert ok("https://www.bing.com/search?q=lofi+beats", None, "lofi beats")
    assert ok("https://search.yahoo.com/search?p=lofi+beats", None, "lofi beats")
    assert not ok("https://www.bing.com/search?q=lofi+beats", "duckduckgo.com", "lofi beats")
    assert not ok("https://bing.com.evil.example/search?q=lofi+beats", None, "lofi beats")
