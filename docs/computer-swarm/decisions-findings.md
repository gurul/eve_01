# Decisions observation improvements

The retained `docs/decisions-live-results.json` shows the literal Calculator request
`click seven` changing the display from `0` to `7`, while the route returned
`partial / unverified`. This establishes a delivered action and a verification
failure; it does not establish why the model rejected it. The baseline captured
no probability scores. Inspection identified missing evidence: AX static text
with a descriptive label discarded its displayed AXValue, and Decisions omitted
the snapshot's bounded context. Other baseline cases separately abstained or
failed freshness, so richer result evidence alone does not resolve those cases.

The raw AX observation now retains bounded static display values in transport-only
`Snapshot._readouts`. These readings require on-screen candidate geometry, omit
secure nodes and their descendants, and omit values under editable ancestors.
The Decisions evidence includes these readings and an allowlist of bounded
context derived from nonsecure metadata. Secure fields cannot provide a parent
control's fallback label. Candidate IDs, candidate value semantics, fixture JSON,
and snapshot dict round trips remain compatible. Archived snapshot dictionaries
do not carry fresh transport readings; consumers must use the actual raw AX walk
to obtain them.

Admission now explicitly compares readouts, context, focus and fullscreen state
in addition to the existing candidate, window and pixel checks. Post-action
verification checks process, app, bundle and foreground identity before consulting
the model. Logs retain the gate probabilities, AX-versus-pixel staleness and the
phase/type of exceptions, without including upstream error payloads. The
verification prompt identifies readouts as observed text and still requires
evidence that the requested step succeeded. A click or display change alone
cannot report completion.

No probability threshold, risk threshold, repeat prevention, pixel tolerance,
input permission or fallback status was relaxed. Decisions remains opt-in.
The direct Python daemon's Screen Recording failure is outside this patch;
no OS permissions were edited. No fresh live result is claimed by this leaf.
The root integration must rerun the retained live protocol through its permitted
CUA transport and record the actual outcome separately from the baseline.

Validation: after `git fetch origin`, HEAD and origin/main had zero divergence.
The leaf's declared tests passed, and the expanded regression run passed **203
tests** across Decisions, AX candidates, frozen AX fixtures, fast lane, desk lane,
and desktop helpers. Ruff passed for the changed production modules and new
observation tests. New tests exercise positive and negative observed result
evidence, unchanged displays, stale readouts with zero input, secure descendants,
editable ancestors, bounded text, archived-schema compatibility, post-action
focus loss and capture failures with retained partial actions. The positive
verification test uses a fake model to inspect actual supplied readout evidence;
it proves wiring and admission, not real-model success or calibration.

Four passes were completed: implementation; source/interface reread (caught
transport readings accidentally breaking archived Snapshot equality); regression
and counterexample hunt (added editable-ancestor exclusion and post-action focus
checks); and polish/reread with clean tests. Baseline files were not modified.
