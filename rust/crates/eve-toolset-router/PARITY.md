# eve-toolset-router
Status: complete

## Modules
| Python | Rust |
|---|---|
| `bridge/src/eve_bridge/toolset_router.py` | `src/lib.rs` (tests in `src/tests.rs`) |

## API
| Python | Rust | Notes |
|---|---|---|
| `Predict` (type alias) | `Predict<'a>` | `&dyn Fn(&Value, &Value) -> anyhow::Result<Value>`; an `Err` is the exception predict raises |
| `CALENDAR` .. `WORKSPACE`, `TOOLSETS` | same names, `&str` consts, `TOOLSETS: [&str; 7]` | same order |
| `CONTACTS`, `KEYWORD_ONLY` | `CONTACTS`, `KEYWORD_ONLY` | |
| `MENU`, `EVENTS`, `ALWAYS` | `MENU`, `EVENTS`, `ALWAYS: [(&str, &[&str]); 2]` | dict order kept |
| `always_toolset` | `always_toolset(tool) -> &'static str` | "" when none |
| `MODES`, `TIMEOUT_S`, `GRID`, `MIN_TUNING_RECALL` | same | `GRID` written out; a test checks it equals Python's `round(0.05 * i, 2)` |
| `GATES_FILE` | `GATES_FILE: LazyLock<PathBuf>` | the same checkout path, `bridge/tests/fixtures/routes/toolset_gates.json` |
| `ABOUT`, `INSTRUCTIONS` | `ABOUT`, `INSTRUCTIONS: [(&str, &str); 7]` | verbatim; a test checks them byte-for-byte against the questions cached in `toolset_tuning_answers.json` |
| `questions` | `questions() -> Value` | keys in TOOLSETS order |
| `state` | `state(text, previous) -> Value` | `about`, `previous`, `request`; whitespace collapsed Python's way, clipped to 500 characters |
| `ToolsetAnswer` (+ `to_json`, `from_json`) | `ToolsetAnswer { nouls: IndexMap, ms, error }`, `to_json`, `from_json -> Result<_, Raised>` | `round(…, 4)` / `round(ms, 1)`; `from_json` runs Python's `float()` / `str()` and returns the exception Python raises (ValueError, TypeError, AttributeError) |
| `Route` | `Route { toolsets: BTreeSet<String>, ms, error }` | |
| `ask` | `ask(predict, text, previous, clock) -> Result<ToolsetAnswer, Raised>` | a failing predict is an error answer `"<Class>: <msg>"[:160]`; `Err` only where Python raises outside its `try` (an `answers` that is a truthy non-object) |
| `decide` | `decide(a, gates, budget_ms)` | pass `f64::INFINITY` for Python's default budget; a toolset with no gate uses 1.01 |
| `_SHARE_VERB`, `_SHARE_THING`, `SHARE_WORDS`, `TO_SELF`, `WORKSPACE_WORDS`, `OWN_WORDS`, `FOLLOW_UP` | `SHARE_VERB`, `SHARE_THING`, `SHARE_WORDS`, `TO_SELF`, `WORKSPACE_WORDS` (fancy-regex: it has lookbehind and lookahead), `own_words()`, `FOLLOW_UP` | patterns verbatim with `(?i)` |
| `_words` | `words` (private) | `apps_mcp::APP_WORDS` through `eve_apps_mcp::names_app`, so the owner's repository names open GitHub too |
| `keyword_toolsets` | `keyword_toolsets(text, previous)` | |
| `recall` | `recall(said, needed, toolset) -> Result<f64, Raised>` | `IndexError` when `needed` names a case `said` lacks, as Python |
| `fit` | `fit(answers, keywords, needed, budget_ms) -> Result<IndexMap, Raised>` | pass `TIMEOUT_S * 1000.0` for Python's default; `zip(strict=True)`'s ValueError text kept |
| `GATES`, `SHIPPED` | `GATES: [(&str, f64); 7]` + `gates()`, `SHIPPED` | |
| `load_gates` | `load_gates(path)` | GATES on any read, parse or `float()` failure |
| `router_from` | `router_from(predict, clock, gates) -> Router` | `Router = Arc<dyn Fn(&str, &str) -> anyhow::Result<Route>>`; `gates or GATES` (None or empty) |
| `mode` | `mode(env) -> &'static str` | takes any `eve_jev::Env` (`ProcessEnv` for `os.environ`); same aliases and warning text |
| `_JEV`, `_JEV_LOCK` | `JevCache`, `JEV` | keyed on `(url, model, sha256(key)[:16])`; `Debug` never shows the key |
| `make_router` | `make_router(env)`, `make_router_in(cache, env, shipped)` | the second is the seam the Python tests get from clearing `_JEV` and patching `SHIPPED` |
| `TurnToolsets` (+ `toolsets`, `how`, `log_line`) | `TurnToolsets`, `toolsets()`, `how()`, `log_line()` | `{ms:.0f}` formatted Python's way (ties to even, `nan`, `inf`) |
| `route_turn` | `route_turn(text, previous, router, timeout_s)` | worker thread named `toolset-router`, waited on with `recv_timeout` |
| `toolsets_for` | `toolsets_for(text, previous, router, timeout_s)` | logs `toolset router: <log_line>` at info when a router was asked |
| — | `Raised`, `exception_name`, `perf_counter` | helpers: a Python exception as a value, `type(e).__name__` for a predict/router error, `time.perf_counter` |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `test_toolset_router.py` | every test of `toolset_router` (state, instructions, odd Nouls, decide, errors, fit x4, fitted gates, load_gates, keywords x5 incl. parametrized cases, follow-ups, toolsets_for, follow-up previous, timeout, raise/err/None, mode, make_router, rotated key, not-a-route, work mail) | same names in `src/tests.rs` | `test_the_score_counts_…`, `test_the_bar_needs_all_four_…`, `test_the_eval_drops_a_cached_noul_…`: they test `tools/toolset_eval.py`, an offline script outside `eve_bridge` (its `labels`/`known` filtering is replicated inline in the dataset tests) |
| `test_toolset_router.py` | `test_the_datasets_carry_the_toolsets_and_the_one_removed_which_the_eval_drops` | same name | |
| `test_toolset_router.py` | `test_a_router_that_answers_with_something_else_adds_nothing_and_the_keywords_stand` | — | a `Router` returns `anyhow::Result<Route>`, so a router answering `None` cannot be written in Rust (see Differences); the other path to `"not a route"`, a worker that dies without answering, is `test_a_router_that_raises_a_base_exception_still_leaves_the_keyword_toolsets` |
| `test_lean_routedturn_replay.py` | base-exception router, malformed route, ms not a number | `test_a_router_that_raises_a_base_exception_still_leaves_the_keyword_toolsets`, `test_a_route_whose_ms_is_not_a_number_still_formats`, `test_a_router_whose_answers_are_malformed_errs_with_the_python_class` | the Telegram-turn parts belong to the telegram port |
| `test_github_repos.py` | repository name opens GitHub via `keyword_toolsets` | `test_a_repository_name_opens_github` | the repo-loading parts belong to eve-apps-mcp |
| `test_menu_tools.py`, `test_events.py` | `ALWAYS`, `always_toolset` | `test_the_always_carried_toolsets_are_outside_the_routed_ones` | the comparisons against `menu_tools.TOOL_NAMES` / `events.TOOL_NAMES` belong to those crates; the tool names are asserted literally here |
| `test_telegram_toolsets.py`, `test_link_grade.py`, `test_small_talk.py` | — | — | they test telegram / link_chat wiring with fake routers; nothing in them is this module's own behaviour beyond what is ported above |
| (new) | — | `test_the_keywords_match_python_on_every_dataset_message` | `tests/fixtures/keywords_parity.json`: Python's `keyword_toolsets` and `_words` over all 340 dataset messages plus 98 edge cases (Unicode folding, whitespace, lookbehind and lookahead guards, `#`/`@` forms, the 12-word follow-up limit), recorded 2026-10-20 |
| (new) | — | `test_the_datasets_score_what_the_module_says_they_scored`, `test_every_cached_answer_reads_and_writes_back_the_same`, `test_the_questions_are_the_ones_the_cached_answers_were_asked_with_where_they_overlap` and helpers' edge tests | the cached Jev answers through `from_json`/`to_json`, `decide` and `fit`, matching Python's results today: tuning keywords 169/220, keywords + Jev 218/220, holdout 98/120 and 118/120, and a tuning refit that moves github to 0.7. The tuning numbers differ from the Python comment's 2026-10-05 figures (163, 219) because apps_mcp's words have grown since. Python gives the same numbers. |

## Boundaries
None.

## Differences
- A router returns a typed `Route`, so Python's "answers with something else" and "a Route whose toolsets or ms cannot be read" cases have no Rust equivalent. The remaining failure, a router that dies without answering, is a panic in the worker. It reads as `"not a route"`, as Python's uncaught `BaseException` does. A worker thread that cannot be spawned also reads as `"not a route"`; Python's `Thread.start` would raise out of `route_turn`.
- `load_gates` reads JSON with serde, which refuses the `NaN`/`Infinity` literals Python's `json` accepts. Such a file falls back to GATES.
- `float()` in `from_json` and `load_gates` takes ASCII digits only; Python's also reads other Unicode decimal digits (`float('١')` is 1.0). Neither the eval nor `json.dumps` writes such digits.
