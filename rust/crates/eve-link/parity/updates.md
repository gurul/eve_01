# eve-link: unit "updates" (updates.py → src/updates.rs)
Status: complete

## API
The rows for every top-level def and class of updates.py are already in PARITY.md (architect). Additions this unit
made, all in src/updates.rs:

| Python | Rust | Notes |
|---|---|---|
| updates.chief_items (the `list_jobs` answer it reads) | updates::chief_items_from | the seam Python's test patched (`monkeypatch.setattr(link_chat, "list_jobs", ...)`); `chief_items(chief)` is `chief_items_from(&link_chat::list_jobs(chief, &{}))` |
| `datetime.isoformat()` / `isoformat(timespec="seconds")` | updates::isoformat(dt, seconds) | the snapshot's `now`, `generated_at`, the calendar window, a chief deadline |
| `email.utils.parsedate_to_datetime` (email._parseaddr._parsedate_tz) | private `parsedate_tz`, used by updates::mail_date | ported line for line: day names, RFC 850 dates, the swapped day/month and year/time, 2-digit years, the named zones, `-0000` as no zone |
| `UpdatesStore`'s file JSON | updates::StoreState::to_value | keys in Python's order, written with `json.dumps(ensure_ascii=False)` separators |
| `await u._task` (tests) | Updates::join_refresh | waits for the rebuild running now |
| `u._next_try`, `u._lost_runs`, `u._retry_soon`, `Updates._last_full_is_old` | Updates::next_try / set_next_try, lost_runs / set_lost_runs, retry_soon, last_full_is_old | public so the integration tests can reach them, as Python's tests reach the underscored names |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| bridge/tests/test_updates.py | 49 | 44 ported in tests/updates.rs under the same names, plus 7 of Rust's own (test_mail_date_reads_rfc_2822_as_python_does, test_mail_date_edge_cases_match_python, test_clip_cuts_at_a_word_past_the_middle, test_text_and_verb_follow_the_python_rules, test_merge_keeps_the_nearest_due_first_and_lost_kinds, test_the_store_writes_python_json, test_prompt_input_is_pythons_bytes; each expected value was taken from the Python module) | the 5 `test_eval_*` tests: they test bridge/tools/updates_eval.py, a Python tool outside the daemon that this crate does not port. test_chief_cards_open_ones_only checks the chief's items through `chief_items_from` (the seam Python patched); its `status["chief"] == "ok"` through gather needs a real `link_chat::list_jobs`, which another unit (chat-core) ports and tests |

How the Python fixtures became Rust: `sources_for`'s Sources subclass is `TestSources`, an `UpdateSources` impl;
`FakeApps` implements `DirectApps` (async: `slow` sleeps on tokio's clock, and the `ValueError("boom")` subclass
becomes a panic inside the call, which the gather's own task contains and counts as failed); `FakeClient` implements
`SuggestClient`. The tests give every Updates and gather an environment with `EVE_PHONE_LOCATION=0`, and the `here()`
tests point `EVE_PHONE_LOCATION_FILE` at a temp file, so no test reads the Mac's real phone fix or memory.

## Boundaries
None.

## Differences
- `gather` without a usable Composio bridge marks the five Composio kinds off itself instead of asking
  `updates_composio::reads` (which answers exactly that for no bridge), and `served` asks `updates_composio::safe_link`
  only for a card whose `link` is a string (safe_link answers None for anything else). Same results.
- `gather` passes `GatherOptions::environ` to `here()`, and `Updates::rebuild` passes the Updates' own environment;
  Python's `here()` always read the process environment. In production both are the process environment.
- The chief's read and the hidden words are read on the calling task, the sources' `vault()` and `records_dir()` just
  before their reads start (Python asked them inside the worker thread). A source read that panics is a failed
  source, as an exception was.
- A timed-out async read (mail, calendar, Composio) is aborted; a timed-out blocking read (notes, journal) keeps
  running on its worker thread, as Python's `to_thread` did.
- Log lines name an error's type where Rust keeps one (`ReadError::kind`, `SuggestError`, the OS error's Python class);
  other errors log as "Error" rather than the Python exception's class name.
- `mail_date`: a time written with dots in four or more parts ("1.2.3.4") is no date here; Python's `_parsedate_tz`
  raised `UnboundLocalError` on it, which `_mail_date` did not catch, so the whole email source failed.
- A store file whose `cards` is a number reads as no cards (Python's iteration raised `TypeError` out of `load`).
- `Updates::refresh` outside a tokio runtime answers false with a log line (Python's `get_running_loop` raised).
  `Updates::dismiss` answers false and logs when the store cannot be written (Python raised to the route).
- `chief_items`: a deadline too large for a local datetime gives the item no `when` (Python's `fromtimestamp` raised,
  failing the chief source).
