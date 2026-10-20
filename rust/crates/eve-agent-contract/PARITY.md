# eve-agent-contract
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/agent_contract.py` | `src/lib.rs` |

## API
| Python | Rust | Notes |
|---|---|---|
| `AgentEvent` | `AgentEvent` (`new`, `with_text`, `with_turn`, `kind()`, `text()`, `turn()`) | Frozen: private fields, accessors only. `kind: String`, `text: String = ""`, `turn: i64 = 0`. Value `Eq`/`Hash` like the frozen dataclass. Serde shape `{"kind","text","turn"}` (as `dataclasses.asdict`), missing `text`/`turn` default. The docstring's kinds are `kind::*` constants plus `kind::ALL`; `kind` stays a free string as in Python |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| (none in `tests`) | - | 7 unit tests in `src/lib.rs`: defaults, positional construction, value equality/hash, unknown kind, documented kinds, serialization, deserialization defaults | - |
| `bridge/tests/test_app_reflex.py` (ref) | `test_agent_event_has_one_definition` | covered by design: one type in this crate, which `eve-computer-agent` re-exports | Identity check belongs to the computer-agent crate's port |
| `bridge/tests/test_chrome_lane.py`, `test_codex_chat.py`, `test_web_reader.py` (ref) | use `AgentEvent` as a value | `positional_construction_like_chrome_lane_tests` | The rest tests those crates |

## Boundaries
None

## Differences
None. Python's dataclass would accept a non-str `kind`/`text` or non-int `turn` at runtime; Rust types them as the annotations say.
