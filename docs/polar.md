# eve's browser is Polar's agent

Owner, 2026-10-24: "use only polar".

With `EVE_BROWSER_POLAR=1`, a web task that needs only the browser goes whole to the agent
inside [Polar](https://polar.app), a Chromium browser with its own agent. eve doesn't click
through Polar. It hands over the task in one sentence and reads back Polar's answer
(`bridge/src/eve_bridge/polar_lane.py`).

```
web task ─▶ route: browser only? ──no──▶ Codex (the Mac is involved)
                 │ yes
                 ▼
            PolarAgent ─▶ consequential (send, buy, post, delete…)? ─▶ ask you first
                       ─▶ local dispatch, 127.0.0.1    {"method":"dispatch","query":"<task> + read-only line"}
                       ─▶ Polar's API /api/v1/threads/<id>             poll until done
                       ─▶ Polar's API /api/v1/threads/<id>/messages   the last answer is eve's reply
```

**Polar only.** A browser-only task never falls back to Steel or Codex. When Polar can't take
it (not running, signed out, out of credits, stuck), eve says why. Tasks that also involve the
Mac (an app, a file) still go to Codex, because Polar can only browse.

## Why Polar

A blind test on 2026-10-24 compared it with eve's browser lane on Steel. It used 16 read-only
web tasks, written before either system ran: pages to find, a site search box, the USPS ZIP
lookup form, a JavaScript-only page, a multi-page category, live values (crates.io, the Rust
site, GitHub, weather.gov, npm) and bot-check sites (IMDb, Steam). Each task ran on both
systems back to back. A judge scored the answers with the systems hidden, checking live values
itself.

| | Polar's agent | eve on Steel |
|---|---|---|
| Score (0–2 per task) | **32 / 32** | 23 / 32 |
| Median time per task | 8.8 s | 9.4 s |
| Slowest task | 24 s | 79 s |

Steel lost points on two answers that named no value, the USPS form, an npm bot check and a
shortened title.

## Set up

1. Install Polar and sign in.
2. In `~/.config/eve-bridge/env`, set `EVE_BROWSER_POLAR=1` and restart the daemon. The startup
   log reads `chrome lane: off — EVE_BROWSER_POLAR hands browser-only web tasks to Polar's agent`.
3. Polar must run with eve's connection. Polar opens its local dispatch listener only when it
   is launched with the `POLAR_USER_DATA_DIR` environment variable set. eve launches it that way
   (`open -g --env POLAR_USER_DATA_DIR=<profile> -a Polar`) when Polar isn't running. If Polar is
   already open without the connection, eve never quits it: the task says to quit Polar, and the
   next web task starts it with the connection.

| Setting | Default | |
|---|---|---|
| `EVE_BROWSER_POLAR` | `0` | `1` sends browser-only web tasks to Polar. Wins over Steel, eve's own Chrome and attach mode |
| `EVE_POLAR_DIR` | `~/Library/Application Support/com.polar.browser.host` | Polar's profile folder. The listener writes `local-dispatch.json` here |
| `EVE_POLAR_APP` | `/Applications/Polar.app` | |
| `EVE_POLAR_TIMEOUT` | `300` | Seconds before eve stops a Polar run (at least 30) |

## Asking before acting

eve's per-click consent can't see Polar's clicks, so your rule (ask before sending, deleting or
buying) is applied once per task. A task with a consequential verb (`task_router.CONSEQUENTIAL`:
send, post, buy, order, pay, book, delete, submit, share, sign in…) asks you first. On a yes it
goes with a line that allows only what was asked. Every other task goes with:

> Read only: do not sign in, buy, post, send, delete or use any connected account; search and
> lookup forms are fine.

A verb in a negation ("don't buy it") still asks. That costs a question, never an action.

## The connection

- `local-dispatch.json` in the profile folder holds `host`, `port` (new at each launch), `pid`,
  `version` and `authToken`. eve reads it for every call and never logs the token.
- The transport is one JSON line over TCP on 127.0.0.1, with one JSON line back. The methods are
  `status` (signed in, ready), `dispatch` (`query` → `threadId`, `runId`, queued), `stop`
  (`threadId`) and `token` (Polar's API address and a token for your account).
- Results come from Polar's API: `GET /api/v1/threads/<id>` (`latest_run_status`) and
  `GET /api/v1/threads/<id>/messages`. Polar's API is behind Cloudflare, which refuses Python's
  default User-Agent, so eve sends its own.

## Limits

- **Credits.** Each task spends Polar credits. The free plan tops up 50 a day, and a task cost
  roughly 17 in testing.
- **Your account.** Polar acts as you, with your Polar sign-ins and connectors, and every task
  appears in Polar's Recents.
- **An undocumented hook.** `POLAR_USER_DATA_DIR` isn't a documented setting, and a Polar update
  could remove it. eve then says that Polar's connection isn't open.
- **No picture.** Polar's tabs are its own, so "show me the screen" during a Polar task sends the
  desktop, not the page.
- **No steering.** A task can't be changed mid-run, only stopped.
