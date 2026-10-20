# eve-google-rest
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/google_rest.py` | `src/lib.rs` (the module), with `src/mail.rs` (the email package's message writer), `src/pytime.rs` (CPython datetime parsing, `isoformat`, `astimezone`), `src/py.rs` (`str`/`repr`/`int`/whitespace/base64 semantics), `src/html.rs` + `src/html5_entities.rs` (`html.unescape`) |

## API
| Python | Rust | Notes |
|---|---|---|
| `GMAIL`, `PEOPLE`, `CALENDAR`, `DRIVE`, `DRIVE_UPLOAD`, `BODY_CLIP`, `FILE_CLIP`, `THREADS_MAX`, `DOWNLOAD_MAX`, `SEARCH_BACK_DAYS`, `CALENDARS_MAX`, `EVENT_DESC_CLIP`, `PLACE_CLIP`, `RECIPIENTS_MAX`, `CONTACTS_MAX` | same-named `pub const`s | Values verbatim. |
| `_tool(name, description, props, required, reads)` | private `tool()` | Same JSON shape and key order. |
| `_S`, `_B`, `_PAGE`, `_TOKEN`, `_WHEN`, `_NOTIFY` | locals inside `TOOLS`' initializer | Same schemas and descriptions. |
| `TOOLS` | `pub static TOOLS: LazyLock<IndexMap<&str, Vec<Value>>>` | Every tool name, description, schema and readOnlyHint verbatim, in order. |
| `FILE_FIELDS`, `EXPORTS`, `TEXT_TYPES`, `CONTACT_FIELDS`, `OTHER_FIELDS` | same-named `pub const`s | `EXPORTS` is an ordered array of pairs. |
| `_ADDRESS` | private `ADDRESS` regex | `\s` widened to Python's (adds \x1c-\x1f); anchored for `fullmatch`. |
| `class GoogleError(Exception)` | `Error::Google` (type alias `GoogleError = Error`) | `Error` also carries the other classes the Python raises (KeyError, ValueError/JSONDecodeError, httpx errors, AttributeError, TypeError, OverflowError); `type_name()` is `type(e).__name__`, `Display` is `str(e)`. |
| `_header(msg, name)` | private `header()` | Case-insensitive name match, first hit. |
| `_decode(data)` | private `decode_b64()` over `py::b64decode_urlsafe` | Python's lenient decoder: junk characters skipped, a dangling character or bad padding gives `b""`. |
| `_walk(part)` | private `walk()` | Depth-first, the part first. |
| `mail_text(part)` | `mail_text(&Value) -> String` | Same regex (backreference spelled out as two alternatives), `html.unescape`, `str.splitlines`/`strip`, clip to 4000 chars + "…". |
| `attachments(part)` | `attachments(&Value) -> Vec<Value>` | |
| `_message(msg, full)` | private `message()` | |
| `rfc3339(value)` | `rfc3339(&Value)`, `rfc3339_in(&Value, Zone)` | CPython's `fromisoformat` rules and its gap/fold resolution (`pytime`). |
| `_when(value, all_day, tz)` | private `when()` | |
| `_send_updates(args, has_guests)` | private `send_updates()` | `notifyGuests` null counts as unset (`is None`). |
| `free_slots(busy, start, end, minutes, day_start=9, day_end=18)` | `free_slots(&busy, start, end, minutes, day_start, day_end, zone)` | Defaults are explicit arguments; `zone` is the Mac's (`Zone::Local`) in the daemon. Python's `max`/`min` tie rules kept (the first argument wins a tie, so its offset is printed). Same `ValueError: hour must be in 0..23` and OverflowError. |
| `_date_only(calendar_id)` | private `date_only()` | |
| `MAPS_SEARCH`, `MAPS_DIR`, `_URLISH`, `_NOT_A_PLACE`, `_CALL_LINK`, `ATTENDEE_KEYS` | `MAPS_SEARCH`, `MAPS_DIR`, `ATTENDEE_KEYS` consts; private regexes | Same patterns, case-insensitive. |
| `is_place(location)` | `is_place(&Value) -> bool` | |
| `place_links(location)` | `place_links(&Value) -> Map` | `quote(safe="")` = `urlencoding::encode`. |
| `_event_description(text)` | private `event_description()` | A call link past the clip is appended, searched from character 500 as `pattern.search(text, pos)`. |
| `compact_event(event)` | `compact_event(&Value) -> Value` | Same keys, same order. |
| `_start_key(event, tz_name)` | private `start_key()` | An all-day date is midnight in the calendar's IANA zone (`chrono-tz`), ZoneInfo's fold-0 reading; else the Mac's. |
| `_events(items)` | private `events()` | |
| `pdf_text(data)` | `pdf_text(&[u8]) -> Option<String>` | PDFKit through `objc2-pdf-kit` on macOS; `None` elsewhere, as Python without Quartz. |
| `as_text(data, mime)` | `as_text(&[u8], &str) -> Option<String>` | UTF-8 with replacement. |
| `_clip_text(text)` | private `clip_text()` | 50,000 characters. |
| `addresses(value)` | `addresses(&Value) -> Result<Vec<String>, Error>` | `not an email address: <repr>` with Python's `repr` quoting/escaping. |
| `_one_line(value)` | private `one_line()` | |
| `build_mail(to, cc, bcc, subject, body, reply_headers)` | `build_mail(&[String], &[String], &[String], &str, &str, &[(String, String)]) -> Result<String, Error>` | Byte-for-byte `EmailMessage(policy=SMTP).as_bytes()`: `_refold_parse_tree` header folding with RFC 2047 encoded words (q/b choice, combining), address-list folding, `set_content`'s 7bit/8bit/quoted-printable/base64 choice, CRLF. Also the policy's refusal of a header value with a line break by `str.splitlines`. Measured against 49 Python outputs. |
| `_person(p)` | private `person()` | |
| `_http_error(r)` | private `http_error()` | `HTTP <status>: <Google's error.message or the body, 200 chars>`. |
| `class GoogleRestClient` | `struct GoogleRestClient` | `new(app, token, timeout_secs)` (ValueError naming an unknown app), `with_transport(app, token, transport)` (the `http=` argument), `with_zone`, `with_download_max`. |
| `GoogleRestClient.list_tools()` | `list_tools()` | |
| `GoogleRestClient._req(method, url, raw=False, **kw)` | `pub async fn req(method, url, &Params, Body)`, `pub async fn download(url, &Params)` | Public: apps_mcp's `prepare` calls `_req` directly. Not signed in → `not signed in to Google (apps login google)`. Query values encoded as httpx does (`quote_plus`, true/false, repeated keys for a list). |
| `GoogleRestClient._download(...)` | `download()` | Stops reading at the cap: `the file is larger than N MB`. |
| `GoogleRestClient.call_tool(name, arguments)` | `async fn call_tool(&str, &Value) -> Result<Value, Error>` | `Ok` holds the MCP-shaped result (including `isError` ones for GoogleError, KeyError, ValueError, text `"<Type>: <msg>"` except GoogleError's bare message); `Err` is what propagates in Python (transport errors, AttributeError, TypeError, OverflowError). |
| `GoogleRestClient._page(a, default, top)` | private `page()` | `int()` semantics via `py::py_int`. |
| `_gmail_search_threads`, `_gmail_get_thread`, `_gmail_get_message`, `_gmail_get_attachment`, `_gmail_list_labels`, `_mine`, `_gmail_send_message` | same names without the leading underscore (private) | |
| `_contacts_search_contacts` | `contacts_search_contacts` | Warm-up once per client (`AtomicBool`); only GoogleError is swallowed there, as in Python. |
| `_cal(a)` | `pub fn cal(&Map) -> String` | Public: apps_mcp's `prepare` uses it. |
| `_calendar_list_calendars`, `_calendar_list_events`, `_list_one_calendar`, `_calendar_search_events`, `_calendar_get_event`, `_calendar_suggest_time`, `_calendar_create_event`, `_calendar_quick_add_event`, `_calendar_update_event`, `_calendar_respond_to_event`, `_calendar_delete_event` | same names (private) | |
| `_event_body(a)` | `pub fn event_body(&Map) -> Result<Map, Error>` | |
| `_files`, `_drive_search_files`, `_drive_list_recent_files`, `_drive_get_file_metadata`, `_drive_read_file_content`, `_drive_create_file` | same names (private) | `create_file`'s metadata part is Python `json.dumps` (", " / ": ", ASCII-escaped). |
| `_local_zone()` | `pub fn local_zone()` | Public: updates.py imports it. |
| (httpx client) | `trait Transport`, `ReqwestTransport`, `HttpRequest`, `HttpResponse`, `BodyStream`, `TransportError` | reqwest with httpx's 60 s connect/read timeouts; httpx error class names in `TransportError.kind`. |
| (token callable) | `trait TokenSource` (any `Fn() -> Option<String>` is one) | Async so an OAuth refresh can be awaited. |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_apps_mcp.py` (Google section) | `test_gmail_search_summarises_each_thread`, `test_gmail_send_builds_the_email_and_posts_it`, `test_gmail_reply_goes_into_the_thread_with_the_originals_ids`, `test_gmail_send_to_me_is_the_owners_own_address`, `test_gmail_send_refuses_what_is_not_plainly_an_email`, `test_contacts_search_warms_once_and_merges_saved_and_other_people`, `test_contacts_is_a_read_only_toolset`, `test_mail_text_prefers_plain_and_falls_back_to_html`, `test_calendar_lists_a_window_and_creates_with_a_meet_link`, `test_an_event_gets_maps_links_only_for_a_place`, `test_maps_links_follow_googles_url_scheme`, `test_an_event_is_compacted_for_the_model_and_keeps_what_meet_reads`, `test_every_calendar_call_that_returns_events_compacts_them_with_links`, `test_all_day_events_use_dates`, `test_drive_search_quotes_plain_words_and_passes_a_drive_query_through`, `test_drive_reads_a_doc_by_export_and_says_when_it_cannot`, `test_drive_stops_reading_a_file_at_the_download_cap`, `test_drive_download_still_reports_the_http_error`, `test_suggest_time_keeps_an_explicit_midnight_workday_start`, `test_a_google_error_is_a_tool_error_not_an_exception`, `test_the_rest_tools_carry_the_mcp_servers_names`, `test_times_without_an_offset_get_the_macs`, `test_free_slots_skip_busy_time_and_keep_to_working_hours`, `test_suggest_time_reads_freebusy_over_the_owners_calendars`, `test_list_events_reads_every_calendar_the_owner_sees_in_one_call`, `test_list_events_across_calendars_keeps_what_it_read_when_one_calendar_fails`, `test_responding_keeps_every_guest_and_changes_only_the_owner`, `test_guests_are_emailed_only_when_there_are_guests_or_it_is_asked`, `test_update_adds_guests_to_the_ones_already_there`, `test_a_recurring_event_carries_its_zone`, `test_attachments_are_listed_and_a_text_one_is_read`, `test_a_pdf_is_read_with_pdfkit_where_it_exists` | same names in `tests/test_google_rest.rs`, plus `test_list_events_fails_when_every_calendar_fails`, `test_drive_creates_a_file_as_one_multipart_upload`, `test_a_missing_argument_is_a_key_error_and_a_dropped_connection_propagates` | The `meet.event_link` / `meet.events_in` and `apps_mcp.tool_reads` assertions belong to the meet and apps-mcp crates (the compacted fields they read are asserted here). `test_suggest_time_keeps_an_explicit_midnight_workday_start` asserts the slots instead of monkeypatching `free_slots`. `test_an_email_question_is_the_whole_email_with_hidden_characters_shown` is apps_mcp's. |
| `bridge/tests/test_lean_calendaredits_replay.py` | `test_list_events_reads_the_primary_when_the_calendar_list_has_a_network_error`, `test_suggest_time_keeps_to_working_hours_across_the_end_of_daylight_time`, `test_suggest_time_keeps_to_the_owners_working_hours_when_asked_in_utc` | same names in `tests/test_google_rest.rs` | The `los_angeles` fixture (setting `TZ`) becomes `with_zone(Zone::Named(America/Los_Angeles))`. |
| `bridge/tests/test_routine_app_policy.py` | `test_routine_calendar_decision_matches_real_rest_delivery_parameters` (client half); the `prepare` tests' raw event read | `test_routine_calendar_decision_matches_real_rest_delivery_parameters`, `test_prepare_reads_the_full_event_with_no_field_mask` | The `direct(...)` / `AppsBridge.prepare` decisions belong to apps-mcp. |
| `bridge/tests/test_routine_confirmations.py` | `test_chat_prepares_private_calendar_edit_and_keeps_guest_edit_approval` | covered by the update_event delivery tests above | The chat/approval flow belongs to telegram and apps-mcp. |
| (parity measurement) | — | `tests/test_parity.rs`: `fromisoformat_matches_cpython`, `rfc3339_matches_python_in_los_angeles`, `html_unescape_matches_python`, `mail_text_matches_python`, `place_links_match_python`, `event_descriptions_match_python`, `addresses_match_python`, `build_mail_is_byte_for_byte_pythons`, `free_slots_match_python_in_los_angeles`, `local_zone_reads_the_macs_zone`; fixture written by `tests/fixtures/gen_parity.py` on the live CPython 3.13 | |
| (unit) | — | `src/py.rs`, `src/pytime.rs`, `src/mail.rs`, `src/lib.rs` `#[cfg(test)]` modules | |

## Boundaries
None. PDF text comes from PDFKit through `objc2-pdf-kit` (macOS), as the Python uses PDFKit through pyobjc.

## Differences
- `call_tool` is async and returns `Result`: the `Err` side is exactly what Python lets propagate out of `call_tool` (apps_mcp's `execute` turns it into `"<Type>: <message>"`).
- The token is a `TokenSource` (async), not a sync callable; any `Fn() -> Option<String>` closure still works.
- A nested field of Google's reply that has the wrong JSON type (a string where an object or list belongs, below the top level) is read as absent; Python raised AttributeError/TypeError out of `call_tool`. A top-level reply that is not an object (a text/HTML body) still raises `AttributeError: '<type>' object has no attribute 'get'`, as in Python.
- A JSON body that fails to parse is `JSONDecodeError: <serde's message>` (Python: `Expecting value: line 1 column 1 (char 0)`-style text).
- `fromisoformat` refuses a UTC offset with a fraction of a second (chrono offsets are whole seconds; CPython accepts `+05:30:15.5`).
- `int()` of a string reads ASCII digits only (CPython also reads other Unicode decimal digits); integers beyond i64 saturate.
- `repr()` of a string judges "printable" by a fixed list of non-printable ranges (controls, format characters, non-ASCII separators, private use, non-characters) rather than the full Unicode database.
- `build_mail`: an address that is not a plain dot-atom (comments, quoted local parts, domain literals) is written as one unfoldable word; Python's address parser would rebuild it. A subject that itself contains a literal RFC 2047 encoded word is not decoded and re-encoded as Python's header parser does.
- Request bodies are serde's compact JSON, which is what httpx 0.28 sends (`separators=(",", ":")`, `ensure_ascii=False`).
