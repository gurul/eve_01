# Image-aware decisions before Codex: the desk lane's visual judge

`EVE_DECISIONS` adds a bounded visual click loop (`decision_lane.py`) after the desk lane's keyword router
declines a request with zero input. The keyword path still finishes its measured simple requests without a model
call. **It is off by default**: the judge failed its blind eval on 2026-10-10
([verdict](evals/visual-judge-2026-10/VERDICT.md)).

## The judge: Gemma 4 26B looks, Jev decides, Clef only when transport fails

Since 2026-10-10 the judge is `visual_judge.GemmaJevJudge`, on the owner's OpenRouter key
([one key: OpenRouter](openrouter.md)):

1. **Gemma 4 26B looks** (`google/gemma-4-26b-a4b-it`). Code crops each offered control's own rectangle (plus
   12 pt), upscales it 2x, writes the control's id beside it, and packs the tiles on at most two contact sheets.
   Gemma says in four words per id what the control shows and does. It never gives coordinates: in the probe it
   hit 1 to 3 of 8 targets pointing on a full screen and 7 of 8 on a 2x crop.
2. **Jev decides** (`typesafe/jev-1.13`). It answers the lane's own questions (`target`, `present`, `covered`,
   `risky`, and `verified` after a click) over the Accessibility text, where each control gains `looks_like`
   (Gemma's words, marked as an untrusted visual description).
3. **The lane's gates decide** (`visual_judge.GATES`, fitted on the tuning set), and `decision_lane.route` keeps
   every rule it had: consequential, typing and question requests are refused before any model call; the pick
   is one offered id; the screen and Accessibility tree are observed again before the click; the click goes
   through the hit-test or AXPress; a separate call verifies it; anything unverified goes to Codex.
4. **Clef** (`clef.py`) is Cloudflare's Jev-compatible decision model, which can also read images. It is a
   fallback for **transport failures only**: Gemma or Jev erroring, timing out, or answering in a shape that does
   not translate. It is never asked after an abstain, a `none`, a low probability or a risky answer (asking a
   second model after a no is shopping for a yes), and there is never a third model. It acts only with
   `EVE_CLEF=1` and once it has passed its own holdout (`CLEF_SHIPPED`); until then `EVE_CLEF=1` runs as `shadow`
   (asked in the background, numbers logged, never acting). The slot is empty without both Cloudflare keys.

One selection call is one Gemma call (about $0.0001) and one Jev call (about $0.00002), with a budget of 5 s;
an answer that arrives after it is discarded as an error and Codex takes the task. The budget is not a wall-clock
limit: the lane waits for the call to return, which on the holdout took up to 7.7 s.

Jev's choice reply must give a probability for every offered id and the abstain key (as `decider.py` requires),
and the gate reads the chosen id's own probability; Jev's `confidence` is never gated on.

## Switches

| Variable | Default | What it does |
|---|---|---|
| `EVE_DECISIONS` | off (follows `visual_judge.SHIPPED`, False) | `1`: the judge's pick clicks. `shadow`: the judge decides as a dry run on a background thread and logs scores only; the miss goes to Codex at once, never held for the judge (one shadow at a time; a desk-lane request that arrives while one is running waits for it). `0`: off |
| `EVE_GEMMA_MODEL` | `google/gemma-4-26b-a4b-it` | the looking model. The `:free` id is refused (it answered HTTP 429 five times in five) |
| `EVE_GEMMA_PROVIDERS` | `nextbit` | the OpenRouter host(s), sent as `provider.order` with fallbacks off |
| `EVE_GEMMA_TIMEOUT` | `4.0` | seconds per Gemma call (at most the 5 s budget) |
| `EVE_JEV_ROUTE`, `EVE_JEV_MODEL`, `EVE_JEV_TIMEOUT` | unchanged | Jev, as for the other Jev features |
| `EVE_CLEF` | `0` | `0`, `shadow` or `1` (treated as `shadow` until `CLEF_SHIPPED`) |
| `EVE_CLEF_MODEL` / `EVE_CLEF_TIMEOUT` / `EVE_CLEF_PATH` | `clef` / `2.0` / `@cf/cloudflare/clef` | Clef's body model (`clef` or `clef-flash`), time limit and Workers AI path |
| `CLOUDFLARE_ACCOUNT_ID`, `CLOUDFLARE_AUTH_TOKEN` | absent | Clef's keys. Add them yourself (a Cloudflare API token with the Workers AI permission); they are never logged |

`EVE_COMPUTER_CONTROL=0` or `EVE_DESK_LANE=0` turns all of this off, because the desk lane never runs.

## The blind eval (2026-10-10)

[BAR.md](evals/visual-judge-2026-10/BAR.md) states six gates, written before the judge existed. A separate agent
session wrote the holdout (72 selection cases on synthetic app states, 20 verification pairs), frozen with its
sha256 before the judge's first commit, and scored once with `bridge/tools/visual_judge_eval.py --holdout`.

| | Gemma+Jev | Jev-only | Gemma-alone |
|---|---|---|---|
| Unsafe clicks | 0 | 0 | 7 |
| Fired clicks right | 18 of 18 | 29 of 29 | 42 of 42 |
| Coverage of 40 click cases (bar 0.40) | 0.375 | 0.575 | 0.875 |
| p95 per selection (bar 4.5 s) | 5.8 s | 0.46 s | 3.4 s |
| Errors (bar 2%) | 7.6% | 0% | 3.3% |

Gemma+Jev failed gates 3 to 6, and Gemma's descriptions lowered Jev's coverage instead of raising it, so Gemma does
not ship. A synthetic set could not ship the lane in any case; the real-capture holdout (`real-1`, captured with
`bridge/tools/visual_judge_capture.py` while the owner drives his apps) has not been run.
[VERDICT.md](evals/visual-judge-2026-10/VERDICT.md) has the details.

## Before 2026-10-10: OpenAI's Decisions beta

The first judge called OpenAI's beta `POST /v1/decisions` (`gpt-6-luna`) with screenshots and predicate questions.
It went off on 2026-10-08 when every model call moved to OpenRouter, which has no Decisions API.
`decisions.Client` and the development tools (`tools/decisions_eval.py`, `tools/decisions_live_demo.py`) still
call it with an `OPENAI_API_KEY` of their own. The sections below describe that judge and its measurements.

## The loop

1. Observe the frontmost window's Accessibility tree and a detached screen image
   in screen points. Images are bounded to 1600 pixels on their longest side for
   transport, with the original dimensions supplied alongside the control rectangles.
2. Offer up to 25 enabled, labelled controls from the existing fast lane's menu.
   One API request asks for a target choice, whether a suitable control is present,
   whether one click covers the whole step, and whether the step is consequential.
3. Require target probability and confidence at least 0.95, margin at least 0.5,
   presence and coverage at least 0.95, and risk at most 0.05. These are conservative
   experimental cut-offs; they have not been calibrated on unseen desktop tasks.
4. Reobserve both tree and pixels. Changed controls, values, window title, process,
   dialogs, truncation or pixels below the menu bar invalidate the pending decision.
   Click through the existing live hit-test and AXPress fallback, using the freshly
   observed control. The model cannot invent coordinates or execute code.
5. Settle, observe again, and send before/after evidence to a separate verification
   predicate. Require 0.95 that the entire requested step is visibly satisfied.
   Screen change or focus alone is insufficient. A selected control can satisfy its
   step without a click after selection and freshness checks pass.

Requests are limited to four explicitly worded clauses and ten meaningful words.
Questions, typing, consequential requests, another installed app, System Settings,
dialogs and truncated snapshots stay with Codex. Sensitive and secure controls
retain the existing code filters. Each control may be attempted once per route.
An effector or capture exception after a possibly delivered click preserves that
click in a `partial` result. The existing Codex handoff lists the attempted controls
and tells it to inspect the screen before doing anything again.

This first loop grounds existing labelled Accessibility controls. Vision helps
interpret those controls and verify their effect. An unlabelled canvas with no
observed actionable controls still needs Codex. It does not generate a plan for
arbitrary tasks. Animated screens may invalidate its strict freshness check.

The daemon serializes this lane with its existing desk-lane lock. Its existing
cancellation check runs after the lane returns; this change does not add interruption
of a model request or of the worker thread while a route is executing.

## Data and usage

When enabled for a request, the step, app name, control labels/rectangles and
screenshots leave the Mac for OpenAI. The text evidence excludes secure controls
and arbitrary field values; the screenshot can still show whatever is visible.
Fresh nonsecure readonly AX display values also reach the verifier; editable values
and secure descendants are excluded from this text channel. Freshness compares those
readings and context. Screenshots and request bodies are not written to the route log.
Only timings, bounded numeric gate scores and redacted failure reasons are logged. The HTTP client reuses its connection,
has a three-second timeout per request, and does not retry a decision.

Every answered request records its usage under `computer_decisions` in the existing
spend ledger, even if parsing subsequently rejects its answer. These entries are
unpriced until Decisions-specific billing is verified; ordinary Responses prices
are not silently applied to the beta endpoint.

## Evidence, 2026-10-07

The read-only comparison calls both live services with authored calendar toolbar
screens. Six target cases include three whose Accessibility labels conceal the
visible view names; four verification cases include both satisfied and unsatisfied
outcomes with and without useful text state. It performs no mouse or keyboard input.

| Measured result | Decisions | Jev |
|---|---:|---:|
| Gated correct target choices | 6/6 | 3/6 |
| Correct result verifications | 4/4 | 2/4 |
| Target request median | 190 ms | 283 ms |
| Verification request median | 198 ms | 296 ms |
| Target input tokens across six cases | 7,656 | 5,298 |
| Verification input tokens across four cases | 3,612 | 1,400 |

These are API-request timings, not total computer-task latency. Decisions receives
images; Jev receives the available text. The sample is small and deliberately
exercises image-dependent cases. It establishes working image input and a useful
diagnostic signal, not a general accuracy, cost or real-desktop speedup claim.

The [initial run](decisions-eval-initial.json) chose plausible target IDs but admitted
zero clicks: its predicates conflated view navigation with a persistent setting
change and included unrelated policy statements. Predicate wording was made atomic
and the distinction explicit, then the same cases were rerun. The table is that
[second tuning run](decisions-eval.json), not an unread evaluation set. No thresholds
were relaxed to obtain it.

Reproduce from the repo root (paid API calls, synthetic evidence only):

```sh
bridge/.venv/bin/python bridge/tools/decisions_eval.py --live --out /tmp/decisions-eval.json
```

The adapter, desktop wiring, stale-state admission, bounded execution, failure
handoffs, repeat protection and verification have deterministic tests; the
[task ledger](decisions-gates.md) records their executed checks. This feature remains opt-in.

## Visible desktop trial, October 7

The [live report](decisions-live-results.json) records eight actual Calculator requests.
The Decisions route chose and clicked 7; Calculator visibly showed 7, but the model's
verification did not reach the unchanged 0.95 threshold. Four following action requests
abstained or stopped at screenshot revalidation. The question, typing request and
request to open Safari handed off without input. There were **zero autonomously verified
completions**, one observed but unverified result, and three safe handoffs. Later
requests depended on the earlier calculation state, so this sequence is diagnostic,
not eight independent accuracy trials.

![Actual Calculator result](decisions-live-demo/capture-07.png)

The Python daemon's own capture failed because macOS Screen Recording permission was
not granted. The demonstration used Codex's permitted CUA app connection for capture
and input around the real Decisions route and real AX walker. Images and rectangles
were expressed in Calculator window points. The target-bound app PID replaced the
global foreground PID for this transport only. The production adapter was not changed.

The [first attempt](decisions-live-initial.json) made no clicks because its global
foreground observation saw Codex. The [next attempt](decisions-live-stale.json) made
no clicks because CUA's animated pointer changed screenshot pixels. The final trial
placed the pointer in the top strip and used the production-style 32-point exclusion;
later pointer motion still caused stale-observation refusals. These failures are retained.
No policy or probability threshold was lowered. The [demo ledger](decisions-live-gates.md)
and harness document the evidence and transport differences.

See the [extensive protocol comparison](computer-protocol-evaluation.md) for separately
measured AX grounding, image-dependent diagnostics and whole browser tasks. An image
classification advantage alone does not establish an autonomous desktop speedup.

Official wire reference: [Create a decision](https://developers.openai.com/api/reference/resources/decisions/methods/create).

## Swarm rerun

The [fresh eight-request Calculator report](computer-swarm/results/desktop.json) now
verifies the first click, `0 → 7`, at 0.99. Four later inputs still fail or abstain,
including three pixel-staleness handoffs; three planner-only requests hand off safely.
The whole arithmetic sequence remains incomplete. Readonly AX result evidence improved
verification while the 0.95 threshold and capture permissions stayed unchanged.
See [before/after graphs and the full evaluation](computer-swarm-improvements.md).
