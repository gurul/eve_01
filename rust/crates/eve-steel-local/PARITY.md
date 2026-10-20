# eve-steel-local
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/steel_local.py` | `src/lib.rs` (tests in `src/tests.rs`) |

## API
| Python | Rust | Notes |
|---|---|---|
| `DEFAULT_URL`, `DEFAULT_DIR`, `DEFAULT_PROFILE`, `LAUNCHER`, `LOG_FILE`, `START_SECS`, `HTTP_TIMEOUT`, `LOOPBACK` | same `pub const`s | Values verbatim (`LOOPBACK` is an array, not a frozenset) |
| `log` | `tracing::info!` | Same messages: `steel: starting the local server (...)`, `steel: up at ...`, `steel: session <8 chars>` |
| `SteelError` | `SteelError(String)` (`thiserror`) | Returned inside `anyhow::Error`; callers `downcast_ref::<SteelError>()` where Python catches it. Messages verbatim |
| `SteelConfig` | `SteelConfig { url, steel_dir, profile }` | `Default` = the dataclass defaults, `~` expanded |
| `configured` | `configured()`, `configured_from(Fn(&str) -> Option<String>)` | `EVE_STEEL_URL` trimmed, trailing `/` stripped, blank → default; `EVE_STEEL_DIR`, `EVE_STEEL_PROFILE` trimmed, blank → default, `Path(...).expanduser()` via `py_path` (pathlib spelling: no doubled/trailing slashes, no `.` parts) |
| `_loopback` | `loopback` (private) over `hostname()` | `hostname()` / `port()` are a port of `urlsplit` (scheme detection, netloc, `[...]` IPv6 validation, lowercasing, port `ValueError`s as `UrlError`) |
| `_request` | `Http` trait + `DefaultHttp` | reqwest, no proxy (loopback only); `Content-Type: application/json` on every request; body is `json.dumps(body)` byte for byte (`py_dumps`); empty reply → `{}`; non-2xx → `HttpError("HTTP Error <code>: <reason>")` |
| `Http` (type alias) | `Http` trait (`async_trait`) | `request(method, url, body, timeout_secs) -> Result<Map, HttpError>`; `HttpError` is what Python caught (`OSError`, `ValueError`, `URLError`) |
| `healthy` | `healthy(&cfg, &dyn Http) -> bool` | `GET {url}/v1/health`, 2 s, `status == "ok"` |
| `launch_command` | `launch_command(&cfg) -> Vec<String>` | `[str(steel_dir / "steel-local.sh")]` |
| `ensure_server` | `ensure_server(&cfg)`, `ensure_server_with(&cfg, &Deps)` | `Deps { http, spawn, clock, wait_secs, log_file }` are Python's `http`, `popen`, `sleep`+`clock`, `wait_secs` (+ the log path, fixed at `LOG_FILE` by default). Same order: loopback check before any request, health, launcher check, profile `mkdir(parents, mode=0o700)`, port (`or 3000`), env `{**os.environ, HOST, PORT, CHROME_USER_DATA_DIR}`, log opened for append, start in a new session (`setsid`), poll every 0.5 s until the deadline |
| `subprocess.Popen(...)` | `Spawn` trait + `ProcessSpawn` | stdin `/dev/null`, stdout+stderr to the log, `cwd=steel_dir`, `setsid()` in the child; the child is reaped by a background thread when it exits, never waited on |
| `time.sleep`, `time.monotonic` | `Clock` trait + `SystemClock` | `tokio::time::sleep` |
| `start_session` | `start_session(&cfg, visible)`, `start_session_with(&cfg, &Deps, visible)` | Body `{"blockAds": true}` (+ `"headless": false` when visible); websocket must be `ws://`/`wss://` on loopback; `str(x or default)` spelled as Python does |
| `release` | `release(&cfg)`, `release_with(&cfg, &dyn Http)` | `POST /v1/sessions/release` with `{}`; every `HttpError` swallowed |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_steel_local.py` | `test_steel_settings_have_local_defaults_and_can_be_moved`, `test_a_running_server_is_used_and_nothing_is_started`, `test_a_server_that_is_down_is_started_on_loopback_with_eves_profile`, `test_a_server_that_never_answers_is_an_error_naming_the_log`, `test_no_checkout_means_a_plain_error_and_no_start`, `test_steel_off_this_mac_is_refused_before_any_request` (both params), `test_a_session_returns_its_local_websocket`, `test_a_visible_session_asks_for_a_window`, `test_a_session_without_a_local_websocket_is_refused` (all three params), `test_release_is_quiet_when_steel_is_gone` | same names in `src/tests.rs`, plus `blank_settings_mean_the_defaults_and_values_are_trimmed`, `paths_expand_and_normalise_like_pathlib`, `loopback_hosts_are_read_like_urlsplit`, `ports_are_read_like_urlsplit`, `the_port_comes_from_the_url_and_zero_means_3000`, `a_malformed_port_is_not_a_steel_error`, `a_refused_session_is_a_steel_error_with_its_reason`, `values_read_like_python_str`, `the_http_client_speaks_json_like_urllib` (real loopback server), `the_http_client_fails_on_error_status_bad_json_and_no_server`, `the_launcher_runs_in_its_own_session_with_its_env_cwd_and_log` (real process) | |
| | `test_live_the_lane_reads_a_page_through_local_steel_starting_it_when_down` | `test_live_a_session_through_local_steel_starting_it_when_down` (`#[ignore = "live: ..."]`) | Reading the page goes through the browser lane; the Rust live test covers this crate's part (start, session, release) |
| | `test_steel_is_off_unless_asked`, `test_steel_turns_the_lane_on_and_wins_over_own_and_attach`, `test_the_lane_attaches_to_the_steel_session`, `test_steel_not_reachable_is_an_attach_error_with_its_reason`, `test_closing_the_steel_lane_closes_eves_tab_and_releases_the_session`, `test_closing_eves_own_chrome_never_touches_steel`, `test_meet_keeps_eves_own_chrome_when_steel_drives_the_rest` | — | They test `browser_lane.configured`, `BrowserLane` and `meet`, which belong to the browser-lane and meet crates |

## Boundaries
None. (The server itself is the external `steel-local.sh`, started exactly as Python starts it.)

## Differences
- The API is async (tokio): the Rust browser lane is async, where Python's lane calls this from a worker thread.
- A reply that is valid JSON but not an object is an `HttpError` (so `healthy` is false and `start_session` says "Steel refused a new session"); Python would raise `AttributeError` out of `.get`. JSON `NaN`/`Infinity` literals, which Python's `json.loads` accepts, are refused the same way.
- `HttpError` messages for transport failures are reqwest's text inside `<urlopen error ...>`, not urllib's exact wording; the HTTP status message (`HTTP Error 503: Service Unavailable`) matches.
- `Path.expanduser()` for an unknown `~user` leaves the path unchanged; pathlib raises `RuntimeError`.
- `Deps.log_file` lets tests point the log away from `~/Library/Logs`; the default is `LOG_FILE`.
