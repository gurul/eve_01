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
- **The literal arguments**: the site, the query and a result number. Code never
  guesses a kind.
  - **The site** comes from a table of about fifty common sites (search engines,
    developer and reference sites, news, video and music, social, shopping, travel,
    jobs), each with its search address where the URL can carry one. A name that is
    also an ordinary word (Target, Medium, Apple, Steam, Indeed, Twitch) counts only
    after a cue such as "open", "at" or "go to", or written as a domain, so "on medium
    heat" names no site.
  - **The query** is the rest of the clause after one of these phrase families:
    "search (for)", "look up", "search <site> (for)", "search the web / internet /
    online for", "look online for", "web search (for)", "do a (quick) search for",
    "google <words>", "browse / check <site> for", and "find / look for / shop for
    <words> on <site>". A bare "find" with no site is not a query. Politeness and
    filler come off both ends ("please", "for me", "real quick", "um", a leading
    "the" or "a"), and so does a second step ("and tell me...", "and log in", "so I
    can..."). "cats and dogs" stays whole.
  - **The result number** is an ordinal or number before a result noun ("the 3rd
    video", "result number two"), or one with no noun where nothing else could follow
    it: after a verb of picking ("open number two", "play the sixth", "go with the
    fourth") or as the whole request ("the seventh"). An ordinal with words after it
    ("the first moon landing", "the sixth president") is not a result number.
- **`none` without Laya**: a request with no site, no query and no result number is
  `none` from code, and Laya is not asked.
- **Which kinds Laya may pick**: only the kinds the arguments can carry. A spoken
  result number rules out a plain search, and a spoken query rules out just opening
  the site.
- **The check itself**: a `search` outcome becomes a `url_query` success check
  (`plan_contract.request_check`). Only code can make that check. A plan that supplies
  one is rejected. It compares the query's words, with filler trimmed, and accepts a
  search on the site already open, a Wikipedia article whose title is the query, and a
  site outside the table that the query names last
  ([routing](stackchan/routing.md#guards-ported-from-layabrowse-2026-10-16)).

## What Laya decides

Only the kind. The model is the browser-trained checkpoint `cklxx/laya-browser` (v10s,
revision `8a7e625`), run through `laya-mlx` on this Mac. It is asked LayaBrowse's
outcome question: one choice whose goal is the request, whose rules say "the outcome,
not the next action", and whose options are the fixed outcome descriptions plus
"unsupported".

**How the request is shown to it.** Disfluencies ("um", "uh") are dropped. In a
one-step request, a navigation verb before a site or a result ("go to", "pull up",
"take me to", "click on", "go with") is shown as "open", and a bare result reference
("the sixth") as "open the sixth". On the pinned checkpoint, "open YouTube" scores 0.99
for `open_site` and "go to you tube" 0.43, so this is the verb it reads. A request with
a second step ("pull up Netflix and log in") is shown as said: Laya reads "open X and
..." as plain `open_site`, and it would lose the extra step. The literal arguments are
always read from the request itself.

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
| `EVE_BROWSER_INTENT_IDLE_S` | `600` | Seconds with no question before Laya is unloaded (about 1.1 GB back); the next question loads it again in about 0.4 s. `0` keeps it loaded. |

Install: `pip install -e ".[laya]"`. The checkpoint is read from the Hugging Face
cache only (`local_files_only`). eve never downloads it.

## Measured on v1 (first look, now contaminated), 2026-10-16

This is the first set's first, honest run. The set was then used to fix the misses
below, so it no longer measures anything; the blind number is
[v2](#measured-on-a-blind-set-v2-2026-10-16).

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
| Memory | about 1.1 GB more resident after the first call (MLX: 614 MB active, 685 MB peak); since 2026-10-16 it is unloaded after 10 idle minutes |

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

**Closed in code since (2026-10-16, not re-measured on this set).** The site table, the
query phrase families, the bare result numbers and the leading "the" above are now
read by code, by phrase family rather than by example. Laya's nine `none` answers on
"go to" and "click on" phrasings are addressed by showing it "open" (above). This set
was seen while fixing them, so its numbers would no longer be blind; the next
measurement is a new blind set.

On the cases Laya answered, its kind was right 21 times out of 31 (about 68 %). The
labels' author saw the fixtures, so these are tuning-set numbers until someone runs a
blind set.

## Measured on a blind set (v2), 2026-10-16

A second holdout of 80 requests (`bridge/tests/fixtures/browser_intent/v2/holdout.jsonl`),
written and labelled by an author who never saw the code or the first set, was scored
**once**, after the fixes above, with the same tool and scoring:
`tools/browser_intent_eval.py --holdout … --label v2`. Nothing was tuned against it.
Full result: [browser-intent-eval-v2.json](browser-intent-eval-v2.json).

| Measure | v2 (blind) | v1 second look (contaminated) |
|---|---|---|
| Coverage (answered) | 95.0 % (4 abstentions) | 91.7 % (5) |
| Precision on answered | 77.6 % | 87.3 % |
| Accuracy, whole outcome | 73.8 % | 80.0 % |
| Wrong | 17 of 80, 21.3 % | 7 of 60, 11.7 % |
| Per request, p50 / p95 | 6.6 / 8.5 ms | 6.5 / 7.8 ms |

**It still does not clear eve's bar** (precision at least 80 %, wrong at most 5 %). The
v1 column is the old set re-run after it was used for fixing, so it is not a blind
number ([browser-intent-eval-v1-second-look.json](browser-intent-eval-v1-second-look.json)).

| Kind (v2) | n | Fully correct | Wrong |
|---|---|---|---|
| search | 35 | 18 | 13 |
| open_site | 15 | 11 | 4 |
| open_result | 12 | 12 | 0 |
| none | 18 | 18 | 0 |

Where the 17 wrong answers come from:

- **Laya answered `none` for a site search (3)**, with the site name first or the
  query after it: "hacker news search …", "on ebay look for …", "etsy search …".
- **Code read the wrong site or query while Laya's kind was right (9).** BBC resolves to
  `bbc.co.uk` where the label says `bbc.com` (2, one also keeping "news for" in the
  query); Kayak and The Guardian are not in the table, so the site name stayed in the
  query (2); YouTube Music resolved to `youtube.com` (1); "google X" names `google.com`
  as the site where the label says general web (3; `url_query` treats Google as a
  search engine, so this may be a label convention rather than a failure; even with those 3
  excused, 14 of 80 wrong is 17.5 %, still over the bar); a trailing
  "online" stayed in the query (1).
- **Code answered `none` before Laya was asked (5).** A bare "find …" with no site is
  not read as a query (2, by design); "…, search it up" after the query (1); "bring up"
  is not an open verb (1); The Verge is not in the table (1).

Open result numbers and `none` were perfect. Abstentions (4) are safe, not wrong.

## What leaves the Mac

Nothing. The model runs locally on MLX, the checkpoint comes from the local cache, and
no hosted model (Jev or any other) is asked.

## Tests

- `bridge/tests/test_site_names.py`: site-name joining, including the cue-word cases.
- `bridge/tests/test_browser_intent.py`: the delegate with a stubbed engine, the gate,
  the abstentions when `laya-mlx` is missing, and one real-model test when the
  checkpoint is cached.
- `bridge/tests/test_browser_intent_gaps.py`: the broader site table, the query phrase
  families and filler trimming, result numbers with no noun, how the request is shown
  to Laya, the idle unload and lazy reload, and a real-model test on phrasings outside
  the first eval.
- `bridge/tests/test_plan_success_guard.py`, `test_plan_scope.py` and
  `test_url_query_tolerance.py`: the `url_query` check, what it tolerates, and which
  plans the strict success rule covers ([routing](stackchan/routing.md#the-browser-lane-playwright-as-the-hands-jev-as-the-judge)).
