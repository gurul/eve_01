# Watching prices, stocks and ticket releases

The owner asked on 2026-09-25: "build in a feature to monitor prices, stocks,
ticket prices, etc using a rate limiter … let me know when prices drop/increase
for a concert ticket or maybe like a concert ticket is out like released".

Text eve what to watch, and it checks on a schedule and texts you when it
happens. Some examples:

- "tell me when AAPL drops below 300"
- "watch bitcoin and tell me if it rises 5%"
- "let me know when this is under $80: https://…"
- "tell me when Olivia Rodrigo tickets in Seattle go on sale"
- `/watch VOO below 500`

`/watch` on its own lists what eve is watching, answered by code with no model
call. `/watch <words>` is a watch request for the text brain. "stop watching w2"
removes one. Alerts arrive as a new message titled **Watch**, and they join the
chat's history, so you can reply to one directly ("stop watching that").

**Pause, resume, and a watch for a while.** "pause w1" stops its checks and
alerts until "resume w1". "pause w1 for 2 days" resumes it by itself. A paused
watch keeps its mark and its last reading, and it stays paused across a
restart. "watch AAPL below 300 for the next week", "until Friday", or "stop w1
after the presale" gives a watch an end. At that time it is removed, and you
get a text saying so. `/watch` shows each watch's state: paused, paused until
when, or watched until when.

Code: `bridge/src/eve_bridge/watch.py`. Tests: `bridge/tests/test_watch.py`
and the watcher test in `test_telegram.py`. Live check:
`bridge/tools/watch_smoke.py`.

## The switch

`EVE_WATCH` is **on** by default whenever the Telegram door is on. Nothing
is fetched until you add a watch. Set `EVE_WATCH=0` to turn it off. The
watch list lives in `~/.config/eve-bridge/watches.json` (mode 600, written
atomically), so watches and their state survive a restart. `EVE_WATCH_FILE`
moves it.

## What it can watch

| Kind | Target | How it reads | Cost per check |
|---|---|---|---|
| `quote` | A Yahoo Finance symbol: `AAPL`, `VOO`, `BTC-USD`, `VOD.L` | The chart endpoint's regular-session price. Pence, cents and agorot (`GBp`, `ZAc`, `ILA`) are converted to pounds, rand and shekels. Outside market hours, the listing also shows Yahoo's extended-hours price. | Free |
| `page` | A link to a product or ticket page | See [how a page is read](#how-a-page-is-read). | Free when the page states its own price. Otherwise a model call: a screenshot read measured $0.00038 (13 calls, 2026-09-25); a text read is not measured yet |
| `search` | A question with no link: the artist or item, the city or store, and the dates | One OpenRouter call with its web search tool. The answer is JSON: on sale or not, lowest price, link. | One OpenRouter call with web search; not measured yet (shown in `/spend` under watching) |
| `ticketmaster` | An artist, team or show, or a ticketmaster.com event link, plus an optional city | Ticketmaster's Discovery API. Needs `TICKETMASTER_API_KEY`. | Free (5,000 calls a day) |

The text brain is offered only the kinds this Mac can watch. Without a
Ticketmaster key there is no `ticketmaster`.

## What it can watch for

| Condition | Fires when |
|---|---|
| `below` / `above` | The price crosses the mark. It fires once, then re-arms only after the price has gone back over (or under) the mark. |
| `drop_pct` / `rise_pct` | The price moves the percentage away from the baseline. The baseline then becomes that price, so a second 10% drop is 10% from there. |
| `change` | Any new price, or a flip in availability. |
| `available` | The item comes into stock, or the tickets go on sale. It re-arms when the item is sold out again. |
| `appears` | A phrase shows up in the page's visible text. Case and spacing are ignored. |

A condition never fires on the first reading `watch_add` shows you: that reply already says
"it's already under 300". If the add's own check failed or was queued, you never saw that reading,
so a condition that already holds on the first background check does fire.

## How a page is read

First the page is fetched. Some shops refuse a plain Python request no matter
what its headers say, because its TLS handshake is Python's rather than the
Chrome its user agent claims. If the plain read gets a 401 or 403, or no answer
within 20 s, eve fetches the page once more with **`curl_cffi`**, which is
libcurl built to shake hands the way Chrome does. A page that needed this goes
straight to it on every later check. The check interval is unchanged: this
fetch costs no more than a plain one. A 429 is never retried this way. It's a
request to slow down, and eve honours it. This needs
`pip install -e ".[tls]"`. `EVE_WATCH_TLS=0` turns it off.

Measured on 2026-09-26 from a home connection:

| Site | Plain read | Read as Chrome would |
|---|---|---|
| LEGO | 403 | $849.99, from its own data, in about 1 s |
| Best Buy | No answer in 20 s | $84.99, from its own data, in about 2 s |

The same test showed Scrapling, which wraps `curl_cffi` with extra headers,
being blocked by StubHub and Walmart. Both read fine with `curl_cffi` alone,
so eve uses `curl_cffi` without Scrapling.

Then the page goes to the cheapest reader that answers:

1. **The page's own structured data.** No model call. This covers:
   - JSON-LD offers (`price`, `lowPrice`, `availability`). The lowest price is
     taken, because a ticket page lists many tiers.
   - `product:price:amount` and `og:price:amount` meta tags.
   - Microdata `itemprop="price"` and `availability`.

   The parser is lenient. It accepts:
   - raw newlines inside strings
   - CDATA and comment framing
   - entity-escaped blocks
   - several objects in one script
   - `1.299,00` and `1,299.00`

   Microdata follows the W3C value rules (`content`, `href`, `value`, then the
   element's text). It only counts a property whose nearest `itemscope` is an
   offer or a product, so a price inside a review is not mistaken for the item's.
   These ideas come from extruct and changedetection.io and were re-implemented
   here; neither is a dependency.
2. **The page's text, read by a cheap model.** This is one OpenRouter call, with
   the site's nav, header, footer and aside stripped first. If the page text has
   not changed since the last check, the previous answer is reused and the check
   costs nothing.
3. **The page in a browser.** Headless Chromium loads the page, waits for the
   network to settle, and presses safe overlay buttons out of the way:
   - a cookie notice's refusing button (browser_lane's rule: never an agreeing one)
   - a region or entry modal's button reading exactly one of: continue, continue to / shopping /
     to site, close, dismiss, ×, ✕, x, no thanks, not now, maybe later, skip, stay here, stay on …,
     enter, enter site. Never a link that navigates. A click that navigated anyway is undone with
     Back, and nothing more is pressed. It looks until two looks in a row a moment apart find
     nothing, since a modal can open just after the page settles.

   It never presses anything that agrees, signs up or buys. Then:
   - The rendered page's structured data is read (free).
   - If that doesn't answer, a **vision model reads the screenshot**. It reports
     the price and whether a buy or get-tickets button is live. It also reports
     whether the screen is a bot check.

   Once a page has needed the browser, it goes straight there on every later
   check, at most every 15 minutes. This step needs Playwright and its Chromium
   (`pip install -e ".[browser]"` then `python -m playwright install chromium`).
   `EVE_WATCH_BROWSER=0` turns it off.
4. **TinyFish** (free, off without a key). A page that refuses every reader
   on this Mac is read through TinyFish Fetch, which reads it on TinyFish's
   side. A plain read and the Chrome-like read got a 401/403, and the browser
   got a 401/403 or a bot check. Once a page has needed TinyFish, it goes
   straight there on every later check, at most hourly. The read path is saved
   as `hosted`.
   - Fetch returns cleaned HTML with the scripts removed, so a price kept only
     in the page's structured data (JSON-LD) is lost. The page's text is read
     by the cheap model instead. (Firecrawl, this step until 2026-09-29,
     returned the raw page.)
   - TinyFish Fetch is free. eve still makes at most
     `EVE_WATCH_HOSTED_CALLS` reads a day (default 100), recorded in the
     spend ledger at $0.
   - eve asks for a live read every time (`ttl: 0`), never TinyFish's cache.
   - TinyFish's own problems never move a watch to search: a bad key, its rate
     limit, a timeout or a proxy error. They count as ordinary failures, and
     "I can't read X" tells you. Only the page's own refusal moves the watch
     on: an HTTP 401/403/404 that Fetch reports, `bot_blocked` (as 403) or
     `login_required` (as 401).

   Set `TINYFISH_API_KEY` in `~/.config/eve-bridge/env`.
   `EVE_WATCH_HOSTED=0` turns it off.
5. **Search.** A page that refuses all of the above is watched by web search
   from then on, at most hourly.

Measured on 2026-09-25 (`tools/watch_smoke.py`):

| Site | What happened |
|---|---|
| IKEA | Answered from its own data, with no model call |
| LEGO | Refused a plain read with 403. Headless Chromium got the page past its entry modal: $849.99, in stock, from the rendered data. The vision model read the same $849.99 off the screenshot. |
| Target | Showed the browser a "press & hold" bot check. The vision model recognised it, so the watch would move on to search. |

## Ticketmaster

With `TICKETMASTER_API_KEY` set (a free key from developer.ticketmaster.com),
concert, game and show on-sale alerts come from Ticketmaster's own data rather
than a search.

- **Available** is judged by Ticketmaster's documented statuses:
  - `onsale` and `rescheduled` count as on sale.
  - Any presale window that is open now also counts, even while the status is
    `offsale`.
  - `postponed` and `canceled` don't count.
  - An `offsale` event whose public sale has not started yet is exactly what you
    are waiting for, so it keeps being watched.
- **Artists** are resolved once to an attraction id. The id is the one whose
  name or alias matches what you said, and never a tribute act. After that, only
  that attraction's events count.
- **Event links**: the hex id in a ticketmaster.com link is not the API's event
  id, so the watcher searches for the link's words and keeps the event whose
  `url` ends in that id.
- **Prices are refused.** Ticketmaster removed `priceRanges` from its feeds in
  March 2025, so a ticket price has no reliable source there. To watch a ticket
  price, watch the event page (`page`) instead.
- A 429 carries `Rate-Limit-Reset` (epoch ms) rather than `Retry-After`. The
  limiter reads either.

## The rate limiter

Every request that leaves the Mac goes through one limiter, and every model call
counts against one daily cap:

| Setting | Default | What it does |
|---|---|---|
| `EVE_WATCH_RATE` | 12 per minute | A global token bucket over all hosts |
| `EVE_WATCH_BURST` | 4 | Requests that may go back to back after a quiet spell |
| `EVE_WATCH_HOST_GAP` | 20 s | The minimum gap between two requests to one host |
| (fixed) | 60 s, doubling, capped at 1 h | A host's backoff after a 429 or 503. It is at least the server's `Retry-After` (or `Rate-Limit-Reset`), and a success clears it. |
| `EVE_WATCH_MODEL_CALLS` | 48 per day | Text reads, vision reads and searches together. Past the cap, a check that needs a model waits for the next day. |

How often a watch may be checked, at least:

| Kind | Most often |
|---|---|
| Quote | 1 min |
| Page | 5 min |
| Page read in the browser | 15 min |
| Page read through TinyFish | 1 h |
| Ticketmaster | 5 min |
| Search | 1 h |

The defaults are 15 min for a quote, 30 min for a page, 30 min for Ticketmaster
and 6 h for a search. Each next check has ±10% jitter, so watches on one host
never march in step.

A failing watch is checked less often:
- It backs off 2×, 4×, up to 8× its interval.
- After four failures in a row, you are told once ("I can't read X right now").
- You are told again when it recovers.

The limiter is hand-rolled, not PyrateLimiter or aiolimiter. Neither can say how
long a host must wait without spending a token, and the scheduler needs that to
reschedule a watch instead of blocking. Neither handles Retry-After either. Its
refill mark only moves forward, so a clock that steps back never mints tokens
(the same rule PyrateLimiter's GCRA follows).

Yahoo answers a full Chrome user agent from a non-Chrome TLS stack with a 429,
and the bare `Mozilla/5.0` with a 200 (measured 2026-09-25). So quote requests
send only `Mozilla/5.0`.

## Safety

- **Links**: only http and https links to public addresses are fetched. The
  check and the connection are one step: `_connect_public` resolves the name
  once, refuses unless every answer is public, and connects to one of those
  very addresses. A name that answers a public address to the check and
  `127.0.0.1` to the connection (DNS rebinding) can't reach the LAN.

  These are refused:
  - loopback, private, link-local and the cloud metadata address
  - any address inside a network on this Mac's own interfaces, read from
    `ifconfig` and cached for a minute. A home network's IPv6 devices, this
    Mac included, have global addresses that `ipaddress` calls public. If the
    interfaces can't be read, no global IPv6 address is trusted.
  - NAT64 and IPv4-compatible IPv6 forms
  - an IPv4-mapped, 6to4 or Teredo address carrying a private IPv4
  - a redirect to any of them

  Proxies from the environment are ignored, since they would make the check
  moot.
- **The Chrome-like read** (`tls_request`) keeps the same guarantees:
  - Every hop is checked with `check_url`.
  - Each hop's name is resolved once, and every answer must be public.
  - libcurl is pinned to the checked address with `RESOLVE`, so it never makes
    a second lookup that could be rebound.
  - After the exchange, the address libcurl reports it used must be the pinned
    one.
  - Redirects are followed by eve, never by libcurl, so each hop is checked
    the same way.
  - The rest matches the plain read: no proxy from the environment, http and
    https only, one deadline for the whole answer, and the same size cap.

  Live check: plain addresses and public names that point at the LAN are
  refused, such as a `nip.io` name for the cloud metadata address and a public
  redirector bounced to `127.0.0.1` or `10.0.0.1`. The same redirector pointed
  at a public page is followed.
- **The browser**: it runs in a child process with a hard deadline, and the
  whole process group is killed past it, so a page whose script spins can't
  hold the watcher.
  - Chromium's only way out is a local guard proxy that opens every connection
    through the same `_connect_public`, loopback included (`<-loopback>`).
    That covers redirects, WebSockets, iframes, beacons and popups, which a
    page-level route guard never sees.
  - Popups are closed unread, and service workers are blocked.
  - The proxy carries one request per plain-http connection. A kept-alive
    connection used to carry the next request to the first server.
  - Past its deadline, the render is killed along with every process under
    it: Chromium runs in a process group of its own, so killing the child's
    group missed it. The child also kills itself and its descendants when its
    own deadline passes or the daemon is gone.
  - The live test's negative control (`test_watch_reverify.py`) reaches the LAN by fetch, iframe, redirect and WebSocket (and, in the recorded run, beacon and popup) when the guard
    lets everything through, and none with the real guard.
- **Secrets**:
  - A redirect to another host, another port, or down from https to http drops
    `Authorization` and `Cookie`, so the OpenRouter key never follows one.
  - The Ticketmaster key is only ever in a request URL, and never in a log
    line, an error, the chat or the watch file.
- **Models**: page text and screenshots are data. They only ever reach a model
  that answers with JSON and has no tools. The answer is validated before
  anything is texted:
  - a finite, non-negative price that fits a float
  - a real boolean
  - a three-letter currency
  - a short note whose `[` and `]` become `(` and `)`, so no link can hide
    behind words in eve's message
  - a link, kept only from a search

  A page alert always links the page you gave, whatever the page says. Every
  note is made plain in the same way, Ticketmaster's event and presale names
  included.
- **The chat's history never holds words from the web.** The alert you
  receive can carry a search's note or an event's name. The history the
  tool-using chat brain reads keeps only a line built from the watch itself:
  "(I texted a watch alert: w3, Apple, at or below $300.)". Before, a web page
  could put words into the brain's history as if eve had said them.
- **Resources**:
  - The page parsers are linear. The earlier lazy regexes were quadratic:
    250 KB of unclosed script tags stalled the loop for seconds.
  - Parsing runs off the event loop.
  - A fetch has one deadline for the whole exchange, headers included: at the
    deadline its socket is shut, so a server that trickles a byte at a time is
    cut off.
  - An unknown charset or a garbage status line is a failed read, not a crash.
- **Budget**: the daily model cap bounds what a watch list can spend. A clock
  or time zone that steps back onto a spent day finds it still spent. Model
  calls appear under **watching** in `/spend`.

## Robustness

- **One broken watch never stops the others.** Anything a check raises is
  that watch's error: it is counted, backed off, and reported after four in a
  row. It never aborts the tick. It used to: a zero first price, then any
  `drop_pct` check, raised `ZeroDivisionError` on every tick, and every watch
  after it was never checked.
- **Alerts aren't lost:**
  - An alert Telegram refuses is kept on the watch and sent again at the next
    tick.
  - "I can't read X" counts as said only once it was actually sent.
  - A watch you remove while its check is in flight texts nothing more.
- **Readings the owner never saw:** a condition that already holds on the first
  reading you never saw fires. That happens when the add's own check failed or
  was queued.
- **The limiter:**
  - A later refusal never shortens a hold still in force.
  - A check let through before a 429 landed waits the hold out.
  - A model service's 429 holds the model service, not the page's site.
- **Ids are never reused**, even after a removal. A hand-edited watch file is
  read defensively:
  - duplicate ids are dropped
  - wrong types are reset
  - numbers that are not finite, or out of bounds, are ignored
  - the load is linear and stops at 25

  A file that can't be read at all is moved aside as `watches.json.bad-<time>`
  and never saved over. You're told once, and ids carry on from the old file's.
  A watcher that fails to start costs the watching, never the daemon.
- **Currency**: a watch's currency is set by its first priced reading. A priced
  reading in another currency is a failed read, counted and told, never
  compared with a mark in the first currency.
- **OpenRouter's refusals are OpenRouter's.** A 401 or 403 from the model
  service never moves a page to the browser or to search, and a 404 never
  refuses a watch as a bad link. A page's model call waits out OpenRouter's
  own hold.
- **Nothing fails in silence.** A page, whether read plainly or in the
  browser, that no longer states the watched fact is a failed read. A phrase
  watch never falls back to search, which can't see a phrase. It counts the
  refusal instead.

Each of these properties has a pytest replay that failed before its fix.

## Not done

- No voice alerts and no robot chirp. An alert is a Telegram message only.
- No batching: each quote is its own request. Yahoo's crumb-free `v7/finance/spark`
  endpoint could read up to about 20 symbols in one request, but it silently
  drops unknown symbols.
- Ticketmaster prices (see above), and ticket resale sites, which block automated
  reads.
- A **new** Ticketmaster date going on sale while another date is already on
  sale sends no second alert, because "available" means any listed date is on
  sale. One transient empty answer from Ticketmaster can also re-arm an
  `available` watch, so the next on-sale reading texts again. Either change
  would alter the rule the Lean model proves, so they wait for that model's
  next revision.
- The limiter's holds and strikes live in memory: a restart forgets a
  Retry-After, though each watch's own stretched interval is saved. A clock that
  jumps forward and then back holds the limiter until real time catches up.
- Watching the room through the robot's camera ("tell me when my package is at
  the door") would use `scene.py`, not this module.

### Watch model (October 9)

Text extraction, screenshot reads and watch search answers default to
`deepseek/deepseek-v4.1-flash`, independently of chat/web research synthesis.
Reasoning is explicitly disabled so the short JSON response fits the existing
200-token page/vision and 300-token search budgets. `EVE_WATCH_MODEL` and
`EVE_WATCH_VISION_MODEL` still override their respective paths.
The [model comparison](../evals/model-routing-2026-10-09/VERDICT.md) covers the
selection and its limits. Code-based page reads still run before model fallbacks.
