# eve-openrouter
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/openrouter.py` | `src/lib.rs` |

## API
| Python | Rust | Notes |
|---|---|---|
| `KEY_ENV`, `BASE_URL`, `ANTHROPIC_BASE_URL`, `PROVIDER`, `HEADERS`, `PROVIDER_ORDER` | same-named `pub const`s | Values verbatim. `HEADERS` is an ordered array of pairs. |
| `_OPENAI`, `_CLAUDE_VERSION` | private `LazyLock<Regex>` `OPENAI`, `CLAUDE_VERSION` | Same patterns, case-insensitive. |
| `class NoKey(RuntimeError)` | `struct NoKey` (thiserror), wrapped as `Error::NoKey` | Message `OPENROUTER_API_KEY is not set`. |
| `provider_prefs(model_id)` | `provider_prefs(&str) -> Option<Value>` | `{"order": [...], "allow_fallbacks": true}` or None. |
| `key(env=None)` | `key(Option<&HashMap<String,String>>) -> String` | None reads the process environment; stripped. |
| `available(env=None)` | `available(Option<&HashMap>) -> bool` | |
| `model(name)` | `model(&str) -> String` | Same branches: prefixed ids kept, bare OpenAI names get `openai/`, Claude names `anthropic/` with a dotted version. |
| `_rewrite(options)` | `rewrite(&mut Value) -> bool`, plus `rewrite_parts(&mut [Part])` for multipart | Idempotent; run on every attempt. |
| `class _Rewrites` | the private `Core::prepare`, called by every send of `Client` / `BlockingClient` | Rust has no SDK class to mix into: the client itself applies the rewrite on each request (JSON and multipart). |
| `_key_or_raise(api_key)` | `key_or_raise(Option<&str>, Option<&HashMap>) -> Result<String, NoKey>` | A blank argument falls back to the environment key. |
| `_merge_headers(kwargs)` | `merge_headers(&[(String,String)]) -> Vec<(String,String)>` | `{**HEADERS, **default_headers}`: the caller wins. |
| `_cls(name, base)` | (no equivalent needed) | Cached SDK subclasses with the rewrite mixed in; in Rust the `Client` / `BlockingClient` types are those classes. |
| `client(api_key=None, **kwargs)` | `client(Option<&str>, ClientOptions) -> Result<BlockingClient>` | Blocking OpenAI-shaped client on `BASE_URL`, Bearer auth. |
| `async_client(api_key=None, **kwargs)` | `async_client(Option<&str>, ClientOptions) -> Result<Client>` | Async OpenAI-shaped client. |
| `anthropic_client(api_key=None, **kwargs)` | `anthropic_client(Option<&str>, ClientOptions) -> Result<Client>` | Async Anthropic-shaped client on `ANTHROPIC_BASE_URL`; `x-api-key` + `anthropic-version: 2023-06-01`; `messages_create` posts to `/v1/messages`. |
| (SDK `**kwargs`: `timeout`, `max_retries`, `default_headers`, `http_client`) | `ClientOptions` | Defaults as the SDKs': 600 s timeout, 2 retries. |
| (SDK resources) | `Client::{responses_create, responses_stream, chat_completions_create, chat_completions_stream, embeddings_create, audio_transcriptions_create, audio_speech_create, messages_create, messages_stream, post, post_bytes, post_multipart, post_raw, post_stream, get}` and the blocking equivalents | The SDK surface the call sites use, over free-form JSON bodies. |
| (SDK retry policy) | private `should_retry`, `retry_delay` | `x-should-retry`, then 408/409/429/5xx; `retry-after-ms` / `retry-after` within 0-60 s, else 0.5 s doubling to 8 s with up to 25% jitter. |
| (SDK `APIStatusError` etc.) | `Error::{Status, Http, Json, Invalid}` | `Status` displays as `Error code: <status> - <body>`. |
| (SDK SSE stream) | `sse_events`, `SseEvent` | Ends at `data: [DONE]`. |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_openrouter.py` | `test_model_ids_get_their_openrouter_prefix`, `test_the_key_gate_reads_only_the_openrouter_key`, `test_no_client_is_built_without_the_key`, `test_every_request_goes_to_openrouter_with_the_prefixed_model`, `test_a_multipart_transcription_gets_the_prefixed_model_too`, `test_the_async_client_points_at_openrouter`, `test_the_anthropic_client_is_on_openrouter_with_the_dotted_opus_id`, `test_the_guard_catches_a_known_positive_control`, `test_no_production_module_reads_the_openai_or_anthropic_key_except_the_allowed`, `test_google_models_prefer_ai_studio_and_keep_fallbacks` | same names in `tests/test_openrouter.rs`, plus `test_model_ids_edge_cases_match_python`, `test_the_rewrite_touches_only_a_string_model`, `test_the_blocking_client_sends_the_prefixed_model`, `test_a_non_2xx_reply_is_a_status_error_after_the_retries`, `test_a_stream_yields_each_event_until_done`, `test_the_spend_provider_and_constants_are_python_s`; unit tests in `src/lib.rs` for the retry policy, SSE parsing and form flattening | `test_the_brain_and_the_app_builder_build_their_clients_here` belongs to `computer_agent`'s crate. The `websearch.request` half of `test_google_models_prefer_ai_studio_and_keep_fallbacks` belongs to the websearch crate. The guard scans the Rust workspace's `crates/*/src` instead of the Python package; it checks an allowed module's exception only once that module is ported, and catches direct hosts and key names (Rust has no SDK constructor to catch by name). |

## Boundaries
None

## Differences
- Rust has no openai / anthropic SDK, so the clients are this crate's own reqwest clients with the SDKs' base URLs, auth headers, defaults and retry policy; bodies and replies are `serde_json::Value`.
- `ClientOptions::base_url` exists only so tests can point a client at a local server (the Python tests used a mock transport); `ClientOptions::env` lets a test supply the environment instead of mutating the process's.
- `key` / `available` take `Option<&HashMap>` where Python takes any mapping.
