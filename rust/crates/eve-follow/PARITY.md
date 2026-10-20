# eve-follow
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/follow.py` | `src/lib.rs` (tests in `src/tests.rs`) |

## API
| Python | Rust | Notes |
|---|---|---|
| `FOLLOW_PHASES` | `FOLLOW_PHASES: &[&str]` + `is_follow_phase` | same four phases |
| `REFUSED_DEG` … `MAX_YAW` (all constants) | same-named `pub const`s | values verbatim; counts are `usize`, angles in whole degrees `i64`, the rest `f64` |
| `PRESENCE_TOPIC`, `DEFAULT_PRESENCE_FILE` | same-named `&str` consts | `/eve/presence`, `~/.config/eve-bridge/presence.json` |
| `Sender` | `Sender` (= `eve_head::Sender`) | `Arc<dyn Fn(Value) -> BoxFuture<bool>>` |
| `Publisher` | `Publisher` | `Arc<dyn Fn(&str, Map) -> Result<(), String>>`; an `Err` or a panic is logged at debug and ignored |
| `_reachable` | `reachable` (private) | |
| `class Track` | `struct Track` (+ `Track::new`) | dataclass defaults; `speed` is `speed()` |
| `class PresenceMap` | `struct PresenceMap` | `PresenceMap::new(path)` / `with_wall(path, wall)`; cells in an `IndexMap` so equal weights keep the dict's insertion order |
| `PresenceMap.BIN`, `HALF_LIFE_DAYS`, `MAX_CELLS` | associated consts | |
| `PresenceMap.cell` | `PresenceMap::cell` | `int(round(x))` is round-half-to-even |
| `PresenceMap._decay` | `decay` (private) | |
| `PresenceMap.note` | `note(yaw, pitch)` / `note_weight(yaw, pitch, weight)` | |
| `PresenceMap.top` | `top(n)` / `top_excluding(n, exclude)` / `top_with(n, exclude, min_weight)` | stable sort, heaviest first |
| `PresenceMap.load` | `load(self) -> Self` | same accepted shapes as `int()`/`float()`; any failure leaves the cells empty |
| `PresenceMap.save` | `save(&mut self) -> bool` | writes `presence.tmp` then renames; bytes identical to `json.dumps` (checked against the Python) |
| `class _Search` | `struct Search` | |
| `class SpeakerFollower` | `struct SpeakerFollower` | `new(send)` / `with(send, FollowerOptions)`; state behind a `parking_lot::Mutex` never held across an await, so it is shared as `Arc<SpeakerFollower>` |
| `SpeakerFollower.__init__` kwargs | `FollowerOptions { clock, enabled, blocked, presence, publish }` | same defaults (monotonic clock, enabled, nobody blocking, empty in-memory map, no bus) |
| attributes `phase`, `holding`, `track`, `search`, `commanded`, `last_move_at`, `moves`, `searches`, `found_by`, `owner_hold_until`, `_acquired`, `presence` | `phase()`, `holding()`, `track()`, `search()`, `commanded()`, `last_move_at()`, `moves()`, `searches()`, `found_by()`, `owner_hold_until()`, `acquired()`, `with_presence(f)` | read-outs (clones) |
| `SpeakerFollower.active` | `active()` | |
| `SpeakerFollower.on_phase` | `async on_phase(&str)` | |
| `SpeakerFollower.owner_moved` | `owner_moved(f64)` | sync, for `Head::set_on_move` |
| `SpeakerFollower.release` | `async release(&str)` | |
| `SpeakerFollower._say` | `say` + `emit` (private) | the message is built under the lock and published after it is dropped; key order `state, time, phase, extra…, yaw, pitch, speed` |
| `SpeakerFollower._pick` | `pick` (private) | min/max pick the first of equals, as Python |
| `SpeakerFollower._expected` | `expected` (private) | |
| `SpeakerFollower._update` | `update` (private) | |
| `SpeakerFollower.on_faces` | `async on_faces(&[Face], Option<f64>, Option<f64>)` | faces as `Face { bx, by, size, conf }` (vision's `FaceResult` shape; the daemon maps one to the other) |
| `SpeakerFollower._look` | `look` (private) | |
| `SpeakerFollower.plan_search` | `plan_search(&Track) -> Vec<Place>` | |
| `SpeakerFollower._plan_opening` | `plan_opening` (private) | |
| `SpeakerFollower._lost` | `lost` (private) | |
| `configured` | `configured(Option<&HashMap>)` | `None` reads the process env; `EVE_FOLLOW_SPEAKER`, default on, off for 0/false/no/off |
| `presence_path` | `presence_path(Option<&HashMap>)` | `EVE_PRESENCE_FILE`, `~` and `~user` expanded like `Path.expanduser` |
| `time.monotonic` / `time.time` defaults | `monotonic()` / `wall_time()` | |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_follow.py` | test_it_only_follows_while_eve_should_be_looking_at_its_person, test_it_converges_on_a_still_person_and_then_sits_still, test_one_sighting_is_not_a_person_and_an_outlier_does_not_move_the_head, test_frames_from_a_swinging_head_are_not_evidence, test_a_walking_person_is_led_not_chased, test_a_second_face_does_not_steal_the_gaze, test_weak_or_tiny_faces_are_not_people, test_an_asked_for_pose_is_the_owners_for_as_long_as_it_is_held, test_a_track_that_aged_while_standing_down_is_forgotten_not_searched_on, test_leaving_a_follow_phase_hands_the_head_back_at_once, test_someone_who_leaves_fast_is_followed_the_way_they_went, test_someone_who_vanishes_from_stillness_is_looked_for_where_they_usually_are, test_a_conversation_that_opens_on_an_empty_frame_looks_where_they_usually_are, test_a_seated_owner_below_the_usual_place_is_found_by_looking_a_little_lower, test_every_return_from_thinking_with_nobody_in_view_gets_a_fresh_opening, test_a_learned_place_past_the_limits_is_brought_back_inside_them, test_a_refused_look_is_noticed_and_the_head_is_not_walked_to_its_limits, test_it_gives_the_head_back_and_does_not_keep_scanning_an_empty_room, test_the_presence_map_learns_decays_and_survives_a_restart, test_it_never_looks_behind_itself_and_survives_a_dead_link_the_switch_and_the_bus, test_an_asked_for_pose_reaches_the_follower_through_the_head_and_presence_is_a_bus_topic | same names in `src/tests.rs` (the last one also wires `Head::set_on_move` to `owner_moved`), plus test_saved_file_is_byte_for_byte_what_python_writes, test_load_reads_what_python_reads_and_refuses_what_it_refuses, test_python_number_formats, test_configured_and_presence_path_read_the_environment_like_python, test_owner_moved_with_a_nan_or_negative_hold_ends_at_once, test_the_followers_futures_are_send | test_the_tracker_hands_the_follower_every_face_with_the_frames_pose_and_never_dies_of_it: it tests vision's FaceTracker, covered in eve-vision by test_tracker_identity_fills_who_and_on_faces_gets_every_face and test_tracker_on_faces_failure_never_stops_tracker |
| `bridge/tests/test_desk_call.py` (ref) | test_during_a_desk_call_eve_turns_to_the_face_it_sees (follower part) | test_during_a_desk_call_eve_turns_to_the_face_it_sees (phases driven directly) | the desk call itself, and test_a_desk_call_is_a_conversation_to_the_head_follower, belong to eve-desk-call |

## Boundaries
None

## Differences
- A presence file whose top level is not an object, or whose `cells` is a non-empty non-object, loads as an empty map; Python raises `AttributeError` out of `load()` there.
- `serde_json` rejects the non-standard `NaN`/`Infinity` tokens `json.loads` accepts, so such a file loads as an empty map.
- A `~` path whose home cannot be found is left as it is; `Path.expanduser` raises `RuntimeError`.
- Faces arrive as `follow::Face` instead of `vision.FaceResult`; the fields and meaning are the same.
