# eve-web: watcher unit
Status: complete

watch.py lines 2023-2818 (`Watcher`: the store, the readers, one check, the loop, the chat tools) and 2932-2941
(`make_watcher`), in `src/watch/watcher.rs` (store, readers, check, loop) and `src/watch/chat.rs` (the chat tools, a
second `impl Watcher`).

## API
| Python | Rust | Notes |
|---|---|---|
| watch.Notify | watch::watcher::Notify | always called with `about` (the text itself when the caller had none) |
| watch.Watcher | watch::watcher::Watcher, WatcherDeps | the keyword arguments are `WatcherDeps` (`WatcherDeps::new(ctx)` is Python's defaults); `evaluate` is a new seam (see Differences) |
| watch.Watcher.__init__ | Watcher::new | |
| watch.Watcher.watches | Watcher::watches (shared handles), snapshot (copies), push (`watches.append`) | a watch is a `WatchRef` (`Arc<Mutex<Watch>>`); identity is `Arc::ptr_eq` |
| watch.Watcher.load_problem | Watcher::load_problem | |
| watch.Watcher._load | Watcher::load | the file is read by `watcher::pyjson::loads` (Python's json.loads: NaN, Infinity, huge numbers, lone surrogates) |
| watch.Watcher.save | Watcher::save | `watcher::pyjson::dumps_indent1`: `json.dump(indent=1)`, ensure_ascii, floats by repr |
| watch.Watcher.host | Watcher::host | |
| watch.Watcher._next | Watcher::next | |
| watch.Watcher._call | Watcher::call | `spawn_blocking` when `offload`, else inline; a panic in the call goes on unwinding in the caller |
| watch.Watcher.read | Watcher::read | |
| watch.Watcher._read_quote | Watcher::read_quote | |
| watch.Watcher._model_budget | Watcher::model_budget | |
| watch.Watcher._missing | watch::watcher::missing | |
| watch.Watcher._merge | watch::watcher::merge | |
| watch.Watcher._read_page | Watcher::read_page | |
| watch.Watcher._crawl_on | Watcher::crawl_on | |
| watch.Watcher._read_crawled | Watcher::read_crawled | |
| watch.Watcher._fetch_page | Watcher::fetch_page | |
| watch.Watcher._from_html | Watcher::from_html | |
| watch.Watcher._ask_text | Watcher::ask_text | |
| watch.Watcher._read_rendered | Watcher::read_rendered | |
| watch.Watcher._read_search | Watcher::read_search | |
| watch.Watcher._tm_get | Watcher::tm_get | TICKETMASTER_API_KEY from the watcher's ctx |
| watch.Watcher._read_ticketmaster | Watcher::read_ticketmaster | |
| watch.Watcher.check | Watcher::check | |
| watch.Watcher._tell | Watcher::tell | |
| watch.Watcher._kept | Watcher::kept | public (the tests' seam) |
| watch.Watcher._run_one | Watcher::run_one | public (Python's tests call it) |
| watch.Watcher.tick | Watcher::tick | |
| watch.Watcher.run | Watcher::run | |
| watch.Watcher.kinds | Watcher::kinds | |
| watch.Watcher.instructions | Watcher::instructions | |
| watch.Watcher.tools | Watcher::tools | |
| watch.Watcher.handle | Watcher::handle | |
| watch.Watcher._find | Watcher::find | |
| watch.Watcher._no_such | Watcher::no_such | |
| watch.Watcher.pause | Watcher::pause | |
| watch.Watcher.resume | Watcher::resume | |
| watch.Watcher.set_end | Watcher::set_end | |
| watch.Watcher._summary | Watcher::summary | |
| watch.Watcher.remove | Watcher::remove | |
| watch.Watcher._new_id | Watcher::new_id | |
| watch.Watcher.validate | Watcher::validate | `Result<Watch, String>` for Python's `(watch or None, why)` |
| watch.Watcher.add | Watcher::add | |
| watch.Watcher.listing | Watcher::listing | |
| watch.make_watcher | watch::watcher::make_watcher | tls = `tls::tls_request`, crawl = `hosted::fetch_hosted` over `Ctx::global()` |

## Tests
All in `tests/watcher.rs` (plus unit tests in watcher.rs and chat.rs for the Python helpers: urlsplit, quote,
urlencode, `str()`, `repr()`, json.loads / json.dump(indent=1), and that every Watcher future is Send).

| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| test_watch.py | 29 | 29, same names, plus test_make_watcher_builds_the_daemons_watcher_when_on and test_watch_list_and_an_unknown_tool | none |
| test_watch_lean_scheduler.py | 6 | 6, same names (offload on, a gated fetch held mid-flight while the owner's tool call runs, as in Python) | none |
| test_watch_reverify.py | 34 | 29 (2 `#[ignore = "live: ..."]`: test_negative_control_the_guard_test_sees_a_leak_when_the_guard_is_open, test_an_overlay_button_that_navigates_is_undone) | test_tell_owner_puts_only_the_about_line_in_the_history, test_a_refused_alert_reports_not_sent, test_watch_with_a_bot_suffix_lists_and_a_watch_request_skips_an_open_launch_tree (telegram.py's TelegramInlet: the telegram crate's); test_a_watcher_that_cannot_start_costs_the_watching_not_the_daemon (daemon.py: the daemon crate's); test_the_render_child_dies_with_its_parent (monkeypatches `_kill_tree_below` and `os._exit`; render_watchdog has no such seam: watch-pages unit) |
| test_watch_lean_limiter.py | (2 of 7, left to this unit by watch-rules) | test_a_stale_gate_cannot_shorten_the_hosts_backoff, test_no_read_goes_to_a_host_inside_its_hold: Python replaces `_call` with a coroutine that holds the first read; here the read runs off the loop (offload) and a gate holds it, the same interleaving | |

Test adaptations:
- `monkeypatch.setattr(watch, "evaluate", lambda: 1/0)` is `WatcherDeps.evaluate` set to a rule that panics.
- `w._fetch = ...`, `w._render = ...`, `w._ask = ...` are swappable fakes lent before the watcher is built.
- test_this_macs_own_ipv6_prefixes_are_the_lan: Python patches `IPv6Address.is_global` to call 2001:db8:: (doc space)
  global; a real global prefix (2606:4700:88c0::/64) stands in.
- test_a_ticketmaster_price_past_a_float_is_no_price: serde_json holds no 1e400 or 10**400; f64::MAX and 1e300 stand
  in (both past MAX_PRICE).
- test_no_link_hides_behind_words_from_a_search_url_or_a_ticketmaster_note: telegram_format.compose is not in this
  crate; the alert is checked for the markup compose would turn into a link (`[` / `](`).
- `loopback_is_public` (patching check_url and `_ip_public`) is `HttpOptions.rules` / `GuardProxy::start` with
  `AddressRules { ip_public: always, resolve: always, ..system() }`; the loopback servers are std TcpListener threads.
- These tests call the other units' functions (evaluate, structured, RateLimiter, http_request, render, ...). They
  pass only once those units land.

## Boundaries
None in this unit (the Chrome-like read and the render are watch-pages' boundaries; the watcher only calls them).

## Differences
- A reader or a rule that raises something other than FetchError: Python catches any `Exception` in `_run_one`, `add`
  and `run`. A Rust reader returns FetchError where Python's would raise one; what stands for the other exceptions is
  a panic, caught (`catch_unwind`) at the same three places with the same effect (an error counted, "an answer I could
  not make sense of", next_at stretched; a tick that panics waits 60 s). Answers Python trips on with AttributeError
  (a Yahoo `chart` or `meta` that is not an object, a model message that is a string) are FetchErrors here ("the quote
  service sent an unreadable answer"; the model reply reads as empty text): still this watch's counted error, with
  that reason as `last_error`.
- `WatcherDeps.evaluate` is a seam Python does not have (its tests monkeypatch the module function); the default is
  `rules::evaluate`.
- `_takes_about`: the Rust Notify always takes `about`, so `about or text` is always passed.
- The store is read with a json.loads equivalent: a number serde_json cannot hold (NaN, ±Infinity, past a float)
  reads as null, which `Watch::from_json` and `last_id` treat exactly as Python treats the number (the field keeps
  its default). A lone surrogate escape reads as U+FFFD, so a re-save writes `�` where Python wrote the
  surrogate back.
- An unreadable file's ids (`"w(\d{1,9})"`): a non-ASCII digit matches but is not counted (Python's `int()` reads
  it).
- `tick`: a watch whose time is up but that the chat removed during an earlier await of the same tick is skipped;
  Python's `list.remove` raises ValueError there and the tick fails (the loop logs it and waits 60 s).
- `_wake`: Python clears the event after each tick, so a set while no wait is pending is lost; `Notify::notify_waiters`
  stores no permit, the same.
- validate: Python's `float(value)` of an integer past a float raises OverflowError out of the tool call; serde_json
  has already read such a number as a float, so it is no positive value.
- `str(x or "")` of a list or dict argument is its JSON text, not Python's repr.
- Log lines name the error kind as Python does where a Python exception class is involved (JSONDecodeError,
  UnicodeDecodeError, PermissionError...); a notify failure is logged by its message.
