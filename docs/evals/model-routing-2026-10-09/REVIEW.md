# Fixture review record

Freeze time (UTC): 2026-10-10T01:36:28+00:00.

The independent author read only the task brief, scope plan, unlazy guidance, leaf gate, and authored evaluation artifacts. No original or changed implementation, candidate outputs, or sibling reports were inspected. No network or model calls were made by this leaf. The coordinating runner was asked to wait for this freeze.

1. Implementation: authored six role bars, three synthetic cases per role, strict schemas, exact expected facts, fixed critical paths, and an offline checker.
2. Domain review: checked corrections, negative instructions, uncertainty, source freshness, owner-versus-third-party attribution, and preparing-versus-authorizing acts across all cases. The screen's limited scope is explicit in BAR.md.
3. Defect review: clarified which reflection evidence IDs are requested; added an otherwise valid duplicate-key response as a parser control; refused optimized Python so validation and selftest assertions cannot disappear.
4. Final polish and clean pass: reread all case instructions and expected outputs, checked schema-message equality and array ordering, re-executed validation and all controls, and exercised CLI exit codes with temporary synthetic files. No further defect found.

Measured checker coverage: 18 valid responses and 463 rejected controls. Breakdown: {"duplicate_valid_response": 18, "invalid_array": 31, "invalid_consent": 3, "invalid_fact": 82, "invalid_parse_or_root": 144, "invalid_schema": 102, "invalid_type": 82, "invented_memory": 1, "valid": 18}.

CLI integration evidence: correct=0, incorrect consent=1, malformed JSON=1, unknown case=2, optimized-Python refusal=2. Temporary CLI inputs were removed. These are oracle tests, not model-quality results.

The SHA256SUMS file records BAR.md, cases.json, and check.py. Any input change invalidates this freeze and must be disclosed before scoring; changes informed by model outputs require a fresh independent set.

## Post-run independent audit

After the freeze and the coordinating agent's first screen, the same independent fixture author received explicit authorization to inspect the saved responses, runner, summaries, benchmark snapshot, and catalog snapshots. This later audit does not change the blind fixture authorship claim at freeze time. BAR.md, cases.json, and check.py remain unchanged.

1. Implemented independent response rescoring and decimal-based cost selection, a sanitized results artifact, a public verdict, and an explicitly opt-in reusable runner.
2. Reviewed the experimental configuration against BAR.md. Found omitted supported temperature settings in the exploratory runner; VERDICT.md labels the results provisional and exploratory. Flagged absent returned-model/provider evidence and filename-only cache reuse. No actual arithmetic or scoring mismatch was found.
3. Tested known corrupted raw text and corrupted summary costs: both were rejected. Tested cost tie boundaries and missing-cost handling; missing eligible cost now makes ranking inconclusive. Tested custom case IDs and matched request plans with mocked network rejection, a mocked full response, and overwrite refusal before another request. No real model calls occurred.
4. Reread the runner and public verdict. All 57 responses independently pass; 19 comparison aggregates reconcile; total reported spend is $0.034552920. Verified all eight printed GPQA rows against the benchmark snapshot, scanned public artifacts for actual private request identifiers and home paths with positive controls, and confirmed frozen input checksums. No further defect found in the reviewed scope.

The reusable runner defaults to matched supported temperature settings and accepts a fresh case manifest plus checker/bar paths. It tests the checker's bundled controls before loading the alternate case file, so a fresh manifest may use new case IDs. It needs a saved catalog covering every chosen model. The historical result verifier intentionally audits the original saved screen; it is not a confirmation-set scorer.
