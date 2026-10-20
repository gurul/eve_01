# eve-link
Status: complete

Eve Link's daemon half: a port of five Python modules (3889 lines): `link_chat.py`, `link_today.py`, `link_topics.py`,
`updates.py` and `updates_composio.py`. The routes, statuses, JSON bodies, files (`~/.config/eve-bridge/chat/
bookmarks.json`, `topic-titles.json`, `updates.json`, `hidden-jobs.json`), environment switches (`EVE_TOPIC_TITLES`,
`EVE_TOPIC_TITLE_MODEL`, `EVE_UPDATES`, `EVE_UPDATES_EVERY_MIN`, `EVE_UPDATES_MODEL`, `EVE_UPDATES_COMPOSIO`), model
ids and prompts are Python's. Every string constant of 25 characters or more in the five modules (prompts, error lines, the OpenUI guide
note) was checked present in the Rust sources; the only ones not found verbatim are regexes (Rust raw-string
escapes, `$` written as `\n?\z`) and log formats (`%s` written as `{}`).

What Python reached by duck typing on another module's object is a trait in `src/seams.rs`: the brain implements
`Inlet` (and `Chief` for the chief, `MiniApp`/`Hub` for the Mini App server and its cards); `DirectApps`,
`ComposioRead` and `WorkDoor` are implemented there for `AppsBridge`, `ComposioBridge` and `WorkSessions`. Python's
tests that need the real TelegramInlet, Chief or MiniAppServer run here against fakes of those traits; what only the
real objects can show is listed as not ported, with the crate that will test it.

`src/lib.rs` holds the shared helpers: `Env`/`env_get` (Python's `environ` parameter or `os.environ`), `expanduser`,
`monotonic`, `Mono`, `Now` and `local_now`.

## Modules
| Python | Rust |
|---|---|
| link_chat.py | src/link_chat/mod.rs, src/link_chat/door.rs |
| link_today.py | src/link_today.rs |
| link_topics.py | src/link_topics.rs |
| updates.py | src/updates.rs |
| updates_composio.py | src/updates_composio.rs |

## API
| Python | Rust | Notes |
|---|---|---|
| link_chat.controls_listing | link_chat::controls_listing |  |
| link_chat.app_block | link_chat::app_block |  |
| link_chat.strip_openui | link_chat::strip_openui |  |
| link_chat.history_text | link_chat::history_text |  |
| link_chat._row | link_chat::row |  |
| link_chat.att_record | link_chat::att_record |  |
| link_chat._att | link_chat::att |  |
| link_chat.read_messages | link_chat::read_messages |  |
| link_chat.read_before | link_chat::read_before |  |
| link_chat.split_cursor | link_chat::split_cursor |  |
| link_chat._length | link_chat::length |  |
| link_chat.turn_id | link_chat::turn_id |  |
| link_chat._Turn | link_chat::door::Turn (crate-private) |  |
| link_chat.sse | link_chat::sse |  |
| link_chat._chunk | link_chat::chunk |  |
| link_chat._go_ready | link_chat::go_ready |  |
| link_chat.job_needs_you | link_chat::job_needs_you |  |
| link_chat.job_actions | link_chat::job_actions |  |
| link_chat.job_line | link_chat::job_line |  |
| link_chat.job_entry | link_chat::job_entry |  |
| link_chat.list_jobs | link_chat::list_jobs |  |
| link_chat.HiddenJobs | link_chat::HiddenJobs |  |
| link_chat.LinkChat | link_chat::LinkChat (door.rs) | private methods are door.rs private fns |
| link_today._clip | link_today::clip |  |
| link_today.jobs_part | link_today::jobs_part |  |
| link_today.watches_part | link_today::watches_part |  |
| link_today._moment | link_today::moment |  |
| link_today.events_part | link_today::events_part |  |
| link_today.music_part | link_today::music_part |  |
| link_today.tv_part | link_today::tv_part |  |
| link_today.Today | link_today::Today |  |
| link_topics.plain | link_topics::plain |  |
| link_topics.clip | link_topics::clip |  |
| link_topics.small_talk | link_topics::small_talk |  |
| link_topics.segment | link_topics::segment |  |
| link_topics.read_rows | link_topics::read_rows |  |
| link_topics.Bookmarks | link_topics::Bookmarks |  |
| link_topics.Titler | link_topics::Titler |  |
| link_topics.view | link_topics::view |  |
| link_topics.known | link_topics::known |  |
| updates.enabled | updates::enabled |  |
| updates.every_secs | updates::every_secs |  |
| updates._one_line | updates::one_line, one_line_str |  |
| updates._clip | updates::clip |  |
| updates._clip_body | updates::clip_body |  |
| updates.local_zone | updates::local_zone |  |
| updates.item | updates::item |  |
| updates._structured | updates::structured |  |
| updates.mail_items | updates::mail_items |  |
| updates._owners | updates::owners |  |
| updates._thread_text | updates::thread_text |  |
| updates._mail_date | updates::mail_date |  |
| updates.calendar_items | updates::calendar_items |  |
| updates.event_item | updates::event_item |  |
| updates._open_todos | updates::open_todos |  |
| updates.note_items | updates::note_items |  |
| updates.journal_open | updates::journal_open |  |
| updates.memory_items | updates::memory_items |  |
| updates.chief_items | updates::chief_items |  |
| updates.drop_hidden | updates::drop_hidden |  |
| updates.Sources | updates::Sources, UpdateSources |  |
| updates._records_dir | updates::default_records_dir |  |
| updates._hidden_match | updates::default_hidden_match |  |
| updates.gather | updates::gather |  |
| updates.here | updates::here |  |
| updates.SuggestError | updates::SuggestError |  |
| updates.prompt_input | updates::prompt_input |  |
| updates.card_id | updates::card_id |  |
| updates._verb | updates::verb_of |  |
| updates._text | updates::text_of |  |
| updates._due | updates::due_of |  |
| updates.carries_payload | updates::carries_payload |  |
| updates.ref_kind | updates::ref_kind |  |
| updates.clean_cards | updates::clean_cards |  |
| updates.suggest | updates::suggest |  |
| updates.reply | updates::reply |  |
| updates.Client | updates::Client, SuggestClient |  |
| updates.make_client | updates::make_client |  |
| updates.UpdatesStore | updates::UpdatesStore, StoreState |  |
| updates.live_dismissed | updates::live_dismissed |  |
| updates.served | updates::served |  |
| updates.lost_kinds | updates::lost_kinds |  |
| updates.merge_cards | updates::merge_cards |  |
| updates.card_hidden | updates::card_hidden |  |
| updates._parse_at | updates::parse_at |  |
| updates.Updates | updates::Updates, UpdatesOptions |  |
| updates_composio.SourceOff | updates_composio::source_off (updates::ReadError::Off) |  |
| updates_composio.ReadFailed | updates_composio::read_failed (updates::ReadError::Failed) |  |
| updates_composio.enabled | updates_composio::enabled |  |
| updates_composio.usable | updates_composio::usable |  |
| updates_composio.safe_link | updates_composio::safe_link |  |
| updates_composio._linked | updates_composio::linked |  |
| updates_composio.alias_word | updates_composio::alias_word |  |
| updates_composio.Reader | updates_composio::Reader |  |
| updates_composio._iso | updates_composio::iso |  |
| updates_composio._parse | updates_composio::parse |  |
| updates_composio._day | updates_composio::day |  |
| updates_composio.slack_text | updates_composio::slack_text |  |
| updates_composio._matches | updates_composio::matches |  |
| updates_composio._slack_ok | updates_composio::slack_ok |  |
| updates_composio.slack_items | updates_composio::slack_items |  |
| updates_composio._plain | updates_composio::plain |  |
| updates_composio._mentions | updates_composio::mentions |  |
| updates_composio.notion_page | updates_composio::notion_page |  |
| updates_composio.notion_blocks | updates_composio::notion_blocks |  |
| updates_composio.notion_items | updates_composio::notion_items |  |
| updates_composio.linear_items | updates_composio::linear_items |  |
| updates_composio._labels | updates_composio::labels |  |
| updates_composio.work_email_items | updates_composio::work_email_items |  |
| updates_composio.work_calendar_items | updates_composio::work_calendar_items |  |
| updates_composio.reads | updates_composio::reads |  |

Further rows (helpers, seams and Python privates the port names differently):

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
| LinkChat.__init__ | LinkChat::new(inlet, LinkChatOptions) | keyword arguments are LinkChatOptions fields; each store left None is made as Python makes it (Today(), Updates(inlet), Bookmarks(), Titler(), UploadStore(), HiddenJobs()) |
| LinkChat.today / updates / bookmarks / titler / uploads / hidden | same names, accessors | |
| LinkChat._attach | attach (private) | app_out and link_uploads are set on every request (the seam has no getter to compare); the transcript listener is added once per store (Arc::ptr_eq), the reactions listener when the store has none |
| LinkChat._on_append / _wake | the listener closure in attach / Shared::wake | `_appended.set()` and the reaction wake are `watch` bumps |
| LinkChat.close | close | aborts the drivers, marks every turn done and pokes it |
| LinkChat.settle | settle(timeout) | waits until no stream is open or the timeout passes |
| LinkChat.deliver | deliver, and app_out() (the AppOut closure handed to the inlet, holding a Weak) | |
| LinkChat._newest_open / _prune | newest_open / prune (private) | |
| LinkChat.serve | serve(method, path, query, headers, reader, writer, token_check, apps) | `reader`/`writer` are `seams::DynRead`/`DynWrite`; `apps` is `Option<Arc<dyn MiniApp>>`; the routing order, statuses and every JSON body are Python's |
| LinkChat._turn / _start / _drive / _stream | turn / register+start / drive / stream (private) | a stream's hang-up is watched by a 1-byte read, as Python's `reader.read(1)` |
| LinkChat._topics / _messages / _older / _thumbs | topics / messages / older / thumbs (private) | the long poll subscribes before it reads, so no append is missed |
| LinkChat._upload / _file | upload / file (private) | |
| LinkChat._jobs / _new_job / _await_card / _optional_body | jobs / new_job / await_card / optional_body (private) | |
| LinkChat._updates / _location / _places / _sessions / _apps / _controls / _muse | updates_route / location / places / sessions / apps / controls / muse (private) | |
| LinkChat._busy / _discard / _json | busy / discard / write_json (pub) | write_json is Python's json.dumps (link_chat::py_json) with the same head |
| link_today._clip | link_today::clip | |
| link_today.jobs_part / watches_part / events_part / music_part / tv_part | same names | jobs_part takes `&dyn Chief`; watches_part takes the watches (the Inlet seam hands the list, not the watcher) |
| link_today._moment | link_today::moment | |
| link_today.Today | link_today::Today, Today::new(TodayOptions) | `_calendar_raw` / `_read_calendar` are calendar_raw / read_calendar (a spawned task behind a Shared future: a read that outlasts its request still finishes and is kept); gather(inlet, server) |
| updates.chief_items (the `list_jobs` answer it reads) | updates::chief_items_from | the seam Python's test patched (`monkeypatch.setattr(link_chat, "list_jobs", ...)`); `chief_items(chief)` is `chief_items_from(&link_chat::list_jobs(chief, &{}))` |
| `datetime.isoformat()` / `isoformat(timespec="seconds")` | updates::isoformat(dt, seconds) | the snapshot's `now`, `generated_at`, the calendar window, a chief deadline |
| `email.utils.parsedate_to_datetime` (email._parseaddr._parsedate_tz) | private `parsedate_tz`, used by updates::mail_date | ported line for line: day names, RFC 850 dates, the swapped day/month and year/time, 2-digit years, the named zones, `-0000` as no zone |
| `UpdatesStore`'s file JSON | updates::StoreState::to_value | keys in Python's order, written with `json.dumps(ensure_ascii=False)` separators |
| `await u._task` (tests) | Updates::join_refresh | waits for the rebuild running now |
| `u._next_try`, `u._lost_runs`, `u._retry_soon`, `Updates._last_full_is_old` | Updates::next_try / set_next_try, lost_runs / set_lost_runs, retry_soon, last_full_is_old | public so the integration tests can reach them, as Python's tests reach the underscored names |
| updates_composio.READERS | updates_composio::READERS (the kinds) + updates_composio::read_kind | Python's dict of functions becomes its key set plus one dispatcher. `read_kind("notion", …)` reads with no deadline, as `READERS["notion"](reader, now)` did. |
| updates_composio.Reader.accounts / accounts_for / call | Reader::accounts / accounts_for / call (async) | The listing is held under a `tokio::sync::Mutex` across the call, so a second source waits for the first one's answer, as Python's `threading.Lock` made it. A failed listing is cached by its type name (`ComposioError::type_name`). |
| (monkeypatch of READ_SLUGS) | Reader::with_allowed | The test seam for the allow-list. `Reader::new` uses READ_SLUGS. |
| updates_composio._iso | updates_composio::iso, updates_composio::try_iso | `try_iso` raises Python's errors for a time that no datetime can hold. The sources call `try_iso`. |
| updates_composio.notion_page / notion_blocks | notion_page / notion_blocks, plus try_notion_page / try_notion_blocks | The `try_` forms raise Python's TypeError when a `people` or `results` value is a number or true. `notion_items` calls them. |
| urllib.parse.urlsplit (inside safe_link) | private `urlsplit` | Ported as Python 3.13 parses printable ASCII: scheme detection, the netloc up to `/?#`, the bracket and IPvFuture checks, and `_hostinfo`'s hostname and port. |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| test_link_topics.py | test_a_pause_longer_than_45_minutes_starts_a_new_topic_and_exactly_45_does_not, test_eves_own_lines_alone_are_not_a_topic, test_a_hello_is_not_a_title_the_next_message_is, test_friendly_small_talk_is_small_talk, test_a_title_says_something, test_only_topics_with_substance_are_important, test_titles_are_plain_and_clipped_at_a_word, test_bookmarks_are_private_survive_and_unset, test_model_titles_are_cached_by_count_and_small_talk_hides_a_card, test_a_failing_model_keeps_the_first_message_title, test_titles_off_never_ask, test_a_list_titles_in_the_background_without_racing_the_reply, test_older_pages_reach_the_first_day_in_order | same names | |
| test_link_topics.py | test_routes_list_save_and_refuse | the_view_lists_important_topics_newest_first_and_the_saved_ones (the view, `known` and the bookmark below the door) | the HTTP statuses (400/401/403/404/405) are in tests/chat_door.rs (the_topic_routes_list_save_and_refuse) |
| test_link_topics.py | test_the_older_route_answers_a_page_and_refuses_a_bad_id | an_older_page_and_a_bad_message_id (read_before's page and refusals) | the route's JSON and 400 are in tests/chat_door.rs (the_older_route_answers_a_page_and_refuses_a_bad_id) |
| test_link_chat.py | test_history_is_oldest_first_and_the_cursor_crosses_the_four_oclock_day, test_history_is_the_newest_hundred_lines_however_far_back, test_only_what_was_said_is_shown_and_forgotten_lines_are_left_out, test_claude_on_shows_both_sides_of_the_relay_in_the_app, test_a_line_without_via_still_loads_and_via_is_written_only_when_given, test_openui_programs_are_one_plain_line_outside_the_app, test_history_text_per_channel, test_no_guide_file_means_no_addition_logged_once, test_days_cross_at_four_even_with_a_clock_near_midnight, test_the_job_lead_names_no_act, test_a_bare_program_after_prose_is_stripped_like_the_app_draws_it, test_only_the_last_twenty_finished_cards_are_listed | same names | |
| test_link_chat.py | test_the_shipped_guide_is_package_data | the_shipped_guide_is_package_data | the pyproject package-data check is Python packaging; Rust reads the same file in place |
| test_link_chat.py | test_the_openui_guide_is_in_app_turns_only | the_openui_guide_is_rule_reference_line_in_that_order (app_block's text and order) | which turns carry the block needs the real TelegramInlet (the brain's crate) |
| test_link_chat.py | test_jobs_list_open_cards_first_needs_you_first_then_the_finished, test_a_finished_job_can_be_removed_from_work_for_good, test_an_act_that_may_have_happened_gets_no_go_only_the_question | same names, on a fake `seams::Chief` whose cards are in the states the real chief leaves them | the actions over HTTP are in tests/chat_door.rs; the real Chief's rules are eve-chief's |
| test_link_chat.py | test_controls_list_what_this_mac_has | same name, with fake hubs (MemoryPanel/NotesPanel availability as `Hub::available`) | the 405 is in tests/chat_door.rs (controls_need_the_link_token_and_no_origin) |
| (new) | | plain_drops_programs_and_a_clicked_context, bookmarks_read_what_python_wrote, a_saved_small_talk_topic_still_shows_and_newest_bookmark_comes_first, the_title_model_and_switch_come_from_the_environment, an_empty_store_starts_the_cursor_at_today, an_owner_line_shows_its_attachments_its_reaction_key_and_eves_sources, a_line_the_app_cannot_place_is_not_shown, att_record_keeps_id_name_and_kind, a_cursor_splits_into_its_position_and_reaction_version, a_turn_id_is_the_client_id_when_plain_and_free, the_content_length_is_read_as_python_reads_it, a_stream_chunk_is_an_openai_delta_framed_as_sse, each_card_line_and_action_follows_its_state | Python covers these through the door; here they are pinned at the function |
| test_link_chat.py | only_the_app_with_the_link_token_gets_in, a_wrong_scheme_an_unpaired_phone_and_no_link_door_are_refused, no_chat_running_is_a_503_with_a_reason, a_turn_is_heard_once_as_typed_and_streams_each_line_then_done, the_same_client_id_is_not_heard_again_and_gets_the_first_reply, a_turn_that_says_nothing_is_an_empty_completion, a_quiet_stream_is_kept_alive_and_ends_at_its_limit, the_limits_on_a_turn, wrong_methods_and_paths, a_long_poll_also_sees_a_write_from_another_process, busy_is_a_turn_in_flight_and_memory_off_is_a_503, a_long_poll_returns_when_busy_clears_not_at_its_timeout, an_app_turns_line_nobody_reads_goes_to_telegram, closing_the_server_ends_an_open_stream_with_done, the_grade_reaches_the_inlet_and_auto_is_the_default, every_new_path_needs_the_link_token_and_no_origin, jobs_list_open_cards_first_needs_you_first_then_the_finished, a_finished_job_can_be_removed_from_work_for_good, each_action_is_the_chiefs_own_method, an_act_that_may_have_happened_gets_no_go_only_the_question, a_new_job_is_heard_as_an_app_turn_and_answers_the_card_it_filed, a_new_job_the_brain_does_not_file_says_so_and_files_nothing, a_new_job_retried_with_its_client_id_is_heard_once, a_new_job_answers_the_card_its_own_turn_filed_not_a_newer_one, a_new_job_whose_turn_filed_nothing_is_not_handed_another_turns_card, a_new_job_still_running_at_its_limit_says_so, the_limits_on_a_new_job, a_new_job_needs_the_token_the_chief_and_the_chat, apps_are_listed_and_open_with_a_fresh_app_token, controls_need_the_link_token_and_no_origin, controls_list_what_this_mac_has, an_unknown_control_or_a_missing_device_is_a_404, a_control_is_the_cards_own_panel_body_in_json_out, updates_need_the_link_token_and_no_origin, updates_serve_the_contract_dismiss_and_refresh_without_waiting, updates_off_answer_empty_and_read_nothing, the_phone_location_needs_the_link_token_and_no_origin, a_body_that_is_not_a_fix_is_a_400_and_keeps_nothing, a_body_too_big_is_a_413, an_older_fix_is_ignored_and_the_log_never_says_where, the_switch_off_is_a_403_and_keeps_nothing, a_fix_that_cannot_be_kept_is_a_500, the_location_route_needs_no_chat_running, visits_and_fixes_build_the_history_and_the_log_never_says_where, places_off_is_a_403_and_keeps_nothing | same names | |
| test_link_chat.py | a_long_poll_wakes_on_a_new_line_and_times_out_empty_without_leaking | a_long_poll_wakes_on_a_new_line_and_times_out_empty | the listener-count assertion: the Transcripts listener list is not inspectable from outside eve-transcripts; the test instead checks every append still wakes a later poll |
| test_link_chat.py | a_question_an_app_turn_asks_is_answered_by_the_next_text_from_the_app | a_question_ends_the_stream_and_the_next_text_from_the_app_carries_the_rest | the real TelegramInlet's question routing is the brain's crate; here a fake inlet asks and answers |
| test_link_chat.py | a_go_from_the_app_keeps_the_chiefs_rules | a_go_from_the_app_carries_the_rev_the_app_showed | the real Chief's go rules are eve-chief's; the door's part (the rev, its 400s, the call) is ported |
| test_link_chat.py | an_app_turn_is_answered_in_the_app_not_in_telegram_and_marked_via_app, a_question_asked_in_telegram_is_answered_from_the_app, a_new_request_typed_in_the_app_while_a_go_waits_is_a_turn_not_dropped, an_app_reply_nobody_reads_reaches_telegram_and_history_without_its_program, an_app_follow_up_sees_the_options_its_last_block_offered, a_telegram_turn_after_an_app_block_still_reads_the_placeholder, recent_messages_reads_blocks_the_way_the_turn_does, each_grade_sets_the_turns_model_and_effort_and_instant_offers_no_think_hard, a_think_hard_call_on_instant_is_refused_and_on_auto_it_runs, quick_answers_still_come_first_in_every_grade, the_app_think_settings_come_from_the_environment, a_new_job_through_the_real_chat_is_the_brains_take_on, an_app_turn_offers_and_answers_the_phone_location_from_the_fix_it_posted | | need the real TelegramInlet (its brain, model calls, transcript writes); the door side is covered by the fake-inlet tests above |
| test_link_chat.py | history_*, only_what_was_said_*, claude_on_*, a_line_without_via_*, openui_programs_*, history_text_per_channel, the_openui_guide_*, no_guide_file_*, the_shipped_guide_*, days_cross_at_four_*, only_the_last_twenty_finished_cards_are_listed, the_job_lead_names_no_act, a_bare_program_after_prose_* | same names, in tests/chat_core.rs (listed above) | |
| test_link_today.py | all nine | same names, plus clip_is_one_line_with_an_ellipsis | |
| test_link_reactions.py | a_reaction_wakes_the_long_poll_and_an_old_version_answers_at_once | same name | the rest are eve-link-reactions' store or need the real TelegramInlet's receipts |
| test_link_uploads.py | upload_needs_the_link_token_and_no_origin, upload_size_cap_and_type_refusal, a_turn_names_only_known_live_ids, attachments_reach_the_inlet_and_an_empty_text_is_fine_with_one, photos_and_pdfs_over_the_inline_cap_together_are_refused_before_the_brain, an_expired_attachment_shows_as_gone_in_the_history | same names, plus upload_with_chunked_encoding_is_refused | the store's own tests are eve-link-uploads'; an_image_turn_reaches_the_brain_*, a_pdf_and_a_text_file_*, with_the_claude_relay_on_*, an_attachment_is_not_taken_as_the_answer_*, an_owner_lines_attachment_note_* need the real TelegramInlet; a_bad_att_on_a_transcript_line_is_left_out is chat-core's row |
| test_app_media.py | the_link_door_lends_its_store_to_the_inlet, the_file_door_needs_the_token_and_serves_live_ids_only, a_kept_file_is_also_an_owner_upload_s_door, the_file_door_serves_only_a_photo_or_pdf_inline_and_never_an_active_type | same names | the rest are the TelegramInlet's media sends or eve-link-uploads' store |
| test_obsbot_panel.py, test_pika.py | it_is_listed_and_its_panel_is_behind_the_link_token, the_control_and_the_video_need_the_link_token, pika_is_listed_only_when_it_is_on | a_picture_stream_is_the_cards_own_and_behind_the_link_token, a_control_is_the_cards_own_panel_body_in_json_out, an_unknown_control_or_a_missing_device_is_a_404 (fake hubs) | the gimbal, capture and car protocol are the device crates' |
| test_work_sessions.py | every_session_route_needs_the_link_token_and_no_origin, the_routes_shapes_wrong_methods_and_unknown_paths, work_off_is_a_503_on_every_route | same names, on a fake `seams::WorkDoor` | the rest are eve-work-sessions' |
| test_muse_pi.py | the_link_door_reads_and_flips_muse | same name | the rest are eve-muse-pi's |
| test_link_topics.py | test_routes_list_save_and_refuse, test_the_older_route_answers_a_page_and_refuses_a_bad_id | the_topic_routes_list_save_and_refuse, the_older_route_answers_a_page_and_refuses_a_bad_id | |
| test_link_grade.py, test_memory_sources.py | all | | the grader and the sources are the TelegramInlet's; the door only passes `grade` through (the_grade_reaches_the_inlet_and_auto_is_the_default) |
| (new) | | a_turn_that_cannot_start_ends_at_once, a_line_said_while_the_words_are_heard_goes_to_the_app, a_client_that_hangs_up_sends_the_rest_to_telegram, a_new_job_that_cannot_start_is_a_500, write_json_is_pythons_json_with_its_head, discard_reads_at_most_what_was_said_and_stops_at_eof, busy_is_in_flight_and_not_waiting_on_the_owner | paths Python covers only through the real inlet |
| bridge/tests/test_updates.py | 49 | 44 ported in tests/updates.rs under the same names, plus 7 of Rust's own (test_mail_date_reads_rfc_2822_as_python_does, test_mail_date_edge_cases_match_python, test_clip_cuts_at_a_word_past_the_middle, test_text_and_verb_follow_the_python_rules, test_merge_keeps_the_nearest_due_first_and_lost_kinds, test_the_store_writes_python_json, test_prompt_input_is_pythons_bytes; each expected value was taken from the Python module) | the 5 `test_eval_*` tests: they test bridge/tools/updates_eval.py, a Python tool outside the daemon that this crate does not port. test_chief_cards_open_ones_only checks the chief's items through `chief_items_from` (the seam Python patched); its gather half (`status["chief"] == "ok"`) is test_chief_cards_open_ones_only_through_gather (the last row of this table) |
| test_updates_composio.py | test_slack_keeps_unanswered_dms_and_mentions_one_per_conversation, test_notion_reads_open_todos_mentions_and_assigned_open_tasks, test_notion_reads_no_blocks_after_its_deadline, test_linear_keeps_open_issues_due_soon_or_recently_updated, test_work_email_reads_the_work_account_only_one_item_per_thread, test_work_calendar_reads_like_the_owners_calendar, test_no_other_account_means_the_work_kinds_are_off, test_links_are_kept_only_on_the_apps_own_https_hosts, test_every_composio_source_lands_in_gather_with_status_ok, test_the_model_never_reads_an_items_link, test_off_without_a_bridge_or_with_the_switch_off, test_a_bridge_that_cannot_read_is_off, test_a_toolkit_that_is_not_connected_is_off_and_the_rest_still_read, test_an_expired_connection_fails_and_is_lost, test_a_failing_call_fails_its_source_only, test_an_unreadable_listing_fails_every_composio_source, test_a_source_that_goes_off_after_a_full_build_is_lost, test_the_accounts_are_listed_once_per_gather, test_hidden_words_and_dismissed_refs_drop_composio_items, test_every_allowed_slug_only_reads, test_a_sending_slug_is_refused_before_it_reaches_composio, test_the_consequential_check_guards_even_an_allow_listed_slug, test_every_slug_the_sources_run_is_on_the_allow_list, test_clean_cards_accepts_every_composio_ref_and_keeps_the_link, test_a_card_citing_an_unknown_composio_ref_is_dropped, test_a_merged_card_keeps_the_link_of_its_first_ref_that_has_one, test_a_served_card_never_carries_a_link_off_its_apps_host, test_a_rebuild_keeps_a_lost_composio_sources_old_cards, test_the_prompt_names_the_kinds_as_one_positive_set, test_updates_knows_every_composio_kind_and_ref_prefix | the same names in tests/updates_composio.rs, plus test_safe_link_reads_the_url_as_urlsplit_does, test_small_readers, test_notion_page_reads_its_properties, test_reads_share_one_listing_and_are_off_without_a_usable_bridge, test_an_expired_only_connection_fails_and_a_missing_one_is_off | test_the_bridge_refuses_a_writing_slug_itself and test_connected_accounts_leaves_direct_apps_out test `composio_tools.ComposioBridge` itself. They are ported under the same names in eve-composio-tools (src/tests.rs), because they need the bridge's private client field. |
| test_updates.py | test_chief_cards_open_ones_only (its gather half) | test_chief_cards_open_ones_only_through_gather (tests/updates.rs): status["chief"] == "ok" and only the open card, through the real link_chat::list_jobs on a fake `seams::Chief`, with a no-chief positive control | |

Rust test files: tests/chat_core.rs (link_chat functions, link_topics), tests/chat_door.rs (the HTTP door,
link_today), tests/updates.rs, tests/updates_composio.rs.

tests/chat_core.rs: test_the_job_lead_names_no_act: this crate does not depend on eve-chief-card (Cargo.toml is architect-complete), so the
test reads task_router.CONSEQUENTIAL's pattern from eve-desktop-lanes' source and folds the matches as chief_card.verbs
does, with Python's positive control ("order it" names "order").

tests/chat_door.rs: The tests serve the door on a loopback port through a stand-in for miniapp.MiniAppServer's `/api/link/` dispatch (it
reads the head as miniapp.py does) and drive it with a raw HTTP/1.1 client; no test changes the process environment
(the location and places files come through LinkChatOptions.environ, the upload expiry through the store's clock).

tests/updates.rs: How the Python fixtures became Rust: `sources_for`'s Sources subclass is `TestSources`, an `UpdateSources` impl;
`FakeApps` implements `DirectApps` (async: `slow` sleeps on tokio's clock, and the `ValueError("boom")` subclass
becomes a panic inside the call, which the gather's own task contains and counts as failed); `FakeClient` implements
`SuggestClient`. The tests give every Updates and gather an environment with `EVE_PHONE_LOCATION=0`, and the `here()`
tests point `EVE_PHONE_LOCATION_FILE` at a temp file, so no test reads the Mac's real phone fix or memory.

tests/updates_composio.rs: The fake bridge answers from the shared fixture `bridge/tests/fixtures/updates/composio/responses.json`, which is
read with `include_str!`. The test sources stand in for Python's `Sources` subclass: no direct apps, no vault and no
chief. They read the bridge, the hidden words and the records folder through the real `updates::Sources`, so the
`EVE_UPDATES_COMPOSIO` switch and `usable` are the production checks.

In test_a_rebuild_keeps_a_lost_composio_sources_old_cards, Python's `u._next_try > 0` (a private field) becomes
`!u.due(later, true)`. The cache is 3 hours old, which is past the 2-hour default interval, so only the retry wait
that the lost build set can keep the rebuild from being due.

Parity was checked beyond the assertions. The five sources ran on the fixture in Python 3.13 (the bridge venv) and in
Rust. The items and the exact calls each one made (slug, arguments with their key order, account) were the same JSON,
key order included.

The gather, clean_cards, served and rebuild tests also run src/updates.rs; they pass in the integrated crate.

## Boundaries
None.

## Differences
### link_chat (lines 1-735) and link_topics
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

### link_chat.LinkChat and link_today
- Logs that Python writes with an exception's class name (`%s`, type(e).__name__) say `error` or `panic` here: a seam's
  anyhow error has no Python class.
- The upload size cap is the door's UploadStore's `max_bytes` (Python reads the module's MAX_BYTES, which tests
  monkeypatch); a store made with the default has the same cap.
- `inlet.watches()` is read in place: a watcher that panics is `"watches": "failed"` (Python: one that raises).
- A chief method that panics is the same 500 as one that raises; a hub panel that panics is the controls' 500.
- `wait=nan` on the long poll is no wait (Python carries a NaN deadline into asyncio).
- A New job's client_id is kept under the same lock that looked it up (Python's single loop made the look-up and
  the keep one step); a start that fails takes it back, so a retry is heard anew.
- The location and places routes read their switches and file paths from LinkChatOptions.environ when given, else
  the process environment (Python: os.environ).

### updates
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

### updates_composio
- `safe_link` with a malformed port (`:ab`, `:99999`): Python's `parts.port` raised ValueError outside the
  `try`, so one bad permalink failed the whole source. Here that URL gives no link, and the item is kept without one.
  The signature `Option<String>` cannot raise. An app's own permalinks never carry a port.
- `iso` (the public helper) returns "" for a time that no datetime can hold, where Python raised. Every source calls
  `try_iso`, which fails the source as Python did: ValueError for a year past 9999, OverflowError past time_t or
  when the local shift leaves the datetime range, OSError past what gmtime holds. The kinds are only logged.
- `notion_page` / `notion_blocks` (the public helpers) read a non-iterable `people` / `results` as empty, where
  Python raised TypeError. `notion_items` calls the `try_` forms and fails like Python.
- The label of a work account (`alias.strip().capitalize()`) title-cases the first character with a table of
  the Latin and Armenian characters whose title case differs from their upper case (ǅ, ǈ, ǋ, ǲ, ß → "Ss", the ﬀ–ﬆ
  ligatures, ŉ, և). Rare Greek characters with a ypogegrammeni use their upper case. An alias is a plain word such as
  "work", so this never shows.
