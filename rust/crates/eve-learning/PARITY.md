# eve-learning
Status: complete

Eve's local learning workspace: the lesson store, the tutor (live OpenRouter and the offline demo), TinyFish
practice references, think out loud, and the loopback HTTP server with its whiteboard page. Same files as Python:
`$EVE_LEARNING_DIR` (default `~/.config/eve-bridge/learning`, `demo/` under it for the standalone `--demo`),
`lessons.sqlite3` with the same three tables, the same JSON text in them (`json.dumps(ensure_ascii=False)`
bytes, so blob hashes and the 4096-character threshold agree) and `dashboard.json` (`json.dumps` default,
ASCII-escaped), written through `dashboard.tmp` and a rename. Same HTTP routes, status codes, headers (CSP with a
per-load nonce, `Cache-Control: no-store`, `nosniff`) and JSON bodies (Python's `json.dumps` bytes). The page,
fonts and tldraw bundle are the bytes of `bridge/src/eve_bridge/learning/web/`, embedded by `build.rs`. Prompts,
the reply schema, the model default (`gpt-6-astra` → `openai/gpt-6-astra`), timeouts (90 s) and every learner
sentence are verbatim.

Python reached the environment, `watch.http_request`, spend's ledger, `urllib.request.urlopen` and
`webbrowser.open` through globals; here they are seams: `eve_web::Ctx` (environment, TinyFish HTTP, ledger),
`tutor::TutorHttp` (the OpenRouter POST), `StartOptions::opener`, and `main_with`'s environ / start function /
stderr.

## Modules
| Python | Rust |
|---|---|
| learning/__init__.py | src/lib.rs |
| learning/__main__.py | src/main.rs (bin `eve-learning`) |
| learning/search.py | src/search.rs |
| learning/server.py | src/server.rs (+ build.rs embedding web/) |
| learning/store.py | src/store.rs |
| learning/think_aloud.py | src/think_aloud.rs |
| learning/tutor.py | src/tutor.rs |
| (Python built-ins: json.dumps, str/repr, truthiness, int()) | src/py.rs |

## API
| Python | Rust | Notes |
|---|---|---|
| learning.LESSON_ACTIONS, LESSON_MODES, TUTOR_ACTIONS | `LESSON_ACTIONS`, `LESSON_MODES`, `TUTOR_ACTIONS` | same order |
| learning.UNAVAILABLE, STALE_LESSON, ROBOT_APP_NOT_RUNNING | same names | verbatim |
| learning._LIMITS | `LIMITS` (private) | topic 200, level 100, text 20000 |
| learning.lesson_request | `lesson_request` → `LessonRequest` | `Err(String)` is the ValueError sentence; `LessonRequest::to_map` is Python's dict |
| learning.run_lesson | `run_lesson(Option<VoiceFn>, &Value)` | NotFound → STALE_LESSON, Value → reason, Type/Other propagate |
| (ValueError / KeyError / TypeError) | `LearningError::{Value, NotFound, Type, Other}` | |
| learning.__main__ | src/main.rs | `std::process::exit(main())` |
| search.RESULTS, TEXT_CHARS | `search::RESULTS`, `TEXT_CHARS` | 3, 3000 |
| search.search_problems | `search::search_problems(&Ctx, topic, level)` | `eve_web::web_reader::search_and_read`; spend line `tinyfish / search+fetch / lessons / 0.0 / "N pages, free"` |
| (urllib.parse.urlsplit) | `search::split_authority` | scheme, hostname (lowercased, brackets dropped), username, password |
| server.DEFAULT_PORT, MAX_BODY, MAX_BOARD, SPOKEN_KEEP, NONCE_SLOT | same names | 48766, 12 MiB, 4 MiB, 50, `__EVE_NONCE__` |
| server.ASSETS, FONTS, CANVAS | `server::asset(name)` | embedded table from build.rs (`fonts/…`, `canvas/…`, the three page files) |
| server.FONT_FILE, CANVAS_FILE | `FONT_FILE`, `CANVAS_FILE` (private regexes) | fullmatch semantics (anchored) |
| server.CANVAS_MIME | `server::canvas_mime(suffix)` | `Path(name).suffix` is `path_suffix` |
| server.content_policy | `server::content_policy(nonce)` | |
| server.CSP | `server::CSP` | |
| server.LISTEN_OFF | `server::listen_off()` | |
| server.validate_work | `server::validate_work(&Value)` | `base64.b64decode(validate=True)` is `b64decode_validate`; a non-dict `work` behaves as Python's membership test (`not_a_work_dict`) |
| server.mark_count | `server::mark_count` | |
| server.merge_spoken | `server::merge_spoken(ideas, lesson, base: f64)` | |
| server.LearningApp | `server::LearningApp` | `new(StartOptions)`; `dispatch`, `append_spoken`, `listen_status`, `listen`, `stop_listening`, `voice(&LessonRequest)`; `_ask_tutor` is `ask_tutor` (parking_lot `MutexGuard::unlocked`, the RLock released around the tutor), `event`, `_listen_result` / `_call_listener` private; `active()`, `url()` / `set_url`, `set_tutor` (Python's `app.tutor = ...`) |
| (LearningApp.listener duck type) | `server::Listener` trait | start / stop / status, each `anyhow::Result<Map>` |
| (notify callable) | `server::Notify` | errors are logged; the lesson is saved |
| webbrowser.open | `server::open_in_browser`, `StartOptions::opener` | |
| server.Handler | `server::Shared` (`get` = do_GET, `post_head` + `post_body` = do_POST, `allowed`), `Reply` (= reply), `handle` (axum fallback, each request on `spawn_blocking`) | |
| secrets.token_urlsafe | `server::token_urlsafe` | 16 bytes for the nonce, 32 for the page token |
| server.start | `server::start(StartOptions) -> io::Result<LearningServer>` | binds 127.0.0.1 (SO_REUSEADDR), port 0 picks one; axum on its own thread and runtime; `LearningServer { app, token, port }`, `shutdown` (stops accepting, gives open connections one second, never waits for a tutor call, as Python's daemon request threads), `server_close`, Drop shuts down |
| server.main | `server::main()`, `server::main_with(argv, environ, start_fn, stderr)` | clap for argparse; EADDRINUSE → the same message on stderr and exit 2; other errors propagate; Ctrl-C → 0 |
| store.BLOB_FIELDS, BLOB_MIN, SUMMARY_FIELDS | same names | |
| store.default_dir | `store::default_dir()`, `default_dir_from(Option<&str>)` | `Path("")` is "." |
| store.Store | `store::Store` | `new`, `connect` (busy timeout 15 s), `get`, `list`, `summary`, `save` (BEGIN IMMEDIATE), `create`, `revision_data` (= `_revision_data`), `history`, `delete`, `write_widget` |
| think_aloud.SILENCE_SECS, LISTEN_ACTIONS, LISTEN_STAGES, MAX_SPOKEN_IDEA | same names | |
| think_aloud._CONTEXT_LIMITS, _MAX_STEPS | `CONTEXT_*`, `MAX_STEPS` (private) | |
| think_aloud.VOICE_RULES, BACKEND_RULES | same names | verbatim, leading newline kept |
| think_aloud._DONE | `DONE` (private regex) | |
| think_aloud.lesson_context | `think_aloud::lesson_context(Option<&Map>)` | |
| think_aloud.voice_instructions | `think_aloud::voice_instructions` | |
| think_aloud.backend_instructions | `think_aloud::backend_instructions` | |
| think_aloud.said_done | `think_aloud::said_done` | |
| think_aloud.listen_refusal | `think_aloud::listen_refusal` | |
| tutor.PROMPT | `tutor::PROMPT` | verbatim |
| tutor.FIELDS | `tutor::fields()`, `FIELD_NAMES`, `STATUSES` | |
| tutor.validate | `tutor::validate(Value, action)` | |
| tutor.live_settings | `tutor::live_settings(&Ctx) -> LiveSettings` | `to_map()` keeps Python's key order |
| tutor.LiveTutor | `tutor::LiveTutor { ctx, http }` implementing `Tutor` | the POST is `TutorHttp` (`ReqwestPost`: blocking reqwest, rustls); `PostError::Status` is HTTPError, `Io` is OSError/TimeoutError |
| tutor.DemoTutor | `tutor::DemoTutor` implementing `Tutor` | |
| (a plain function as app.tutor) | `tutor::FnTutor` | |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| test_learning.py | test_learn_hints_steps_complete_recap_and_restart, test_help_requires_confirmation_and_ideas, test_stuck_is_a_valid_start_and_demo_does_not_fake_ocr, test_work_snapshots_conflicts_end_resume_and_delete, test_failed_tutor_leaves_saved_work_intact, test_invalid_work_rejected, test_text_boxes_are_strokes_that_round_trip, test_tldraw_board_round_trips_and_keeps_every_revision, test_invalid_board_rejected, test_marks_count_tldraw_shapes_and_strokes_from_before_tldraw, test_help_path_counts_a_new_mark_on_the_board_as_an_attempt, test_page_policy_is_same_origin_with_a_fresh_style_nonce_and_never_unsafe_inline, test_whiteboard_bundle_is_served_same_origin_and_traversal_is_rejected, test_tldraw_licence_key_reaches_the_page_only_when_set, test_live_requires_key_and_strict_reply, test_live_request_uses_current_images_and_a_strict_schema, test_http_origin_token_and_persistence, test_bundled_fonts_are_served_same_origin_and_traversal_is_rejected, test_voice_uses_same_lesson_state, test_provider_selection_and_astra_default, test_the_tutor_goes_to_openrouter_even_with_an_openai_key, test_an_openai_key_alone_asks_for_the_openrouter_key, test_invalid_provider_fails_without_guessing, test_openrouter_request_and_parser, test_openrouter_incomplete_response_is_not_accepted, test_openrouter_http_error_never_exposes_credentials, test_config_reports_openrouter_readiness_without_key, test_openrouter_terms_restriction_is_actionable_without_leaking_body, test_launcher_explains_busy_port_without_traceback, test_launcher_does_not_hide_other_startup_failures, test_an_unchanged_image_is_stored_once_across_revisions, test_the_widget_and_list_read_summaries_without_parsing_lessons, test_the_tutor_call_does_not_hold_the_lesson_lock, test_a_lesson_moved_on_during_the_tutor_call_is_refused | tests/learning.rs: the same names, plus notify_gets_each_action_but_not_saves_and_a_failure_keeps_the_lesson, lessons_keep_their_mode, a_python_written_lesson_reads_back, a_stored_lesson_without_a_revision_is_a_conflict_for_a_checked_save, the_real_post_answers_from_a_worker_thread_of_a_runtime, shutdown_does_not_wait_for_a_tutor_call_in_flight | test_invalid_work_rejected's `float('nan')` point: serde_json cannot hold NaN, so a non-number point stands in. test_the_widget_and_list_read_summaries_without_parsing_lessons cannot patch `json.loads`; it checks the rows and the widget file (the store's SQL does the extraction, as in Python). test_tldraw_licence_key changes the environment between two servers rather than two requests (tests never mutate the process environment). |
| test_learning_search.py | test_missing_key, test_search_failure, test_references_and_privacy, test_search_is_subject_neutral, test_demo_never_searches, test_env_file_supplies_the_tinyfish_key_to_standalone_launcher, test_daemon_cli_loads_env_before_subcommands | src/search.rs: test_missing_key, test_search_failure, test_search_is_subject_neutral, only_https_references_without_credentials_are_kept_and_the_search_is_metered, split_authority_reads_like_urlsplit; tests/learning_search.rs: test_references_and_privacy, test_demo_never_searches, test_env_file_supplies_the_tinyfish_key_to_standalone_launcher | test_daemon_cli_loads_env_before_subcommands tests `cli.main` (the eve-bridge CLI crate), not this crate. |
| test_learning_think_aloud.py | test_listen_and_stop_listening_are_shared_lesson_actions, test_without_the_daemon_listening_says_the_robot_app_is_not_running, test_listening_needs_an_open_lesson_with_a_problem_ready, test_a_listener_is_given_the_lesson_the_browser_names, test_spoken_ideas_append_and_never_erase_typed_ideas, test_a_stale_save_is_still_refused_after_a_real_change, test_spoken_ideas_do_not_reach_an_ended_lesson, test_the_browser_toggle_reaches_the_daemon_hook, test_a_standalone_server_answers_the_toggle_with_robot_app_not_running, test_a_refused_listen_from_the_browser_is_a_409_with_the_reason, test_listening_instructions_carry_the_rules_and_the_lesson, test_listening_rules_assume_no_age_and_use_the_lesson_tool_name, test_lesson_context_is_bounded_and_handles_no_lesson, test_said_done_only_matches_a_whole_dismissal | src/lib.rs, src/think_aloud.rs and tests/learning_think_aloud.rs: the same names, plus a_failing_listener_is_answered_not_raised, listen_refusal_by_stage | none (said_done uses eve-intent's normalize, a dev-dependency) |
| test_spend_wiring.py (ref) | test_a_lesson_turn_and_its_reference_search_are_metered | tests/learning_search.rs: test_a_lesson_turn_and_its_reference_search_are_metered | the rest of the file covers other crates |
| test_voice_agent.py (ref) | test_lesson_drops_unknown_arguments, test_lesson_with_unreadable_arguments_does_not_open_the_whiteboard, test_lesson_stale_lesson_is_actionable (the run_lesson side) | src/lib.rs: the same names, requests_are_normalized_and_bounded | the voice session, tool schema and instructions belong to the voice agent crate |
| test_daemon_lesson.py (ref) | daemon IPC and CLI around `LESSON_ACTIONS` | (none) | they test the daemon and CLI crates; this crate exports `LESSON_ACTIONS`, `TUTOR_ACTIONS` and `run_lesson` for them |
| (Python built-ins) | | src/py.rs: float_repr_matches_python, dumps_matches_python, repr_and_str_match_python, parse_int_matches_python | |

## Boundaries
- `webbrowser.open` is the system opener as a subprocess (`open` on macOS, `xdg-open` elsewhere, `cmd /C start`
  on Windows), with its output discarded, as Python's webbrowser does.
- The page assets are embedded at build time from `bridge/src/eve_bridge/learning/web/` (build.rs), where Python
  reads them from disk per request: a rebuilt tldraw bundle needs a rebuild of this crate.

## Differences
- A request body or tutor reply that is not JSON: Python's `json.loads` message ("Expecting value: line 1 column
  1 (char 0)") becomes serde_json's message in the 400 (or the tutor's ValueError). Python's json also accepts
  `NaN`/`Infinity` and lone surrogates, and UTF-16/32 bodies; serde_json refuses them (a 400). A UTF-8 BOM is
  skipped as Python does.
- `search_problems`' "TinyFish search was unavailable" sentence (Python's `OSError`/`ValueError`/`TypeError`
  from a patched or broken `http_request`) cannot happen: every failure arrives as `FetchError` and gets the
  "TinyFish search failed" sentence. A result URL Python's `urlsplit` rejects outright (a bad IPv6 bracket) is
  skipped instead of raising.
- `GET /api/config` with an invalid EVE_LEARNING_PROVIDER, or a store failure on a GET, answers 500 with
  Python's generic "Eve could not finish this request" sentence; Python's handler raised and closed the
  connection with no reply.
- A method other than GET/POST answers 501 with a plain-text body (Python: an HTML error page).
- `--port` must fit in 0..65535 (clap refuses with exit 2; Python failed at bind).
- `LearningApp::listen` never fails: a store error other than a missing lesson (SQLite unreadable) is logged and
  answered as "Open a lesson first"; Python's `listen` let that exception out of its `except KeyError`.
- Python's odd type corners that end in a crash (`KeyError(0)` from a dict `choices`, an `AttributeError`)
  are `LearningError::Other` (HTTP 500, logged) rather than whatever Python's exception mapped to.
