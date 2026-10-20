# eve-plan-contract
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/plan_contract.py` | `src/lib.rs` (Python string semantics in `src/pystr.rs`; `urllib.parse` as Python 3.13 has it in `src/urlparse.rs`) |

## API
| Python | Rust | Notes |
|---|---|---|
| `KINDS`, `EXPECT_KINDS`, `CODE_EXPECT_KINDS`, `KEYS` | same names, `[&str; N]` | Same order. |
| `VAULT_HANDLE` | `static VAULT_HANDLE: LazyLock<Regex>` | `^v_[a-z0-9]{8}\n?\z`: Python's `$` also matches before a final newline. |
| `MAX_STEPS`, `MAX_TEXT_CHARS`, `MAX_TARGET_CHARS`, `MAX_SAY_CHARS` | same (`usize`) | Limits count code points, as Python slices do. |
| `class PlanError` | `struct PlanError(pub String)` (`thiserror`, `Display` is the message) | Every message is Python's, including the tuple and value `repr`s (`pystr::repr`, `repr_tuple`). |
| `class Expect` | `struct Expect { kind, value, host, here }`, `Expect::new(kind, value)` | `Eq + Hash` like the frozen dataclass. |
| `class PlanStep` (+ `describe`) | `struct PlanStep`, `PlanStep::new`, `PlanStep::describe` | `kind`, `text_source` stay strings so dicts round-trip unchanged. |
| `class Plan` | `struct Plan`, `Plan::new(steps)` | Tuples become `Vec`s. |
| `def _text` | `fn text_of` | `" ".join(s.split())[:limit]` for a string, "" for anything else. |
| `def canonical_url` | `pub fn canonical_url(&str) -> Option<CanonicalUrl>`, `canonical_url_value(&Value)` | The 6-tuple is `struct CanonicalUrl { scheme, host, port, path, query, fragment }`. Built on a port of Python 3.13's `urlsplit` (lstrip of C0/space, tab/CR/LF removal, scheme chars, netloc split, bracket and IPvFuture checks, the NFKC netloc check) and its `hostname` / `port` / `username` / `password` properties; every `ValueError` is `None`. |
| `def url_matches` | `pub fn url_matches(&str, &str)` | A caller holding a non-string passes "" (both are not URLs). |
| `def _norm_query` | `fn norm_query` | |
| `_HOSTNAME` | `static HOSTNAME` | ASCII class, `$` as above. |
| `SEARCH_ENGINES`, `COMMON_QUERY_KEYS` | same | `SEARCH_ENGINES` is an array (only membership is used). |
| `REWRITTEN_KEYS`, `ARTICLE_PATHS` | `&[(&str, &[&str])]`, `&[(&str, &str)]` | Looked up by key as the dicts are. |
| `def bare_host` | `pub fn bare_host(&str)`, `pub fn bare_host_value(Option<&Value>)` | `bare_host_value` is `str(value or "")` first, so `bare_host(outline.get("url"))` ports as `bare_host_value(outline.get("url"))`. |
| `def _on_host` | `fn on_host` | |
| `def _search_addresses` | `fn search_addresses` | Reads `eve_browser_intent::SITES` (same table, same order). |
| `_WORD`, `_LEAD_WORDS`, `_LEAD_PHRASES`, `_TRAIL_WORDS`, `_TRAIL_PHRASES`, `_CLAUSE`, `_AT_SITE`, `_TLDS` | `WORD`, `LEAD_WORDS`, `LEAD_PHRASES`, `TRAIL_WORDS`, `TRAIL_PHRASES`, `CLAUSE`, `AT_SITE`, `TLDS` | `[^\W_]` is `[\p{L}\p{N}]` (Python's `isalnum`); `_CLAUSE`'s trailing `\b` is spelled `(?:[^\p{L}\p{N}_].*)?$`, because Rust's `\b` counts combining marks as word characters and Python's does not. |
| `def _query_words` | `fn query_words` | Same trimming loop (lead phrase, else lead word, else trail word, else trail phrase). |
| `def _site_named_last` | `fn site_named_last` | |
| `def _site_label` | `fn site_label` | |
| `def _article_title` | `fn article_title` | `unquote` ported (`urlparse::unquote`: ASCII runs percent-decoded, invalid UTF-8 replaced). |
| `def url_query_expect` | `pub fn url_query_expect(host: Option<&str>, query: &str, here: &str)` | |
| `def url_query_matches` | `pub fn url_query_matches(&str, &Expect)` | `parse_qsl(keep_blank_values=True)` ported (`urlparse::parse_qsl`). |
| `def request_check` | `pub fn request_check(request, intent: Option<Intent>, here)` | `Intent` is `&dyn Fn(&str) -> anyhow::Result<Option<RequestOutcome>>`; None calls `eve_browser_intent::expected_outcome(request, None, None)`. An `Err` or a panic is an abstention, like Python's `except Exception`. |
| `def _expect` | `fn parse_expect` | Same check order. |
| `def parse_step` | `pub fn parse_step(&Value, usize, &str)` | The whole-word check `(?<!\w)…(?!\w)` is done by hand (Rust's regex has no look-around), with Python's `\w`. |
| `def parse_plan` | `pub fn parse_plan(&Value, &str)`, `pub fn parse_plan_with(&Value, &str, ParseOptions)` | `ParseOptions { source (default "astra"), browser, intent }` are the keyword arguments. `"here"` is read as `str(raw.get("here") or "")`. |
| `def step_to_dict` | `pub fn step_to_dict -> Value` | Same key order. |
| `def plan_to_dict` | `pub fn plan_to_dict -> Value` | Same key order; a code-derived check sends the planner's. |
| `_EXPECT_SCHEMA` | `fn expect_schema` | |
| `PLAN_SCHEMA` | `static PLAN_SCHEMA: LazyLock<Value>` | Same keys and order. |
| `PLAN_INSTRUCTIONS`, `PLAN_INSTRUCTIONS_BROWSER` | same (`&str`) | Verbatim (checked against Python's strings in the differential test). |
| `PLAN_INSTRUCTIONS_READ` | `static PLAN_INSTRUCTIONS_READ: LazyLock<String>` | Built the same way: the browser text up to its last `"\n\nSet needs_eyes true"` plus the read paragraph. |
| `def plan_request_text` | `pub fn plan_request_text(&str, &PlanInput)` | `PlanInput` holds the keyword arguments; `done` / `vault` None and `[]` read the same in Python, so they are plain `Vec`s. |
| `def plan_request` | `pub fn plan_request(model, request, &PlanInput, effort, timeout: f64, reads, browser) -> Value` | Same dict, key order kept. |
| `READ_NOT_FOUND`, `READ_INSTRUCTIONS` | same | Verbatim. |
| `def read_request` | `pub fn read_request(model, request, page: &Map, effort, timeout) -> Value` | `page.get(k) or ''` read through `str()` as Python's f-string does. |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| (none in `tests`) | — | — | plan.json lists no primary test file; the tests below are the parts of `tests_ref` that exercise this module. |
| `bridge/tests/test_plan_executor.py` (tests_ref) | `test_a_plan_the_code_cannot_fully_read_is_not_a_plan`, `test_where_text_came_from_is_checked_not_trusted`, `test_text_inside_a_longer_word_is_not_the_owners`, `test_the_planner_can_decline`, `test_the_planner_is_shown_labels_and_roles_never_the_title` | same names in `src/tests.rs` | The walking tests are `plan_executor`'s (crate eve-desktop-lanes). |
| `bridge/tests/test_vault_fill.py` (tests_ref) | `test_a_fill_step_names_a_vault_handle_and_nothing_else`, `test_the_planner_is_told_the_vaults_lines_in_the_browser_only` | same names | Executor and browser-page tests belong to plan_executor / browser_lane / vault. |
| `bridge/tests/test_plan_url_expectation.py` (tests_ref) | `test_url_identity_is_exact_with_only_host_default_port_and_root_normalization`, `test_invalid_url_expectations_reject_whole_plan`, `test_urls_are_not_truncated_to_text_label_limit`, `test_browser_planner_gets_observed_navigation_evidence` | same names | `test_exact_url_requires_fresh_snapshot…` tests `plan_executor._holds`; the live test is browser_lane's. |
| `bridge/tests/test_plan_success_guard.py` (tests_ref) | `test_url_query_is_code_only_and_validated`, `test_url_query_matches_host_and_the_literal_query`, `test_only_a_browser_plan_asks_for_the_request_outcome`, `test_a_failing_intent_is_an_abstention_not_a_bad_plan`, `test_the_derived_check_survives_the_round_trip_the_lane_makes`, and the contract half of `test_duck_duck_go_by_google_is_not_done`, `test_duck_duck_go_by_duck_duck_go_is_done`, `test_the_request_check_replaces_the_planners_choice`, `test_no_outcome_falls_back_to_the_planners_check` | same names (the four mixed tests assert the derived check and `url_query_matches` on the page the executor would see) | The `run_plan` halves and the pre-check / resume tests are plan_executor's; the agent and desktop_worker tests are computer_agent's / desktop_worker's. |
| `bridge/tests/test_url_query_tolerance.py` (tests_ref) | `test_no_site_named_accepts_a_search_on_the_page_already_open`, `test_an_observed_page_that_is_not_a_hostname_is_ignored`, `test_the_plan_dict_carries_the_page_it_was_planned_on`, `test_a_wikipedia_article_whose_title_is_the_query_is_the_search`, `test_an_article_page_is_not_a_search_on_some_other_site`, `test_hacker_news_rewrites_q_to_query`, `test_filler_around_the_query_is_not_part_of_it`, `test_a_different_query_is_still_refused`, `test_a_site_named_at_the_end_of_the_query_is_where_it_was_searched`, `test_the_old_refusals_still_hold`, `test_no_site_named_accepts_every_general_search_engine_in_the_table`, `test_the_agent_sends_the_host_of_the_page_it_planned_on` (its `bare_host` part) | same names (parametrized cases become loops) | `test_the_executor_reads_the_page_open_before_step_1` walks a plan (plan_executor, crate eve-desktop-lanes). |
| `bridge/tests/test_agent_browser_lane.py`, `bridge/tests/test_agent_plan_once.py` (tests_ref) | the prompt and request assertions (instructions, schema, read request) | `test_the_prompts_say_what_the_lanes_rely_on`, `test_the_mac_plan_request_is_text_in_strict_json_out`, `test_the_read_request_is_plain_text` | The agent flows are computer_agent's. |

Extra Rust tests: `test_describe_names_each_step_the_way_the_ledger_does`, `test_plan_to_dict_sends_the_planners_check_never_the_derived_one`, `test_python_reprs_in_error_messages`, `test_casefold_matches_python`, and `test_matches_python_on_the_differential_cases`: 3383 cases (canonical_url, url_matches, bare_host on JSON values, query words, url_query_expect, 2500 url_query_matches, parse_step / parse_plan results and error messages, plan_request_text, plan_request, read_request, every constant) produced by the Python module itself under Python 3.13 (`tests/fixtures/gen_differential.py`, output `tests/fixtures/differential.json`).

## Boundaries
None.

## Differences
- `str.casefold()` is per-character lowercasing plus a table of every character whose full case folding differs
  from its lowercase in Python 3.13's Unicode database (ß, ﬁ, final sigma, Cherokee, Greek iota subscripts, …).
  Letter, number and case data come from Rust's and `unicode-general-category`'s Unicode version, which can differ
  from Python 3.13's (Unicode 15.1) on characters assigned since.
- `ipaddress.ip_address` inside URL brackets is `std::net` parsing (`Ipv4Addr`, `Ipv6Addr` plus Python's `%scope`
  rule); the two agree on every ordinary address form.
- `read_request` / `plan_request` take `timeout` as `f64`, so it is always a JSON float (Python passes
  `config.api_timeout_secs`, a float, too).
