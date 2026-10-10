"""follow.py: in a conversation eve keeps its eyes on whoever is talking to it, and works out where
they went when it loses them.

A fake clock and a recording sender; faces are vision.FaceResult-shaped (bx, by, size, conf). One degree
of yaw is about 3 units of bx (a 66° lens: 100 units = 33°)."""

from __future__ import annotations

import asyncio
import json
from pathlib import Path

from eve_bridge import follow as fo
from eve_bridge.follow import PresenceMap, SpeakerFollower
from eve_bridge.vision import FaceResult


def face_at(true_yaw: float, true_pitch: float, pose_yaw: float, pose_pitch: float, size: int = 22, conf: int = 90) -> FaceResult:
    """The box a face at (true_yaw, true_pitch) makes in a frame taken at the given pose."""
    bx = (true_yaw - pose_yaw) / 33.0 * 100.0
    by = -(true_pitch - pose_pitch) / 24.75 * 100.0
    return FaceResult(bx=int(round(max(-100, min(100, bx)))), by=int(round(max(-100, min(100, by)))), size=size, conf=conf)


class Bench:
    def __init__(self, **kw) -> None:
        self.t = 100.0
        self.sent: list[dict] = []
        self.events: list[tuple[str, dict]] = []
        self.ok = True
        self.pose = (0.0, 45.0)               # where the head is: every accepted look moves it there at once
        kw.setdefault("publish", lambda topic, msg: self.events.append((topic, msg)))
        self.f = SpeakerFollower(self.send, clock=lambda: self.t, **kw)

    async def send(self, cmd: dict) -> bool:
        self.sent.append(cmd)
        if self.ok and cmd["cmd"] == "look":
            self.pose = (float(cmd["yaw"]), float(cmd["pitch"]))
        return self.ok

    def see(self, where, dt: float = 0.25, extra=()) -> None:
        """One frame: a person at absolute `where` (or None for nobody), seen from the current pose."""
        self.t += dt
        faces = list(extra)
        if where is not None and abs(where[0] - self.pose[0]) <= 33 and abs(where[1] - self.pose[1]) <= 24:
            faces.append(face_at(where[0], where[1], *self.pose))
        asyncio.run(self.f.on_faces(faces, *self.pose))

    def watch(self, where, secs: float, dt: float = 0.25) -> None:
        for _ in range(int(secs / dt)):
            self.see(where(self.t) if callable(where) else where, dt)

    def phase(self, name: str) -> None:
        asyncio.run(self.f.on_phase(name))

    def looks(self) -> list[tuple[int, int, int]]:
        return [(c["yaw"], c["pitch"], c["hold"]) for c in self.sent if c["cmd"] == "look"]

    def states(self) -> list[str]:
        return [m["state"] + (":" + m["how"] if "how" in m else "") for _t, m in self.events]


CHAIR = (-23.0, 38.0)          # where the owner really sat on 2026-09-21: to eve's left, a little below level


def test_it_only_follows_while_eve_should_be_looking_at_its_person() -> None:
    for phase in ("thinking", "working", "idle", "done", "error"):
        b = Bench()
        b.phase(phase)
        b.watch(CHAIR, 4.0)
        assert b.sent == [], phase
    for phase in fo.FOLLOW_PHASES:
        b = Bench()
        b.phase(phase)
        b.watch(CHAIR, 6.0)
        assert b.f.holding and abs(b.pose[0] - CHAIR[0]) <= fo.DEADBAND_YAW, (phase, b.pose)


def test_it_converges_on_a_still_person_and_then_sits_still() -> None:
    b = Bench()
    b.phase("listening")
    b.watch(CHAIR, 8.0)
    assert abs(b.pose[0] - CHAIR[0]) <= fo.DEADBAND_YAW and abs(b.pose[1] - CHAIR[1]) <= fo.DEADBAND_PITCH
    moves = b.f.moves
    assert moves <= 3                                          # it walked there in steps, it did not hunt
    b.watch(CHAIR, 10.0)
    assert b.f.moves == moves                                  # ten more seconds of a still person: not one twitch
    assert all(abs(y2 - y1) <= fo.MAX_STEP_DEG + 1 for (y1, *_), (y2, *_) in zip(b.looks(), b.looks()[1:], strict=False))
    assert b.looks()[-1][2] == 4000                            # and the hold is still being renewed
    assert b.states() == ["seen"]


def test_one_sighting_is_not_a_person_and_an_outlier_does_not_move_the_head() -> None:
    b = Bench()
    b.phase("listening")
    b.see(CHAIR)
    assert b.sent == [] and b.f.track is not None and b.f.track.confirmed == 1
    b.watch(CHAIR, 8.0)
    settled = b.pose
    moves_before = b.f.moves
    b.see((5.0, 50.0), dt=1.5)                                 # one frame says they are 25° away: a blur, a poster
    assert b.pose == settled and b.f.track.doubt is not None
    b.see(CHAIR, dt=0.3)
    assert b.pose == settled and b.f.track.doubt is None and b.f.moves == moves_before
    b.see((5.0, 50.0), dt=1.5)
    b.see((5.0, 50.0), dt=0.3)                                 # twice in a row: that is where they are now
    assert b.pose[0] > settled[0]


def test_frames_from_a_swinging_head_are_not_evidence() -> None:
    b = Bench()
    b.phase("listening")
    b.watch(CHAIR, 3.0)
    assert b.f.moves >= 1
    n = len(b.sent)
    b.t = b.f.last_move_at                                    # rewind to the moment of the move
    for _ in range(4):                                         # 1.0 s of frames that claim they are somewhere wild
        b.see((60.0, 70.0), dt=0.25)
    assert len(b.sent) == n and b.f.track.doubt is None        # inside SETTLE_SECS: not even an outlier


def test_a_walking_person_is_led_not_chased() -> None:
    b = Bench()
    b.phase("speaking")
    walk = lambda t: (-30.0 + 6.0 * (t - 100.0), 45.0)         # 6°/s to eve's right  # noqa: E731
    b.watch(walk, 10.0)
    target = walk(b.t)[0]
    assert b.f.track.vyaw > 3.0                                # it knows they are moving, and which way
    assert abs(b.pose[0] - target) <= 20 and b.f.moves >= 3    # still in frame the whole way
    assert "lost" not in b.states()


def test_a_second_face_does_not_steal_the_gaze() -> None:
    b = Bench()
    b.phase("listening")
    b.watch(CHAIR, 6.0)
    settled = b.pose
    for _ in range(12):                                        # a passer-by, bigger and nearer, for three seconds
        b.see(CHAIR, extra=[face_at(settled[0] + 25, 45, *b.pose, size=45, conf=95)])
    assert abs(b.pose[0] - settled[0]) <= fo.DEADBAND_YAW
    fresh = Bench()
    fresh.phase("listening")
    for _ in range(6):                                         # no history: the largest, which is the nearest
        fresh.see((-25.0, 45.0), extra=[face_at(20, 45, *fresh.pose, size=45, conf=95)])
    assert fresh.pose[0] > 0


def test_weak_or_tiny_faces_are_not_people() -> None:
    b = Bench()
    b.phase("listening")
    for _ in range(8):
        b.t += 0.25
        asyncio.run(b.f.on_faces([FaceResult(70, 0, 20, 20), FaceResult(-70, 0, 2, 95)], 0.0, 45.0))
    # never a track; the only looks are the empty-map opening scan, which keeps the head's own yaw (a
    # followed face at bx=70 would have turned it)
    assert b.f.track is None and all(c["yaw"] == 0 for c in b.sent)


def test_an_asked_for_pose_is_the_owners_for_as_long_as_it_is_held() -> None:
    b = Bench()
    b.phase("listening")
    b.watch(CHAIR, 6.0)
    b.f.owner_moved(15.0)                                      # "look left" → move_head, hold 15 s
    b.pose = (-60.0, 45.0)
    n = len(b.sent)
    b.watch((-50.0, 45.0), 10.0)                               # a face right there, and it does not move
    assert len(b.sent) == n
    b.watch((-50.0, 45.0), 8.0)                                # the hold has run out
    assert len(b.sent) > n and b.f.holding


def test_a_track_that_aged_while_standing_down_is_forgotten_not_searched_on() -> None:
    b = Bench()
    b.phase("listening")
    start = b.t
    b.watch(lambda t: (0.0 + 8.0 * (t - start), 45.0), 4.0)    # they were drifting right …
    b.f.owner_moved(15.0)                                      # … then "look left", held for 15 s
    assert b.f.track is None
    b.pose = (-70.0, 45.0)
    b.watch(None, 17.0, dt=0.5)                                # the hold runs out on an empty frame
    assert b.f.searches == 0 and b.f.search is None            # no search on history
    thinking = Bench()
    thinking.phase("listening")
    thinking.watch(CHAIR, 6.0)
    thinking.phase("thinking")
    thinking.watch(None, 12.0, dt=0.5)                         # a long think: the follower is standing down
    thinking.phase("speaking")
    thinking.watch(None, 3.0, dt=0.5)
    assert thinking.f.searches == 0 and thinking.f.track is None


def test_leaving_a_follow_phase_hands_the_head_back_at_once() -> None:
    b = Bench()
    b.phase("listening")
    b.watch(CHAIR, 6.0)
    b.phase("thinking")                                        # the board glances aside here: let it
    assert b.looks()[-1][2] == 0 and not b.f.holding
    why = {"v": ""}
    c = Bench(blocked=lambda: why["v"])
    c.phase("listening")
    c.watch(CHAIR, 6.0)
    why["v"] = "explore"
    c.see(CHAIR, dt=1.0)
    assert c.looks()[-1][2] == 0 and not c.f.holding           # someone else owns the head: released, then silent
    n = len(c.sent)
    c.watch(CHAIR, 3.0)
    assert len(c.sent) == n


def test_someone_who_leaves_fast_is_followed_the_way_they_went() -> None:
    b = Bench()
    b.phase("listening")
    b.watch((0.0, 45.0), 4.0)
    start = b.t
    dash = lambda t: (0.0 + 40.0 * (t - start), 45.0)          # 40°/s to eve's right: out of frame within a second  # noqa: E731
    b.watch(dash, 1.0)
    gone_to = (70.0, 45.0)                                     # where they stopped, far outside the frame
    b.watch(None, 0.5)
    b.watch(gone_to, 9.0)
    assert "found:where they were heading" in b.states(), b.states()
    assert abs(b.pose[0] - 70) <= 20 and b.f.found_by == ["where they were heading"]
    lost_at = next(m["time"] for _t, m in b.events if m["state"] == "lost")
    assert lost_at and b.f.searches == 1


def test_someone_who_vanishes_from_stillness_is_looked_for_where_they_usually_are() -> None:
    usual = PresenceMap(None)
    for _ in range(40):
        usual.note(45.0, 60.0)                                 # they are often standing over to the right
    for _ in range(25):
        usual.note(-23.0, 38.0)                                # … and usually in the chair
    b = Bench(presence=usual)
    b.phase("listening")
    b.watch(CHAIR, 6.0)
    order = list(dict.fromkeys(w for *_p, w in b.f.plan_search(b.f.track)))
    assert order == ["where they usually are", "either side", "up and down"]   # what it has learned comes first
    b.watch(None, 3.0)                                         # still, then gone: patience first
    assert b.f.search is not None and b.f.searches == 1
    plan = [why for *_pose, why in b.f.search.places] + [b.f.search.looking]
    assert "where they usually are" in plan and "where they were heading" not in plan   # they were still: no guess at a heading
    cold = Bench()                                             # nothing learned yet: either side comes first
    cold.phase("listening")
    cold.watch(CHAIR, 6.0)
    assert [w for *_p, w in cold.f.plan_search(cold.f.track)][:2] == ["either side", "either side"]
    b.watch((45.0, 60.0), 12.0)
    assert "found:where they usually are" in b.states(), b.states()
    assert abs(b.pose[0] - 45) <= 12


def test_a_conversation_that_opens_on_an_empty_frame_looks_where_they_usually_are() -> None:
    usual = PresenceMap(None)
    for _ in range(30):
        usual.note(*CHAIR)
    b = Bench(presence=usual)
    b.phase("wake")
    far = (-45.0, 38.0)                                        # out of view from the board's fixed pose (0, 45)
    b.watch(far, 1.0)
    assert b.sent == []                                        # patience first
    b.watch(far, 6.0)
    assert b.looks()[0][:2] == (-20, 40) and b.f._acquired      # one look at the chair …
    assert b.f.holding and abs(b.pose[0] - far[0]) <= 12        # … and from there it sees them and follows
    assert b.f.searches == 0                                    # that look is not a search
    empty = Bench()                                            # nothing learned yet: scan down from the head's pose
    empty.phase("wake")
    empty.watch(None, 1.0)
    assert empty.sent == []                                    # patience first
    empty.watch(None, 6.0)
    assert [look[:2] for look in empty.looks()][:3] == [(0, 27), (0, 9), (0, 63)]
    seen = Bench(presence=usual)                               # someone already in view: the look is never spent
    seen.phase("wake")
    seen.watch((3.0, 45.0), 6.0)
    assert all(abs(y) <= 8 for y, *_ in seen.looks())



def test_a_seated_owner_below_the_usual_place_is_found_by_looking_a_little_lower() -> None:
    """2026-09-27: the level pose looked over the seated owner (frames held the top of his hair), and a face
    at a frame's edge is not detected. The opening looks where they usually are, then lower."""
    usual = PresenceMap(None)
    for _ in range(30):
        usual.note(-10.0, 30.0)
    b = Bench(presence=usual)
    seated = (-10.0, 12.0)
    b.phase("listening")
    for _ in range(40):                                        # only a face well inside the frame is detected
        b.t += 0.25
        near = abs(seated[1] - b.pose[1]) <= 12 and abs(seated[0] - b.pose[0]) <= 20
        asyncio.run(b.f.on_faces([face_at(*seated, *b.pose)] if near else [], *b.pose))
    whys = [p for p in b.looks()]
    assert whys[0][:2] == (-10, 30) and whys[1][:2] == (-10, 12)
    assert b.f.holding and abs(b.pose[1] - seated[1]) <= fo.DEADBAND_PITCH


def test_every_return_from_thinking_with_nobody_in_view_gets_a_fresh_opening() -> None:
    """A desk call hands the head back for each "thinking"; the board's own pose then looks over the owner.
    Coming back to speak, eve looks for them again, at most MAX_OPENINGS times a conversation."""
    usual = PresenceMap(None)
    for _ in range(30):
        usual.note(*CHAIR)
    b = Bench(presence=usual)
    for turn in range(fo.MAX_OPENINGS + 2):
        b.phase("listening" if turn == 0 else "speaking")
        b.watch(None, 6.0)
        b.phase("thinking")
        b.pose = (0.0, 45.0)                                   # the board takes the head back to level
        b.watch(None, 1.0)
    starts = [c for c in b.sent if c["cmd"] == "look" and c["hold"] > 0 and (c["yaw"], c["pitch"]) == (-20, 40)]
    assert len(starts) == fo.MAX_OPENINGS
    b.phase("speaking")
    b.watch(CHAIR, 6.0)                                        # someone in view is still followed after that
    assert b.f.holding and abs(b.pose[0] - CHAIR[0]) <= fo.DEADBAND_YAW


def test_a_learned_place_past_the_limits_is_brought_back_inside_them() -> None:
    """2026-09-27: the map held yaw 120 pitch 0 (10° bins, learned off refused looks) and the opening look
    turned the head right round to the wall behind it."""
    usual = PresenceMap(None)
    for _ in range(30):
        usual.note(118.0, 2.0)
    b = Bench(presence=usual)
    b.phase("listening")
    b.watch(None, 4.0)
    assert b.looks() and all(abs(y) <= fo.MAX_YAW and 5 <= p <= 85 for y, p, _ in b.looks())


def test_a_refused_look_is_noticed_and_the_head_is_not_walked_to_its_limits() -> None:
    """The board keeps the head when it wants the owner's attention (hostlook.h). The follower must then
    build on the pose the board echoes, not the one it asked for — or a face that never moves in the
    frame walks the ask to yaw 100 (live, 2026-09-27 09:49)."""
    b = Bench()
    b.ok = True
    b.phase("listening")
    real = (0.0, 45.0)
    for _ in range(40):                                        # the board answers every look but never moves
        b.t += 0.25
        asyncio.run(b.f.on_faces([face_at(20.0, 45.0, *real)], *real))
    asked = [y for y, _p, _h in b.looks()]
    assert max(asked) <= 20 + fo.DEADBAND_YAW
    b = Bench()
    b.phase("listening")
    b.see((-4.0, 38.0))
    b.see((0.0, 45.0), dt=0.5)                                 # two boxes 8° apart: "9°/s", from a person sitting still
    assert b.f.track.confirmed == 2 and b.f.track.speed > 2         # a spurious velocity, from box jitter alone
    assert "where they were heading" not in [w for *_p, w in b.f.plan_search(b.f.track)]


def test_it_gives_the_head_back_and_does_not_keep_scanning_an_empty_room() -> None:
    b = Bench()
    b.phase("listening")
    b.watch(CHAIR, 6.0)
    b.watch(None, 120.0, dt=0.5)                               # two minutes of nobody
    assert b.f.searches == 1 and not b.f.holding and b.looks()[-1][2] == 0      # ONE search per loss, then it waits
    search_looks = [look for look in b.looks() if look[2] == 4000][-fo.MAX_PLACES:]
    assert len({(y, p) for y, p, _ in search_looks}) <= fo.MAX_PLACES
    assert b.states().count("lost") == 1
    b.watch(CHAIR, 8.0)                                        # they come back by themselves: followed again …
    assert b.f.holding and "seen" in b.states()[-1:]
    b.watch(None, 20.0, dt=0.5)                                # … and a second loss earns a second search
    assert b.f.searches == 2
    b.phase("idle")
    assert b.f.searches == 0 and b.f.track is None             # the next conversation starts fresh


def test_the_presence_map_learns_decays_and_survives_a_restart(tmp_path: Path) -> None:
    clock = {"t": 1_000_000.0}
    path = tmp_path / "presence.json"
    m = PresenceMap(path, wall=lambda: clock["t"])
    for _ in range(10):
        m.note(-23.4, 38.2)
    for _ in range(4):
        m.note(44.0, 61.0)
    m.note(90.0, 45.0)                                         # once: not a place, a passing glance
    assert m.top(2) == [(-20, 40), (40, 60)] and m.top(2, exclude=[(-23.0, 38.0)]) == [(40, 60)]
    assert m.save() and json.loads(path.read_text())["cells"]["-20,40"] == 10.0
    again = PresenceMap(path, wall=lambda: clock["t"]).load()
    assert again.top(1) == [(-20, 40)] and again.save() is False           # nothing new to write
    clock["t"] += 28 * 86400                                   # four weeks on: two half-lives
    later = PresenceMap(path, wall=lambda: clock["t"]).load()
    assert 2.4 <= later.cells[(-20, 40)] <= 2.6 and later.top(2) == []      # faded below "usually"
    assert PresenceMap(tmp_path / "missing.json").load().cells == {}
    (tmp_path / "bad.json").write_text("{not json")
    assert PresenceMap(tmp_path / "bad.json").load().cells == {}
    b = Bench(presence=PresenceMap(path, wall=lambda: clock["t"]).load())
    b.phase("listening")
    b.watch(CHAIR, 6.0)
    b.phase("idle")                                            # a conversation's worth of sightings is kept
    assert json.loads(path.read_text())["cells"]["-20,40"] > 2.6


def test_it_never_looks_behind_itself_and_survives_a_dead_link_the_switch_and_the_bus() -> None:
    b = Bench()
    b.phase("listening")
    b.pose = (95.0, 45.0)
    b.watch((125.0, 45.0), 8.0)
    assert max(y for y, *_ in b.looks()) <= fo.MAX_YAW
    dead = Bench()
    dead.ok = False
    dead.phase("listening")
    dead.watch(CHAIR, 4.0)
    assert not dead.f.holding and dead.f.moves == 0            # the move did not reach the robot: nothing is claimed
    off = Bench(enabled=False)
    off.phase("listening")
    off.watch(CHAIR, 4.0)
    assert off.sent == []
    assert fo.configured({}) is True and fo.configured({"EVE_FOLLOW_SPEAKER": "0"}) is False
    assert fo.presence_path({"EVE_PRESENCE_FILE": "/tmp/p.json"}) == Path("/tmp/p.json")
    none = Bench()
    none.phase("listening")
    asyncio.run(none.f.on_faces([FaceResult(60, 0, 20, 90)], None, None))    # a frame without its pose cannot be placed
    assert none.sent == []

    def broken(topic, msg):
        raise RuntimeError("bus down")
    deaf = Bench(publish=broken)
    deaf.phase("listening")
    deaf.watch(CHAIR, 6.0)
    assert deaf.f.holding                                      # the bus is an audience, never a dependency


def test_the_tracker_hands_the_follower_every_face_with_the_frames_pose_and_never_dies_of_it() -> None:
    import base64

    from eve_bridge.vision import FaceTracker, Rect

    got: list = []
    sent: list[dict] = []

    async def send(cmd: dict) -> bool:
        sent.append(cmd)
        return True

    async def on_faces(faces, yaw, pitch) -> None:
        got.append(([(f.bx, f.size) for f in faces], yaw, pitch))
        raise RuntimeError("a follower bug")                   # must not stop the tracker

    rects = [Rect(x=10, y=30, w=40, h=40, conf=0.9), Rect(x=110, y=40, w=20, h=20, conf=0.8)]
    tracker = FaceTracker(detect=lambda frame: rects, send=send, on_faces=on_faces)
    frame = {"seq": 7, "w": 160, "h": 120, "fmt": "gray", "yaw": 12, "pitch": 40,
             "b64": base64.b64encode(bytes(160 * 120)).decode()}

    async def go() -> None:
        await tracker.on_frame(frame)
        await tracker._task

    asyncio.run(go())
    tracker.stop()
    assert got == [([(-62, 25), (50, 12)], 12, 40)]            # both faces, left then right, with the pose
    assert [c["cmd"] for c in sent] == ["face"] and sent[0]["seq"] == 7   # the board still gets the largest, as before


def test_an_asked_for_pose_reaches_the_follower_through_the_head_and_presence_is_a_bus_topic() -> None:
    from eve_bridge.head import Head
    from eve_bridge.memory_bus import TOPICS

    sent: list[dict] = []
    holds: list[float] = []

    async def send(cmd: dict) -> bool:
        sent.append(cmd)
        return True

    async def fail(cmd: dict) -> bool:
        return False

    head = Head(send)
    head.on_move = holds.append
    assert asyncio.run(head.move(-60, 45, hold_secs=15))["ok"] and holds == [15.0]
    failing = Head(fail)
    failing.on_move = holds.append
    assert asyncio.run(failing.move(10, 45))["ok"] is False and holds == [15.0]    # a move that did not land is not a hold
    assert fo.PRESENCE_TOPIC in TOPICS
