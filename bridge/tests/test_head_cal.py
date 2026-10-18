"""head_cal.py: the head calibration page's API, driven with a fake daemon."""
from __future__ import annotations

from typing import Any

from eve_bridge import head_cal


def fake(answer: dict[str, Any] | None):
    sent: list[dict[str, Any]] = []

    def ipc(req: dict[str, Any]) -> dict[str, Any] | None:
        sent.append(req)
        return answer
    return ipc, sent


def test_each_extreme_holds_a_pose_inside_the_boards_limits() -> None:
    for name, (yaw, pitch) in head_cal.POSES.items():
        keys = head_cal.pose_keys(name)
        assert len(keys) <= 8 and keys[-1][0] <= 8000           # motion.h: 8 keys inside kBoutMaxMs
        assert all(k[1] == yaw and k[2] == pitch for k in keys)
        assert abs(yaw) <= 100 and 5 <= pitch <= 85
    assert head_cal.POSES["down"][1] < head_cal.POSES["center"][1] < head_cal.POSES["up"][1]
    assert head_cal.POSES["left"][0] > 0 > head_cal.POSES["right"][0]   # body.cpp: +yaw is the viewer's left


def test_a_tap_moves_the_head_and_a_bad_pose_is_refused() -> None:
    ipc, sent = fake({"ok": True})
    assert head_cal.handle("/api/move", {"pose": "down"}, ipc) == (200, {"ok": True, "error": None, "pose": "down"})
    assert sent[-1]["evt"] == "move" and sent[-1]["kind"] == "keys" and sent[-1]["keys"][0][2] == 5
    code, out = head_cal.handle("/api/move", {"pose": "sideways"}, ipc)
    assert code == 400 and not out["ok"] and len(sent) == 1


def test_a_flip_goes_to_the_board_and_a_read_sends_nothing_to_set() -> None:
    ipc, sent = fake({"ok": True, "pitch_rev": False, "yaw_rev": False})
    assert head_cal.handle("/api/axis", {"pitch_rev": False}, ipc)[1]["pitch_rev"] is False
    assert sent[-1] == {"evt": "axis", "pitch_rev": False}
    head_cal.handle("/api/axis", {}, ipc)
    assert sent[-1] == {"evt": "axis"}


def test_no_daemon_is_said_plainly() -> None:
    ipc, _ = fake(None)
    code, out = head_cal.handle("/api/move", {"pose": "up"}, ipc)
    assert code == 502 and "not reachable" in out["error"]


def test_only_the_page_itself_may_call_the_api() -> None:
    """Review A9 (2026-10-18): any web page could POST /api/axis (text/plain, no preflight) and flip the robot's
    axes. learning/server.py's guard: the Host is this server's, and an Origin, when sent, is too."""
    ok = head_cal.allowed
    assert ok({"Host": "127.0.0.1:8766"}, 8766)                                   # curl, or the page's own GET
    assert ok({"Host": "127.0.0.1:8766", "Origin": "http://127.0.0.1:8766"}, 8766)
    assert not ok({"Host": "127.0.0.1:8766", "Origin": "https://evil.example"}, 8766)
    assert not ok({"Host": "evil.example:8766"}, 8766)                             # DNS rebinding
    assert not ok({}, 8766)
