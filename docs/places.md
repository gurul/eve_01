# Places: where you go, your habits, and where things are

Owner, 2026-10-18: "make a location service that tracks location ... digests it tracks patterns habits where
things are ... cuz rn u get pure coordinates". Before this, eve kept only the phone's latest fix
([phone location](phone-location.md)), and the brain saw a latitude and a longitude. Now the Mac keeps the history
and turns it into words: "Home, since 18:40", "Equinox, usually Mon, Wed and Fri around 18:00, about 1 h",
"Wednesday: Home 00:00-08:30; Work 09:00-17:30; Home 17:50-now. Missed: Equinox".

The Mac's side is `bridge/src/eve_bridge/places.py`; the phone's is `ios/EveLink/App/LocationSharer.swift` and
`ios/EveLink/Shared/PhoneLocation.swift`.

## What eve can answer

| You ask | eve uses |
|---|---|
| "where was I on Saturday afternoon", "when did I get home last night" | `places_recall` timeline |
| "how was my day", "what did I do yesterday" | `places_recall` day: the places in order with times, time at home, at work and out, about how far you moved, new places, usual places you missed |
| "how was my week" | `places_recall` week: the top places by time and days, new places, your routine |
| "how often do I go to the gym", "what's my routine" | `places_recall` habits: each place's usual days, arrival time and stay over the last 8 weeks, and when you usually leave home and get back on weekdays |
| "what places do you know" | `places_recall` places |
| "this is Mom's", "call the climbing gym Crux" | `place_name` |
| "I parked on level 3", "where's my car", "I left my jacket at Sam's" | `thing_place`: remember, find (with how far it is from here and an Apple Maps link), list, forget |

Every turn's system context also says where you are now, by name, when the phone has shared a fix in the last
hour: "Where the owner is now, from their phone: Equinox (fitness center), since 18:02." The `phone_location`
tool's answer carries the same (`at`). The tools ride with `phone_location`: they are offered once the phone has
shared a location, in the chat, the app chat and voice.

`place_name` and `thing_place` write the places history, so the tool gate counts them as acts with the `routine`
approval policy: they never ask, after a read or not. Their answers can carry the phone's geocoder's words (a
business's own name), so they still mark the turn as having read outside text, as `places_recall` does.

## Where the evidence comes from

- **Every fix** the app already sends (`POST /api/link/location`) is kept as well. Fixes within 150 m of each
  other for 10 minutes or more are a stay. When a fix lands far away, the stay ends at that fix minus the travel
  time at 50 km/h, and is marked approximate.
- **iOS visits** (`POST /api/link/visit`, with "Always" location permission): iOS works out arrivals and departures
  itself at almost no battery cost, and the app adds the nearest point of interest within 60 m from Apple Maps
  (a name like "Equinox" and a kind like "fitness center"). A visit replaces the stays read from fixes for the
  same time. A visit that cannot be sent waits on the phone (at most 50) and goes with the next send.

A visit joins the nearest known place within 120 m (up to 250 m for a vague fix), or the same point of interest
within 300 m; otherwise it is a new place. A fix vaguer than 1 km is kept but says nothing about a stay.

**Home and work** are inferred every hour over the last 21 days: home is where the most night hours (00:00-06:00)
are spent, on at least 3 nights; work is another place with the most weekday hours (09:00-17:00), on at least 3
days and 10 hours in all. Your own name for a place always wins.

**Habits** are counts and medians, no model: a weekday is "usual" for a place when it was visited on that weekday
in at least half the weeks seen (and at least twice). A usual place you did not go to on its usual weekday shows in
that day's digest as missed, once the day is over.

Digests are computed when asked, so a visit the phone sends late is never left out.

## Where it is kept

One SQLite file, `~/.config/eve-bridge/places.db` (`EVE_PLACES_FILE` moves it), owner-only (0600):

| Table | Holds | Kept |
|---|---|---|
| `fixes` | every fix: time, coordinates, accuracy, speed, the phone's place name | 30 days |
| `visits` | arrival, departure, the place, where the evidence came from | `EVE_PLACES_DAYS` (365 by default) |
| `places` | a center, your name, home/work, the point of interest, its kind, the address | while a visit or thing points at it, or you named it |
| `things` | the thing, your note, where, when | until you forget it |

Nothing in `places.py` sends anything anywhere. The brain's tools answer with place names and local times; only a
thing's map link carries coordinates (five decimals), so the phone can open Apple Maps there. Logs say what was done
("visit arrival", "a stay ended"), never a place or a coordinate.

## Turning it off and forgetting

- `EVE_PLACES=0` in `~/.config/eve-bridge/env`: nothing is kept, the tools are not offered, the routes answer 403.
  It is also off whenever `EVE_PHONE_LOCATION` is off, and in eve lite.
- **Eve Link, Settings, Location:** "Places eve knows" shows the count; **Forget location history** (after a
  confirmation) wipes the places, visits, fixes, things and the latest fix (`POST /api/link/places/forget`).
- On the Mac: `eve-bridge places forget` (asks first; `--yes` does not).

## In the terminal

```bash
eve-bridge places day              # today's digest; or a date: eve-bridge places day 2026-10-17
eve-bridge places week             # this week
eve-bridge places habits
eve-bridge places summary
```
