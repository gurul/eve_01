# eve-meet
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/meet.py` | `src/meet.rs` (re-exported from `src/lib.rs`); the Python library semantics it rests on in `src/py.rs` |
| `eve_bridge/meet_page.js` (asset) | not copied: `PAGE_SCRIPT` embeds the same file with `include_str!` |

## API
| Python | Rust | Notes |
|---|---|---|
| `MEET_DEFAULT`, `MEET_HOST`, `DEFAULT_GUEST_NAME`, `DEFAULT_SUMMARY_MODEL`, `JOIN_SECS`, `NO_CONTROLS_SECS`, `DEFAULT_LOBBY_MINUTES`, `DEFAULT_MAX_HOURS`, `JOIN_POLL_SECS`, `LISTEN_POLL_SECS`, `OUT_OF_CALL_POLLS`, `PAGE_ERRORS`, `NO_CAPTIONS_SECS`, `SETTLE_SECS`, `IDLE_COMMIT_SECS`, `PARAGRAPH_WORDS`, `REVISED_TAIL_WORDS`, `CALENDAR_LOOKBACK`, `CALENDAR_AHEAD`, `TIME_SLACK`, `MAX_TRANSCRIPT_CHARS`, `OFF_REASON` | same names, `pub const` | values verbatim; the three timedeltas are `chrono::Duration` |
| `CODE` | `CODE` (`LazyLock<Regex>`) | same pattern |
| `PAGE_SCRIPT` | `PAGE_SCRIPT` | `include_str!` of `bridge/src/eve_bridge/meet_page.js`: both daemons run the same bytes |
| `TOOLS`, `TOOL_NAMES` | `TOOLS` (`LazyLock<Value>`), `TOOL_NAMES` | key order and text equal Python's (differential test) |
| `INSTRUCTIONS_BLOCK`, `SUMMARY_PROMPT` | same | byte-identical (differential test) |
| `parse_link` | `parse_link(&str) -> (Option<String>, String)` | `urlsplit` / `parse_qsl` / `urlencode` as Python's (`py.rs`); 40 edge inputs checked against Python |
| `meeting_code` | `meeting_code` | |
| `_key` | private `key` | |
| `merge` | `merge(&str, &str) -> Option<String>` | `difflib.SequenceMatcher(None, a, b, autojunk=False).get_matching_blocks()` ported in `py::matching_blocks` |
| `class Line` | `struct Line { speaker, text, at }` | |
| `class _Open` | `struct OpenBlock` | |
| `class CaptionMerger` (`feed`, `tick`, `finish`, `_emit`, `_stamp`) | `struct CaptionMerger` (`feed`, `tick`, `finish`, private `emit`, `stamp`) | `open` is an `IndexMap` so its order is the dict's (a re-rendered block moves to the end, a replaced one keeps its place). `feed` returns `Result`: the errors are what Python raises on a malformed snapshot |
| `events_in` | `events_in(&Value, depth)` | JSON strings parsed with `eve_transcripts::py::loads` (NaN/Infinity accepted as Python does) |
| `event_link` | `event_link(&Value)` | |
| `_when` | private `when` | `eve_transcripts::py::fromisoformat` (3.11 forms); a naive time is read as local (`astimezone()`) |
| `_CLOCK`, `_NOW_WORDS` | private `CLOCK` (`fancy_regex`, for the `(?!\w)` lookahead), `NOW_WORDS` | same patterns, `re.I` as `(?i)` |
| `find_meeting` | `find_meeting(&[Value], &str, DateTime<FixedOffset>)` | aware times compare by instant, as Python's do; 31 cases checked against Python |
| `class MeetPage` (Protocol) | `trait MeetPage` (async: `open`, `run`, `close`) | `run` takes the options as a JSON value and returns the page's dict |
| `class ChromeMeetPage` | `struct ChromeMeetPage::new(BrowserLane, Option<Answer>)` | `open` = `connect_answering(answer)` / `connect()` then `open_url`; `run` evaluates `PAGE_SCRIPT` on the lane's thread; `close` closes eve's tab only |
| `_LIST`, `MEET_SCHEMA` | `MEET_SCHEMA` (`LazyLock<Value>`) | same keys in the same order |
| `make_summarizer` | `make_summarizer(Option<&HashMap>) -> Option<Summarize>`; `summarizer(env, ClientOptions) -> Option<Summarizer>` | the closure as a value (`Summarizer { model, sdk, ledger }`) so a test points it at a local server and a temp ledger. Same request body (`model`, `instructions`, `input` = last 120 000 characters, strict `json_schema` `notes`, `max_output_tokens` 2500, `reasoning.effort` low), metered as `meet notes` under the bare model name, through `eve_notes::OpenRouterSdk` |
| `line_text` | `line_text(&Line)` | |
| `class NotesFile` (`_head`, `append`, `transcript`, `finish`) | `struct NotesFile` (`new`, private `head`, `append`, `transcript`, `finish`) | 0600 files (`O_EXCL|O_NOFOLLOW|O_CLOEXEC`) in 0700 folders, `-2` … `-50` names, temp + rename at the end; the file bytes equal Python's (differential test, the `ended:` clock masked) |
| `summary_message` | `summary_message(label, ended, &Map, lines, Option<&Path>)` | text equal to Python's (differential test) |
| `Notify` | `type Notify` | `(text, about) -> future` |
| `class MeetConfig` | `struct MeetConfig` (+ `Default` = the dataclass defaults) | |
| `configured` | `configured(Option<&HashMap>)` | `EVE_MEET`, `EVE_MEET_PROFILE`, `EVE_MEET_LOBBY_MINUTES` (1–120), `EVE_MEET_MAX_HOURS` (0.25–12), `EVE_MEET_QUIET`, `EVE_MEET_GUEST_NAME` (40 chars); `float()` parsing via `eve_notes::py_float`, NaN clamps to the low bound as `min(hi, max(lo, nan))` does; 15 cases checked against Python |
| `ABOUT_IN`, `ABOUT_LOBBY`, `ABOUT_CAPTIONS`, `ABOUT_JOINING`, `ABOUT_NOT_JOINING` | same | verbatim |
| `class SessionState` | `struct SessionState` | `clicks` holds the page's values as given |
| `class MeetSession` (`status`, `leave`, `_tell`, `_poll`, `run`, `_join`, `_why_not_in`, `_listen`, `_write`, `_leave_page`, `_finish`) | `struct MeetSession::new(url, label, page, notes_root, notify, SessionOptions)` (`handle`, `state`, `status`, `leave`, `run`, private `tell`, `poll`, `join`, `listen`, `write`, `leave_page`, `finish`); `why_not_in` is a free function | the clock, sleep, wall clock and page clock are lent in `SessionOptions`, as Python's keyword arguments. `SessionHandle` is what the owner's side holds while `run` owns the session (state, `leave`, `cancel`) |
| `class Meeter` (`busy`, `instructions`, `tools`, `resolve`, `join`, `status`, `leave`, `status_line`, `handle`, `close`) | `struct Meeter::new(config, make_page, notes_root, MeeterOptions)` (same methods; `busy()`; `wait()` is `await meeter.task`; `session()`) | `join` / `handle` return `anyhow::Result`: the only error is `make_page` failing, which raises out of `join` in Python |
| `_no_notify` | `no_notify()` | same log line |
| `calendar_lister` | `calendar_lister(Arc<dyn CalendarApps>) -> ListEvents` | `CalendarApps::execute` is `apps_mcp.AppsBridge.execute`; same arguments, the same RuntimeError text on a refusal |
| `make_meeter` | `make_meeter(config, notes_root, answer, apps, environ) -> Option<Meeter>`, plus `meet_lane_config` | Steel → eve's own Chrome; `EVE_MEET_PROFILE` overrides the lane's profile; same log lines. `Meeter::lane_config` exposes the lane the pages are built on (what `make_page()._lane.config` reads in Python) |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `tests/test_meet.py` | all 33 (the parametrized ones as loops) | `src/tests.rs`: same names, plus `meeter_status_leave_and_close_follow_the_call`, `a_session_cancelled_mid_call_says_eve_stopped_and_still_writes_the_notes`, `a_page_that_fails_to_open_is_told_as_it_failed` | none. `test_calendar_via_the_meeter_resolves_and_joins` builds its events in the Mac's own zone (Python's are Pacific and pass because the Mac is in Pacific): the Meeter reads "3pm" in the local zone either way |
| `tests/test_meet_page.py` | all 14 | `tests/meet_page.rs`: same names, run against Playwright's headless Chromium through eve-browser-lane's CDP driver (skipped with a line where no Chromium is installed, as Python skips), plus `chrome_meet_page_runs_the_script_on_its_own_lane` | none. `test_only_safe_by_reading_the_source` checks the Rust module's source where Python checked meet.py's, with a positive control |
| `tests/test_model_memory_roles.py` (ref) | `test_meet_summary_preserves_model_override` | `src/tests_ref.rs`: same name, all three parametrizations, through the real OpenRouter client against a local server, metering checked in a temp ledger | the other tests are other modules' |
| `tests/test_steel_local.py` (ref) | `test_meet_keeps_eves_own_chrome_when_steel_drives_the_rest` | `src/tests_ref.rs`: same name (plus the profile override and both off switches) | the other tests are browser-lane's and steel-local's |
| `tests/test_apps_mcp.py` (ref) | `test_an_event_is_compacted_for_the_model_and_keeps_what_meet_reads` (the meet half) | `src/tests_ref.rs`: same name, over a hand-compacted event | `google_rest.compact_event` itself belongs to the apps-mcp crate |
| `tests/test_telegram.py` (ref) | `test_meet_command_*`, `test_meet_join_in_words_is_a_model_turn_with_the_meet_tools` | (Meeter behavior they rely on is covered in `src/tests.rs`) | they test telegram's `/meet` command and brain turn: the telegram crate |
| (differential, no Python test) | meet.py's outputs for edge inputs, in `tests/fixtures/python_reference.json` (regenerate with `tests/fixtures/gen_reference.py`) | `src/tests_ref.rs`: `links_and_codes_match_pythons`, `merges_and_merger_scripts_match_pythons`, `the_calendar_matches_pythons`, `the_notes_file_and_message_match_pythons`, `the_configuration_matches_pythons`, `the_tools_prompts_and_constants_are_pythons`; plus `the_summary_is_the_last_of_a_long_transcript_and_a_bad_reply_fails`, `the_calendar_lister_asks_for_the_day_and_turns_a_refusal_into_an_error`, and the `py.rs` unit tests | |

## Boundaries
- Playwright is replaced by eve-browser-lane's Chrome DevTools Protocol driver (see its PARITY.md): `ChromeMeetPage` evaluates `meet_page.js` through `PageHandle::evaluate` on the lane's own thread.

## Differences
- `make_meeter` has no "Playwright is not installed" branch: the Rust lane speaks CDP itself, so there is nothing to import.
- Python checks `page.is_closed()` before evaluating; the driver has no such flag, so a `TargetClosedError` from the evaluation is read as the tab closed (`{"closed": true}`). A tab closed between the check and the evaluation was a page error in Python (five in a row end the call); here it ends the call at once as "the tab was closed".
- Cancelling a call (`Meeter::close`, `SessionHandle::cancel`) is cooperative: the call stops at its next await and the notes are written and texted, as Python's shielded `_finish` does. A cancel that lands while the notes are already being written lets them finish, where Python's outer task would return early.
- A Meet link with a port Python cannot read (`meet.google.com:x`, out of range) is refused as "that is not a plain Meet link"; Python's `u.port` raises `ValueError` out of `parse_link`. `meeting_code` of a URL `urlsplit` refuses is "the meeting" where Python raises.
- A summary reply that is valid JSON but not an object fails the summary (`ValueError`, logged); Python kept the list and failed later with `AttributeError` out of `NotesFile.finish`, leaving the owner untexted.
- `event_link` reads entry points only from a `conferenceData` object; Python raised `AttributeError` for a truthy non-dict `conferenceData`.
- `CaptionMerger.feed` on a snapshot whose `t` is NaN or infinite behaves as Python's float math does; a row `id` that `int()` would overflow on cannot occur in JSON.
