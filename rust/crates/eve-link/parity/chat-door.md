# chat-door
Status: complete

link_chat.py lines 737-1677 (class LinkChat, in src/link_chat/door.rs) and link_today.py (src/link_today.rs). Every
todo!() body is filled; the architect's LinkChatOptions, TodayOptions and constants are unchanged.

## Modules
| Python | Rust |
|---|---|
| link_chat.LinkChat (lines 737-1677) | link_chat::door::LinkChat (re-exported as link_chat::LinkChat) |
| link_chat._Turn | door::Turn (private): its state behind a mutex, `event` is a `watch` channel bumped by `poke` |
| link_today | link_today |

## API
| Python | Rust | Notes |
|---|---|---|
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

## Tests
| Python file | Python tests | Rust tests (tests/chat_door.rs) | Not ported (why) |
|---|---|---|---|
| test_link_chat.py | only_the_app_with_the_link_token_gets_in, a_wrong_scheme_an_unpaired_phone_and_no_link_door_are_refused, no_chat_running_is_a_503_with_a_reason, a_turn_is_heard_once_as_typed_and_streams_each_line_then_done, the_same_client_id_is_not_heard_again_and_gets_the_first_reply, a_turn_that_says_nothing_is_an_empty_completion, a_quiet_stream_is_kept_alive_and_ends_at_its_limit, the_limits_on_a_turn, wrong_methods_and_paths, a_long_poll_also_sees_a_write_from_another_process, busy_is_a_turn_in_flight_and_memory_off_is_a_503, a_long_poll_returns_when_busy_clears_not_at_its_timeout, an_app_turns_line_nobody_reads_goes_to_telegram, closing_the_server_ends_an_open_stream_with_done, the_grade_reaches_the_inlet_and_auto_is_the_default, every_new_path_needs_the_link_token_and_no_origin, jobs_list_open_cards_first_needs_you_first_then_the_finished, a_finished_job_can_be_removed_from_work_for_good, each_action_is_the_chiefs_own_method, an_act_that_may_have_happened_gets_no_go_only_the_question, a_new_job_is_heard_as_an_app_turn_and_answers_the_card_it_filed, a_new_job_the_brain_does_not_file_says_so_and_files_nothing, a_new_job_retried_with_its_client_id_is_heard_once, a_new_job_answers_the_card_its_own_turn_filed_not_a_newer_one, a_new_job_whose_turn_filed_nothing_is_not_handed_another_turns_card, a_new_job_still_running_at_its_limit_says_so, the_limits_on_a_new_job, a_new_job_needs_the_token_the_chief_and_the_chat, apps_are_listed_and_open_with_a_fresh_app_token, controls_need_the_link_token_and_no_origin, controls_list_what_this_mac_has, an_unknown_control_or_a_missing_device_is_a_404, a_control_is_the_cards_own_panel_body_in_json_out, updates_need_the_link_token_and_no_origin, updates_serve_the_contract_dismiss_and_refresh_without_waiting, updates_off_answer_empty_and_read_nothing, the_phone_location_needs_the_link_token_and_no_origin, a_body_that_is_not_a_fix_is_a_400_and_keeps_nothing, a_body_too_big_is_a_413, an_older_fix_is_ignored_and_the_log_never_says_where, the_switch_off_is_a_403_and_keeps_nothing, a_fix_that_cannot_be_kept_is_a_500, the_location_route_needs_no_chat_running, visits_and_fixes_build_the_history_and_the_log_never_says_where, places_off_is_a_403_and_keeps_nothing | same names | |
| test_link_chat.py | a_long_poll_wakes_on_a_new_line_and_times_out_empty_without_leaking | a_long_poll_wakes_on_a_new_line_and_times_out_empty | the listener-count assertion: the Transcripts listener list is not inspectable from outside eve-transcripts; the test instead checks every append still wakes a later poll |
| test_link_chat.py | a_question_an_app_turn_asks_is_answered_by_the_next_text_from_the_app | a_question_ends_the_stream_and_the_next_text_from_the_app_carries_the_rest | the real TelegramInlet's question routing is the brain's crate; here a fake inlet asks and answers |
| test_link_chat.py | a_go_from_the_app_keeps_the_chiefs_rules | a_go_from_the_app_carries_the_rev_the_app_showed | the real Chief's go rules are eve-chief's; the door's part (the rev, its 400s, the call) is ported |
| test_link_chat.py | an_app_turn_is_answered_in_the_app_not_in_telegram_and_marked_via_app, a_question_asked_in_telegram_is_answered_from_the_app, a_new_request_typed_in_the_app_while_a_go_waits_is_a_turn_not_dropped, an_app_reply_nobody_reads_reaches_telegram_and_history_without_its_program, an_app_follow_up_sees_the_options_its_last_block_offered, a_telegram_turn_after_an_app_block_still_reads_the_placeholder, recent_messages_reads_blocks_the_way_the_turn_does, each_grade_sets_the_turns_model_and_effort_and_instant_offers_no_think_hard, a_think_hard_call_on_instant_is_refused_and_on_auto_it_runs, quick_answers_still_come_first_in_every_grade, the_app_think_settings_come_from_the_environment, a_new_job_through_the_real_chat_is_the_brains_take_on, an_app_turn_offers_and_answers_the_phone_location_from_the_fix_it_posted | | need the real TelegramInlet (its brain, model calls, transcript writes); the door side is covered by the fake-inlet tests above |
| test_link_chat.py | history_*, only_what_was_said_*, claude_on_*, a_line_without_via_*, openui_programs_*, history_text_per_channel, the_openui_guide_*, no_guide_file_*, the_shipped_guide_*, days_cross_at_four_*, only_the_last_twenty_finished_cards_are_listed, the_job_lead_names_no_act, a_bare_program_after_prose_* | | the chat-core unit's (tests/chat_core.rs) |
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

The tests serve the door on a loopback port through a stand-in for miniapp.MiniAppServer's `/api/link/` dispatch (it
reads the head as miniapp.py does) and drive it with a raw HTTP/1.1 client; no test changes the process environment
(the location and places files come through LinkChatOptions.environ, the upload expiry through the store's clock).

## Boundaries
None.

## Differences
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
