# eve-lite-web
Status: complete

## Modules
| Python | Rust |
|---|---|
| `bridge/src/eve_bridge/lite_web.py` | `src/lite_web.rs` (re-exported from `src/lib.rs`, which holds the module docstring); segno's QR drawing in `src/qr.rs` |

## API
| Python | Rust | Notes |
|---|---|---|
| `LOCAL_PORTS`, `LAN_PORTS`, `LOCAL_HOSTS`, `LAN_HOSTS`, `LOOPBACK_NAMES`, `WEB_FILE`, `WEB_DIR_ENV` | same-named `const`s | same values, same order |
| `PACKAGED_WEB_DIR` | `const PACKAGED_WEB_DIR` | `bridge/src/eve_bridge/web_lite`, the folder the Python package serves, found from `CARGO_MANIFEST_DIR` |
| `HEAD_SECS`, `BODY_SECS`, `TLS_SECS`, `CLOSE_SECS`, `RTC_SECS`, `APPS_SECS`, `CONNECTED_SECS`, `CREDIT_TTL_SECS`, `CREDIT_WAIT_SECS`, `NAMES_TTL_SECS`, `RESTART_DELAY_SECS`, `MAX_CONNECTIONS`, `MAX_PER_PEER`, `MAX_HEADERS`, `MAX_TARGET_CHARS`, `MAX_JSON_BYTES`, `MAX_HEAD_HOLD`, `CACHE_MAX_BYTES`, `GZIP_MIN_BYTES` | same-named `const`s | same values; the four the tests patch (`HEAD_SECS`, `CREDIT_WAIT_SECS`, `MAX_CONNECTIONS`, `MAX_PER_PEER`) are also `Parts::limits` (`struct Limits`), read per connection or request |
| the route paths (`HEALTH_PATH` … `LINK_PREFIX`) | same-named `const`s | verbatim |
| `ASSET_RE`, `APP_CONNECT_RE`, `LINK_FILE_RE` | same-named `static`s | anchored `\A…\z` (Python's `match`/`fullmatch` on these patterns) |
| `CSP`, `SECURITY_HEADERS`, `LINK_HEADERS`, `LINK_DROPPED`, `JSON_TYPE`, `HTML_TYPE`, `NO_STORE`, `IMMUTABLE`, `TYPES`, `GZIP_TYPES`, `REASONS` | same-named `const`s | dicts are ordered arrays of pairs, in Python's insertion order (the headers go out in that order) |
| `READY`, `NEEDS_OPENROUTER` and every page line (`WRONG_HOST` … `FAILED`) | same-named `const`s | verbatim; `{want}` / `{name}` filled with `str::replace` |
| `_METHOD`, `_FIELD`, `_SINGLE`, `_STATE`, `_CREDIT_WHY`, `_OPAQUE`, `_KINDS`, `_BROWSERS` | `METHOD_RE`, `FIELD_RE`, `SINGLE`, `STATE_RE`, `CREDIT_WHY_RE`, `OPAQUE_RE`, `KINDS`, `BROWSERS` (private) | fullmatch patterns anchored |
| `log` | `tracing` | the same lines at the same levels; counts, ports and exception type names only |
| `_port` | `fn port_of` (private) | an int (not a bool) in 1..65535 |
| `_ports` | `pub fn ports` | |
| `_plain` | `pub fn plain` | Python's `str.split()` whitespace (with U+001C..U+001F), 300 characters |
| `_credit_view` | `pub fn credit_view`, `type CreditView` | field by field; `Parts::credit_view` is the seam the security test swaps (`monkeypatch.setattr(lite_web, "_credit_view", ...)`) |
| `_key_rejected` | `pub fn key_rejected` | |
| `device_label` | `pub fn device_label` | |
| `_host_name` | `pub fn host_name` | `str.isdigit()` on the part after the last colon (ASCII digits; the Host header passed the latin-1 field check) |
| `_query` | `pub fn query` | `parse_qsl(keep_blank_values=True, max_num_fields=32)`: split on `&`, more than 32 fields is `{}`, `+` is a space, percent-escapes as UTF-8 with replacement; the first of each name |
| `_media` | `fn media` (private) | |
| `_content_type` | `pub fn content_type` | `os.path.splitext` (leading dots are no extension) |
| `_accepts_gzip` | `pub fn accepts_gzip` | `float()` of the weight via `eve_apps_mcp::py::parse_float` |
| `_page_html` | `pub fn page_html` | `html.escape` (quote=True) |
| `_NO_SEGNO` | the `SAID` flag in `qr_svg` | said once (Differences) |
| `qr_svg` | `pub fn qr_svg`, `src/qr.rs` | byte for byte segno 1.6.6's `make(url, error="m").svg_data_uri(scale=1, border=4, dark="#000", light="#fff", omitsize=True)` |
| `_quietly` | inline in `Door::pair_open` | the alert hook's failure is logged at INFO with its type, never failing the request |
| `_Saved` | `pub struct Saved`, `enum Change`, `struct SavedState` | `save(&[Change]) -> bool` writes the whole file through `eve_apps_mcp::write_private_json` (0600, atomic) and holds the values only once written; an unknown field is impossible by type (Python's ValueError); `set_writer` is the tests' full disk, `set_held` the tests' `saved.local_port = …` |
| `_Files` | `pub struct Files` | resolve both paths (`canonicalize`), `starts_with`, regular file; keyed by the real path with (inode, size, mtime ns); the gzip kept beside it; the 64 MiB cap and the quarter-cap rule as in Python |
| `_Daemon` | `struct Daemon` (private) | |
| `_Shared` | `pub struct Shared` | `nonce` (`token_urlsafe(32)`), `saved`, and accessors for `restarting`, `error`, `local_network`, `credit` (`forget_credit` for `shared.credit = None`), `rtc_doors`, `lan`; the credit read is one tokio task shared by every page waiting (`credit_task`, done when its task finished); `tasks` are the background tasks `stop` cancels; `lan_lock` a tokio mutex |
| `WebContext` | `pub struct WebContext`, `pub struct Parts`, `type StatusFn/RestartFn/CreditFn/HostnameFn/AddressesFn/PeerAllowedFn/AlertFn/ProbeFn`, `struct WebEnv` | the parts are read again on every request (`parts()`), and set later with `update(|p| …)`; `Parts::new` takes the five a door needs and gives the daemon's defaults for the rest (lan_net, onboarding.credit, the port ranges, os.environ); `Debug` never shows the environment (`__repr__`) |
| `WebContext.web_root` | `WebContext::web_root` | |
| `_require_lite` | `WebContext::require_lite` (private) | `StartError::Runtime` with Python's text; profile's ValueError is `StartError::Value` |
| `PortsTaken` | `pub struct PortsTaken` (`StartError::PortsTaken`) | errno EADDRINUSE, text `[Errno 48] no free port among N` |
| `_Refused` | `struct Refused` (private) | |
| `_Request` | `struct Request` (private) | no Debug (the headers hold the cookie); `peer()` is the address without its port |
| `_read_head` | `fn read_head`, `fn read_until_blank`, `fn parse_head` (private) | `readuntil(b"\r\n\r\n")` under asyncio's 64 KiB limit (an overrun is the 431), then the same checks in the same order with the same lines |
| `_head_bytes` | `fn head_bytes` (private) | a value with a line break, or a character past U+00FF, fails as Python's encode does |
| `_AddedHeaders` | `struct ConnWriter` (private) | the connection's writer; `add_headers(LINK_HEADERS)` holds the next head to its blank line and adds what LinkChat did not set; `release` is `flush()` |
| `_Route` | `pub struct Route` (`methods()`, `on()`, `cookie()`), `enum Handler`, `enum Where` | |
| `ROUTES`, `ASSET_ROUTE`, `CONNECT_ROUTE`, `LINK_ROUTES`, `LINK_FILE_ROUTE` | same-named private `const`s | |
| `GET_HEAD`, `GET`, `POST`, `LOCAL`, `LAN`, `BOTH` | same-named private `const`s | |
| `route_for` | `pub fn route_for -> Option<(Route, String)>` | None for Python's `(None, "")` |
| `_Door` | `struct Door` (private) | one door's sockets, counts and handlers; `kind` (`Where`) picks `where`, `scheme`, `cookie` and `signed_out`; every `_Door`, `LocalServer` and `LanServer` method is here, the two public types wrap it |
| `_Door._socket`, `_bind` | `Door::socket`, `Door::bind` | socket2: SO_REUSEADDR, IPV6_V6ONLY on "::", listen(128); a later host that this Mac cannot have (EADDRNOTAVAIL, EAFNOSUPPORT, EPROTONOSUPPORT) left out |
| `_Door._close`, `_connection`, `_shut`, `_admit`, `_serve_one`, `_handle`, `_signed_in` | same-named `Door` methods (`close`, `connection`, `admit`, `serve_one`, `handle`, `signed_in`) | the caps checked as Python does (`len(conns) > max`, `per_peer >= max`); the counts given back before the close |
| `_Door._send`, `_json`, `_error`, `_page`, `_redirect`, `_body`, `_cookie_again`, `_spawn`, `_warn_once` | `fn send`, `Door::json/error/page/redirect/body/cookie_again`, `Shared::spawn/warn_once` | the body: ASCII digits only, 413 past MAX_JSON_BYTES, BODY_SECS, `json.loads` of bytes (`eve_apps_mcp::py::json_loads_bytes`), an object only |
| `_Door._daemon`, `_credit`, `_read_credit`, `_composio_signed_in`, `_composio_live`, `lan_on`, `status`, `_close_peers`, `_revoke_ids`, `_probe_soon`, `_sign_in` | same-named `Door` methods | |
| `_Door._index`, `_asset`, `_static`, `_health`, `_rtc`, `_link`, `_nonce_ok`, `_apps`, `_connect`, `_status`, `_logout` | `Door::static_file`, `health`, `rtc`, `link`, `apps`, `connect`, `status_route`, `logout`; `_nonce_ok` is the closure `link` hands LinkChat (`compare_digest`) | |
| `LocalServer` | `pub struct LocalServer` | `new`, `start` / `start_with(resume_lan)`, `stop`, `set_lan`, `revoke`, `status`, `port`, `url`, `running`, `bound`, `refused`, `lan`, `lan_on`; `open_connections`, `connections_from`, `any_peer` for the tests' `_conns` and `_per_peer` |
| `LocalServer._mac_login`, `_onboard_state`, `_openrouter_start`, `_composio_start`, `_openrouter_callback`, `_composio_callback`, `_signed_up`, `_signin_failed`, `_restart_soon`, `_composio_skip`, `_pair_open`, `_devices`, `_devices_revoke`, `_lan`, `_hosts` | same-named `Door` methods (`lan_route` for `_lan`, `hosts` for `_hosts`) | |
| `LanServer` | `pub struct LanServer` | `new`, `start`, `stop`, `refresh_names`, `port`, `running`, `bound`, `dropped`, `refused`, `lan_on`, `status`, `hostname`, `lan_addresses`, `same`; `expire_names` for the tests' `_allowed_at = 0.0` |
| `LanServer._hosts`, `_admit`, `_pair`, `_ca`, `_mobileconfig`, `_download` | `Door::lan_hosts`, `admit`, `pair`, `download(profile)` | TLS with `tokio_rustls::TlsAcceptor` on the accepted stream, after the peer filter and before a byte is read |
| link_chat.LinkChat, lite_rtc.RtcHub, lite_tls.LocalTls, onboarding's two logins, composio_connect.ConnectBridge (duck-typed in Python) | traits `LinkServe`, `RtcDoor`, `TlsDoor`, `OpenRouterSignIn`, `ComposioSignIn`, `AppsDoor`, each implemented for the real type | the tests implement them for their fakes |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_lite_web_local.py` | 28 | 28 in `tests/lite_web_local.rs`, same names, plus `helpers_match_python` (the pure helpers) and `a_credit_read_that_landed_late_is_not_reused` (a defect found in the parity hunt); the rig and fakes in `tests/common/mod.rs` | none |
| `bridge/tests/test_lite_web_lan.py` | 15 (the hosts test parametrized 9 ways) | 15 in `tests/lite_web_lan.rs`, same names; the parametrized cases run as a loop in one test | none |
| `bridge/tests/test_lite_web_security.py` | 21 (Content-Length 4 ways, LinkChat paths 19, raw heads 14) | 21 in `tests/lite_web_security.rs`, same names; parametrized cases as loops; `test_wrong_methods…` also checks the `Allow` values | none |
| `bridge/tests/test_cli_lite.py` (tests_ref) | uses `lite_web.LAN_FAILED` only | `LAN_FAILED` is the same `const` | the CLI tests are the CLI crate's |
| (segno) | — | `tests/qr.rs`: 26 cases drawn by segno 1.6.6 itself (4 pairing links, the three modes, versions 1 to 20 and more) compared byte for byte, and the too-long case | — |

The rig is Python's: a real LocalServer and LanServer on ephemeral loopback ports, real web_auth objects, a real LocalTls and the real onboarding sign-ins over a mock transport, the real LinkChat behind a FakeInlet, and a raw HTTP client (plain and rustls trusting the rig's CA). Every log check uses one process-wide subscriber (a scoped one misses lines whose callsite another test reached first).

## Boundaries
None. Everything the doors call is in-process Rust: eve-web-auth, eve-lite-tls, eve-lite-rtc, eve-link, eve-brain (onboarding), eve-composio-connect, eve-lan-net, eve-profile, eve-apps-mcp. No upstream crate was edited.

## Differences
- **gzip bytes.** `_Files` compresses with flate2 at level 6 with mtime 0, as `gzip.compress(data, compresslevel=6, mtime=0)` does; the deflate stream can differ byte-wise from zlib's. It decompresses to the same bytes, and the cache rules are the same.
- **QR codes.** `qr.rs` reproduces segno's full QR codes exactly (mode choice, version search and the M→Q→H boost, segno's padding, masks and their scores, format and version information, the SVG path and its quoting). It does not make Micro QR codes (segno picks Micro for very short texts; a pairing link never is one) or use Kanji mode. A text too long for version 40 gets `""` and one warning; segno raised `DataOverflowError` there, which Python's `_pair_open` would have turned into a 500. Python's "segno is not installed" line has no counterpart: the drawing is built in.
- **Callables.** Python accepts `status` and `restart` as sync or async callables; here they are async closures (`StatusFn`, `RestartFn`; `status_value` wraps a fixed mapping). `credit`, `hostname`, `addresses`, `peer_allowed`, `alert` and `probe` return `anyhow::Result`, an `Err` standing for Python's exception.
- **Exception names in logs.** Python logs `type(e).__name__`. Here an I/O error anywhere in an error's chain is named by its kind (`OSError`, `PermissionError`, `FileNotFoundError`, `TimeoutError`, …), a sign-in error by its variant (`ValueError`, `OnboardingError`, `StateError`, `RuntimeError`), anything else `ValueError` (`RuntimeError` for a restart). The credit read's `error` reason for a failing stand-in follows the same naming.
- **Setting a part.** Python assigns attributes on the context (`ctx.rtc = None`); here `WebContext::update(|p| p.rtc = None)`. The module constants the tests patch are `Parts::limits` and `Parts::credit_view`; `write_private_json` is `Saved::set_writer`.
- **What LinkChat gets.** The headers are an `eve_link::seams::Headers` (a HashMap) without Origin, Cookie and Authorization, plus the nonce as `Authorization: Bearer`; the request's own stream is passed as `DynRead`/`DynWrite`. The answer goes through `ConnWriter`, which adds LINK_HEADERS as `_AddedHeaders` does.
- **The close.** After its answer a connection reads off at most 128 KiB the peer sent and nobody read (a refused body), for at most CLOSE_SECS, before the socket goes, so the close is a FIN and not a reset that could beat the answer to the client. asyncio had those bytes in its buffer already. The connection's counts are given back before this, as Python's `finally` does before `_shut`.
- **TLS.** The LAN door accepts TLS with rustls (the `rustls::ServerConfig` from `LocalTls::context`) rather than `start_tls` with an `ssl.SSLContext`; a failed or slow handshake (TLS_SECS) is closed silently as in Python.
- **An OSError inside a handler.** Python re-raises an `OSError` from a route (for example the devices file unreadable during a list) and closes without an answer; here only a failed read or write on the connection itself closes silently, and any other failure is the logged 500 `FAILED`.
- **The peer's address.** `Request::peer` and the per-address count use `SocketAddr::ip()`, which leaves out an IPv6 link-local address's `%scope` that Python's `str(peername[0])` keeps. The count is per device either way, and lite_rtc reads only an IPv4 peer address.
