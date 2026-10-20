# ax-candidates unit (ax_candidates.py → src/ax_candidates.rs, src/ax_ffi.rs, src/bin/ax_candidates.rs)

Every top-level definition of `ax_candidates.py` is already listed in the crate's PARITY.md API table; this
fragment adds the private helpers the port introduced and records tests, boundaries and differences.

## API additions (private helpers, not Python names)
| Python | Rust | Notes |
|---|---|---|
| (urllib.parse internals: `_check_bracketed_netloc`, `_check_bracketed_host`, the ValueError) | `try_urlsplit`, `check_bracketed_netloc`, `check_bracketed_host` (private) | `urlsplit` is the never-failing public face; `is_account_change` uses the checked one so a ValueError fails closed as the Python's `except` does |
| `SENSITIVE_LABEL.search(text) is not None` | `SENSITIVE_PRESENT` (private `regex::Regex`) | the same existence question with the lookarounds as consumed boundary characters or text edges; `SENSITIVE_LABEL` stays public as the fancy-regex lookaround pattern |
| `ax_walk.scalar` / `fetch` | `AxScalar`, `MacAx::scalar`, `WalkNode`, the `fetch` closure in `ax_walk_with` | |
| pyobjc bridging | `ax_ffi::convert` | CFBoolean → Bool, CFNumber → Int or Float by its type, CFString → Str, AXValue point/size → Point/Size (AXError, range, rect → Null), CFArray → Array, CFAttributedString → its string, AXUIElement → Element |

## Tests
| Python file | Python tests | Rust tests (tests/ax_candidates.rs) | Not ported (why) |
|---|---|---|---|
| test_ax_candidates.py | all 27 | same names; plus test_sensitive_agrees_with_the_lookaround_pattern_and_survives_long_labels, test_walk_keeps_each_candidates_live_element, test_walk_without_a_pid_or_a_window_is_empty, test_urlsplit_parse_qsl_unquote_match_python, test_fixture_dict_round_trip_keeps_numbers | the `is_sensitive(None)` / `is_sensitive(42)` asserts and `rank_candidates(kinds="anything")` ValueError: labels are `&str` and `kinds` is an enum, so neither input can be written |
| test_fixtures_ax.py (tests_ref) | all 5 | test_fixtures_exist (added: the parametrised tests loop over the files, so an empty glob must fail), test_no_fixture_is_in_both_sets, test_fixture_rederives_from_its_raw_dump (loops over the 13 fixtures; compares with Python's `==` and also `Snapshot ==`), test_fixture_cases_point_at_real_candidates, test_denylist_catches_a_known_positive_control, test_no_fixture_carries_owner_data | none |
| test_side_effect_links.py (tests_ref) | the is_account_change / needs_confirmation / rank tests (8 of 9, parametrised ones as loops) | same names; `page()` builds the Snapshot browser_lane.snapshot_from_page would | test_the_browser_click_refuses_an_upvote_link_without_a_yes (browser_lane._Page, another crate); test_non_strings_fail_closed keeps only the "missing href is no href" assert (None / 42 labels cannot be written) and adds the urlsplit-ValueError fail-closed case |
| test_side_effect_precision.py (tests_ref) | the is_account_change / needs_confirmation / Snapshot.href / rank tests (10 of 14) | same names | test_gate_asks_for_an_href_only_account_link, test_the_lane_confirms_an_href_only_vote_link_instead_of_clicking_it (fast_lane unit), test_the_outline_hint_uses_the_lanes_own_confirmation_rule (desktop_helpers unit), test_the_browser_lanes_snapshot_carries_query_bearing_hrefs_to_the_gate (browser_lane crate) |

Live check (2026-10-19, this Mac): `eve-ax-candidates --app <name> --json` and `python -m eve_bridge.ax_candidates
--app <name> --json` gave equal `raw`, `snapshot` and `candidates` (secs aside) for Finder, Wispr Flow, Slack and
Google Chrome.

## Boundaries
None. The AX walk, NSRunningApplication / NSWorkspace and CGDisplayBounds go through objc2 (`ax_ffi`), macOS only.
Off the Mac `ax_walk` returns the windowless dump, `pid_for_app` 0, `app_identity` ("", "") and
`main_screen_points` None (the Python raised ImportError there).

## Differences
- `is_sensitive`, `is_account_change` and `needs_confirmation` take `&str` / `&Candidate`, so Python's fail-closed
  branch for a label that is not a string has no caller. `is_sensitive` matches with an automaton
  (`SENSITIVE_PRESENT`) that cannot fail at match time; fancy-regex's VM hits its backtrack limit on long text where
  Python's `re` does not, which would have made long labels "sensitive" for the wrong reason.
- Python's `\s` also matches U+001C..U+001F (`str.isspace()`); Rust's does not. `SPACES_RE`, `URL_RE`'s `\S` and the
  `\s` in the sensitive phrases spell those characters out, so whitespace collapsing and phrase matching agree.
  `\w` / `\d` use Rust's Unicode classes, which differ from Python's only on combining marks.
- A NaN or infinite float cannot sit in a `serde_json::Value`: the walker writes `null` where Python's dump carried
  NaN / Infinity. An infinite frame is no frame either way; a NaN AXValue would read "nan" in Python and "" here
  (the committed fixtures carry one, on an AXScrollBar that is never a candidate). The fixture tests read those
  bare tokens as `null`.
- `urlsplit` does not port `_checknetloc`'s NFKC check (a non-ASCII netloc whose NFKC form introduces `/?#@:`
  raised ValueError, which is_account_change turned into "asks"); there is no Unicode normaliser in this crate's
  dependencies. Bracketed hosts are validated as Python 3.13 does (IPv6 or IPvFuture only).
- `Candidate.frame` is a 4-tuple: `Candidate::from_dict` rejects a frame of any other length (Python built a
  tuple of whatever length it got). A raw dump node that is not an object reads as an empty node (Python raised
  AttributeError).
- A live element is keyed by its `NodePath` (child indices from the root) instead of Python's `id(node)`.
- CLI: argument errors are clap's wording (exit 2, `--help` exit 0, as argparse).
- `str()` of an attribute list in a title slot reads `[<AXUIElement>, ...]` (Python printed pyobjc reprs with
  addresses); no app has been seen to answer a list for AXTitle / AXDescription / AXRole.

## Note for integration
Parallel fillers of this crate must not share one `CARGO_TARGET_DIR`: cargo's metadata hash for a workspace path
package does not include its directory, so two worktrees building `eve-desktop-lanes` into `target-wt` overwrite
each other's rlib and fingerprint, and a test run can link another worktree's skeleton. This unit was verified in
`target-wt-ax-candidates`.
