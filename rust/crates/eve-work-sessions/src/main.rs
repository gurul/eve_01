//! `eve-work-sessions launch <sid>`: a Work session's first moment (Python's
//! `python -m eve_bridge.work_sessions launch <sid>`). Reads and deletes the task, then becomes Claude Code.

fn main() {
    std::process::exit(eve_work_sessions::main());
}
