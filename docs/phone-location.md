# The phone's location

Owner, 2026-10-12: "give access to live phone location through app". The Eve Link app sends the iPhone's
location to the bridge on the Mac. eve can then answer "where am I", "what's near me", "how long to get there
from here" and the weather where the owner is, in the chat, the app chat and out loud.

The phone's side is `ios/EveLink/App/LocationSharer.swift` and `ios/EveLink/Shared/PhoneLocation.swift`; the
Mac's side is `bridge/src/eve_bridge/phone_location.py`. The Settings row is described in
[the app chat doc](app-chat.md).

## What the phone sends, and when

Each send is one fix: latitude, longitude, how accurate it is in meters, when the phone took it, a place name
the phone looked up itself, and the speed when iOS knows it. Nothing else: no route, no other app's data. With
Always, the app also sends iOS's visits (arrived and left a place, with its nearest point of interest); the Mac
keeps both in the [places](places.md) history.

- **Permission.** Once the app is paired and open, it asks for "While using the app", then once for "Always".
  Approximate location (Precise Location off in iOS) works; the Settings row says so.
- **With the app open**, it takes standard updates at about 100 m accuracy, every 50 m of movement. When the
  phone is still, the updates restart every 10 minutes so the Mac's copy stays fresh.
- **In the background**, only with Always, it uses significant-change monitoring: iOS wakes the app after a
  move of about 500 m, even after the app was closed, and the app sends that fix.
- **A fix is sent** when it is the first one; when the phone moved at least 100 m and more than the fix's own
  error; when the fix got a lot sharper (half the error or less, and at least 50 m better); or when the same
  place is 10 minutes old with the app open (30 minutes in the background). A fix more than 10 minutes old on
  arrival is not sent, and two sends are at least 15 seconds apart.
- **The place name** comes from the phone's reverse geocoder (Apple's), at most once a minute and reused within
  200 m: a street for a sharp fix (100 m or better), the town otherwise.

## Battery

The cost is low by design. In the background there are no standard updates and no timers: significant-change
monitoring rides on the cell radio and is the cheapest location service iOS has. Standard updates run only
while the app is open, at 100 m accuracy (not GPS-precise), and iOS pauses them when the phone is still.
Place lookups are rate-limited and cached. Turning sharing off stops every location service.

## The route

`POST /api/link/location` on the Mini App's server, through the tunnel. It uses the same door as every other
`/api/link/*` path (`link_chat.py`): the link token as `Authorization: Bearer`, and any `Origin` gets 403
before the token is read.

```json
{"lat": 0.0, "lon": 0.0, "accuracy_m": 15.0, "at": "2026-10-12T17:55:00Z",
 "place": "Null Island", "speed_mps": null}
```

| Field | Rule |
|---|---|
| `lat`, `lon` | numbers, -90..90 and -180..180 |
| `accuracy_m` | horizontal accuracy in meters, 0..100000 |
| `at` | when the phone took the fix, ISO 8601 with a zone; at most 5 minutes ahead of the Mac's clock |
| `place` | the phone's reverse-geocoded name (a street or a town), or null; one line, at most 120 characters |
| `speed_mps` | meters per second, 0..350, or null; a negative value (iOS's "invalid") is kept as null |

The answer is `{"ok": true}`. A body that does not follow these rules gets 400 with a reason, and nothing is
kept. A fix older than the one kept, or the same one again, is answered `{"ok": true}` and ignored, so a late
retry never moves the owner back. With `EVE_PHONE_LOCATION=0` the route answers 403.

## What is kept, and where it goes

- **Only the latest fix.** It is one file, `~/.config/eve-bridge/phone-location.json`
  (`EVE_PHONE_LOCATION_FILE` moves it). The file is replaced atomically (a temp file in the same folder,
  then `os.replace`) and is owner-only (0600). This file has no history; the [places](places.md) history (on
  unless `EVE_PLACES=0`) keeps every fix for 30 days and the visits read from them for a year.
- **Never logged.** The log line is `link chat: location updated (age 12 s, accuracy 15 m)`, or `ignored,
  not newer`. It never has coordinates or the place. The weather calls to Open-Meteo do carry coordinates,
  and httpx logs each request line with its URL, so quick answers put a filter on the `httpx` logger that
  shows `latitude=<hidden>&longitude=<hidden>` in any Open-Meteo URL (`quick_answers.CoordinateFilter`).
- **Never sent anywhere by the bridge**, except in three places. First, the `phone_location` tool's answer
  goes to the model that asked for it, like any tool result. It is not written to the transcript. Second,
  quick answers send a fresh fix's coordinates to Open-Meteo for the weather, rounded to two decimals
  (about 1 km). Third, an `events_search` near the owner ("concerts near me") sends the same rounded
  coordinates to Ticketmaster ([events](events.md#near-the-owner)); its URL is never logged.

## The tool

`phone_location` takes no arguments. It answers with this shape:

```json
{"place": "Null Island", "lat": 0.0, "lon": 0.0, "accuracy_m": 15, "age_min": 3, "stale": false}
```

`stale` is true when the fix is more than 60 minutes old. When the phone has not sent a fix yet, the answer
is `{"error": "no location from the phone yet"}`.

The chat brain (`telegram.py`, which also serves the app chat) and the voice backend (`voice_agent.py`)
offer the tool when the switch is on and the phone has sent at least one fix. The fix can be any age: the
tool reports when it is stale. A Mac whose phone never sent a location sends the same prompts as before,
byte for byte. The camera's tools work the same way: they are offered only when there is a camera. Think out
loud (a lesson) never offers it.

The taint gate (`tool_gate.py`) treats the tool as a read. It never asks for confirmation. Its result taints
the turn like any read, because the place name comes from the phone's geocoder and can be a business's own
name. The intent eval (`bridge/tools/intent_eval.py`) labels it `phone_location` and gives each run a
fictional fresh fix (a city center) so the tool is offered. The toolset router does not route it: like the
camera's tools, it rides every turn while it is offered.

## Quick answers

"What's the weather" and "when is sunset" are answered by code (`quick_answers.py`). They use a fresh phone
fix (at most 60 minutes old) as "here", named by its `place`, or "where you are" when it has no name. With no
fresh fix, or with the switch off, "here" is `EVE_WEATHER_PLACE` or `EVE_WEATHER_LAT`/`EVE_WEATHER_LON`, as
before. A place the owner names ("weather in Tokyo") always wins.

## Turning it off

- **On the phone:** Settings in Eve Link, **Share location** off. It stops every location service at once.
  iOS's own Settings (Eve Link, Location) can also take the permission away.
- **On the Mac:** `EVE_PHONE_LOCATION` in `~/.config/eve-bridge/env` is on unless it is `0`, `false`, `no` or
  `off`. When it is off, the route answers 403 (the phone's Settings row says location is off on the Mac), the
  tool is not offered, a stray call gets a reason instead of the fix, and quick answers ignore the file.
- Neither switch deletes the fix the Mac already has. To forget it and the places history, use **Forget
  location history** in Settings or `eve-bridge places forget`.

## Privacy

- The Mac keeps the latest fix in one owner-only file, and the [places](places.md) history in another
  (`places.db`, owner-only). `EVE_PLACES=0` keeps no history; Settings, **Forget location history** wipes both.
- The phone keeps only the last fix it sent and the last place name, in the app's own storage, to decide when
  to send next. It never leaves the phone except as the next send.
- Neither side logs where the phone is. The Mac's log and the app's diagnostics say only how old and how
  accurate a fix was.
- The fix goes from the phone to this Mac through eve's own tunnel, with the link token. The bridge sends it
  nowhere else, except the three places listed above: the model that called the tool, Open-Meteo (rounded to
  about 1 km) for the weather, and Ticketmaster (rounded the same) for events near the owner.

## Check it is live

```bash
bridge/.venv/bin/python bridge/tools/phone_location_check.py
```

This reads the fix the way the running daemon does. It uses the state file that the daemon's env file
(`~/.config/eve-bridge/env`) names, or the default, and the switch from the same file. It prints only the
fix's age in minutes and its accuracy in meters. It prints `LOCATION_LIVE_OK` and exits 0 when the fix is
less than 30 minutes old, and exits 1 otherwise.

## Tests

`bridge/tests/test_phone_location.py` covers parsing, newest-only atomic 0600 storage, the tool's answers,
the switch, each surface, quick answers and the check. `bridge/tests/test_link_chat.py` covers the route:
auth, Origin, 400, 403, 413, 500, an ignored older fix, the log's words, and an app turn that posts a fix and
asks. The suite never touches the real file, because `conftest.py` points `EVE_PHONE_LOCATION_FILE` at a
temporary path.

## Files

| File | What it is |
|---|---|
| `ios/EveLink/App/LocationSharer.swift` | the phone's location manager, permission, sends, Settings state |
| `ios/EveLink/Shared/PhoneLocation.swift` | the wire format and the send, skip and place-name rules (tested on the Mac) |
| `ios/EveLink/App/SettingsSheet.swift` | the Location section in Settings |
| `ios/EveLink/project.yml` | the two location purpose strings and the `location` background mode |
| `bridge/src/eve_bridge/phone_location.py` | parsing, storage, the tool and the switch |
| `bridge/src/eve_bridge/link_chat.py` | the `POST /api/link/location` route |
| `bridge/tools/phone_location_check.py` | the live check |
| `~/.config/eve-bridge/phone-location.json` | the latest fix (0600; `EVE_PHONE_LOCATION_FILE` moves it) |

The iOS checks are in `ios/EveLink/Tests/main.swift` (`sh ios/EveLink/Tests/run.sh` prints `LOCATION_OK`).
