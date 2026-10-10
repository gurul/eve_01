# Independent fixture review

This report was written and frozen before confirmation inference. The author read the previous screen's bar, schema/fixtures, and checker to preserve its interface. The author did not inspect model responses, implementation changes, candidate models, or sibling verdicts, and made no network or model calls. All 18 scenarios are newly authored and synthetic.

## Four passes

1. Implemented three complete cases for each of the six requested roles, exact schemas and answers, critical paths, a preregistered bar, and an offline checker using the prior CLI/import contract.
2. Re-read every scenario and expected answer for domain meaning. Verified that corrections are field-specific, stale source excerpts cannot override current scoped evidence, and preferences belong to the owner. Included affirmative cases for notification and exact-scope send consent, so universal refusal does not pass.
3. Checked evidence and oracle defects. Removed three redundant sentences that directly stated target conclusions. The selftest flips every expected scalar, drops each required field, injects unsupported fields, and tests array additions, emptiness, duplication, and order. It also supplies explicit unauthorized-consent and invented-memory positive controls. Parsing rejects duplicate keys even when the final duplicated key would yield an otherwise correct object.
4. Re-read the final scenarios, bar, and freeze checker. The final review found no further factual or interface defect. Recorded the narrow scope and limits rather than treating synthetic three-case success as production quality evidence.

## Measured offline checks

Manifest validation found 18 cases, exactly three per role. Checker selftest accepted all 18 expected fixtures and rejected 454 incorrect controls: 109 schema, 84 fact, 84 type, 11 array, 144 malformed/root, 18 duplicate-valid-response, 3 consent, and 1 invented-memory controls. None is a model observation.

The freeze validator's success-only marker is emitted after checking the exact frozen file set and every digest. Its rejection paths are checked against both a mutated fixture file and a missing hash entry in temporary copies. The original frozen inputs are never modified by those checks.
