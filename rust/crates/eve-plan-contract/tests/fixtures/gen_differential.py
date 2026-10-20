"""Writes differential.json: inputs and what Python's plan_contract returns for each, so the Rust port is checked
against the Python module itself. Run from the repository root with the daemon's Python (3.13):

    ~/Documents/personal/eve/bridge/.venv/bin/python rust/crates/eve-plan-contract/tests/fixtures/gen_differential.py
"""
from __future__ import annotations

import json
import random
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parents[4] / "bridge" / "src"))
from eve_bridge import browser_intent as bi  # noqa: E402
from eve_bridge import plan_contract as pc  # noqa: E402

rng = random.Random(20261019)
cases: list[dict] = []


def add(fn: str, args: list, out) -> None:
    cases.append({"fn": fn, "args": args, "out": out})


def err(f):
    try:
        return {"ok": f()}
    except pc.PlanError as e:
        return {"err": str(e)}


def canon(v):
    c = pc.canonical_url(v)
    return list(c) if c is not None else None


def expect(e):
    return None if e is None else {"kind": e.kind, "value": e.value, "host": e.host, "here": e.here}


# ---- URLs -----------------------------------------------------------------------------------------------------

URLS = [
    "https://EXAMPLE.com:443", "https://example.com/", "https://example.com/article", "http://example.com/article",
    "https://example.com/Article", "https://example.com:444/article", "https://example.com:0/article",
    "https://example.com:65535/a", "https://example.com:65536/a", "https://example.com:00443/a",
    "javascript:alert(1)", "https://u:p@example.com/a", "https://u@example.com/a", "https://example.com:bad/a",
    "https://example.com/a b", "", "https://example.com\\evil/a", "HTTPS://Example.COM/Path?Q=1#F",
    "https://[::1]/x", "https://[::1]:8080/x", "https://[1.2.3.4]/x", "https://[v1.abc]/x", "https://[vz.abc]/x",
    "https://[::1/x", "https://::1]/x", "https://a[::1]/x", "https://[::1]x/x", "https://[fe80::1%25eth0]/x",
    "https://[fe80::1%eTh0]/x", "https://ex℀ample.com/", "https://éxample.com/", "https://ÉXAMPLE.com/",
    "https://example.com.", "https://example.com./a", "https:example.com", "https:///path", "https://:80/",
    "https://example.com:/a", "ftp://example.com/", "https://example.com/?", "https://example.com/#",
    "https://example.com?a=1", "https://example.com#frag?x", "https://x.com/%7Euser", "https://ΣΑΣ.gr/",
    "https://example.com:٣/a", "https://example.com:+1/a", "1https://example.com/", "+https://example.com/",
    "https://example.com/\x7f", "https://example.com/ ", "https://example.com/ ", "https://example.com/\x1c",
    "https://duckduckgo.com/?q=cats", "https://www.google.com/search?q=cats", "about:blank",
    "https://en.wikipedia.org/wiki/Alan_Turing", "https://hn.algolia.com/?q=laya+browse",
    "https://example.com/" + "a" * 2030, "https://example.com/" + "a" * 2040, "https://[::ffff:1.2.3.4]/",
    "https://[1:2:3:4:5:6:7:8]/", "https://[1::2::3]/", "https://[::1%]/", "https://[v1.]/", "https://[V1.a]/",
]
for u in URLS:
    add("canonical_url", [u], canon(u))
for _ in range(150):
    a, b = rng.choice(URLS), rng.choice(URLS)
    add("url_matches", [a, b], pc.url_matches(a, b))
for u in URLS:
    add("url_matches", [u, u], pc.url_matches(u, u))

# ---- bare_host ------------------------------------------------------------------------------------------------

HOSTS = [
    "youtube.com", "www.youtube.com", "WWW.YouTube.com.", "about:blank", "evil example", "", None, 1.5, 5, True,
    False, 0, ["a.com"], {"a": "b.com"}, "https://www.etsy.com/search?q=x", "https://www.youtube.com/watch?v=x",
    "http://[::1]/", "https://bad[/", " news.ycombinator.com ", "a.b", "a..b", "-a.com", "a-.com", "a" * 64 + ".com",
    "a" * 63 + ".com", "xn--bcher-kva.example", "ex_ample.com", "localhost", "ÉXAMPLE.com", "www.www.example.com",
    "ftp://files.example.com/x", "https://user:pw@host.example.com:8080/", "https://ex℀ample.com/",
    "\tyoutube.com\n", "https://you\ttube.com/", "1e+16", 1e16, 1e-05, -0.0, 12345678901234567890,
]
for h in HOSTS:
    add("bare_host", [h], pc.bare_host(h))

# ---- query words and url_query ---------------------------------------------------------------------------------

QUERIES = [
    "cats", "  Cats  ", "cats please", "please cats", "the beatles", "beatles", "cats and tell me what you find",
    "cats, then show me the top one", "cats and let me know", "um cats for me", "Cats!", "alan turing's paper",
    "Alan Turing’s paper", "cats and dogs", "red shoes", "vitamin a", "show and tell ideas", "blue mugs on etsy",
    "blue mugs on etsy.com", "blue mugs at argos co uk", "tell me about cats", "show me", "tell me", "please",
    "the the", "so so so", "ok thanks", "thank you", "for me", "cats thank you", "lofi beats", "laya browse",
    "CAFÉ", "café", "café", "Straße", "STRASSE", "ﬁsh", "नमस्ते दुनिया", "x_y z", "½ cup", "n²",
    "cats, and then tell me", "cats and see what happens", "cats so find out", "cats andtell me", "cats, tell meet",
    "find out", "cats and show us", "cats and give me", "cats then read me", "a", "",
    "cats onK now", "ſo cats", "x" * 401, "Σ test ΣΑΣ", "日本語 テスト", "rock'n'roll", "o'", "'quoted'",
]
SITES = [None, "", "duckduckgo.com", "google.com", "youtube.com", "wikipedia.org", "news.ycombinator.com",
         "github.com", "evil.example", "duckduckgo.com/x", " DuckDuckGo.com ", "ebay.com", "amazon.com", "bing.com",
         "yahoo.com", "x.com"]
HERES = ["", "youtube.com", "www.youtube.com", "github.com", "etsy.com", "about:blank", "evil example",
         "https://www.amazon.com/", "en.wikipedia.org"]
for q in QUERIES:
    add("query_words", [q], list(pc._query_words(q)))
for s in SITES:
    for q in ("cats", "", "x" * 401, "  Lofi  Beats "):
        add("url_query_expect", [s, q, ""], err(lambda: expect(pc.url_query_expect(s, q))))
for h in HERES:
    add("url_query_expect", [None, "cats", h], err(lambda: expect(pc.url_query_expect(None, "cats", here=h))))

SEARCH_URLS = []
ENGINES = {
    "duckduckgo": "https://duckduckgo.com/?q={}", "google": "https://www.google.com/search?q={}",
    "bing": "https://www.bing.com/search?q={}", "yahoo": "https://search.yahoo.com/search?p={}",
    "youtube": "https://www.youtube.com/results?search_query={}", "wiki": "https://en.wikipedia.org/w/index.php?search={}",
    "wikiart": "https://en.wikipedia.org/wiki/{}", "mwiki": "https://en.m.wikipedia.org/wiki/{}",
    "hn": "https://hn.algolia.com/?query={}&sort=byPopularity", "hnq": "https://hn.algolia.com/?q={}",
    "etsy": "https://www.etsy.com/search?q={}", "amazon": "https://www.amazon.com/s?k={}",
    "argos": "https://www.argos.co.uk/search/?searchTerm={}", "argosq": "https://www.argos.co.uk/search?q={}",
    "ebay": "https://www.ebay.com/sch/i.html?_nkw={}", "github": "https://github.com/search?q={}&type=repositories",
    "evil": "https://duckduckgo.com.evil.example/?q={}", "dotted": "https://duckduckgo.com./?q={}",
    "upper": "https://DuckDuckGo.com/?Q={}", "blank": "https://duckduckgo.com/?q=&q={}", "noval": "https://duckduckgo.com/?q",
    "x": "https://duckduckgo.com/?x={}", "gh-wiki": "https://github.com/wiki/{}",
}
from urllib.parse import quote, quote_plus  # noqa: E402

for q in QUERIES[:45]:
    for key, tmpl in ENGINES.items():
        enc = quote(q.replace(" ", "_")) if "wiki/" in tmpl else rng.choice([quote_plus, quote])(q)
        SEARCH_URLS.append(tmpl.format(enc))
SEARCH_URLS += ["https://en.wikipedia.org/wiki/Alan_Turing_(film)", "https://en.wikipedia.org/wiki/Special:Search",
                "https://en.wikipedia.org/wiki/Main_Page", "https://en.wikipedia.org/wiki/%E6%97%A5%E6%9C%AC",
                "https://en.wikipedia.org/wiki/caf%C3%A9", "https://en.wikipedia.org/wiki/caf%C3", "https://en.wikipedia.org/wiki/",
                "https://duckduckgo.com/?q=caf%C3%A9", "https://duckduckgo.com/?q=caf%C3+%A9", "https://duckduckgo.com/?q=%zz",
                "https://duckduckgo.com/?q=café%C3%A9", "https://duckduckgo.com/?q=%",
                "about:blank", "https://www.youtube.com/watch?v=x"]
for _ in range(2500):
    url = rng.choice(SEARCH_URLS)
    s = rng.choice(SITES[:8] + [None, None])
    q = rng.choice(QUERIES[:45])
    h = rng.choice(HERES)
    try:
        e = pc.url_query_expect(s, q, here=h)
    except pc.PlanError:
        continue
    add("url_query_matches", [url, s, q, h], pc.url_query_matches(url, e))
# A non-url_query expect never matches.
add("url_query_matches_kind", ["https://duckduckgo.com/?q=cats"], pc.url_query_matches("https://duckduckgo.com/?q=cats", pc.Expect("url_is", "cats")))

# ---- parsing plans ---------------------------------------------------------------------------------------------

STEP_VARIANTS = [
    {"kind": "click", "target": "Week", "label_hint": "Week"},
    {"kind": "click", "target": "  Week \n view  ", "label_hint": "  W  "},
    {"kind": "click", "target": ""},
    {"kind": "click", "target": "x" * 130, "label_hint": "y" * 130},
    {"kind": "drag", "target": "x"},
    {"kind": None, "target": "x"},
    {"target": "x"},
    {"kind": 3, "target": "x"},
    {"kind": ["click"], "target": "x"},
    {"kind": "click", "target": 5},
    "nope",
    None,
    {"kind": "open_url", "target": "http://plain.example"},
    {"kind": "open_url", "target": "https://a b.example"},
    {"kind": "open_url", "target": "https://example.com/"},
    {"kind": "open_url", "target": "https://localhost"},
    {"kind": "open_app", "target": "Calendar"},
    {"kind": "press_key", "key": "cmd+q"},
    {"kind": "press_key", "key": "Return"},
    {"kind": "press_key", "key": " TAB "},
    {"kind": "press_key", "key": "it's"},
    {"kind": "press_key"},
    {"kind": "type", "target": "Search", "text": ""},
    {"kind": "type", "target": "Search", "text": "Blue  Adapter", "text_source": "utterance"},
    {"kind": "type", "target": "Message", "text": "Running ten minutes late", "text_source": "utterance"},
    {"kind": "type", "target": "Terminal", "text": "rm"},
    {"kind": "type", "target": "Terminal", "text": "ls -la"},
    {"kind": "type", "target": "Search", "text": "  " + "z" * 450},
    {"kind": "type", "target": "Search", "text": 42},
    {"kind": "fill", "target": "the sign-in form", "text": "v_ab12cd34"},
    {"kind": "fill", "target": "the sign-in form", "text": "hunter2"},
    {"kind": "fill", "target": "the form", "text": "v_AB12CD34"},
    {"kind": "fill", "target": "", "text": "v_ab12cd34"},
    {"kind": "fill", "target": "f", "text": " v_ab12cd34 "},
    {"kind": "checkpoint"},
    {"kind": "checkpoint", "target": "after results"},
    {"kind": "click", "target": "a", "consequential": True},
    {"kind": "click", "target": "a", "consequential": "yes"},
    {"kind": "click", "target": "a", "consequential": 0},
    {"kind": "click", "target": "a", "consequential": []},
    {"kind": "click", "target": "a", "expect": {"kind": "text_visible", "value": "  Done  now "}},
    {"kind": "click", "target": "a", "expect": {"kind": "title_contains", "value": ""}},
    {"kind": "click", "target": "a", "expect": {"kind": "none", "value": "x"}},
    {"kind": "click", "target": "a", "expect": {"kind": "", "value": "x"}},
    {"kind": "click", "target": "a", "expect": {"value": "x"}},
    {"kind": "click", "target": "a", "expect": {"kind": "url_query", "value": "x"}},
    {"kind": "click", "target": "a", "expect": {"kind": "screen", "value": "x"}},
    {"kind": "click", "target": "a", "expect": {"kind": 7, "value": "x"}},
    {"kind": "click", "target": "a", "expect": {"kind": "url_is", "value": "https://example.com/" + "a" * 150}},
    {"kind": "click", "target": "a", "expect": {"kind": "url_is", "value": "https://u:p@example.com/a"}},
    {"kind": "click", "target": "a", "expect": {"kind": "url_is", "value": 42}},
    {"kind": "click", "target": "a", "expect": {"kind": "url_is"}},
    {"kind": "click", "target": "a", "expect": "text"},
    {"kind": "click", "target": "a", "expect": None},
    {"kind": "click", "target": "a", "expect": {"kind": "app_frontmost", "value": 5}},
]
REQUESTS = ["search my notes for blue adapter and tell Sam I'm late", "open terminal and list the files",
            "open terminal and run ls -lah", "open terminal and run ls -la please", "search for Blue Adapter.",
            "search duck duck go for cats", "", "RUNNING ten minutes LATE", "rm -rf", "xrm"]
for i, s in enumerate(STEP_VARIANTS):
    for r in REQUESTS[:5] if i in (23, 24, 25, 26) else REQUESTS[:1]:
        def one():
            st = pc.parse_step(s, 2, r)
            return {"step": pc.step_to_dict(st), "describe": st.describe()}
        add("parse_step", [s, 2, r], err(one))

for text, request in [("rm", "open terminal and list the files"), ("ls -la", "open terminal and run ls -lah"),
                      ("ls -la", "open terminal and run ls -la please"), ("blue adapter", "search for Blue Adapter."),
                      ("Running ten minutes late", "RUNNING  ten minutes LATE!"), ("late", "i'm late"),
                      ("i'm", "i'm late"), ("m late", "i'm late"), ("x_y", "a x_y b"), ("x", "x_y"), ("é", "café"),
                      ("e", "café"), ("½", "a ½ cup"), ("ß", "STRASSE ß"), ("ss", "Straße")]:
    s = {"kind": "type", "target": "Terminal", "text": text}
    add("parse_step", [s, 1, request], err(lambda: {"step": pc.step_to_dict(pc.parse_step(s, 1, request)),
                                                     "describe": pc.parse_step(s, 1, request).describe()}))

OUTCOMES = {
    "none": None, "ddg": ("search", "duckduckgo.com", "cats"), "any": ("search", None, "lofi beats"),
    "site": ("open_site", "github.com", None), "nothing": ("none", None, None), "badhost": ("search", "evil.example", "cats"),
    "empty": ("search", None, ""), "boom": "boom",
}


def intent_for(name):
    o = OUTCOMES[name]
    if o == "boom":
        def boom(r):
            raise RuntimeError("metal went away")
        return boom
    if o is None:
        return lambda r: None
    return lambda r: bi.RequestOutcome(o[0], o[1], o[2], None, 0.9, "laya")


def full(*steps, success=None, **extra):
    st = [{"kind": "click", "target": "", "label_hint": "", "text": "", "key": "", "expect": None,
           "consequential": False, **s} for s in steps]
    return {"needs_eyes": False, "why": "", "steps": st, "final_say": "All set.", "success": success, **extra}


PLANS = [
    {"steps": []}, {"steps": [{"kind": "click", "target": "x"}] * (pc.MAX_STEPS + 1)}, "nope", None, [],
    {"steps": [{"kind": "click", "target": "x"}] * pc.MAX_STEPS},
    {"steps": "x"}, {}, {"needs_eyes": True, "why": "it asks what is on the calendar", "steps": []},
    {"needs_eyes": True, "why": "  w " * 100}, {"needs_eyes": 1, "steps": []}, {"needs_eyes": "true", "steps": [{"kind": "checkpoint"}]},
    full({"kind": "open_app", "target": "Calendar"}, success={"kind": "text_visible", "value": "Week"}),
    full({"kind": "open_url", "target": "https://duckduckgo.com/?q=cats"}, app="browser"),
    full({"kind": "open_url", "target": "https://www.google.com/search?q=cats"}, success={"kind": "text_visible", "value": "cats"}, app="browser"),
    full({"kind": "open_url", "target": "https://www.google.com/"}, success={"kind": "url_query", "value": "cats"}),
    full({"kind": "open_url", "target": "https://www.youtube.com/"}, app="browser", here="www.youtube.com"),
    full({"kind": "open_url", "target": "https://www.youtube.com/"}, app="browser", here="https://www.youtube.com/watch?v=x"),
    full({"kind": "open_url", "target": "https://www.youtube.com/"}, app="browser", here=1.5),
    full({"kind": "open_url", "target": "https://www.youtube.com/"}, app="browser", here=None),
    full({"kind": "open_url", "target": "https://example.com/pricing"}, success={"kind": "url_is", "value": "https://example.com/pricing"}, app="browser"),
    full({"kind": "open_app", "target": "Notes"}, app="Notes"),
    {**full({"kind": "click", "target": "a"}), "final_say": "  " + "s " * 150},
    {**full({"kind": "click", "target": "a"}), "final_say": 5},
    full({"kind": "click", "target": "a"}, success="x"),
    full({"kind": "click", "target": "a"}, {"kind": "drag"}),
]
for plan in PLANS:
    for oname in OUTCOMES:
        for browser in (None, True, False):
            if browser is not None and oname not in ("ddg", "any"):
                continue
            def one():
                p = pc.parse_plan(plan, "search duck duck go for cats", "code" if browser is False else "astra",
                                  browser=browser, intent=intent_for(oname))
                return {"dict": pc.plan_to_dict(p), "success": expect(p.success), "planned": expect(p.planned_success),
                        "reasons": list(p.reasons), "sources": [s.text_source for s in p.steps]}
            add("parse_plan", [plan, "search duck duck go for cats", "code" if browser is False else "astra", browser, oname], err(one))

# The round trip the lane makes.
first = pc.parse_plan(full({"kind": "open_url", "target": "https://duckduckgo.com/?q=cats"}), "search duck duck go for cats",
                      intent=intent_for("ddg"))
again = {**pc.plan_to_dict(first), "app": "browser"}
add("parse_plan", [again, "search duck duck go for cats", "astra", None, "ddg"],
    err(lambda: {"dict": pc.plan_to_dict(pc.parse_plan(again, "search duck duck go for cats", intent=intent_for("ddg"))),
                 "success": expect(pc.parse_plan(again, "search duck duck go for cats", intent=intent_for("ddg")).success),
                 "planned": None, "reasons": [], "sources": ["utterance"]}))

# ---- the planner's request -------------------------------------------------------------------------------------

INPUTS = [
    dict(app="Calendar", outline=["Week (radio button)"], apps=["Calendar"]),
    dict(app="", outline=[], apps=[]),
    dict(app="browser", outline=["button: Sign in"], apps=[], site="chase.com",
         vault=['v_ab12cd34 login "Chase" for chase.com', 'v_ef56gh78 card "Visa"']),
    dict(app="browser", outline=[], apps=[], vault=[]),
    dict(app="browser", outline=[], apps=[], vault=None, current_url="https://example.com/"),
    dict(app="Finder", outline=[], apps=["Notes"], vault=["v_ab12cd34 x"]),
    dict(app="browser", outline=[f"line {i}" for i in range(100)], apps=[f"App{i}" for i in range(130)],
         site="x.com", done=["click a", "type b"], again="the page changed", vault=[f"v_{i:08d}" for i in range(50)]),
    dict(app="Mail", outline=["a"], apps=[], done=[], again=""),
]
for i, kw in enumerate(INPUTS):
    req = "  make it\tthe week  view "
    add("plan_request_text", [req, kw], pc.plan_request_text(req, **kw))
    for reads, browser in ((False, False), (False, True), (True, False), (True, True))[: 4 if i in (0, 2) else 1]:
        add("plan_request", ["gpt-6-luna", req, kw, "low", 30.5, reads, browser],
            pc.plan_request("gpt-6-luna", req, effort="low", timeout=30.5, reads=reads, browser=browser, **kw))

PAGES = [{"title": "Inbox", "url": "https://mail.google.com/", "text": "You have 3 messages"},
         {}, {"title": None, "url": 5, "text": "x" * 13000}, {"title": "", "text": ["a"]}]
for page in PAGES:
    add("read_request", ["m", " how many  emails ", page, "low", 12.0],
        pc.read_request("m", " how many  emails ", page, effort="low", timeout=12.0))

add("constants", [], {
    "PLAN_INSTRUCTIONS": pc.PLAN_INSTRUCTIONS, "PLAN_INSTRUCTIONS_BROWSER": pc.PLAN_INSTRUCTIONS_BROWSER,
    "PLAN_INSTRUCTIONS_READ": pc.PLAN_INSTRUCTIONS_READ, "READ_INSTRUCTIONS": pc.READ_INSTRUCTIONS,
    "READ_NOT_FOUND": pc.READ_NOT_FOUND, "PLAN_SCHEMA": pc.PLAN_SCHEMA, "KINDS": list(pc.KINDS),
    "EXPECT_KINDS": list(pc.EXPECT_KINDS), "CODE_EXPECT_KINDS": list(pc.CODE_EXPECT_KINDS), "KEYS": list(pc.KEYS),
    "MAX_STEPS": pc.MAX_STEPS, "MAX_TEXT_CHARS": pc.MAX_TEXT_CHARS, "MAX_TARGET_CHARS": pc.MAX_TARGET_CHARS,
    "MAX_SAY_CHARS": pc.MAX_SAY_CHARS, "SEARCH_ENGINES": sorted(pc.SEARCH_ENGINES),
    "COMMON_QUERY_KEYS": list(pc.COMMON_QUERY_KEYS), "REWRITTEN_KEYS": {k: list(v) for k, v in pc.REWRITTEN_KEYS.items()},
    "ARTICLE_PATHS": pc.ARTICLE_PATHS,
})

(HERE / "differential.json").write_text(json.dumps(cases, ensure_ascii=False, indent=0) + "\n")
print(f"{len(cases)} cases")
