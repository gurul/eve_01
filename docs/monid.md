# Monid: the web search's last resort

[Monid](https://monid.ai) is a paid catalogue of data endpoints behind one
command-line tool. eve uses one of its endpoints, DataForSEO's live Google
results, only when its normal web search cannot answer.

The code is `bridge/src/eve_bridge/monid.py`. The search calls it from
`websearch.engine_search`.

## When it runs

A web search normally asks Perplexity through OpenRouter, and starts Exa as
well when Perplexity is slow or fails. Monid runs **once**, only when:

- both OpenRouter engines failed, and
- neither engine failed with an HTTP 4xx other than 429. A request the
  engines refused (a bad request, a bad key) is not sent anywhere else.

It does not run when an engine answered, when Monid is switched off, when
today's caps are spent, or when less than 3 s of the search's budget is left.

The whole search waits at most 25 s. Monid gets what is left of the 25 s, at
most 12 s. If the engines used the whole budget (both timed out), there is no
time left and Monid does not run.

When Monid cannot answer (the CLI is missing, it has no key, it timed out, it
sent unreadable output, or Google gave no results), the search returns the
first engine's failure, as it did before Monid was added.

## What it returns

The same result the brain gets from any search:
`{ok, answer, sources: [{title, url, snippet}], usage, ms, cost_usd}`, plus
`"fallback": "monid"`.

- **Sources:** Google's organic results (at most 8), with their titles, links
  and snippets.
- **Answer:** one short line built by code from what Google showed on top
  (its AI overview, else a featured snippet) and the top snippets. No model
  call is made.
- **Request:** one query, one page of 10 results, United States, English
  (`location_code` 2840, `language_code` "en", `depth` 10). This is the input
  shape `monid inspect -p dataforseo -e /serp/google-organic` gave on
  2026-10-08.

## Cost and caps

The endpoint costs **$0.002 a call** (one page of 10 results). It usually
takes about 6 s (p50 5.8 s, p95 8.7 s, from `monid inspect`). A live check
on 2026-10-08 took 6.2 s and cost $0.002.

Every run spends the owner's Monid balance. Two daily caps hold it, counted in
one place (`monid.py`) and kept across a restart in
`monid-runs.json` beside the spend ledger:

| Setting | Default | What it does |
|---|---|---|
| `EVE_MONID` | `fallback` | `0` (or `off`) switches Monid off. |
| `EVE_MONID_PER_DAY` | `20` | Monid runs a local day. `0` stops them all. |
| `EVE_MONID_USD_PER_DAY` | `0.25` | Dollars of Monid runs a local day. `0` stops them all. |
| `EVE_MONID_CLI` | (found) | The path of the `monid` command, when it is not in a usual place. |

When a cap is spent, no run is sent and one line is logged. Each run is
recorded in the spend ledger under provider `monid`, with the cost Monid
reports. A run that reports no cost is recorded as unpriced, and its list
price ($0.002) is counted against the day's dollar cap.

## Setup

1. Install the CLI: `npm install -g @monid-ai/cli`.
2. Make an account at [app.monid.ai](https://app.monid.ai) and an API key.
3. Give the key to the CLI: `monid keys add -k <key> -l main`.

eve never reads or holds the key. The CLI keeps it. eve only runs
`monid` and checks that `monid keys list` shows an active key; it reads only
the `active` flag.

eve runs under launchd, which has a short PATH. It looks for `monid` on the
PATH, then in `/opt/homebrew/bin`, `/usr/local/bin` and npm's user-level
global folders. It adds Homebrew's folders to the CLI's PATH, because
`monid` is a Node script and needs `node`.

## How it runs the CLI

- An argument list, never a shell string.
- `NO_COLOR=1` and `-j` (JSON output).
- A hard timeout: the process is stopped a little after its wait.
- No call raises. Every failure comes back as `{ok: false, reason}`.

`monid.run(provider, endpoint, body, wait_secs)`, `monid.balance()` and
`monid.available()` are the general calls. `arun` and `abalance` run them off
the event loop.

## Privacy

The search query goes to Monid, and from Monid to DataForSEO, which runs the
Google search. Nothing else is sent. eve's logs carry only the shape of a
run (ok, result count, time, cost). They never carry the query or the key.

## Tests

`bridge/tests/test_monid.py` uses a fake CLI. It needs no network and never
runs the real CLI. The suite-wide `conftest.py` sets `EVE_MONID=0`, so
no other test can spend the balance.


## After review (2026-10-08)

- **It also covers hangs.** With Monid on, the two OpenRouter engines race for the search budget less a 10 s reserve
  (`websearch.MONID_RESERVE_SECS`), so when both hang Monid still gets its 9 s or more (`monid.MIN_SEARCH_SECS`, the
  endpoint's p95 is 8.7 s). A run with less time left is not sent, so nothing is paid for a run that cannot finish.
- **Account refusals.** OpenRouter answering 401, 402 or 403 (credits gone, key revoked) goes straight to Monid: the
  other engine is the same account. Any other 4xx is still never retried.
- **The page reader never uses it.** The reader needs an answer to its own question or "not found"; Monid's raw
  Google snippets are neither (`engine_search(monid_ok=False)`).
- **The CLI gets no secrets.** The `monid` process gets only HOME, USER, LANG, TMPDIR, PATH and NO_COLOR; its own key
  is in its own config. eve's OpenRouter, Telegram and other keys never reach the npm package.
- **Costs without a unit are unpriced**, never read as dollars; odd settings (`inf`) fall back to the defaults.
