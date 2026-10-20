# eve-canvas
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/canvas.py` | `src/lib.rs` (with `src/transport.rs` for the HTTP seam, `src/py.rs` and `src/pytime.rs` for the Python semantics it leans on, `src/html.rs` + `src/html5_entities.rs` for `html.unescape`) |

## API
| Python | Rust | Notes |
|---|---|---|
| constants `OFF_REASON`, `TOKEN_LINE`, `RATE_LINE`, `TIMEOUT_SECS`, `PER_PAGE`, `MAX_PAGES`, `DEFAULT_DAYS`, `MAX_DAYS`, `OVERDUE_DAYS`, `MAX_DUE`, `MAX_ANNOUNCEMENTS`, `MAX_LINKS`, `MESSAGE_CLIP`, `KINDS`, `_ON` | same names as `pub const` (`_ON` is the private `OFF`) | verbatim values |
| `class CanvasError` | `CanvasError { line, status, kind }` | `kind` is `CanvasErrorKind::Canvas`; `type_name()` gives the Python class for the log line |
| `class TokenRefused` | `CanvasError` with `CanvasErrorKind::TokenRefused` (`CanvasError::token_refused`) | callers match on `kind` where Python matched the subclass |
| `class WriteRefused` | `CanvasError` with `CanvasErrorKind::WriteRefused` (`CanvasError::write_refused`), carried by `TransportError::WriteRefused` | |
| `class CanvasConfig` | `CanvasConfig` | `Debug` prints Python's repr, without the token |
| `normalize_base` | `normalize_base` | urlsplit semantics ported in `py::urlsplit` |
| `configured` | `configured(Option<&Environ>)` | None reads the process environment; same warning line |
| `class ReadOnlyTransport` | `transport::ReadOnlyTransport` (a `Transport` wrapping `Arc<dyn Transport>`) | refuses every method but GET before the inner transport |
| `_LINK`, `next_link` | `next_link` | same regex, with `\s` widened by `\x1c-\x1f`, which Python's `\s` matches |
| `_error_message` | `error_message` (private) | |
| `class CanvasApi` | `CanvasApi` (`new`, `get`, `get_list`, `close`, `client`, `request`, `set_max_pages`) | async; `set_max_pages` is the seam the cap test monkeypatched `MAX_PAGES` through |
| `parse_time` | `parse_time` | CPython's `fromisoformat` ported in `pytime::fromisoformat` |
| `local_words` | `local_words(dt, Zone)` | `Zone::Local` is Python's `tz=None` |
| `_TAG`, `html_text` | `html_text(value, limit)` | `html::unescape` is a port of `html.unescape` (full HTML5 table) |
| `course_row` | `course_row` | |
| `is_current` | `is_current` | |
| `due_row` | `due_row` | |
| `TOOLS`, `TOOL_NAMES`, `INSTRUCTIONS` | `TOOLS`, `TOOL_NAMES`, `INSTRUCTIONS` | checked byte for byte against the Python's `json.dumps(TOOLS)` and the strings |
| `class Canvas` | `Canvas` (`new`, `with_tz`, `with_now`, `tools`, `instructions`, `handle`, `call`, `courses`, `pick`, `one_course`, `due`, `announcements`, `find_link`, `rundown`, `rundown_default`, `close`) | `call` is async (Python's is blocking and `handle` ran it in a thread) |
| `_days` | `days_arg` | |
| `_text` | `text_arg` | |
| `make_canvas` | `make_canvas(Option<&Environ>, Option<Arc<dyn Transport>>)` | |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_canvas.py` | `test_off_when_unset_or_half_set_or_not_https`, `test_courses_are_the_current_ones_and_the_token_rides_the_header`, `test_pagination_follows_link_next_to_the_end`, `test_pagination_stops_at_the_cap`, `test_a_next_link_to_another_host_is_not_followed`, `test_next_link_parsing`, `test_due_lists_the_window_in_local_time_with_submission_status`, `test_due_times_follow_the_zone`, `test_due_for_one_course_and_a_longer_window`, `test_a_refused_token_says_how_to_make_a_new_one`, `test_canvas_permission_401_is_a_course_refusal_not_a_dead_token`, `test_throttle_and_timeout_are_plain_lines`, `test_announcements_are_text_newest_first_with_links`, `test_find_link_across_kinds`, `test_only_gets_leave_and_a_post_is_refused_before_the_transport`, `test_rundown_part_is_short_and_only_open_work` | `tests/canvas.rs`: the same names, same assertions (a fake `Transport` stands in for `httpx.MockTransport`), plus `tools_and_instructions_name_the_four_tools`, `tools_and_prompt_text_match_the_python_byte_for_byte` (the `json.dumps` of `TOOLS`, `INSTRUCTIONS`, `TOKEN_LINE` and `OFF_REASON` as Python printed them) and `a_closed_client_fails_as_python_does`; `src/lib.rs` unit tests pin `html_text`, `local_words`, `parse_time`, `normalize_base`, `days_arg`, `is_current`, `due_row` to values the Python module returned for the same inputs; `src/py.rs`, `src/pytime.rs` and `src/transport.rs` unit tests pin `urlsplit`'s ValueError cases, `repr`, POSIX `TZ` names and `json.loads` byte decoding | `test_the_telegram_brain_gets_the_tools_only_when_canvas_is_on`, `test_the_rundown_carries_canvas_deadlines_only_when_on`, `test_self_context_names_canvas_only_when_on`: they test the `telegram`, `rundown` and `self_context` modules' wiring of this hub, and belong to those crates |

## Boundaries
None. Canvas is reached over its REST API with reqwest (rustls).

## Differences
- `CanvasApi` and `Canvas` are async (tokio); Python's were blocking and `Canvas.handle` ran `call` in a thread.
  The results are the same dicts.
- A UTC offset with a fraction of a second (`+05:30:00.5`) is read as no time (`parse_time` gives None), since
  chrono offsets are whole seconds; Python keeps it. Canvas never sends one.
- Where CPython 3.13's `urlsplit` raises ValueError on `CANVAS_BASE_URL` (an unbalanced or invalid IPv6
  bracket, an IPv4 address in brackets, a netloc that NFKC turns into URL syntax), `normalize_base` gives ""
  and Canvas stays off with the missing-setting warning; Python raised the ValueError out of `configured`. The
  checks themselves are ported (`py::urlsplit`), so a pagination link Python refuses is refused here too
  ("Canvas read failed (ValueError)").
- `Zone::Local` reads `TZ` as libc does: an IANA name, empty or unreadable (UTC), or a POSIX string such as
  `XYZ-3` (its own names; chrono's `Local` gives the offset). A POSIX string with a daylight name but no rules
  (`AAA3BBB`) takes libc's default US rules in Python; chrono does not read it, so the offset comes from the
  Mac's zone instead. With no `TZ` and no readable `/etc/localtime` link, the zone is named the way Python names
  a fixed offset ("UTC-07:00") instead of libc's abbreviation.
- `x-rate-limit-remaining` is read with Rust's float parser, which does not take Python's digit underscores.
- `resp.json()` decodes the body as `json.loads(bytes)` does (BOM, UTF-16/32 by zero bytes), but without
  `surrogatepass`: a body carrying an encoded lone surrogate is a UnicodeDecodeError here, and Python's
  `NaN`/`Infinity` literals are a JSONDecodeError. Canvas sends neither.
- `int()` of a course id string reads ASCII digits only; Python also reads other Unicode decimal digits.
