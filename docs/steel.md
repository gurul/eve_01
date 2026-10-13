# eve's browser on Steel (local, no API key)

Owner, 2026-10-13: "replace browser with steel for eve", then "local steel".

With `EVE_BROWSER_STEEL=1`, eve's web tasks run in a Chrome driven by
[Steel](https://github.com/steel-dev/steel-browser) (Apache-2.0), the self-hosted
browser API, on this Mac. Nothing goes to Steel's cloud and no Steel API key is used.
Steel replaces the Chrome that the browser lane attaches to. Everything above it is
unchanged: astra plans once, Jev grounds each step, a sensitive control (buy, send,
delete, pay…) stops for your yes, and anything unfinished goes to Codex
([routing](stackchan/routing.md)).

```
web task ─▶ browser lane ─▶ steel_local.ensure_server()   start steel-local.sh when /v1/health does not answer
                         ─▶ steel_local.start_session()   POST /v1/sessions → ws://127.0.0.1:3000/
                         ─▶ Playwright connect_over_cdp   eve's tab in Steel's Chrome
done ─▶ release the session (Chrome closes and writes the profile)
```

## Set up

1. A steel-browser checkout, built, at `~/Documents/personal/steel-browser`
   (`EVE_STEEL_DIR` moves it), with its `steel-local.sh`. That script binds
   127.0.0.1 only (upstream listens on every interface with no auth) and moves
   Steel's Chrome DevTools port from 9222 to 9333, off eve's own Chrome.
2. Homebrew's `node@22` (`brew install node@22`; keg-only, your default `node`
   is untouched). Node 26 cannot build steel-browser's `duckdb`.
3. In `~/.config/eve-bridge/env`: `EVE_BROWSER_STEEL=1`, then restart the daemon.
   The startup line reads `chrome lane: on — web tasks try eve's Steel browser first …`.
4. Sign in to the sites eve may use: `eve-bridge steel-profile [url]` opens a Steel
   session in a window on eve's Steel profile (default: Google sign-in); press Enter
   in the terminal to save and close it.

eve starts the Steel server itself the first time a web task needs it (about 2 s
cold), and leaves it running between tasks, like eve's own Chrome. Its log is
`~/Library/Logs/eve-steel.log`.

## Switches

| Variable | Default | What it does |
|---|---|---|
| `EVE_BROWSER_STEEL` | off | Web tasks run in local Steel. Turns the browser lane on by itself and takes precedence over `EVE_BROWSER_OWN` and `EVE_BROWSER_ATTACH`. |
| `EVE_STEEL_URL` | `http://127.0.0.1:3000` | Where Steel answers. Must be on this Mac (127.0.0.1 or localhost); anything else is refused. |
| `EVE_STEEL_DIR` | `~/Documents/personal/steel-browser` | The checkout eve starts the server from. |
| `EVE_STEEL_PROFILE` | `~/.config/eve-bridge/steel-profile` | Steel's Chrome user data directory: eve's sign-ins (0700). |

## What stays on Chrome

- **Meet** joins from eve's own Chrome even with Steel on: Steel's Chrome is
  headless with its sound muted, and a meeting needs real audio
  ([meet](stackchan/meet.md)).
- **Codex** computer tasks keep using your own Chrome, where its browser
  extension lives.

## Known limits

- Steel runs one session at a time. A `steel` CLI session started by hand replaces
  eve's, and the reverse.
- Upstream steel-browser reports a session timeout of 0, and Steel's CLI reads that
  as expired. eve does not read it. The local checkout carries a two-line patch
  (`SESSION_TIMEOUT_MS`, default 24 h) so the `steel` CLI works against it too.
- Upstream ignores a session's `userDataDir`; eve sets `CHROME_USER_DATA_DIR` when it
  starts the server instead, so a server started by hand uses its own temp profile.
- The server stops on SIGINT; SIGTERM alone did not stop it (2026-10-13).
- Sites that block automated Chrome may still block Steel's, stealth or not. Google
  sign-in through Steel is untested.

## Tests, 2026-10-13

`bridge/tests/test_steel_local.py`: configuration and precedence; the server start
(loopback, eve's profile, a new session); refusing a Steel URL off this Mac; a
session without a local websocket refused; the lane attaching, its error, and
closing (eve's tab, then release); Meet staying on eve's own Chrome (positive
control: it fails with that change removed). Live (`EVE_LIVE=1`): with Steel
stopped, the lane started it, attached and read example.com's outline in 2.2 s.
