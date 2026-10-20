# eve-spotify
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/spotify.py` | `src/lib.rs` |

## API
| Python | Rust | Notes |
|---|---|---|
| constants (`CONFIG_PATH`, `OFF_REASON`, `NOT_SET_UP_LINE`, `API`, `ACCOUNTS`, `REDIRECT_URI`, `LOGIN_PORT`, `SCOPES`, `HTTP_TIMEOUT_SECS`, `TOKEN_MARGIN_SECS`, `VOLUME_STEP`, `WAKE_SECS`, `MAC_APP`, `DEVICE_COLORS`, `COLORS_FILE`, `ART_HOST`, `FORGET_DEVICE_DAYS`, `ACTIONS`, `KINDS`, `TOOLS`, `TOOL_NAMES`, `INSTRUCTIONS`) | same names, `pub const` / `pub static TOOLS` | Values verbatim; `tests/parity_python.rs` diffs TOOLS, INSTRUCTIONS and the lines against the live Python |
| `Http` (type alias) | `trait Http` (async), `HttpReply` | `(method, url, headers, body) -> (status, headers, body)`; an `io::Error` is Python's `OSError` (no answer) |
| `SpotifyError` | `struct SpotifyError { status, reason, line }` | Plus `enum Error { Spotify, Io, Other }` for what a call can raise |
| `urllib_http` | `UrllibHttp` / `urllib_http()` | reqwest (rustls), 10 s timeout, header names lower-cased, never fails for an HTTP status |
| `load` | `load(path) -> Dict` | `{}` unless a JSON object with truthy `client_id` and `refresh_token` |
| `save` | `save(cfg, path)` | `.tmp` sibling opened 0600, Python's `json.dump(indent=2)` bytes (`py_dumps`, `ensure_ascii`), chmod 600, rename |
| `pkce_pair` | `pkce_pair()` | `token_urlsafe(64)` verifier (86 chars), S256 challenge |
| `authorize_url` | `authorize_url()` | `urlencode` reimplemented as Python's `quote_plus` |
| `_token_post` | `token_post()` (private) | Basic auth with a secret; PKCE puts `client_id` in the form |
| `Token` (`__init__`, `get`, `_get`) | `Token::new` / `Token::with(cfg, http, save_to, clock)`, `Token::get(force)`, `Token::cfg()` | tokio Mutex held across the refresh (Python's `threading.Lock`): two callers refresh once; rotated refresh token saved at once |
| `_refusal` | `refusal()` (private) | Same lines for 404 no device, 403 premium / restricted, 429, 401, other |
| `SpotifyApi` (`call`, `player`, `devices`, `play`, `pause`, `next`, `previous`, `volume`, `transfer`, `search`) | `SpotifyApi` with the same methods (async) | One 401 retry with a forced refresh; JSON bodies as Python's `json.dumps` |
| `DeviceColors` (`__init__`, `_save`, `index`, `rgb`, `hex`) | `DeviceColors::new(path)` / `::with(path, today, forget_days)`, `index`, `rgb`, `hex`, `known: IndexMap<String, ColorEntry>` | Reads the `{"i", "seen"}` shape and the first (bare index) shape; any bad entry resets all, as Python |
| `now_of` | `now_of(player)` | Same keys and order |
| `_device_public` | `device_public()` | |
| `said_now` | `said_now()` | |
| `_join` | `join()` | |
| `_norm` | `norm()` | |
| `Command` | `struct Command` (`Command::new(action)`) | Fields `action, query, volume, change, device, on` |
| `match` (module function) | `match_text(text)` | `match` is a Rust keyword. Patterns verbatim on `fancy-regex` (lookaheads), anchored at both ends like `re.fullmatch`; a volume in any Unicode decimal digits reads as `int()` reads it; cross-checked against Python on 75 phrases |
| `Spotify.__init__` | `Spotify::new(api)` + `with_wake`, `with_sleep`, `with_wake_secs`, `with_colors` | `mode` is `set_mode(Option<Arc<dyn SpotifyMode>>)` / `mode()` |
| `Spotify._run` | `Spotify::wake()` (private) | Only the blocking wake goes to `spawn_blocking`; the API calls are async already |
| `Spotify.now_playing` | `Spotify::now_playing()` | `anyhow::Result<Dict>`; refusals and no network become `{"ok": false, "reason"}` |
| `Spotify.pick` | `Spotify::pick(devices, name)` | exact, word, kind; `{name!r}` via `py_repr` |
| `Spotify._devices_or_wake` | `devices_or_wake()` (private) | |
| `Spotify._play` | `play_on()` (private) | Falls back to an online device, the computer first |
| `Spotify.control` / `_control` | `Spotify::control(action, ControlArgs)` / `control_inner` | kwargs become `ControlArgs { query, kind, volume, volume_change, device }` |
| `Spotify.tools` | `Spotify::tools()` | A copy of TOOLS |
| `Spotify.instructions` | `Spotify::instructions()` | |
| `Spotify.handle` | `Spotify::handle(name, args)` | ints and floats (not bools) for volume / volume_change |
| `Spotify.match` | `Spotify::match_text(text)` | |
| `Spotify.run` | `Spotify::run(&Command)` | |
| `Spotify.panel` | `Spotify::panel(body)` | `mode` is the mode's `on` or null |
| `Spotify.close` | `Spotify::close()` | |
| `self.mode` (music_mode.MusicMode) | `trait SpotifyMode { set, on }`, implemented for `eve_music_mode::MusicMode` | And `impl eve_music_mode::SpotifyControl for Spotify`, so the daemon wires the two directly |
| `open_mac_app` | `open_mac_app()` | `open -g -a Spotify`, 10 s timeout, false off macOS or without `/Applications/Spotify.app` |
| `enabled` | `enabled(environ)` | `None` reads the process environment |
| `make_spotify` | `make_spotify(path, environ, http)` | colours beside the login in `spotify-colors.json` |
| `login` | `login(...)` / `login_on(listener, ...)` | Loopback server on 127.0.0.1:8888 (tokio), state checked, code traded with the PKCE verifier |
| `cli` | `cli(argv, path, out, http) -> i32` (async) | clap in place of argparse, same subcommands, flags, defaults (`SPOTIFY_CLIENT_ID`), exit codes (0 / 1 / 2) and lines |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_spotify.py` | all 31 (`test_match` and `test_match_leaves_the_rest_to_the_model` parametrized) | `tests/test_spotify.rs`: the same 31 names, each parametrized case looped in one test | None |
| `bridge/tests/test_music_mode.py` | `test_spotify_mode_by_words_and_by_the_tool` (the spotify half) | `tests/test_spotify.rs::test_spotify_mode_by_words_and_by_the_tool` with the real `eve_music_mode::MusicMode`; plus `test_the_panel_reports_the_mode` | The rest belongs to eve-music-mode |
| `bridge/tests/test_app_registry.py`, `test_telegram.py`, `test_tool_gate.py`, `test_voice_agent.py` | use `spotify.TOOLS`, `INSTRUCTIONS`, `OFF_REASON`, `Spotify.match`, `handle` | covered by the tests above and `tests/parity_python.rs` | Their own behavior belongs to those crates |
| (new) | | `tests/parity_python.rs`: TOOLS / INSTRUCTIONS / lines equal to the live Python; `match` on 75 phrases equal to Python's; the login file written by Rust is read and rewritten byte-identically by Python. Skips when python3 or the bridge source is absent | |
| (new) | | `src/lib.rs` unit tests: `py_dumps`, `py_repr`, `quote_plus`, `join`, `norm`; `test_the_colours_file_is_written_as_python_writes_it` | |

## Boundaries
- Opening the Mac's Spotify (`open -g -a Spotify`) and the browser for the login (`open URL` on macOS, `xdg-open` elsewhere, in place of Python's `webbrowser.open`) are subprocesses, as in the Python.

## Differences
- Async throughout: `Token::get`, every `SpotifyApi` call and `cli` / `login` are `async` (Python runs the blocking calls in threads and the CLI under `asyncio.run`). The behaviour and the wire are the same.
- `Spotify::control`, `now_playing`, `handle`, `run` and `panel` return `anyhow::Result`: `Err` only where the Python would raise something other than a Spotify refusal or `OSError` (a failing Voice PE send in Spotify mode).
- A malformed answer (a non-object JSON body, a non-string device type) reads as empty where Python would raise `AttributeError`.
- `login` answers a non-GET request on the loopback with 501 and closes; the redirect page and its headers are otherwise the same, minus Python's `Server`/`Date` headers.
- `cli logout` reports a delete that fails for a reason other than "not found" (Python would raise).
- `login` reports a token call that gets no answer, or a login file it cannot write, as a line and exit code 1 (Python would raise).
- `norm` and the code path follow Python's `\w` / `\s` / `\d` and `int()` on Unicode (cross-checked in `tests/parity_python.rs`), except that a combining mark Rust counts as alphabetic (Other_Alphabetic) is kept where Python's `isalnum()` turns it into a space.
