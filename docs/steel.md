# eve's browser on Steel (local, no API key)

Owner, 2026-10-13: "replace browser with steel for eve", then "local steel".

`EVE_BROWSER_POLAR=1` wins over Steel: with it on, the Steel lane is not built and browser-only web
tasks go to Polar's agent instead ([Polar](polar.md); Polar 32/32 against Steel 23/32 in a blind test,
2026-10-24).

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

## Bot checks (challenge pages)

Steel's Chrome is still an automated Chrome, so some sites answer with a bot check
instead of the page. The browser lane recognises one on every look at the page
(`browser_lane.browser_blocker`, ported from LayaBrowse, MIT):

- the path starts with `/splashui/challenge`;
- Google's `/sorry/` page, on a Google host only (`/sorry/` alone is an ordinary path);
- the title is "Just a moment", "Access denied", "Robot check", "Pardon our
  interruption" or "Attention Required! | Cloudflare";
- Cloudflare's "checking your browser" or "needs to review the security of your
  connection" in the first 500 characters of the page.

Two kinds behave differently. Cloudflare's "Just a moment" title and its "checking your
browser" / "review the security of your connection" text are a JavaScript check that
usually clears itself in about five seconds, so the lane looks again every second
(`CHALLENGE_POLL_SECS`) for at most eight (`CHALLENGE_SETTLE_SECS`) before it blocks,
and it does not wait twice on the same URL. Every other check (the challenge path,
Google's `/sorry/`, "Access denied", "Attention Required! | Cloudflare") blocks at
once. Tests: `bridge/tests/test_challenge_settle.py`.

On a bot check that stays up, the lane stops and touches nothing. Every click, type, key press and
vault fill is refused (`refused: blocked:`), a plan ends with status `blocked` (reason
`browser_challenge`) before step 1 or as soon as a step lands on one, and the page
text is not waited for. The task is **not** handed on to Codex or the screenshot
planner, because they would try to solve the check. eve says one line that names the
site only, never the path or query:

> google.com is showing a bot check, so I stopped: I can't get past it myself. Open
> google.com in Chrome, finish the check, then ask me again.

An ordinary page whose title or text only mentions these words (an AWS page about
"Access Denied" errors, a search for "sorry") is not a bot check. Tests:
`bridge/tests/test_browser_challenge.py`.

## Known limits

- Steel runs one session at a time. A `steel` CLI session started by hand replaces
  eve's, and the reverse.
- Upstream steel-browser reports a session timeout of 0, and Steel's CLI reads that
  as expired. eve does not read it. The local checkout carries a two-line patch
  (`SESSION_TIMEOUT_MS`, default 24 h) so the `steel` CLI works against it too.
- Upstream ignores a session's `userDataDir`; eve sets `CHROME_USER_DATA_DIR` when it
  starts the server instead, so a server started by hand uses its own temp profile.
- The server stops on SIGINT; SIGTERM alone did not stop it (2026-10-13).
- Sites that block automated Chrome may still block Steel's, stealth or not; eve then
  stops and tells you ([bot checks](#bot-checks-challenge-pages)). Google sign-in
  through Steel is untested.

## Staying on the newest Steel

Owner, 2026-10-13: "setup a hook for steel opensource repo like we did for openui". The Steel checkout runs a local
branch, `eve`: upstream plus eve's patch (the reported session timeout and `steel-local.sh`), committed so updates
merge cleanly. `sh tools/steel_update.sh` fetches upstream; when there are new commits it merges them into a copy of
`eve` in its own worktree (`../steel-browser-update`, a merge, never a rebase), installs with `node@22`, builds the
API and smoke-tests it on a spare port (127.0.0.1:3100, DevTools 9334, a throwaway profile: a session opens and the
scrape route reads example.com). All green: it fast-forwards `eve`, rebuilds the live checkout, stops the running
server (eve starts the new build on its next web task) and updates the `steel` CLI. Anything failing changes nothing
and eve says what failed (ntfy). `--dry-run` only reports; `--schedule` runs it nightly at 05:10 (LaunchAgent
`com.github.eve-bridge.steel-update`, log `~/Library/Logs/eve-steel-update.log`); `--unschedule` stops it. It never
pushes.

## Speed, measured 2026-10-13

The same steps through eve's browser lane (connect, then open and outline three pages):

| | Connect (warm / cold) | Wikipedia | Hacker News | example.com |
|---|---|---|---|---|
| eve's own Chrome | 183 ms / 1.3 s | 549 ms | 476 ms | 452 ms |
| Steel | 550 ms / 2.7 s | 357 ms | 226 ms | 240 ms |

A page is 35-55% faster on Steel; connecting is slower (a fresh Steel session), but the daemon connects once on the
first web task and keeps the lane.

## Tests, 2026-10-13

`bridge/tests/test_steel_local.py`: configuration and precedence; the server start
(loopback, eve's profile, a new session); refusing a Steel URL off this Mac; a
session without a local websocket refused; the lane attaching, its error, and
closing (eve's tab, then release); Meet staying on eve's own Chrome (positive
control: it fails with that change removed). Live (`EVE_LIVE=1`): with Steel
stopped, the lane started it, attached and read example.com's outline in 2.2 s.
