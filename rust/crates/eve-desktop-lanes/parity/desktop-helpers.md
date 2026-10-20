# desktop-helpers (desktop_helpers.py → src/desktop_helpers.rs)
Status: complete

## API additions (beyond the skeleton's rows in PARITY.md)
| Python | Rust | Notes |
|---|---|---|
| desktop_helpers.EMPTY_FRONT | desktop_helpers::Front::default() | |
| desktop_helpers.ax_frontmost (pid half) | desktop_helpers::ax_frontmost_for_pid | the test seam for "every probe 0 → EMPTY_FRONT" |
| Helpers._acting | Helpers::gui_action (gui_click / gui_hotkey / gui_press use it) | the worker routes every other ACTION_NAMES call (write, scroll, dragTo, ...) through gui_action |
| Helpers._action_seq, _last_click | Helpers::action_seq(), last_click() | read-only accessors for the tests and the worker |
| monkeypatch.setenv("EVE_AX_PRESS") | Helpers::ax_press_environ | None reads the process environment, as Python does |
| _LaneAdapter._thumb_before / _thumb0 / _snapshot (assigned by tests) | LaneAdapter::set_thumbs, set_snapshot | |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| test_desktop_helpers.py | all 30 | tests/desktop_helpers.rs, same names (monkeypatch of `_ax_focused_pid` etc. → `focused_pid_from` with injected probes; `h._element_at = …` → Backends at construction with shared state) | none |
| test_ax_press_fallback.py | the 10 that test desktop_helpers (the parametrized one runs its 6 cases in a loop) | same names, plus test_ax_press_on_reads_the_switch | test_refs_ride_the_snapshot_and_never_reach_its_dict: tests ax_candidates.snapshot_from_raw (the ax-candidates unit) |
| test_decision_lane.py | test_adapter_observation_uses_captured_retina_frame_in_screen_points, test_pixel_checker_has_positive_and_negative_controls | same names (ax_snapshot monkeypatch → Backends.ax_snapshot) | the rest test decision_lane (fast-lane unit) |
| test_desktop_worker.py | the Helpers half of the settle de-duplication and timing tests, test_the_jev_asker_is_configured_only_by_the_owners_switches (Helpers part) | test_open_app_auto_screenshot_settles_once, test_raw_click_still_settles_up_to_1_5s, test_timed_out_settle_reuses_the_latest_frame_without_a_second_wait, test_explicit_settle_timeout_is_false_and_does_not_wait_again_for_video, test_timed_out_settle_is_not_reused_after_a_later_input, test_timed_out_settle_is_not_reused_when_the_frame_is_stale, test_settle_is_not_reused_after_a_later_input, test_reply_carries_timing_with_helpers, test_the_jev_asker_is_configured_only_by_the_owners_switches | the worker's execute/serve/encode parts belong to the desktop worker's crate; the counts of `_settle` calls are asserted through the fake clock's sleeps instead of a spy |
| (new) | — | test_the_acting_wrap_covers_every_input_call, test_what_matches_and_norm, test_off_mac_backends_fail_like_the_missing_import, live_capture_and_ocr_read_the_real_screen (#[ignore = "live: …"]) | |

Cross-unit dependencies of these tests: the adapter and open_url tests call ax_candidates::{is_sensitive, Snapshot::get,
role_name, urlsplit} (ax-candidates unit) and test_delegate_without_a_decider_is_unavailable_and_moves_nothing calls
fast_lane::run_delegate (fast-lane unit). At this unit's commit those are still todo!(); all 55 non-live tests were run
green against local stand-ins for exactly those five functions (not committed). The live test was run on this Mac:
cg_capture, Frame::pil and vision_ocr("accurate") returned text; it then stops at ax_ffi (ax-candidates unit).

## Boundaries
- Screen capture (CGDisplayCreateImage + CGDataProvider), the window list (CGWindowListCopyWindowInfo) and Vision OCR
  (VNRecognizeTextRequest / VNImageRequestHandler) are objc2 calls in this file's `#[cfg(target_os = "macos")] mod mac`;
  AX and AppKit go through crate::ax_ffi. Off the Mac: cg_capture / vision_ocr / ax_press fail with
  ModuleNotFoundError (pyobjc's missing import), the probes answer 0 and the AX readers None, as the Python's
  `except Exception` branches did.
- pyperclip → arboard (SystemClipboard); subprocess.run → std::process::Command with output drained and a timeout
  (TimeoutExpired, child killed).

## Differences
- Frame.thumb: Pillow's `convert("L")` is reproduced exactly (fixed-point 601-2 luma); the 1/4 resize uses the image
  crate's CatmullRom (Pillow's default BICUBIC, a = -0.5) with its own edge handling and rounding, so a thumb pixel can
  differ by a grey level or so near an edge. change_box's thresholds and every test's bounds hold.
  Helpers._points uses Lanczos3 (Pillow LANCZOS) likewise.
- `{x!r}` uses a local repr without the Unicode tables: unassigned code points print as themselves instead of `\uXXXX`.
- round(x, 2) / round(x, 3) round the binary value half-to-even after scaling, not Python's correctly-rounded decimal.
- Helpers.run_plan: a `plan` that is not an object reads as "bad plan: the plan is not an object" (unavailable); Python
  raised AttributeError from `plan.get` before the try.
- Helpers.lane_first: lane_router::route and the frontmost read cannot fail in Rust, so Python's
  `RouteResult("none", reason="error:<kind>")` branch has no Rust trigger.
- install() returns the names to bind instead of writing into a namespace; the acting wrap is a flag (installed) that
  gui_action honours, so "no double wrapping" holds by construction.
- The lane's `clock=` is the Helpers' own clock read through the adapter (Python passed `self._clock`); the decider and
  step asker are lent to the lane for the duration of a delegate / run_plan call and put back after.
- Backends.run answers the return code (Python's CompletedProcess.returncode is the only field read).

## Remaining / notes for integration
- PARITY.md's Tests / Boundaries / Differences sections: merge the three sections above.
- None of this unit's code is a placeholder; the live paths depend on ax_ffi being filled (ax-candidates unit).
