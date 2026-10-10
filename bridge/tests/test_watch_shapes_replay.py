"""Replays for watch.py and its Telegram wiring on real-world shapes (the shapes hunt, 2026-09-25).

Each test pins one defect found against the code as it was: it fails there, for exactly that defect, and
passes once the fix named in its docstring is applied. No network: every request goes to a fake.
"""

from __future__ import annotations

import asyncio
import json
import random
import sys
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).parent))

import test_telegram as T  # noqa: E402  (the Telegram rig, reused, not re-implemented)

from eve_bridge import watch  # noqa: E402
from eve_bridge.watch import Watch, WatchConfig, Watcher  # noqa: E402

HUGE = "1" + "0" * 400                  # a JSON integer literal no float can hold


class Clock:
    def __init__(self, t: float = 1_800_000_000.0) -> None:
        self.t = t

    def __call__(self) -> float:
        return self.t


def _no_price(body: dict[str, Any]) -> dict[str, Any]:
    return {"choices": [{"message": {"content": '{"price": null, "available": null, "note": ""}'}}]}


# ---- 1. a huge integer price crashes the reader, and the crash starves every other watch --------------

def test_a_huge_integer_price_is_dropped_not_raised() -> None:
    """Fix: _price and model_reading turn an OverflowError from float(int) into None."""
    assert watch._price(int(HUGE)) is None
    page = '<script type="application/ld+json">{"@type":"Product","name":"X","offers":{"price":' + HUGE + '}}</script>'
    assert watch.structured(page).value is None
    reply = {"choices": [{"message": {"content": '{"price": ' + HUGE + ', "available": true}'}}]}
    assert watch.model_reading(reply, "model").value is None


def test_one_page_with_a_huge_price_does_not_starve_the_other_watches(tmp_path: Path) -> None:
    """A page whose JSON-LD price is a 401-digit integer raised OverflowError out of structured(); check()
    catches only FetchError, so the tick died before next_at moved, the same watch was first in line on every
    later tick, the shop was fetched once a minute, and no other watch was ever checked."""
    clock = Clock()
    page = '<script type="application/ld+json">{"@type":"Product","name":"X","offers":{"price":' + HUGE + '}}</script>'
    quote = json.dumps({"chart": {"result": [{"meta": {"symbol": "AAPL", "currency": "USD",
                                                       "regularMarketPrice": 250.0}}]}})
    fetched: list[str] = []

    def fetch(url: str, **kw: Any) -> tuple[int, str]:
        fetched.append(url)
        return 200, (quote if "yahoo" in url else page)

    w = Watcher(WatchConfig(enabled=True, path=tmp_path / "w.json"), fetch=fetch, ask=_no_price, clock=clock,
                offload=False, rng=random.Random(1), resolve=lambda h: True)
    w.watches = [Watch(id="w1", kind="page", target="https://shop.example/x", label="X", condition="below",
                       value=100, every_secs=1800, next_at=clock.t - 10),
                 Watch(id="w2", kind="quote", target="AAPL", label="Apple", condition="below", value=300,
                       every_secs=900, next_at=clock.t - 5)]

    async def go() -> None:
        for _ in range(4):
            try:
                await w.tick()
            except Exception:  # noqa: BLE001 — run() logs a failed tick and waits 60 s; so does this
                pass
            clock.t += 60

    asyncio.run(go())
    assert w.watches[1].checks >= 1, "the quote watch was never checked"
    assert sum("shop.example" in u for u in fetched) == 1, fetched


# ---- 2. "/watch <words>" is typed into Claude Code or Codex while a relay is on -----------------------

def _watcher(tmp_path: Path) -> Watcher:
    return Watcher(WatchConfig(enabled=True, path=tmp_path / "w.json"), offload=False)


def test_a_watch_command_reaches_eve_while_the_claude_relay_is_on(tmp_path: Path) -> None:
    """Bare /watch is eve's own code word, relay or not; "/watch AAPL below 300" was rewritten to "Watch AAPL
    below 300" and typed into the Claude session, which has no watch tools. Fix: in _handle, a watch_ask skips
    the Codex and Claude relay branches and is addressed to eve, as "eve: watch ..." is."""
    async def go() -> tuple[list[str], int]:
        typed: list[str] = []
        rig = T.relay_rig(T.FakeApi(), typed, watcher=_watcher(tmp_path))
        rig.create.responses = [T.say("On it.")]
        rig.inlet.claude = True
        await T.dispatch(rig, "/watch AAPL below 300", update_id=2)
        return typed, len(rig.create.requests)

    typed, turns = asyncio.run(go())
    assert typed == [], f"typed into the Claude session: {typed}"
    assert turns == 1


def test_a_watch_command_reaches_eve_while_the_codex_relay_is_on(tmp_path: Path) -> None:
    async def go() -> tuple[list[str], int]:
        codex, api = T.FakeCodex(), T.FakeApi()
        rig = T.Rig(api, T.FakeCreate(T.say("On it.")), codex=codex, codex_folders=lambda: [T.CODEX_FOLDER],
                    watcher=_watcher(tmp_path))
        await T.dispatch(rig, "codex eve")
        assert codex.selected == [T.CODEX_FOLDER] and rig.inlet._codex_chat == T.OWNER
        await T.dispatch(rig, "/watch AAPL below 300", update_id=7)
        return codex.sent, len(rig.create.requests)

    sent, turns = asyncio.run(go())
    assert sent == [], f"sent to Codex: {sent}"
    assert turns == 1


# ---- 3. a <meta itemprop="price"> is taken from any scope, ahead of microdata's own scoping ----------

def test_a_meta_itemprop_price_in_a_review_is_not_the_items() -> None:
    """test_messy_structured_data_is_still_read pins "a price inside a review's scope is not the item's" with a
    <span>. The same review with the price as a <meta itemprop> tag (microdata's commonest form) went through
    structured()'s meta-tag pass, which ignores itemscope, and won. Fix: the meta-tag passes key on property
    and name only; itemprop metas are microdata's, read with their scope."""
    page = ('<div itemscope itemtype="https://schema.org/Product"><h1>Mug</h1>'
            '<div itemprop="offers" itemscope itemtype="https://schema.org/Offer">'
            '<span itemprop="price">$1,299.00</span><meta content="USD" itemprop="priceCurrency">'
            "<link itemprop='availability' href='https://schema.org/OutOfStock'></div>"
            '<div itemprop="review" itemscope itemtype="https://schema.org/Review">'
            '<meta itemprop="price" content="5"></div></div>')
    r = watch.structured(page)
    assert (r.value, r.currency) == (1299.0, "USD")


def test_a_later_products_meta_price_is_not_the_items_and_the_currency_is_kept() -> None:
    """The first priced offer is the item's (structured's own comment); a "you may also like" product's
    <meta itemprop="price"> after it is not, and <meta itemprop="priceCurrency"> is the price's currency."""
    page = ('<div itemscope itemtype="https://schema.org/Product"><h1 itemprop="name">Camera X</h1>'
            '<div itemprop="offers" itemscope itemtype="https://schema.org/Offer">'
            '<meta itemprop="price" content="499.00"><meta itemprop="priceCurrency" content="EUR"></div></div>'
            '<section class="also-like"><div itemscope itemtype="https://schema.org/Product">'
            '<span itemprop="name">Lens cap</span><div itemprop="offers" itemscope itemtype="https://schema.org/Offer">'
            '<meta itemprop="price" content="9.99"><meta itemprop="priceCurrency" content="EUR"></div></div></section>')
    r = watch.structured(page)
    assert (r.value, r.currency) == (499.0, "EUR")


# ---- 4. JSON-LD: a related product's or a members-only price is taken as the item's ------------------

def test_json_ld_related_products_and_member_prices_are_not_the_items() -> None:
    """_offers walks every nested dict, so an isRelatedTo product's offer and a loyalty tier's price
    (priceSpecification with validForMemberTier, Google's member-pricing markup) joined the min. Fix: _offers
    does not descend into isRelatedTo / isSimilarTo / isAccessoryOrSparePartFor / isConsumableFor and skips a
    node that carries validForMemberTier."""
    related = ('<script type="application/ld+json">{"@type":"Product","name":"Camera","offers":{"@type":"Offer",'
               '"price":499,"availability":"https://schema.org/OutOfStock"},"isRelatedTo":[{"@type":"Product",'
               '"name":"Strap","offers":{"@type":"Offer","price":19,"availability":"https://schema.org/InStock"}}]}'
               '</script>')
    r = watch.structured(related)
    assert (r.value, r.available) == (499.0, False)
    member = ('<script type="application/ld+json">{"@type":"Product","name":"Jacket","offers":{"@type":"Offer",'
              '"price":120,"priceCurrency":"USD","priceSpecification":[{"@type":"UnitPriceSpecification",'
              '"price":120,"priceCurrency":"USD"},{"@type":"UnitPriceSpecification","price":96,"priceCurrency":"USD",'
              '"validForMemberTier":{"@id":"https://shop.example/member-plan#gold"}}]}}</script>')
    assert watch.structured(member).value == 120.0


# ---- 5. main_text drops the item itself: a section's <header>, a custom element named nav-* -----------

def test_main_text_keeps_the_items_own_header_and_custom_elements() -> None:
    """main_text is the model's whole view of a page with no structured data, and the model cache's key. It
    removed an <article>'s own <header> (the product's name and price) and, for a custom element such as
    <nav-drawer> ("\\b" matches at the hyphen), everything up to the next real </nav> (the footer's). Fix:
    read <main> when the page has one, and match chrome tags only as whole tag names."""
    article = ('<body><nav>Home Shop</nav><main><article><header><h1>Widget</h1><p class=price>$49.00</p>'
               '</header><p>Great widget.</p></article></main><footer>Gift cards from $10</footer></body>')
    text = watch.main_text(article)
    assert "$49.00" in text and "Gift cards" not in text
    custom = ('<body><nav-drawer>menu</nav-drawer><div id=product><h1>Widget</h1><p>$49.00 In stock</p></div>'
              '<footer><nav>About</nav></footer></body>')
    assert "$49.00" in watch.main_text(custom)


# ---- 6. money() prints a sub-cent price in scientific notation --------------------------------------

def test_money_never_prints_scientific_notation() -> None:
    """SHIB-USD trades near $0.00001: the alert read "dropped to $1.234e-05". Fix: format the six significant
    digits positionally (Decimal)."""
    assert watch.money(0.00001234, "USD") == "$0.00001234"
    assert watch.money(0.5, "USD") == "$0.5" and watch.money(12.5, "USD") == "$12.50"
    w = Watch(id="w1", kind="quote", target="SHIB-USD", label="Shiba", condition="below", value=0.00001,
              currency="USD")
    assert "e-" not in watch.describe(w)


# ---- 7. a page that never states the watched fact reads as "no reading yet", forever, silently -------

def test_a_page_that_never_states_its_price_is_reported_not_silent(tmp_path: Path) -> None:
    """_read_page returned a Reading with no price (no structured data, the model said null, no browser) and
    check() took it as a success: errors stayed 0, so TELL_AFTER_ERRORS never told the owner, and /watches said
    "no reading yet" with no problem. Fix: read() raises FetchError for a page reading that lacks what the
    condition needs (Watcher._missing), so it counts as a failed read."""
    clock, told = Clock(), []

    async def notify(text: str) -> None:
        told.append(text)

    page = "<html><body><main><h1>Widget</h1><p>Call for price</p></main></body></html>"
    w = Watcher(WatchConfig(enabled=True, path=tmp_path / "w.json", browser=False), notify=notify,
                fetch=lambda url, **kw: (200, page), ask=_no_price, clock=clock, offload=False,
                rng=random.Random(1), resolve=lambda h: True)
    w.watches = [Watch(id="w1", kind="page", target="https://shop.example/x", label="Widget", condition="below",
                       value=50, every_secs=300, next_at=clock.t)]

    async def go() -> None:
        for _ in range(watch.TELL_AFTER_ERRORS + 4):
            await w.tick()
            clock.t = max(clock.t + 60, w.watches[0].next_at)

    asyncio.run(go())
    assert w.watches[0].errors >= watch.TELL_AFTER_ERRORS
    assert any(t.startswith("I can't read Widget right now") for t in told), told


# ---- 8. JSON-LD behind an unquoted or parameterised type attribute is not read ----------------------

def test_json_ld_with_an_unquoted_or_parameterised_type_is_read() -> None:
    """_LD_RE required the type in quotes and nothing after "json": a minified page's
    <script type=application/ld+json> (attribute quotes removed, valid HTML) and a type with a charset
    parameter were skipped, so a priced page went to the model (a paid call) instead. Fix: quotes optional,
    and anything after application/ld+json inside the attribute allowed."""
    body = '{"@type":"Product","name":"A","offers":{"price":"49.99","priceCurrency":"USD"}}'
    assert watch.structured(f"<script type=application/ld+json>{body}</script>").value == 49.99
    assert watch.structured(f'<script type="application/ld+json; charset=utf-8">{body}</script>').value == 49.99
