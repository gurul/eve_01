# eve-vault-native
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/vault_native.py` | `src/lib.rs` (constants, `Field`, the `Native` trait, the walk over `AxTree`), `src/mac.rs` (`NativeMac` on macOS: ApplicationServices AX + AppKit `NSRunningApplication` via objc2) |

## API
| Python | Rust | Notes |
|---|---|---|
| `TEXT_ROLES` | `TEXT_ROLES` | same three roles |
| `SECURE_SUBROLE` | `SECURE_SUBROLE` | |
| `MAX_NODES` | `MAX_NODES` | 40000 per browser |
| `BROWSERS` | `BROWSERS` | stored already sorted; Python walks `sorted(BROWSERS)` |
| `NAME_ATTRS` | `NAME_ATTRS` | same order |
| `class Field` (frozen dataclass) | `struct Field` (`Clone`, `Default`, `PartialEq`, `Hash`, `Debug`) | same fields and defaults; `element: Option<Element>` where `Element = Arc<dyn Any + Send + Sync>` (Python's `Any`); `==` compares elements by CFEqual on the Mac, as pyobjc's `==` does |
| `Field.secure` (property) | `Field::secure()` | `subrole == "AXSecureTextField"` |
| `class NativeMac` | `struct NativeMac` + `impl Native for NativeMac` | |
| `NativeMac.__init__` | `NativeMac::new() -> anyhow::Result<Self>` | off macOS fails with `the vault fills fields only on macOS` (Python's `RuntimeError`) |
| "a fake with the same four methods" (duck typing) | `trait Native` | `trusted` defaults to `true`, as vault_mcp's `hasattr(native, "trusted")` treats a native without it; implemented for `&T` and `Box<T>` too |
| `NativeMac._attr` | `AxTree::attr` (`mac.rs`: `copy_attr` + `convert`) | `None` on any AXError; CF values converted as pyobjc converts them (`AxValue`: str, bool, int, float, NSURL, element, list, other) |
| `NativeMac.trusted` | `Native::trusted` | `AXIsProcessTrusted()` |
| `NativeMac._url` | `url_of` | CFURL → `absoluteString`; else `str(raw or "")` |
| `NativeMac._words` | `words_of` | NAME_ATTRS, then the AXTitleUIElement's `AXValue or AXTitle`; only non-blank strings (Python `isspace`), joined by a space, first 200 characters |
| `NativeMac.page_fields` | `Native::page_fields` (`mac.rs`) + `walk_app` (the per-browser walk, generic over `AxTree`) | running apps asked per bundle id (`runningApplicationsWithBundleIdentifier`), terminated ones dropped; `AXManualAccessibility` set to true and its error ignored; depth-first stack, last window first, children reversed, `order` = nodes seen; refused pages never walked; frames only when `want` accepts them or `frames` and inside an accepted page |
| `NativeMac.set_value` | `Native::set_value` | `AXUIElementSetAttributeValue(element, "AXValue", text) == 0` |
| `NativeMac.char_count` | `Native::char_count` + `count_of` | `int(n) if isinstance(n, int) else None` (a CFBoolean counts as 0/1, as Python's bool is an int) |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| (none: `tests` is empty in plan.json) | | `src/tests.rs`: `page_fields_reads_only_the_pages_want_accepts`, `order_counts_nodes_top_to_bottom_and_the_last_window_is_walked_first`, `a_text_field_outside_a_page_or_inside_a_field_is_not_listed`, `a_frame_needs_want_unless_frames_and_keeps_its_top_page`, `words_join_the_names_and_the_label_never_the_value`, `words_stop_at_200_characters`, `focus_is_only_a_true_boolean`, `the_url_is_the_absolute_string_or_str_of_what_the_browser_gives`, `a_page_with_no_url_has_no_fields_listed`, `a_walk_stops_at_max_nodes`, `a_subrole_that_is_missing_or_empty_is_empty`, `the_elements_are_the_walked_nodes`, `char_count_is_an_int_or_none`, `field_defaults_and_secure_match_the_dataclass`, `the_constants_are_pythons`, `native_mac_refuses_off_macos` (non-mac), `native_mac_builds_on_macos_and_a_field_without_an_element_takes_nothing` (mac), `live_native_mac_walks_running_browsers` (`#[ignore = "live: ..."]`) | |
| `bridge/tests/test_vault_mcp.py` (tests_ref) | `FakeMac`, `f()` helper | `a_fake_with_the_four_methods_is_a_native_and_trusted_by_default` (the FakeMac contract through `Native`) | the fill/MCP tests test `vault_mcp` (crate `eve-vault-mcp`); the two live Playwright tests drive a headed Chromium and belong with `vault_mcp`'s port; this crate's walk is covered over a fake tree and by its own live test |

## Boundaries
None. The Accessibility and AppKit calls are made directly through `objc2-application-services` and `objc2-app-kit` (macOS only).

## Differences
- `set_value` / `char_count` on a `Field` with no Mac element (a fake's field) return `false` / `None`; Python would raise from pyobjc. `NativeMac` only ever receives fields from its own `page_fields`.
- A non-element entry in an `AXChildren`/`AXWindows` array, or an `AXTitleUIElement` that is not an element, is skipped; pyobjc would raise. Browsers never return either.
