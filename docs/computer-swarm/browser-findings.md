# Browser observation and benchmark diagnosis

The retained browser benchmark records 12 correct destination visits followed by
handoff for `search_then_link` and `cookie_banner` (two repeats for each of Luna,
Sol and Astra). These handoffs are **not evidence of stale page reads**.
`bridge/tools/browser_model_eval.py` serves a generic article title/body and a
generic recipe title/body. The destination documents never contain the requested
article or recipe names. The original plans ask for those names in the title or
visible text, so the executor correctly declines to assert those expectations.
The source baseline reports and fixtures were not changed by this leaf.

The browser adapter has a separate observation race: Chromium can destroy a
document execution context between an action and a DOM read. Previously that
snapshot raised immediately, and the text oracle returned false immediately.
`browser_lane._Page` now retries these two known context errors for at most the
existing three-second snapshot budget, with a hard attempt cap as well. It uses
the new document on every retry. Unrelated errors are not retried; no action is
repeated. A failed snapshot clears the cached focused candidate. Original title,
text, risk, consent and approval criteria remain intact.

Four review passes covered implementation, an expert reread of the executor and
browser adapter, regression counterexamples, and polish. The counterexamples
include an unrelated page, a click that does not navigate, a closed page,
persistent context errors, and a clock that does not advance. The actual
Chromium integration tests preserve exact title/text expectations and inject one
context-destroyed error during the post-action read. They assert one click only,
completion on the correct destination, and partial handoff on the wrong page or
no navigation. The existing safe-cookie rejection and accept-only refusal tests
also run.

An initial test attempt returned `no_candidate` before any click: its source
fixture offered only one pressable control, below the existing lane admission
gate. The fixture was corrected to include a second harmless button; the
production admission gate was preserved.

Validation: the corrected initial run passed all 19 browser and freshness tests
in 17.98 seconds with `EVE_LIVE=1`; the final leaf ledger records the
re-executed result. Ruff passed. Origin was fetched before testing and HEAD was
even with origin/main (0 ahead, 0 behind). The observation race is controlled
fault injection around real Chromium, not a newly measured incidence rate or a
claim that the retained 12 benchmark handoffs were fixed. The root integration
owns any new observable navigation predicates and their complete-task reruns.

The metadata extension exposes DOM-resolved link destinations beside visible
labelled link rows and the current page URL in the planner outline. Metadata
identifies the observed full http(s) origin/path only when the URL has no query,
fragment or credentials; ambiguous destinations are omitted rather than
shortened. Host/scheme case and default ports are canonicalized, while different
origins, non-default ports and paths remain distinct. The adapter's `current_url()`
keeps the complete raw URL from its most recent successful snapshot for the root's
strict URL oracle, including query/fragment. It performs no second page read and
clears when a snapshot fails. No expectation is changed after the action.
Host-only `site` context remains available for query/fragment pages, so the
planner does not mistake a logged-in SPA or search page for a blank tab. The
host-only field omits credential-bearing URLs and contains no path/query.

The extension's four passes checked URL parsing, snapshot/outline compatibility,
ambiguous destinations, failed observations and canonical origin/path identity.
Tests include real DOM resolution of relative hrefs, plus credentials, script,
query-only and hash-only links. The expanded suite passed 39 tests in 18.24 seconds
before gate re-execution; Ruff passed. The original fixture/report data remains
unchanged. New whole-task accuracy measurements belong to root integration.
