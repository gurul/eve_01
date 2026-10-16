"""Site names that speech recognition splits ("git hub", "you tube") are joined before a request is routed.

browser_intent.join_site_names is the joiner and its site table; task_router.classify runs it on every request,
so a search the router builds carries the site's real name.
"""

from __future__ import annotations

import pytest

from eve_bridge import browser_intent as bi
from eve_bridge import task_router as tr


@pytest.mark.parametrize("said, joined", [
    ("open git hub", "open GitHub"),
    ("search you tube for lofi beats", "search YouTube for lofi beats"),
    ("search duck duck go for privacy tools", "search DuckDuckGo for privacy tools"),
    ("go to stack overflow", "go to Stack Overflow"),
    ("open wiki pedia", "open Wikipedia"),
    ("what's new on red dit", "what's new on Reddit"),
    ("search e bay for a used camera", "search eBay for a used camera"),
    ("open linked in", "open LinkedIn"),
    ("put something on net flix", "put something on Netflix"),
    ("open hacker news", "open Hacker News"),
    ("Open Git Hub and search for laya", "Open GitHub and search for laya"),
    ("open YOU TUBE", "open YouTube"),
])
def test_split_site_names_are_joined(said: str, joined: str) -> None:
    assert bi.join_site_names(said) == joined


@pytest.mark.parametrize("said", [
    "do you know the way",
    "meet me at the hub",
    "send an e mail to the team",
    "check my net income",
    "the red dot on the map",
    "the doc I linked in slack",
    "I hit a stack overflow error in my recursion",
    "a duck went for a swim",
    "news about hackers",
    "",
])
def test_ordinary_words_are_not_joined(said: str) -> None:
    assert bi.join_site_names(said) == said


def test_hacker_news_stays_two_words_but_maps_to_its_host() -> None:
    assert bi.site_for("Hacker News").host == "news.ycombinator.com"
    assert bi.site_for("hacker news").name == "Hacker News"


def test_the_table_maps_names_to_hosts_and_search_urls() -> None:
    for name, host in (("GitHub", "github.com"), ("YouTube", "youtube.com"), ("DuckDuckGo", "duckduckgo.com"),
                       ("Stack Overflow", "stackoverflow.com"), ("Wikipedia", "wikipedia.org"),
                       ("Reddit", "reddit.com"), ("eBay", "ebay.com"), ("LinkedIn", "linkedin.com"),
                       ("Netflix", "netflix.com")):
        site = bi.site_for(name)
        assert site is not None and site.host == host, name
    assert bi.site_for("youtube").search_url("lofi beats") == "https://www.youtube.com/results?search_query=lofi+beats"
    assert bi.site_for("GitHub").search_url("a&b") == "https://github.com/search?q=a%26b&type=repositories"
    assert bi.site_for("Wikipedia").query_key == "search"
    assert bi.site_for("myspace") is None


def test_mentioned_site_is_found_after_joining() -> None:
    assert bi.mentioned_site("search you tube for cats").host == "youtube.com"
    assert bi.mentioned_site("go to stack overflow").host == "stackoverflow.com"
    assert bi.mentioned_site("I hit a stack overflow error") is None
    assert bi.mentioned_site("set a timer") is None


def test_the_router_sees_joined_names() -> None:
    p = tr.classify("search for git hub copilot pricing", apps=["Safari"])
    assert p.kind == "search" and p.query == "GitHub copilot pricing"
    p = tr.classify("look up you tube premium plans", apps=["Safari"])
    assert p.kind == "search" and p.query == "YouTube premium plans"
