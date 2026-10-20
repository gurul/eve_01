"""Writes differential.json: random and edge inputs run through bridge/src/eve_bridge/nyt.py, for
tests/differential.rs. Run from rust/: ../../eve/bridge/.venv/bin/python crates/eve-nyt/tests/fixtures/gen_differential.py
(any Python that can import httpx)."""
import json
import random
import sys
from datetime import datetime, timezone
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parents[4] / "bridge" / "src"))
from eve_bridge import nyt  # noqa: E402

NOW = datetime(2026, 10, 16, 12, 0, tzinfo=timezone.utc)
rng = random.Random(20261019)
PIECES = ["a", "Z", " ", "  ", "\t", "\n", "\x00", "\x1c", "\x1f", "\x7f", "\x85", "\x9f", "\xa0", " ", " ",
          "　", "<b>", "</b>", "<", ">", "&amp;", "&lt;", "&#x80;", "&#0;", "&#1;", "&#55296;", "&#x110000;",
          "&notit;", "&amp", "&copy", "&bogus;", "&#65;", "&#x41", "İ", "ß", "Σ", "…", "٣", "7", "0", "-", "/", ".",
          "'", '"', "\\", "é", "😀", "Books", "review", "U.S.", "op-ed", "x" * 50, "https://", "http://", "nytimes.com",
          "fiction", "non-fiction", "ya", "kids", "hardcover", "audio", "​", "﻿"]


def rand_text(n=None):
    n = rng.randint(0, 8) if n is None else n
    return "".join(rng.choice(PIECES) for _ in range(n))


SCALARS = [None, True, False, 0, 1, -3, 7, 1.5, 2.0, -0.0, 1e9, 999999999, 1234567890, "12", " 7 ", "-5", "1e3", "",
           " ", "٣٠", "0x10", "+4", "1_0", [], [1, "a"], {}, {"a": 1, "b": [None, True]}, "abc", 1e-5, 1e16]


def value():
    return rng.choice(SCALARS) if rng.random() < 0.4 else rand_text()


def safe(fn, *args):
    try:
        return {"ok": fn(*args)}
    except nyt.BadArgs as e:
        return {"err": str(e)}


out = {}
out["clean_text"] = [[v, lim, nyt.clean_text(v, lim)] for v, lim in
                     [(value(), rng.choice([1, 2, 5, 10, 40, 120])) for _ in range(600)]]
def py_int(v):
    try:
        return {"ok": nyt._int(v)}
    except ValueError:
        return {"raises": True}       # Python's int() refuses what its \s let through (e.g. "\x1c0"): a crash, not a value


out["int"] = [[v, py_int(v)] for v in SCALARS + [rand_text(2) for _ in range(50)]]
out["https"] = [[v, nyt.https_url(v)] for v in
                [rng.choice(["https://", "http://", " https://", "https://a b", ""]) + rand_text() for _ in range(200)]
                + SCALARS + ["https://" + "x" * 592, "https://" + "x" * 593]]
out["day"] = [[v, nyt._day(v)] for v in SCALARS + ["2026-10-01T09:00", " 1999-01-02", "x2026-10-01", "٢٠٢٦-١٠-٠١"]]
out["key"] = [[v, nyt._key(v)] for v in SCALARS + [rand_text() for _ in range(200)]]
out["parse_date"] = [[v, safe(lambda x: (lambda d: d.isoformat() if d else None)(nyt.parse_date(x, "d")), v)]
                     for v in SCALARS + ["2024-02-29", "2023-02-29", "20240101", "2024-0101", "0000-01-01",
                                         "9999-12-31", " 2024-01-01 ", "٢٠٢٤-٠١-٠١", "2024-1-01", 20240101]]
out["fq"] = [[s, m, nyt.build_fq(s, m)] for s, m in [(value(), value()) for _ in range(300)]]


def sp(args):
    return nyt.search_params(args, NOW)


out["search_params"] = [[args, safe(sp, args)] for args in
                        [{"query": value(), "begin_date": rng.choice([None, "2024-05-01", "20240101", "bad", "1851-01-01", ""]),
                          "end_date": rng.choice([None, "2020-01-01", "2026-10-16", "2026-10-17"]),
                          "section": rng.choice([None, value()]), "material": rng.choice([None, value()]),
                          "sort": rng.choice([None, "", "Newest", " best ", "popular", 3]),
                          "page": rng.choice([None, 0, 5, 500, -2, "3", 2.0, 2.5, True])} for _ in range(300)]]
out["archive_month"] = [[args, safe(lambda a: list(nyt.archive_month(a, NOW)), args)] for args in
                        [{"year": rng.choice([1850, 1851, 1969, 2026, 2027, "1969", None, 1969.0, True]),
                          "month": rng.choice([0, 1, 7, 10, 11, 12, 13, "7", None, 7.5])} for _ in range(120)]]
out["books_date"] = [[v, safe(lambda x: (lambda d: d.isoformat() if d else None)(nyt.books_date(x, NOW)), v)]
                     for v in [None, "", "2017-01-01", "2016-12-31", "2026-10-30", "2026-10-31", "soon", "20250504"]]
out["popular_path"] = [[args, safe(nyt.popular_path, args)] for args in
                       [{"by": rng.choice([None, "", "viewed", "EMAILED", " shared ", "liked", 0, "  "]),
                         "period": rng.choice([None, 1, 7, 30, "7", 2, 0, "week", 7.0, True]),
                         "share_type": rng.choice([None, "", "facebook", "Facebook", "twitter"])} for _ in range(200)]]
SECTIONS = list(nyt.TOP_SECTIONS) + list(nyt.RSS_SECTIONS) + list(nyt.SECTION_ALIASES) + [
    "new york", "front page", "T Magazine", "SUNDAY REVIEW", "gardening", None, "", "  ", "Books / Review", 5]
out["resolve_section"] = [[v, safe(lambda x: list(nyt.resolve_section(x)), v)] for v in SECTIONS]
lists = [{"list_name_encoded": s, "display_name": n} for s, n in [
    ("combined-print-and-e-book-fiction", "Combined Print & E-Book Fiction"),
    ("combined-print-and-e-book-nonfiction", "Combined Print & E-Book Nonfiction"),
    ("hardcover-fiction", "Hardcover Fiction"), ("hardcover-nonfiction", "Hardcover Nonfiction"),
    ("paperback-nonfiction", "Paperback Nonfiction"), ("trade-fiction-paperback", "Paperback Trade Fiction"),
    ("advice-how-to-and-miscellaneous", "Advice, How-To & Miscellaneous"),
    ("childrens-middle-grade-hardcover", "Children's Middle Grade Hardcover"),
    ("picture-books", "Children's Picture Books"), ("series-books", "Children's Series"),
    ("young-adult-hardcover", "Young Adult Hardcover"), ("audio-fiction", "Audio Fiction"),
    ("audio-nonfiction", "Audio Nonfiction"), ("graphic-books-and-manga", "Graphic Books and Manga"),
    ("mass-market-monthly", "Mass Market Monthly"), ("Bad Slug", "x"), ("x", "too short"), ("ok-slug", None)]]
names = nyt._list_names(lists)
out["list_names_in"] = lists
out["list_names"] = names
WANTED = ["fiction", "nonfiction", "Non-Fiction", "non fiction", "paperback", "young adult", "YA", "teen", "audio",
          "audiobooks", "audio nonfiction", "Hardcover Fiction", "kids", "children's", "picture books", "manga",
          "graphic", "series", "mass market", "novel", "poetry", "", "!!!", "e-book", "combined", "  Hardcover   fiction ",
          "paperback nonfiction", "trade", "ok slug", "OK-SLUG", "advice"]
out["match_list"] = [[w, (nyt.match_list(nyt.clean_text(w, nyt.NAME_CHARS), names) or {}).get("slug")] for w in WANTED]


def doc():
    d = {}
    for k in ["web_url", "abstract", "snippet", "lead_paragraph", "pub_date", "section_name", "type_of_material",
              "news_desk", "desk"]:
        if rng.random() < 0.7:
            d[k] = rng.choice([value(), "https://www.nytimes.com/x" + rand_text(2), "2026-10-01T00:00:00Z"])
    d["web_url"] = rng.choice(["https://www.nytimes.com/a.html", "http://x", value()])
    d["headline"] = rng.choice([{"main": value()}, {"main": "", "print_headline": value()}, value(), {}])
    d["byline"] = rng.choice([{"original": value()}, value(), None])
    d["keywords"] = rng.choice([[{"value": value()} for _ in range(rng.randint(0, 15))], "x", None, {"value": "a"}])
    crop = lambda: {"url": rng.choice(["images/a.jpg", "/images/b.jpg", "//cdn/x.jpg", "https://s/x.jpg", "http://s/x.jpg",
                                       "javascript:x", "", None]),
                    "width": rng.choice([None, 399, 400, 600, "800", 1050.0, True, 2000]),
                    "height": rng.choice([None, 0, -1, 300, "300", 700]), "caption": value(), "credit": value()}
    d["multimedia"] = rng.choice([{"default": crop(), "thumbnail": crop(), "caption": value(), "credit": value()},
                                  [crop() for _ in range(rng.randint(0, 4))],
                                  [{"default": crop(), "caption": value()}, crop()], None, "x"])
    return d


docs = [doc() for _ in range(300)]
out["article"] = [[d, nyt.article(d)] for d in docs]
out["archive_row"] = [[d, nyt._archive_row(d)] for d in docs]


def bk():
    b = {k: value() for k in ["rank", "title", "author", "contributor", "publisher", "weeks_on_list", "description"]
         if rng.random() < 0.8}
    for k in ["amazon_product_url", "book_review_link", "sunday_review_link"]:
        b[k] = rng.choice(["", None, "https://www.amazon.com/x", "https://www.nytimes.com/r.html", "https://NYTimes.com:443/r",
                           "https://evilnytimes.com/r", "https://u@www.nytimes.com/r", "http://www.nytimes.com/r",
                           "https://[::1]/r", "https://example.com/r"])
    b["buy_links"] = rng.choice([[{"name": rng.choice(["Amazon", "Bookshop.org", "Apple Books", None]),
                                   "url": rng.choice(["https://a/x", "http://a/x", None])} for _ in range(rng.randint(0, 4))],
                                 None, "x"])
    b["book_image"] = rng.choice(["https://s/c.jpg", "http://s/c.jpg", "images/c.jpg", None])
    b["book_image_width"] = rng.choice([None, 199, 200, 331, "312"])
    b["book_image_height"] = rng.choice([None, 299, 300, 500, 0])
    return b


out["book"] = [[b, nyt.book(b)] for b in (bk() for _ in range(300))]


def pop():
    return {"url": rng.choice(["https://www.nytimes.com/p.html", "http://x", None]), "title": value(),
            "abstract": value(), "section": value(), "byline": value(), "published_date": value(),
            "media": rng.choice([None, "x", [{"caption": value(), "copyright": value(),
                                              "media-metadata": [{"url": rng.choice(["https://s/m.jpg", "images/m.jpg"]),
                                                                  "width": rng.choice([75, 210, 440, 440, "600"]),
                                                                  "height": rng.choice([75, 293, None])}
                                                                 for _ in range(rng.randint(0, 3))]}]])}


out["popular_row"] = [[p, nyt.popular_row(7, p)] for p in (pop() for _ in range(150))]


def top():
    return {"url": rng.choice(["https://www.nytimes.com/t.html", "http://x", None]), "title": value(),
            "abstract": value(), "section": value(), "subsection": value(), "byline": value(),
            "published_date": value(), "des_facet": rng.choice([None, "x", [value() for _ in range(9)]]),
            "geo_facet": rng.choice([None, [value(), None, 3]]),
            "multimedia": rng.choice([None, [{"url": "https://s/t.jpg", "width": rng.choice([599, 600, 2048]),
                                              "height": 400, "caption": value(), "copyright": value()}
                                             for _ in range(rng.randint(0, 3))]])}


out["top_row"] = [[t, nyt.top_row(t)] for t in (top() for _ in range(150))]


def xml_text():
    return rng.choice(["Plain", "A &amp; B", "<![CDATA[<b>Bold</b> & co]]>", "one<!-- c -->two", "", "  spaced  ",
                       "&#x2028;line", "&lt;i&gt;x&lt;/i&gt;"])


def item():
    parts = []
    if rng.random() < 0.9:
        parts.append(f"<title>{xml_text()}</title>")
    parts.append(rng.choice(["<link>https://www.nytimes.com/r.html</link>", "<link> https://www.nytimes.com/s.html </link>",
                             "<link>http://x</link>", ""]))
    if rng.random() < 0.7:
        parts.append(f"<description>{xml_text()}</description>")
    if rng.random() < 0.5:
        parts.append(f"<dc:creator>{xml_text()}</dc:creator>")
    if rng.random() < 0.5:
        parts.append("<pubDate>Thu, 16 Oct 2026 09:00:00 +0000</pubDate>")
    for _ in range(rng.randint(0, 8)):
        parts.append(f"<category>{xml_text()}</category>")
    for _ in range(rng.randint(0, 3)):
        w = rng.choice(["150", "400", "1800", "x"])
        parts.append(f'<media:content url="{rng.choice(["https://s/i.jpg", "images/i.jpg", "http://s/i.jpg"])}" width="{w}" height="100"/>')
    if rng.random() < 0.5:
        parts.append(f"<media:description>{xml_text()}</media:description>")
    if rng.random() < 0.5:
        parts.append(f"<media:credit>{xml_text()}</media:credit>")
    if rng.random() < 0.2:
        parts.append("<other><item><title>Nested</title><link>https://www.nytimes.com/n.html</link></item></other>")
    rng.shuffle(parts)
    return "<item>" + "".join(parts) + "</item>"


def feed():
    head = rng.choice(['<?xml version="1.0" encoding="UTF-8"?>', "", "<?xml version='1.0'?>"])
    body = "".join(item() for _ in range(rng.randint(0, 4)))
    return (head + '<rss xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:media="http://search.yahoo.com/mrss/">'
            "<channel>" + body + "</channel></rss>")


feeds = [feed() for _ in range(150)] + ["<rss><item><title>x</title>", "not xml", "<a xmlns='urn:x'><item><title>T</title>"
                                        "<link>https://www.nytimes.com/q.html</link></item></a>",
                                        "<item><title>Root</title><link>https://www.nytimes.com/root.html</link></item>"]


def pr(f):
    try:
        return {"ok": nyt.parse_rss(f.encode("utf-8"))}
    except nyt.FeedRefused as e:
        return {"err": str(e)}


out["parse_rss"] = [[f, pr(f)] for f in feeds]


def prb(b):
    try:
        return {"ok": nyt.parse_rss(b)}
    except nyt.FeedRefused as e:
        return {"err": str(e)}


ITEM = "<rss><channel><item><title>Caf\u00e9 \u20ac</title><link>https://www.nytimes.com/c.html</link></item></channel></rss>"
byte_feeds = [
    ("<?xml version='1.0' encoding='ISO-8859-1'?>" + ITEM.replace("\u20ac", "")).encode("latin-1"),
    ("<?xml version='1.0' encoding='windows-1252'?>" + ITEM).encode("cp1252"),
    ("<?xml version='1.0' encoding='UTF-16'?>" + ITEM).encode("utf-16"),
    ("<?xml version='1.0' encoding='UTF-16'?>" + ITEM).encode("utf-16-be"),
    ("<?xml version='1.0' encoding='UTF-16'?>" + ITEM).encode("utf-16-le"),
    "\ufeff".encode("utf-8") + ("<?xml version='1.0'?>" + ITEM).encode("utf-8"),
    ("<?xml version='1.0' encoding='us-ascii'?>" + ITEM).encode("utf-8"),
    ("<?xml version='1.0' encoding='latin1'?>" + ITEM.replace("\u20ac", "")).encode("latin-1"),
    ITEM.replace("\u20ac", "").encode("latin-1"),
    b"<rss>\x00<!DOCTYPE x></rss>",
    b"<rss><item><title>A</title><link>https://www.nytimes.com/a</link></item></rss>  \n",
    b"  <rss/>",
    b"<rss/><rss/>",
]
out["parse_rss_bytes"] = [[b.hex(), prb(b)] for b in byte_feeds]
# ---- whole calls: the same replies, the same result and the same requests ------------------------------------
import asyncio  # noqa: E402
import tempfile  # noqa: E402

import httpx  # noqa: E402

sys.path.insert(0, str(HERE.parents[4] / "bridge" / "tests"))
import test_nyt as T  # noqa: E402


def body_of(b):
    return b if isinstance(b, bytes) else json.dumps(b).encode()


def scenario(name, args, replies, key=True):
    pending = list(replies)
    urls = []

    def handler(request):
        urls.append(str(request.url))
        reply = pending.pop(0) if len(pending) > 1 else pending[0]
        if reply == "timeout":
            raise httpx.ReadTimeout("slow")
        if reply == "connect":
            raise httpx.ConnectError("no route")
        status, body, headers = reply
        return httpx.Response(status, content=body_of(body), headers=headers)

    async def nosleep(_s):
        return None

    env = {nyt.SWITCH: "1", **({nyt.KEY_ENV: T.CANARY} if key else {})}
    with tempfile.TemporaryDirectory() as tmp:
        nyt.CACHE_DIR = Path(tmp)
        result = asyncio.run(nyt.handle(name, args, environ=env, transport=httpx.MockTransport(handler), sleep=nosleep,
                                        now=NOW, limiter=T.free(), rss_limiter=T.free()))
    wire = [[r if r in ("timeout", "connect") else [r[0], body_of(r[1]).hex(), r[2]]] for r in replies]
    return {"name": name, "args": args, "key": key, "replies": [w[0] for w in wire], "result": result,
            "urls": [u.replace(T.CANARY, "KEY") for u in urls]}


J = {"Content-Type": "application/json"}
S = [
    scenario(nyt.SEARCH, T.a(T.SEARCH_ARGS, section="books", material="review", sort="newest", page=2),
             [(200, T.SEARCH_BODY, J)]),
    scenario(nyt.SEARCH, T.a(T.SEARCH_ARGS, query="a*b~c d+e/f", begin_date="2020-01-01"), [(200, {"response": {"docs": []}}, J)]),
    scenario(nyt.SEARCH, T.SEARCH_ARGS, [(200, {"response": {"docs": "x"}}, J)]),
    scenario(nyt.SEARCH, T.SEARCH_ARGS, [(429, {"fault": 1}, {"Retry-After": "40"})]),
    scenario(nyt.SEARCH, T.SEARCH_ARGS, [(429, b"busy", {})]),
    scenario(nyt.SEARCH, T.SEARCH_ARGS, [(502, b"down", {})]),
    scenario(nyt.SEARCH, T.SEARCH_ARGS, ["timeout"]),
    scenario(nyt.SEARCH, T.SEARCH_ARGS, ["connect"]),
    scenario(nyt.SEARCH, T.SEARCH_ARGS, [(401, b"no", {})]),
    scenario(nyt.SEARCH, T.SEARCH_ARGS, [(403, b"no", {})]),
    scenario(nyt.SEARCH, T.SEARCH_ARGS, [(404, b"no", {})]),
    scenario(nyt.SEARCH, T.SEARCH_ARGS, [(301, b"", {"Location": "https://x/"})]),
    scenario(nyt.SEARCH, T.SEARCH_ARGS, [(200, b"\xef\xbb\xbf{}", {})]),
    scenario(nyt.SEARCH, T.a(T.SEARCH_ARGS, query=""), [(200, T.SEARCH_BODY, J)]),
    scenario(nyt.ARCHIVE, T.a(T.ARCHIVE_ARGS, contains="space"), [(200, T.ARCHIVE_BODY, J)]),
    scenario(nyt.ARCHIVE, T.a(T.ARCHIVE_ARGS, section="Archives", limit=1), [(200, T.ARCHIVE_BODY, J)]),
    scenario(nyt.ARCHIVE, T.a(T.ARCHIVE_ARGS, month=13), [(200, T.ARCHIVE_BODY, J)]),
    scenario(nyt.BESTSELLERS, T.BOOK_ARGS, [(200, T.OVERVIEW_BODY, J)]),
    scenario(nyt.BESTSELLERS, T.a(T.BOOK_ARGS, date="2025-05-04"), [(200, T.OVERVIEW_BODY, J)]),
    scenario(nyt.BESTSELLERS, T.a(T.BOOK_ARGS, list="fiction"), [(200, T.OVERVIEW_BODY, J), (200, T.LIST_BODY, J)]),
    scenario(nyt.BESTSELLERS, T.a(T.BOOK_ARGS, list="poetry"), [(200, T.OVERVIEW_BODY, J)]),
    scenario(nyt.BESTSELLERS, T.a(T.BOOK_ARGS, list="fiction"), [(200, T.OVERVIEW_BODY, J), (200, {"results": {}}, J)]),
    scenario(nyt.BESTSELLERS, T.a(T.BOOK_ARGS, list="fiction"), [(500, b"x", {})]),
    scenario(nyt.POPULAR, T.a(T.POP_ARGS, by="shared", period=30, share_type="facebook"), [(200, T.POP_BODY, J)]),
    scenario(nyt.POPULAR, T.a(T.POP_ARGS, by="liked"), [(200, T.POP_BODY, J)]),
    scenario(nyt.HEADLINES, T.a(T.HEAD_ARGS, section="tech", limit=0), [(200, T.TOP_BODY, J)]),
    scenario(nyt.HEADLINES, T.a(T.HEAD_ARGS, section="climate"), [(200, T.RSS_FEED, {})]),
    scenario(nyt.HEADLINES, T.HEAD_ARGS, [(200, T.RSS_FEED, {})], key=False),
    scenario(nyt.HEADLINES, T.a(T.HEAD_ARGS, section="sports"), [(200, T.SPORTS_DEAD, J), (200, T.EMPTY_FEED, {})]),
    scenario(nyt.HEADLINES, T.a(T.HEAD_ARGS, section="technology", limit=1),
             [(503, b"x", {}), (503, b"x", {}), (200, T.RSS_FEED, {})]),
    scenario(nyt.HEADLINES, T.a(T.HEAD_ARGS, section="technology"), ["timeout", "timeout", (200, T.RSS_FEED, {})]),
    scenario(nyt.HEADLINES, T.a(T.HEAD_ARGS, section="technology"), [(404, b"x", {})]),
    scenario(nyt.HEADLINES, T.a(T.HEAD_ARGS, section="opinion"), [(404, b"x", {})]),
    scenario(nyt.HEADLINES, T.a(T.HEAD_ARGS, section="opinion"), [(200, T.SPORTS_DEAD, J)], key=False),
    scenario(nyt.HEADLINES, T.a(T.HEAD_ARGS, section="world"), [(200, b"<html>", {})], key=False),
    scenario(nyt.HEADLINES, T.a(T.HEAD_ARGS, section="world"), [(500, b"x", {})], key=False),
    scenario("web_search", {}, [(200, b"", {})]),
    scenario(nyt.SEARCH, ["q"], [(200, b"", {})]),
    scenario(nyt.POPULAR, T.POP_ARGS, [(200, b"", {})], key=False),
]
out["calls"] = S

(HERE / "differential.json").write_text(json.dumps(out, ensure_ascii=False, indent=0), encoding="utf-8")
print("cases:", {k: len(v) for k, v in out.items()})
