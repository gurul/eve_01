"""Generate pages, read them with Python's watch.py readers, and write the case file for the differential test
`the_page_readers_match_python` (tests/watch_pages.rs):

    bridge/.venv/bin/python3 rust/crates/eve-web/tests/gen_watch_pages.py SEED COUNT cases.json
    EVE_WATCH_PAGES_DIFF=cases.json cargo test -p eve-web --test watch_pages the_page_readers_match_python -- --ignored

The pages mix JSON-LD (typed, graphs, CDATA-wrapped, truncated, NaN), meta tags, microdata, entities and the
chrome tags main_text drops, then a random cut of the whole page.
"""
import json
import random
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[4] / "bridge" / "src"))
from eve_bridge import watch

seed, n, out = int(sys.argv[1]), int(sys.argv[2]), sys.argv[3]
R = random.Random(seed)
names = ["Blue Kettle", "Caf&eacute; Lamp", "Widget &amp; Co", "Tour 2026", "Señor Chair", "X"]
prices = ["12.99", "1,299.00", "85", "$45", "€12,50", "0", "-3", "1e3", "\"\"", "null", "19.999", "7.5 USD", "100000000"]
avail = ["https://schema.org/InStock", "OutOfStock", "http://schema.org/SoldOut", "PreOrder", "LimitedAvailability", "Discontinued", "instock", ""]
types = ['"Product"', '"Offer"', '["Product","Thing"]', '"Event"', '"schema:Product"', '"AggregateOffer"', '"Review"', '"ItemList"']
cur = ["USD", "EUR", "GBP", "JPY", "usd", "", "KWD"]
def ld():
    t = R.choice(types)
    offer = '{"@type":"Offer","price":%s,"priceCurrency":"%s","availability":"%s"}' % (R.choice(prices), R.choice(cur), R.choice(avail))
    if R.random() < 0.3:
        offer = "[" + ",".join([offer] * R.randint(1, 3)) + "]"
    if R.random() < 0.2:
        offer = '{"@type":"AggregateOffer","lowPrice":%s,"highPrice":%s,"priceCurrency":"%s"}' % (R.choice(prices), R.choice(prices), R.choice(cur))
    body = '{"@context":"https://schema.org","@type":%s,"name":"%s","offers":%s}' % (t, R.choice(names), offer)
    if R.random() < 0.15:
        body = '{"@graph":[%s,{"@type":"Product","name":"Other","offers":{"price":"9"}}]}' % body
    if R.random() < 0.1:
        body = "/*<![CDATA[*/" + body + "/*]]>*/"
    if R.random() < 0.1:
        body = body[: R.randint(1, len(body))]
    if R.random() < 0.05:
        body = body.replace('"price":', '"price": NaN, "p":')
    return '<script type="application/ld+json">%s</script>' % body
def meta():
    k = R.choice(["product:price:amount", "og:price:amount", "product:price:currency", "og:availability", "product:availability"])
    v = R.choice(prices + cur + avail)
    attr = R.choice(["property", "name", "itemprop"])
    return '<meta %s="%s" content="%s">' % (attr, k, v.strip('"'))
def micro():
    return ('<div itemscope itemtype="https://schema.org/Product"><span itemprop="name">%s</span>'
            '<div itemprop="offers" itemscope itemtype="https://schema.org/Offer"><meta itemprop="price" content="%s">'
            '<span itemprop="priceCurrency">%s</span><link itemprop="availability" href="%s"></div></div>') % (
        R.choice(names), R.choice(prices).strip('"'), R.choice(cur), R.choice(avail))
def chrome():
    return R.choice(["<nav>Home &gt; Shop $5 gift</nav>", "<footer>Gift card $25</footer>", "<aside>Related $9</aside>",
                     "<header><h1>%s</h1></header>" % R.choice(names), "<script>var p = '$99';</script>",
                     "<style>.p{}</style>", "<noscript>enable js</noscript>", "<template><b>$1</b></template>",
                     "<svg><text>$3</text></svg>", "<!-- <p>$7</p> -->", "<p>Now only &#36;%s &nbsp;today</p>" % R.choice(prices),
                     "<my-price>$12</my-price>", "<main class='x'><p>Item %s</p></main>" % R.choice(names), "&amp;&lt;&copy&notit;"])
parts = [ld, meta, micro, chrome, chrome]
cases = []
for _ in range(n):
    bits = [R.choice(parts)() for _ in range(R.randint(1, 7))]
    page = "<html><head><title>%s</title></head><body>%s</body></html>" % (R.choice(names), "".join(bits))
    if R.random() < 0.1:
        page = page[: R.randint(1, len(page))]
    r = watch.structured(page)
    cases.append({"page": page, "value": r.value, "currency": r.currency, "available": r.available, "name": r.name,
                  "source": r.source, "note": r.note, "main": watch.main_text(page), "visible": watch.visible_text(page),
                  "micro": [list(x) for x in watch.microdata(page)]})
json.dump(cases, open(out, "w"))
print(len(cases), "cases;", sum(c["value"] is not None for c in cases), "with a value")
