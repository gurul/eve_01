# Events from Ticketmaster

eve finds concerts, games, theatre, comedy and other ticketed events by place and date with one tool,
`events_search`, on Ticketmaster's Discovery API (`bridge/src/eve_bridge/events.py`).

Why (owner, 2026-10-08: "this should also be using ticketmaster"): "any concerts in Seattle this weekend" went to
`web_search`, which timed out on a slow night. Ticketmaster answers the same question in about 300 ms.

## When it is offered

- `EVE_MENU_TOOLS` is on (the default since the v4 blind tool-choice eval passed, 2026-10-09), and
- `TICKETMASTER_API_KEY` is set in eve's environment. The key is read at each call and is never logged, returned or
  kept.

While it is offered, every turn carries it (an always-carried toolset, [toolset router](toolset-router.md#always-carried-toolsets)),
with one line in the instructions. That line names `watch_add` only while the watcher offers the `ticketmaster`
kind. A rundown turn does not carry it.

## What it takes and returns

| Argument | Meaning |
|---|---|
| `city` | A city, or "city, ST" ("Portland, OR"); empty for near the owner or anywhere |
| `near` | True for "near me", "nearby", "around here" with no city |
| `keyword` | An artist, team, show, venue or genre; empty for any |
| `category` | `music`, `sports`, `arts & theatre`, `film`, `family`, or empty for every kind |
| `start_date`, `end_date` | `YYYY-MM-DD` in the owner's own days; empty for from now / no end |

It returns up to 8 upcoming events, soonest first. Each has its name, local date and time, venue, city, on-sale
status, genre, a price range when Ticketmaster lists one (a missing range means unknown, never free) and the ticket
link, plus the total number found.

## Near the owner

"Concerts near me" sent no city, so Ticketmaster searched the whole country (8 of 10,000 events, none local;
2026-10-18). A search with no city is now near the owner when `near` is true, or when it has no keyword either (a
what's-on search across the whole country is never what was meant). It goes to Ticketmaster as `latlong` with a
50-mile radius around where the owner is: the phone's fresh fix ([phone location](phone-location.md), at most an
hour old), else `EVE_WEATHER_LAT` / `EVE_WEATHER_LON`. Both are rounded to two decimals (about 1 km), as the
weather's are. The result says `near: "within 50 miles of where the owner is"`. With neither known, the search
goes everywhere and its `note` asks the brain to ask which city. An artist's tour or a team's schedule (a keyword,
`near` false) still searches everywhere.

## Days and time zones

A day is the owner's midnight to midnight, sent to Ticketmaster in UTC. Each midnight uses that day's own UTC
offset, so a range across a daylight-saving change is not an hour off. `datetime.now().astimezone()` carries a fixed
offset (today's); such an offset is read as the Mac's own zone, which applies the right offset on each day
(`events.midnight`). "Today" starts now: what began this morning is not news.

## Rate limits and failures

The key's quota (5 a second, 5000 a day) is shared with the Ticketmaster watches, so a search goes through the
watcher's rate limiter when a watcher runs, the same way a watch does:

- a back-off Ticketmaster asked for is kept: the search sends nothing and says when to try again;
- the watches' own pacing (20 seconds between two requests to one host) is not waited: it refused a retry three
  seconds after a first search (2026-10-18), and the key allows 5 a second;
- each search is counted (`take`); a 429 or 503 backs the key off for the watches too (`penalize`, with the
  server's Retry-After); an answer ends a back-off (`clear`).

A search never raises and never fails the turn. No key, a timeout (6 seconds), a refusal or an unreadable answer is
an `ok: false` result with a plain reason, by HTTP status only, never with the URL (it carries the key).

## What it is not

Telling the owner when tickets go on sale is a watch (`watch_add`, kind `ticketmaster`, `watch.py`). Booking or
buying tickets is not done by this tool.

## Tests

`bridge/tests/test_events.py` (no network): the query in UTC, daylight-saving days in a real zone and in a fixed
offset, trimming, failures by status with no key in any reason, the limiter's back-off, wait, count and penalty.
