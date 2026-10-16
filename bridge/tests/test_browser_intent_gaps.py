"""The code gaps the first Laya-delegate eval found, closed by phrase family rather than by example (G16).

- the site table covers the common sites (about fifty), each with a search address where the site has one;
- "search the web for", "look online for", "web search" and friends are read as queries, and polite filler is
  trimmed from both ends of a query;
- a result number with no noun ("open number two", "play the sixth") is read, but an ordinal inside a query is not;
- the spoken navigation verbs Laya reads poorly ("go to", "pull up", "click on") are shown to it as "open";
- the shared engine unloads Laya after an idle period (EVE_BROWSER_INTENT_IDLE_S) and reloads it lazily, and it
  no longer asks laya-mlx for a prompt cache that every new request would miss.
"""

from __future__ import annotations

import sys
import time
import types
from pathlib import Path
from urllib.parse import parse_qsl, urlsplit

import pytest

from eve_bridge import browser_intent as bi

# ---- the site table ------------------------------------------------------------------------------------------

@pytest.mark.parametrize("said, host", [
    ("npm", "npmjs.com"), ("Spotify", "spotify.com"), ("IKEA", "ikea.com"), ("Bing", "bing.com"),
    ("BBC", "bbc.co.uk"), ("IMDb", "imdb.com"), ("Twitter", "x.com"), ("Facebook", "facebook.com"),
    ("Instagram", "instagram.com"), ("TikTok", "tiktok.com"), ("Pinterest", "pinterest.com"),
    ("Twitch", "twitch.tv"), ("Yahoo", "yahoo.com"), ("Etsy", "etsy.com"), ("Walmart", "walmart.com"),
    ("Target", "target.com"), ("Best Buy", "bestbuy.com"), ("Craigslist", "craigslist.org"),
    ("Zillow", "zillow.com"), ("Yelp", "yelp.com"), ("Tripadvisor", "tripadvisor.com"),
    ("Airbnb", "airbnb.com"), ("Booking.com", "booking.com"), ("Expedia", "expedia.com"),
    ("Quora", "quora.com"), ("Medium", "medium.com"), ("PyPI", "pypi.org"), ("Hugging Face", "huggingface.co"),
    ("arXiv", "arxiv.org"), ("CNN", "cnn.com"), ("New York Times", "nytimes.com"), ("Apple", "apple.com"),
    ("Steam", "steampowered.com"), ("SoundCloud", "soundcloud.com"), ("Vimeo", "vimeo.com"),
    ("Goodreads", "goodreads.com"), ("Wolfram Alpha", "wolframalpha.com"), ("Indeed", "indeed.com"),
    ("Glassdoor", "glassdoor.com"), ("Home Depot", "homedepot.com"), ("Costco", "costco.com"),
    ("ESPN", "espn.com"), ("Gmail", "mail.google.com"), ("Google Maps", "maps.google.com"),
])
def test_common_sites_are_in_the_table(said: str, host: str) -> None:
    site = bi.site_for(said)
    assert site is not None and site.host == host, said
    assert site.home.startswith("https://")


def test_the_table_is_broad_and_consistent() -> None:
    assert len(bi.SITES) >= 40
    hosts = [s.host for s in bi.SITES]
    assert len(hosts) == len(set(hosts))
    names = [s.name.casefold() for s in bi.SITES]
    assert len(names) == len(set(names))
    for s in bi.SITES:
        if not s.search_template:
            assert not s.query_key, s.name
            continue
        url = s.search_url("blue shoes & socks")
        parts = urlsplit(url)
        assert parts.scheme == "https" and parts.hostname, s.name
        if s.query_key:
            assert ("blue shoes & socks" in dict(parse_qsl(parts.query)).values()), (s.name, url)
        else:
            # a path search: the query is encoded into the path, spaces as %20, never a bare "&"
            assert "blue%20shoes%20%26%20socks" in parts.path, (s.name, url)


def test_short_or_shared_host_labels_are_not_names() -> None:
    assert bi.site_for("x") is None           # x.com is Twitter's host, but "x" names nothing
    assert bi.site_for("mail") is None        # mail.google.com
    assert bi.site_for("news") is None        # news.ycombinator.com
    assert bi.site_for("google").host == "google.com"


@pytest.mark.parametrize("said, host", [
    ("pull up imdb", "imdb.com"),
    ("search spotify for jazz piano", "spotify.com"),
    ("look for bookshelves on ikea", "ikea.com"),
    ("open the bbc", "bbc.co.uk"),
    ("search n p m for left pad", "npmjs.com"),
    ("go to target", "target.com"),
    ("is it in stock at target", "target.com"),
    ("open medium.com", "medium.com"),
    ("browse air bnb for cabins", "airbnb.com"),
    ("visit sound cloud", "soundcloud.com"),
    ("search face book for my old school", "facebook.com"),
])
def test_new_sites_are_found_in_requests(said: str, host: str) -> None:
    site = bi.mentioned_site(said)
    assert site is not None and site.host == host, said


@pytest.mark.parametrize("said", [
    "what's my target heart rate",
    "cook the steak on medium heat",
    "find me an apple pie recipe",
    "let off some steam",
    "indeed it is",
    "I need to make a booking for dinner",
    "that was the best buy I made all year",
    "he gave me a twitch of a smile",
])
def test_everyday_words_are_not_sites(said: str) -> None:
    assert bi.mentioned_site(said) is None, said


# ---- queries ------------------------------------------------------------------------------------------------

@pytest.mark.parametrize("said, query", [
    ("search the web for cheap flights to lisbon", "cheap flights to lisbon"),
    ("search the internet for vegan lasagna", "vegan lasagna"),
    ("search online for standing desks", "standing desks"),
    ("look online for a used bike", "used bike"),
    ("look on the internet for jazz clubs in berlin", "jazz clubs in berlin"),
    ("web search mechanical keyboards", "mechanical keyboards"),
    ("do a web search for solar panel prices", "solar panel prices"),
    ("can you do a quick search for tide times", "tide times"),
    ("google best ramen in tokyo", "best ramen in tokyo"),
    ("search youtube lofi beats", "lofi beats"),
    ("search for weather in paris on bing", "weather in paris"),
])
def test_query_phrase_families(said: str, query: str) -> None:
    assert bi.literal_arguments(said).query == query, said


@pytest.mark.parametrize("said, query", [
    ("please search for hiking boots please", "hiking boots"),
    ("search for the history of rome", "history of rome"),
    ("look up ramen near me for me", "ramen near me"),
    ("search for pasta recipes and tell me the best one", "pasta recipes"),
    ("can you look up the opening hours of the louvre real quick", "opening hours of the louvre"),
    ("search for flights to oslo so I can plan the trip", "flights to oslo"),
    ("um search for uh noise cancelling headphones", "noise cancelling headphones"),
    ("search for cats and dogs", "cats and dogs"),
])
def test_filler_is_trimmed_but_the_query_kept(said: str, query: str) -> None:
    assert bi.literal_arguments(said).query == query, said


def test_search_results_words_are_not_a_query() -> None:
    a = bi.literal_arguments("open the second search result")
    assert (a.query, a.n) == (None, 2)
    assert bi.literal_arguments("click the search bar").query is None


# ---- result numbers with no noun -----------------------------------------------------------------------------

@pytest.mark.parametrize("said, n", [
    ("open number two", 2),
    ("click number 3", 3),
    ("play the sixth", 6),
    ("go with the fourth please", 4),
    ("pick the second and read it to me", 2),
    ("the seventh", 7),
    ("number five", 5),
    ("tap on the third", 3),
])
def test_result_numbers_without_a_noun(said: str, n: int) -> None:
    assert bi.literal_arguments(said).n == n, said


@pytest.mark.parametrize("said", [
    "search for the first moon landing",
    "the second half of the game was great",
    "what's the number two song this week",
    "remind me on the third",
    "who was the sixth president",
])
def test_an_ordinal_that_is_not_a_result_number(said: str) -> None:
    assert bi.literal_arguments(said).n is None, said


def test_an_ordinal_inside_a_query_keeps_the_search() -> None:
    a = bi.literal_arguments("search for the first moon landing")
    assert (a.query, a.n) == ("first moon landing", None)
    assert set(bi.offered(a)) == {"search", "open_result", "unsupported"}


# ---- how Laya is asked ----------------------------------------------------------------------------------------

class Recorder:
    def __init__(self, key: str) -> None:
        self.key = key
        self.goals: list[str] = []

    def ask(self, goal: str, question: dict, state: dict) -> dict:
        self.goals.append(goal)
        assert question["instructions"]["goal"] == goal
        options = list(question["criteria"])
        probs = {k: (0.9 if k == self.key else 0.1 / (len(options) - 1)) for k in options}
        return {"type": "choice", "choice": self.key, "probabilities": probs}


@pytest.mark.parametrize("said, goal", [
    ("um go to sound cloud", "open SoundCloud"),
    ("pull up yelp", "open yelp"),
    ("take me to git hub", "open GitHub"),
    ("navigate to the bbc", "open the bbc"),
    ("bring up reddit please", "open reddit please"),
    ("click on the fifth video", "open the fifth video"),
    ("uh go into the second one", "open the second one"),
    ("tap number three", "open number three"),
    ("play the second", "open the second"),
    ("watch the third one please", "open the third one please"),
    ("the sixth", "open the sixth"),
    ("number two", "open number two"),
])
def test_navigation_verbs_reach_laya_as_open(said: str, goal: str) -> None:
    rec = Recorder("unsupported")
    bi.expected_outcome(said, engine=rec, environ={})
    assert rec.goals == [goal]


@pytest.mark.parametrize("said", [
    "pull up netflix and log into my account",
    "take me to GitHub and star laya",
    "click on the second link, then copy it",
    "go to youtube settings then turn off autoplay",
])
def test_a_second_step_reaches_laya_as_said(said: str) -> None:
    # Laya reads "open X and ..." as plain open_site, so a request with a second step keeps its own verb.
    rec = Recorder("unsupported")
    bi.expected_outcome(said, engine=rec, environ={})
    assert rec.goals == [bi.join_site_names(said)]


def test_a_second_step_is_not_part_of_the_query() -> None:
    assert bi.literal_arguments("search youtube for cats and log in").query == "cats"
    assert bi.literal_arguments("search reddit for keyboards and subscribe to the top one").query == "keyboards"


def test_other_verbs_reach_laya_unchanged() -> None:
    rec = Recorder("search")
    r = bi.expected_outcome("search the web for cheap flights", engine=rec, environ={})
    assert rec.goals == ["search the web for cheap flights"]
    assert r is not None and (r.kind, r.query, r.site_host) == ("search", "cheap flights", None)


def test_a_bare_result_number_offers_only_open_result() -> None:
    a = bi.literal_arguments("open number two")
    assert set(bi.offered(a)) == {"open_result", "unsupported"}
    r = bi.expected_outcome("open number two", engine=Recorder("open_result"), environ={})
    assert r is not None and (r.kind, r.n) == ("open_result", 2)


# ---- Laya is unloaded after an idle period and reloaded lazily ---------------------------------------------------

class _Agent:
    cfg = {"max_len": 512, "head_max_len": 192}

    def predict(self, state, questions):
        (qid, q), = questions.items()
        probs = {k: (0.94 if k == "search" else 0.06 / (len(q["criteria"]) - 1)) for k in q["criteria"]}
        return {"answers": {qid: {"type": "choice", "choice": "search", "probabilities": probs}}}


@pytest.fixture
def fake_mlx(monkeypatch: pytest.MonkeyPatch) -> dict:
    record: dict = {"loads": [], "cleared": 0}
    fake = types.ModuleType("laya_mlx")

    def load(*args, **kwargs):
        record["loads"].append(kwargs)
        return _Agent()
    fake.load = load
    mlx = types.ModuleType("mlx")
    core = types.ModuleType("mlx.core")
    core.set_cache_limit = lambda n: None

    def clear_cache():
        record["cleared"] += 1
    core.clear_cache = clear_cache
    mlx.core = core
    monkeypatch.setitem(sys.modules, "laya_mlx", fake)
    monkeypatch.setitem(sys.modules, "mlx", mlx)
    monkeypatch.setitem(sys.modules, "mlx.core", core)
    monkeypatch.setattr(bi, "_cached_checkpoint", lambda: "/cached")
    monkeypatch.setattr(bi.LayaKindEngine, "_prefix_fits", lambda self, q: True)
    return record


def test_idle_seconds_is_env_tunable() -> None:
    assert bi.idle_seconds({}) == 600
    assert bi.idle_seconds({"EVE_BROWSER_INTENT_IDLE_S": "30"}) == 30
    assert bi.idle_seconds({"EVE_BROWSER_INTENT_IDLE_S": "0"}) == 0      # 0 keeps Laya loaded
    assert bi.idle_seconds({"EVE_BROWSER_INTENT_IDLE_S": "soon"}) == 600


def test_unloads_after_idle_and_reloads_lazily(fake_mlx: dict) -> None:
    now = [1000.0]
    engine = bi.LayaKindEngine(idle_s=600, clock=lambda: now[0], timer=False)
    assert bi.expected_outcome("search for lofi beats", engine=engine, environ={}).kind == "search"
    assert engine.loaded and len(fake_mlx["loads"]) == 1
    now[0] += 599
    assert engine.unload_if_idle() is False and engine.loaded
    now[0] += 2
    assert engine.unload_if_idle() is True
    assert not engine.loaded and fake_mlx["cleared"] >= 1
    assert bi.expected_outcome("search for lofi beats", engine=engine, environ={}).kind == "search"
    assert engine.loaded and len(fake_mlx["loads"]) == 2


def test_use_resets_the_idle_clock(fake_mlx: dict) -> None:
    now = [0.0]
    engine = bi.LayaKindEngine(idle_s=600, clock=lambda: now[0], timer=False)
    bi.expected_outcome("search for lofi beats", engine=engine, environ={})
    now[0] = 500
    bi.expected_outcome("search for lofi beats", engine=engine, environ={})
    now[0] = 1000
    assert engine.unload_if_idle() is False and engine.loaded
    now[0] = 1101
    assert engine.unload_if_idle() is True


def test_zero_idle_never_unloads(fake_mlx: dict) -> None:
    now = [0.0]
    engine = bi.LayaKindEngine(idle_s=0, clock=lambda: now[0], timer=False)
    bi.expected_outcome("search for lofi beats", engine=engine, environ={})
    now[0] = 1e9
    assert engine.unload_if_idle() is False and engine.loaded


def test_the_idle_timer_really_unloads(fake_mlx: dict) -> None:
    engine = bi.LayaKindEngine(idle_s=0.05)
    assert bi.expected_outcome("search for lofi beats", engine=engine, environ={}) is not None
    deadline = time.monotonic() + 3
    while engine.loaded and time.monotonic() < deadline:
        time.sleep(0.02)
    assert not engine.loaded
    assert bi.expected_outcome("search for lofi beats", engine=engine, environ={}) is not None
    assert len(fake_mlx["loads"]) == 2
    engine.unload()


def test_no_prompt_cache_that_every_request_would_miss(fake_mlx: dict) -> None:
    engine = bi.LayaKindEngine(timer=False)
    bi.expected_outcome("search for lofi beats", engine=engine, environ={})
    assert fake_mlx["loads"][0].get("cache_prompts", False) is False


# ---- the real checkpoint, on phrasings outside the first eval ----------------------------------------------------

def _weights_cached() -> bool:
    try:
        import laya_mlx  # noqa: F401
    except Exception:
        return False
    path = bi._cached_checkpoint()
    return bool(path) and (Path(path) / bi.BROWSER_SUBFOLDER / "model.safetensors").exists()


@pytest.mark.skipif(not _weights_cached(), reason="laya-mlx or the cklxx/laya-browser checkpoint is not cached")
def test_the_real_checkpoint_on_the_closed_gaps() -> None:
    engine = bi.LayaKindEngine(timer=False)
    expected = {
        "pull up yelp": ("open_site", "yelp.com", None, None),
        "take me to sound cloud": ("open_site", "soundcloud.com", None, None),
        "uh click on the second video": ("open_result", None, None, 2),
        "go with number three": ("open_result", None, None, 3),
        "search the internet for vegan lasagna": ("search", None, "vegan lasagna", None),
        "look online for a used bike": ("search", None, "used bike", None),
        "pull up netflix and log into my account": ("none", None, None, None),   # a second step stays visible
    }
    try:
        for request, want in expected.items():
            r = bi.expected_outcome(request, engine=engine, environ={})
            assert r is not None and (r.kind, r.site_host, r.query, r.n) == want, (request, r)
    finally:
        engine.unload()


@pytest.mark.parametrize("said, host, query", [
    ("browse air bnb for cabins in vermont", "airbnb.com", "cabins in vermont"),
    ("check ebay for a used camera", "ebay.com", "used camera"),
    ("find a floor lamp on ikea", "ikea.com", "floor lamp"),
    ("look for knitting patterns on etsy", "etsy.com", "knitting patterns"),
    ("shop for running shoes at target", "target.com", "running shoes"),
])
def test_a_site_search_said_with_another_verb(said: str, host: str, query: str) -> None:
    a = bi.literal_arguments(said)
    assert (a.site.host, a.query) == (host, query), said


@pytest.mark.parametrize("said", ["find my keys", "look for the remote", "check the oven", "browse my photos"])
def test_find_without_a_site_is_not_a_query(said: str) -> None:
    assert bi.literal_arguments(said).query is None, said
