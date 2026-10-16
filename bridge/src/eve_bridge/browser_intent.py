"""What a browser request asks for, as one outcome: search, open a site, open a result, or none of these.

Two halves, split the way LayaBrowse splits them (github.com/cklxx/LayaBrowse, goal_engine.py):

* Code reads the literal arguments and nothing else: the site (from the table below, after the site names speech
  recognition splits are joined), the query (the rest of the clause after a search phrase: "search for", "look
  up", "search the web for", "web search", "search <site> for", ...) and a result number ("the first result",
  "open number two", "play the sixth"). Code never guesses a kind.
* Laya picks only the KIND, from that finite set, using the browser-trained checkpoint cklxx/laya-browser (v10s,
  pinned revision) through laya-mlx, on this Mac. It is asked the way LayaBrowse asks its outcome question: one
  Choice whose goal is the request, whose rules say "the outcome, not the next action", and whose options are the
  fixed outcome descriptions plus "unsupported". Its answer passes the same schema check and the same 0.50 / 0.10
  gate (top probability, margin over the runner-up) before anything uses it. The goal it reads is the request with
  disfluencies dropped and, in a one-step request, navigation verbs said as "open" (laya_goal).

``expected_outcome`` returns None — an abstention — when the switch is off, laya-mlx is missing, the checkpoint is
not cached or fails to load, the answer is malformed or under the gate, or Laya's kind cannot be carried by the
literal arguments (a search with no query, a site that was never named, a result with no number). A "none"
outcome is an answer: the request is not one of these three.

EVE_BROWSER_INTENT=laya|off (default laya). EVE_BROWSER_INTENT_IDLE_S (default 600): Laya is unloaded after that many
seconds with no question and loaded again on the next (0 keeps it loaded). Nothing leaves the Mac; Jev is not asked.
"""

from __future__ import annotations

import collections
import gc
import logging
import math
import os
import re
import threading
import time
from dataclasses import dataclass, field
from typing import Any, Mapping, Optional
from urllib.parse import quote, quote_plus

log = logging.getLogger(__name__)

# ---- the site table ----------------------------------------------------------------------------------------
#
# Hosts, homes, search URLs and query keys for GitHub, YouTube, Wikipedia, eBay and Google are LayaBrowse's
# (sites/*.json, MIT, Copyright (c) 2026 Aryan Bhujade); the rest are each site's own public search address.
# A site with no search address the URL can carry has only a home: it can be opened, never searched by code.


@dataclass(frozen=True)
class Site:
    name: str                       # the site's own spelling: "GitHub", "Stack Overflow"
    host: str
    home: str
    search_template: str = ""       # "{query}" is replaced by the URL-encoded query
    query_key: str = ""             # the query parameter in search_template; "" when the query is in the path
    split: tuple[str, ...] = ()     # how speech recognition splits the name ("git hub")
    ambiguous: tuple[str, ...] = () # split forms that are also ordinary English: joined only after a cue word
    aliases: tuple[str, ...] = ()   # other names people say for it ("nyt", "x.com")
    everyday: bool = False          # the name is also an ordinary word ("target", "medium"): a site only after a cue

    def search_url(self, query: str) -> str:
        if not self.search_template:
            return ""
        # In a query string a space is "+"; in a path it must be "%20", and nothing may stay unescaped.
        in_path = "?" not in self.search_template.split("{query}")[0]
        encoded = quote(query, safe="") if in_path else quote_plus(query)
        return self.search_template.replace("{query}", encoded)


SITES: tuple[Site, ...] = (
    Site("GitHub", "github.com", "https://github.com/",
         "https://github.com/search?q={query}&type=repositories", "q", split=("git hub",)),
    Site("YouTube", "youtube.com", "https://www.youtube.com/",
         "https://www.youtube.com/results?search_query={query}", "search_query", split=("you tube",)),
    Site("Wikipedia", "wikipedia.org", "https://en.wikipedia.org/wiki/Main_Page",
         "https://en.wikipedia.org/w/index.php?search={query}", "search", split=("wiki pedia", "wikki pedia")),
    Site("eBay", "ebay.com", "https://www.ebay.com/",
         "https://www.ebay.com/sch/i.html?_nkw={query}", "_nkw", split=("e bay", "e-bay")),
    Site("Google", "google.com", "https://www.google.com/",
         "https://www.google.com/search?q={query}", "q"),
    Site("DuckDuckGo", "duckduckgo.com", "https://duckduckgo.com/",
         "https://duckduckgo.com/?q={query}", "q", split=("duck duck go", "duckduck go", "duck duckgo")),
    Site("Bing", "bing.com", "https://www.bing.com/", "https://www.bing.com/search?q={query}", "q"),
    Site("Yahoo", "yahoo.com", "https://www.yahoo.com/", "https://search.yahoo.com/search?p={query}", "p"),
    Site("Stack Overflow", "stackoverflow.com", "https://stackoverflow.com/",
         "https://stackoverflow.com/search?q={query}", "q", split=("stack overflow",),
         ambiguous=("stack overflow",)),
    Site("Reddit", "reddit.com", "https://www.reddit.com/",
         "https://www.reddit.com/search/?q={query}", "q", split=("red dit",)),
    Site("LinkedIn", "linkedin.com", "https://www.linkedin.com/",
         "https://www.linkedin.com/search/results/all/?keywords={query}", "keywords", split=("linked in",),
         ambiguous=("linked in",)),
    Site("Netflix", "netflix.com", "https://www.netflix.com/",
         "https://www.netflix.com/search?q={query}", "q", split=("net flix",)),
    Site("Amazon", "amazon.com", "https://www.amazon.com/", "https://www.amazon.com/s?k={query}", "k"),
    # Two words in its own spelling: nothing to join, but "hacker news" still names the site. hn.algolia.com
    # rewrites ?q= to ?query= on load; plan_contract.REWRITTEN_KEYS accepts both.
    Site("Hacker News", "news.ycombinator.com", "https://news.ycombinator.com/",
         "https://hn.algolia.com/?q={query}", "q", split=("hacker news",), ambiguous=("hacker news",)),
    # Developer and reference
    Site("npm", "npmjs.com", "https://www.npmjs.com/", "https://www.npmjs.com/search?q={query}", "q",
         split=("n p m",)),
    Site("PyPI", "pypi.org", "https://pypi.org/", "https://pypi.org/search/?q={query}", "q", split=("py pi",)),
    Site("Hugging Face", "huggingface.co", "https://huggingface.co/",
         "https://huggingface.co/models?search={query}", "search", split=("hugging face",),
         ambiguous=("hugging face",)),
    Site("arXiv", "arxiv.org", "https://arxiv.org/", "https://arxiv.org/search/?query={query}&searchtype=all",
         "query"),
    Site("Wolfram Alpha", "wolframalpha.com", "https://www.wolframalpha.com/",
         "https://www.wolframalpha.com/input?i={query}", "i"),
    Site("Quora", "quora.com", "https://www.quora.com/", "https://www.quora.com/search?q={query}", "q"),
    Site("Medium", "medium.com", "https://medium.com/", "https://medium.com/search?q={query}", "q",
         everyday=True),
    # News and sport
    Site("BBC", "bbc.co.uk", "https://www.bbc.co.uk/", "https://www.bbc.co.uk/search?q={query}", "q",
         split=("b b c",)),
    Site("CNN", "cnn.com", "https://edition.cnn.com/", "https://edition.cnn.com/search?q={query}", "q",
         split=("c n n",)),
    Site("New York Times", "nytimes.com", "https://www.nytimes.com/",
         "https://www.nytimes.com/search?query={query}", "query", aliases=("nyt", "the new york times")),
    Site("ESPN", "espn.com", "https://www.espn.com/", "https://www.espn.com/search/_/q/{query}",
         split=("e s p n",)),
    # Video, music, social
    Site("IMDb", "imdb.com", "https://www.imdb.com/", "https://www.imdb.com/find/?q={query}", "q",
         split=("i m d b",)),
    Site("Spotify", "spotify.com", "https://open.spotify.com/", "https://open.spotify.com/search/{query}"),
    Site("SoundCloud", "soundcloud.com", "https://soundcloud.com/", "https://soundcloud.com/search?q={query}",
         "q", split=("sound cloud",)),
    Site("Vimeo", "vimeo.com", "https://vimeo.com/", "https://vimeo.com/search?q={query}", "q"),
    Site("Twitch", "twitch.tv", "https://www.twitch.tv/", "https://www.twitch.tv/search?term={query}", "term",
         everyday=True),
    Site("Twitter", "x.com", "https://x.com/", "https://x.com/search?q={query}", "q", aliases=("x.com",)),
    Site("Facebook", "facebook.com", "https://www.facebook.com/",
         "https://www.facebook.com/search/top/?q={query}", "q", split=("face book",)),
    Site("Instagram", "instagram.com", "https://www.instagram.com/", split=("insta gram",)),
    Site("TikTok", "tiktok.com", "https://www.tiktok.com/", "https://www.tiktok.com/search?q={query}", "q",
         split=("tik tok",)),
    Site("Pinterest", "pinterest.com", "https://www.pinterest.com/",
         "https://www.pinterest.com/search/pins/?q={query}", "q"),
    Site("Goodreads", "goodreads.com", "https://www.goodreads.com/",
         "https://www.goodreads.com/search?q={query}", "q", split=("good reads",), ambiguous=("good reads",)),
    Site("Steam", "steampowered.com", "https://store.steampowered.com/",
         "https://store.steampowered.com/search/?term={query}", "term", everyday=True),
    # Shopping
    Site("Etsy", "etsy.com", "https://www.etsy.com/", "https://www.etsy.com/search?q={query}", "q"),
    Site("Walmart", "walmart.com", "https://www.walmart.com/", "https://www.walmart.com/search?q={query}", "q"),
    Site("Target", "target.com", "https://www.target.com/", "https://www.target.com/s?searchTerm={query}",
         "searchTerm", everyday=True),
    Site("Best Buy", "bestbuy.com", "https://www.bestbuy.com/",
         "https://www.bestbuy.com/site/searchpage.jsp?st={query}", "st", split=("best buy",),
         ambiguous=("best buy",)),
    Site("Costco", "costco.com", "https://www.costco.com/",
         "https://www.costco.com/CatalogSearch?keyword={query}", "keyword"),
    Site("IKEA", "ikea.com", "https://www.ikea.com/", "https://www.ikea.com/us/en/search/?q={query}", "q"),
    Site("Home Depot", "homedepot.com", "https://www.homedepot.com/", "https://www.homedepot.com/s/{query}",
         aliases=("the home depot",)),
    Site("Apple", "apple.com", "https://www.apple.com/", "https://www.apple.com/us/search/{query}",
         everyday=True),
    Site("Craigslist", "craigslist.org", "https://www.craigslist.org/",
         "https://www.craigslist.org/search/sss?query={query}", "query", split=("craigs list",)),
    # Places, travel, jobs
    Site("Google Maps", "maps.google.com", "https://www.google.com/maps",
         "https://www.google.com/maps/search/{query}"),
    Site("Gmail", "mail.google.com", "https://mail.google.com/", split=("g mail",)),
    Site("Yelp", "yelp.com", "https://www.yelp.com/", "https://www.yelp.com/search?find_desc={query}",
         "find_desc"),
    Site("Zillow", "zillow.com", "https://www.zillow.com/", "https://www.zillow.com/homes/{query}_rb/"),
    Site("Tripadvisor", "tripadvisor.com", "https://www.tripadvisor.com/",
         "https://www.tripadvisor.com/Search?q={query}", "q", split=("trip advisor",),
         ambiguous=("trip advisor",)),
    Site("Airbnb", "airbnb.com", "https://www.airbnb.com/", "https://www.airbnb.com/s/{query}/homes",
         split=("air bnb", "air b n b", "air b and b")),
    Site("Booking.com", "booking.com", "https://www.booking.com/",
         "https://www.booking.com/searchresults.html?ss={query}", "ss", aliases=("booking",), everyday=True),
    Site("Expedia", "expedia.com", "https://www.expedia.com/"),
    Site("Indeed", "indeed.com", "https://www.indeed.com/", "https://www.indeed.com/jobs?q={query}", "q",
         everyday=True),
    Site("Glassdoor", "glassdoor.com", "https://www.glassdoor.com/",
         "https://www.glassdoor.com/Search/results.htm?keyword={query}", "keyword", split=("glass door",),
         ambiguous=("glass door",)),
)

# The word just before an ambiguous form that makes it a site: "open linked in", "on stack overflow". Without one
# ("the doc I linked in slack", "a stack overflow error") the words are left as they are.
SITE_CUES = {"open", "visit", "on", "to", "at", "launch", "check", "browse", "search", "onto", "via", "using", "into"}
# An everyday word ("target", "medium", "steam") names a site only after one of these, or written as a domain:
# "on medium heat" and "let off some steam" are not sites. "to" counts only after a verb of going ("go to target").
EVERYDAY_CUES = {"open", "visit", "launch", "browse", "search", "onto", "via", "using", "at", "from"}
_GOING = {"go", "head", "me", "navigate", "get", "back", "switch"}
_DOMAIN = r"(?:\.(?:com|org|net|co\.uk|co|tv|io))?"


def _spoken(form: str) -> str:
    return r"[\s-]+".join(re.escape(part) for part in re.split(r"[\s-]+", form))


def _host_label(site: Site) -> Optional[str]:
    """The host's own name ("imdb" for imdb.com, "bbc" for bbc.co.uk) when that is the whole host, so a
    subdomain host (mail.google.com, news.ycombinator.com) or a one-letter one (x.com) names nothing by itself."""
    labels = site.host.split(".")
    if labels[-2:] == ["co", "uk"]:
        labels = labels[:-1]
    return labels[0] if len(labels) == 2 and len(labels[0]) >= 3 else None


_SPLITS = sorted(((form, site) for site in SITES for form in site.split), key=lambda fs: -len(fs[0]))
_SPLIT_RE = re.compile(r"(?<![\w.])(" + "|".join(_spoken(f) for f, _ in _SPLITS) + r")(?![\w])", re.IGNORECASE)
_BY_FORM = {" ".join(re.split(r"[\s-]+", f.casefold())): site for f, site in _SPLITS}
_AMBIGUOUS = {" ".join(f.split()) for site in SITES for f in site.ambiguous}
_NAMES: dict[str, Site] = {}
for _site in SITES:
    for _key in (_site.name, _site.name.replace(" ", ""), _host_label(_site), *_site.aliases):
        if _key:
            _NAMES.setdefault(_key.casefold(), _site)
del _site, _key


def _cued(text: str, start: int, everyday: bool = False) -> bool:
    before = re.findall(r"[a-z']+", text[:start].casefold())
    if not everyday:
        return not before or before[-1] in SITE_CUES
    if not before:
        return False
    return before[-1] in EVERYDAY_CUES or (before[-1] == "to" and len(before) > 1 and before[-2] in _GOING)


def join_site_names(text: str) -> str:
    """The text with split site names joined in the site's own spelling ("git hub" → "GitHub"); nothing else
    changes. A split form that is also ordinary English is joined only after a cue word ("open linked in")."""
    if not text:
        return text

    def join(m: re.Match) -> str:
        form = " ".join(re.split(r"[\s-]+", m.group(0).casefold()))
        if form in _AMBIGUOUS and not _cued(text, m.start()):
            return m.group(0)
        return _BY_FORM[form].name
    return _SPLIT_RE.sub(join, text)


def site_for(name: str) -> Optional[Site]:
    """The site a name, a joined name, an alias or a split form names; None for anything else."""
    key = " ".join(re.split(r"[\s-]+", str(name or "").strip().casefold()))
    return _NAMES.get(key) or _NAMES.get(key.replace(" ", "")) or _BY_FORM.get(key)


_NAME_FORMS = sorted({form for site in SITES
                      for form in (site.name, site.name.replace(" ", ""), _host_label(site) or "", *site.aliases)
                      if form}, key=lambda f: -len(f))
_NAME_ALT = "|".join(r"[\s-]+".join(re.escape(p) for p in f.split(" ")) for f in _NAME_FORMS)
_NAME_RE = re.compile(r"(?<![\w.])(" + _NAME_ALT + r")(" + _DOMAIN + r")(?![\w])", re.IGNORECASE)


def mentioned_site(text: str) -> Optional[Site]:
    """The first site the request names, after joining. An ambiguous two-word name ("stack overflow") counts only
    in the site's own capitals or after a cue word, so "a stack overflow error" names no site; an everyday word
    ("target") counts only after a cue word or written as a domain ("medium.com")."""
    joined = join_site_names(text)
    for m in _NAME_RE.finditer(joined):
        site = site_for(m.group(1))
        if site is None:
            continue
        said = m.group(1)
        if site.name.casefold() in _AMBIGUOUS and " " in said and said != site.name \
                and not _cued(joined, m.start()):
            continue
        if site.everyday and not m.group(2) and "." not in said and not _cued(joined, m.start(), everyday=True):
            continue
        return site
    return None


# ---- literal arguments -------------------------------------------------------------------------------------

ORDINALS = {"first": 1, "top": 1, "second": 2, "third": 3, "fourth": 4, "fifth": 5, "sixth": 6, "seventh": 7,
            "eighth": 8, "ninth": 9, "tenth": 10, "last": None}
NUMBERS = {"one": 1, "two": 2, "three": 3, "four": 4, "five": 5, "six": 6, "seven": 7, "eight": 8, "nine": 9,
           "ten": 10}
RESULT_NOUN = r"(?:result|video|link|item|one|repo|repository|article|listing|product|hit|story|post|page|song|track)s?"
_ORD_ALT = "|".join(k for k in ORDINALS if k != "last") + r"|\d{1,2}(?:st|nd|rd|th)"
_NUM_ALT = r"\d{1,2}|" + "|".join(NUMBERS)
_ORDINAL_RE = re.compile(r"\b(" + _ORD_ALT + r")\s+(?:[a-z]+\s+)?" + RESULT_NOUN + r"\b", re.IGNORECASE)
_NUMBER_RE = re.compile(r"\b(?:result|video|link|item)\s+(?:number\s+)?(" + _NUM_ALT + r")\b", re.IGNORECASE)
# A result number with no noun is read only where nothing else could follow it: after a verb of picking ("open
# number two", "play the sixth") and at the end of the clause, or as the whole request ("the seventh"). An ordinal
# with words after it ("the first moon landing", "the sixth president") is not a result number.
_PICK_VERB = (r"(?:open|click|play|pick|select|choose|tap|watch|take|show\s+me|read|try|hit|press|listen\s+to|"
              r"go\s+(?:with|for|to|into|in\s+to)|bring\s+up|pull\s+up)")
_CLAUSE_END = r"(?=\s*$|\s*[.,!?;]|\s+(?:please|and|then|one|of\s+(?:them|those|these|the\s+results|the\s+list))\b)"
_BARE_RE = re.compile(r"\b" + _PICK_VERB + r"\s+(?:on\s+|into\s+)?(?:the\s+)?(?:(" + _ORD_ALT + r")|(?:number|#)\s*("
                      + _NUM_ALT + r"))\b" + _CLAUSE_END, re.IGNORECASE)
_ALONE_RE = re.compile(r"^\W*(?:(?:um+|uh+|erm|okay|ok|and)\s+)*(?:(?:the\s+)?(" + _ORD_ALT + r")(?:\s+one)?|"
                       r"(?:number|#)\s*(" + _NUM_ALT + r"))\W*$", re.IGNORECASE)

_SITE_ALT = _NAME_ALT
_DISFLUENCY = re.compile(r"\b(?:u+m+|u+h+|erm|uhm)\b[,]?\s*", re.IGNORECASE)
# Where a clause starts: a bare "search X" or "google X" is a query only here, so "the search bar" is not one.
_START = r"(?:^|[,.;!?]\s*|\b(?:please|and|then|you|to|just|now|so|ok|okay|also)\s+)"
# The phrase families that introduce a query, most specific first. Each captures the rest of the clause.
_QUERY_INTROS = [
    # "search youtube for X", "search on reddit for X", "search youtube X"
    re.compile(r"\bsearch\s+(?:on\s+|in\s+)?(?:the\s+)?(?:" + _SITE_ALT + r")" + _DOMAIN + r"\s+(?:for\s+|about\s+)?(.+)",
               re.IGNORECASE),
    # "browse airbnb for X", "check ebay for X": a site's own search, said with another verb
    re.compile(r"\b(?:browse|check|look\s+on|shop\s+on|scan)\s+(?:the\s+)?(?:" + _SITE_ALT + r")" + _DOMAIN
               + r"\s+for\s+(.+)", re.IGNORECASE),
    # "find X on ikea", "look for X on etsy", "shop for X at target": only with a site named, never a bare "find"
    re.compile(r"\b(?:find|look\s+for|shop\s+for|browse\s+for|hunt\s+for)\s+(.+?\s+(?:on|at|in)\s+(?:the\s+)?(?:"
               + _SITE_ALT + r")" + _DOMAIN + r"(?![\w]).*)", re.IGNORECASE),
    # "search the web for X", "search online for X", "look on the internet for X", "look online for X"
    re.compile(r"\b(?:search|look|hunt)\s+(?:(?:on\s+)?the\s+(?:web|internet|net)|online)\s+(?:for\s+|about\s+)?(.+)",
               re.IGNORECASE),
    # "web search X", "do a (quick) web search for X", "run a google search for X", "do a search on X"
    re.compile(r"\b(?:(?:do|run|make)\s+(?:a\s+)?)?(?:quick\s+)?(?:web|internet|online|google)?\s*search\s+"
               r"(?:for|on|about)\s+(.+)", re.IGNORECASE),
    re.compile(r"\b(?:web|internet|online)\s+search\s+(?:for\s+)?(.+)", re.IGNORECASE),
    re.compile(r"\b(?:search\s+(?:for|up|about)|look\s+up)\s+(.+)", re.IGNORECASE),
    # "google X" as a verb, at the start of a clause; "google maps" and "google it" are not queries
    re.compile(_START + r"google\s+(?!(?:maps|docs|drive|sheets|it|that|this)\b)(?:for\s+)?(.+)", re.IGNORECASE),
    # a bare "search X" at the start of a clause, but never "search results", "search bar" and the like
    re.compile(_START + r"search\s+(?!(?:for|up|about|results?|bar|box|field|page|button|engine|history|"
               r"settings|it|that|this)\b)(.+)", re.IGNORECASE),
]
_QUERY_END = [
    re.compile(r"\s+(?:on|in|using|at|from|via)\s+(?:the\s+)?(?:" + _SITE_ALT + r")" + _DOMAIN + r"\b.*$",
               re.IGNORECASE),
    re.compile(r"\s*,?\s*\b(?:and|then)\b\s+(?:then\s+)?(?:open|click|play|show|go|pick|select|watch|choose|read|tap|"
               r"tell|let|give|summari[sz]e|see|find|check|send|email|text|share|save|download|add|list|compare|"
               r"explain|log|sign|subscribe|follow|like|comment|reply|post|buy|order|book|turn|copy)\b.*$", re.IGNORECASE),
    re.compile(r"\s+in\s+(?:a\s+)?new\s+tab\b.*$", re.IGNORECASE),
    re.compile(r"\s+so\s+(?:that\s+)?(?:i|we|you)\b.*$", re.IGNORECASE),
    re.compile(r"\s+because\b.*$", re.IGNORECASE),
]
# Politeness and filler at either end of a query, removed until nothing more comes off.
_QUERY_TAIL = re.compile(r"\s+(?:please|for\s+me|real\s+quick|quickly|right\s+now|now|thanks|thank\s+you)$",
                         re.IGNORECASE)
_QUERY_HEAD = re.compile(r"^(?:please|the|a|an|some)\s+", re.IGNORECASE)


@dataclass(frozen=True)
class Arguments:
    site: Optional[Site]
    query: Optional[str]
    n: Optional[int]


def _query(text: str) -> Optional[str]:
    text = _DISFLUENCY.sub("", text)
    for intro in _QUERY_INTROS:
        m = intro.search(text)
        if m:
            break
    else:
        return None
    query = m.group(1)
    for end in _QUERY_END:
        query = end.sub("", query)
    query = query.strip().strip(" .,!?;:")
    if len(query) >= 2 and query[0] == query[-1] and query[0] in "\"'":
        query = query[1:-1]
    query = " ".join(query.split()).strip(" .,!?;:\"'")
    while True:
        trimmed = _QUERY_HEAD.sub("", _QUERY_TAIL.sub("", query)).strip(" .,!?;:\"'")
        if trimmed == query:
            break
        query = trimmed
    if not query or site_for(query) is not None:
        # "search youtube": the only words after the verb name the site, so there is nothing to search for.
        return None
    return query


def _number(word: str) -> int:
    word = word.casefold()
    if word in ORDINALS:
        return ORDINALS[word]
    return NUMBERS.get(word) or int(re.match(r"\d+", word).group(0))


def _ordinal(text: str) -> Optional[int]:
    for pattern in (_ORDINAL_RE, _NUMBER_RE, _BARE_RE, _ALONE_RE):
        m = pattern.search(text)
        if m:
            word = next(g for g in m.groups() if g)
            return _number(word)
    return None


def literal_arguments(request: str) -> Arguments:
    """Site, query and result number, read from the words alone. Each is None when the request does not say it."""
    text = join_site_names(" ".join(str(request or "").split()))
    n = _ordinal(_DISFLUENCY.sub("", text))
    return Arguments(mentioned_site(text), _query(text), n if n and n > 0 else None)


# Spoken navigation verbs that mean "open" when what follows is a site or a result ("go to YouTube", "pull up
# Netflix", "click on the fourth link"). Measured on the pinned checkpoint, Laya reads "open" reliably and these
# poorly, so the goal it is shown says "open" in their place. Only a one-step request is rewritten: when a second
# step follows ("pull up Netflix and log in"), Laya is shown the words as said, because it reads "open X and ..."
# as plain open_site and would lose the extra step. Nothing else in the request changes.
_NAV_VERB = (r"(?:go\s+(?:to|into|in\s+to|on|onto|with)|head\s+(?:over\s+)?to|take\s+me\s+to|bring\s+me\s+to|"
             r"navigate\s+to|jump\s+to|hop\s+on(?:to)?|pull\s+up|bring\s+up|load\s+up|load|visit|launch|"
             r"click\s+on|click|tap\s+on|tap|select|choose|pick)")
_RESULT_VERB = r"(?:play|watch|listen\s+to|show\s+me|read)"   # open a result, never a site ("play the second")
_RESULT_REF = r"(?:the\s+)?(?:(?:" + _ORD_ALT + r")\b|(?:number|#)\s*(?:" + _NUM_ALT + r")\b)"
_SITE_REF = r"(?:the\s+)?(?:" + _SITE_ALT + r")" + _DOMAIN + r"(?![\w])"
_NAV_RE = re.compile(r"\b(?:" + _NAV_VERB + r"(?=\s+(?:" + _SITE_REF + "|" + _RESULT_REF + r"))|" + _RESULT_VERB
                     + r"(?=\s+" + _RESULT_REF + r"))", re.IGNORECASE)
_NEXT_STEP = re.compile(r"\b(?:and|then|after\s+that|also)\b|[,;]", re.IGNORECASE)


def laya_goal(text: str) -> str:
    """The request as Laya is shown it: disfluencies dropped; in a one-step request, a navigation verb before a
    site or a result said as "open", and a bare result reference ("the sixth", "number two") said as "open the
    sixth". The literal arguments are always read from the request itself, not from this."""
    goal = " ".join(_DISFLUENCY.sub("", text).split())
    if _ALONE_RE.match(goal):
        return "open " + goal.strip(" .,!?")
    m = _NAV_RE.search(goal)
    if m is None or _NEXT_STEP.search(goal, m.end()):
        return goal
    return goal[:m.start()] + "open" + goal[m.end():]


# ---- the outcome ---------------------------------------------------------------------------------------------

KINDS = ("search", "open_site", "open_result", "none")


@dataclass(frozen=True)
class RequestOutcome:
    kind: str                       # exactly one of KINDS
    site_host: Optional[str]
    query: Optional[str]
    n: Optional[int]
    confidence: float
    source: str                     # "laya" or "code"
    latency_ms: float = field(default=0.0, compare=False)


# LayaBrowse's own outcome descriptions and rules (goal_engine.objective_descriptions and the "objective"
# question, MIT, Copyright (c) 2026 Aryan Bhujade), so the checkpoint is asked in the shape it was trained on.
OPTIONS = {
    "open_site": "Open the requested website. No search or result opening.",
    "search": "Search for the requested topic and show the results.",
    "open_result": "Open a particular video, article, repository or item from search results.",
    "unsupported": "Not a command, ambiguous, or these outcomes omit part of the request",
}
OPTION_KIND = {"open_site": "open_site", "search": "search", "open_result": "open_result", "unsupported": "none"}
OPTION_KEY = {kind: key for key, kind in OPTION_KIND.items()}
RULES = ("Choose the outcome requested by the speaker, not the next action. "
         "Include ALL requested work, including opening a result. "
         "Unrelated speech, unsupported actions or missing details mean unsupported.")
GATE_TOP, GATE_MARGIN = 0.50, 0.10

BROWSER_MODEL = "cklxx/laya-browser"
BROWSER_REVISION = "8a7e625b481d292d830acf22a2feec81554575fa"
BROWSER_SUBFOLDER = "v10s"
# MLX keeps freed GPU buffers for reuse and, uncapped, grew past 1.8 GB in a mostly idle background process
# (LayaBrowse laya.py); 64 MB costs about a millisecond a decision.
MLX_CACHE_LIMIT_BYTES = 64 * 1024 * 1024

MODES = ("laya", "off")
_LATENCIES: collections.deque = collections.deque(maxlen=256)


def mode(environ: Optional[Mapping[str, str]] = None) -> str:
    env = os.environ if environ is None else environ
    value = (env.get("EVE_BROWSER_INTENT") or "laya").strip().casefold()
    return value if value in MODES else "off"


def recent_latencies() -> list[float]:
    """Milliseconds per Laya call, newest last (the last 256)."""
    return list(_LATENCIES)


class Abstain(Exception):
    pass


def validate_choice(answer: Any, options: Mapping[str, str]) -> tuple[str, float]:
    """(option, top probability) when the answer is a well-formed choice that clears the gate, else Abstain.
    Mirrors LayaBrowse goal_engine.validate_choice: schema integrity, then 0.50 top and 0.10 margin."""
    try:
        probabilities = answer["probabilities"]
        choice = answer["choice"]
        values = list(probabilities.values())
        valid = (choice in options and set(probabilities) == set(options)
                 and all(type(v) in {int, float} and math.isfinite(v) and 0 <= v <= 1 for v in values)
                 and abs(sum(values) - 1) < 0.02
                 and probabilities[choice] >= max(values) - 0.0002)
    except (KeyError, TypeError, ValueError, AttributeError):
        valid = False
    if not valid:
        raise Abstain("malformed answer")
    ordered = sorted(values, reverse=True)
    if ordered[0] < GATE_TOP or (len(ordered) > 1 and ordered[0] - ordered[1] < GATE_MARGIN):
        raise Abstain("under the gate")
    return choice, float(probabilities[choice])


def _cached_checkpoint() -> Optional[str]:
    """The pinned checkpoint's local snapshot, found without the network; None when it is not downloaded."""
    try:
        from huggingface_hub import snapshot_download

        # Only the v10s files: the repository's other checkpoints and assets are never downloaded, and without the
        # pattern huggingface_hub calls a v10s-only cache incomplete.
        return snapshot_download(BROWSER_MODEL, revision=BROWSER_REVISION, local_files_only=True,
                                 allow_patterns=[f"{BROWSER_SUBFOLDER}/*"])
    except Exception:  # noqa: BLE001 — not cached, or no huggingface_hub: abstain
        return None


IDLE_DEFAULT_S = 600.0


def idle_seconds(environ: Optional[Mapping[str, str]] = None) -> float:
    """How long Laya stays loaded with no question before it is unloaded (EVE_BROWSER_INTENT_IDLE_S, default 600).
    0 keeps it loaded for the life of the process; anything unreadable is the default."""
    env = os.environ if environ is None else environ
    raw = (env.get("EVE_BROWSER_INTENT_IDLE_S") or "").strip()
    try:
        value = float(raw) if raw else IDLE_DEFAULT_S
    except ValueError:
        return IDLE_DEFAULT_S
    return value if math.isfinite(value) and value >= 0 else IDLE_DEFAULT_S


class LayaKindEngine:
    """One process-local laya-mlx agent, loaded on first use. A failed load is remembered, so a missing checkpoint
    costs one attempt, not one per request. Predictions are serialised under one lock.

    The loaded model holds about 1.1 GB, so after `idle_s` seconds with no question it is dropped and MLX's buffer
    cache cleared; the next question loads it again (about 0.4 s). A daemon timer does the dropping; `timer=False`
    leaves it to `unload_if_idle()`, which tests call with their own clock."""

    def __init__(self, idle_s: Optional[float] = None, *, clock: Any = time.monotonic, timer: bool = True) -> None:
        self._agent: Any = None
        self._failed = ""
        self._lock = threading.Lock()
        self._idle_s = idle_seconds() if idle_s is None else max(0.0, float(idle_s))
        self._clock = clock
        self._use_timer = timer
        self._timer: Optional[threading.Timer] = None
        self._last_used = 0.0

    @property
    def loaded(self) -> bool:
        return self._agent is not None

    def warm(self) -> None:
        with self._lock:
            self._load_locked()
            self._touch_locked()

    def _load_locked(self) -> None:
        if self._agent is not None:
            return
        if self._failed:
            raise Abstain(self._failed)
        try:
            import laya_mlx
            import mlx.core as mx
        except Exception as exc:  # noqa: BLE001 — laya-mlx not installed: abstain, and keep abstaining
            self._failed = f"laya-mlx unavailable: {exc}"
            raise Abstain(self._failed) from exc
        path = _cached_checkpoint()
        if not path:
            # Never download from inside a request; the checkpoint is fetched once, by hand.
            self._failed = f"{BROWSER_MODEL}@{BROWSER_REVISION[:7]} is not cached"
            raise Abstain(self._failed)
        try:
            mx.set_cache_limit(MLX_CACHE_LIMIT_BYTES)
            # No cache_prompts: laya-mlx keys its prompt cache on the whole instruction, and the instruction holds
            # the request itself, so a new request never hits it.
            self._agent = laya_mlx.load(path, subfolder=BROWSER_SUBFOLDER, revision=BROWSER_REVISION,
                                        dtype="float16")
        except Exception as exc:  # noqa: BLE001 — a checkpoint that will not load abstains
            self._failed = f"load failed: {exc}"
            log.warning("browser intent: %s", self._failed)
            raise Abstain(self._failed) from exc

    def _touch_locked(self) -> None:
        self._last_used = self._clock()
        if self._use_timer and self._idle_s > 0 and self._timer is None:
            self._schedule_locked(self._idle_s)

    def _schedule_locked(self, delay: float) -> None:
        timer = threading.Timer(delay, self._on_timer)
        timer.daemon = True
        timer.name = "browser-intent-idle"
        self._timer = timer
        timer.start()

    def _on_timer(self) -> None:
        with self._lock:
            self._timer = None
            if self._agent is None:
                return
            remaining = self._last_used + self._idle_s - self._clock()
            if remaining > 0:
                self._schedule_locked(remaining)
                return
            self._drop_locked()

    def unload_if_idle(self) -> bool:
        """Drop the model when it has been idle for `idle_s`; True when it was dropped."""
        with self._lock:
            if self._agent is None or self._idle_s <= 0 or self._clock() - self._last_used < self._idle_s:
                return False
            self._drop_locked()
            return True

    def unload(self) -> None:
        """Drop the model now, and stop the idle timer."""
        with self._lock:
            if self._timer is not None:
                self._timer.cancel()
                self._timer = None
            if self._agent is not None:
                self._drop_locked()

    def _drop_locked(self) -> None:
        self._agent = None
        gc.collect()
        try:
            import mlx.core as mx

            clear = getattr(mx, "clear_cache", None) or getattr(getattr(mx, "metal", None), "clear_cache", None)
            if clear is not None:
                clear()
        except Exception:  # noqa: BLE001 — no MLX to clear: the model is still dropped
            pass
        log.info("browser intent: Laya unloaded after %.0f s idle", self._idle_s)

    def _prefix_fits(self, question: dict) -> bool:
        """The question and its options fit the checkpoint's head budget (LayaBrowse goal_engine._prefix_fits):
        refuse rather than let laya-mlx truncate silently."""
        from laya_mlx.common import render_options

        q = self._agent._to_internal(question)
        tok = self._agent.tok

        def length(s: str) -> int:
            return len(tok(s, add_special_tokens=False)["input_ids"])
        options = [length(" " + o.replace(tok.mask_token, " ")) for o in render_options(q)]
        instruction = length(f"{q['t']} question: {q['ins']}".replace(tok.mask_token, " "))
        return (all(n <= 48 for n in options)
                and sum(n + 1 for n in options) + max(16, instruction) <= self._agent.cfg["head_max_len"])

    def ask(self, goal: str, question: dict, state: dict) -> dict:
        with self._lock:
            self._load_locked()
            self._touch_locked()
            if not self._prefix_fits(question):
                raise Abstain("request exceeds the checkpoint's question budget")
            result = self._agent.predict(state, {"objective": question})
            self._touch_locked()
        return (result.get("answers") or {}).get("objective", {}) if isinstance(result, dict) else {}


_ENGINE: Optional[LayaKindEngine] = None
_ENGINE_LOCK = threading.Lock()


def shared_engine() -> LayaKindEngine:
    global _ENGINE
    with _ENGINE_LOCK:
        if _ENGINE is None:
            _ENGINE = LayaKindEngine()
        return _ENGINE


def offered(args: Arguments) -> dict[str, str]:
    """The options Laya is shown: only the kinds these arguments could carry, then "unsupported" — the way
    LayaBrowse's contract_options lets a spoken query veto opening the bare site and a spoken result number veto a
    plain search, so Laya never weighs an outcome that would drop part of the request."""
    kinds = [kind for kind in ("open_site", "search", "open_result")
             if kind == "open_result" and (args.n is not None or args.query) or _carried(kind, args)]
    return {**{OPTION_KEY[kind]: OPTIONS[OPTION_KEY[kind]] for kind in kinds}, "unsupported": OPTIONS["unsupported"]}


def _carried(kind: str, args: Arguments) -> bool:
    """Can the literal arguments carry this kind, with no part of the request dropped?"""
    if kind == "search":
        return bool(args.query) and args.n is None
    if kind == "open_site":
        return args.site is not None and not args.query and args.n is None
    if kind == "open_result":
        return args.n is not None
    return kind == "none"


def expected_outcome(request: str, *, engine: Any = None,
                     environ: Optional[Mapping[str, str]] = None) -> Optional[RequestOutcome]:
    """The outcome the request asks for, or None to abstain (module docstring). `engine` answers
    ``ask(goal, question, state) -> laya-mlx choice answer``; the default is the shared local Laya."""
    if mode(environ) != "laya":
        return None
    text = join_site_names(" ".join(str(request or "").split()))
    if not text:
        return None
    args = literal_arguments(text)
    if args.site is None and args.query is None and args.n is None:
        # Nothing to search for, no site and no result: none of the three can be carried out.
        return RequestOutcome("none", None, None, None, 1.0, "code")
    asker = engine if engine is not None else shared_engine()
    options = offered(args)
    goal = laya_goal(text)
    question = {"type": "choice", "instructions": {"goal": goal, "rules": RULES}, "criteria": options}
    # No page yet: the outcome depends on the words. Same keys as LayaBrowse's goal state.
    state = {"page": {"url": "about:blank", "title": "", "text": ""}, "recent_actions": []}
    started = time.perf_counter()
    try:
        answer = asker.ask(goal, question, state)
        latency = (time.perf_counter() - started) * 1000
        _LATENCIES.append(latency)
        option, confidence = validate_choice(answer, options)
    except Abstain as exc:
        log.debug("browser intent abstains: %s", exc)
        return None
    except Exception as exc:  # noqa: BLE001 — any model failure is an abstention, never a crash in routing
        log.warning("browser intent: %s", exc)
        return None
    kind = OPTION_KIND[option]
    if not _carried(kind, args):
        return None
    if kind == "none":
        return RequestOutcome("none", None, None, None, round(confidence, 4), "laya", latency)
    return RequestOutcome(
        kind,
        args.site.host if args.site else None,
        args.query if kind in ("search", "open_result") else None,
        args.n if kind == "open_result" else None,
        round(confidence, 4),
        "laya",
        latency,
    )
