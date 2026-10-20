# eve-app-registry
Status: complete

## Modules
| Python | Rust |
|---|---|
| `bridge/src/eve_bridge/app_registry.py` | `src/lib.rs` (tests in `src/tests.rs`) |

## API
| Python | Rust | Notes |
|---|---|---|
| `_EMPTY` | `IndexMap::new()` / `Map::new()` in `App::default()` | empty mapping default |
| `App` (dataclass, frozen) | `App` struct | same fields, same defaults (`tools: true`, `controls: true`, the rest empty/false) via `App::default()`; frozensets are `BTreeSet<String>`, mappings `IndexMap` (insertion order kept), `voice_kwargs` a `serde_json::Map`, `cli` a `Vec<String>` |
| `App.mod` | `App::module_info` / `App::module_in(&dyn ModuleSource)` | Python imports `eve_bridge.<module>`; Rust reads a registered `ModuleInfo` (see Boundaries) |
| `App.cli_mod` | `App::cli_module_info` / `App::cli_module_in`, `App::cli_module_name` | `cli_module or module` |
| `_fs` | `fs` (private) | |
| `_map` | `map` (private) | |
| `APPS` | `APPS: LazyLock<Vec<App>>` | same five apps, same values, same order; the marker comment is kept at the end of the list |
| `CODE_ORDER` | `CODE_ORDER` | |
| `PHONE_ORDER` | `PHONE_ORDER` | |
| `RESERVED` | `RESERVED` | `&[&str]` |
| `MARKER` | `MARKER` | verbatim (the Python registry's line, which the scaffold writes above) |
| `_ON` / `_OFF` | `ON` / `OFF` (private) | |
| `get` | `get` | `Option<&'static App>` |
| `with_tools` | `with_tools` | |
| `ordered` | `ordered` | listed slots first (duplicates kept, unknown skipped), then the rest in APPS order |
| `by_code` | `by_code` | |
| `for_phone` | `for_phone` | |
| `switched_on` | `switched_on(app, Option<&HashMap>)` | None reads the process env; `strip().lower()` → `py_strip` (Python's `isspace` set, U+001C to U+001F included) then `to_lowercase()` |
| `owner` | `owner(name, voice)` / `owner_in(name, voice, src)` | |
| `_union` | `union` (private) | |
| `ACTING` | `acting()` | `BTreeSet<String>` |
| `CLEAN` | `clean()` | |
| `SOURCES` | `sources()` | `IndexMap`, APPS order |
| `SAY` | `say()` | |
| `OWN_WORDS` | `own_words()` | |
| `labels` | `labels()` | |
| `prefixes` | `prefixes()` | |
| `check_hubs` | `check_hubs(keys) -> Result<(), UnexpectedHubs>` | Python's `TypeError`; same message, sorted bad keys |
| `check` | `check(app)` / `check_in(app, src)` | same problems, same order, same text (Python reprs reproduced) |
| `_hub_class` | `hub_class(module, make)` | the factory's declared return class, else the one class with `panel` and `close` |
| `UI_VERSION` | `UI_VERSION` | 1 |
| `MAX_CONTROLS` | `MAX_CONTROLS` | 24 |
| `CONTROL_TYPES` | `CONTROL_TYPES` | |
| `button` | `button(id, label, action, symbol, destructive)` | returns `serde_json::Value`, same key order |
| `toggle` | `toggle` | |
| `slider` | `slider(id, label, value, lo, hi, action, step: Option<Value>)` | numbers keep their JSON kind (int stays int) |
| `panel` | `panel(status, controls, line, ok)` | |
| `check_panel` | `check_panel(&Value)` | Python semantics: `ui` equal to 1 as Python compares (`1`, `1.0`, `true`), `str(id)` for the seen-ids set, `repr` of a bad type |
| `_number` | `is_number` (private) | int/float, not bool, finite |
| `SITES` | `SITES` | |
| `site_names` | `site_names()` / `site_names_in(dir)` | same three regexes over the Python wiring sites' source (`site_dir()` = `bridge/src/eve_bridge`); output checked identical to Python's (1211 names, same SHA-256) |
| `name_problem` | `name_problem(name, own_prefixes, labels)` / `name_problem_in(..., src, sites_dir)` | Python 3.13 `keyword.kwlist` and `softkwlist` |
| (repr helpers) | `py_repr`, `py_str_repr`, `py_printable` | Python `repr()` for messages; escapes what `str.isprintable()` rejects (Cc, Cf, Co, Zs/Zl/Zp but space, noncharacters) |
| (module seam) | `ModuleInfo`, `ClassInfo`, `ModuleSource`, `Modules`, `Registered`, `register_module` | see Boundaries |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_app_registry.py` | `test_every_app_meets_the_contract_once`, `test_ordered_puts_unnamed_apps_last`, `test_switched_on_reads_the_switch_both_ways`, `test_a_hub_for_no_app_is_refused`, `test_owner_finds_the_app_and_out_loud_skips_voice_omit`, `test_the_panel_contract_example_passes_and_bad_controls_do_not`, `test_agents_md_names_every_switch_and_the_walkthrough_exists` | same names in `src/tests.rs` (module surfaces described as the Python modules define them today) | None |
| | `test_the_tool_gate_sets_are_todays`, `test_the_eval_labels_and_prefixes_are_todays` | same names: the registry's side (ACTING/CLEAN/SOURCES/SAY/OWN_WORDS, labels, prefixes) | the `tool_gate` and `intent_eval` halves belong to the tool-gate crate and the Python eval tool |
| | `test_a_new_name_may_not_take_a_tool_prefix_a_label_or_a_wiring_attribute` | same name, with the label members the Python test asserts on (`screenshot`, `stealth`, `camera_photo`, `meet`) | `app_template.eval_labels/own_names/own_prefixes` are the app-template crate's |
| | `test_the_chat_reads_the_apps_in_todays_order`, `test_the_voice_reads_the_apps_in_todays_order`, `test_a_tool_reaches_its_apps_hub_or_its_off_reason`, `test_an_app_switched_off_is_gone_everywhere`, `test_a_tv_command_with_the_tv_off_goes_to_the_brain`, `test_the_mini_app_keeps_its_three_cards`, `test_a_new_slot_is_a_control_path`, `test_a_scaffolded_app_passes_its_own_tests_and_registers` | — | they drive telegram, voice_agent, daemon, link_chat, miniapp, intent_eval and app_template; they belong to the brain, daemon, link and app-template crates |
| (Rust only) | | `test_ordered_skips_unknown_slots`, `test_owner_reads_the_registered_modules`, `test_an_unregistered_module_holds_no_tools`, `test_check_names_every_broken_part`, `test_check_wants_tool_names_and_a_prefix`, `test_hub_class_is_the_factorys_return_else_the_one_panel_class`, `test_panel_writes_only_what_it_was_given`, `test_check_panel_messages`, `test_py_repr_matches_python`, `test_name_problem_messages`, `test_site_names_reads_attributes_defs_and_fields` | messages checked against the Python module's output |

## Boundaries
- **App modules.** Python's `App.mod()` imports `eve_bridge.<module>` at call time, and `check`, `owner`,
  `name_problem` and `_hub_class` read its attributes (`TOOL_NAMES`, `TOOLS`, `OFF_REASON`, `INSTRUCTIONS` /
  `INSTRUCTIONS_HEAD`, the factory, `match`, `cli`, and the hub class's methods). Rust has no runtime import and the
  app crates (lights, spotify, firetv, obsbot-panel, obsbot, pika) do not depend on this one, so each describes its
  module as a `ModuleInfo` and the daemon calls `register_module` at startup. Every reader also has an `_in` form
  that takes a `ModuleSource`, which the tests use.
- **Wiring sites.** `site_names` reads the Python wiring sites (`telegram.py`, `voice_agent.py`, `miniapp.py`,
  `daemon.py`, `link_chat.py`) from `bridge/src/eve_bridge`, resolved from this crate's manifest dir at build time,
  as Python reads them beside `app_registry.py`.

## Differences
- A module that no app crate registered: Python's import would raise. `check` reports
  `eve_bridge.<module> is not registered` and stops (and, for a CLI module such as `obsbot` that is not
  registered, `eve_bridge.<cli_module> is not registered`); `owner` and `name_problem` skip that app's tool names and
  log a warning, once per module.
- `site_names` skips a site file it cannot read (logged); Python would raise on a decode error.
- `check_panel`: an unhashable control id (a list or object) makes Python's `in ids` raise; Rust treats it as unseen.
- Python's `str.isidentifier()` / `str.islower()` use Unicode XID / cased tables; Rust approximates with
  `char::is_alphabetic` / `is_alphanumeric` / `is_lowercase` / `is_uppercase`. ASCII slots behave identically.
- `repr()` of a string escapes what Python's `str.isprintable()` rejects from a fixed list of those categories;
  other unassigned code points (Cn), which Python also escapes, print as themselves.
