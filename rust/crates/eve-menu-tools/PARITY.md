# eve-menu-tools
Status: complete

## Modules
| Python | Rust |
|---|---|
| `bridge/src/eve_bridge/menu_tools.py` | `src/lib.rs` |

## API
| Python | Rust | Notes |
|---|---|---|
| `SHIPPED` | `SHIPPED` | `true`, same comment on why |
| `MODES` | `MODES` | `["on", "off"]` |
| `enabled` | `enabled(environ: Option<&HashMap<String, String>>)` | `None` reads the process env (as Python's `os.environ`); same truthy/falsy spellings, strip + lower; anything else is `SHIPPED`. Strips U+001C..U+001F as Python's `str.strip()` does |
| `SPEND`, `RUNDOWN`, `CLAUDE`, `CODEX`, `STEALTH`, `APPS`, `LINK` | same-named `&str` consts | same tool names |
| `_NO_ARGS` | `no_args()` (private) | builds the same empty strict schema |
| `TOOLS` | `TOOLS: LazyLock<Vec<Value>>` | descriptions and schemas verbatim; key order kept (`preserve_order`); test compares compact JSON byte for byte with the Python's output |
| `TOOL_NAMES` | `TOOL_NAMES: [&str; 7]` | same order as `TOOLS` |
| `READ_FILE` | `READ_FILE` | `"read_file"` |
| `FILE_TOOLS` | `FILE_TOOLS: LazyLock<Vec<Value>>` | verbatim |
| `HANDS_OFF` | `HANDS_OFF: [&str; 6]` + `hands_off(name)` | frozenset → array with a membership helper |
| `tools` | `tools(environ)` | `TOOLS` + `FILE_TOOLS` (clones) while on, empty while off |
| `ended` | `ended(result: &Value)` | object only; `ok` and `end_turn` judged by Python truthiness (null/false/0/""/[]/{} are false) |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_menu_tools.py` | `test_every_menu_tool_is_lent_allowed_and_dispatched_on_every_turn` (order of the lent prefix), `test_the_switch_off_lends_nothing_and_writes_no_line` (switch parts), `test_the_always_carried_toolsets_match_the_tools_and_leave_the_routed_seven_alone` (strict + required == properties, ALWAYS == TOOL_NAMES + READ_FILE), `test_a_failed_hand_off_goes_back_to_the_model`, `test_read_file_is_lent_with_the_switch_and_dispatched` (lent with the switch) | same names in `tests/menu_tools.rs`, plus `test_tool_schemas_are_the_pythons_byte_for_byte`, `test_the_switch_reads_every_spelling`, `test_every_hand_off_is_a_menu_tool_and_spend_is_a_reading`; unit tests `test_truthy_follows_python`, `test_enabled_reads_the_map_not_the_process_when_given` in `src/lib.rs` | The rest (`test_events_search_*`, `test_the_switch_on_writes_the_tool_choice_lines_*`, `test_intent_block_*`, `test_every_tool_name_a_written_block_holds_is_offered`, `test_a_bare_device_word_line_*`, `test_the_switch_rewords_the_start_task_lines_*`, every hand-off/relay/spend/rundown/apps/link/events dispatch test, `test_read_file_reads_plain_text_*`, and the dispatch/tool_gate halves of the partially ported ones) drive `telegram.TelegramInlet`, `telegram.intent_block`, `telegram.read_file`, `toolset_router` and `tool_gate`: they test those modules and belong to their crates |
| `bridge/tests/test_menu_off_bytes.py` (tests_ref) | `test_switch_off_jobs_list_and_opentable_are_origin_main_s_wording` (the `menu_tools.enabled()` assert) | `test_switch_off_jobs_list_and_opentable_are_origin_main_s_wording` | chief/opentable/telegram request bytes belong to those crates |
| `bridge/tests/test_intent_eval.py`, `bridge/tests/test_tool_gate.py` (tests_ref) | only use `menu_tools.CLAUDE` / `menu_tools.STEALTH` as names | covered by the byte-for-byte schema test | the harness and gate belong to their crates |

## Boundaries
None

## Differences
- `enabled` and `tools` take `Option<&HashMap<String, String>>` instead of any `Mapping`; `None` is the process environment, as in Python.
- `HANDS_OFF` is an array plus `hands_off()` rather than a frozenset; the members are the same.
- The fixture `tests/fixtures/python_tools.json` is the Python module's own `json.dumps(..., separators=(",", ":"), ensure_ascii=False)` output, captured once; if the Python schemas change, regenerate it.
