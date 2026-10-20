# Porting eve's bridge to Rust

This directory is a module-for-module Rust port of `bridge/src/eve_bridge`, the
Python daemon. `plan.json` maps every Python module to exactly one crate. Every
crate follows the rules below so the pieces fit together without a central
editor.

## The goal: a drop-in replacement

The Rust daemon must be able to replace the Python one on the same Mac, reading
and writing the same state. So a port keeps, byte for byte where it matters:

- **Environment variables**: same names, same defaults, same parsing (truthy
  strings, int fallbacks).
- **Files on disk**: same paths (`~/.config/eve-bridge/...`, `~/.claude/...`,
  the vault, transcripts), same formats (JSON keys, JSONL lines, SQLite
  schemas and queries). Rust must read a file the Python wrote and vice versa.
- **Wire protocols**: same HTTP routes, methods, status codes, JSON bodies,
  WebSocket messages, IPC socket messages, serial and BLE frames.
- **Prompts and model ids**: copy prompt strings, tool schemas, model names,
  thresholds and constants verbatim. Prompt text is product behavior.
- **Behavior**: same branching, same fallbacks, same error messages the owner
  sees. Preserve the comments that explain why; drop the ones that only
  narrate.

Read the Python module in full before writing Rust, and read the tests: they
are the spec.

## Layout

- Crate `crates/eve-<name>`, package name `eve-<name>`, lib name
  `eve_<name with underscores>` (cargo does this by default).
- Each Python module becomes one Rust module file: `telegram.py` →
  `src/telegram.rs`, `hooks/pretooluse.py` → `src/pretooluse.rs`. A
  single-module crate may put the code straight in `src/lib.rs`.
- Python files that are entry points (`__main__`, hook scripts, `cli.py`)
  become `[[bin]]` targets as well as library code.
- `Cargo.toml` uses `version.workspace = true`, `edition.workspace = true`,
  and `x.workspace = true` for anything in the root `[workspace.dependencies]`.
  A dependency that isn't listed there goes in your own crate's `Cargo.toml`
  with an explicit version. **Never edit `rust/Cargo.toml`.**
- Depend on another eve crate by path: `eve-spend = { path = "../eve-spend" }`.
- **One version per external crate.** Before adding an external dependency
  that isn't in the workspace list, check whether another crate already uses it
  (`grep -h '^<crate> *=' crates/*/Cargo.toml`) and copy that exact version
  requirement. If none does, take the newest release from
  `cargo search <crate> --limit 1`. The point is that `objc2`, `btleplug` and
  the others resolve once and their types can cross crate boundaries.
- Every crate directory already exists with a minimal `Cargo.toml` and a
  one-line `src/lib.rs`: read them, then replace them. Never delete either
  file, even for a moment, and never create a directory under `crates/`
  without both. `crates/*` is a glob, so one crate directory without a
  manifest stops every other agent's cargo from loading the workspace.

## Idioms

- **Async**: tokio. A Python `async def` becomes an `async fn`. Background
  tasks become `tokio::spawn`; `asyncio.Queue` becomes `tokio::sync::mpsc`;
  `asyncio.Lock` becomes `tokio::sync::Mutex`.
- **Errors**: `anyhow::Result` at boundaries. Use `thiserror` when callers
  match on the error kind (where Python catches a specific exception).
- **JSON**: typed `serde` structs where the shape is fixed and
  `serde_json::Value` where Python passes free-form dicts. `preserve_order` is
  on, so key order matches Python's dict order when written.
- **HTTP**: `reqwest` (rustls). Model calls go through `eve-openrouter`, the
  port of `openrouter.py`. Servers use `axum`.
- **Logging**: `tracing`. Match the Python log messages.
- **Globals and module state**: `std::sync::LazyLock` / `OnceLock`, with
  `parking_lot::Mutex` or `tokio::sync::Mutex`.
- **Time**: `chrono`. Keep Python's timestamp formats exactly (ISO strings,
  epoch floats).
- **macOS frameworks** (pyobjc Quartz, ApplicationServices/AX, Vision, AppKit,
  Keychain): use `objc2` and its framework crates (`objc2-foundation`,
  `objc2-app-kit`, `objc2-vision`), `core-graphics`, `core-foundation`,
  `accessibility-sys`, `security-framework`, gated with
  `#[cfg(target_os = "macos")]`, plus a non-mac fallback that matches what the
  Python does off-mac.
- **Third-party Python libraries**: use the Rust equivalent. `bleak` →
  `btleplug`; `pyserial` → `serialport`; `watchfiles` → `notify`;
  `sounddevice` → `cpal`; `numpy` PCM work → plain `Vec<i16>`/`Vec<f32>`;
  `pillow` → `image`; `sherpa-onnx` → `sherpa-rs`; `aiortc` → `webrtc`;
  `playwright` → `chromiumoxide` (CDP); `curl_cffi` → `rquest` (Chrome TLS
  impersonation); `segno` → `qrcode`; `pyautogui` → `enigo` or Quartz events;
  `pyperclip` → `arboard`. Composio, mem0, Exa, Firecrawl, TinyFish and the
  other services are called over their HTTP APIs.
- **Boundaries**: when a capability has no workable Rust path (a Python-only
  ML runtime, for example), call the existing tool through a subprocess with
  the same inputs and outputs, and record it under *Boundaries* in
  `PARITY.md`. That is a design decision, written down; a silent stub is a
  defect.

## Hard rules

- **No placeholders.** No `todo!()`, `unimplemented!()`, `panic!("not
  implemented")`, empty function bodies standing in for logic, or comments like
  "TODO: port". `tools/check_port.py` rejects them. If a piece is genuinely
  impossible, write the boundary and record it.
- **Do not change Python files.** The Python daemon is live.
- **Stay in your crate.** You may make a small additive edit to an upstream
  crate only to expose something its Python module already provides, and only
  when the port left it private or missing. Record the edit in that crate's
  `PARITY.md`, and re-run its tests.
- **Assets** (HTML, JS, prompt files, templates, fonts) stay where they are in
  `bridge/src/eve_bridge/`. Embed them with
  `include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../bridge/src/eve_bridge/<file>"))`
  or `include_dir!`, so both daemons serve the same bytes.

## Tests

Port the crate's Python test files (`tests` in `plan.json`) to Rust: unit tests
in `#[cfg(test)]` or integration tests under `tests/`. Keep the test names in
snake_case and the assertions. Pytest fixtures become helper functions;
`monkeypatch` of env or HOME becomes a temp dir plus an explicit path or
config parameter, because tests run in parallel threads and must not mutate
the process environment. Where Python patched a module function, introduce a
trait or a function parameter for the seam. Tests marked `live` become
`#[ignore = "live: ..."]`. The files in `tests_ref` exercise this crate from
elsewhere: read them for behavior, and port the parts that test this crate.

Run from `rust/`:

    cargo test -p eve-<name>
    cargo clippy -p eve-<name> --tests
    python3 tools/check_port.py --crate <name>

## PARITY.md

Every crate has a `PARITY.md` with these sections:

```
# eve-<name>
Status: complete

## Modules
| Python | Rust |

## API
| Python | Rust | Notes |
(every top-level def, async def and class in each module: one row each)

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |

## Boundaries
(subprocess or platform boundaries, or "None")

## Differences
(any intended behavioral difference, or "None")
```

`check_port.py` requires `Status: complete` and a row naming every top-level
Python definition. Write `Status: partial` only if you were stopped before
finishing, and list exactly what remains.

## Committing

Commit only your own paths, and do not add AI attribution:

    git add rust/crates/eve-<name> && git commit -m "rust: port <name>" -- rust/crates/eve-<name>

If `index.lock` exists, another agent is committing: wait a few seconds and
retry. Never `git add -A`, never push, never rebase.
