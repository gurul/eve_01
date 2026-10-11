# Toolset router

`bridge/src/eve_bridge/toolset_router.py` decides which of the owner's toolsets a Telegram turn starts with.
It combines two sources. Keywords always apply. Jev's judgment of the message adds toolsets on top. The eval is
`bridge/tools/toolset_eval.py`.

## Why

A turn starts with only some toolsets, plus `open_app` for the rest. When a turn needs a toolset it did not start
with, two things go wrong:

- **It costs a model round.** The brain calls `open_app` first, and only gets the toolset's tools on the next round.
- **It busts the prompt cache.** The tool list changes in the middle of the turn. Live on 2026-10-05, a 13k-token
  turn whose tool list changed read `cached=0` on the next round.

Keywords alone (`apps_mcp.APP_WORDS`) miss most of what the owner actually types: "whens my thing w the advisor",
"did anyone @ me". Jev reads the job instead, and answers in 148-266 ms (measured live on
2026-10-05, for example "my next class and a maps link" gave calendar 0.93 and maps 0.90).

## The toolsets

| Toolset | What it covers | Where its tools come from |
|---|---|---|
| calendar | events, classes, meetings, free time, invites | direct app (`apps_mcp.py`) |
| maps | places, directions, travel time, traffic, weather | direct app |
| gmail | reading the owner's own mail | direct app |
| drive | docs, sheets, slides and PDFs in Drive | direct app |
| github | repos, issues, PRs, commits, CI | direct app |
| share | sending a file from the Mac to another person | `share.py` |
| workspace | the owner's Slack, Notion and Linear | Composio (`composio_tools.py`) |

The full labelling policy is the `policy` field of `bridge/tests/fixtures/routes/toolset_tuning.json`.

### Always-carried toolsets

Two more toolsets were added on 2026-10-08. Every turn carries them, and the router never routes them
(`toolset_router.ALWAYS`):

| Toolset | What it covers | Tools | Lent when |
|---|---|---|---|
| menu | the `/` menu's commands that had no tool, and reading a text file on the Mac (v4) | `spend_report`, `daily_rundown`, `claude_relay`, `codex_relay`, `set_stealth`, `open_apps`, `pair_eve_link`, `read_file` (`menu_tools.py`) | `EVE_MENU_TOOLS` is on |
| events | concerts, games and shows by place and date on Ticketmaster | `events_search` (`events.py`) | `EVE_MENU_TOOLS` is on and `TICKETMASTER_API_KEY` is set |

`EVE_MENU_TOOLS` is `on` or `off`; unset follows `menu_tools.SHIPPED`, which is `True` since the v4 blind
tool-choice eval passed (2026-10-09: 95.0% against live's 72.6%, bar 88%; below). Off, no request carries these tools or the tool-choice lines below, byte for
byte as before (`test_menu_off_bytes.py` runs a session with the chief, OpenTable and a Ticketmaster key lent and
compares every request body against origin/main's `telegram.py`, and `jobs_list`'s and OpenTable's wording against
origin/main's `chief.py` and `opentable.py`).

They are not routed for three reasons. Their tools are few and small. Their schemas never change from turn to
turn, so the cached prefix stays the same. A routed toolset needs its own Noul in Jev's call. That changes the
call the gates above were fitted on, so it needs a refit with `tools/toolset_eval.py`. Jev's questions and the
fitted gates are unchanged.

These tools ride at the start of the watch slot (`telegram._menu_tools`). `events_search` adds one line to the
instructions. That line names `watch_add` only while the watcher offers the `ticketmaster` kind. Because every
turn carries `events_search`, an event question can reach Ticketmaster on any turn, and `web_search` handles the
rest of the web.

With the switch on, a short block of tool-choice lines rides the same slot (`telegram.intent_block`). Each line says
when a tool is the one, as a positive set in the owner's words, and is written only when every tool it names is
offered on that turn. No line names a wrong tool, even as a warning.

### Scoring tool choice

`bridge/tools/intent_eval.py` scores which tool the brain picks for plain words. It takes a cases JSON of
`{id, text, primary, expect[]}` and runs each case as an app turn through `TelegramInlet._think`, with the
**production toolset router**: Jev over OpenRouter, as the owner's Mac runs it. Before any model call it runs every
path `TelegramInlet._handle` answers by code, in its order (the code words, the chief's words, then the Fire TV,
camera, lights, Spotify and quick-answer matchers), and scores such a message as the tool its code path runs.
`--require LABEL=PCT` (repeatable) sets a bar for the run: each label's cases score at least PCT%, and no hard
negative acts, no `never` label fires and no provider errors; it prints `INTENT_BAR_OK` and exits 0 only when all
hold. A case may list `never` labels that must not fire at any step (reported as `NEVER_FIRED`). A `hard_negative`
case that accepts a label only acting tools reach is refused as a `CONFLICT`: it would fail by construction. It stops at
the first tool call, so no tool runs; `open_app` is let through (it only opens toolsets), so the call it leads to is
the one scored, and an `open_app` followed by a reply is labelled by the app the owner's words name (Slack, Notion
or Linear for the workspace). It maps the tool to one label (`intent_eval.LABELS`; a reply that asks a question is `ask`, a plain
reply is `none`). It prints PASS/FAIL per case, totals per primary label, the overall percentage, the hard
negatives (primary `none` or `ask`) that fired an acting tool, Jev's errors and timeouts, and `PROVIDER_ERRORS`.

The environment it needs, and sets when the shell has not: `EVE_TELEGRAM_MODEL=gpt-6-luna`,
`EVE_JEV_ROUTE=openrouter`, `EVE_OPENTABLE=1`, `EVE_MENU_TOOLS=on` (pass `--menu-tools off` to score
the switch off). `EVE_TOOLSET_ROUTER`, `EVE_CHIEF` and `EVE_GRADE_ROUTER` stay at their defaults,
as in production. Keys: only `OPENROUTER_API_KEY` (every model call and Jev) and `TICKETMASTER_API_KEY` (for
`events_search` to be lent), exported by the command:

```bash
export $(grep -E '^(OPENROUTER_API_KEY|TICKETMASTER_API_KEY)=' ~/.config/eve-bridge/env | xargs)
cd bridge && uv run python tools/intent_eval.py CASES.json
```

It refuses to run on keywords alone unless `--allow-keywords` is given. It lends every service with eve's own
schemas and nothing that can act: memory, the chat log, the second brain, the watcher, Meet, lights, Spotify, Fire
TV, america, Share, OpenTable, the chief, the app maker, the direct apps (Gmail, Calendar and Drive with their real
tools; GitHub and Maps with a few canned tools, because their servers are remote) and the Composio workspace (two
canned meta tools). It imports only what the checkout has, so it scores an older checkout too when it is copied
there alone. The dev set is `bridge/tests/fixtures/intent_dev.json` (15 cases, written by the builder): 15/15 on this
branch, 9/15 on live (2026-10-08). It checks the harness and is not evidence of how well the brain does.

**v3 (2026-10-08), dev numbers only.** The v2 blind set (269 cases) was spent by its verdict (v2: 208/269, 77.3%,
under the old harness) and is now dev data. With the harness fixed (code paths first, `open_app` scored by the call
it leads to) and the v3 tool-choice lines, two runs on it scored 233/269 (86.6%) and 230/269 (85.5%), zero provider
errors, and hard negatives that acted 0 and 1 (`c269` "keep going, dont stop" reached `steer_task`). The run-to-run
spread is about three cases. Fixed by category: the screen and files (screenshot 5/5, room notes 5/5), forgetting
(4-5/5), stopping a task (5/5), the TV (10/10), Codex (5/5), spend (5/5), pairing (5/5). Still weak: contacts (0/5,
no contacts tool), sending email (1/5), GitHub requests that the brain sends to the workspace (3/5), and the
calendar's writes (it reads the event first). These are dev numbers on a set the lines were written against; only a
fresh blind set decides the switch.

**The v4 scorer (pre-registered 2026-10-09, before any v4 result).** The scoring rule changed from v3 and is fixed
in `intent_eval.score`:

- The brain's actions in the turn are mapped to labels, in order (`intent_eval.Step`). A case passes when (a) its
  first action maps to an acceptable label, or (b) its first action is a read-only lookup from `PREP` and its
  second action, made in a later model round after the lookup's result came back, maps to one. `PREP` is exactly
  `list_files`, the calendar's list and search events, Gmail's search and read threads, `memory_search`,
  `watch_list`, `jobs_list` and `search_notes`. A call written beside the lookup in its own round is not a second
  action, and nothing gets a third.
- The lookup gets a short neutral stub (`intent_eval.prep_stub`), and the turn runs one more model round. Nothing of
  the owner's is read: a file listing holds one file named from the request's own words, a calendar or mail lookup
  one item titled from them, and memory, the watches, the jobs and the notes come back empty. Any other call is
  recorded and stops the turn, so no acting tool ever runs.
- A reply with no tool is `none` on a case whose primary is `none`, whether or not it ends in a question. On every
  other case it is `ask` when it ends in a question, else `none`.
- A code path that answers before the model counts as the tool it runs; one that changes something (the TV, the
  lights, Spotify, a session, a booking, Meet, a screenshot: `intent_eval.CODE_ACTING`) counts as acting.
- A call to a tool the turn did not offer (production refuses it, and the next round may repair it) is an action
  labelled `unknown_tool`, which no case accepts, and stops the turn.
- `HARD_NEGATIVE_ACTED` lists every case whose group is `hard_negative`, or whose primary is `none` or `ask`, that
  fired an acting tool at any step. Acting (`intent_eval.acting`) is tool_gate's rule plus the owner's app writes:
  a direct app's tool in the run's writes set (its server does not mark it read only) or named as a write
  (`calendar__quick_add_event`, `calendar__respond_to_event`, `drive__create_file`, GitHub's create, merge, push,
  fork and comment tools), and a Composio call by the slugs it executes (`composio_tools.consequential_slugs`).
  This was widened after round 4 scored (2026-10-09 review): it only makes the check stricter, and round 4 was not
  re-scored with it.

The bar NEW must clear, scored once per arm on the blind set (BASE is live with this harness copied in; NEW is the
branch with `EVE_MENU_TOOLS` on): overall at least 88%; every primary with at least 6 cases at or above BASE
less one case; `HARD_NEGATIVE_ACTED` 0; events at least 80%, `restaurant_booking` at least 90%, `tv_control` at least
80%; `PROVIDER_ERRORS` 0. `bridge/tests/test_intent_eval.py` unit-tests the scorer, with positive controls that must
fail: a lookup then the wrong tool, a lookup then nothing on a case that wants a tool, a call beside the lookup in its
own round, a call to a tool not offered, and an acting call at step two on a hard negative (counted).

**v4 dev numbers (2026-10-09), dev data only.** Both earlier blind sets are spent and the v4 lines were written by
category from their failures, so these say little about the blind set; only a fresh one decides the switch. With the
v4 scorer, one run per arm: the v3 set (324) scored 235 (72.5%) on live and 301 (92.9%) on the branch; the v2 set
(269) scored 188 (69.9%) on live and 246 (91.4%) on the branch. Zero provider errors; hard negatives that acted: 0 on
the branch, 2 on live (the lights and Spotify). Still weak: sending email (Gmail writes are refused by policy) and
contacts (`find_contact` rides only when Share opens), a few GitHub requests that still go to the workspace,
calendar moves (the brain reads the event and then asks), and a price to watch for a product named only loosely.

**v4 blind result (2026-10-09): PASS, switch on.** A fresh set written without the code or the earlier sets (318
cases, sha256 `506c5aed…`), each arm scored once: live 231/318 (72.6%), the branch 302/318 (95.0%). Every bar item
held: overall 95.0% against 88%; no primary with 6 or more cases fell by more than one (room notes and Slack each lost
one); zero acting tools fired on hard negatives, none or ask cases; events 12/12, restaurant_booking 12/12,
tv_control 12/12; zero provider errors. The reviews after it closed the open safety findings (meet titles and
Spotify catalog text taint, the voice's yes bound to one question and to code's question as said, hidden characters
shown in gate questions, the rundown and mem0 keep outside words marked) and made the harness's acting check
stricter (direct and Composio writes by slug); round 4 was not re-scored with it. Some short example phrases in the
tool-choice lines come from the v2 and v3 sets, so those sets are dev data only.

## How it decides

1. **One Jev call per turn.** The state has three fields: `about` (one line saying this is a message the owner
   texted their assistant, where the previous message is context for a follow-up), `previous` (the owner's
   previous message, or `""`) and `request` (the message). The questions are one absolute Noul per toolset, each
   written as a plain judgment of the job ("To do what the owner asks, must the assistant read or change the
   owner's own calendar ..."). The instructions use the owner's own words for things and name no wrong thing.
2. **Gates.** A toolset opens when its Noul is at or above its fitted gate (`GATES`).
3. **Keywords.** `apps_mcp.APP_WORDS` covers the direct apps. The module adds its own patterns for share
   and workspace. For a short follow-up that points back ("and send it to ana"), the previous message's
   keywords are added too.
   Google Contacts (2026-10-09) is opened by `APP_WORDS` alone (`KEYWORD_ONLY`): it is one small read tool, so a
   false open costs a few hundred tokens, and a Noul of its own would change the call the gates were fitted on.
   Sending email (2026-10-09) also rides the keywords: Gmail's Noul was written when Gmail was read only and still
   answers no for a request that only sends, so "email Sam that …" opens Gmail (and Contacts) by its words. A
   send asked without a mail word reaches Gmail through `open_app`, one round later. A refit on a fresh blind set
   is the way to move that into Jev.
4. **Union.** The turn starts with keywords ∪ Jev.
5. **Failure.** A Jev error, a timeout (`TIMEOUT_S` = 0.8 s), a missing key or the mode switched off gives an
   empty set from Jev. The keywords and `open_app` still apply, exactly as before the router.

## Method

This is the house recipe from `tools/route_eval.py --search`:

- **Fit on the tuning set only.** `toolset_tuning.json` has 220 cases, from writer A. Live Jev runs once over it,
  and its answers are cached in `toolset_tuning_answers.json` so a refit makes no calls. For each toolset, the
  fit keeps the **highest** gate on the grid 0.05..0.95 at which keywords + Jev open that toolset for at least
  98% of the tuning cases that need it. The output is `toolset_gates.json`, and the gates are pasted into `GATES`.
- **Score once on the blind holdout.** `toolset_holdout.json` has 120 cases, written blind by writer B. Live Jev
  runs once over it, and the answers are kept in `toolset_holdout_answers.json`. The holdout is scored against
  the bar below, and nothing is refitted after it.

### Pre-registered bar

The orchestrator wrote this bar before any holdout result existed. Verbatim:

> (1) case-level full recall (every needed toolset opened) of keywords∪Jev >= 0.95;
> (2) strictly higher than keywords alone on the same metric;
> (3) mean unneeded toolsets opened per case <= 0.6;
> (4) Jev latency p95 <= 800 ms.

`SHIPPED` is True only if all four hold.

## Result (2026-10-05)

Fitted gates:

| calendar | maps | gmail | drive | github | share | workspace |
|---|---|---|---|---|---|---|
| 0.55 | 0.55 | 0.2 | 0.6 | 0.8 | 0.95 | 0.8 |

On the tuning set, which is seen data and not evidence, keywords alone fully covered 163 of 220 cases, and
keywords + Jev covered 219.

Holdout scorecard (n = 120, scored once):

| Metric | Keywords alone | Keywords ∪ Jev | Bar | Verdict |
|---|---|---|---|---|
| (1) Full recall | 92/120 = 76.7% | 118/120 = 98.3% | ≥ 95% | pass |
| (2) Better than keywords alone | | 98.3% > 76.7% | strictly higher | pass |
| (3) Mean unneeded toolsets per case | 0.217 | 0.333 | ≤ 0.6 | pass |
| (4) Jev latency p95 | | 286 ms (p50 172 ms, 0 errors) | ≤ 800 ms | pass |

**`SHIPPED = True`.**

On the holdout, the router cut missed toolsets from 30 to 2. Each missed toolset costs one `open_app` round, so
that is 28 rounds saved, plus the prompt-cache reads those rounds would have lost.

The two misses:

- "how long a drive to the restaurant for dinner w sam tonight": needed calendar and maps. Calendar's Noul was
  0.51, under its 0.55 gate.
- "send my mom the boarding pass on my desktop and check traffic to sea-tac": needed maps and share. Share's Noul
  was 0.94, under its 0.95 gate, and the share keywords have no "boarding pass". Gmail (0.35) opened although
  this case did not need it.

The weakest number is gmail precision: 48% on the holdout, because of its low gate. Each unneeded gmail open
costs tokens on that turn, but it costs no extra round.

### DoorDash removed, re-scored (2026-10-05)

The owner removed the DoorDash integration on 2026-10-05 ("not necessary"). There were eight toolsets; there are
now seven. What changed for the router:

- **Its Noul is no longer asked.** `TOOLSETS`, `INSTRUCTIONS`, `GATES` and `toolset_gates.json` no longer carry it,
  and its keyword pattern is gone.
- **The remaining gates are unchanged.** Nothing was refitted.
- **The datasets are history and were not edited.** `toolset_tuning.json`, `toolset_holdout.json` and both
  `*_answers.json` caches still carry its labels and cached Nouls. `tools/toolset_eval.py` drops, when it reads
  them, every label and cached Noul for a toolset not in `TOOLSETS`. A case that needed only DoorDash now needs
  nothing, so any toolset opened on it counts as unneeded.
- **Re-scored from the cached answers, no Jev calls.** Latency is the cached latency.

| Metric (holdout, n = 120) | Keywords alone | Keywords ∪ Jev | Bar | Verdict |
|---|---|---|---|---|
| (1) Full recall | 98/120 = 81.7% | 118/120 = 98.3% | ≥ 95% | pass |
| (2) Better than keywords alone | | 98.3% > 81.7% | strictly higher | pass |
| (3) Mean unneeded toolsets per case | 0.200 | 0.317 | ≤ 0.6 | pass |
| (4) Jev latency p95 | | 286 ms (p50 172 ms, 0 errors) | ≤ 800 ms | pass |

Missed toolsets went from 24 (keywords alone) to 2 (keywords ∪ Jev): the same two misses as above. On the tuning
set (seen data), keywords + Jev covered 219 of 220 cases, with 0.318 unneeded toolsets per case. For comparison,
the same code scoring all eight toolsets gives 92/120 = 76.7% and 0.208 for keywords alone, and 118/120 = 98.3% and
0.325 for keywords ∪ Jev. The unneeded counts in the scorecard above (0.217 and 0.333) are the original run.

`SHIPPED` stays True. The cached answers came from a call that asked one more Noul than a turn now asks. A fresh
blind set should confirm these numbers on the next refit.

## Wiring into the Telegram turn

`telegram.TelegramInlet` builds the router once, in its constructor (`toolset_router.make_router()`; `None` means
keywords only). Each turn then works like this:

1. **Turn start.** For every turn except the rundown, `_think` calls `toolset_router.route_turn(message, previous,
   router)`. `previous` is the owner's message before this one; the turn's own line is already in the history, so
   it is skipped. With a Jev router, this runs on a worker thread **at the same time** as the prompt is built
   (`asyncio.gather` with `_turn_parts`), so Jev's 150-300 ms is hidden behind the memory reads. With no router,
   the keywords run inline: they take microseconds.
2. **The sets.** The direct toolsets that are up go into `_direct_open`. Share and the workspace go into a new
   per-turn set, `_lent`, when that module is on (Share lent to the inlet, Composio's session up). The app tools, the allowed names and the instructions are then rebuilt, so round 1 already has the tools.
3. **Gating.** Share's and Composio's tools, and their instruction blocks (Share's, Composio's accounts note,
   and `APPS_BLOCK`), are offered only while their toolset is open. The blocks go into
   `request()`'s `lent_block`, after every block that does not change.
4. **`open_app`.** Its enum lists every closed toolset that is available: the direct apps that are up, and
   Share and the workspace when they are on. Opening one adds it to `_direct_open` or `_lent`. A name
   that is not a toolset here is not repeated back: the result lists the toolsets that are.
5. **Auto-open.** A call to the tool of a closed toolset that is on (for example `find_contact` when Share was
   not opened) opens the toolset and runs the call, as `AppsBridge.ensure_open` does for a direct app. Composio
   tools are claimed by `AllowedNames` while the workspace is closed, so the call is never refused as unknown.
6. **Refresh.** After each round, the loop compares `(_direct_open, _lent)` with the sets before the round. When
   they changed, the tools and the lent block are rebuilt for the next round.
7. **Order.** The tool list is eve's own tools, the memory tools, web search, the vault, the watch slot, then the
   app tools. In the app tools, the opened direct apps come first, then the workspace and Share, then
   `open_app` last. What changes from turn to turn sits at the end, so the cached prefix is as long as it can be.
8. **The call warm-up** (`_warm`, when a phone call connects) opens no toolset: the previous turn's sets are stale.
9. **The log.** One line per turn, names and milliseconds only, never the message:
   `telegram: toolsets: calendar (jev), maps (both); jev 212 ms`. Each toolset is marked `keywords`, `jev` or
   `both`. With no router, the line ends `jev off`; a Jev that missed its budget ends `(timeout)`.

With `EVE_TOOLSET_ROUTER=off`, none of this runs: no router pass, no gating, and every request body is byte
for byte what it was before the router (`test_telegram_chief.py` compares it with `origin/main`). The rundown
keeps its fixed sources in every mode. The tests are in `bridge/tests/test_telegram_toolsets.py`.

## Settings

| Variable | Values | Effect |
|---|---|---|
| `EVE_TOOLSET_ROUTER` | `auto` (default) | Jev is on when `SHIPPED` is True, which it is. |
| | `on` | Jev is on even when `SHIPPED` is False, for a live trial. |
| | `off` | Keywords only. |
| `EVE_JEV_ROUTE`, `OPENROUTER_API_KEY` / `TYPESAFE_API_KEY` | | The Jev route (`jev.route_config`). With no key, the router is keywords only. |

Privacy: while the router is on, the owner's message and the previous message go to Jev on every Telegram turn.

## Commands

From `bridge/`:

```bash
.venv/bin/python tools/toolset_eval.py --fit       # reuses toolset_tuning_answers.json when it covers the set
.venv/bin/python tools/toolset_eval.py --holdout   # re-scores the kept answers; it never asks Jev again
.venv/bin/python -m pytest -q tests/test_toolset_router.py
```

Before any refit, write a new blind holdout. The current holdout is seen data now.
