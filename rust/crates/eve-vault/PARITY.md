# eve-vault
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/vault.py` | `src/lib.rs` (the vault), `src/cli.rs` (`cli`, its argparse parser, `input`/`getpass`) |
| (Python built-ins the vault's behavior rests on) | `src/pystr.rs` (`str.isspace/split/strip`, `repr`, `str()`/truthiness/`int()` of a JSON value, `json.dumps` ASCII escaping), `src/pyurl.rs` (3.13 `urlsplit` and its `hostname`/`port`/`username`/`password`, `parse_qs`, `unquote`) |

## API
| Python | Rust | Notes |
|---|---|---|
| `KINDS`, `SERVICE`, `HANDLE`, `SECURITY`, `MAX_LABEL_CHARS` | `KINDS`, `SERVICE`, `HANDLE`, `SECURITY`, `MAX_LABEL_CHARS` | `HANDLE` keeps `re.match` semantics: `$` also accepts one trailing newline |
| `TOTP_ALGORITHMS` | `TOTP_ALGORITHMS` (names) + `hmac_digest` | SHA1 / SHA256 / SHA512 |
| `PARTS`, `ALL_PARTS` | `PARTS`, `parts(kind)`, `ALL_PARTS` | same order |
| `class VaultError` | `enum VaultError::Error` | message is the Python message |
| `class Refused` | `VaultError::Refused`, `VaultError::is_refused()` | a subclass in Python; a variant here |
| `default_index` | `default_index()`, `default_index_from(Option<&str>)` | `EVE_VAULT_INDEX`, `~` expanded |
| `_DEFAULT_PORTS` | `default_port(scheme)` | |
| `canonical_origin` | `canonical_origin(&str) -> Result<String>` | same messages (`not a site: '...'` with Python `repr`) |
| `site_allows` | `site_allows(saved, page_url) -> bool` | port 0 is falsy, as in Python |
| `site_of` | `site_of(origin) -> String` | |
| `hotp` | `hotp(key, counter, digits, algorithm) -> Result<String>` | |
| `b32_key` | `b32_key(secret) -> Result<Vec<u8>>` | Python's `b32decode` padding rules |
| `totp` | `totp(secret, at, digits, period, algorithm)`, `totp_default(secret, at)` | CPython float floor division for the counter |
| `parse_totp` | `parse_totp(text) -> Result<Values>` | keys in Python's order: digits, period, algorithm, secret |
| `class Keychain` | `struct Keychain` + `trait KeychainStore` | `Keychain::new(keychain, runner)`, `with_env`, `from_env`; `put`/`get`/`delete` via `KeychainStore` |
| `Keychain.__init__(run=subprocess.run)` | `trait Runner`, `SubprocessRunner` | the seam Python's `run` parameter is; 20 s timeout |
| `Keychain._tail` | `Keychain::tail` | |
| `Keychain.put` | `KeychainStore::put` | `security -i` on stdin, compact ASCII-escaped JSON as lowercase hex; read back and compared |
| `Keychain.get` | `KeychainStore::get -> Result<Option<Values>>` | `bytes.fromhex` whitespace rules |
| `Keychain.delete` | `KeychainStore::delete -> Result<bool>` | |
| (test_vault_fill.py / test_vault_mcp.py `MemKeychain`) | `MemKeychain` | public so the crates porting those tests share one |
| `class Item` | `struct Item` | `line()`, `to_dict()` |
| `_clean_label` | `clean_label` | quotes to `'`, whitespace squashed, 60 characters |
| `_card` | `card` | |
| `class Vault` | `struct Vault` | `new(index, keychain, clock)`, `open_default()`, `with_store`, `with_clock` |
| `Vault.items` | `Vault::items` | skips what Python skips |
| `Vault.get` | `Vault::get` | handle stripped |
| `Vault.lines` | `Vault::lines(limit)`, `LINES_LIMIT` = 40 | |
| `Vault._write` | `Vault::write` | `json.dump(indent=1)` bytes, 0600, `.tmp` then rename |
| `Vault.add` | `Vault::add(kind, label, value, site)` | label checked before the Keychain is touched |
| `Vault.remove` | `Vault::remove -> Result<bool>` | |
| `Vault.fill_values` | `Vault::fill_values(handle, page_url, part) -> Result<(Item, FillValues)>` | `part` "" means every part; same refusal messages |
| `cli` | `cli(argv, vault, ask, ask_secret, out) -> i32`, `cli_main(argv)` | argparse usage/help/errors reproduced; exit 2 where Python raises `SystemExit(2)`, 0 after help |
| `input`, `getpass.getpass` (defaults of `cli`) | `input`, `getpass` | `/dev/tty` with echo off; stdin fallback with Python's warning |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_vault.py` | `test_totp_matches_the_rfc_6238_vectors`, `test_a_2fa_secret_is_read_from_a_uri_or_bare`, `test_a_site_is_the_saved_host_or_its_subdomains_on_the_same_scheme_and_port`, `test_a_secret_never_rides_on_a_command_line_and_the_index_holds_none`, `test_a_fill_is_refused_off_its_site_and_a_card_fills_anywhere`, `test_bad_items_are_refused_before_the_keychain_is_touched`, `test_remove_deletes_the_keychain_item_and_the_index_line`, `test_the_cli_reads_secrets_with_getpass_and_prints_no_value`, `test_live_keychain_round_trip_in_a_throwaway_keychain` | same names in `src/tests.rs` (the live one `#[ignore = "live: ..."]`; run and passing 2026-10-19) | none |
| `bridge/tests/test_vault_fill.py` | `test_the_page_refuses_before_touching_the_page_off_site_or_without_a_vault` (vault side), `test_live_fill_fills_each_form_and_no_value_leaves_the_page` (vault side) | `test_the_page_refuses_before_touching_the_page_off_site_or_without_a_vault`, `a_live_fill_reads_each_kind_from_a_mem_keychain` | `test_a_fill_step_names_a_vault_handle_and_nothing_else`, `test_the_planner_is_told_the_vaults_lines_in_the_browser_only`, `test_the_executor_runs_a_fill_through_the_page_and_stops_on_a_refusal` and the page/Chromium halves test plan_contract, plan_executor and browser_lane: they belong to those crates |
| `bridge/tests/test_vault_mcp.py` (tests_ref) | uses `Vault` + `MemKeychain` only as a fixture | `MemKeychain` provided for its port | every test exercises vault_mcp / codex_computer / vault_native: those crates |
| (parity additions) | | `parse_totp_keeps_pythons_key_order_and_lowercased_query_names`, `b32_key_decodes_like_python`, `sites_match_python_on_odd_inputs`, `the_index_is_written_byte_for_byte_as_python_writes_it`, `the_keychain_blob_is_the_hex_python_writes`, `the_keychain_names_another_keychain_file_and_refuses_a_quoted_one`, `the_index_reader_skips_what_python_skips`, `labels_are_cleaned_as_python_cleans_them`, `a_card_is_normalized_as_python_normalizes_it`, `the_default_index_follows_eve_vault_index`, `the_cli_parses_arguments_as_argparse_does`, plus unit tests in `pystr` and `pyurl` | expected bytes and values were produced by the daemon's Python 3.13; verified 2026-10-19 by a differential run of ~3500 inputs (sites, 2FA secrets and URIs, labels, cards, index files, `repr`, compact JSON) and 83 `cli` argument vectors against Python 3.13.14: every remaining difference is listed under Differences |

## Boundaries
- The Keychain is driven through `/usr/bin/security` as a subprocess, exactly as the Python does (`security -i` with the add command on stdin, `find-generic-password -w`, `delete-generic-password`). This is the Python design, not a port fallback: items `security` creates trust `security`, so both daemons read each other's items without a prompt. The Security framework is deliberately not used, since an item created by the Rust binary would trust that binary instead.

## Differences
- Where Python lets a non-`VaultError` exception escape (a `security` timeout or OSError, an index write failure), Rust returns `VaultError::Os` with the error text; `cli` prints it as `vault: ...` and exits 1, where Python prints a traceback and exits 1.
- A base32 secret whose length leaves bad padding (for example a single character) raises `binascii.Error` in Python, which escapes `cli` as a traceback; Rust refuses it with the vault's own "a 2FA secret is base32 ..." message.
- A Keychain item missing a card or 2FA field (only possible if something other than the vault wrote it) raises `KeyError` in Python; Rust returns `VaultError::Error("\"<label>\" is damaged in the Keychain")`.
- An index whose `items` is a number, `null` or a boolean makes Python's `items()` raise `TypeError`; Rust treats it as an empty vault. A `site_of` on an origin `urlsplit` or its `port` refuses (a malformed port, bad brackets, an NFKC-unsafe host: only from a hand-edited index) raises `ValueError` in Python; Rust shows what it can of the host (or nothing).
- The index is read with serde_json, not Python's `json`: a hand-edited index holding `NaN`, `Infinity` or a number beyond f64 range (`1e400`) is unreadable (an empty vault) where Python reads it, and an integer label beyond 64 bits is shown as a float (`1.2345678901234568e+22`) where Python keeps every digit. The vault itself only ever writes strings there.
- A card expiry made of Unicode digits that are not decimals (`²`) passes Python's `isdigit()` and then makes `int()` raise `ValueError`, a traceback from `cli`; Rust refuses it with "a card needs its expiry month (1-12) and year".
- `parse_totp` on an `otpauth://` URI whose netloc `urlsplit` refuses (bad brackets) lets Python's `ValueError` escape; Rust returns that message as a `VaultError`. Likewise a bad `EVE_VAULT_KEYCHAIN` makes `cli`'s `Vault()` raise outside its `try` in Python (a traceback, exit 1); Rust prints `vault: ...` and exits 1.
- When `getpass` cannot turn echo off, Python also emits a `GetPassWarning` through `warnings` (a line naming getpass.py's source); Rust prints only getpass's own "Warning: Password input may be echoed." line. Everything else (`/dev/tty` with `O_NOCTTY`, `TCSAFLUSH|TCSASOFT`, stdin when there is no `/dev/tty`, the newline after the reply) follows CPython's `unix_getpass`.
- `pystr::repr` decides printability from `unicode-general-category` (Unicode 16) where Python 3.13 uses Unicode 15.1: a character assigned in 16.0 prints raw in Rust and escaped in Python.
- `Path.expanduser` also expands `~user`; Rust expands `~` and `~/...` only.
- argparse wraps its help text to the terminal width; Rust prints the 80-column layout always.
- `EVE_VAULT_KEYCHAIN` and `EVE_VAULT_INDEX` are read where Python reads them (`Keychain()` and `default_index()`), and also have explicit-value variants (`Keychain::with_env`, `default_index_from`) so tests need not mutate the process environment.
