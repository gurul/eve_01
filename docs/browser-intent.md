# What a browser request asks for (the Laya delegate)

Owner, 2026-10-16, after the eval below: "i think laya i good keep it". So the delegate
ships **on** (`EVE_BROWSER_INTENT=laya`, the default), even though it does not clear
eve's usual bar. Read the numbers before you rely on it.

`bridge/src/eve_bridge/browser_intent.py` answers one question about a web request:
what outcome would show it is done? The set is exactly four kinds:

| Kind | Example | What code then checks |
|---|---|---|
| `search` | "search git hub for laya mlx" | the page URL is that site's search for that query (`url_query`) |
| `open_site` | "open stack overflow" | nothing new yet; the planner's own check stands |
| `open_result` | "search youtube for lofi and play the first video" | nothing new yet; the planner's own check stands |
| `none` | "what's the weather" | nothing; the planner's own check stands |

The split follows LayaBrowse (github.com/cklxx/LayaBrowse, `goal_engine.py`, MIT).

## What code decides

- **Site names that speech recognition splits** are joined before any routing
  (`join_site_names`, called from `task_router.classify`). "git hub" becomes GitHub,
  "you tube" becomes YouTube, "duck duck go" becomes DuckDuckGo. "linked in", "stack
  overflow" and "hacker news" are also ordinary English, so they are joined only after
  a cue word such as "open", "on" or "to". "The doc I linked in slack" stays as it is.
- **The literal arguments**: the site (from a table of 12 sites), the query (the
  words after "search for" or "search <site> for") and a result number ("the first
  result", "the 3rd video"). Code never guesses a kind.
- **`none` without Laya**: a request with no site, no query and no result number is
  `none` from code, and Laya is not asked.
- **Which kinds Laya may pick**: only the kinds the arguments can carry. A spoken
  result number rules out a plain search, and a spoken query rules out just opening
  the site.
- **The check itself**: a `search` outcome becomes a `url_query` success check
  (`plan_contract.request_check`). Only code can make that check. A plan that supplies
  one is rejected.

## What Laya decides

Only the kind. The model is the browser-trained checkpoint `cklxx/laya-browser` (v10s,
revision `8a7e625`), run through `laya-mlx` on this Mac. It is asked LayaBrowse's
outcome question: one choice whose goal is the request, whose rules say "the outcome,
not the next action", and whose options are the fixed outcome descriptions plus
"unsupported".

## The gate

An answer is used only when all of these hold:

1. It passes the schema check.
2. Its top probability is at least 0.50, and its margin over the runner-up is at
   least 0.10 (the same gate as LayaBrowse).
3. The arguments can carry the kind: a search needs a query, `open_site` needs a site,
   `open_result` needs a number.

Anything else is an abstention, and the planner's own success check stands. These are
abstentions too: the switch is off, `laya-mlx` is not installed, the checkpoint is not
in the local cache or fails to load, or the request has a step outside the four kinds
("search for cats and email them to my boss").

**What a wrong answer costs.** The outcome only sets the success check; it never picks
a click. A wrong `search` check makes a plan that did the right thing end `partial`,
so the step-by-step loop takes over and the task takes longer. A wrong `none` leaves
the planner's check in place, as before this change.

## Switch

| Variable | Default | What it does |
|---|---|---|
| `EVE_BROWSER_INTENT` | `laya` | `laya` asks the delegate. `off` never loads the model, and every request abstains. Site-name joining runs either way. |

Install: `pip install -e ".[laya]"`. The checkpoint is read from the Hugging Face
cache only (`local_files_only`). eve never downloads it.

## Measured, 2026-10-16

`bridge/tools/browser_intent_eval.py` runs 60 held-out requests
(`bridge/tests/fixtures/browser_intent/holdout.jsonl`) through `expected_outcome`, the
same way routing calls it. The full result, with every miss, is
[browser-intent-eval.json](browser-intent-eval.json).

| Measure | Value |
|---|---|
| Coverage (answered) | 93.3 % (4 abstentions) |
| Precision on answered | 60.7 % (eve's bar: at least 80 %) |
| Accuracy, whole outcome | 56.7 % |
| Wrong | 22 of 60, 36.7 % (eve's bar: at most 5 %) |
| Wrong, by source | 12 from Laya, 10 from code |
| Per request, p50 / p95 | 6.4 / 7.9 ms |
| Per Laya call, p50 / p95 | 6.7 / 7.9 ms (35 calls) |
| First call (loads the model) | about 0.4 s |
| Memory | about 1.1 GB more resident after the first call (MLX: 614 MB active, 685 MB peak) |

| Kind | n | Fully correct | Wrong |
|---|---|---|---|
| search | 25 | 8 | 14 |
| open_site | 10 | 6 | 4 |
| open_result | 10 | 5 | 4 |
| none | 15 | 15 | 0 |

Where the wrong answers come from:

- **Laya picked the wrong kind (10).** Nine came back `none` with confidence 0.57 to
  0.86, such as "go to you tube" and "click on the fourth link".
- **Code read the wrong details (2).** One query kept a leading "the", and one site
  (IMDb) is not in the table.
- **Code answered `none` before Laya was asked (10).** Five sites are not in the table
  (npm, Spotify, IKEA, Bing, BBC). "Search the web for", "look online for" and "web
  search" are not read as queries. "Number two" and "the sixth" are not read as result
  numbers.

On the cases Laya answered, its kind was right 21 times out of 31 (about 68 %). The
labels' author saw the fixtures, so these are tuning-set numbers until someone runs a
blind set.

## What leaves the Mac

Nothing. The model runs locally on MLX, the checkpoint comes from the local cache, and
no hosted model (Jev or any other) is asked.

## Tests

- `bridge/tests/test_site_names.py`: site-name joining, including the cue-word cases.
- `bridge/tests/test_browser_intent.py`: the delegate with a stubbed engine, the gate,
  the abstentions when `laya-mlx` is missing, and one real-model test when the
  checkpoint is cached.
- `bridge/tests/test_plan_success_guard.py`: the `url_query` check and the plan
  success rules ([routing](stackchan/routing.md#the-browser-lane-playwright-as-the-hands-jev-as-the-judge)).
