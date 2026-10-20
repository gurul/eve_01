# eve-agentcard
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/agentcard.py` | `src/lib.rs`; Python-behavior helpers (`json.dumps`, `urlsplit`, `float()`, `expanduser`, lookaround `re.sub`) in `src/py.rs`; tests in `src/tests.rs` |

## API
| Python | Rust | Notes |
|---|---|---|
| `API_ORIGIN`, `OFF_REASON`, `TOOL_NAMES` | same `pub const`s | Strings verbatim; `TOOL_NAMES` is a `[&str; 4]` |
| `_HASH`, `_ID`, `_STAGES`, `_BUY_STATUSES`, `_CHARGES`, `_ADDRESS_FIELDS` | `HASH`, `ID`, `STAGES`, `BUY_STATUSES`, `CHARGES`, `ADDRESS_FIELDS` | `\A…\z` anchors, as `fullmatch` / `\Z` |
| `_address` | `address(&Value) -> Result<Map, AgentCardError>` | Same checks; `\d` is Unicode in both |
| `AgentCardConfig` | `AgentCardConfig { enabled, token, timeout_secs, address_json }` | `Debug` prints `AgentCardConfig(enabled=True, timeout_secs=180.0)`: token and address hidden like `repr=False` |
| `configured` | `configured(Option<&HashMap>)`, `configured_from(Fn(&str) -> Option<String>)` | Same env names, truthy set, `float()` parsing (ASCII and Unicode whitespace, Unicode decimal digits, `_`), ≥ 120 s, address file `expanduser`, stored as `json.dumps(sort_keys=True)`; errors are `AgentCardError` with the Python `ValueError` texts |
| `make_agentcard` | `make_agentcard(Option<&HashMap>) -> Option<AgentCard>` | Logs `agentcard: invalid configuration; staying off` |
| `_Snapshot` | `Snapshot` (opaque, wraps `Arc<SnapshotData>`) | Not `Clone`; `confirm` accepts only the instance handed out (`Arc::ptr_eq`, Python's `is`); `Debug` redacted |
| `_StateError` | `StateError` | Every `OSError`/`ValueError`/`TypeError`/`KeyError` inside a locked section maps to it |
| `_failure` | `failure`, `failure_with` | Key order `ok`, `reason`, extras |
| `_integer` | `integer(Option<&Value>, min)` | JSON integers only (no bool, no float), ≤ 10**12 |
| `AgentCard` | `AgentCard::new(config, Option<Arc<dyn Transport>>, Option<PathBuf>)` | Blocking like Python; async callers use `spawn_blocking` (Python: `asyncio.to_thread`). `Sync`: one instance serves concurrent workers |
| `AgentCard.close` | `close()` | |
| `AgentCard._http_request` | `HttpTransport` (`Transport` impl) | reqwest per call, no redirects, `no_proxy()` (httpx `trust_env=False`), connect/read timeouts = `timeout_secs`, no idle pool. Blocking; called inside a tokio runtime it runs on its own thread instead of panicking |
| (`request=` seam) | `Transport` trait, `HttpRequest`, `HttpResponse` | Closures implement `Transport` |
| `AgentCard.tools` | `tools() -> Vec<Value>` | Schemas verbatim |
| `AgentCard.instructions` | `AgentCard::instructions()` | Text verbatim |
| `AgentCard._text` | `text()` | Token (case-insensitive), address and card brand/last4 with `(?<!\w)…(?!\w)` semantics (`py::sub_word`, retrying one char later like `re`), `Bearer` scrub, control-char filter, char limit |
| `AgentCard._fields` | `fields()` | Same opaque-key set |
| `AgentCard._url` | `url()` | `py::urlsplit` reproduces Python 3.12's checks (bracket/IPv6/IPvFuture, NFKC netloc, port `ValueError`) |
| `AgentCard._cart`, `_carts` | `cart()`, `carts()` | Same validation and conflict detection (map equality is order-insensitive like dict) |
| `AgentCard._project`, `_receipt` | `project()`, `receipt()` | Same key order as Python dict insertion |
| `AgentCard._call` | `call()` | `(status, body)` or `(None, None)`; failures carry no text. The body is decoded as `json.loads(bytes)` does (`py::json_loads_bytes`: UTF-8 with or without BOM, UTF-16/32 by BOM or zero-byte position) |
| `AgentCard._locked` | `locked(closure)` | Process mutex (2 s), `mkdir(mode=0o700, parents=True)` (only the leaf `0700`), symlink refusals, `O_NOFOLLOW` `state.lock` `0600`, non-blocking `flock` (`File::try_lock`) |
| `AgentCard._read_state` | `read_state()`, `parse_state()` | Exact schema validation; 4 MB cap; `chmod 0600` |
| `AgentCard._write` | `write()`, `write_file()` | `json.dumps(sort_keys=True)` bytes, token check, `.state-` temp file `0600`, fsync, atomic rename, directory fsync |
| `AgentCard._default_address` | `default_address()` | |
| `AgentCard._digest` | `digest()` | sha256 of `json.dumps(sort_keys=True)` (ASCII-escaped); verified equal to Python's for the same cart/address |
| `AgentCard._bound_address`, `_new_conversation`, `_update` | `bound_address()`, `new_conversation()`, `update()` | |
| `AgentCard._ledger_safe`, `_vault_pause` | `ledger_safe()`, `vault_pause()` | |
| `AgentCard._read_ledger`, `_save_ledger` | `read_ledger()`, `save_ledger()` | |
| `AgentCard.handle` | `handle(&str, &Value) -> Value` | Same reasons and branch order |
| `AgentCard._merchants` | `merchants()` | |
| `AgentCard._binding` | `binding()` | |
| `AgentCard.prepare_confirm` | `prepare_confirm(&Value, &Value) -> Prepared` | `Prepared { result, snapshot }`: the dict without the snapshot object, plus the snapshot |
| `AgentCard._preview` | `preview()` | Byte-identical question text (checked against Python output) |
| `AgentCard.confirm` | `confirm(&Snapshot, Option<&AtomicBool>) -> Value`, `try_confirm(...) -> Result<Value, ConfirmRaised>` | `cancelled` is the worker's flag (`threading.Event`). Python's one escaping exception (a Vault retry whose fresh ledger read failed: `_failure(..., **ledger_failure)` passes `reason` twice, `TypeError`, before any claim or POST) is `Err(ConfirmRaised)` from `try_confirm`; the owner-approval gateway uses `try_confirm`, so it answers "outcome unknown" as Python's does |
| `AgentCard._checkout_result` | `checkout_result()` | Same stage ladder |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_agentcard.py` | all 37 (parametrized cases looped inside each Rust test) | same names in `src/tests.rs` | — `caplog` checks become "the error text holds no token"; `test_default_transport_…` checks fixed origin, bearer and timeout through a recording transport and no-redirect against a local socket (env-proxy isolation is `no_proxy()` by construction: tests must not mutate the process environment) |
| `bridge/tests/test_telegram_agentcard.py` | `test_real_client_through_chat_waits_for_owner_then_sends_exact_vault_request` (client half) | same name | The inlet, owner question, buttons and daemon factory belong to the telegram / agentcard_tools / daemon ports |
| (Rust only) | | `state_file_bytes_match_python_json_dumps_sort_keys`, `python_written_state_is_read_back`, `test_preview_matches_python_text_exactly`, `text_redacts_token_address_card_and_bearer`, `address_validation_matches_python`, `vault_retry_with_unavailable_ledger_raises_before_dispatch`, `state_directory_parents_keep_the_default_mode`, `default_transport_called_inside_a_runtime_blocks_instead_of_panicking`, `python_strip_removes_ascii_separators`, `py::tests::*` | |
| (differential) | | `differential_cases_recorded_from_python_match` replays `tests/data/differential.jsonl`: 802 cases recorded from `agentcard.py` (`_project`, `_checkout_result`, `_ledger_safe`, `_vault_pause`, `_url`, `_text`, `configured`, `_address`), compared byte for byte including key order. `differential_cases_from_file_match` (ignored) replays a larger file named by `AGENTCARD_DIFF_CASES`; a 6000-case set passed at verification | |

Python `monkeypatch` of `client._write` became test-only `WriteHooks` (`fail`, `after`); `client._request` replacement became the test server's `fail_when`.

## Boundaries
None.

## Differences
- `confirm` cannot receive a non-snapshot (the type system enforces it), so `invalid_confirmation_snapshot` is unreachable.
- State validation requires `address_digest` and `shopping_pending` to be JSON strings; Python's `str(value)` would also accept a 64-digit JSON integer. Python never writes one.
- When `address_json` is malformed (only possible by constructing the config by hand), `agentcard_merchants` returns `merchants_unavailable` where Python raised.
- Secrets of equal length are redacted in a fixed order (Python's set order is arbitrary).
- `confirm` (the `Value` form) answers `vault_retry_requires_reconciliation` with `status: uncertain` where Python raised `TypeError`; `try_confirm` keeps the exception.
- Provider bodies Python's `json` accepts but `serde_json` refuses read as no answer (`(None, None)`): `NaN`/`Infinity` literals, floats beyond `f64`, lone surrogates, nesting deeper than 128.
- `\w` for address redaction is Rust's `char::is_alphanumeric` plus `_`; Python's `str.isalnum` differs for a few combining marks (`Other_Alphabetic`).
