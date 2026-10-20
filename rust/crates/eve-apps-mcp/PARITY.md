# eve-apps-mcp
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/apps_mcp.py` | `src/apps_mcp.rs` (re-exported at the crate root) |
| `eve_bridge/github_repos.py` | `src/github_repos.rs` |
| (Python builtins these lean on) | `src/py.rs`: `json.dumps` flat and `indent=2`, `float()`, `os.path.expanduser`, `quote_plus` / `urlencode`, httpx's form values, `==` of a JSON-RPC id |

## API
| Python | Rust | Notes |
|---|---|---|
| `APP_HOSTS` | `APP_HOSTS` | Same eight hosts. |
| `_quiet_app_urls(record)` | `quiet_app_urls(line) -> bool` | Takes the rendered line. No logging filter is installed (see Differences). |
| `CONFIG_DIR`, `PROTOCOL_VERSION`, `DEFAULT_TIMEOUT_SECS`, `SEP`, `_ON` | `CONFIG_DIR`, `PROTOCOL_VERSION`, `DEFAULT_TIMEOUT_SECS`, `SEP`, private `ON` | Values verbatim. |
| `class Server` | `struct Server` | `&'static str` fields; `headers` a slice of pairs in order. |
| `SERVERS` | `SERVERS: LazyLock<IndexMap<&str, Server>>` | Same six apps, order, labels, urls, auth kinds, default policies and GitHub's two headers. |
| `_APOS`, `APP_WORDS` | private `APOS`, `APP_WORDS: LazyLock<IndexMap<&str, fancy_regex::Regex>>` | Patterns verbatim with `(?i)`; `fancy-regex` runs the lookbehinds and lookaheads. |
| `GITHUB_ISSUE`, `ISSUE_ELSEWHERE` | same names (`regex::Regex`) | |
| `REPO_NAMES` | `repo_names()` / `set_repo_names()` over a process-wide `RwLock` | Python's module global; `AppsBridge::start` sets it. |
| `names_app(app, text)` | `names_app(&str, &str)` | Python's IGNORECASE also matches U+0130/U+0131 to `i`; those two are read as `i` before matching (measured: the only non-ASCII characters where Python and the regex crate's folding differ for the ASCII letters). |
| `OPEN_TOOL`, `KEY_HEADERS`, `POLICIES`, `READ_VERBS`, `WRITE_VERBS`, `NOUN_TOO`, `DESC_CLIP`, `PROP_CLIP`, `RESULT_CLIP`, `VALUE_CLIP`, `LINE_CLIP` | same names | `KEY_HEADERS` an array of pairs; verb sets as arrays. |
| `open_tool_spec(closed)` | `open_tool_spec(&[(&str, &str)]) -> Option<Value>` | Same JSON, key order and text. |
| `open_asked(args)` | `open_asked(&Map) -> Vec<String>` | `str()`, `strip()`, `lower()`, de-duplicated in order. |
| `_truthy(value)` | private `truthy_env` | |
| `class AppsConfig` (+ `google_client_path`, `google_token_path`) | `struct AppsConfig` (+ `google_client_path()`, `google_token_path()`) | `AppsConfig::new(enabled)` is the dataclass defaults; `environ` is `Environ = HashMap<String, String>`, `policy` is `Policy = IndexMap<String, String>`. |
| `policy(environ=None)` | `policy(Option<&Environ>) -> Policy` | `None` reads the process environment. Same warning text, `%r` via Python's `repr`. |
| `configured(environ=None)` | `configured(Option<&Environ>) -> AppsConfig` | `float()` parsing as Python (whitespace, `_`, `inf`, `nan`); a timeout that is not `> 0` is the default. |
| `_read_json(path)` | `read_json(&Path) -> Map` | Public: onboarding, lite_web and share use it. |
| `write_private_json(path, data)` | `write_private_json(&Path, &Map) -> io::Result<()>` | Byte for byte `json.dump(indent=2)` (ensure_ascii, Python float repr) plus a newline; `.tmp` sibling opened 0600, chmod 0600, renamed over. |
| `GOOGLE_AUTH_URL`, `GOOGLE_TOKEN_URL` | same names | |
| `google_scopes(pol)` | `google_scopes(&Policy) -> Vec<String>` | |
| `ENV_TOKENS` | `ENV_TOKENS` | |
| `class TokenSource` (`available`, `__call__`, `_post`, `_google`, `__repr__`) | `struct TokenSource` (`available()`, `get()` (async), private `post`, `google`; `Debug` prints `TokenSource(kind)`) | Implements `eve_google_rest::TokenSource` and `AppToken`. `with_transport` is the `http=` argument; the default posts over reqwest with a 30 s timeout. A tokio mutex stands for the threading lock. |
| `_google_client(path)` | `google_client(&Path) -> Result<Map, CallError>` | `ValueError: no OAuth client in <path>`. |
| `class McpError(Exception)` | `CallError { kind: "McpError" }`, `mcp_error()`, `MCP_ERROR` | One error type carries every Python class by name (`CallError.kind` is `type(e).__name__`). |
| `_sse_messages(text)` | `sse_messages(&str) -> Vec<Map>` | `str.splitlines`, `lstrip`, `strip` as Python. |
| `class McpClient` (`_send`, `_open`, `request`, `list_tools`, `call_tool`) | `struct McpClient` (private `send`, `open`; `request`, `list_tools`, `call_tool`, all async) | Over `eve_google_rest::Transport` (reqwest in the daemon, a script in tests): `new(url, token, headers, timeout, key_header)`, `with_transport(...)`. Headers assembled as the Python dict merge does; session id, agreed protocol version, 404 reopen once, 401/403 and >=400 messages, SSE or JSON replies, `id` compared as Python's `==` (1.0 and True match 1). |
| `class _SessionGone(Exception)` | `CallError { kind: "_SessionGone" }` (private) | Raised from `_open` it propagates, as in Python. |
| `split_name(name)` | `split_name(&str) -> (&str, &str)` | |
| `_words(tool)` | private `words` | |
| `name_reads(tool)` | `name_reads(&str)` | |
| `tool_reads(tool)` | `tool_reads(&Value)` | `readOnlyHint` must be exactly `true`. |
| `routine_write(app, tool, args)` | `routine_write(&str, &str, Option<&Value>)` | Shared with eve-composio-tools. `in (None, False)` keeps Python's `0 == False`. |
| `_clip(text, limit)` | `clip(&str, usize)` | Public (composio uses it). |
| `slim_schema(schema)` | `slim_schema(&Value) -> Value` | Key order kept. |
| `function_tool(app, tool)` | `function_tool(&str, &Value) -> Value` | Name cut to 64 characters. |
| `class Decision` | `struct Decision { action: Action, why }`, `enum Action { Run, Ask, Refuse }` | `Action` compares equal to `"run"` / `"ask"` / `"refuse"` and has `as_str()`. |
| `_flat(value)` | private `flat` | `json.dumps(ensure_ascii=False)` with Python's separators and float repr. |
| `_describe(name, args, value_clip)` | private `describe_line` | |
| `describe_for_confirmation(name, args)` | same | |
| `WHOLE_ASKS`, `WHOLE_ASK_CHARS` | same names | |
| `asks_whole(name)` | same | |
| `_email_preview(args)` | private `email_preview` | |
| `describe_whole(name, args, limit)` | `describe_whole(&str, &Map, usize) -> Option<String>` | Hidden characters through `eve_tool_gate::visible`. |
| `result_for_model(result)` | `result_for_model(&Map) -> Map` | Same keys in the same order; clip counted in characters. |
| `_google_rest_client(app, token, timeout)` | the default `RestFactory` inside `AppsBridge::new` | `GoogleRestClient::new(app, token, timeout)`. |
| `class AppsBridge` | `struct AppsBridge` | `new(config)`; the `client_factory=`, `rest_factory=`, `tokens=` seams are `with_client_factory`, `with_rest_factory`, `with_tokens`; `insert_client` / `insert_raw` stand for the tests' `_clients[...]` / `_raw[...]`. `available`, `started()`, `apps()`, `start` (async), `tools`, `guess`, `open_tool`, `tools_for`, `open`, `owns`, `ensure_open`, `names()`, `reads`, `prepare` (async), `decide`, `execute` (async), `instructions`; `_connect`, `_load_repos`, `_tool_names` private. `paths`, `policy`, `repos`, `config` public fields. |
| (`AppsBridge.execute` as github_repos' `call`) | `impl github_repos::Call for AppsBridge` | |
| (an app client: McpClient or GoogleRestClient, duck-typed) | `trait AppClient` (`list_tools`, `call_tool`, `google_rest()`) | Implemented for `McpClient` and `GoogleRestClient`; `google_rest()` is Python's `type(client) is GoogleRestClient`. |
| (a token callable with or without `available`) | `trait AppToken`, `FnToken`, `token_fn`, `SharedToken` | `available()` is `None` for a plain closure (Python's `getattr(src, "available", None)`). |
| `_pkce()` | `pkce()` | `token_urlsafe(64)` verifier, S256 challenge. |
| `google_login(config, open_url, port=0, say=print)` | `google_login(&config, open_url, port, say)` (async), `google_login_with(..., transport)` | Loopback listener on 127.0.0.1, one request per accept (300 s each), state checked, plain-text page, PKCE exchange, 0600 token file with the same keys, the "Not granted" line. Errors as `anyhow` with the Python messages. |
| `github_repos.REFRESH_SECS`, `FETCH_MAX`, `SHOWN`, `DESC_CLIP`, `MIN_NAME`, `WORDS`, `FILE`, `DICTIONARY` | same names in `github_repos` | |
| `github_repos._english()` | private `english()` (`OnceLock`) | |
| `github_repos.Call` | `trait github_repos::Call` (async; any `Fn(&str, &Map) -> Value` is one) | Returns `Result` so a raising call is logged by type, as Python's `except Exception`. |
| `github_repos._parse(result)` | private `parse` | |
| `github_repos.fetch(call)` | `fetch(&dyn Call) -> Result<Option<Vec<Repo>>, CallError>` (async) | `Repo { name, description }` for the `{name, description}` dicts. AttributeError/TypeError where Python raises them. |
| `github_repos.load(path)` | `load(&Path) -> (Vec<Repo>, f64)` | |
| `github_repos.refresh(path, call, clock=time.time)` | `refresh(&Path, &dyn Call, clock)` (async) | `Err` only when the fresh list cannot be written (Python lets that OSError out). |
| `github_repos.line(repos)` | `line(&[Repo])` | |
| `github_repos.keyword_names(repos)` | `keyword_names(&[Repo]) -> BTreeSet<String>`, `keyword_names_in(.., dictionary)` | |
| `github_repos.names_repo(text, names)` | `names_repo(&str, &BTreeSet<String>)` | |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_apps_mcp.py` | `test_a_tool_reads_only_when_its_hint_and_its_name_agree`, `test_the_policy_has_the_owners_defaults_and_an_env_override`, `test_scopes_follow_the_policy`, `test_a_schema_is_slimmed_for_the_model`, `test_the_confirmation_line_is_plain_and_clipped`, `test_a_result_reaches_the_model_structured_or_as_text_and_clipped`, `test_start_offers_each_apps_tools_under_its_policy`, `test_an_app_that_fails_is_left_out_and_the_rest_come_up`, `test_decisions_follow_the_policy`, `test_open_app_loads_an_app_for_the_turn`, `test_open_app_opens_several_apps_in_one_call_and_reports_the_unknown_ones`, `test_a_message_opens_the_apps_it_names`, `test_owns_and_ensure_open_cover_every_up_apps_names`, `test_a_tool_an_app_lacks_is_refused_with_its_real_tools`, `test_execute_calls_the_servers_own_tool_and_never_raises`, `test_instructions_name_the_apps_up_and_their_rules`, `test_instructions_steer_the_brain_to_the_apps_and_to_chain_them`, `test_an_app_without_its_sign_in_is_not_available`, `test_configured_reads_the_switch`, `test_the_client_initializes_keeps_the_session_and_reads_sse`, `test_the_client_reopens_a_forgotten_session_once`, `test_the_client_sends_an_api_key_in_its_header_and_reports_errors`, `test_sse_parsing_joins_data_lines_and_skips_junk`, `test_a_google_token_is_refreshed_only_near_expiry_and_kept_private`, `test_a_refused_refresh_is_none_and_a_log_line_without_the_token`, `test_env_tokens_are_read_as_they_are`, `test_app_urls_stay_out_of_the_log`, `test_contacts_is_a_read_only_toolset`, `test_an_email_question_is_the_whole_email_with_hidden_characters_shown`, `test_the_rest_tools_carry_the_mcp_servers_names` (the tool_reads half), `test_the_client_uses_the_protocol_version_the_server_agreed`, `test_github_asks_for_ci_beside_its_toolsets`, `test_maps_replies_keep_their_links` | same names in `tests/test_apps_mcp.rs` (parametrized cases looped), plus `test_the_contacts_words_find_someone_elses_address`, `test_the_client_reports_an_http_error_with_its_body_and_a_missing_reply`, `test_a_token_endpoint_that_answers_junk_is_none_and_names_the_type_only`, `test_a_private_json_file_is_written_as_python_writes_it`, `test_a_reply_question_says_it_goes_into_the_thread`, `test_open_tool_spec_and_open_asked`, `test_google_login_signs_in_on_a_loopback_port_and_says_what_was_not_granted`, `test_pkce_is_s256_over_the_verifier` | The Google REST tests in this file (`test_gmail_*`, `test_contacts_search_*`, `test_mail_text_*`, `test_calendar_*`, `test_an_event_*`, `test_maps_links_*`, `test_every_calendar_call_*`, `test_all_day_events_use_dates`, `test_drive_*`, `test_suggest_time_*`, `test_a_google_error_*`, `test_times_without_an_offset_get_the_macs`, `test_free_slots_*`, `test_list_events_*`, `test_responding_*`, `test_guests_*`, `test_update_adds_guests_*`, `test_a_recurring_event_carries_its_zone`, `test_attachments_*`, `test_a_pdf_*`) test google_rest.py and are ported in eve-google-rest's `tests/test_google_rest.rs`. `test_app_urls_stay_out_of_the_log`'s third assertion (the filter installed on the httpx logger) has no Rust counterpart: there is no httpx logger. |
| `bridge/tests/test_github_repos.py` | `test_fetch_reads_the_owners_login_then_their_repositories`, `test_refresh_keeps_a_day_and_keeps_the_old_list_when_github_fails`, `test_the_line_names_each_repository_with_what_it_is_clipped`, `test_only_unusual_names_open_github_as_words`, `test_the_bridge_loads_them_and_the_brain_and_the_router_see_them` | same names in `tests/test_github_repos.rs`, plus `test_fetch_reads_a_structured_result_and_the_repositories_key`, `test_a_failing_call_keeps_the_old_list_and_logs_the_type_only`, `test_load_reads_nothing_from_a_broken_file`, `test_a_dictionary_word_is_never_a_keyword` | The `toolset_router.keyword_toolsets` assertions in the bridge test belong to eve-toolset-router (here `names_app` is asserted instead). |
| `bridge/tests/test_routine_app_policy.py` (ref) | `test_supported_private_changes_run_and_owner_overrides_still_apply`, `test_consequential_or_unclassified_writes_ask_even_with_write_policy`, `test_consequential_names_cannot_hide_behind_a_read_hint` (apps half), `test_routine_calendar_decision_matches_real_rest_delivery_parameters`, `test_ambiguous_aliases_or_nonmapping_arguments_cannot_make_a_write_routine` (apps half), `test_prepare_private_edit_pins_delivery_and_survives_a_guest_race`, `test_prepare_guests_or_incomplete_event_leave_arguments_and_confirmation_unchanged`, `test_prepare_complete_event_without_attendees_field_is_private`, `test_prepare_failed_read_asks_and_does_not_log_event_content`, `test_prepare_respects_controls_and_policy_without_an_extra_read`, `test_prepare_never_trusts_an_unknown_calendar_adapter` | same names in `tests/test_routine_app_policy.rs`, plus `test_routine_write_reads_composio_names_and_their_arguments` | The `composio_tools.decide` / `is_read_only` assertions and `test_composio_checks_each_calls_actual_arguments_and_takes_the_strictest`, `test_uninspectable_composio_batches_do_not_run`, `test_workbench_writes_need_review_when_their_arguments_are_not_known` belong to eve-composio-tools (the `routine_write` calls they rely on are covered by the added test). |
| `bridge/tests/test_routine_confirmations.py` (ref) | `test_chat_prepares_private_calendar_edit_and_keeps_guest_edit_approval` (the direct bridge's part) | `test_a_guest_free_edit_runs_unasked_and_a_shared_one_asks` | The Telegram inlet, its question and taint belong to eve-brain. |
| `bridge/tests/test_lean_appspolicy_replay.py` (ref) | `test_an_unhinted_tool_named_like_a_read_does_not_run_under_a_read_policy`, `test_control_a_hinted_read_runs_and_a_hinted_trash_does_not` | `test_lean_replay_an_unhinted_tool_named_like_a_read_does_not_run_under_a_read_policy`, `test_lean_replay_control_a_hinted_read_runs_and_a_hinted_trash_does_not` (in `tests/test_apps_mcp.rs`) | The Composio session and workbench replays belong to eve-composio-tools. |
| `bridge/tests/test_tool_gate.py` (ref) | `test_a_gate_question_shows_every_hidden_character` (the `apps_mcp.describe_whole` assertion) | `test_a_gate_question_shows_every_hidden_character` (in `tests/test_routine_app_policy.rs`) | The rest are eve-tool-gate's and eve-composio-tools'. |
| `bridge/tests/test_self_context.py` (ref) | the `apps_mcp.POLICIES` assertion | `test_the_policies_are_the_four_self_context_words_for` | The rest are self_context's. |
| `test_composio_connect.py`, `test_eve_mcp.py`, `test_telegram.py`, `test_telegram_toolsets.py` (ref) | use `write_private_json`, `slim_schema`, `PROP_CLIP`, `DESC_CLIP`, `RESULT_CLIP`, `PROTOCOL_VERSION`, `Decision`, `OPEN_TOOL` and `AppsBridge` with fakes from other modules | covered by the tests above and by `tests/test_parity.rs` | The flows are composio_connect's, eve_mcp's and telegram's. |
| (parity measurement) | — | `tests/test_parity.rs`: `names_app_matches_python` (2544 texts × 6 apps), `tool_reads_matches_python`, `routine_write_matches_python` (4000 cases), `describing_an_act_matches_python`, `results_reach_the_model_as_python_hands_them`, `schemas_and_function_tools_match_python` (byte for byte, key order included), `open_asked_matches_python`, `policy_and_scopes_match_python`, `clip_matches_python`, `sse_matches_python`, `configured_matches_python`, `private_files_are_written_byte_for_byte_as_python_writes_them`, `github_repos_match_python`; the fixture is written by `tests/fixtures/gen_parity.py` from the live Python | |
| (unit) | — | `src/py.rs` `#[cfg(test)]` | |

## Boundaries
None.

## Differences
- Everything that talks to the network is async (`McpClient`, `TokenSource::get`, `AppsBridge::start` / `execute` / `prepare`, `github_repos::fetch` / `refresh`, `google_login`); the Python was blocking.
- One error type, `CallError { kind, message }`, stands for every Python exception class (`kind` is the class name, so `execute`'s `"<Type>: <message>"` reads the same). A transport failure carries httpx's class name, as eve-google-rest's transport names it.
- The Python installs a filter on the httpx logger that drops request lines naming the apps' hosts. The Rust HTTP stack logs no request line at INFO, so there is nothing to filter; `quiet_app_urls` is kept for any log layer that wants the same rule.
- `function_tool` reads a tool without a `name` as an empty name, and `start` leaves such a tool out (Python raised KeyError out of `start`; McpClient never lists one).
- A policy value outside `POLICIES` set directly on `AppsConfig` (never through `policy()`) gets the `ask` line in `instructions` instead of Python's KeyError.
- An unreadable `/usr/share/dict/words` that is not UTF-8 is an empty dictionary (Python's UnicodeDecodeError would have left `start`).
- An infinite `EVE_APPS_TIMEOUT_SECS` (`inf`) is a century-long timeout for reqwest (httpx waits forever).
- A token file's float that is not finite is written as `null` (Python writes `Infinity`); a reply holding `NaN`/`Infinity` is not JSON to serde (Python's json accepts both).
- `str.lower()` is Rust's `to_lowercase` (full mappings with final sigma, as Python's).
