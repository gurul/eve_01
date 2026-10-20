# eve-browser-intent
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/browser_intent.py` | `src/lib.rs` (laya-mlx side: `src/laya_helper.py`, embedded with `include_str!`) |

## API
| Python | Rust | Notes |
|---|---|---|
| `class Site` (+ `Site.search_url`) | `struct Site`, `Site::search_url` | Fields are `&'static str` / `&'static [&str]`; `""` and `&[]` where Python has defaults. `quote(safe="")` / `quote_plus` ported as `percent_encode` (UTF-8 bytes, `A-Za-z0-9_.-~` kept, uppercase hex). |
| `SITES` | `pub static SITES: &[Site]` | Same 54 sites, same order, same hosts, homes, templates, keys, split / ambiguous / aliases / everyday. |
| `SITE_CUES`, `EVERYDAY_CUES`, `_GOING`, `_DOMAIN` | `SITE_CUES`, `EVERYDAY_CUES`, `GOING`, `DOMAIN` | |
| `def _spoken` | `fn spoken` | |
| `def _host_label` | `fn host_label` | |
| `_SPLITS`, `_SPLIT_RE`, `_BY_FORM`, `_AMBIGUOUS`, `_NAMES`, `_NAME_FORMS`, `_NAME_ALT`, `_NAME_RE` | `struct Table` in `static TABLE: LazyLock` | Same construction order (stable sort by descending length, `setdefault` for names, last-wins for split forms). Patterns use `fancy-regex` for the look-arounds. |
| `def _cued` | `fn cued` | Byte offsets on the same text: `text[..start]` is Python's `text[:start]`. |
| `def join_site_names` | `pub fn join_site_names` | |
| `def site_for` | `pub fn site_for` | Returns `Option<&'static Site>`. |
| `def mentioned_site` | `pub fn mentioned_site` | |
| `ORDINALS`, `NUMBERS`, `RESULT_NOUN` | `ORDINALS: &[(&str, Option<u32>)]`, `NUMBERS`, `RESULT_NOUN` | |
| `_ORD_ALT`, `_NUM_ALT`, `_ORDINAL_RE`, `_NUMBER_RE`, `_PICK_VERB`, `_CLAUSE_END`, `_BARE_RE`, `_ALONE_RE` | same names in SCREAMING_CASE | Pattern text copied verbatim, `(?i)` for `re.IGNORECASE`. |
| `_SITE_ALT`, `_DISFLUENCY`, `_START`, `_QUERY_INTROS`, `_QUERY_END`, `_QUERY_TAIL`, `_QUERY_HEAD` | `TABLE.name_alt`, `DISFLUENCY`, `START`, `QUERY_INTROS`, `QUERY_END`, `QUERY_TAIL`, `QUERY_HEAD` | Verbatim. |
| `class Arguments` | `struct Arguments { site, query, n }` | `n: Option<u32>`. |
| `def _query` | `fn query` | Same trimming loop and quote stripping. |
| `def _number` | `fn number` | Unicode decimal digits read as Python's `int()` does (`digit_value`). |
| `def _ordinal` | `fn ordinal` | |
| `def literal_arguments` | `pub fn literal_arguments` | |
| `_NAV_VERB`, `_RESULT_VERB`, `_RESULT_REF`, `_SITE_REF`, `_NAV_RE`, `_NEXT_STEP` | `NAV_VERB`, `RESULT_VERB`, `NAV_RE` (builds the two refs), `NEXT_STEP` | `_NEXT_STEP.search(goal, pos)` is `regex::Regex::find_at`, which sees the text before `pos` for `\b` as Python does. |
| `def laya_goal` | `pub fn laya_goal` | |
| `KINDS` | `KINDS`, `enum Kind` (`as_str`, `parse`, `Display`) | |
| `class RequestOutcome` | `struct RequestOutcome` | `kind: Kind`, `source: &'static str`; `PartialEq` ignores `latency_ms` (Python `compare=False`). `RequestOutcome::new` is the positional constructor with `latency_ms = 0`. |
| `OPTIONS`, `OPTION_KIND`, `OPTION_KEY`, `RULES`, `GATE_TOP`, `GATE_MARGIN` | `OPTIONS: &[(&str, &str)]`, `OPTION_KIND`, `fn option_key`, `RULES`, `GATE_TOP`, `GATE_MARGIN` | Text verbatim, order kept. |
| `BROWSER_MODEL`, `BROWSER_REVISION`, `BROWSER_SUBFOLDER`, `MLX_CACHE_LIMIT_BYTES` | same | |
| `MODES`, `_LATENCIES` | `MODES`, `LATENCIES` (`Mutex<VecDeque>`, max 256) | |
| `def mode` | `pub fn mode(environ: Option<&HashMap>)` | `None` reads the process environment. |
| `def recent_latencies` | `pub fn recent_latencies` | |
| `class Abstain` | `struct Abstain(String)` (`thiserror`) | Carried through `anyhow::Error`; `expected_outcome` downcasts to log at debug, like the Python `except Abstain`. |
| `def validate_choice` | `pub fn validate_choice` | Same schema check: choice in options, keys == options, every value an int/float (not bool) finite in [0, 1], sum within 0.02 of 1, choice within 0.0002 of the top; then the 0.50 / 0.10 gate. |
| `def _cached_checkpoint` | `pub fn cached_checkpoint`, `LayaSubprocess::cached_checkpoint` | Runs the same `huggingface_hub.snapshot_download(..., local_files_only=True, allow_patterns=["v10s/*"])` in the helper (`locate` mode). |
| `IDLE_DEFAULT_S` | `IDLE_DEFAULT_S` | |
| `def idle_seconds` | `pub fn idle_seconds` | Python `float()` grammar incl. `1_000`, `inf`, `1e3`; non-finite or negative → 600. |
| `class LayaKindEngine` | `struct LayaKindEngine` + `EngineOptions` | `__init__(idle_s, clock, timer)` → `LayaKindEngine::new(EngineOptions { idle_s, clock, timer, runtime })`. `loaded` → `loaded()`, `warm` → `warm()`, `unload_if_idle`, `unload`, `ask` (trait `IntentAsker`). `_load_locked`, `_touch_locked`, `_schedule_locked`, `_on_timer`, `_drop_locked` keep their names on `EngineInner`. `_prefix_fits` runs in the helper (`LayaAgent::prefix_fits`), verbatim. The idle timer is a named thread (`browser-intent-idle`) cancelled through a channel. Failure messages identical: `laya-mlx unavailable: …`, `cklxx/laya-browser@8a7e625 is not cached`, `load failed: …`, `request exceeds the checkpoint's question budget`. |
| `_ENGINE`, `_ENGINE_LOCK`, `def shared_engine` | `static ENGINE: OnceLock`, `pub fn shared_engine` | |
| `def offered` | `pub fn offered -> Vec<(&str, &str)>` | Same order: open_site, search, open_result, then unsupported. |
| `def _carried` | `fn carried` | |
| `def expected_outcome` | `pub fn expected_outcome(request, engine: Option<&dyn IntentAsker>, environ: Option<&HashMap>)` | Same question/state JSON (key order kept), same latency bookkeeping, `round(confidence, 4)`. |
| — | `trait IntentAsker`, `trait LayaRuntime`, `trait LayaAgent`, `enum LoadError`, `struct LayaSubprocess` | Seams for what Python duck-types (`engine.ask`) and monkeypatches (`laya_mlx`, `mlx.core`, `_cached_checkpoint`, `_prefix_fits`). |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_browser_intent.py` | `test_literal_arguments_come_from_the_words`, `test_search_carries_the_literal_query_and_site`, `test_open_site_and_open_result`, `test_none_is_an_answer_not_an_abstention`, `test_laya_is_asked_in_its_native_shape`, `test_only_kinds_the_arguments_can_carry_are_offered`, `test_no_arguments_at_all_is_none_from_code_without_asking_laya`, `test_a_kind_the_arguments_cannot_carry_abstains`, `test_the_gate_abstains_on_low_confidence_or_a_thin_margin`, `test_a_malformed_answer_abstains`, `test_off_switch`, `test_abstains_cleanly_when_laya_mlx_is_absent`, `test_a_failed_load_abstains_and_is_not_retried_every_call`, `test_not_cached_abstains_without_the_network`, `test_warm_loads_once_under_concurrency`, `test_the_real_checkpoint_on_three_obvious_requests` | same names in `src/tests.rs`; the three module-patching tests exist twice: against a fake `LayaRuntime` and, as `test_helper_*`, against the real helper process with fake `laya_mlx` / `mlx` / `huggingface_hub` modules on PYTHONPATH (load kwargs, cache limit, snapshot arguments asserted) | The real-checkpoint test is `#[ignore = "live: ..."]`. |
| `bridge/tests/test_browser_intent_gaps.py` | `test_common_sites_are_in_the_table`, `test_the_table_is_broad_and_consistent`, `test_short_or_shared_host_labels_are_not_names`, `test_new_sites_are_found_in_requests`, `test_everyday_words_are_not_sites`, `test_query_phrase_families`, `test_filler_is_trimmed_but_the_query_kept`, `test_search_results_words_are_not_a_query`, `test_result_numbers_without_a_noun`, `test_an_ordinal_that_is_not_a_result_number`, `test_an_ordinal_inside_a_query_keeps_the_search`, `test_navigation_verbs_reach_laya_as_open`, `test_a_second_step_reaches_laya_as_said`, `test_a_second_step_is_not_part_of_the_query`, `test_other_verbs_reach_laya_unchanged`, `test_a_bare_result_number_offers_only_open_result`, `test_idle_seconds_is_env_tunable`, `test_unloads_after_idle_and_reloads_lazily`, `test_use_resets_the_idle_clock`, `test_zero_idle_never_unloads`, `test_the_idle_timer_really_unloads`, `test_no_prompt_cache_that_every_request_would_miss`, `test_the_real_checkpoint_on_the_closed_gaps`, `test_a_site_search_said_with_another_verb`, `test_find_without_a_site_is_not_a_query` | same names (parametrized cases become loops); `test_no_prompt_cache_that_every_request_would_miss` is asserted inside `test_helper_failed_load_is_not_retried_every_call` on the recorded `laya_mlx.load` kwargs | The real-checkpoint test is `#[ignore = "live: ..."]`. |
| `bridge/tests/test_site_names.py` (tests_ref) | `test_split_site_names_are_joined`, `test_ordinary_words_are_not_joined`, `test_hacker_news_stays_two_words_but_maps_to_its_host`, `test_the_table_maps_names_to_hosts_and_search_urls`, `test_mentioned_site_is_found_after_joining` | same names | `test_the_router_sees_joined_names` tests `task_router.classify` (crate eve-task-router's concern). |
| `bridge/tests/test_plan_success_guard.py` (tests_ref) | uses `expected_outcome` with a fake engine and constructs `RequestOutcome` | covered by the outcome tests above | Its assertions are about `plan_contract` (crate eve-plan-contract). |

Extra Rust tests: `test_offered_keeps_laya_browse_order_and_text`, `test_validate_choice_schema_checks`, `test_result_number_zero_and_numerals`, `test_unload_stops_the_timer_and_drops_the_model`, `test_helper_answers_and_unloads`, `test_helper_refuses_a_question_over_the_head_budget`, `test_helper_that_dies_is_reloaded_on_the_next_question`, `test_helper_locates_the_cached_checkpoint`, `test_missing_interpreter_is_unavailable`, `test_helper_args`, `test_the_real_checkpoint_is_located` (live), `test_search_url_encodes_like_python_quote`.

## Boundaries
- **laya-mlx (Python-only MLX runtime).** The loaded model lives in a helper Python process running
  `src/laya_helper.py` (passed with `-c`, cwd `/`). Spawning it is `_load_locked` (it imports `laya_mlx` and
  `mlx.core`, finds the checkpoint with `huggingface_hub.snapshot_download(local_files_only=True,
  allow_patterns=["v10s/*"])`, calls `mx.set_cache_limit(64 MB)` and `laya_mlx.load(path, subfolder="v10s",
  revision=<pinned>, dtype="float16")`, with no `cache_prompts`); killing it is `_drop_locked`. It speaks one JSON
  object per line: a status line, then `fits` (the verbatim `_prefix_fits`) and `predict` requests. Library output
  on stdout is redirected to stderr so it cannot corrupt the protocol. Values that are not JSON-native (numpy
  scalars, NaN) are sent as their repr, so the Rust schema check rejects them as Python's `type(v) in {int, float}`
  check does.
- **Which Python.** `EVE_BROWSER_INTENT_PYTHON` (new; the interpreter that has laya-mlx), else the bridge
  checkout's venv (`bridge/.venv/bin/python3`, resolved from this crate's location at build time) when it exists,
  else `python3` on PATH. A missing interpreter abstains as "laya-mlx unavailable: …".

## Differences
- Unloading kills the helper process, which frees the model and MLX's buffer cache entirely; Python drops the
  reference, runs `gc.collect()` and `mx.clear_cache()`. Same effect, same log line.
- If the helper process dies mid-question, the engine drops it so the next question loads a fresh one (Python's
  in-process agent cannot die separately from the daemon). A failed *load* is still remembered and not retried.
- `str.casefold()` is ported as lowercasing plus the Latin/Greek full foldings (ß, ẞ, ſ, ς, the ﬀ–ﬆ ligatures);
  other scripts' rare full foldings are not applied. Every name and cue word the module compares is ASCII.
- `\w`, `\d`, `\s` follow Rust's Unicode classes, which match Python's on all text this module meets; they can
  differ on rare combining marks.
