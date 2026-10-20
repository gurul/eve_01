# chat-core
Status: complete

link_chat.py lines 1-735 (src/link_chat/mod.rs) and link_topics.py (src/link_topics.rs). Every todo!() body is filled;
the architect's constants, regexes, `reason`, `py_json` and `prompt_path` are unchanged.

## API notes
| Python | Rust | Notes |
|---|---|---|
| link_chat._prompt | link_chat::PROMPT (private) | the one cached reference: (path, mtime ns, size), text, warned |
| link_chat.controls_listing | link_chat::controls_listing | `get` returns `Option<Arc<dyn Hub>>`; `getattr(hub, "available", True)` is `Hub::available` |
| link_chat.app_block | link_chat::app_block | `path` None is PROMPT_PATH, the Python package's own openui_prompt.txt |
| link_chat._row | link_chat::row | `str(line["ts"])` and `str(text)` of a non-string value are Python's `str()` (eve_app_registry::py_repr) |
| link_chat.read_messages / read_before / split_cursor | same names | `ValueError("bad cursor")` / `("bad message id")` is `Err(BadCursor(..))` with the same words |
| link_chat._length | link_chat::length | Python's `int()`: whitespace, a sign, single underscores, any Unicode digit; past i64 saturates |
| link_chat.turn_id | link_chat::turn_id | `taken` is a predicate (Python's `in taken`); random ids are `secrets.token_hex(8)`'s 16 hex |
| link_chat.job_needs_you / job_actions / job_line / job_entry | same names | take only the card: `chief._doubtful(card) is not None` is `JobCard::doubtful`, `chief._as_data(card)` is `JobCard::data` |
| link_chat.list_jobs | link_chat::list_jobs | `hidden` is required (Python's default `()` is an empty set) |
| link_chat.HiddenJobs | link_chat::HiddenJobs | `ids` behind a mutex (`ids()` copies); `add` returns the write's io::Result |
| link_topics._call / _has_key / _save | Titler::call / has_key, write_private (private) | the call is OpenRouter's blocking client, priced with `spend.record_chat_completion(MEMORY, ..)` |
| link_topics._PROGRAM, _MARKS, _GREETING, _PLEASANTRY | PROGRAM, MARKS, GREETING, PLEASANTRY | architect's ports |

## Tests
| Python file | Python tests | Rust tests (tests/chat_core.rs) | Not ported (why) |
|---|---|---|---|
| test_link_topics.py | test_a_pause_longer_than_45_minutes_starts_a_new_topic_and_exactly_45_does_not, test_eves_own_lines_alone_are_not_a_topic, test_a_hello_is_not_a_title_the_next_message_is, test_friendly_small_talk_is_small_talk, test_a_title_says_something, test_only_topics_with_substance_are_important, test_titles_are_plain_and_clipped_at_a_word, test_bookmarks_are_private_survive_and_unset, test_model_titles_are_cached_by_count_and_small_talk_hides_a_card, test_a_failing_model_keeps_the_first_message_title, test_titles_off_never_ask, test_a_list_titles_in_the_background_without_racing_the_reply, test_older_pages_reach_the_first_day_in_order | same names | |
| test_link_topics.py | test_routes_list_save_and_refuse | the_view_lists_important_topics_newest_first_and_the_saved_ones (the view, `known` and the bookmark below the door) | the HTTP statuses (400/401/403/404/405) are the chat-door unit's (tests/chat_door.rs) |
| test_link_topics.py | test_the_older_route_answers_a_page_and_refuses_a_bad_id | an_older_page_and_a_bad_message_id (read_before's page and refusals) | the route's JSON and 400 are the chat-door unit's |
| test_link_chat.py | test_history_is_oldest_first_and_the_cursor_crosses_the_four_oclock_day, test_history_is_the_newest_hundred_lines_however_far_back, test_only_what_was_said_is_shown_and_forgotten_lines_are_left_out, test_claude_on_shows_both_sides_of_the_relay_in_the_app, test_a_line_without_via_still_loads_and_via_is_written_only_when_given, test_openui_programs_are_one_plain_line_outside_the_app, test_history_text_per_channel, test_no_guide_file_means_no_addition_logged_once, test_days_cross_at_four_even_with_a_clock_near_midnight, test_the_job_lead_names_no_act, test_a_bare_program_after_prose_is_stripped_like_the_app_draws_it, test_only_the_last_twenty_finished_cards_are_listed | same names | |
| test_link_chat.py | test_the_shipped_guide_is_package_data | the_shipped_guide_is_package_data | the pyproject package-data check is Python packaging; Rust reads the same file in place |
| test_link_chat.py | test_the_openui_guide_is_in_app_turns_only | the_openui_guide_is_rule_reference_line_in_that_order (app_block's text and order) | which turns carry the block needs the real TelegramInlet (the brain's crate) |
| test_link_chat.py | test_jobs_list_open_cards_first_needs_you_first_then_the_finished, test_a_finished_job_can_be_removed_from_work_for_good, test_an_act_that_may_have_happened_gets_no_go_only_the_question | same names, on a fake `seams::Chief` whose cards are in the states the real chief leaves them | the actions over HTTP and the real Chief's rules are the chat-door unit's and the chief's crate's |
| test_link_chat.py | test_controls_list_what_this_mac_has | same name, with fake hubs (MemoryPanel/NotesPanel availability as `Hub::available`) | the 405 is the chat-door unit's |
| (new) | | plain_drops_programs_and_a_clicked_context, bookmarks_read_what_python_wrote, a_saved_small_talk_topic_still_shows_and_newest_bookmark_comes_first, the_title_model_and_switch_come_from_the_environment, an_empty_store_starts_the_cursor_at_today, an_owner_line_shows_its_attachments_its_reaction_key_and_eves_sources, a_line_the_app_cannot_place_is_not_shown, att_record_keeps_id_name_and_kind, a_cursor_splits_into_its_position_and_reaction_version, a_turn_id_is_the_client_id_when_plain_and_free, the_content_length_is_read_as_python_reads_it, a_stream_chunk_is_an_openai_delta_framed_as_sse, each_card_line_and_action_follows_its_state | Python covers these through the door; here they are pinned at the function |

test_the_job_lead_names_no_act: this crate does not depend on eve-chief-card (Cargo.toml is architect-complete), so the
test reads task_router.CONSEQUENTIAL's pattern from eve-desktop-lanes' source and folds the matches as chief_card.verbs
does, with Python's positive control ("order it" names "order").

## Boundaries
None.

## Differences
- `HiddenJobs::new`: a `"hidden"` value that is a number, bool or null makes Python raise TypeError out of `__init__`;
  here it is logged as `TypeError` and every finished card shows. A string or an object is iterated as Python does
  (its characters, its keys).
- `Bookmarks::load`: an `"items"` value that is not an object makes Python raise AttributeError out of `load`; here it
  is no bookmarks.
- `app_block`: a guide file that is not UTF-8 makes Python raise UnicodeDecodeError; here it counts as absent (no
  addition, logged once).
- `Titler::title_pass`: a cache that cannot be written raises out of Python's pass (the thread's exception hook logs
  it); here it is logged and the count of titles made is still returned. `Titler::kick`: a thread that cannot be
  spawned is false and logged (Python raises).
- The log line for a failed title names the error as Python's class name where the kind is known (`RuntimeError`,
  `JSONDecodeError`, `APIStatusError`, `APITimeoutError`, `APIConnectionError`, an OSError's name), else
  `Exception`; a test's `ask` error is `Exception`.
- `go_ready`: a phase approval of JSON null counts as no approval (Python's `None`).
