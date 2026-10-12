"""W4 (docs/work-sessions-gates.md): a Work session, live on this Mac, through the same HTTP door Eve Link uses.

Run by the orchestrator only: it opens one Terminal window running Claude Code in ~/Documents/personal/work.

1. Finds the daemon's link port and reads the link token (obsbot_live_check.py's way; the token is never printed).
2. ``POST /api/link/sessions/start`` with a harmless task (``TASK``).
3. Waits (at most ``LIST_SECS``) for the session in ``GET /api/link/sessions`` with Claude running (state not
   "starting"), and checks the process's working folder is the work folder.
4. Waits (at most ``TURN_SECS``) for Claude's answer in ``GET /api/link/sessions/<id>``.
5. Replies through ``POST .../reply`` and waits for the reply and Claude's next answer in detail.
6. Stops it (``POST .../stop``) and checks it leaves the list.

Prints counts, states and timings only, never a transcript line, then WORK_LIVE_OK. Exits 1 with FAIL: on the first
step that does not hold, after stopping the session it started.

    bridge/.venv/bin/python bridge/tools/work_sessions_check.py
"""

from __future__ import annotations

import json
import subprocess
import sys
import time
from pathlib import Path
from typing import Any, Optional

sys.path.insert(0, str(Path(__file__).resolve().parent))
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "src"))

import obsbot_live_check as live  # noqa: E402 — the token path, the port finder and the HTTP call

from eve_bridge import work_sessions  # noqa: E402

SESSIONS = "/api/link/sessions"
TASK = "Reply with the single word ready. Do not use any tools."
REPLY = "Now reply with the single word done. Do not use any tools."
LIST_SECS = 15.0
TURN_SECS = 90.0
GONE_SECS = 15.0
POLL_SECS = 1.0


def session_row(port: int, token: str, sid: str) -> Optional[dict[str, Any]]:
    status, raw, _ = live.call(port, token, "GET", SESSIONS, timeout=20)
    if status != 200:
        raise RuntimeError(f"list -> HTTP {status}")
    return next((s for s in json.loads(raw).get("sessions", []) if s.get("id") == sid), None)


def detail(port: int, token: str, sid: str) -> dict[str, Any]:
    status, raw, _ = live.call(port, token, "GET", f"{SESSIONS}/{sid}", timeout=20)
    if status != 200:
        raise RuntimeError(f"detail -> HTTP {status}")
    return json.loads(raw)


def counts(d: dict[str, Any]) -> tuple[int, int]:
    turns = d.get("turns", [])
    return sum(t.get("role") == "owner" for t in turns), sum(t.get("role") == "claude" for t in turns)


def cwd_of(pid: int) -> str:
    out = subprocess.run(["lsof", "-a", "-p", str(pid), "-d", "cwd", "-Fn"], capture_output=True, text=True).stdout
    return next((line[1:] for line in out.splitlines() if line.startswith("n/")), "")


def wait_for(what: str, secs: float, probe: Any) -> Any:
    deadline = time.monotonic() + secs
    while time.monotonic() < deadline:
        got = probe()
        if got:
            return got
        time.sleep(POLL_SECS)
    raise RuntimeError(f"{what}: not within {secs:.0f} s")


def main() -> int:
    token = json.loads(live.TOKEN_PATH.read_text()).get("token", "")
    if not token:
        live.fail("no link token")
    port = None
    for p in live.daemon_ports():
        try:
            status, raw, _ = live.call(p, token, "GET", SESSIONS, timeout=10)
        except OSError:
            continue
        if status == 200 and b'"sessions"' in raw:
            port = p
            break
    if port is None:
        live.fail("no daemon port answers /api/link/sessions (is the daemon on this branch, Work sessions on?)")
    before = len(json.loads(raw).get("sessions", []))
    print(f"daemon port found; {before} sessions listed before")

    t0 = time.monotonic()
    status, raw, _ = live.call(port, token, "POST", f"{SESSIONS}/start",
                               {"task": TASK, "client_id": f"w4-{int(time.time())}"})
    if status != 200:
        live.fail(f"start -> HTTP {status}")
    sid = json.loads(raw)["session"]["id"]
    print(f"start: 200 in {time.monotonic() - t0:.1f} s, state {json.loads(raw)['session']['state']}")
    failure = ""
    try:
        row = wait_for("listed with claude running", LIST_SECS,
                       lambda: (lambda r: r if r and r.get("state") != "starting" else None)(
                           session_row(port, token, sid)))
        print(f"listed: state {row['state']} after {time.monotonic() - t0:.1f} s, folder {row['folder']}, "
              f"can_reply {row['can_reply']}, can_stop {row['can_stop']}")
        if row["folder"] != "work" or not row["from_work"] or not row["can_reply"]:
            raise RuntimeError("the row is not a Work session that can be replied to")
        pid = (work_sessions.session_pids() or {}).get(sid)
        if pid is None:
            raise RuntimeError("ps does not show claude with this session's id")
        here = Path(cwd_of(pid)).resolve()
        if here != work_sessions.work_root().resolve():
            raise RuntimeError("claude is not working in ~/Documents/personal/work")
        print("process: claude found by its session id, working in personal/work")
        first = wait_for("Claude's first answer", TURN_SECS,
                         lambda: (lambda d: d if counts(d)[1] >= 1 else None)(detail(port, token, sid)))
        print(f"detail: {counts(first)[0]} owner and {counts(first)[1]} claude turns after "
              f"{time.monotonic() - t0:.1f} s")
        wait_for("idle after the first answer", TURN_SECS,
                 lambda: (lambda r: r if r and r.get("state") == "idle" else None)(session_row(port, token, sid)))
        status, _, _ = live.call(port, token, "POST", f"{SESSIONS}/{sid}/reply", {"text": REPLY})
        if status != 200:
            raise RuntimeError(f"reply -> HTTP {status}")
        owners, claudes = counts(first)
        second = wait_for("the reply and Claude's answer to it", TURN_SECS,
                          lambda: (lambda d: d if counts(d)[0] > owners and counts(d)[1] > claudes else None)(
                              detail(port, token, sid)))
        print(f"reply: 200; detail now {counts(second)[0]} owner and {counts(second)[1]} claude turns")
    except (RuntimeError, OSError, ValueError, KeyError) as e:
        failure = str(e) if isinstance(e, RuntimeError) else type(e).__name__
    status, raw, _ = live.call(port, token, "POST", f"{SESSIONS}/{sid}/stop", {})
    print(f"stop: HTTP {status}")
    if failure:
        live.fail(failure)
    if status != 200 or not json.loads(raw).get("stopped"):
        live.fail("stop did not answer stopped")
    try:
        wait_for("gone from the list", GONE_SECS, lambda: session_row(port, token, sid) is None)
    except RuntimeError as e:
        live.fail(str(e))
    print("stopped: gone from the list")
    print("WORK_LIVE_OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
