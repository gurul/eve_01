# eve-app-template
Status: complete

## Modules
| Python | Rust |
|---|---|
| `bridge/src/eve_bridge/app_template/__init__.py` | `src/lib.rs` (the scaffold), `src/cli.rs` (`cli` and its argparse parser), `src/pyast.rs` (the `ast` walk `eval_labels` does), `src/template.rs` (`string.Template.substitute`); tests in `src/tests.rs` |
| `app_template/module.py.tmpl`, `test.py.tmpl`, `doc.md.tmpl` (assets) | embedded from where they are with `include_str!` (`MODULE_TEMPLATE`, `TEST_TEMPLATE`, `DOC_TEMPLATE`) |

## API
| Python | Rust | Notes |
|---|---|---|
| `HERE` | `MODULE_TEMPLATE`, `TEST_TEMPLATE`, `DOC_TEMPLATE` | Python reads the three templates beside it; Rust embeds the same files |
| `REGISTRY_LINE` | `REGISTRY_LINE` | the Template text verbatim |
| `Refused` | `ScaffoldError::Refused(String)` | the other variants are what Python lets propagate: `Value` (`str.index`'s "substring not found"), `Template` (KeyError/ValueError from `substitute`), `Syntax` (`ast.parse` of intent_eval.py), `Io` |
| `find_root` | `find_root(start: Option<&Path>)` | `Path.resolve()` (symlinks followed, a missing tail kept), then the first folder up with `bridge/pyproject.toml`; same Refused text |
| `own_prefixes` | `own_prefixes() -> BTreeSet<String>` | `eve_tool_gate::{ACTING, CLEAN, SOURCES, SAY_HEADS}` less every app's `acting`/`clean`/`sources`/`say` names; the first word plus `_` of each name with a `_` |
| `own_names` | `own_names() -> BTreeSet<String>` | the four tool-gate sets, whole |
| `eval_labels` | `eval_labels(root) -> Result<BTreeSet<String>, ScaffoldError>` | `pyast::labels_in` over the file read as `read_text` reads it (universal newlines, a BOM dropped as `ast.parse` drops it); empty when the file is not there |
| (`ast.parse` + `ast.walk` inside `eval_labels`) | `pyast::labels_in(src, names)`, `pyast::LABEL_NAMES`, `pyast::ParseError` | top-level `Assign` (first target) / `AnnAssign` with a `Name` target (redundant parentheses allowed); every str constant in the value, a dict display's values only; adjacent literals joined, f-string literal pieces (and `{x=}` text, and format specs) as Python 3.12+ makes them `Constant`s; escapes decoded (`\N{...}` through `unicode_names2`) |
| `paths` | `paths(name, root) -> Paths` | a struct `{module, test, doc, registry}` in place of the dict |
| `scaffold` | `scaffold(name, root: Option<&Path>, dry_run: bool)` | same checks in the same order, same messages, writes in the same order (module, test, doc, registry) and nothing when refused |
| (`string.Template.substitute`) | `template::substitute(text, &HashMap)`, `TemplateError` | Template's pattern: `$$`, `$id`, `${id}` with ASCII ids ignoring case; `KeyError` / "Invalid placeholder in string: line N, col M" (lines counted as `str.splitlines` counts them) |
| `cli` | `cli(argv) -> i32`, `cli_in(argv, cwd, environ, out, err) -> i32` | the argparse parser reproduced: Python 3.13's option matching (exact, `=`, `--` abbreviations, single-dash chaining, unknown optionals, negative numbers and spaces as positionals, `--`), subparser handling, messages, usage and help text; exit 2 after a usage error, 0 after help, 1 after a refusal |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_app_registry.py` (ref) | `test_a_scaffolded_app_passes_its_own_tests_and_registers` | same name (dry run writes nothing, would == wrote, every file written, a second time and twelve bad names refused with nothing changed; plus the four files compared byte for byte with what Python's scaffold wrote, in `tests/fixtures`), and `the_generated_app_passes_its_own_tests_with_the_daemons_python` (`#[ignore]`, live: runs the generated `test_demo.py` with the daemon's Python, 7 pass, and probes `APPS[-1]`; run 2026-10-20, passed) | ruff over the generated files: the files are byte-identical to Python's, which the Python suite lints |
| | `test_a_new_name_may_not_take_a_tool_prefix_a_label_or_a_wiring_attribute` | same name | none |
| | the rest of the file | — | eve-app-registry's, the tool gate's and the wiring crates' |
| (parity additions) | | `the_eval_labels_are_todays`, `eval_labels_reads_what_python_s_ast_walk_reads` (29 sources, Python 3.13's results in `tests/fixtures/labels_cases.json`), `eval_labels_reads_crlf_as_python_s_read_text_does`, `own_prefixes_and_names_are_todays`, `find_root_walks_up_to_bridge_pyproject`, `a_refusal_names_what_is_in_the_way`, `the_registry_is_read_with_universal_newlines`, `the_cli_parses_prints_and_exits_as_python_s_does` (66 argument vectors, Python 3.13's exit codes, stdout and stderr in `tests/fixtures/cli_cases.jsonl`, including `new fan` in a repository), `app_list_reads_the_switches_from_the_environment`, `pyast::tests::reads_the_shapes_python_s_walk_reads`, `template::tests::substitutes_as_python_template_does` | |

Verification (2026-10-20), one-off and not kept in the tree: `labels_in` against Python 3.13's walk for every top-level
assigned name in all 579 Python files under `bridge/src`, `bridge/tests` and `bridge/tools` (5013 names): all equal.
`cli_in` against Python 3.13's `cli` for 1500 random argument vectors: exit codes, stdout and stderr all equal.

## Boundaries
None. The scaffold writes the Python app (module, test, doc and its line in `bridge/src/eve_bridge/app_registry.py`)
exactly as the Python scaffold does; that is its output, not a call into Python.

## Differences
- The app written is a Python app registered in the Python registry. The Rust registry (`eve_app_registry::APPS`)
  is compiled, so `cli`'s `list` and `name_problem` see a newly scaffolded app only once a Rust crate for it is
  added; the files, messages and refusals are otherwise the same.
- The templates are embedded at build time; Python reads them on each call, so an edited template reaches the Rust
  scaffold at the next build.
- A failure Python lets propagate as a traceback (exit status 1) is printed as one line, `eve-bridge app: <error>`,
  with exit status 1.
- `eval_labels` tokenizes rather than compiles: a syntax error in intent_eval.py is reported when it is in the
  tokens (an unterminated string or bracket, a bad escape, bytes mixed with str in a read value), not anywhere in
  the file as `ast.parse` would. A `\u`/`\U`/octal escape naming a lone surrogate reads as U+FFFD (a Rust string
  cannot hold one).
- argparse wraps its help to the terminal width; Rust prints the 80-column layout always (the layout Python prints
  with no terminal).
