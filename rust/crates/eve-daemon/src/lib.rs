//! Port of `daemon.py`: eve's main daemon, which wires the IPC server, the robot's link (BLE or USB serial, and the
//! Voice PE controller), the session state and the transcript tailer together, and lends the voice, the text door,
//! the Mini App, the explorer, the memory and the lesson workspace what each needs.
//!
//! Python's `Daemon` is one class of ~3,300 lines whose tests drive single methods on a `SimpleNamespace` stub.
//! Here [`Daemon`] is one struct, built as an `Arc` ([`Daemon::assemble`] from [`DaemonParts`], or
//! [`Daemon::new`] for the real thing), with its methods spread over one file per section of the Python class:
//!
//! * [`daemon`]: the struct, its mutable state, construction, `_spawn` and `_work_note`.
//! * [`seams`]: the board link, the text door, the microphone, the lesson workspace, the voice session and the
//!   other duck-typed objects as traits, and the module functions the tests patch.
//! * [`link`]: the board link over BLE or USB serial (with the Voice PE's mirror).
//! * [`board`]: heartbeats, resync, the status poller, the Mac microphone, the camera, the sound, identity.
//! * [`ble_handler`]: what the board sends (`_handle_ble`), the tailer's callbacks, the deferred turn end, acks.
//! * [`run`]: `run()` and shutdown, the memory facade, the dream, the memory bus and its sinks.
//! * [`voice`]: "hey eve": the ears, the conversation, the desk calls, the voice gate, room and phone notes.
//! * [`lesson`]: the robot's caption and phase, lessons over IPC, think out loud.
//! * [`explore`]: the idle explorer, thoughts on screen, snapshots and photos, the listen key, the update check.
//! * [`ipc`]: the IPC handler and one method per event.
//! * [`hooks`]: PreToolUse, PermissionRequest, the command-risk gate, Read.
//! * [`door`]: the text door and what it is lent (apps, Canvas, sharing, purchases, the Meet notetaker, the chief),
//!   the Mini App, the phone's expressions.
//! * [`inlet`]: [`seams::Inlet`] for the real `TelegramInlet`.
//! * [`agents`]: the computer-use agent factory and its bodies (reflex, Chrome lane, web reader, desk lane).
//! * [`apps`]: eve's apps' hubs (lights, Spotify, the TV, the camera, the car) and the Voice PE's say.
//! * [`startup`]: the code origin and the permission summary logged at start.

// Integration removes this allow: the skeleton's todo!() bodies leave parameters and imports unused until filled.
#![allow(unused_variables, unused_imports, dead_code, clippy::unused_async)]

pub mod agents;
pub mod apps;
pub mod ble_handler;
pub mod board;
pub mod daemon;
pub mod door;
pub mod explore;
pub mod hooks;
pub mod inlet;
pub mod ipc;
pub mod lesson;
pub mod link;
pub mod run;
pub mod seams;
pub mod startup;
pub mod voice;

pub use daemon::{Daemon, DaemonOptions, DaemonParts, DaemonState};
pub use startup::{code_origin, log_permission_config_summary};

/// Entry text is prefixed with a 2-byte marker ("> ", "@ ", "+ ") before being stored. Budget the user-supplied
/// portion so the full entry stays within the firmware's line buffer without _format_entry needing to re-truncate.
pub const ENTRY_PAYLOAD_MAX_BYTES: usize = eve_protocol::ENTRY_MAX_BYTES - 2;

/// Think out loud: how long the learning server's thread waits for the loop to take a listen request.
pub const THINK_ALOUD_CALL_SECS: f64 = 12.0;
/// The name of a conversation task opened for listening (so a stop can cancel one still connecting).
pub const THINK_ALOUD_TASK: &str = "think-aloud-conversation";
/// Memory (owner, 2026-09-23). A voice conversation opens with today's lines only when they are built this fast;
/// shutdown gives the close markers and then the background work (the dream) this long.
pub const TODAY_BUILD_SECS: f64 = 0.05;
pub const CLOSE_MARKERS_SECS: f64 = 3.0;
pub const DRAIN_SECS: f64 = 12.0;

/// `Daemon.SNAP_TIMEOUT_SECS`: how long the diary waits for the board's one full-size frame.
pub const SNAP_TIMEOUT_SECS: f64 = 3.0;

/// a Telegram message is at most 4096 characters
pub const CHAT_MAX_CHARS: usize = 4000;
/// an eve MCP call waits the chat's ask_timeout_secs plus this (eve_mcp.py)
pub const MCP_CALL_MARGIN_SECS: f64 = 120.0;
/// the daemon answers within ask_timeout_secs plus this: before the MCP side gives up
pub const MCP_CALL_BOUND_SECS: f64 = 90.0;
pub const MCP_STILL_RUNNING: &str = "that call is still running in eve, so its outcome is unknown: it may yet run or finish. \
Do not retry it; ask the owner what happened.";

/// `_status_poller`'s POLL_INTERVAL and MISSED_LIMIT (~2min of unanswered polls before reconnecting).
pub const STATUS_POLL_INTERVAL_SECS: f64 = 60.0;
pub const STATUS_MISSED_LIMIT: u32 = 2;
/// `_update_check_loop`'s INTERVAL.
pub const UPDATE_CHECK_INTERVAL_SECS: f64 = 24.0 * 3600.0;
/// `_ipc_turn_end`'s CELEBRATE_SECS and the deferral of the running→0 flip.
pub const CELEBRATE_SECS: f64 = 5.0;
pub const TURN_END_DEFER_SECS: f64 = 15.0;
/// `_motion_trace` keeps the last this many poses.
pub const MOTION_TRACE_MAX: usize = 400;
