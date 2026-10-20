# eve-memory-bus
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/memory_bus.py` | `src/lib.rs` (tests in `src/tests.rs`) |

## API
| Python | Rust | Notes |
|---|---|---|
| `DEFAULT_ROSBRIDGE_HOST`, `DEFAULT_ROSBRIDGE_PORT` | same consts | `127.0.0.1`, `9090` |
| `_ON` | private `ON` | `1 true yes on` |
| `BusConfig` (frozen dataclass) | `struct BusConfig` (`Default`, `PartialEq`) | port is `i64`, as Python `int` (no u16 range check, like Python) |
| `configured(environ=None)` | `configured(Option<&HashMap<String,String>>)` | `None` reads the process env; same strip/lower/flag rules, mirror defaults to `claude_mem`, same warning text with `%r` repr of the bad port |
| `TOPICS` | `TOPICS: &[(&str,&str)]` + `topics_map()` (ordered) | verbatim, same order |
| `SERVICES` | `SERVICES` + `services_map()` | verbatim |
| `RECENT_PER_TOPIC` | `RECENT_PER_TOPIC` | 20 |
| `Callback` | `enum Callback { Sync, Async }` | Python distinguishes by "returns a coroutine"; Rust by variant |
| `ServiceHandler` | `type ServiceHandler` | `Fn(Msg) -> BoxFuture<anyhow::Result<Msg>>` |
| `Event` | `struct Event { topic, msg, time }` | `time` is epoch seconds `f64` from the clock |
| `Subscription` | `struct Subscription { topic, callback, id }` | |
| `MemoryBus` | `MemoryBus` (`Clone`, shares one `Arc<Inner>`) | |
| `MemoryBus.__init__(clock)` | `MemoryBus::new()` / `with_clock(f)` | default clock is wall time |
| `MemoryBus.dropped` | `dropped()` | atomic counter |
| `MemoryBus.bind_loop` | `bind_loop(tokio::runtime::Handle)` | a guard task flips a `closed` flag when the runtime shuts down (Python's `loop.is_closed()`) |
| `MemoryBus.subscribe` | `subscribe`, `subscribe_fallible`, `subscribe_async`, `subscribe_callback` | prefix patterns end in `*`, same match rule (`pattern != topic`) |
| `MemoryBus.unsubscribe` | `unsubscribe(&Subscription)` | like Python, leaves (or creates) an empty list under the topic |
| `MemoryBus.register_service` | `register_service(name, async fn)` | |
| `MemoryBus.has_service` | `has_service` | |
| `MemoryBus.call_service` | `call_service -> Result<Msg, ServiceError>` | `KeyError("no such service: X")` → `ServiceError::NoSuchService`, same text; a handler error → `ServiceError::Failed` |
| `MemoryBus.publish` | `publish(topic, Msg) -> Event` | records into the ring, exact subscribers first then prefix patterns in insertion order; inline on the bound runtime, otherwise spawned onto it; inline when unbound or closed |
| `MemoryBus._deliver` | private `Inner::deliver` | an `Err` or a panic from a sync subscriber logs `memory bus: subscriber failed on <topic>` and the rest still run; async futures spawn on the live bound runtime, otherwise are dropped and counted |
| `MemoryBus.recent(topic, n)` | `recent(topic)`, `recent_n(topic, isize)` | `recent_n` keeps Python's `[-n:]` slice semantics (0 = all) |
| `MemoryBus.topics` / `services` | `topics()` / `services()` | fresh ordered maps |
| (reads `bus._subs` in test_rosbridge.py) | `subscription_count()` | total live subscriptions |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `tests/test_memory_bus.py` | all 8 | same names in `src/tests.rs` (the raise test also covers a panicking subscriber) | none |
| `tests/test_daemon_membus.py` | `test_everything_is_off_by_default`, `test_env_flags_and_mirror_default`, `test_nothing_said_in_a_conversation_is_published` (catalogue part) | same names | the daemon wiring tests belong to the daemon crate |
| `tests/test_follow.py` | the `/eve/presence` catalogue check | `test_nothing_said_in_a_conversation_is_published` | the rest tests follow.py |
| `tests/test_rosbridge.py` | bus subscription counting | `subscription_count_tracks_subscribe_and_unsubscribe` | protocol tests belong to the rosbridge crate |
| `tests/test_claude_mem.py`, `tests/test_privacy_guards.py` | use the bus only as a fixture | — | covered by the bus tests above; the sinks are other crates |
| (extra) | | `a_literal_star_topic_reaches_its_exact_subscriber_once`, `publish_on_the_bound_loop_delivers_inline`, `an_async_subscriber_without_a_loop_is_dropped_and_counted`, `a_closed_loop_delivers_inline_and_drops_async_subscribers`, `configured_strips_and_lowercases_like_python`, `python_int_matches_int`, `py_repr_quotes_like_python`, `a_subscriber_can_unsubscribe_itself_while_being_delivered` | |

## Boundaries
None

## Differences
- "On the loop" means "on a thread of the bound tokio runtime" (compared by runtime id); with a
  multi-thread runtime that is any of its worker threads rather than one loop thread.
- Python's subscriber "raises" maps to a sync subscriber returning `Err` (`subscribe_fallible`) or
  panicking; the panic payload is logged as the error field. A panic inside a spawned async
  subscriber is swallowed by tokio, as an unretrieved task exception is in asyncio.
- `EVE_ROSBRIDGE_PORT` parsing follows `int()` for ASCII digits, sign and underscores; non-ASCII
  Unicode digits and values beyond `i64` fall back to 9090 with the warning, where Python accepts them.
- Service handlers return a JSON object (`Msg`), as the Python type says; rosbridge's
  non-dict-values branch cannot arise.
