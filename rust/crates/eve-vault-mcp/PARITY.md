# eve-vault-mcp
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/vault_mcp.py` | `src/lib.rs` (the server, `pick_field`, `same_page`, `server_config`, `main`), `src/casefold.rs` (Python's `str.casefold`), `src/main.rs` (the `eve-vault-mcp` binary, for `python -m eve_bridge.vault_mcp` / `__main__`) |

## API
| Python | Rust | Notes |
|---|---|---|
| `PROTOCOL_VERSIONS` | `PROTOCOL_VERSIONS` | same order; the first is the default |
| `SERVER_NAME`, `SERVER_INFO` | `SERVER_NAME`, `SERVER_VERSION`, `server_info()` | `{"name": "eve_vault", "version": "1.0.0"}` |
| `TOOL_NAMES` | `TOOL_NAMES` | |
| `FIELD_WAITS`, `COUNT_WAIT` | `FIELD_WAITS`, `COUNT_WAIT`; per server `Server::field_waits`, `Server::count_wait` | the per-server copies are the seam the Python tests' `monkeypatch.setattr` was |
| `PART_WORDS` | `PART_WORDS`, `part_words(part)`, `words_fit(part, words)` | same patterns with `(?i)` for `re.I`; `PART_WORDS.get(part).search(words)` is `words_fit`, which keeps two `re` semantics the regex crate lacks: Python's `\b` beside `pan` (its `\w` is letters, numbers and `_`: `pan²` does not fit, `pan` + a combining mark does) and `re.I` matching `i` to `İ`/`ı`. Checked against CPython 3.13 for every character |
| `INSTRUCTIONS`, `TOOLS` | `INSTRUCTIONS`, `TOOLS` | verbatim; `tools/list` checked byte for byte against the Python reply |
| `class Server` | `struct Server` | `Server::new(vault: Arc<Vault>, native: Option<SharedNative>, out)`, `Server::open_default()` for `Server()` |
| `Server.native` (property) | `Server::native() -> Result<SharedNative, Raised>` | `NativeMac` made on first use; a failure is `RuntimeError` and is retried next call, as Python leaves `_native` unset; `with_make_native` replaces the maker |
| `Server.vault_list` | `Server::vault_list` | `lines()` with the default 40 |
| `Server.vault_fill` | `Server::vault_fill(handle, part, page, label) -> Result<(bool, String), Raised>` | takes the raw JSON arguments and applies `str(x or "")`, `strip`, `" ".join(split())` as Python does; every message verbatim (Python `repr` via `eve_vault::pystr::repr`) |
| `Server.call` | `Server::call(name, args) -> Value` | an escaping exception is `The vault could not do that (<Python class>).`; a Rust panic is caught too (`Exception`) |
| `Server.write` | `Server::write` | `json.dumps(msg, ensure_ascii=False)` (`dumps`: `", "` / `": "` separators, Python float repr), newline, flush, under a lock |
| `Server.handle` | `Server::handle` | same invalid-request / response / notification / method-not-found branches and codes |
| `Server.handle_line` | `Server::handle_line` | `str.strip`, parse error `-32700` with `id: null`, a batch answered one line per request |
| `Server.serve` | `Server::serve(BufRead) -> io::Result<i32>` | lines split as Python's text-mode stdin (universal newlines) |
| `pick_field` | `pick_field(fields, part, label) -> Result<Field, String>` | Python's `(None, why)` is `Err(why)`; sorted by `order` (stable); label matched with `casefold` |
| `_same_page` | `same_page(url, page) -> Result<bool, Raised>` | `urlsplit` from `eve_vault::pyurl`; a URL it refuses is `ValueError`, as in Python |
| `server_config` | `python_server_config(python, env)` (Python's exact entry: `-I -m eve_bridge.vault_mcp`), `server_config(command)` / `server_config_with(command, env)` (this crate's binary), `vault_env`, `default_command` | `env` only when `EVE_VAULT_INDEX` / `EVE_VAULT_KEYCHAIN` are set and non-empty |
| `main` | `main()`, `main_with(make)` | `src/main.rs` calls it and exits with its code |
| (`str.casefold`, `str.lower`) | `casefold`, `lower` | table of every character whose fold differs from its lowercase, copied from CPython 3.13.14 (Unicode 15.1), and the 55 characters unassigned there that Rust's newer Unicode lowercases (kept as Python keeps them); both checked against CPython 3.13 for every character. `lower` is `_same_page`'s `netloc.lower()` |
| (`Field`, `NativeMac`, the fake's four methods) | `eve_vault_native::{Field, Native, NativeMac}` | upstream |
| (`Vault`, `VaultError`, `site_allows`, `site_of`, `PARTS`, `ALL_PARTS`) | `eve_vault::*` | upstream |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_vault_mcp.py` | `test_the_server_speaks_mcp_and_lists_no_value`, `test_a_fill_finds_the_field_by_what_it_is_on_the_items_own_page`, `test_codex_vault_server_config_carries_the_vault_settings`, `test_a_page_named_without_its_scheme_is_still_that_page` | same names in `src/tests.rs` | |
| `bridge/tests/test_vault_mcp.py` | `test_live_native_fill_sets_the_right_fields_with_the_browser_behind_and_unfocused`, `test_live_a_card_fills_a_payment_frame_from_another_host_inside_the_named_page` | same names, `#[ignore = "live: ..."]`, same pages, steps and assertions | Playwright is replaced by the Chrome DevTools Protocol (`tests::live`: a static file server, a headed Chrome for Testing from Playwright's cache or `EVE_LIVE_CHROME`, a window per page, `Runtime.evaluate` in the page or its out-of-process frame); the front app is read with `lsappinfo`. The harness was checked headless; the fills themselves need the Accessibility permission and a desktop, so they were not run here |
| `bridge/tests/test_vault_mcp.py` | `test_codex_threads_get_the_vault_server_only_when_the_vault_has_items`, `test_codex_a_warm_thread_is_stale_once_the_vault_gains_or_loses_its_items`, `test_codex_accepts_only_the_vault_servers_own_tool_approvals` | | they test `codex_computer` (`_thread_params`, `_vault_changed`, `vault_tool_call`): crate `eve-codex-computer` |
| (parity additions) | | `tools_list_is_byte_for_byte_what_python_writes`, `replies_are_written_as_python_json_dumps_writes_them`, `initialize_answers_a_known_version_else_the_newest`, `invalid_requests_responses_notifications_and_batches_are_handled_as_python_does`, `serve_reads_lines_as_pythons_text_stdin_splits_them`, `unknown_tools_odd_arguments_and_an_empty_vault_answer_in_sentences`, `a_mac_that_cannot_be_made_or_is_not_trusted_is_said_plainly`, `a_page_urlsplit_refuses_is_a_value_error_as_in_python`, `pick_field_messages_and_order_match_python`, `casefold_is_pythons`, `part_words_match_as_pythons_re_does`, `lower_is_pythons_and_keeps_what_python_does_not_know`, `a_filled_field_is_named_by_its_words_or_as_text`, `tools_and_constants_are_pythons`, `server_config_starts_this_binary_or_pythons_module` | expected strings were produced by running the Python module (CPython 3.13) on the same inputs |

## Boundaries
None. The Accessibility work is `eve-vault-native`; the Keychain's `security` subprocess is `eve-vault`'s (its PARITY.md).

## Differences
- Unicode version: the regex crate's `\p{L}`/`\p{N}` (the `\b` beside `pan`) follow Unicode 16, CPython 3.13 follows 15.1. Measured over every character: 4382 characters assigned in Unicode 16 and unassigned in 15.1 count as word characters here and not in Python. No character Python knows differs. (`casefold` and `lower` have no such gap: the 55 cased ones are tabled.)
- `server_config(command)` starts this crate's `eve-vault-mcp` binary with no arguments (the one beside the running executable, else from `PATH`), so a Rust daemon's Codex threads get the Rust server. `python_server_config` gives Python's exact entry (`<python> -I -m eve_bridge.vault_mcp`) for a caller that wants the Python server; with no `python` it names `python3` where Python uses its own `sys.executable`.
- The exception class in "The vault could not do that (...)" comes from a mapping: `NativeMac` failing is `RuntimeError`, a URL `urlsplit` refuses is `ValueError`, a `security` run that timed out is `TimeoutExpired`, one that could not start is `FileNotFoundError` / `PermissionError` / `OSError` by its OS error. A Rust panic is `Exception`. A Keychain item missing a field is `eve-vault`'s "is damaged in the Keychain" refusal where Python raises `KeyError` (an `eve-vault` difference).
- Input lines that are not valid UTF-8 are decoded lossily (and so answer a parse error or an invalid request); Python's stdin raises `UnicodeDecodeError` and the server exits.
- `serde_json` rejects what Python's `json.loads` accepts beyond JSON (`NaN`, `Infinity`, lone surrogate escapes) with a parse error, and reads an integer beyond 64 bits as a float, so such a request id is echoed as a float.
- A server that cannot be made (a bad `EVE_VAULT_KEYCHAIN`) or a reply that cannot be written prints one line on stderr and exits 1, where Python prints a traceback and exits 1.
