# eve-app-reflex
Status: complete

The launch and quit reflex in front of whichever computer-use agent the daemon builds, plus the desk lane and the
body router seams. `cargo test -p eve-app-reflex`: 38 tests pass.

## Modules
| Python | Rust |
|---|---|
| app_reflex.py | src/lib.rs (tests in src/tests.rs) |

## API
| Python | Rust | Notes |
|---|---|---|
| reflexes_on | reflexes_on(environ) | `Environ` = `Option<&HashMap>`; `None` reads the process environment. Unset/empty is `task_router::REFLEX_DEFAULT`; "0", "false", "no", "off" (stripped, lower-cased) turn it off |
| LINK, REFERS_BACK | LINK, REFERS_BACK | same patterns, case-insensitive |
| RECENT_OWNER_MESSAGES, MAX_REFERENCED_LINKS | same | 4, 3 |
| links_in | links_in | trailing `.,;:!?)]}` stripped, deduplicated, in order |
| refers_back | refers_back | |
| with_referenced_links | with_referenced_links(goal, impl IntoIterator<Item: AsRef<str>>) | same note text, newest link first, last 4 messages |
| goal_shape | goal_shape | word count is Python `split()`; hosts are `urlsplit(u).hostname` (user info and port dropped, lower-cased; the part after an IPv6 zone `%` keeps its case), `?` when empty |
| QUIT_ALL_KEEP | QUIT_ALL_KEEP ([&str; 14]) | same 14 names |
| QUIT_SETTLE_SECS, QUIT_POLL_SECS | same | 2.0, 0.25 |
| quit_keep | quit_keep(environ) -> HashSet<String> | `EVE_QUIT_ALL_KEEP` comma list, stripped and case-folded (`eve_desktop_lanes::casefold`) |
| jev_quit_asker | jev_quit_asker(environ) -> Option<Asker> | `EVE_ROUTER_MODEL=jev` only; `jev::route_config` + `jev::make_predict(timeout_s=2.0)` + `typed_ask::make_jev_quit_asker(…, perf_counter)`; a route error logs "app-reflex: Jev quit is off (JevError)" and returns None |
| _osascript | trait Osascript::osascript, RealOsascript | `osascript -e line …`; (ok, stdout if ok else stderr), lossy UTF-8, stripped. The trait is the seam the Python tests monkeypatched |
| _quote | quote | AppleScript string literal, `\` then `"` escaped |
| _running | Osascript::running (default method) | `return application "X" is running` == "true" |
| _gone | gone (private) | polls every QUIT_POLL_SECS until all quit or `settle` passes; tokio sleep |
| quit_app | quit_app(name, settle), quit_app_with(&dyn Osascript, …) | same four sentences; error text cut to 120 characters |
| quit_all | quit_all(keep, settle), quit_all_with(&dyn Osascript, …) | same System Events query, same sentences ("stable" shown as "Warp", Finder never listed as kept) |
| jev_asker | jev_asker(environ) -> Option<Asker> | as jev_quit_asker with `make_jev_request_asker`; logs "app-reflex: Jev is off (JevError)" |
| open_app | open_app(app) -> Result<(bool, String)> | `open -a app`, stdout to /dev/null, stderr stripped and cut to 200 characters |
| ReflexFirstAgent | ReflexFirstAgent::new(make_inner, on_event, ReflexOptions) | keyword arguments are `ReflexOptions` (Default = Python's defaults: installed_apps, open_app, quit_app, `quit_all(quit_keep())` read when it runs, provider "codex", enabled true) |
| ReflexFirstAgent.running | ReflexFirstAgent::running() | reflex/lane working, or the wrapped agent's `running` |
| ReflexFirstAgent._choose_body | choose_body (private) | router asked only when `make_auto` or `bodies` is set; a router error logs "app-reflex: the body router failed (…); Codex takes it" |
| ReflexFirstAgent.cancel | cancel(reason) | before the handoff only records the reason; after it forwards |
| ReflexFirstAgent.steer | steer(text) | false before the handoff |
| ReflexFirstAgent.status | status() -> Map | `{"running","provider","goal":"","reflex"}` before the handoff (`reflex` null until something was reflexed), else the wrapped agent's |
| ReflexFirstAgent.__getattr__ | inner() -> Option<Arc<dyn Agent>>, Agent::as_any | the wrapped agent itself, to downcast for `browser_used`, `final` … |
| ReflexFirstAgent._plan | plan (private) | `apps()` and `task_router::classify(goal, "", apps, asker, quit_asker)` each on `spawn_blocking` (Jev is a blocking HTTP call) |
| ReflexFirstAgent.run | run(goal, note) -> anyhow::Result<String> | same order: quit/launch reflex, cancel check, desk lane (complete with a sentence answers; partial prepends `desk_lane::codex_note`), cancel check, body choice, cancel check with `discard` + `on_done`, then the body runs `goal + note` and `on_done` follows. Same log lines |
| (attributes body, provider, reflexed) | body(), provider(), reflexed() | behind one `parking_lot::Mutex` with the inner agent and the cancel reason, so a Stop and the handoff cannot interleave |
| (the wrapped agent's duck type) | trait Agent | `run`, `cancel`, `steer`, `status` required; `running` (false), `provider` (""), `discard` (no-op) default as Python's `getattr(..., default)` reads; ReflexFirstAgent implements it too |
| (desk=DeskLane.attempt) | desk_from_lane(DeskLane) -> Desk | |
| `type(e).__name__` in the logs | exception_name | JevError, PredictFailure/PredictError/LaneError kinds, io::Error kinds mapped to the OSError subclasses, else "Exception"; never the message |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| test_app_reflex.py | all 18 | same names in src/tests.rs (test_switches also covers jev_quit_asker) | test_agent_event_has_one_definition checks computer_agent's re-export; here it checks this crate's `AgentEvent` is eve-agent-contract's |
| test_quit_reflex.py (ref) | test_the_wrapper_quits_without_codex, test_quit_all_keeps_eves_own_and_the_terminals | same names | the classify / typed_ask cases belong to eve-desktop-lanes and eve-typed-ask |
| test_desk_lane.py (ref) | the seven ReflexFirstAgent rig tests | same names | DeskLane itself is eve-desk-lane's; Daemon._desk is eve-daemon's |
| test_browser_router.py (ref) | test_the_wrapper_sends_a_routed_task_to_its_named_body_and_keeps_reflexes_first, test_named_bodies_alone_are_enough_and_a_broken_router_keeps_codex | same names | the router itself is eve-browser-router's |
| (added) | - | test_the_switches_parse_like_python, test_links_are_capped_and_deduplicated, test_goal_shape_names_hosts_never_words, test_quit_all_sentences, test_quit_app_sentences, test_quote_and_the_keep_list, test_a_quit_that_fails_falls_back_to_the_inner_agent, test_a_router_without_a_body_to_choose_is_never_asked, test_status_before_any_handoff_and_steer_without_a_body | expected strings checked against the Python |

The tests that use `browser_lane.is_web_goal` and `telegram.TASK_FILE_HINT` carry test-local copies of
`WEB_WORDS` and the hint text, since those crates sit downstream of this one.

## Boundaries
`osascript` and `open -a` are subprocesses, as in Python.

## Differences
- `goal_shape` never fails: Python's `urlsplit` raises `ValueError("Invalid IPv6 URL")` for a link with an unbalanced
  or non-IPv6 bracket (`http://[::1]` loses its `]` to the trailing-punctuation strip), which made `run` raise after
  choosing the body and before running it. Rust logs the host as it reads it and runs the task.
- The wrapped agent is a typed `Agent` trait, so `steer` and `status` must exist where Python would raise
  `AttributeError` on an agent that lacked them at call time.
- A panic inside `apps()` or `classify` (on its blocking thread) is caught as Python's `except Exception` catches a
  raised one: the reflex is skipped and the body takes the goal.
