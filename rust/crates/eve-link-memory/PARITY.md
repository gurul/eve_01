# eve-link-memory
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/link_memory.py` | `src/lib.rs` (Python semantics it relies on in `src/py.rs`) |

## API
| Python | Rust | Notes |
|---|---|---|
| `MAX_QUERY_CHARS`, `MAX_TOKEN_CHARS`, `MAX_REMEMBER_CHARS`, `MAX_HIDE_CHARS` | same-named `pub const` (`usize`) | Values verbatim (200, 64, 500, 500), counted in characters as Python slices. |
| `OFF_LINE` | `OFF_LINE` | Verbatim. |
| `ACTIONS` | `ACTIONS: [&str; 8]` | Same order. |
| `MemoryPanel` | `MemoryPanel` | `MemoryPanel(getter)` is `MemoryPanel::new(|| Option<Arc<dyn MemoryApi>>)`; `MemoryPanel::with_getter(Getter)` for a getter that can fail (raise). |
| `MemoryPanel.available` (property) | `MemoryPanel::available()` | A getter that fails counts as unavailable, as the Python's `except Exception`. |
| `MemoryPanel.panel` (async) | `async MemoryPanel::panel(&Value) -> anyhow::Result<Value>` | Same branching, lines and JSON key order. Each memory call runs on `spawn_blocking` (`asyncio.to_thread`). `Err` is the exception the Python raises (getter, memory call, `int()`/`dict()`/iteration of an odd answer); the controls route turns it into its 500. |
| `MemoryPanel._forgotten` (staticmethod) | `MemoryPanel::forgotten(&Value)` | Public so the route and tests can reach it. |
| `_layers` | `layers(Option<&Value>)` | Same names map and order; anything but a dict with a truthy value is "memory". |
| (duck-typed `memory.Memory`) | trait `MemoryApi` | `profile`, `search(query)`, `forget_preview`, `forget_apply`, `star`, `hide`, `unhide`, `hidden`: the eight `Memory` methods the card calls, blocking, returning `anyhow::Result`. The memory port implements it. |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_link_memory.py` | `test_off_memory_is_unavailable_and_says_so`, `test_the_profile_is_the_default_action`, `test_a_search_returns_the_three_groups_without_the_lines_around_a_hit`, `test_forget_is_a_preview_with_counts_then_the_token_once`, `test_remember_stars_one_claim_and_says_so`, `test_an_unknown_action_and_an_overlong_query_are_bounded`, `test_hide_unhide_and_the_hidden_list`, `test_hide_needs_words_and_is_bounded` | same names in `tests/link_memory.rs` (caplog becomes a `tracing_subscriber` writer into a buffer) | None |
| `bridge/tests/test_link_chat.py` (tests_ref) | `test_controls_list_what_this_mac_has`, its Memory-card lines | `test_controls_list_memory_only_while_built` | The HTTP route and the rest of the listing belong to the link-chat crate. |
| (new) | | `a_getter_that_fails_is_unavailable_and_the_panel_raises`, `a_falsy_action_is_the_profile_and_a_non_string_one_is_unknown`, `a_memory_without_hiding_raises_out_of_the_panel`, `the_token_is_stripped_and_cut_and_a_number_reads_as_its_str`, `the_memorys_odd_answers_read_as_the_python_reads_them`, `layers_names_what_a_forget_touches`; `src/py.rs` unit tests for `str()`, `int()`, `split()`, iteration and `dict()` | |

## Boundaries
None

## Differences
- The memory object is a `MemoryApi` trait object instead of any object with those methods. A memory without
  `hide`/`unhide`/`hidden` (an `AttributeError` in Python) is an implementation that returns `Err` from them; the
  panel then raises the same way.
- The Python `MemoryPanel.panel` raises; the Rust one returns `Err`. A panic inside a blocking memory call is also
  an `Err` (the Python thread would have raised).
- Log lines go through `tracing` with the crate's module target instead of the `eve_bridge.link_memory` logger;
  the messages are the same and still carry counts, never the words.
