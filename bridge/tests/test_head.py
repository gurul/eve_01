"""head.py: pose geometry, the look command, relative moves from the echoed pose,
and the look_around / find routines with a fake scene and a fake clock."""

from __future__ import annotations

import asyncio

import pytest

from eve_bridge.head import (
    PITCH_LEVEL,
    YAW_MAX,
    Head,
    build_look_cmd,
    clamp_pose,
    direction_word,
    find,
    look_around,
    target_pose,
)


class Clock:
    def __init__(self) -> None:
        self.t = 0.0

    def __call__(self) -> float:
        return self.t

    async def sleep(self, secs: float) -> None:
        self.t += secs


class Wire:
    def __init__(self, ok: bool = True) -> None:
        self.sent: list[dict] = []
        self.ok = ok

    async def send(self, cmd: dict) -> bool:
        self.sent.append(cmd)
        return self.ok


def head(wire: Wire | None = None, clock: Clock | None = None, connected: bool = True) -> Head:
    clock = clock or Clock()
    return Head((wire or Wire()).send, connected=lambda: connected, clock=clock, sleep=clock.sleep)


def test_clamp_and_command() -> None:
    assert clamp_pose(-200, 100) == (-YAW_MAX, 85, True)
    assert clamp_pose(30.4, 44.6) == (30, 45, False)
    assert build_look_cmd(-60, 50, 99999) == {"cmd": "look", "yaw": -60, "pitch": 50, "hold": 65535}


def test_direction_words_follow_the_robot_frame() -> None:
    assert direction_word(0) == "straight ahead"
    assert direction_word(-25) == "a little to my left"
    assert direction_word(60) == "to my right"
    assert direction_word(-100) == "far to my left"
    assert direction_word(120) == "behind me on my right"


def test_target_pose_centres_a_point_like_the_firmware() -> None:
    # Right edge of a frame taken straight ahead: half the 66° field of view to the right.
    assert target_pose(0, 45, 100, 0) == (33, 45, False)
    # Bottom of the frame: look down by half the vertical field (24.75°).
    assert target_pose(0, 45, 0, 100) == (0, 20, False)
    assert target_pose(110, 45, 100, 0) == (YAW_MAX, 45, True)


def test_absolute_and_relative_moves_use_the_echoed_pose() -> None:
    async def go():
        wire = Wire()
        h = head(wire)
        r = await h.move(-60, 30, hold_secs=10)
        assert r == {"ok": True, "yaw": -60, "pitch": 30, "facing": "to my left", "hold_secs": 10.0}
        assert wire.sent[-1] == {"cmd": "look", "yaw": -60, "pitch": 30, "hold": 10000}
        h.observe(-55, 32)                     # the board says where the head really is
        r = await h.move(-15, None, relative=True)
        assert (r["yaw"], r["pitch"]) == (-70, 32)   # from -55, not from the commanded -60
        r = await h.move(-90, 0, relative=True)
        assert r["yaw"] == -YAW_MAX and "note" in r  # clamped: says it hit the limit
        h.observe(True, "x")                   # junk is ignored
        assert (h.yaw, h.pitch) == (-120.0, 32.0)
    asyncio.run(go())


def test_move_refuses_without_a_board_and_reports_a_lost_send() -> None:
    async def go():
        assert (await head(connected=False).move(10, 45))["ok"] is False
        r = await head(Wire(ok=False)).move(10, 45)
        assert r == {"ok": False, "reason": "the move did not reach the robot"}
    asyncio.run(go())


def test_hold_is_capped_and_zero_hands_the_head_back() -> None:
    async def go():
        wire = Wire()
        h = head(wire)
        await h.move(0, 45, hold_secs=500)
        assert wire.sent[-1]["hold"] == 60000
        await h.move(0, 45, hold_secs=0)
        assert wire.sent[-1]["hold"] == 0
    asyncio.run(go())


class FakeScene:
    """look/locate answers keyed by the yaw the head was sent to last."""

    def __init__(self, h: Head, views: dict[int, str] | None = None, found_at: int | None = None,
                 lost: bool = False) -> None:
        self.h, self.views, self.found_at, self.lost = h, views or {}, found_at, lost
        self.looks: list[tuple[float, float | None]] = []
        self.locates: list[tuple[str, float | None]] = []

    async def look(self, fresh_secs: float = 4.0, newer_than: float | None = None) -> dict:
        self.looks.append((fresh_secs, newer_than))
        if self.lost:
            return {"ok": False, "reason": "the camera is not connected, so nothing can be seen right now"}
        return {"ok": True, "view": self.views.get(int(self.h.yaw), "a wall")}

    async def locate(self, target: str, newer_than: float | None = None) -> dict:
        self.locates.append((target, newer_than))
        if int(self.h.yaw) == self.found_at:
            return {"ok": True, "visible": True, "x": 50.0, "y": -40.0, "what": "a green mug",
                    "yaw": self.h.yaw, "pitch": self.h.pitch}
        return {"ok": True, "visible": False, "x": 0.0, "y": 0.0, "what": "", "yaw": self.h.yaw,
                "pitch": self.h.pitch}


def test_look_around_describes_each_stop_after_it_settles_then_faces_the_owner() -> None:
    async def go():
        clock, wire = Clock(), Wire()
        h = head(wire, clock)
        scene = FakeScene(h, {-100: "a door", 0: "the owner at the desk", 100: "a window"})
        out = await look_around(h, scene)
        assert out["ok"] is True
        assert [v["direction"] for v in out["views"]] == [
            "far to my left", "to my left", "straight ahead", "to my right", "far to my right"]
        assert out["views"][0]["view"] == "a door" and out["views"][4]["view"] == "a window"
        # Every view was asked for from a frame newer than the moment the head stopped moving.
        assert [nt for _, nt in scene.looks] == [0.0, 1.0, 2.0, 3.0, 4.0]
        assert wire.sent[-1] == {"cmd": "look", "yaw": 0, "pitch": PITCH_LEVEL, "hold": 3000}
    asyncio.run(go())


def test_look_around_stops_when_the_camera_is_gone() -> None:
    async def go():
        h = head()
        out = await look_around(h, FakeScene(h, lost=True))
        assert out["ok"] is False and "not connected" in out["reason"]
    asyncio.run(go())


def test_find_checks_the_current_view_first_then_sweeps_and_centres() -> None:
    async def go():
        clock, wire = Clock(), Wire()
        h = head(wire, clock)
        scene = FakeScene(h, found_at=50)
        out = await find(h, scene, "my green mug")
        assert out["found"] is True and out["what"] == "a green mug"
        # Current view first (no newer_than), then the sweep until the sighting at yaw 50.
        assert scene.locates[0] == ("my green mug", None)
        assert [nt is None for _, nt in scene.locates] == [True, False, False, False, False]
        # x=50 → +16.5° from yaw 50; y=-40 (above centre) → +9.9° pitch from 50.
        assert (out["yaw"], out["pitch"]) == (66, 60)
        assert wire.sent[-1]["hold"] == 15000
    asyncio.run(go())


def test_find_reports_not_found_and_faces_the_owner() -> None:
    async def go():
        wire = Wire()
        h = head(wire)
        out = await find(h, FakeScene(h, found_at=None), "a unicorn")
        assert out["found"] is False and out["searched_stops"] == 6
        assert wire.sent[-1]["yaw"] == 0
        assert (await find(h, FakeScene(h), "  "))["ok"] is False
    asyncio.run(go())


@pytest.mark.parametrize("yaw", [-120, 120])
def test_behind_is_the_neck_limit(yaw: int) -> None:
    assert clamp_pose(yaw * 1.5, 45)[0] == yaw
