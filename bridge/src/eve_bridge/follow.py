"""eve keeps its eyes on whoever is talking to it — and works out where they went when it loses them.

The Mac already finds every face in every camera frame (vision.py) and tells the board where the
largest one is. What the board does with that is its own policy (gaze.cpp): it live-tracks the
OWNER, in its attention and dictation states. In a voice conversation it goes to one fixed
"facing you" pose and stays there; a face that is not the owner's gets a single glance in the
calm states and nothing more. So a guest talking to eve is never followed, and neither is the
owner once the conversation has opened — lean back, stand up, walk round the desk, and eve
keeps addressing the chair.

This module is the policy for a conversation, on the host, with no reflash: the firmware accepts a
host pose in any state while a conversation is open, and "a host look owns the head from the
moment it lands until its hold runs out" (hostlook.h).

IT FOLLOWS A PERSON, NOT A SIGHTING. The first version turned toward every face box it was handed,
and on a real face (2026-09-21) it swung ±20° around someone sitting still. The offsets logged with
each move showed why: a frame reaches this code about half a second after it was taken (JPEG on the
board, 115200 baud, a Vision request), so "0.6 s after the move" was still a picture from mid-swing,
stamped with a pose the head had already left. So there is a track:

- a position and a VELOCITY in absolute head angles, averaged over sightings;
- nothing is evidence for SETTLE_SECS after a move, which covers the glide AND that pipeline delay;
- a sighting has to AGREE with where the track says the person should be (AGREE_DEG) before the head
  acts on it; one that does not is an outlier until a second one says the same, and then it is the
  truth (they really did jump, or it is someone else);
- moves are limited to MAX_STEP_DEG, led slightly by the velocity (LEAD_SECS) so a walking person is
  met rather than chased, inside a deadband, with the hold renewed while the face sits still.

WHEN IT LOSES THEM IT REASONS, IN THIS ORDER, from what it knew a moment ago and what it has learned:

1. WHERE THEY WERE HEADING. If the last sightings were moving (SPEED_FAST deg/s), or the face left by a
   frame edge, it looks further the same way — one field-of-view step, then another. Someone who
   leaves quickly is followed at once (LEAVING_HOLD_SECS), not after the ordinary patience.
2. WHERE THEY USUALLY ARE. A PresenceMap — a decayed histogram of the absolute angles at which faces
   have been followed, kept in ~/.config/eve-bridge/presence.json across restarts — gives the two
   likeliest places that have not just been tried. At a desk that is "the chair" and "standing".
3. EITHER SIDE of where they were, one field-of-view step: at a desk people shift sideways more than
   anything else, and this is what finds them before the map has learned anything.
4. UP AND DOWN at the last heading: they stood up, or sat back.
Then it gives the head back, and it does NOT search again until it has really seen someone again: one
search per loss, at most MAX_SEARCHES a conversation. The first live run (2026-09-21) searched three
times back to back at an empty chair, which is how an anxious robot behaves.

IT USES WHAT THE DAEMON KNOWS:
- WHO IS TALKING, from the conversation's phase: it follows in `wake`, `listening`, `asking` and
  `speaking`; in `thinking` (the glance aside) and `working` (head down at the desk) it hands the head
  back at once, so eve keeps its own expressions.
- WHICH FACE: with several, the one nearest the track, so a passer-by does not steal the gaze.
- WHAT THE OWNER ASKED FOR: "look left", look_around and find own the head for as long as they hold it.
  An explore, the dictation key and a lesson's listening pose own it too (`blocked`).

WHEN A CONVERSATION OPENS AND NOBODY IS IN VIEW, it does not wait at the board's fixed pose hoping. On
2026-09-21 the owner sat 23° to eve's left and a little below level, and from the fixed pose his face
was in a quarter of the frames, mostly at the edge. After ACQUIRE_AFTER_SECS with no confirmed face it
looks where they were last followed, then at the place the map says people most often are, then a
little lower there (with nothing learned yet: lower, lower still, then higher than the head is now),
and follows from there. That opening re-arms each time the conversation comes back
to a follow phase with nobody in view (a desk call hands the head back for every "thinking", and the
board's level pose looks over a seated owner: on 2026-09-27 its frames held only the top of his hair),
at most MAX_OPENINGS times a conversation.

AND IT TELLS THE MEMORY BUS. Every change — seen, lost, found and how — is published on
`/eve/presence`, so rosbridge clients (Foxglove, `eve-bridge memory tail`) see eve's sense of
where its person is, live. The control loop itself reads the local map, never the bus or claude-mem:
a head cannot wait on an HTTP recall. Nothing here identifies anyone; the map holds angles, not faces.
"""

from __future__ import annotations

import json
import logging
import math
import os
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Awaitable, Callable, Optional, Sequence

from .head import CAMERA_HFOV_DEG, build_look_cmd, clamp_pose, target_pose

log = logging.getLogger(__name__)

FOLLOW_PHASES = frozenset({"wake", "listening", "asking", "speaking"})
REFUSED_DEG = 8.0            # the echoed pose this far from the asked pose after settling: the board said no
SETTLE_SECS = 1.2            # after a move: the glide (≥ 0.3 s) plus ~0.5 s of frame pipeline, with margin
DEADBAND_YAW = 6.0           # degrees off-centre before the head bothers to turn
DEADBAND_PITCH = 5.0
SMOOTH = 0.5                 # weight of a new agreeing sighting in the track's position
VEL_SMOOTH = 0.3             # face boxes jitter by a few degrees; a velocity is an average, not a difference
VEL_MIN_DT = 0.4             # two sightings closer together than this say nothing about speed
AGREE_DEG = 14.0             # a sighting this close to where the track expects the person is the same person
MAX_STEP_DEG = 20.0          # one move never swings further than this
LEAD_SECS = 0.35             # aim this far ahead along the velocity
HOLD_SECS = 4.0              # each pose owns the head this long …
REFRESH_SECS = 2.5           # … and is renewed this often while the face stays put
LOST_HOLD_SECS = 2.5         # a still person who vanished: people look down; wait this long
LEAVING_HOLD_SECS = 0.8      # a person who was moving fast, or left by a frame edge: go after them now
SPEED_FAST = 15.0            # deg/s: someone really leaving; a sitting person's box jitter reads as up to ~12
STALE_TRACK_SECS = 6.0       # a track this old is history: forget it rather than search on it
EDGE = 65                    # |bx| or |by| at or past this is "by the edge of the frame"
SEARCH_STEP_SECS = 1.6       # settle, then at least one clean frame, at each place it looks
MAX_SEARCHES = 3             # per conversation
USUAL_PLACES = 2
MAX_PLACES = 6               # a search is at most this many looks (~10 s), likeliest first
ACQUIRE_AFTER_SECS = 1.5     # a conversation opened and nobody is in view yet: look where they usually are
LOWER_DEG = 18               # the opening's last look: this much lower than the usual place (a seated person)
MAX_OPENINGS = 4             # opening sequences per conversation (each at most three looks)
MIN_CONF = 35                # the tracker's 0..100; weaker boxes are posters and lamps
MIN_SIZE = 4                 # a face under 4% of the frame width is across the room
MAX_YAW = 100                # short of the neck's ±120: past that the robot is looking behind itself
PRESENCE_TOPIC = "/eve/presence"
DEFAULT_PRESENCE_FILE = "~/.config/eve-bridge/presence.json"

Sender = Callable[[dict[str, Any]], Awaitable[bool]]
Publisher = Callable[[str, dict[str, Any]], Any]


def _reachable(yaw: float, pitch: float) -> tuple[int, int]:
    """A learned place, brought inside the poses the follower may ask for: the map's cells are 10° bins
    and can sit on 0 or past MAX_YAW (2026-09-27: an opening look at yaw 120 pitch 0 turned the head right
    round to face the wall behind it)."""
    y, p, _ = clamp_pose(max(-MAX_YAW, min(MAX_YAW, yaw)), pitch)
    return y, p


@dataclass
class Track:
    """The person being followed, in absolute head angles."""

    yaw: float
    pitch: float
    seen_at: float
    vyaw: float = 0.0            # deg/s
    vpitch: float = 0.0
    confirmed: int = 1           # agreeing sightings in a row
    edge: tuple[int, int] = (0, 0)   # the last sighting's bx, by: which side of the frame they were on
    size: int = 0
    doubt: Optional[tuple[float, float]] = None   # one sighting that did not agree, waiting for a second

    @property
    def speed(self) -> float:
        return math.hypot(self.vyaw, self.vpitch)


class PresenceMap:
    """Where faces have been followed: a decayed histogram over absolute head angles.

    BIN-degree cells; a cell's weight halves every HALF_LIFE_DAYS, so the map follows a moved desk.
    It answers one question — "where is this person usually?" — and holds angles, never images."""

    BIN = 10
    HALF_LIFE_DAYS = 14.0
    MAX_CELLS = 120

    def __init__(self, path: Optional[Path] = None, wall: Callable[[], float] = time.time) -> None:
        self.path = path
        self.wall = wall
        self.cells: dict[tuple[int, int], float] = {}
        self.stamp = wall()
        self.dirty = False

    @classmethod
    def cell(cls, yaw: float, pitch: float) -> tuple[int, int]:
        return int(round(yaw / cls.BIN)) * cls.BIN, int(round(pitch / cls.BIN)) * cls.BIN

    def _decay(self) -> None:
        now = self.wall()
        days = max(0.0, (now - self.stamp) / 86400.0)
        if days > 0.01:
            k = 0.5 ** (days / self.HALF_LIFE_DAYS)
            self.cells = {c: w * k for c, w in self.cells.items() if w * k >= 0.05}
            self.stamp = now

    def note(self, yaw: float, pitch: float, weight: float = 1.0) -> None:
        c = self.cell(yaw, pitch)
        self.cells[c] = self.cells.get(c, 0.0) + weight
        self.dirty = True

    def top(self, n: int, exclude: Sequence[tuple[float, float]] = (), min_weight: float = 3.0) -> list[tuple[int, int]]:
        """The n heaviest cells, skipping any within a cell's width of a place in `exclude`."""
        self._decay()
        out: list[tuple[int, int]] = []
        for (y, p), w in sorted(self.cells.items(), key=lambda kv: -kv[1]):
            if w < min_weight or len(out) >= n:
                break
            if any(abs(y - ey) <= self.BIN and abs(p - ep) <= self.BIN for ey, ep in exclude):
                continue
            out.append((y, p))
        return out

    def load(self) -> "PresenceMap":
        if self.path is None:
            return self
        try:
            data = json.loads(self.path.read_text(encoding="utf-8"))
            self.cells = {(int(k.split(",")[0]), int(k.split(",")[1])): float(v)
                          for k, v in (data.get("cells") or {}).items()}
            self.stamp = float(data.get("stamp") or self.wall())
            self._decay()
        except (OSError, ValueError, KeyError, IndexError, TypeError):
            self.cells = {}                          # no map yet, or an unreadable one: start learning again
        return self

    def save(self) -> bool:
        if self.path is None or not self.dirty:
            return False
        self._decay()
        cells = dict(sorted(self.cells.items(), key=lambda kv: -kv[1])[:self.MAX_CELLS])
        try:
            self.path.parent.mkdir(parents=True, exist_ok=True)
            tmp = self.path.with_suffix(".tmp")
            tmp.write_text(json.dumps({"stamp": self.stamp, "bin_deg": self.BIN,
                                       "cells": {f"{y},{p}": round(w, 2) for (y, p), w in cells.items()}}),
                           encoding="utf-8")
            os.replace(tmp, self.path)
            self.dirty = False
            return True
        except OSError as e:
            log.warning("follow: could not save the presence map (%s)", e)
            return False


@dataclass
class _Search:
    places: list[tuple[int, int, str]]               # (yaw, pitch, why) still to look at
    started_at: float
    step_at: float = float("-inf")
    looking: str = ""                                # the reason for the place it is looking at now
    tried: list[tuple[float, float]] = field(default_factory=list)


class SpeakerFollower:
    """Turn the head toward the person eve is talking with, and find them again (the module docstring)."""

    def __init__(self, send: Sender, clock: Callable[[], float] = time.monotonic, enabled: bool = True,
                 blocked: Callable[[], str] = lambda: "", presence: Optional[PresenceMap] = None,
                 publish: Optional[Publisher] = None) -> None:
        self.send = send
        self.clock = clock
        self.enabled = enabled
        self.blocked = blocked                # the daemon's word on who else owns the head right now ("" = nobody)
        self.presence = presence if presence is not None else PresenceMap(None)
        self.publish = publish
        self.phase = "idle"
        self.owner_hold_until = float("-inf")
        self.track: Optional[Track] = None
        self.commanded: Optional[tuple[int, int]] = None       # the last pose sent
        self.last_move_at = float("-inf")
        self.holding = False                  # a follower pose currently owns the head
        self.search: Optional[_Search] = None
        self.moves = 0
        self.searches = 0
        self.found_by: list[str] = []         # how each re-acquisition happened, this conversation
        self._noted_at = float("-inf")
        self._announced = ""                  # the last presence state published
        self._opened_at: Optional[float] = None    # when the current stretch of follow phases began
        self._acquired = False                # this stretch's opening sequence has been planned
        self._opening: list[tuple[int, int, str]] = []   # its looks still to make
        self._opening_at = float("-inf")
        self._openings = 0                    # opening sequences this conversation
        self._last_seen: Optional[tuple[float, float]] = None   # where someone was last followed

    # -- context from the daemon --
    @property
    def active(self) -> bool:
        return (self.enabled and self.phase in FOLLOW_PHASES and not self.blocked()
                and self.clock() >= self.owner_hold_until)

    async def on_phase(self, phase: str) -> None:
        """The conversation's phase changed. Leaving a follow phase hands the head back at once."""
        was = self.active
        entering = phase in FOLLOW_PHASES and self.phase not in FOLLOW_PHASES
        self.phase = phase
        if entering:                                 # a new stretch: nobody in view by then earns a fresh opening
            self._opened_at = self.clock()
            self._acquired = False
        if was and not self.active:
            await self.release("phase " + phase)
            self._opening = []
        if phase == "idle":                          # the conversation is over: keep what was learned, start fresh
            if self.presence.save():
                log.info("follow: presence map saved (%d places)", len(self.presence.cells))
            self.track = None
            self.search = None
            self.searches = 0
            self.found_by = []
            self._announced = ""
            self._opened_at = None
            self._acquired = False
            self._opening = []
            self._openings = 0
            self._last_seen = None

    def owner_moved(self, hold_secs: float) -> None:
        """The owner asked for a pose (move_head, look_around, find): it is theirs for as long as it is held."""
        self.owner_hold_until = self.clock() + max(0.0, float(hold_secs))
        self.holding = False
        self.search = None
        self.track = None      # by the time the hold ends the track is history: start again from what is seen

    async def release(self, why: str) -> None:
        """Give the head back to the board's own behaviour now (a look with hold 0)."""
        if self.holding and self.commanded is not None:
            await self.send(build_look_cmd(self.commanded[0], self.commanded[1], 0))
            log.info("follow: let go (%s)", why)
        self.holding = False
        self.search = None

    def _say(self, state: str, **extra: Any) -> None:
        """One event on the memory bus per change of state (and per find), never per frame."""
        if self.publish is None or (state == self._announced and state != "found"):
            return
        self._announced = state
        t = self.track
        msg = {"state": state, "time": time.time(), "phase": self.phase, **extra}
        if t is not None:
            msg.update(yaw=round(t.yaw, 1), pitch=round(t.pitch, 1), speed=round(t.speed, 1))
        try:
            self.publish(PRESENCE_TOPIC, msg)
        except Exception:  # noqa: BLE001 — the bus is an audience, never a dependency
            log.debug("follow: publish failed", exc_info=True)

    # -- the frames --
    def _pick(self, faces: Sequence[Any], pose_yaw: float, pose_pitch: float, now: float,
              ) -> Optional[tuple[float, float, Any]]:
        """(absolute yaw, pitch, the face) to consider in this frame: nearest the track, else the largest."""
        usable = [f for f in faces if f.conf >= MIN_CONF and f.size >= MIN_SIZE]
        if not usable:
            return None
        angles = [target_pose(pose_yaw, pose_pitch, f.bx, f.by)[:2] for f in usable]
        t = self.track
        if t is not None and now - t.seen_at <= LOST_HOLD_SECS + SEARCH_STEP_SECS:
            ey, ep = self._expected(t, now)
            i = min(range(len(usable)), key=lambda k: math.hypot(angles[k][0] - ey, angles[k][1] - ep))
        else:
            i = max(range(len(usable)), key=lambda k: usable[k].size)
        return float(angles[i][0]), float(angles[i][1]), usable[i]

    @staticmethod
    def _expected(t: Track, now: float) -> tuple[float, float]:
        dt = min(1.5, max(0.0, now - t.seen_at))     # a velocity is only believed for a moment
        return t.yaw + t.vyaw * dt, t.pitch + t.vpitch * dt

    def _update(self, y: float, p: float, face: Any, now: float) -> bool:
        """Fold one sighting into the track. True when the track may be acted on."""
        t = self.track
        if t is None or now - t.seen_at > LOST_HOLD_SECS + 8 * SEARCH_STEP_SECS:
            self.track = Track(y, p, now, edge=(face.bx, face.by), size=face.size)
            return False                             # one sighting is a candidate, not a person
        if self.search is not None and t.confirmed >= 2:
            # While it is looking for someone it lost, the first face it finds IS the find: the track's
            # old position is where they were, not where a sighting has to agree with.
            self.track = Track(y, p, now, confirmed=2, edge=(face.bx, face.by), size=face.size)
            return True
        ey, ep = self._expected(t, now)
        if math.hypot(y - ey, p - ep) <= AGREE_DEG:
            dt = now - t.seen_at
            if VEL_MIN_DT <= dt <= 2.0:
                t.vyaw += VEL_SMOOTH * ((y - t.yaw) / dt - t.vyaw)
                t.vpitch += VEL_SMOOTH * ((p - t.pitch) / dt - t.vpitch)
            t.yaw += SMOOTH * (y - t.yaw)
            t.pitch += SMOOTH * (p - t.pitch)
            t.seen_at, t.confirmed, t.doubt = now, t.confirmed + 1, None
            t.edge, t.size = (face.bx, face.by), face.size
            return t.confirmed >= 2
        if t.doubt is not None and math.hypot(y - t.doubt[0], p - t.doubt[1]) <= AGREE_DEG:
            # twice in a row somewhere else: that is where they are (or who is here) now
            self.track = Track(y, p, now, confirmed=2, edge=(face.bx, face.by), size=face.size)
            return True
        t.doubt = (y, p)                             # once is an outlier: a blurred frame, a poster, a reflection
        return False

    async def on_faces(self, faces: Sequence[Any], pose_yaw: Optional[float], pose_pitch: Optional[float]) -> None:
        """Every processed camera frame lands here, with the pose it was taken at."""
        if not self.active or pose_yaw is None or pose_pitch is None:
            if self.holding and not self.active:     # someone else owns the head now: hand it over at once
                await self.release(self.blocked() or "standing down")
            return
        now = self.clock()
        if now - self.last_move_at < SETTLE_SECS:
            return                                   # this frame was taken while the head was still swinging
        if self.holding and self.commanded is not None and (
                abs(float(pose_yaw) - self.commanded[0]) > REFUSED_DEG
                or abs(float(pose_pitch) - self.commanded[1]) > REFUSED_DEG):
            # The board kept the head (it refuses a host look while it wants the owner's attention, or
            # outside a conversation, hostlook.h): the pose it echoes is the truth, not the one asked for.
            # Building on the ask walked the head to its limits at a face that never moved (2026-09-27).
            log.info("follow: the board kept the head at yaw %.0f pitch %.0f (asked %d/%d)",
                     pose_yaw, pose_pitch, *self.commanded)
            self.holding = False
            self.commanded = None
            self.track = None
        picked = self._pick(faces, float(pose_yaw), float(pose_pitch), now)
        if picked is None:
            await self._lost(now, float(pose_yaw), float(pose_pitch))
            return
        y, p, face = picked
        was_searching = self.search
        if not self._update(y, p, face, now):
            return
        t = self.track
        assert t is not None
        self._acquired = True                        # someone is followed: this stretch's opening is spent
        self._opening = []
        self._last_seen = (t.yaw, t.pitch)
        if was_searching is not None:
            how = was_searching.looking or "they came back"
            self.found_by.append(how)
            self.search = None
            log.info("follow: found them again (%s) at yaw %.0f pitch %.0f", how, t.yaw, t.pitch)
            self._say("found", how=how)
        else:
            self._say("seen")
        if now - self._noted_at >= 1.0:              # learn where people are, about once a second
            self._noted_at = now
            self.presence.note(t.yaw, t.pitch)
        ay, ap = t.yaw + t.vyaw * LEAD_SECS, t.pitch + t.vpitch * LEAD_SECS
        base = self.commanded if (self.holding and self.commanded is not None) else (float(pose_yaw), float(pose_pitch))
        dy, dp = ay - base[0], ap - base[1]
        far = abs(dy) >= DEADBAND_YAW or abs(dp) >= DEADBAND_PITCH
        if far:
            scale = min(1.0, MAX_STEP_DEG / max(abs(dy), abs(dp)))
            want_y = max(-MAX_YAW, min(MAX_YAW, base[0] + dy * scale))
            yy, pp, _ = clamp_pose(want_y, base[1] + dp * scale)
            await self._look(yy, pp, now, "follow", face)
        elif self.holding and now - self.last_move_at >= REFRESH_SECS and self.commanded is not None:
            # in the deadband: renew the hold so the board's own pose cannot pull the head away mid-sentence
            if await self.send(build_look_cmd(self.commanded[0], self.commanded[1], int(HOLD_SECS * 1000))):
                self.last_move_at = now - SETTLE_SECS     # a renewal moves nothing: do not blind the next frame
        elif not self.holding:
            # already looking at them from the board's own pose: take the head so it stays that way
            await self._look(int(round(pose_yaw)), int(round(pose_pitch)), now, "hold", face)

    async def _look(self, yaw: int, pitch: int, now: float, why: str, face: Any = None) -> bool:
        if not await self.send(build_look_cmd(yaw, pitch, int(HOLD_SECS * 1000))):
            return False
        moved = self.commanded != (yaw, pitch)
        self.commanded = (yaw, pitch)
        self.holding = True
        self.last_move_at = now if moved else now - SETTLE_SECS
        if moved and why == "follow":
            self.moves += 1
            t = self.track
            log.info("follow: → yaw %d pitch %d (face bx=%d by=%d; track %.0f/%.0f moving %.0f°/s)", yaw, pitch,
                     getattr(face, "bx", 0), getattr(face, "by", 0), t.yaw if t else 0, t.pitch if t else 0,
                     t.speed if t else 0)
        elif moved and why != "hold":
            log.info("follow: looking %s → yaw %d pitch %d", why, yaw, pitch)
        return True

    # -- losing them, and working out where they went --
    def plan_search(self, t: Track) -> list[tuple[int, int, str]]:
        """The places to look, likeliest first (the module docstring has the reasoning)."""
        places: list[tuple[int, int, str]] = []
        step = CAMERA_HFOV_DEG / 3.0                 # 22°: what was at the frame's edge lands near its middle
        # A heading is believed only from a real exit: fast motion seen over at least three sightings, or
        # leaving by a frame edge. Two noisy boxes make a "velocity" of 8°/s out of a person sitting still,
        # and the first live search (2026-09-21) went looking up and to the right on the strength of one.
        moving = t.confirmed >= 3 and t.speed >= SPEED_FAST
        dir_y = (1 if t.vyaw > 0 else -1) if moving and abs(t.vyaw) >= SPEED_FAST / 2 else (
            1 if t.edge[0] >= EDGE else -1 if t.edge[0] <= -EDGE else 0)
        dir_p = (1 if t.vpitch > 0 else -1) if moving and abs(t.vpitch) >= SPEED_FAST / 2 else (
            -1 if t.edge[1] >= EDGE else 1 if t.edge[1] <= -EDGE else 0)      # +by is down: lower pitch
        if dir_y or dir_p:
            for k in (1, 2):
                y, p, _ = clamp_pose(max(-MAX_YAW, min(MAX_YAW, t.yaw + dir_y * step * k)),
                                     t.pitch + dir_p * step * 0.6 * k)
                places.append((y, p, "where they were heading"))
        tried = [(float(y), float(p)) for y, p, _ in places] + [(t.yaw, t.pitch)]
        for y, p in self.presence.top(USUAL_PLACES, exclude=tried):
            places.append((*_reachable(y, p), "where they usually are"))
        heading = int(round(max(-MAX_YAW, min(MAX_YAW, t.yaw))))
        tried += [(float(y), float(p)) for y, p, _ in places[len(tried) - 1:]]
        for side in (-1, 1):                         # a sideways shift is the commonest way to leave a frame
            y, p, _ = clamp_pose(max(-MAX_YAW, min(MAX_YAW, t.yaw + side * step)), t.pitch)
            if not any(abs(y - ty) <= PresenceMap.BIN and abs(p - tp) <= PresenceMap.BIN for ty, tp in tried):
                places.append((y, p, "either side"))
        for pitch in (62, 30):                       # they stood up; they sat back
            if abs(pitch - t.pitch) > DEADBAND_PITCH:
                places.append((heading, pitch, "up and down"))
        return places[:MAX_PLACES]

    def _plan_opening(self, pose_yaw: float, pose_pitch: float) -> list[tuple[int, int, str]]:
        places: list[tuple[int, int, str]] = []
        if self._last_seen is not None:
            y, p, _ = clamp_pose(*self._last_seen)
            places.append((y, p, "where they were"))
        usual = self.presence.top(1)
        if usual:
            places.append((*_reachable(usual[0][0], usual[0][1]), "where they usually are"))
            places.append((*_reachable(usual[0][0], usual[0][1] - LOWER_DEG), "a little lower"))
        if not places:
            # Nothing learned yet (a first conversation, a cleared map): scan from where the head is,
            # downward first — a desk robot's person is more often below its level gaze than above it.
            for k, why in ((-1, "lower"), (-2, "lower still"), (1, "higher")):
                y, p, _ = clamp_pose(pose_yaw, pose_pitch + k * LOWER_DEG)
                places.append((y, p, why))
        kept: list[tuple[int, int, str]] = []
        for y, p, why in places:                     # never the pose it already has, never the same place twice
            if abs(y - pose_yaw) < DEADBAND_YAW and abs(p - pose_pitch) < DEADBAND_PITCH:
                continue
            if any(abs(y - ky) < DEADBAND_YAW and abs(p - kp) < DEADBAND_PITCH for ky, kp, _ in kept):
                continue
            kept.append((y, p, why))
        return kept

    async def _lost(self, now: float, pose_yaw: float, pose_pitch: float) -> None:
        t = self.track
        if t is None or t.confirmed < 2:
            # Nobody is being followed, so nothing was lost — but a stretch of conversation that opens on an
            # empty frame gets a short opening: where they were last, where they usually are, a little lower.
            if (not self._acquired and self._opened_at is not None and self.search is None
                    and now - self._opened_at >= ACQUIRE_AFTER_SECS):
                self._acquired = True
                if self._openings < MAX_OPENINGS:
                    self._opening = self._plan_opening(pose_yaw, pose_pitch)
                    if self._opening:
                        self._openings += 1
            quiet = self.track is None or now - self.track.seen_at >= SEARCH_STEP_SECS   # one stray box is no find
            if self._opening and quiet and now - self._opening_at >= SEARCH_STEP_SECS:
                y, p, why = self._opening.pop(0)
                self._opening_at = now
                await self._look(y, p, now, why + ", to start with")
            return
        gone = now - t.seen_at
        if self.search is None and gone > STALE_TRACK_SECS + LOST_HOLD_SECS:
            # The follower was standing down (an asked-for pose, thinking, working) while this track aged.
            # Its velocity and its frame edge describe a moment long gone (live, 2026-09-21: a search
            # "where they were heading" launched off a 17-second-old track). Forget it; follow what is seen.
            self.track = None
            return
        if self.search is None:
            leaving = ((t.confirmed >= 3 and t.speed >= SPEED_FAST)
                       or abs(t.edge[0]) >= EDGE or abs(t.edge[1]) >= EDGE)
            if gone < (LEAVING_HOLD_SECS if leaving else LOST_HOLD_SECS):
                return
            if self.searches >= MAX_SEARCHES:
                if self.holding:
                    await self.release(f"no face for {gone:.1f} s")
                    self._say("lost")
                return
            self.searches += 1
            self.search = _Search(self.plan_search(t), started_at=now)
            how = (f"moving {t.speed:.0f}°/s" if t.speed >= SPEED_FAST else
                   "left by the frame edge" if leaving else "they were still")
            log.info("follow: lost them %.1f s ago (%s) — looking: %s", gone, how,
                     ", then ".join(dict.fromkeys(w for *_, w in self.search.places)))
            self._say("lost", leaving=leaving)
        s = self.search
        if now - s.step_at < SEARCH_STEP_SECS:
            return
        if not s.places:
            await self.release("looked where it could think of, nobody there")
            self.track = None                        # one search per loss: nothing more until someone is SEEN again
            return
        y, p, why = s.places.pop(0)
        s.step_at, s.looking = now, why
        s.tried.append((float(y), float(p)))
        await self._look(y, p, now, why)


def configured(environ: Any = None) -> bool:
    env = os.environ if environ is None else environ
    return (env.get("EVE_FOLLOW_SPEAKER") or "1").strip().lower() not in ("0", "false", "no", "off")


def presence_path(environ: Any = None) -> Path:
    env = os.environ if environ is None else environ
    return Path((env.get("EVE_PRESENCE_FILE") or DEFAULT_PRESENCE_FILE)).expanduser()
