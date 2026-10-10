# Routing: who carries a request

**Current daemon route (2026-09-22):** voice and Telegram `start_task` use
`codex_computer.py` and Codex's installed Computer Use plugin. This page documents
the retained legacy worker and its experiments; the daemon does not select it or
fall back to it. See [the Codex integration](../codex-computer-use/README.md).

Codex is the only desktop executor. Holo, an opt-in alternative to Codex
(`EVE_COMPUTER=holo`), was removed on 2026-09-30 at the owner's request; a
leftover setting is ignored with one warning at start, and the code is in git history.

eve has three engines that can decide something, and a fourth tier that is no model at all.
They are not interchangeable, and the first version of this work went wrong by treating two of
them as if they were. This page says what each one is, how it has to be asked, what was
measured, and how a request finds its way to the cheapest one that can do it safely.

- [The engines, as individuals](#the-engines-as-individuals)
- [Ask each one in its own idiom](#ask-each-one-in-its-own-idiom)
- [The tiers a request passes through](#the-tiers-a-request-passes-through)
- [What was measured](#what-was-measured)
- [Head moves](#head-moves)
- [Plan once, execute with Jev](#plan-once-execute-with-jev)
- [Knobs](#knobs)
- [How the evidence was kept honest](#how-the-evidence-was-kept-honest)
- [Borrowed, and from where](#borrowed-and-from-where)

## The engines, as individuals

| | **astra** (`gpt-6-astra`) | **jev** (TypeSafe System One, `jev.py`) | **laya** (`laya-mlx`, local) | **code** |
|---|---|---|---|---|
| What it is | a vision and reasoning model that writes and runs Python on the desktop | a hosted typed-decision model: answers `choice`, `score` and `noul` questions about a text state with probabilities | a 322 M-parameter mmBERT **encoder** with decision heads; nothing is generated | rules in `task_router.py`, `lane_router.py`, `fast_lane.py` |
| Time per decision | 3.4 s median a turn here (2.8 s before the first token) | 226–244 ms p50, 292–341 ms p95 measured here; 368 / 833 ms inside matrixorigin/Astra | 8–12 ms | microseconds |
| Sees the screen | yes | no: "Text only" | no | the Accessibility tree |
| Writes text or code | yes | no ("not trained to generate text") | no | no |
| Options per question | n/a | up to 255 | degrades past about 20; "77 options receive only ~3 to 4 tokens per label" | n/a |
| State it can read | 1.05 M tokens | 64 k a request, 32 k of state; suffers "context rot" in a large irrelevant one | 1024 tokens here, a 256-token head for the whole question | n/a |
| Confidence you can trust | n/a | calibrated; thresholds are per question type | "over-confident as shipped"; this checkpoint "ships with no fitted temperatures at all" | n/a |
| Leaves the Mac | screenshots and the prompt | the state you send | nothing | nothing |
| Cost | $10 in / $50 out per M tokens | $0.042 per M input tokens, output free | $0 | $0 |
| Stated weak spots | asks clarifying questions; no latency promise | "quite literal"; counting, maths, dates, indirection; "DONE is never independent evidence of success" | "near chance on typed-decisions zero-shot … a fast base to specialise, not a zero-shot decision engine"; ordinal `score` is its weakest primitive | knows only the wording it was written for |

Sources: OpenAI's [computer-use guide](https://developers.openai.com/api/docs/guides/tools-computer-use)
and [model page](https://developers.openai.com/api/docs/models/gpt-6-astra); TypeSafe's
[docs](https://docs.typesafe.ai/llms.txt), [models page](https://docs.typesafe.ai/models) and
[privacy policy](https://typesafe.ai/legal/privacy-policy) (no numeric retention period is stated for
non-enterprise accounts); [browser-use/jev-ultrafast](https://github.com/browser-use/jev-ultrafast);
[laya-mlx](https://github.com/mizorewww/laya-mlx) and upstream [laya](https://github.com/NandhaKishorM/laya)
"Honest limits". Pulled 2026-09-21; the latencies in the first column of each pair are this project's own.

## Ask each one in its own idiom

laya and Jev share a wire format — `predict(state, questions)` — and that is all they share.
`typed_ask.py` holds one asker per model, and they have nothing in common but the return type.

**laya ranks; it does not judge.** A `choice` is a ranking of the options against each other,
and with short options it is crisp: asked "left / right / up / down / center" it answers at
p = 1.00 in 9 ms. Its absolute yes/no (`noul`) is its weak primitive — it says about 0.1 for a plain
yes. So for laya: few options, a word or two each, the utterance as the whole state, a gate asked as a
choice rather than a yes/no, several small questions in one call, and **no threshold borrowed from
anywhere** — each cut-off is fitted on a calibration set. What laya cannot do zero-shot is tell
"move your head" from "scroll the screen", and its own authors say the honest way to use it is to
fine-tune it.

**Jev judges; its choice alone is not a gate.** TypeSafe: "A Choice is relative … each Noul is
absolute and can be low for all of them." Its `noul` is excellent (0.94 for a head command, 0.04 for
"scroll down a bit"), and its `choice` happily answers "down" for the scroll. So every Jev decision
here is an **absolute noul gate plus the relative choices, in one request** — "adding questions
barely changes the response time". It reads literally, so each instruction says in plain words what
counts *and what does not*. The state is small and structured. An app is chosen from the list that
is really installed, with an explicit "none". It never produces text: a search query is still
extracted by code.

What asking properly was worth, same model, same utterances:

| | asked one relative question | asked in its own idiom |
|---|---|---|
| Jev, head pose | 86.4 %, 1 false move in 44 | **97.0 %**, 0 wrong poses, 0 false moves (test half, unseen by its cut-offs) |
| Jev, request class | 80.7 %, **8 unsafe** reflexes in 100 | **97.6 %**, **0 unsafe** in 110 unseen |
| laya, head pose | 16.7 % | 12.1 % — the framing was not its problem; zero-shot gating is |

## The tiers a request passes through

`task_router.classify()` returns a plan: the tiers to try, in order. Each declines unless it is sure,
and the last is always the planner unless a reflex fully answers the request.

```
code gates      consequential (send, order, delete, quit …)   → planner only: only it has ask_user
                text to type or content to create             → planner only
                a question, or "tell me …" without a search    → planner only (needs eyes)
                a launch of Mail, Messages, Notes, Terminal …  → planner only if anything follows the launch
reflex (code)   "Open Spotify"            → open_app('Spotify')          1.6 s live   (8–32 s with the planner)
                "Search Google for …"     → open_url(search?q=…)         2.6 s live   (9–25 s)
                "Open Safari and go to …" → the launch, then the planner starts from there with a [note]
jev (optional)  wording the rules do not know ("Can you get Safari up on the screen?") → a bare launch only,
                asked natively, behind the code gates                     2.5 s live
lane (code)     lane_router.py: one labelled control accounts for every word → clicked    1.2–1.5 s live
astra           everything else; it may hand the lane exact labels: delegate(steps=[…])
```

An app is matched by its installed name, a small alias table ("settings" → System Settings), or the same
letters without spaces ("photobooth" → Photo Booth, only when one app has them). Finder counts as installed
though it lives in CoreServices, so "open finder" is a launch too.

A web search is harmless whatever its query says, so "google how to reset a Casio watch" is a reflex;
"search for earbuds and buy the cheapest pair" is the planner's, because the risky word starts a
second clause. "Search Spotify for jazz", "search for the invoice in Mail" and "look up Dana in my
contacts" are searches inside an app or your own data, not the web.

A reflex is confirmed by the helper's own sentence ("opened Spotify (frontmost after 0.4 s)"). If it
does not confirm within 4 s — Preview with no document never takes focus — the planner runs as if
nothing had been tried.

## What was measured

Request classification, scored on `tests/fixtures/routes/` by `tools/route_eval.py`. A reflex that
fires on a request labelled planner-only counts as **unsafe**, and the ship bar — precision ≥ 95 % on
≥ 15 fired reflexes, zero unsafe — was fixed before the first run.

| Set | Rules | Decision |
|---|---|---|
| holdout 1, 90 requests, unseen | 92.7 %, 1 unsafe | disabled |
| holdout 2, 100 requests, unseen | 89.1 %, 2 unsafe | disabled |
| **holdout 3, 110 requests, unseen, rules frozen before it was written** | **96.2 % on 52 fired, 0 unsafe** | **enabled** |

On holdout 3, counting only complete reflexes: rules alone 34 fired, 34 right, coverage 74 %; Jev
native alone 42 fired, 41 right, 0 unsafe; **rules then Jev native 45 fired, 44 right, 0 unsafe,
coverage 96 %**. Jev's one miss opened the Maps app for "Open Google Maps". laya asked one relative
question: 70 %, 2 unsafe, 10 fired.

The lane's router is measured separately (`tools/fastlane_eval.py --router`, see
[voice.md](voice.md#lane-first-the-router-before-the-planner)): 23 of 23 on holdout, 1.16 s and
1.54 s live against 13.1 s.

## Opening an app in front of Codex

Computer use now goes to Codex (`codex_computer.py`), and a Codex turn costs seconds
before it does anything. A bare "open Spotify" doesn't need that, so
`app_reflex.ReflexFirstAgent` wraps the agent the daemon builds (voice and Telegram
alike):

1. **The rules** (`task_router.classify`, code, microseconds) take the wording they
   know.
2. **Jev** (`EVE_ROUTER_MODEL=jev`, over `EVE_JEV_ROUTE`) is asked only
   when the rules find no reflex and no gate has fired. It can only name an
   installed app, under the shipped `JEV_REQUEST_GATES`.
3. **A complete bare launch** runs `open -a <App>`. Anything else goes to Codex
   unchanged, and so does a failed `open` or a Jev error. The Codex agent is
   only built at that handoff, so an app launch never uses up the warm one
   (below). `EVE_REFLEXES=0` turns this off.

Chosen by `tools/route_eval.py --model {jev,laya} --native` on 2026-09-23. Each
model was asked in its own idiom (`typed_ask.py`), with cut-offs fitted on the 270
seen requests (zero unsafe, zero wrong allowed). The table scores complete
reflexes on the 110-request holdout nobody tuned on:

| Arm | Fired / right | Coverage | Unsafe | Latency |
|---|---|---|---|---|
| **Rules, then Jev** (shipped) | 44 / 44 | 95.7% | 0 | µs when the rules hit, ~0.2 s otherwise |
| Jev alone | 42 / 42 | 91.3% | 0 | 207 ms p50, 298 ms p95 |
| Rules, then laya | 35 / 35 | 76.1% | 0 | µs, or 10 ms |
| Rules alone (pure code) | 34 / 34 | 73.9% | 0 | µs |
| laya alone | 21 / 9 | 19.6% | 1 | 10 ms p50 |

laya gets a 256-token head, so the app is a choice over a code shortlist of close
names. Even at the strictest cut-offs it fired on "Open Calendar and switch to
week view." Pure code is the fastest arm but misses a quarter of launches
("Get me Preview.", "Take me to Safari."). Jev closes most of that gap without
one unsafe fire, and costs ~0.2 s only on the requests the rules didn't
recognise.

**Quitting** follows the same path, with the owner's rule (2026-09-23): only the
explicit word **quit** counts. "close", "exit", "kill" and "force quit" stay with
Codex.

- **"quit <App>"** is a graceful quit, like ⌘Q, sent without waiting. An app
  with unsaved work shows its own save dialog. The reply says whether it quit,
  checked every 0.25 s for up to 2 s.
- **"quit all"** quits every regular app except those in `QUIT_ALL_KEEP`, plus
  `EVE_QUIT_ALL_KEEP`. The built-in list keeps the terminals Claude Code
  runs in, Claude, ChatGPT/Codex, eve's own app, and Finder.
- **Refused:** a question, a second clause, two apps, a tab or window, or an app
  that isn't installed. All of these go to Codex.

Jev is asked only when a request says "quit" (and not "force") and the rules
didn't take it. It answers two absolute yes/no questions ("only quitting one
app?", "quitting all apps?") and picks the app from the installed list. Its
cut-offs are fitted on `quit_tuning.json` with zero wrong quits allowed.
`tools/route_eval.py --quit` scores it on `holdout_quit.json`, 90 requests
written blind by an author who saw neither the code nor the rules. The bar is at
least 15 fired and 100% precision, because a wrong app quit is never
acceptable:

| Arm | Quits right | Coverage | Unsafe | Latency |
|---|---|---|---|---|
| **Rules, then Jev** (shipped) | 45 / 45 | 95.7% | 0 | µs, or ~0.2 s when asked |
| Jev alone, behind the "quit" gate | 42 / 42 | 89.4% | 0 | 220 ms p50 |
| Rules alone | 39 / 39 | 83.0% | 0 | µs |

Jev has to sit behind the "quit" gate. Asked about "close Mail" or "exit
Slack", it reads each as quitting one app (0.82 and 0.96). By meaning that's
right, but the owner's rule is about the word, and a literal reader cannot know
that. Fitted without the gate, no cut-off was clean. The earlier holdouts'
"Quit Spotify." and "Quit Safari." were relabelled from planner-only to quit
under the new rule; "Close Safari." stays planner-only.

**Codex warm-up** (`codex_warm.py`). Before a cold Codex run uses the goal, it
spends 4.8 to 11 s on the app-server, `initialize`, an ephemeral thread and the
`cua_repl` check (all measured 2026-09-23). `WarmCodex` does that ahead of time and
hands the ready agent to the next task that needs Codex. That task starts its
turn at once, and handing the agent over measured about 0 ms. Rules for the warm
agent:

- There is at most one, and it is used once.
- The next one is warmed after a task ends, never during it.
- It is replaced after `EVE_CODEX_WARM_SECS` (default 900) or if its
  app-server dies.
- `EVE_CODEX_PREWARM=0` turns it off.

## Clicking the front window before Codex (the desk lane)

A request that is one or two clicks on the window in front ("switch to week view", "year view and then next
year", "all clear") is done by `desk_lane.py` before Codex is started. It runs right after the launch reflex
above declines, in the daemon, on a thread:

1. **Refuse without looking** (`lane_router.refuse_reason`, `names_other_app`): a question, an empty or long
   request, or one that names another installed app ("open notes and make a list") goes to Codex.
2. **One accessibility snapshot** of the frontmost window. The keyword gate must find exactly one control
   whose label accounts for every word of the request (`require_cover`); a two-to-four-clause request is
   tried clause by clause under the same rule. It never types and never presses a key.
3. **Every stop of the fast lane applies**: a sensitive label ("delete", "send", "quit", "close window", …),
   an open dialog, a System Settings value, a hit-test miss that the AXPress check cannot clear.

| Outcome | What happens |
|---|---|
| complete: every click seen to change something | eve says what it clicked ("Done. I clicked All Clear."). Codex never starts. |
| partial: something clicked, not all of it, or not verified | Codex runs the task, told what was already clicked, so it checks the screen and does not click it again blind |
| none / refused | Codex runs the task unchanged |

Measured 2026-10-06 on this Mac, live: "all clear" with Calculator in front was clicked, verified and answered
in 2.19 s (the first task also loads PyAutoGUI), with no Codex call. Requests the lane declines ("find the email
from Sam", "what is seven times eight", "delete everything", "open notes and make a list") reached Codex with
0.18–0.27 s added. The router's offline numbers are in [voice.md](voice.md#lane-first-the-router-before-the-planner):
on holdout it engaged on 23 of 82 cases and was right on all 23, with no sensitive click.

It needs Accessibility access for the daemon's Python; without it the lane logs `desk lane: off` once and every
task goes to Codex. `EVE_DESK_LANE=0` turns it off. The chief's approved one-way acts skip it and go to
Codex alone.

**The visual judge behind it (off by default).** When the keyword gate misses (`no_match`, `ambiguous_target`,
`uncovered`) and `EVE_DECISIONS` is `1` or `shadow`, the lane asks `decision_lane.route` with the visual judge
(`visual_judge.py`): Gemma 4 26B describes zoomed crops of the offered controls, Jev picks one under the lane's
gates, and Clef (Cloudflare) is asked only if Gemma or Jev fail in transport. `shadow` decides as a dry run on a background
thread and only logs scores; Codex takes the task at once, never held for the judge. It failed its blind eval on 2026-10-10 (Gemma+Jev covered 0.375 of the
click cases against a 0.40 bar, 20 points below Jev alone, with p95 5.8 s), so `EVE_DECISIONS` stays off:
[decisions-computer-use.md](../decisions-computer-use.md).

## Controlling your logged-in Chrome (attach mode)

The owner's priority (2026-09-23): eve should drive the **real, signed-in
Chrome**. With `EVE_BROWSER_ATTACH=1`, the Playwright lane
(`browser_lane.py`) attaches to your running Chrome instead of launching its
own profile.

- **How it connects.** Chrome 136+ ignores `--remote-debugging-port` on your
  default profile. The supported route (Chrome 144+; this Mac has 153) is to
  switch on remote debugging **once** at `chrome://inspect/#remote-debugging`.
  Chrome then writes `DevToolsActivePort` (a port, then a WebSocket path) into
  `~/Library/Application Support/Google/Chrome`. The lane reads that file
  exactly as Google's chrome-devtools-mcp `--autoConnect` does, and attaches
  with Playwright's `connect_over_cdp`. `EVE_CHROME_DIR` moves where it
  looks.
- **What it touches.** It works only in **its own new tab**. Closing it closes
  that tab and disconnects. It never closes your context, your windows or
  your other tabs. The same approval gate applies: a sensitive control (buy,
  send, delete, pay…) stops the plan until you say yes.
- **If it's off.** When remote debugging is off or Chrome isn't running, the
  lane says how to switch it on and never guesses.
- **Your click is the gate.** Chrome asks **"Allow remote debugging?"** for
  every new connection. On macOS, other apps can't read Chrome's
  `DevToolsActivePort` because app-data protection returns "Operation not
  permitted". So when that read is refused, the lane connects on Chrome's
  debugging port (9222 by default, `EVE_CHROME_DEBUG_PORT`; it's the
  "Server running at" that chrome://inspect shows), and **your Allow click
  grants access**. It waits up to 2 minutes for the click. One connection
  serves many pages, so you click once per connection, not per page.
- **Verified 2026-09-23 on your real Chrome.** The attach took 6.1 s including
  the Allow click, and read `example.com` in 0.13 s. A second page on the same
  connection took 0.43 s with no new prompt. Afterward only eve's tab had
  closed; your tab and Chrome were untouched.
- **Verified 2026-09-23 on a stand-in Chrome** (Chrome for Testing with
  `--remote-debugging-port=0`, which writes the same file). The lane attached
  in 0.75 s, typed a Wikipedia search and pressed Enter in its own tab. The
  "owner's" tab and the browser were still there afterward
  (`tests/test_browser_attach.py`; the live test runs with `EVE_LIVE=1`).
- **You approve from your phone** (`chrome_consent.py`). When eve connects,
  it watches for Chrome's own "Allow remote debugging?" dialog and texts you
  on Telegram: *"eve wants to control your Chrome… Allow it? yes / no"*.
  - A clear yes (`consent.py`, fail-closed) makes eve press **Allow**,
    using Accessibility's AXPress; Chrome's web-UI buttons ignore a
    synthetic click. A press only counts once the dialog has gone: Chrome
    ignores a press in the dialog's first moments while still reporting it
    done (measured 2026-09-24: 9 of 10 first presses did nothing, every
    second press 0.5 s later landed), so eve presses, waits half a second,
    checks, and presses again — up to five times.
  - A no, anything unclear, silence for 3 minutes, or Telegram being off
    makes it press **Cancel**. The task then goes to Codex, which drives your
    Chrome without this prompt.
  - It only answers the dialog raised by its **own** connection, and only one
    reading exactly "Allow remote debugging?" with an Allow button.
  - The daemon keeps **one** connection for its whole life, so the text
    comes once per Chrome session. If Chrome restarts, the next web task
    reconnects and asks again.
- **Or never asked** (`EVE_CHROME_ACCESS=allow`, default `ask`). This is
  your standing yes: eve presses **Allow** on its own the moment the
  dialog shows, with no text. It still acts only on the dialog raised by its
  own connection, and only on Chrome's own "Allow remote debugging?". If the
  dialog is still up after five presses, it never presses Cancel; Chrome's
  own 2-minute wait runs out and Codex takes the task. The daemon logs every outcome as
  `chrome-consent: <outcome>` (for example `auto_allowed` or `no_dialog`), and
  logs the mode at startup on the `chrome lane: on` line.
- **Which profile.** eve identifies each open Chrome profile by its
  signed-in Google account. It asks Google's account list with that
  profile's cookies, so no tab opens. Every profile is asked **at once**,
  within 3 s in total (`PROFILE_LOOKUP_BUDGET_SECS`), and the answer is kept
  for the life of the connection. With only one profile open there is
  nothing to choose, so nothing is asked. (Before 2026-09-24 the profiles
  were asked one after another with 8 s each: 7 s and 19 s before a task
  began, and no account identified.) A task that names
  an account works in that profile, by the address, its name before the @, or
  its domain word: "my work inbox" goes to owner@work.example, "canvas on my
  school account" to student@school.example, "gmail" to owner@gmail.com. `EVE_CHROME_PROFILE` sets the default. A
  profile needs an open Chrome window to be reachable, and naming one without
  a window gets a plain "open a window in that profile".
- **Which tasks.** A goal the Chrome lane can finish alone
  (`browser_lane.is_browser_only_goal`) tries your Chrome first. That is a web
  goal (`is_web_goal`: a URL, a site name, the browser) that needs nothing on
  the Mac: it does not say "on my computer" or "the desktop app", and it
  names no Mac app, unless it names the browser for it ("open spotify in
  chrome") or is a web search about the app ("google how to reset
  spotify"). Apple's everyday-word apps (News, Weather, Maps …) don't count
  as naming a Mac app: "the latest news on Google" is a web search.
  Everything else, including anything the lane can't finish, goes to Codex.
  "Open Spotify on my computer and on my browser" and "open spotify and
  chrome" go to Codex, which has both the Mac and the browser.
- **Only the owner's words are routed.** The Telegram door gives the task a
  note about files ("give the full path … do not open it in a browser").
  It goes beside the goal (`ReflexFirstAgent.run(goal, note=…)`) and is
  added only after the route is chosen. Until 2026-09-26 it was part of the
  goal, so its word "browser" sent every Telegram task to Chrome, and its
  length meant "open Spotify" was never a bare launch. "Open up Spotify"
  opened the website instead of the app.
- **"Open it" after a link.** A task's goal is the owner's words for that
  one request, so a link sent in an earlier message was not in it
  (2026-09-24: a Google Maps link, then "Use Google search to open it up").
  `app_reflex.with_referenced_links` adds the links from the owner's last 4
  messages (newest first, at most 3) when the goal refers back ("open it",
  "that link") and has no link of its own. The Telegram door passes its
  recent owner messages to it when it starts a task (`telegram.py`
  `_start_task`). Every task logs the goal's shape, never its words:
  `app-reflex: chrome-lane gets a goal of 7 words, 1 link(s) (maps.app.goo.gl), refers back`.
- **Nothing done in 15 s → Codex** (`chrome_lane.NOTHING_DONE_BUDGET_SECS`).
  If no plan is executing and no step has been reported 15 s after the task
  started (still connecting, still waiting on Chrome's Allow, a slow plan
  call), the lane stops and Codex takes the whole task. A plan that has
  started is never cut off. Why 15 s: on 2026-09-23/24, once connected, the
  lane reached its first step or gave up in 4–6 s; the runs that did
  nothing still spent 11.3 s and 25.6 s before handing on, and one waited
  36 s on Allow until you pressed Stop. A question ("how many
  unread…") is planned as navigation, then answered from the page's visible
  text, read once it stops loading. A redirect to a sign-in page is reported
  as "you're not signed in".
- **Measured 2026-09-23 on your Chrome** (`tools/chrome_lane_eval.py`, five
  read-only tasks). Hacker News, Amazon and weather.com were answered
  correctly in 7.5–11.3 s. Gmail was read too early (fixed since: the read now
  waits for the page to fill in), and GitHub is signed out in that profile.
  The Codex comparison run is still to do.
- **Caution.** While remote debugging is on, any local program can connect to
  your Chrome, not just eve. Switch it off in the same place when you don't
  need it.

## eve's own Chrome (no Allow to press)

The owner, 2026-09-27: "lets give eve its own profile". With
`EVE_BROWSER_OWN=1`, the Chrome lane drives **a second Chrome that
belongs to eve** instead of yours. Everything in attach mode above still
applies (its own tab, the approval gate, 15 s then Codex), except the parts
about the Allow dialog and profiles.

- **Why there's no dialog.** Chrome refuses `--remote-debugging-port` only on
  your **default** user data folder, which is why your Chrome asks "Allow
  remote debugging?". eve's Chrome runs from its own folder,
  `~/.config/eve-bridge/chrome` (`EVE_OWN_CHROME_DIR` moves it), and is
  started with `--remote-debugging-port=0`. Chrome picks a free port and
  writes it to `DevToolsActivePort` in that folder. eve can read that file
  because the folder is its own, so nothing asks and nothing is pressed.
- **It's real Google Chrome**, started with `open -na "Google Chrome"`, not
  Playwright's launch. It stays open between tasks and shows as a second
  Chrome icon in the Dock. If it isn't running, the next web task starts it.
  eve trusts a port file only while something listens on that port, so a
  file left by a Chrome that has quit is ignored.
- **Sign in once: `eve-bridge chrome-profile`** (or `… chrome-profile <url>`).
  Google's sign-in may refuse a Chrome that another program can drive, so
  the command:
  1. quits eve's Chrome through its own port (never by app name, which would
     also match your Chrome),
  2. reopens the same folder with **no port** at Google's sign-in page,
  3. waits while you sign in to each site eve may use and then quit it (⌘Q),
  4. starts it again with the port, opens Gmail in eve's tab, and says
     whether it landed in the inbox.

  The sign-ins stay in the folder. If eve needs the browser while the
  sign-in window is still open, the task says to quit it and goes to Codex.
  Google's account list (`ListAccounts`) answers 400 to a request made from
  outside a page, so a real Gmail load is the check.
- **One profile.** eve's Chrome has one profile, so `EVE_CHROME_PROFILE`
  and the account lookup don't apply. Meet joins as the account signed in
  there.
- **The trade.** Any program on this Mac can drive eve's Chrome through its
  port, with no prompt. That's why it holds only what you sign into it, and
  never your own Chrome's sessions.
- **Verified 2026-09-27** (Chrome 154, this Mac):
  - A cold start plus connect took 0.8 s with no dialog, and a reconnect 0.18 s.
  - The owner signed in with the port off. After a restart with the port,
    Gmail opened the inbox, and the check read signed in.
  - The same check on an empty profile read not signed in.
  - Quitting eve's Chrome left the owner's Chrome running.
- **Switching.** `EVE_BROWSER_OWN=1` turns the lane on by itself, so it
  doesn't need `EVE_BROWSER_ATTACH`. `EVE_CHROME_ACCESS` does nothing
  in this mode. The daemon's startup line reads
  `chrome lane: on — web tasks try eve's own Chrome first … no Allow to press`.
- **Codex needs your Chrome open.** Codex's browser extension lives only in
  your own Chrome profile, never in eve's. Before the lane hands a task to
  Codex, it checks for your Chrome and starts it (`open -na "Google Chrome"`)
  if it isn't running (`browser_lane.ensure_owner_chrome`). Plain `open -a`
  would only bring eve's Chrome to the front. On 2026-10-07, with only
  eve's Chrome open, Codex answered "the browser-control connection is
  unavailable" after 8 s.

## Public-web reading tasks go to the web reader

A task that isn't a reflex goes to your computer: Codex, which drives your
real Chrome (signed in to your accounts) and the rest of the Mac, or the
Chrome lane for a web goal. Many tasks don't need any of that. "What's the
weather in Seattle this weekend", "compare the Linear and Jira team plans",
"what does the React doc say useEffectEvent does" only need someone to read
public web pages. Those go to the **web reader** (`web_reader.py`), which uses
[TinyFish](https://docs.tinyfish.ai): one search, one fetch that reads the top
three pages, then one cheap model call that answers from those pages. It takes
seconds, needs none of your accounts, and never takes over your screen.

Firecrawl was the web reader until 2026-09-29. It moved to TinyFish because
TinyFish Search and Fetch are free, while Firecrawl charged a credit a page.
Firecrawl came back the same day as one of three search providers, kept for
what only it has: the Alexandria data tools (flight fares, government
records). With the search router on, the web reader answers through it like
every other search ([below](#web-searches-go-to-the-provider-that-fits)).
With it off (the default until its gates pass a blind holdout), the web
reader is TinyFish alone, exactly as described here.

**Which body** (`browser_router.py`): Jev decides, asked the way TypeSafe's
docs recommend for a routing decision:

- **State:** only the request.
- **Options:** the two bodies (`owner_computer`, `web_reader`) are the options
  of one question, each described with its `what`, `for` and `not_for`.
- **Yes/no questions**, one judgment each:
  - Does it need your signed-in accounts, or something only you know?
  - Does it need the Mac, or a local or staging address?
  - Do you want it shown on your own screen?
  - Does it need something typed into a site: a form, a code, a date?
  - Is the whole job reading public websites, reported back as text?

  Code combines the answers. The web reader gets the task only when every
  condition holds *and* the choice picks it with probability ≥ 0.95. An error,
  a timeout, or any doubt keeps the task on your computer.

**Measured:**

- The first wording, fitted on `browser_tuning.json` and scored once on
  `browser_holdout.json`, routed only 12 requests (all correct). That is under
  the bar of 15, so it was held.
- The typing question then read too literally ("look up definitions" counted
  as typing). It was reworded, refitted on all 122 seen requests with zero
  unsafe allowed, and scored once on `browser_holdout2.json`. That set is 79
  requests written blind by an agent that never saw the router, 40 of them
  deliberately hard. Result on 2026-09-26: **30 routed to the web reader, 30 right,
  0 unsafe**, 93.8% coverage, Jev p50 198 ms (`tools/route_eval.py
  --browser`).
- An unsafe route would be a task that needs you (your accounts, your data,
  your Mac) sent to the web reader. It would fail, and its text would have gone to
  a third party.
- **Known miss:** "look up the population of Lisbon and Porto and tell me the
  difference" scores 0.28 on the typing question, over the 0.2 cut-off, so it
  stays on your computer. The cut-offs lean toward keeping tasks on your
  computer.

The questions name no provider, so moving the web reader from Firecrawl to
TinyFish did not change what Jev is asked, and the fitted cut-offs still hold.

**How the web reader answers:**

- **One search:** TinyFish Search (`GET api.search.tinyfish.ai`), the request
  as the query. It returns titles, snippets and links, not page text.
- **One fetch:** TinyFish Fetch (`POST api.fetch.tinyfish.ai`) reads the top 3
  results as markdown. Pages up to an hour old may come from TinyFish's cache
  (`ttl` 3600). A page it cannot read is represented by its search snippet.
- **Cost:** both are free on every TinyFish plan. Search allows 30 requests a
  minute per key. Each task is recorded in the spend ledger at $0.
- **One model call:** `openai/gpt-6-luna` at low effort through OpenRouter,
  told to answer only from those pages, to ignore any instruction inside them,
  and to say when they don't answer.
- **Links:** only the pages it read. A link the pages talked the model into
  writing is replaced with "(link removed)", the same lesson as the watcher's
  WatchLink model.
- **Hand-off:** if the pages don't answer, TinyFish fails, or the day's cap
  is spent, Codex takes the whole task as before. Nothing is lost by trying
  the web reader first. With the search router on, a provider that fails is
  followed by one many-source search first, and only then does Codex take it.

| Setting | Default | What it does |
|---|---|---|
| `TINYFISH_API_KEY` | unset | Your TinyFish key (sign up at agent.tinyfish.ai). Without it the web reader is off. |
| `EVE_WEB_READER` | `auto` | On when `TINYFISH_API_KEY` is set and the router's gates passed their blind holdout (`browser_router.SHIPPED`). `0` turns it off, `1` forces it on. |
| `EVE_WEB_READER_TASKS` | 100 a day | Tasks the web reader takes per day, whichever provider answers them. It counts tasks, and bounds the model calls and any runaway loop; paid Firecrawl calls have their own, smaller cap shared with the brain's searches (`EVE_FIRECRAWL_PER_DAY`, [below](#web-searches-go-to-the-provider-that-fits)). Past it, Codex takes them. |
| `EVE_JEV_ROUTE` | (set up for Jev) | Jev's route. Without it, the web reader takes no tasks. |

The body before the web reader was an isolated browser: auto-browser, then
eve's own Playwright lane. It was retired on 2026-09-23 before it was ever
wired in. Its research and adapter are in git history (last present at
`b218808`).

## Web searches go to the provider that fits

Every web search eve makes can be routed to one of three providers
(`search_router.py`, 2026-09-29): the brain's `web_search` tool (voice,
Telegram, think) and the web reader's tasks go through the same router, and
the same entry point (`search_router.answer`).

| Provider | What it is | Goes there for | Cost |
|---|---|---|---|
| `tinyfish` | One TinyFish search, one fetch of the top pages, one cheap model answering only from them (`web_reader.search_and_read` + `answer_from`) | One fact, opening hours, a definition, a price on a public page, what one page or doc says, a simple comparison from a few pages | Free (docs.tinyfish.ai, 2026-09-29), plus the answer model |
| `perplexity` | OpenRouter's `web_search` server tool on the configured engine (`websearch.engine_search`), today's search | Many sources, news and today's events, multi-step research, anything vague, **and every doubt**: it is the safe default | About $0.005 a search plus the answer model |
| `firecrawl` | Firecrawl search with Alexandria's live data tools (`firecrawl.py`): the model may pick one tool and its options, eve runs it and answers from its data | Live fares for a route and dates, prices from a data service, official records | Paid credits, recorded under `firecrawl` |

**Last resort: Monid.** When the many-source search fails on both
OpenRouter engines (and neither failed with a 4xx other than 429), Monid's
Google results get one try in the time that is left, at most 12 s, $0.002 a
run, under their own daily caps. The result has the same shape, with
`fallback` "monid". See [Monid](../monid.md).

**How Jev is asked** (the same idiom as the web-reader router above):

- **State:** the request, and one line saying eve is about to search for
  its owner and the text is either the brain's query or the owner's words.
- **Options:** all three providers as one Choice (`quick_page_read`,
  `deep_web_research`, `live_data_service`), each with its `what`, `for` and
  `not_for`, the quick-read boundary written on both sides (a settled fact
  that stays the same over time is the quick reader's; an answer that
  changes over time is the many-source search's). The words name the job,
  never a brand. **Jev always sees all three**, whichever keys this Mac
  holds, so the question never varies and the fitted gates hold on every
  machine; a pick whose key is missing is turned into the many-source search
  in code.
- **Yes/no questions**, one judgment each: `needs_live_structured_data`,
  `answer_changes_over_time` (does the correct answer change over time or
  hang on recent news, so a page may be stale),
  `needs_many_sources_or_searches` (many sources weighed together, or
  several searches in a row), `single_lookup_or_page_read`.
- **Code decides:** Firecrawl only when its Choice probability and the
  structured-data answer both clear their gates; TinyFish only when its
  probability and the single-lookup answer clear theirs *and* both the
  changes-over-time answer and the many-sources answer are under their
  maximums (`SearchGates`: `firecrawl`, `structured`, `tinyfish`, `single`,
  `many_max`, `changes_max`); everything else is the many-source search. An
  error, a 2 s Jev timeout, a missing key, today's Firecrawl cap spent, or a
  provider that fails, doesn't answer or runs past its time budget: one
  many-source search.
- **Wrong routes** are the costly direction only: a request that needs many
  sources, a current answer or a data service sent to the quick reader, or
  anything that doesn't need a data service sent to Firecrawl. The gates
  are fitted with zero wrong routes allowed on the seen sets
  (`tools/route_eval.py --search`: `search_tuning.json` +
  `search_holdout.json`) and scored once on the blind
  `search_holdout2.json`.
- **The bar** (pre-registered 2026-09-29, before the fresh holdout was
  scored): at least 20 requests routed away from the many-source search,
  zero wrong routes to Firecrawl, and at least 95% precision on routes to
  TinyFish. All three: `SHIPPED=True`; otherwise `False`. `route_eval.py`
  prints exactly this verdict.
- **Result** (2026-09-29, one run on live Jev, 90 blind requests): 37 routed
  away from the many-source search (need 20), 0 wrong routes to Firecrawl,
  21 of 22 TinyFish routes right (95.5%, need 95%). The one wrong route:
  "what's the tallest building in the world" went to TinyFish. Jev p50 189 ms.
  `SHIPPED=True`. Caveat: the holdout repeats two already-seen requests word
  for word; without them TinyFish precision is 20 of 21 (95.2%), still a
  pass by one route. Write a new blind set before any refit.
- **Firecrawl fares, measured live:** three one-way fare questions answered
  from Google Flights data in 6.9–7.8 s each, inside the brain's 10 s budget.
  A data tool whose required option is an id that another tool must look up
  first (named `…_id`) is never offered: the answer model picked one and it
  failed.

The first fit (on `search_tuning.json` alone) was scored on
`search_holdout.json` and sent one request whose answer changes ("who's the
CEO" of a company) to the quick reader. That split the old many-sources-or-news
question into the two above.

**Time budget:** a routed TinyFish or Firecrawl attempt runs in a worker and
is waited on for at most its budget: `EVE_SEARCH_ROUTED_SECS` (10 s) for
the brain's `web_search`, and 90 s for the web reader, whose tasks can run a
data tool. This is an outer timeout, not the per-call HTTP timeouts: past it
the router stops waiting, the worker is told to stop at its next step, and
the many-source search answers within 25 s (`DEFAULT_TIMEOUT_SECS`). The two OpenRouter engines race:
Perplexity starts first; when it has not answered in 6 s (`HEDGE_SECS`, past the normal p95), or fails with a
timeout, no route or a 429/5xx, Exa starts too, and the first good answer wins (the result says `fallback` when it
was Exa's, and `ms` is the whole wait). A 4xx other than 429 is not tried again. A voice or app search is therefore
about 2 s (Jev) + 10 s + 25 s at worst, and usually 4 to 6 s.

History: on 2026-10-08 the morning's 12 s + 8 s budget (from 74 logged searches, p95 5.1 s, slowest 5.7 s) failed
every search that evening, when the answer model (`google/gemini-3.1-flash-lite`) took 7 to 11 s just to say "ok"
and searches took 15 to 27 s; the older 20 s limit had answered them slowly. The race keeps fast days fast and
slow nights answered.

The answer model's host is pinned too (`openrouter.PROVIDER_ORDER`): Google models ask OpenRouter for Google AI
Studio first, fallbacks on. That evening OpenRouter's provider page showed `google/gemini-3.1-flash-lite` on Vertex
"global" at 0% uptime for the last 5 minutes (72.7% over the day) while AI Studio answered in 0.72 s at 100%; the
same model on the pinned host answered searches in 3-4 s. The same preference applies to every call through
`watch.openrouter` (the web reader, the chief's reflection, the search router's answers).

**Firecrawl's daily cap:** `EVE_FIRECRAWL_PER_DAY` Firecrawl calls a
local day (default 5), counted in one place (`firecrawl.py`) for both the
brain and the web reader. Each search and each data-tool run takes one before
it is sent. Once the day's calls are spent, a Firecrawl route goes to the
many-source search, and the reason is logged.

**What stays the same:** the brain's tool result has the same shape and cost
fields whichever provider answered (`ok`, `answer`, `sources`, `usage`, `ms`,
`cost_usd`). `cost_usd` is a number only when every part was priced. When the
answer model reported no cost, or Firecrawl credits have no price
(`EVE_FIRECRAWL_USD` unset), it is `null` ("unpriced", as the spend
ledger records it), never a partial figure that looks complete. The
many-source search keeps its pre-router figure: OpenRouter's reported cost,
else the search fee alone. Only links eve read survive: pages it fetched,
sources the search cited, links a data tool returned; any other becomes
"(link removed)". With routing on, the `web_search` tool's description tells
the brain what the search can reach (quick page reads, and live fares and
records when Firecrawl is set up), so it writes queries that name the route,
dates or name. Each routed search logs one line: the provider, the Choice's
probability and Jev's milliseconds, never the query.

| Setting | Default | What it does |
|---|---|---|
| `EVE_SEARCH_ROUTER` | `auto` | `auto`: routes when `search_router.SHIPPED` is true and a Jev route is set up. `on`: routes before the fit, for a live trial. `off`: the brain searches on `EVE_WEB_SEARCH`'s engine and the web reader reads through TinyFish, exactly as before the router. |
| `EVE_SEARCH_ROUTED_SECS` | 10 | The brain's time budget for a routed TinyFish or Firecrawl attempt (1 to 60 s). Past it, the many-source search answers. The web reader's budget is at least 90 s. |
| `OPENROUTER_API_KEY` | unset | The many-source search and every provider's answer model run through OpenRouter. Without it, or with `EVE_WEB_SEARCH=openai`/`off`, nothing is routed. |
| `TINYFISH_API_KEY` | unset | Lets a quick-read pick run. Without it, that pick goes to the many-source search. Jev is asked the same question either way. |
| `FIRECRAWL_API_KEY` | unset | Lets a live-data pick run. Without it, that pick goes to the many-source search. |
| `EVE_FIRECRAWL_PER_DAY` | 5 | Firecrawl calls a local day (a search or a data-tool run each count one), shared by the brain and the web reader. `0` stops them all. |
| `EVE_FIRECRAWL_USD` | unset | One Firecrawl credit's price on your plan, for the spend ledger and `cost_usd`. Unset: Firecrawl lines are recorded unpriced, and a Firecrawl answer's `cost_usd` is `null`, never guessed. |
| `EVE_JEV_ROUTE` | (set up for Jev) | Jev's route. Without it, nothing is routed. |

The request's words go to TypeSafe or OpenRouter (Jev) when routing is on,
as they already do for the web-reader router.

## Head moves

"hey eve, look left" takes about 3.2 s today (voice → backend → `move_head`; daemon log,
2026-09-20 16:42). `tools/head_eval.py` scores three cheaper deciders on 110 utterances written by an
author who saw no code — 66 pose commands, 44 non-commands, 34 of those built to tempt a keyword
matcher ("scroll down a bit", "look up the weather", "you were right"):

| Decider | Right pose | Wrong pose | False moves | Latency | Leaves the Mac |
|---|---|---|---|---|---|
| **Jev, native** (test half) | **97.0 %**, messy speech 12 of 12 | 0 | 0 of 22 | 244 ms | the utterance |
| keyword rule, untuned | 71.2 %, messy speech 55 % | 1 | 4 of 44 | 0 ms | nothing |
| laya, native (test half) | 12.1 % | 3 | 0 of 22 | 10 ms | nothing |
| laya, told which are commands | direction 81.8 %, exact pose 62.1 % | — | — | 10 ms | nothing |

Jev does this job now, behind `EVE_HEAD_MODEL=jev` (`voice_agent._fast_head_move`). Only a turn that
mentions a direction or the head is sent to it (`HEAD_WORDS`: a filter, not a judge — "scroll down"
passes it and Jev says no). A sure pose turns the head at once through the same `Head.move` the backend
uses, with the backend's own numbers ("left" 70°, "a bit" 15°, `head.POSE_MOVES`). The ordinary routing
still runs and sees that the head has moved, and if the voice delegated the same turn, the backend's
`move_head` is answered "already done" instead of turning the head a second time — a relative nudge must
never run twice. `none`, an abstention, a timeout (1.5 s) or any error leaves the turn to the backend
exactly as before. laya is not ready, zero-shot: it ranks
directions well and sizes badly ("a bit" against "all the way" is an ordinal judgement), and it cannot
gate. The labelled utterances are the seed of the fine-tuning set its authors say it needs.

## Plan once, execute with Jev

The lane lost its A/B to turn overhead, not to its clicks: the planner spent a ~3.4 s turn to call
`delegate` and another to say it was done ([voice.md](voice.md#the-fast-lane)). So with
`EVE_PLAN_EXEC=1` a request that reaches the planner is **planned once and then executed without it**:

```
astra, one call     the request + the front window's outline (labels and roles; no title, no field
                    contents, no screenshot) → a typed plan: plan_contract.py, strict JSON schema
executor            plan_executor.py walks the steps. A click is fast_lane.run_delegate, one step, on a
                    FRESH snapshot: the exact label first when the planner read one in the outline, then
                    its description. open_app/open_url are the helpers' own calls; type goes into the one
                    editable field, or Jev's pick among several; press_key is one key.
no closing turn     done is code: every step applied, none a suspected no-op, every `expect` held.
                    The sentence is the plan's `final_say`, or a local one that says what could not be
                    confirmed.
the floor           no plan, a bad plan, a checkpoint or a step that finds no control → today's
                    turn-by-turn loop, with a [note] of what was already applied
```

A plan is descriptions, never coordinates or element ids, so a plan written for a screen that has moved on
fails closed: the step finds no control. If the request names an app that is not in front, it is opened
before the outline is read (a planner shown nothing answers with a `checkpoint`); and because planning
takes seconds while the human keeps using the Mac, the executor brings the planned app back to the front
before its first step.

**Who decides a click.** `typed_ask.ask_jev_step` asks Jev the way this page says Jev must be asked: one
request carrying the `target` choice over the lane's own menu **plus an explicit `none`**, and beside it three
absolute nouls — `present`, `already_done`, `risky`. Two things were learned the hard way and are asserted
by tests: the nouls are answered against the *state*, so the control list has to be in the state too (with it
only in the choice's criteria, `present` read 0.33 for a right control and 0.37 for a missing one); and the
window title buys nothing (top-1 58/69 without it, 57/69 with it), so it is never sent.

In the lane's `jev` decide mode the keyword gate **proposes and Jev can refuse**: a gate pick is pressed only
when Jev's own top control is the same one; otherwise the step is Jev's, under cut-offs fitted with zero wrong
presses allowed. `tools/jev_step_eval.py`, on the Accessibility fixtures (menu of 25):

| holdout, 82 cases — **read before, so a tuning-set number** | pressed | right | wrong | coverage |
|---|---|---|---|---|
| the keyword gate alone (the click path until now) | 45 | 40 | 5 | 55.6 % |
| Jev alone, under the fitted cut-offs | 42 | 42 | 0 | 58.3 % |
| **the gate's pick only if Jev agrees, else Jev** (`jev` mode) | 52 | 51 | 1 | 70.8 % |

Jev's raw top-1 when the right control is on the menu: 57 of 60 on select (laya, the lane's old local model,
removed from the click path 2026-09-24: 29.7 % on the cases it was asked). Requests: 234 ms p50, 289 ms p90 over OpenRouter. **This is not a ship decision**: both fixture sets
had been read during the laya work, and `--check-default` refuses to enable `JEV_STEP_DEFAULT` without
`--fresh DIR`, a set nobody has read. Until one exists both switches ship off.

**What only you can approve.** A plan's `consequential` flag can only add a stop. So can everything else:
`ax_candidates.is_sensitive` (which gained the verbs the request-level gates already knew — Place Order,
Confirm, Publish, Transfer, Clear History, Format Disk, Turn Off, Add to Cart, Kill … — because a plan
executor presses controls nobody named aloud), Jev's `risky` noul (unfitted: 0.91 for "put the file in the
trash", 0.42 for the highest harmless step on select; cut at 0.5), a Return outside a search you dictated, and
composed text headed for a shell, which asks before the first key. The human is asked directly, with no planner
turn, and their yes lets exactly that control through once. Text the planner wrote is typed but never submitted
without that yes; whether text was yours or composed is checked against the request, not taken from the plan.

**Live, 2026-09-21** (`tools/plan_live.py --act`, Calendar, one planner call each):

| task | plan call | executor | total | before |
|---|---|---|---|---|
| "put the calendar on the year view, then go to next year" (2 clicks, both `confirmed`) | 3.72 s | 2.60 s | **6.64 s** | 13.1 s turn by turn, 16.9 s with `delegate` |
| "switch the calendar to the month view" (Calendar in front; label read from the outline) | 4.07 s | 0.86 s | 5.24 s | — |
| "open Calendar and switch it to the year view" (planned blind from a terminal) | 6.80 s | 2.25 s | 9.29 s | — |
| a three-step plan written by hand (`--plan`), no planner call | — | 2.88 s | 2.88 s | — |

Three runs on one app are a demonstration, not a measurement: the A/B with a code oracle per task, against the
turn-by-turn loop **and** against plan-once with the keyword gate alone (`plan_live.py --keyword-only` is that
arm), is what may flip `PLAN_EXEC_DEFAULT`. The plan call is now the whole cost, and `low` is the lowest
reasoning effort `gpt-6-astra` accepts.

## The browser lane: Playwright as the hands, Jev as the judge

The lane's ceiling on the Mac is the Accessibility tree over web content:
nameless buttons, stale frames, coordinate clicks. `browser_lane.py` gives web
goals a better body. Playwright drives **your own Chrome** in attach mode
([above](#controlling-your-logged-in-chrome-attach-mode)), and the page
supplies the candidates: every visible control with its role, accessible name, state and
box, from one JavaScript evaluate. Controls above or below the viewport are
collected too, after the on-screen ones, and are scrolled into view before a
click or a type. A click lands on the element, not on a
point. "Did it work" is a DOM question: the URL, the title, the text now on
the page, what is typed in a field.

Nothing about the brain changes. The page is presented as the same
`ax_candidates.Snapshot` of `Candidate`s the window walk produces, so
`plan_executor.run_plan` — astra's one plan, the keyword gate, Jev's typed step
answer, the sensitive-label table, "only the human approves" — runs unchanged
on top of it. The lane implements the nine-method senses/effectors contract
(`snapshot`, `text_visible`, `focused`, `frontmost_pid`, `screen_changed`,
`click_candidate`, `focus_and_type`, `press`, `settle`), plus `approve` (the
label the human's yes covers for the current click, set by the executor) and
`clear_consent`: before a click or a type, a cookie or consent notice is
dismissed with a button that refuses or closes. A button the sensitive table
knows ("Accept", "Agree", "OK") is never pressed there, so a notice that only
offers to agree stays up and the step stops at `dialog_open`.

```
goal ── is_browser_only_goal? (a web goal with nothing on the Mac in it) ──▶ browser lane
   no │                                                                          │ outline: role + label per control
      ▼                                                                          ▼
 reflex → lane → plan-once → planner (the Mac tiers)          astra plans once (plan_contract, "Frontmost app: browser")
                                                                                 │
                                                                                 ▼
                                             run_plan: per step the page's candidates → gate + Jev → Playwright → expect
                                                                                 │
                     complete → one sentence · checkpoint → re-plan on the new page (≤ 2) · partial → the screenshot planner
```

A web search ("search the web for ramen near me") is one navigation with zero
model calls. When the lane takes a goal the Mac tiers stand down, so the URL
is never also opened in Safari.

Measured on this Mac, 2026-09-21: Chromium up in 0.19 s, a page in 0.52 s, a
full control snapshot in 40 ms. A four-step plan (open, tick a checkbox, type,
Return) ran through the real executor against a local page in the tests in
under 3 s including launch, every step `confirmed` by its oracle; a sensitive
control (`Place Order`) stopped for the human and a yes let exactly that
through.

Install: `pip install -e ".[browser]"` then `python -m playwright install chromium`.

### Choosing the browser planner model

`tools/browser_model_eval.py` runs `ComputerAgent.run_in_browser` with only
the planner model changed. The plan request, the executor, Jev on every click,
and the page read are all production's. It uses eve's own headless Chromium
with a throwaway profile, never your Chrome. There are twelve tasks. Nine are
on local fixture sites (served on 127.0.0.1 and routed as
`https://*.eve-eval.test`): a contact form, a filtered shop cart, a search
then a result, a table read, a toggle behind a tab, a cookie banner, a button
below the fold, a reservation form, and a two-page read. Three are read-only
on example.com and Wikipedia. Success is decided by code: the fixture
server's recorded state, or a pattern in the answer. OpenAI models were called
directly then (since 2026-10-08 every model goes through OpenRouter's Responses API,
[one key: OpenRouter](../openrouter.md)). Other models went through OpenRouter's
Responses API with the same request. Measured 2026-09-24, 3 runs per task (36 per model), Codex not
behind the lane:

| model | success | avg time | planner cost per task | handoffs |
|---|---|---|---|---|
| google/gemini-3.7-flash | 18/36 (50%) | 7.4 s | $0.0026 | 18 |
| gpt-6-sol | 18/36 (50%) | 7.1 s | $0.0034 | 18 |
| gpt-6-astra (default) | 16/36 (44%) | 7.2 s | $0.0152 | 21 (and 2 wrong answers) |
| gpt-6-luna | 16/36 (44%) | 6.4 s | $0.0002 | 20 |
| z-ai/glm-5.3-flash | 9/36 (25%) | 15.0 s | $0.0001 | 26 (does not hold the plan schema) |

Added 2026-10-10, one run per task on the 12 tasks of that day (not the 36-run table above, so a look, not a
verdict): `google/gemma-4-26b-a4b-it` as the planner finished 4 of 12 (33%) at 47 s a task on average, with 8
handoffs and one 180 s timeout, for $0.0008 of planner spend a task. It held the plan schema, but it is slower and no
better than the models above, so `EVE_BROWSER_MODEL` is unchanged.

The differences among astra, sol, luna and gemini-flash are within noise. The
bigger lesson was that six of the nine local tasks failed for every model, and
the causes were in the lane, not the planner:

- the typed-field picker returned `ambiguous_field` on a form with several
  fields;
- a `checkpoint` after an in-page tab handed off;
- an open cookie dialog refused every click;
- controls below the fold were never collected;
- from a blank tab, astra and luna declined an empty outline, and the others
  opened the site and then checkpointed;
- when the page read found nothing, astra's answer was the executor's
  "Done: open url …".

**After the lane fixes (2026-09-24, branch `fix/browser-lane-bugs`)**, the same
twelve tasks, 3 runs each:

| model | before | after | avg time before → after | planner cost per task | planner calls per task | handoffs after |
|---|---|---|---|---|---|---|
| gpt-6-astra (default) | 16/36 (44%) | 36/36 (100%) | 7.2 s → 7.8 s | $0.0152 → $0.0171 | 1.42 → 1.42 | 6 |
| gpt-6-luna | 16/36 (44%) | 36/36 (100%) | 6.4 s → 6.9 s | $0.0002 → $0.0002 | 1.36 → 1.53 | 7 |

Every finished answer was correct (astra 30/30, luna 29/29). Every submit, save
and add-to-cart still stopped for the human's yes. What changed, one commit
each:

- a form's field is the one the step names (or the plan just clicked), with
  Jev's agreement, and a field named exactly is no longer traded for a
  withheld look-alike ("Your message" versus "Send message");
- controls below the fold are collected and scrolled to;
- a cookie notice is refused (never accepted) before a click;
- a checkpoint asks the planner again with the new page's controls and the
  steps already done, at most twice (`MAX_BROWSER_REPLANS`), and the browser
  lane has its own planning wording (`PLAN_INSTRUCTIONS_BROWSER`): plan past a
  tab the request names, open the site on a blank page, `needs_eyes` only when
  no web page could do it;
- a blank tab opens the URL in the request before planning;
- a question is answered from the page text or handed off, never with a
  navigation step, and a re-plan may say nothing is left;
- the human's yes reaches the page's own sensitive-label gate, for that click
  only (the page used to refuse an approved "Send message");
- Return in a search form waits for the results page.

The remaining handoffs are all `search_then_link` and `cookie_banner` (plus one
contact form): the work was done, but the plan's own `expect` named text that
the fixture page does not show (its article title is generic), so the executor
stopped, as it should, rather than claim the step. Real pages usually carry the
name. With the lane fixed, luna matches astra at about 1/88 of the planner
cost. The default is unchanged until the owner decides; `EVE_AGENT_MODEL`
is the knob, and it moves the Mac planner too.

Run it:
`python tools/browser_model_eval.py run --models gpt-6-luna --repeats 3 --out-dir DIR`,
then `report --out-dir DIR`.

## Knobs

| Variable | Default | What it does |
|---|---|---|
| `EVE_REFLEXES` | `1` | launch and web-search reflexes before the planner; `0` turns them off. The default is `tools/route_eval.py`'s decision, and `--check-default` asserts it |
| `EVE_HEAD_MODEL` | `off` | `jev`: a spoken head pose ("look left", "a bit lower") is chosen by Jev in about a quarter of a second instead of ~3.2 s through the backend, which remains the fallback. The words of a turn that mentions a direction go to TypeSafe / OpenRouter |
| `EVE_ROUTER_MODEL` | `off` | `jev`: ask Jev, natively, about a request the rules did not recognise; it may only add a bare launch. The request's words go to TypeSafe / OpenRouter, so it is your switch |
| `EVE_DESK_LANE` | `1` | the desk lane: keyword-decided clicks on the front window before Codex ([above](#clicking-the-front-window-before-codex-the-desk-lane)); `0` sends every desktop task to Codex |
| `EVE_DECISIONS` | off (`visual_judge.SHIPPED`) | the desk lane's visual judge after a keyword miss ([above](#clicking-the-front-window-before-codex-the-desk-lane)): `1` clicks its pick, `shadow` only logs scores. Crops of the window go to one pinned OpenRouter host ([openrouter.md](../openrouter.md)) |
| `EVE_GEMMA_MODEL` / `EVE_GEMMA_PROVIDERS` / `EVE_GEMMA_TIMEOUT` | `google/gemma-4-26b-a4b-it` / `nextbit` / `4.0` | the judge's looking model, its pinned host (no fallbacks, data collection denied) and its time limit |
| `EVE_CLEF` (+ `EVE_CLEF_MODEL`, `EVE_CLEF_TIMEOUT`, `EVE_CLEF_PATH`) | `0` | Clef as the judge's transport fallback: `shadow` logs only; `1` acts once `CLEF_SHIPPED`, else runs as `shadow`. Needs `CLOUDFLARE_ACCOUNT_ID` and `CLOUDFLARE_AUTH_TOKEN` |
| `EVE_LANE_FIRST` | `1` | the lane's router ([voice.md](voice.md#lane-first-the-router-before-the-planner)) |
| `EVE_FAST_LANE_DECIDE` | `keyword` | who picks a lane step under the planner: `keyword`, `model`, or `jev` (the gate proposes, Jev can refuse; the step's words and the window's control labels go to `EVE_JEV_ROUTE`, never the title) |
| `EVE_JEV_GROUPS` | `0` | `1`: a Jev step asker reads up to 100 controls instead of 25, asked as groups of 25 in one request and then a final question over each group's top three (`typed_ask.ask_jev_step_grouped`). A menu of 25 or fewer is asked exactly as before. Off by measurement: see [Borrowed](#borrowed-and-from-where) |
| `EVE_AX_PRESS` | `1` | when the lane's hit-test misses the control it means to click, press that same control through its accessibility `AXPress` action instead of giving up, after checking it is still that control (same app, enabled, offers AXPress, within 2 pt, same title). Never both a mouse click and a press for one action. `0` restores the plain refusal |
| `EVE_PLAN_EXEC` | `0` | `1`: the planner plans once and `plan_executor.py` walks the plan with no planner turn between steps or at the end ([above](#plan-once-execute-with-jev)). Uses Jev for grounding when a route is configured, the keyword gate alone otherwise |
| `EVE_PLAN_EXEC_REASONING` | `low` | the plan call's reasoning effort |
| `EVE_DECIDER` | `unset` | the lane's model in `model` mode: `jev` is the only value; unset, `model` mode stays off. laya is not in the click path any more (removed 2026-09-24); it still drives the eye expressions and is scored by the routing evals |
| `EVE_EXPLORE` | `0` | `1`: eve starts exploring on its own after ten idle minutes. Off: exploring is explicit only ("go explore", a text, `eve-bridge explore`) — owner decision, 2026-09-21 |

Every routing decision is in the task's run log (`{"route": …}`, `{"reflex": …}`, `{"lane_first": …}`),
so a wrong route can be read after the fact. Since 2026-09-21 the log ends with the **bill** (`{"bill": …}`):
the model's tokens in, cached and out and their USD at the grounded rates (`pricing.py`: gpt-6-astra
$10 / $1 cached / $50 per million, developers.openai.com, 2026-09-21), the Jev calls, input tokens and USD
made during the run ($0.042 per million, output free), and the wall seconds. `usd` is null for a model the
table does not know, never a guess. `tools/bill_report.py` sums a runs directory, one row per task and a
total. The Jev Engineering article's rule: "track the bill per completed task", so a cheap decision that
sent a worker down the wrong branch shows up as what it cost.

## How the evidence was kept honest

- A holdout is written by an author who has seen neither the code nor any other set, and is run
  **once**. The moment it is read it becomes a tuning set (`holdout1.json`, `holdout2.json`), and a new
  one decides. It took three.
- The bar is fixed before the run, in the tool. A sub-policy noticed after the fact (bare launches were
  16 of 16 on holdout 2) was not trusted until an unseen set agreed (19 of 19).
- A model's cut-offs are fitted on data it is not scored on: half of the head utterances for the other
  half; the 270 read requests for the 110 unread ones.
- Known soft spots: the requests are text a voice model would write, not audio; the head utterances
  are authored, not recorded; Jev is not deterministic (the other session saw 85.3 % and 86.6 % on two
  runs of the same set), so a gap under three points is noise; and holdout 3 has now been read too.

## Borrowed, and from where

- **Long control lists as groups, and pressing the observed element when the hit-test misses** —
  [musubipapi/computah](https://github.com/musubipapi/computah) (MIT, Swift; Jev over the Accessibility tree,
  assessed 2026-10-06). Two of its pieces were ported:
  - *Grouped choice* (`JevChoice.swift` → `typed_ask.ask_jev_step_grouped`, `EVE_JEV_GROUPS`). Its two
    rules came with it: a short list is asked unchanged, and if every group answers none the step abstains.
    Measured live on the AX fixtures under the shipped cut-offs (`tools/jev_step_eval.py --menu 100 --grouped`
    against `--menu 25`): the right control was offered in 61/69 vs 60/69 (select) and 62/72 vs 62/72
    (holdout); the lane's path pressed 52 right and 2 wrong on select either way, and 52/1 vs 53/1 on
    holdout; p50 rose from 299 to 532 ms on select. Every control the 25-menu missed was disabled when it was
    captured, or withheld as sensitive. None was cut by the cap, so grouping ships **off**. The next place to
    measure it is a web page with more than 25 relevant links (the Chrome lane).
  - *AXPress fallback* (`AXActor.swift` → `desktop_helpers._LaneAdapter._press_instead`, `EVE_AX_PRESS`).
    The walk now keeps each candidate's live element (`Snapshot.ref`, never serialised). A press that the app
    answers with an error counts as sent, with its effect unknown, so the lane never retries it. Checked
    live on Calculator through a forced hit-test miss: All Clear, 7, 8 read 0, 7, 78. It only acts where the
    desktop lane used to refuse, and that lane is not in the live path today (desktop tasks go to Codex).

- **Bands, and abstention means the planner** — [matrixorigin/Astra](https://github.com/matrixorigin/Astra),
  a self-hosted agent runtime (not the OpenAI model) whose admission classifier is the closest prior art:
  "An unknown critical answer does not authorize execution", and it "never silently reuses the main agent
  model". Assessed as solid: 616 commits, CI, committed evaluation evidence including its failures.
- **Thresholds by consequence; a noul beside every choice** — TypeSafe's intent-routing and
  confidence-routing recipes.
- **The typed model picks only from options code built, and never writes text** —
  [browser-use/jev-ultrafast](https://github.com/browser-use/jev-ultrafast). Its 7.07 s figure is one whole
  11-action task (about 0.64 s an action), not a step. It clicks the argmax at any probability and has no
  notion of a consequential action, so none of its gating was taken.
- **Plan once, then ground each step: a closed step vocabulary, `target` as "how it would be labelled on
  screen", a `none` option on the grounding choice** — [savka777/jev-use](https://github.com/savka777/jev-use)
  (MIT), its dormant `Planner.swift` path. Assessed as the right shape with no evidence: one commit, no eval,
  every threshold a literal, and on that path a planned click gets no destructive check at all. The shape was
  taken; the thresholds and the safety model were not.
- **What an action did is one word from a closed set, and "unverifiable" is never success** —
  [trycua/cua](https://github.com/trycua/cua)'s action-result contract (MIT). Its own Jev recipe offers one
  executable candidate a step, so it proves plumbing and says nothing about choosing.
- **Nothing was taken from** [awlevin/typesafe-computer-use](https://github.com/awlevin/typesafe-computer-use):
  a fixed 2.0 s sleep after every action that its headline step time leaves out, one uncalibrated 0.4 threshold,
  no confirm path, and an OCR change-detector that misses text-sized changes (a toast, "Cart (7)" → "Cart (8)").
- **Reflexes under everything, a slow planner that never blocks action** —
  [rmalde/minecraft-agent](https://github.com/rmalde/minecraft-agent), assessed as *thin but real*: the three
  tiers are implemented and its async-planner test passes, but it is one commit by one author, and every
  headline number rests on evidence its README says is "excluded from Git", on a fixed seed in Peaceful
  mode. The ideas stand because they are ordinary control practice with other sources behind them; none of
  its numbers or timer constants were used.
