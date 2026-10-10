"""Live smoke test for the watcher (watch.py) against the real web. Prints WATCH_SMOKE_OK only after every check.

1. A quote: AAPL from Yahoo's chart endpoint through the real rate limiter; a positive price in USD.
2. A shop page read from its structured data with no model call (the model is a fake that fails if called).
3. A shop that refuses a plain read (HTTP 403) read through headless Chromium, still with no model call.
4. Vision (about $0.001 on OpenRouter): the vision model reads a price off a real product screenshot, and
   recognises a real bot check ("press & hold") as blocked.

Needs the network, Playwright's Chromium, and OPENROUTER_API_KEY (read from ~/.config/eve-bridge/env).
Shop pages change; when one stops serving what a step needs, the step says which and the run fails loudly.
Usage: .venv/bin/python tools/watch_smoke.py
"""

from __future__ import annotations

import asyncio
import sys
import tempfile
from pathlib import Path
from typing import Any

from eve_bridge import envfile, watch

QUOTE = "AAPL"
STRUCTURED_PAGE = "https://www.ikea.com/us/en/p/billy-bookcase-white-00263850/"
REFUSING_PAGE = "https://www.lego.com/en-us/product/millennium-falcon-75192"
BOT_CHECK_PAGE = "https://www.target.com/p/apple-airpods-4/-/A-85978621"


def no_model(body: dict[str, Any]) -> dict[str, Any]:
    raise AssertionError("a model was called where the page's own data should have answered")


def main() -> int:
    envfile.load_env_file()
    folder = Path(tempfile.mkdtemp(prefix="watch-smoke-"))
    cfg = watch.configured()
    cfg = watch.WatchConfig(**{**cfg.__dict__, "path": folder / "watches.json"})

    async def run() -> None:
        w = watch.Watcher(cfg, ask=no_model)
        out = await w.add({"kind": "quote", "target": QUOTE, "label": "Apple", "condition": "below", "value": 1,
                           "text": None, "city": None, "every_minutes": None})
        item = w.watches[0]
        assert out["ok"] and item.last_value and item.last_value > 0 and item.currency == "USD", out
        print(f"1. quote {QUOTE}: {out['now']} (limiter took finance.yahoo.com)")

        page = watch.Watch(id="s", kind="page", target=STRUCTURED_PAGE, label="Billy", condition="below", value=1)
        r = await w.read(page)
        assert r.value and r.value > 0 and r.source == "structured", r
        print(f"2. structured page: {watch.money(r.value, r.currency)} from its own data, no model")

        _, body = await asyncio.to_thread(watch.http_request, "https://example.com/")    # the network is up
        try:
            await asyncio.to_thread(watch.http_request, REFUSING_PAGE)
            print("3. note: the refusing page answered a plain read this time")
        except watch.FetchError as e:
            assert e.status == 403, e.reason
        html_page, shot = await asyncio.to_thread(watch.render, REFUSING_PAGE)
        r = watch.structured(html_page)
        assert r.value and r.value > 0, "the rendered page carried no price"
        print(f"3. refused plain read, rendered in Chromium: {watch.money(r.value, r.currency)} "
              f"{'in stock' if r.available else 'availability unknown'} ({r.name[:40]}), no model")

        seer = watch.Watcher(cfg, render=lambda url, **kw: (watch.visible_text("<p></p>"), shot))
        seen_item = watch.Watch(id="v", kind="page", target=REFUSING_PAGE, label="LEGO Millennium Falcon",
                                condition="below", value=1, via="browser")
        seen = await seer.read(seen_item)
        assert seen.value and abs(seen.value - r.value) / r.value < 0.02, (seen, r.value)
        print(f"4a. vision read the screenshot: {watch.money(seen.value, seen.currency)} "
              f"(the page's own data says {watch.money(r.value, r.currency)})")

        _, bot = await asyncio.to_thread(watch.render, BOT_CHECK_PAGE)
        blocked = watch.Watcher(cfg, render=lambda url, **kw: ("<p></p>", bot))
        try:
            await blocked.read(watch.Watch(id="b", kind="page", target=BOT_CHECK_PAGE, label="AirPods",
                                           condition="below", value=1, via="browser"))
            raise AssertionError("the bot check was read as a page (or the site stopped showing one)")
        except watch.FetchError as e:
            assert e.status == 403 and "bot check" in e.reason, e.reason
        print("4b. vision saw the bot check and called it blocked (the watch would move to search)")

    asyncio.run(run())
    print("WATCH_SMOKE_OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
