# eve-chrome-consent
Status: complete

## Modules
| Python | Rust |
|---|---|
| `bridge/src/eve_bridge/chrome_consent.py` | `src/lib.rs` (logic, seams), `src/mac.rs` (the AX API on macOS) |

## API
| Python | Rust | Notes |
|---|---|---|
| `DIALOG_TEXT`, `APPEAR_SECS`, `POLL_SECS`, `PRESS_TRIES`, `SETTLE_SECS`, `ASK_TIMEOUT_SECS`, `GONE_LINE`, `PRESS_FAILED_LINE`, `QUESTION`, `MAX_NODES` | same-named `pub const` | values and owner-facing text verbatim |
| `_ax` | trait `Ax` (`application`, `attr`, `perform`); `mac::MacAx` on macOS | `AXUIElementCreateApplication` / `CopyAttributeValue` / `PerformAction` via `objc2-application-services`; values converted like pyobjc into `AxValue` (Python truthiness and `str()` kept) |
| `_chrome_pids` | `chrome_pids`, `parse_pids` | `pgrep -x "Google Chrome"`, every all-digit word; exit status ignored as in Python |
| `_find_dialog` | `find_dialog_in` (any `Ax`, given PIDs) and `find_dialog` (this Mac) | same walk: each PID's AXWindows, DFS stack, MAX_NODES cap per window, heading/static text == DIALOG_TEXT after `py_strip` (Python's `str.strip()`, which also drops `\x1c`..`\x1f`), first AXButton whose title/description/value == label; `find_dialog` returns `(showing, has_button)` because AX elements stay on the blocking thread |
| `dialog_showing` | `dialog_showing` | `spawn_blocking` for `asyncio.to_thread`; any failure logs `chrome-consent: cannot read Chrome's dialog (<TypeName>)` and returns false |
| `press` | `press` -> `Result<bool, InvalidLabel>`, `press_in` | label other than Allow/Cancel is `Err(InvalidLabel)` (Python `ValueError`); find + AXPress in one blocking pass; failure logs `chrome-consent: could not press <label> (<TypeName>)` |
| `access_preference` | `access_preference()` (process env), `access_preference_in(&HashMap)` | `EVE_CHROME_ACCESS` `py_strip`ped + lowercased == "allow" → "allow", else "ask" |
| `ConsentBroker` | `ConsentBroker::new(ask_owner, BrokerOptions)` | `BrokerOptions` carries the keyword args with the same defaults (`default_showing`/`default_pressing` are `dialog_showing`/`press`); `last()` for `.last`; `answer_own_connection`, `_answer` (`answer`), `_press_home` (`press_home`), `_say` (`say`) same branching and outcome strings; clones share `last` |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_chrome_consent.py` | `test_a_clear_yes_presses_allow`, `test_anything_else_presses_cancel`, `test_silence_and_a_broken_telegram_press_cancel`, `test_no_way_to_ask_presses_cancel_without_asking`, `test_no_dialog_means_nothing_is_pressed_or_asked`, `test_only_allow_and_cancel_can_be_pressed`, `test_a_connection_that_finishes_first_withdraws_the_phone_question`, `test_the_dialog_is_found_in_whichever_chrome_shows_it`, `test_answered_at_the_mac_withdraws_the_phone_question`, `test_a_press_that_fails_while_the_dialog_is_still_up_says_why`, `test_a_press_chrome_ignored_is_repeated_until_the_dialog_is_gone`, `test_the_phone_wait_ends_before_the_connection_gives_up`, `test_the_standing_yes_presses_allow_without_asking`, `test_the_standing_yes_retries_a_lagging_button_and_never_presses_cancel`, `test_the_standing_yes_still_waits_for_chromes_own_dialog`, `test_the_access_preference_is_allow_only_when_spelled_allow` | same names in `src/tests.rs` (parametrized cases looped), plus `test_a_label_falls_back_to_description_then_value_and_is_stripped`, `test_pgrep_output_keeps_only_digit_words`, `test_ax_values_read_as_python_str`, and `test_live_dialog_showing_reads_without_crashing` (ignored, live, read-only) | `test_a_waiting_question_is_never_overwritten_by_the_chrome_question` tests `Daemon._ask_owner_on_phone` (eve-daemon crate). `test_the_phone_wait_ends_before_the_connection_gives_up` writes browser_lane's `CONSENT_TIMEOUT_MS` (120_000) as a local constant: the browser lane depends on this crate, not the other way round |

## Boundaries
None. Off macOS there is no Accessibility API: `find_dialog` returns `ReadError::NoAccessibility` (logged as `ModuleNotFoundError`, as Python's failed pyobjc import), so the dialog reads as "not showing" and nothing is pressed.

## Differences
- Seams are `Arc<dyn Fn .. -> BoxFuture>`: `ask_owner` returns `anyhow::Result<String>` (an `Err` is Python's raised exception). In the "no answer" log line the reason is `TimeoutError`, `Exception` (the ask returned an error) or `JoinError` (the ask task panicked) rather than the exception's own type name.
- Cancelling `answer_own_connection` is dropping its future: the phone question task is aborted from `Drop` and winds down on the runtime's next turn, rather than being awaited (`gather`) inside the cancellation as Python does.
- An `AXChildren`/`AXWindows` entry that is not an AX element is skipped; pyobjc would raise on it, which made Python's whole read fail as "not showing".
