# eve-composio-tools
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/composio_tools.py` | `src/lib.rs` (config, policy, gates, questions, state file, `ComposioBridge`) |
| `composio` 0.22.0 SDK + `composio_client` 1.43 (the parts the module calls: `Composio`, `sessions.create/use`, `session.tools/execute`, `connected_accounts.list`, `tools.execute`) | `src/client.rs` (`ComposioClient` / `ComposioSession` traits, `HttpComposioClient`) |
| `composio.utils.strict_schema.to_strict_json_schema` (used by `OpenAIResponsesProvider(strict=True).wrap_tool`) | `src/strict_schema.rs` |
| CPython `ast.parse` + `ast.walk` (as `workbench_slugs` reads them) | `src/pyast.rs` (ruff's Python parser, re-shaped to CPython's node fields and breadth-first walk) |

## API
| Python | Rust | Notes |
|---|---|---|
| `COMPOSIO_DEFAULT`, `DEFAULT_STATE_PATH`, `DEFAULT_TIMEOUT_SECS`, `_ON` | `COMPOSIO_DEFAULT`, `DEFAULT_STATE_PATH`, `DEFAULT_TIMEOUT_SECS`, `truthy_str` | Same values |
| `MULTI_EXECUTE`, `PRIMARY_ALIAS`, `WEB_TOOLKITS`, `TOOLKITS`, `DIRECT_TOOLKITS`, `OTHER_ACCOUNT_TOOLKITS`, `RUNS_CODE`, `MANAGE_CONNECTIONS`, `MANAGE_KEYS` | same names (`&[&str]` slices) | Same members; frozensets become slices, membership tested the same way |
| `READ_VERBS`, `WRITE_VERBS`, `NOUN_TOO`, `READ_ONLY_SLUG` | same names | `READ_ONLY_SLUG` is `^[A-Za-z0-9]+_[A-Za-z0-9_]+\n?$` so it matches like `re.match` with `$` (one trailing newline allowed) |
| `DEFAULT_TOOLKIT_POLICY`, `POLICIES`, `VALUE_CLIP`, `LINE_CLIP` | `DEFAULT_TOOLKIT_POLICY` (+ `default_toolkit_policy()`), `POLICIES`, `VALUE_CLIP`, `LINE_CLIP` | Policy maps are `IndexMap` (dict order) |
| `WORKBENCH`, `WORKBENCH_TOOL_HELPER`, `WORKBENCH_READ_HELPERS`, `WORKBENCH_ASK_HELPERS`, `WORKBENCH_IMPORTS`, `WORKBENCH_BLOCKED_NAMES`, `WORKBENCH_BLOCKED_ATTRS`, `WORKBENCH_EVALS_STRINGS`, `WORKBENCH_FETCH_READERS`, `MAIL_SEND_WORDS` | same names | The two `WORKBENCH_BLOCKED_*` sets are the final unions (with the evals and fetch readers), as Python rebinds them |
| `ComposioConfig` | `ComposioConfig` (`new`) | Fields `enabled`, `user_id`, `state_path`, `timeout_secs`, `toolkits: Option<Vec<String>>`; the api key is not a field and `Debug` never shows it |
| `_truthy` | `truthy_str` (private) | `strip().lower()` in `{"1","true","yes","on"}` |
| `configured` | `configured(Option<&Environ>, &BTreeSet<i64>)` | `None` reads the process env (`os.environ`). Same log lines (the key is never logged), `float()` parsing of the timeout (`1_5`, padding), `~` expansion of the state path, `EVE_COMPOSIO_TOOLKITS` lowercased/deduped/"all" |
| `slug_words` | `slug_words` | `(toolkit, words)` |
| `is_read_only` | `is_read_only` | |
| `toolkit_policy` | `toolkit_policy(Option<&Environ>)` | Bad entries skipped exactly as Python |
| `_multi_execute_tools` | `multi_execute_tools` (private) | Only mapping items |
| `web_toolkit_slugs` | `web_toolkit_slugs` | |
| `workbench_slugs` | `workbench_slugs(Option<&Value>)` | `ast.parse` via `pyast::parse_module` (Python 3.13 grammar, version-gated syntax rejected like CPython 3.13, null bytes rejected); slugs in `ast.walk` breadth-first order |
| `consequential_slugs` | `consequential_slugs` | |
| `Decision` | `Decision { action: Action, slugs, why }` + `Action::{Run,Ask,Refuse}` (`as_str()` gives `"run"`/`"ask"`/`"refuse"`) | Frozen dataclass: value equality |
| `_personal_account_slugs` | `personal_account_slugs` (private) | |
| `decide` | `decide(name, args, Option<&Policy>, personal_direct: bool)` | Python's keyword default `personal_direct=True` is passed explicitly |
| `manage_needs_yes` | `manage_needs_yes` | |
| `sends_mail` | `sends_mail` | |
| `workbench_reaches_out_lite` | `workbench_reaches_out_lite(Option<&Value>)` | |
| `decide_lite` | `decide_lite` | |
| `_clip` | `clip` (private) | `" ".join(text.split())`, cut by code points, `…` |
| `_flat` | `flat` (private) | `json.dumps(ensure_ascii=False)` text via `eve_apps_mcp::py::dumps` |
| `_pairs` | `pairs` (private) | |
| `_describe` | `describe` (private) | |
| `describe_for_confirmation` | `describe_for_confirmation` | Byte-identical lines (golden replay) |
| `describe_whole` | `describe_whole` | Hidden characters shown as `[U+XXXX]` through `eve_tool_gate::visible` |
| `_read_state` | `read_state` (private) | Missing, unreadable, non-JSON or non-object file is `{}` |
| `_write_state` | `write_state` (private) | `json.dump(indent=2)` + `"\n"`, temp file mode 0600, then rename |
| `_as_dict` | `as_dict` (private) | A non-mapping result becomes `{"data": result}` |
| `ComposioBridge` | `ComposioBridge` (`new`, `started`, `session_id`, `start`, `multi_account`, `accounts_note`, `tools`, `names`, `can_read`, `connected_accounts`, `read_tool`, `execute`) | Async (`start`, `connected_accounts`, `read_tool`, `execute`); `client_factory` is a `ClientFactory` closure, the seam the Python tests used. `connected_accounts` returns `ConnectedAccount` (`to_value()` gives the Python dict). Prompt text of `accounts_note` verbatim |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_composio_tools.py` | all 13 | same 13 names in `src/tests.rs` (fake client/session as in the Python fixture; log capture via `tracing`) | - |
| `bridge/tests/test_composio_workbench.py` (ref) | all 9 | same 9 names, plus `test_the_slugs_come_out_in_ast_walks_breadth_first_order` | - |
| `bridge/tests/test_composio_lite_decide.py` (ref) | 12 of 13 | same names; `test_the_new_keyword_changes_no_verdict_of_full_eve` is `test_every_answer_matches_composio_tools_py` (every golden case, both forms) | `test_a_composio_read_is_your_connected_apps_in_lite_and_three_named_apps_in_full` tests `tool_gate.source` |
| `bridge/tools/owner_golden.py` (DECIDE_CASES) | the whole decide corpus | `test_every_answer_matches_composio_tools_py` replays `tests/fixtures/python_verdicts.json` (123 cases x 4 policies: decide, decide_lite, both question lines, consequential, sends_mail, web, manage_needs_yes, workbench_slugs, reaches_out_lite), generated by `tests/fixtures/make_python_verdicts.py` from the live module | - |
| `bridge/tests/test_lean_appspolicy_replay.py` (ref) | the 5 Composio tests | `test_the_personal_gmail_aliased_with_a_capital_stays_out_of_the_session`, `test_a_configured_gmail_with_only_the_personal_account_stays_out`, `test_all_toolkits_never_lets_drive_or_github_into_the_session`, `test_control_the_work_gmail_joins_and_the_session_names_every_account`, `test_a_workbench_script_that_hides_its_tool_call_never_runs_unasked` | `test_an_unhinted_...`, `test_control_a_hinted_...` test apps_mcp; `test_control_the_hidden_call_really_runs_in_a_python_namespace` runs Python's `exec` |
| `bridge/tests/test_lean_routedturn_replay.py` (ref) | the Composio bridge part of `test_an_unrouted_turn_never_names_composio_while_its_session_is_down` | `test_a_failed_create_leaves_the_bridge_unstarted_with_its_accounts_listed` | The routing itself belongs to the brain's crate |
| `bridge/tests/test_routine_app_policy.py` (ref) | the 4 Composio tests | same 4 names | The rest test apps_mcp / google_rest |
| `bridge/tests/test_updates_composio.py` (ref) | `test_every_allowed_slug_only_reads`, `test_the_bridge_refuses_a_writing_slug_itself`, `test_connected_accounts_leaves_direct_apps_out` | same 3 names | The rest test updates_composio |
| `bridge/tests/test_tool_gate.py` (ref) | `test_a_gate_question_shows_every_hidden_character` (its `describe_whole` part) | same name | The rest test tool_gate |
| `bridge/tests/test_suite_isolation.py` (ref) | `test_the_composio_session_file_is_this_tests_own` | `test_the_state_path_follows_the_setting` (explicit env, no process env) | - |
| `bridge/tests/test_composio_connect.py` (ref) | `manage_needs_yes` controls | covered by `test_a_link_or_a_check_needs_no_yes`, `test_every_other_shape_asks` | The rest test composio_connect |
| `bridge/tests/test_telegram_lite.py` (ref) | uses `MANAGE_CONNECTIONS` | constant exported | The rest test telegram |
| (SDK) | - | `test_strict_schema_matches_the_sdk` replays `tests/fixtures/strict_schemas.json` (30 schemas, SDK's own answers incl. key order, from `make_strict_schemas.py`); `test_enhance_schema_descriptions_...`, `test_a_wrapped_tool_...`, `test_status_errors_...`, three local-HTTP-server tests of request shapes, retries and the bridge end to end | - |

## Boundaries
None. The Composio Python SDK is replaced by `HttpComposioClient`, which speaks the same HTTP API the SDK calls (`/api/v3.1/tool_router/session`, `.../{id}`, `.../{id}/tools`, `.../{id}/execute`, `/api/v3.1/connected_accounts`, `/api/v3.1/tools/execute/{slug}`) with the SDK's headers, retry policy and error texts.

## Differences
- The bridge is async (`start`, `execute`, `read_tool`, `connected_accounts` are `async fn`); Python's were blocking and the daemon ran them in a thread.
- The HTTP client identifies itself as the Python SDK does (`x-source: PYTHON_SDK`, `x-sdk-version: 0.22.0`, `x-framework: openai_responses`), so Composio sees the same client.
- `read_tool` (SDK `tools.execute(..., dangerously_skip_version_check=True)`): the SDK first GETs the tool's schema to drop empty file-upload fields; the port sends the call directly (no file-upload tools are read here). Version is `COMPOSIO_TOOLKIT_VERSION_<TOOLKIT>` or `"latest"` as the SDK resolves it.
- `enhance_schema_descriptions` on a property whose `description` is not a string: the SDK raises `AttributeError` (failing the tool listing); the port treats it as no description.
- `workbench_slugs` parses with ruff's parser instead of CPython. Every script in the fixture corpus (61, incl. f-strings, match, PEP 695, t-strings, `except*`, null bytes) gives CPython's answer; any remaining divergence on exotic syntax yields `None`, i.e. "ask", never "run".
- JSON edge cases outside the wire format (NaN/Infinity, integers beyond i64/u64) are not representable in `serde_json::Value`.
- Exceptions are `ComposioError { kind, message }`; `kind` carries the Python class name (`NotFoundError`, `APITimeoutError`, ...) so `"{type}: {message}"` reasons read the same.
- Logging goes through `tracing` with the same messages and levels; ids, aliases, arguments and the api key are never logged, as in Python.
