# eve-meta
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/__init__.py` | `crates/eve-meta/src/lib.rs` |

## API
| Python | Rust | Notes |
|---|---|---|
| `__version__ = "0.1.2"` | `eve_meta::VERSION` | Literal copied verbatim; not `CARGO_PKG_VERSION` (the workspace manifest says 0.1.3). |
| module docstring | `eve_meta::DESCRIPTION` | Verbatim. |

The module has no top-level `def`, `async def` or `class`.

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| (none in plan) | - | `version_matches_python_package`, `version_is_dotted_integers`, `cli_version_string_format` | - |

`version_matches_python_package` reads `bridge/src/eve_bridge/__init__.py` at build time and fails if the Python version or docstring drifts from the Rust constants.

## Boundaries
None

## Differences
None
