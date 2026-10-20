# eve-claude-mem
Status: complete

## Modules
| Python | Rust |
|---|---|
| `bridge/src/eve_bridge/claude_mem.py` | `src/lib.rs` (Python semantics it relies on: `src/py.rs`) |

## API
| Python | Rust | Notes |
|---|---|---|
| `DEFAULT_PORT`, `SOURCE`, `MIRROR_TOPIC`, `SINK_TOPICS`, `LESSON_ACTIONS_KEPT`, `IDLE_SECS` | same names, `pub const` | Values verbatim. `_MAX_STR`, `_MAX_LIST`, `_WARN_EVERY_SECS`, `_ROW`, `_META_FIELDS` are private consts (`MAX_STR`, `MAX_LIST`, `WARN_EVERY_SECS`, `ROW`, `META_FIELDS`). |
| `worker_url(environ=None, home=None)` | `worker_url()` / `worker_url_from(env, home)` | `EVE_CLAUDE_MEM_URL` (stripped, trailing `/` dropped) wins; then `~/.claude-mem/settings.json` `CLAUDE_MEM_WORKER_PORT`, then `~/.claude-mem/worker.pid` `port`, each read as `int(str(value).strip())` and kept only in 1..65535; else `http://127.0.0.1:37777`. Unreadable, non-JSON, non-dict files fall through as in Python. |
| `_clip(value)` | `clip` / `clip_at` | Code points, 1999 + `…`. |
| `_title(prefix, text)` | `title` | Whitespace squashed the `str.split()` way, 60 code points, bare prefix minus `": "` when empty. |
| `_metadata(topic, msg)` | `metadata` | Same allowlist and value rules (bools count as ints inside a list, `None` or nested values drop a list, dicts never ride). `subject` only on `/eve/memory/lesson`. |
| `to_memory(topic, msg, project="eve")` | `to_memory(topic, &msg, project) -> Result<Option<Value>, NotIterable>` | `Ok(None)` where Python returns None. `Err(NotIterable)` where Python raises TypeError (a truthy number or `True` as `observations`); the sink logs it as the bus does. Non-string values are `str()`'d the Python way (`py::py_str`). |
| `_post(url, body, timeout)` | `post(url, &body, timeout)` (+ `Poster`, `http_poster`, `ClaudeMemSink::with_poster`) | Body bytes are Python's `json.dumps` output (`", "`/`": "`, ensure_ascii). The `Poster` seam replaces the tests' monkeypatch of `_post`. |
| `_get(url, timeout)` | `get(url, timeout)` | Empty body is `null`. Non-2xx is `HTTPError`. |
| `ClaudeMemSink` | `ClaudeMemSink<B: Bus>` + `SinkOptions` | `new(bus, SinkOptions{project="eve", url=None, queue_size=256, timeout=5s})`; `start()` (idempotent, spawns one tokio task), `async stop(wait)` (unsubscribes, sends the sentinel unless the queue is full, waits up to `wait`), `stats() -> SinkStats{saved, dropped, failed}`. `_on_event` is `SinkIntake::on_event` (enqueue only, counts drops), `_run` is `SinkRun::run` (one warning per 600 s, same text). |
| `_parse_rows(markdown)` | `parse_rows` | Same row pattern over `str.splitlines()` lines, each stripped. |
| `recall(query, limit=5, url=None, timeout=5.0, project="eve")` | `async recall(query, limit, url, timeout, project) -> Vec<RecallHit>` | `limit or 5` clamped to 1..20; `GET /api/search?query=&project=&limit=` (urlencode quoting), rows truncated to limit, each body from `GET /api/observation/<id>`; `[]` on any error, with the same debug lines. `RecallHit` serializes as `{"id","title","time","text"}`. |
| `ClaudeMemMirror` | `ClaudeMemMirror<B: Bus>` + `MirrorOptions` | `new(bus, MirrorOptions{url=None, reconnect_secs=5s, skip_project="eve", read_timeout=IDLE_SECS})`; `start()`, `async stop(wait)` (ends a blocked read at once, as the socket shutdown did), `stats() -> MirrorStats{mirrored, reconnects}`. `_run`, `_read_stream`, `_on_line` are `MirrorRun::run`, `MirrorRun::read_stream`, `on_line`: a stream that ends or fails is a counted reconnect after `reconnect_secs`; a stream quiet for `read_timeout` reconnects at once, uncounted. |
| `MemoryBus` (used, not defined here) | `Bus` trait, `Callback`, `Subscription`; `impl Bus for eve_memory_bus::MemoryBus` | The three calls this module makes (`subscribe`, `unsubscribe`, `publish`). Implemented here for the memory-bus port's `MemoryBus`, so the daemon passes `Arc<MemoryBus>` straight in; the tests' `TestBus` is a second implementation. |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_claude_mem.py` | `test_worker_url_precedence_env_settings_pid_default`, `test_to_memory_shapes_each_topic`, `test_to_memory_returns_none_for_noise` (10 params, one loop), `test_to_memory_never_carries_learner_words_even_when_offered`, `test_to_memory_clips_long_strings_and_lists`, `test_sink_posts_each_memory_with_project_and_source`, `test_sink_with_the_worker_down_never_blocks_and_warns_once`, `test_sink_queue_overflow_drops_and_counts`, `test_sink_start_and_stop_are_idempotent`, `test_recall_parses_the_search_table_and_fetches_bodies`, `test_recall_returns_an_empty_list_when_the_worker_is_down`, `test_recall_clamps_limit`, `test_mirror_republishes_other_projects_and_skips_eve`, `test_mirror_reconnects_after_the_stream_ends_and_stops_cleanly`, `test_a_quiet_stream_reconnects_and_keeps_mirroring_without_spinning`, `test_stop_ends_a_blocked_read_at_once`, `test_mirror_survives_a_dead_worker`, `test_live_save_then_recall_by_title`, `test_module_never_names_the_learner_word_fields_in_payload_code` | same names in `src/tests.rs`; extra: `worker_url_skips_ports_python_would_not_read`, `to_memory_follows_python_on_odd_values`, `sink_posts_the_bytes_python_json_dumps_writes`, `recall_walks_the_answer_as_python_does`, `mirror_lines_follow_python`, `the_real_memory_bus_carries_the_sink_and_the_mirror` (sink and mirror on the memory-bus port's `MemoryBus`, as the Python tests use the real bus); `src/py.rs`: `strings_match_python`, `urlencode_and_dumps_match_python` | The live test is `#[ignore = "live: ..."]`. The quiet-stream test's CPU-time check (`time.process_time`) is a connection count instead: tests share one process, so process CPU time is not this test's; at most 5 connections in the quiet second proves no spin. |
| `bridge/tests/test_privacy_guards.py` (ref) | the claude-mem sink part of the guard self-test (patched `_post` sees a leaked word) | `privacy_guard_the_sink_check_sees_a_leak` | The rest of that file exercises other modules. |

## Boundaries
None.

## Differences
- Threads become tokio tasks: `start()` must run inside a tokio runtime, and `stop(wait)` is `async`. Dropping a started mirror stops it.
- Error class names in the warning lines are urllib's (`URLError`, `HTTPError`, `TimeoutError`, `JSONDecodeError`, `UnicodeDecodeError`, `ValueError`), chosen from the reqwest error kind; the message after the name is reqwest's text, not urllib's (`<urlopen error [Errno 61] Connection refused>`).
- A request that times out before the response head is logged as `TimeoutError`; urllib names a timeout while connecting or sending `URLError` (`<urlopen error timed out>`) and one while reading the head `TimeoutError`.
- urllib's timeout bounds each socket operation; here `timeout` bounds the connect and response head together, and then each body read.
- `_META_FIELDS` is a frozenset, so Python's metadata key order varies with hash seeding; Rust writes the allowlist order. The receiver reads JSON, so order carries no meaning.
- A search-table row id beyond i64, a row id written in non-ASCII digits, and a port written in non-ASCII digits are skipped; Python's `int()` would read them. The worker writes ASCII ids.
- JSON tokens Python accepts and serde_json does not (`NaN`, `Infinity`) make a stream line or response unreadable here (the line is skipped, the request fails).
- An empty home (no `$HOME` and no passwd entry) falls back to the default URL where `Path.home()` would raise.
