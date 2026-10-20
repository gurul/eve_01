# eve-web: watch-pages unit
Status: complete (four fetch_hosted tests wait for the reader-providers unit's `tinyfish::fetch`)

watch.py lines 310-1505 (a reading and the page's structured data, the guarded HTTP read, the Chrome-like read,
the hosted read, the render with its guard proxy and watchdog) and its `__main__`, in `src/watch/page.rs`,
`net.rs`, `tls.rs` + `tls_helper.py`, `hosted.rs`, `render.rs` and `src/bin/eve-watch.rs`.

## API
| Python | Rust | Notes |
|---|---|---|
| watch.Reading | watch::page::Reading | |
| watch.THREE_DECIMALS, _NUMBER, _SIGNED, _LD_TYPE, _META_RE, _ATTR_RE, _FRAMING_RE, _VOID, MICRODATA_PROPS, OFFER_SCOPES, NOT_THE_ITEM, ITEM_TYPES, _MAIN_OPEN | watch::page constants | patterns verbatim |
| watch._price | watch::page::price | |
| watch._availability | watch::page::availability | |
| watch._ld_blocks | watch::page::ld_blocks | |
| watch._drop_comments | watch::page::drop_comments | |
| watch._BLOCK_OPEN, _drop_blocks | watch::page::drop_blocks | a linear scanner in place of the cached lookahead regexes |
| watch._ld_values | watch::page::ld_values | Python's json (`strict=False`, `raw_decode`) is a private parser in page.rs |
| watch._Microdata | watch::page::microdata | the HTMLParser subclass is a private port of Python 3.13's tokenizer (CDATA and RCDATA elements, `locatetagend`, `attrfind`) |
| watch.microdata | watch::page::microdata | |
| watch._attrs | watch::page::attrs | |
| watch._offers | watch::page::offers | |
| watch._typed_name | watch::page::typed_name | |
| watch._any_name | watch::page::any_name | |
| watch.structured | watch::page::structured | |
| watch.visible_text | watch::page::visible_text | `html.unescape` is a private port (HTML5 table, legacy names, longest prefix) |
| watch.main_text | watch::page::main_text | |
| watch.phrase_on | watch::page::phrase_on | |
| watch._OPEN_SOCKETS | (none) | the deadline is reqwest's per-hop timeout over what is left, and a sized-body cut |
| watch._ip_public | watch::net::ip_public, ip_public_here | Python 3.13's `ipaddress` private and global tables; `ip_public_here` adds this Mac's interfaces |
| watch._LOCAL_NETS, LOCAL_NETS_SECS, _local_networks | watch::net::local_networks, parse_ifconfig, LOCAL_NETS_SECS | `ifconfig`, cached 60 s |
| watch._public | watch::net::public | |
| watch._connect_public | watch::net::connect_public, ConnectError | |
| watch._PinnedHTTP, _PinnedHTTPS | watch::net::pinned_addrs + reqwest `dns_resolver` | the resolver answers only the checked addresses; SNI and certificates stay on the host name |
| watch._PinnedHTTPHandler, _PinnedHTTPSHandler | watch::net::http_request | folded into it |
| watch.check_url | watch::net::check_url, check_url_with | `resolve` is the closure; `check_url_with` takes `AddressRules` |
| watch._origin | watch::net::origin | |
| watch._REDIRECT_SAFE_HEADERS | watch::net::REDIRECT_SAFE_HEADERS | |
| watch._GuardedRedirects | watch::net::check_redirect, redirect_headers | urllib's redirect rules by hand: 10 redirects, 4 repeats, 301-303 to GET, POST 307/308 refused as HTTP errors |
| watch._retry_after | watch::net::retry_after | |
| watch.http_request | watch::net::http_request, HttpOptions | `rules` is the test seam (None: the system's) |
| watch._exchange | watch::net (send_hop, read_answer); watch::tls + tls_helper.py | |
| urllib.parse.urlsplit | watch::net::urlsplit, SplitUrl, BadUrl | Python 3.13 semantics, `.hostname`, `.port`, `.username`, `.password` |
| watch.TLS_IMPERSONATE, TLS_MAX_HOPS, TLS_REDIRECTS, TOO_LONG | watch::tls constants | |
| watch.tls_available | watch::tls::tls_available, tls_available_with | |
| watch._pin | watch::tls::pin | |
| watch.tls_request | watch::tls::tls_request, tls_request_with | `tls_request_with(ctx, url, timeout, rules)` is the test seam |
| watch.tls_worth | watch::tls::tls_worth | |
| watch.fetch_hosted | watch::hosted::fetch_hosted | takes `&Ctx` and check_url's `resolve` |
| watch.OVERLAY_OK, OVERLAY_NEVER, _OVERLAY_JS, OVERLAY_SETTLE_MS, OVERLAY_LOOKS | watch::render constants | verbatim |
| watch.overlay_choice | watch::render::overlay_choice | |
| watch._page_of | watch::render::page_of | |
| watch._clear_overlays | watch::render::clear_overlays | over eve-browser-lane's `PageHandle` |
| watch.RENDER_TIMEOUT_SECS, RENDER_SETTLE_SECS, RENDER_VIEWPORT, RENDER_DEADLINE_PAD_SECS, PROXY_IDLE_SECS | watch::render constants | |
| watch.render | watch::render::render, render_command | the child is `eve-watch --render URL TIMEOUT` |
| watch._GuardProxy | watch::render::GuardProxy | `start(rules)`, `shutdown`, Drop |
| watch._pipe | watch::render::pipe | poll(2) |
| watch._ProxyHandler | watch::render::proxy_handle, DENIED | one thread per connection |
| watch._relay_one_way | watch::render::relay_one_way | |
| watch._descendants | watch::render::descendants | |
| watch._kill_tree | watch::render::kill_tree | |
| watch._render_here | watch::render::render_here | takes `AddressRules` (the live test's seam) |
| watch._render_watchdog | watch::render::render_watchdog | |
| watch._kill_tree_below | watch::render::kill_tree_below | |
| watch._render_main | watch::render::render_main | |
| `if __name__ == "__main__"` | src/bin/eve-watch.rs | `--render` only, as Python's |

## Boundaries
- **Chrome-like TLS read.** No Rust crate gives Chrome's TLS fingerprint on this Mac: rquest and wreq build
  BoringSSL, which needs cmake. So one exchange per hop runs in curl_cffi, in `src/watch/tls_helper.py`. That file
  is compiled in with `include_str!` and run as `python -I -c`. The helper prints `{"ready": true}` once curl_cffi
  has imported. After that it takes one JSON hop per line (url, the pinned `host:port:ip`, seconds left,
  max_bytes, impersonate) and answers with one JSON line. The URL checks, the pin, redirects, the deadline and the
  check on the address libcurl used all stay in Rust. The interpreter is `EVE_WATCH_TLS_PYTHON`, else the bridge
  checkout's `bridge/.venv/bin/python3`, else `python3`.
- **Render.** The child process is the `eve-watch` bin. It sits beside the running executable, or one folder up
  from a test binary in `deps/`, else it is found on PATH. In place of Playwright, it drives headless Chromium
  over CDP through eve-browser-lane's driver (`find_chromium`, `CdpDriver`, `launch_persistent_context` with a
  throwaway profile). Chromium gets `--proxy-server` pointed at the guard, `--proxy-bypass-list=<-loopback>`, and
  the user agent and `--lang` flags.

## Differences
- **JSON numbers no float can hold.** Python keeps an integer past f64 (or `1e400`, `NaN`, `Infinity`) as an int
  or float, and later rules turn it into None. Here it is kept as the string "Infinity", "-Infinity" or "NaN". No
  price, availability or name rule accepts those strings, so the reading is the same (the 16,000-page
  differential agrees).
- **Tag names and attribute names.** These are lowercased ASCII-only. Python's `str.lower()` also folds non-ASCII
  letters in a tag name. No real tag or attribute name has those letters.
- **Render: service workers.** Playwright's `service_workers="block"` has no counterpart in eve-browser-lane's
  driver. Service workers stay on. Every connection they make still goes through the guard proxy, so the SSRF
  guard is unchanged.
- **Render: popups.** Python closes a popup the moment it opens (`context.on("page")`). Here a popup is closed by
  a sweep after `goto` and again after the overlays: every page but the one marked as the one being read is
  closed. Until the sweep, a popup can load, but only through the guard proxy.
- **Render: response status.** Python reads `response.status` from `page.goto`. Here it comes from the Navigation
  Timing entry (`responseStatus`). When there is no entry, 0 is read, which works like Python's `response is
  None`.
- **Render: unexpected errors.** A failure that is not a FetchError, a browser error or an OSError reads as "the
  browser failed (panic)". Python names the exception class there.
- **Render answer line.** `render_main` writes it with serde_json, so non-ASCII text is not `\u`-escaped as
  `json.dumps` would escape it. The parent decodes both forms the same way.
- **Render: decoding the answer.** `render` decodes the shot with strict base64. Python's `b64decode` drops
  characters outside the alphabet. The child always writes valid base64, so the result is the same.
- **Render: the child's stdin.** The child inherits stdin, as Python's Popen does. When the child is killed at its
  deadline, the stdout reader thread is left to end by itself rather than joined.
- **Guard proxy: CONNECT ports.** A CONNECT port that `int()` takes but u16 does not (`4_43`, a port past 65535)
  is denied with 403. In Python it raises inside the handler and the connection just closes. Either way the
  connection is refused.
- **TLS helper start-up.** Importing curl_cffi happens before the deadline starts, as in Python, where the import
  comes first. A helper that is not ready within 30 s reads as "the Chrome-like reader is not installed". A hop
  that has no answer by its deadline plus a 3 s grace kills the helper and reads as "the site took too long to
  answer".
- **http_request: a sized body cut short.** A body shorter than its Content-Length counts as the end of the body,
  as http.client's `IncompleteRead` partial is returned by urllib's `read()`. A broken chunked body is "the site
  sent an unreadable answer".
- **http_request: charsets.** The charset families Python knows are mapped through encoding_rs. A charset neither
  side knows (`bogus-8`) is decoded as UTF-8 with replacement. Python raises LookupError there, and the security
  replay accepts either result.

## Tests
`tests/watch_pages.rs` has 40 tests: 34 run, 6 are ignored (4 cross-unit, 1 live, 1 differential). There are also
9 unit tests in page.rs and 7 in net.rs.

| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| test_watch_security.py | 11 | dns_rebinding_between_the_check_and_the_connect_is_refused, an_unknown_charset_is_a_fetch_error, a_garbage_status_line_is_a_fetch_error, a_crafted_page_never_blocks_the_event_loop (both readers timed directly, under 2 s), a_cross_host_redirect_never_carries_the_openrouter_key (the same POST and 302 sent straight to http_request, because `openrouter` is the watch-rules unit's), a_trickling_server_is_cut_at_the_deadline, a_render_that_never_answers_is_killed_at_its_deadline, the_render_guard_covers_redirects_websockets_and_popups (ignored: live Chromium; it passes against a real headless Chrome) | test_one_watch_whose_read_crashes_never_starves_the_others: in Rust a read returns `Result<_, FetchError>`, so no other error can escape `check()` (the Watcher's tick is the watcher unit's). test_a_page_alert_links_only_the_page_the_owner_gave: Watcher plus model_reading (watcher unit). test_a_search_note_cannot_hide_a_link_behind_words: `model_reading`, `evaluate` and telegram_format (watch-rules unit, and a crate outside eve-web) |
| test_watch_tls.py | 17 | tls_reads_a_page_and_its_charset, tls_the_connection_goes_to_the_pinned_address_not_a_second_lookup, tls_a_name_answering_a_private_address_is_refused_at_the_connect, tls_a_redirect_is_checked_like_the_first_link, tls_redirects_are_followed_then_capped, tls_a_page_past_the_size_cap_is_refused (pages of MAX_BYTES and a little, because the constant cannot be patched), tls_a_proxy_in_the_environment_is_ignored, tls_a_refusal_carries_its_status_and_retry_after, tls_a_trickling_server_is_cut_at_the_deadline, tls_only_other_schemes_and_credentials_are_refused_before_any_lookup, which_failures_are_worth_the_chrome_like_read. The `tls_` tests skip, as Python's `importorskip` does, when the interpreter has no curl_cffi. | test_the_switch (`_tls_on` is the watch-rules unit's config). The 5 ladder tests (test_a_refused_page_is_read_as_chrome_would_and_remembered, test_a_site_that_never_answers_is_read_as_chrome_would, test_a_rate_limit_is_honoured_not_dodged, test_when_the_chrome_like_read_is_refused_too_the_browser_comes_next, test_off_means_off): Watcher (watcher unit) |
| test_watch_shapes_replay.py | 11 | a_huge_integer_price_is_dropped_not_raised (the structured and `_price` halves), a_meta_itemprop_price_in_a_review_is_not_the_items, a_later_products_meta_price_is_not_the_items_and_the_currency_is_kept, json_ld_related_products_and_member_prices_are_not_the_items, main_text_keeps_the_items_own_header_and_custom_elements, json_ld_with_an_unquoted_or_parameterised_type_is_read | the `model_reading` half of test 1 and test_money_never_prints_scientific_notation (watch-rules unit). test_one_page_with_a_huge_price_does_not_starve_the_other_watches and test_a_page_that_never_states_its_price_is_reported_not_silent (Watcher: watcher unit). The two relay tests (Telegram's `_handle`: eve-telegram) |
| test_watch_tinyfish.py | 14 | hosted_a_private_link_is_never_sent_to_tinyfish; hosted_it_asks_for_a_fresh_page, hosted_the_pages_own_refusal_moves_the_ladder_tinyfishs_never_does, hosted_a_redirected_or_renamed_single_result_still_counts, hosted_no_key_no_request (these 4 are fully written over a fake `Ctx::http`, and ignored until the reader-providers unit's `tinyfish::fetch` lands) | test_the_switches (`configured`: watch-rules unit). The 8 watcher tests (the hosted rung, the daily cap, the via that survives a restart): Watcher (watcher unit) |
| test_watch_reverify.py (watcher unit's file; its render and proxy cases test this unit's code) | 3 of them | the_proxy_carries_one_request_per_connection, a_render_that_hangs_is_killed_with_everything_it_started | test_the_render_child_dies_with_its_parent patches `os._exit`. `render_watchdog` ends the process, so it can only be tested from a child process; left with the watcher unit's port of that file |

Extra Rust tests: the_proxy_refuses_private_addresses_and_strips_proxy_headers, only_a_button_that_closes_or_continues_is_pressed,
a_query_or_a_hash_is_the_same_page, an_overlay_is_pressed_away_until_two_quiet_looks,
a_button_that_went_to_another_page_loads_the_items_page_again (clear_overlays over a scripted `PageHandle`),
the_render_child_is_the_eve_watch_bin, a_private_link_is_never_rendered, and the_page_readers_match_python.

**Differential.** `the_page_readers_match_python` is ignored and needs EVE_WATCH_PAGES_DIFF. It was run against
16,000 generated pages (seeds 7 and 11). The pages mix JSON-LD (broken, commented, CDATA-wrapped, nested,
member-tier, non-finite numbers), microdata, meta tags, entities, chrome tags and truncation. The Python side
was watch.py from this skeleton's base. All five readers matched on every page: `structured`'s value, currency,
availability, name, source and note; `main_text`; `visible_text`; and `microdata`. A changed expectation (a
positive control) makes the test fail.

Live checks: the render guard test passes against a real headless Chrome, and `eve-watch --render
https://example.com/ 20` answers `{"ok": true, ...}` with the page and a JPEG.
