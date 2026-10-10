# Accessibility fixtures for the fast-lane eval

One file per app state, in `select/` (style and threshold choice) or `holdout/` (the
ship decision, read once). A file never appears in both sets, and `tests/test_fixtures_ax.py`
re-derives every snapshot from its raw dump with the current `ax_candidates.py`.

## Format

```jsonc
{
  "captured": {
    "ax_candidates_sha": "<sha256 of src/eve_bridge/ax_candidates.py at capture>",
    "macos": "26.0", "app_version": "15.0", "at": "2026-09-21T09:12:00+00:00",
    "truncated": false,          // a fixture is a complete walk
    "redacted": true,            // every personal label replaced (see below)
    "set": "select"
  },
  "screen": [1512, 982],         // main display in points; the viewport filter uses it
  "raw": { ... },                // the walk dump from `ax_candidates --json` (shape in that module's docstring)
  "snapshot": { ... },           // Snapshot.to_dict(), derived from `raw` — never hand-edited
  "cases": [
    {"goal": "switch to week view", "expected_id": "7", "expected": "click",
     "overlap": true,            // does a goal token appear in the expected label/value?
     "distractor": false,        // does a WRONG candidate share more goal tokens than the right one?
     "text": null,               // for type cases
     "must_not_offer": ["31"],   // ids the menu must drop (page links, sensitive controls)
     "note": "Week is a radio button with AXValue 1 when selected"},
    {"goal": "print the calendar", "expected_id": null, "expected": "abstain",
     "overlap": false, "distractor": false, "text": null, "must_not_offer": [], "note": "no such control"}
  ]
}
```

`expected_id` is the `Candidate.id` in the (redacted) snapshot. An `abstain` case is correct
when the pick is `abstain` or `reobserve`.

## Capture

```bash
open -a Calendar                                   # drive the app to the state by hand
.venv/bin/python -m eve_bridge.ax_candidates --app Calendar --json > /tmp/calendar-month.json
.venv/bin/python tools/fastlane_eval.py --make-fixture /tmp/calendar-month.json \
    --out tests/fixtures/ax/select/calendar-month.json --set select [--redacted]
```

`--make-fixture` computes the sha, re-derives the snapshot from the raw dump, refuses a
truncated walk, and writes an empty `cases` list for you to fill. Pass `--redacted` only
when the capture holds no personal label at all.

## Redaction

Personal data lives in labels: event titles, mail subjects, note bodies, contact and file
names. Replace each with a synthetic label of the same length and shape, so the menu the
model sees keeps its statistics:

```bash
.venv/bin/python tools/fastlane_eval.py --redact tests/fixtures/ax/select/calendar-month.json \
    --map 'Dentist 3pm=Meeting A' --map 'Call Priya=Call Sam'      # or --map-file map.json
```

`--redact` replaces whole strings (title, description, value) throughout the raw dump, then
re-derives the snapshot from it, so raw and snapshot cannot disagree, and sets
`captured.redacted` to true. Author the cases after redacting: ids do not change, labels do.
