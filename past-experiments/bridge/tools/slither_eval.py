"""slither.io as a real-time eval of the local typed-decision model (laya-mlx): hunt, kill, grow.

Not a product feature: a measurement of how a code-owned candidate menu plus the
local model holds up at 15-20 decisions a second on a live game against other
people. The pattern is the drone-racing / sampling-rollout one: code samples
candidate actions, rolls each forward, scores risk and gain, and the model picks
among the survivors; a deterministic shield executes only safe picks. Kills are
the primary metric (Guru, 2026-09-21: "you have to terminate other players"),
length second.

    .venv/bin/python tools/slither_eval.py --episodes 3 --max-secs 300
    .venv/bin/python tools/slither_eval.py --dry-run --secs 10      # read, plan and print; move nothing

Perception is the game's own state over the Chrome DevTools Protocol: a SEPARATE
Chrome (own profile, remote-debugging port) on http://slither.io, one
Runtime.evaluate per decision that does the feature extraction INSIDE the page
(FEATURE_JS) and returns a compact JSON; the websocket stays open. Globals in
build game1107249518.js, verified live 2026-09-21: `window.slither` (own snake:
xx, yy, ang radians, sp, ssp = normal speed 5.79, sc scale, sct segments, fam,
pts[{xx, yy}]), `window.slithers` (every snake incl. own by id, each with pts[],
dead_amt, alive_amt), `window.foods` [{xx, yy, sz}], `window.grd` (map radius;
centre is (grd, grd)), `window.playing`, `window.rank`, `window.fpsls`/`fmlts`
(score tables). The older `snake`/`snakes`/`setAcceleration` names do not exist
in this build. Measured: cruise 180 world units/s (sp 5.79), boost 330 units/s
(sp 14), a 90° turn in ~0.35 s at scale 1 (turn rate ≈ 4.5/sc rad/s), width
round(sc·29); the y axis points DOWN and ang grows clockwise on screen (the mouse
straight below the centre reads +π/2). Boost is refused by the game at the
minimum length (score 10) — grow first.

Actions are real HID input through Quartz CGEventPost (a post costs 0.004 ms;
pyautogui.moveTo cost 12.8 ms with its catch-up sleep): the head sits at the
canvas centre and steers toward the mouse, so a heading is a MouseMoved (or
LeftMouseDragged while the button is held) at centre + 150·(cos a, sin a);
boost is LeftMouseDown held while allowed, LeftMouseUp otherwise, and the
button is released in a `finally` on every exit including Ctrl-C.

The state text the model sees names the controls — steer toward the mouse;
hold to boost, which burns length — then length, boosting, the nearest big head,
the nearest smaller snake, the wall, the distance to the map centre and the
kills so far. One predict carries two questions: `move` (at most 8 candidates,
code-described) and `boost` (noul).

Candidates and scores (code): 16 relative headings × {cruise, boost}, plus
"Cut off smaller snake" intercepts (predict the target head's path, reach a
point ~100 ahead of it before it does; kill score = P(intercept) × its length),
"Trap: circle the small snake" when one is inside our reach, "Dead snake food"
rushes to a death burst unless a bigger head is closer, and "Head to the map
centre" whose score rises with our distance from the centre (that is where the
players are). Every candidate is rolled forward 0.6 s against the wall (radius
grd−300), enemy body points and enemies' PREDICTED head positions; risk = the
minimum clearance, gain = food mass swept in a corridor two widths wide.

Objective, strict priority (Guru): (1) TERMINATE — whenever a feasible kill
exists (a smaller head within reach whose intercept we win), the cut-off / trap
option outranks every food option, in every state, from length 12 up; (2) EAT —
otherwise the best food or burst; food within 150 of the path is always free;
(3) SURVIVE — the shield, with the risk tolerance against BODIES relaxed to
0.7 × DANGER for at most 0.6 s while a cut-off is being executed (never against
a head, never the wall). The mode (terminate | eat) is timed per episode.

Shield: a pick with clearance under DANGER (60 + our width) is never executed
— the model's pick is rerouted to the NEAREST safe heading; boost only when length > 12,
the chosen path is clear for 500, never toward a head, and one of: a food
cluster of mass > 60 within ±60°, a cut-off, a bigger head within 300 behind or
beside us. The model's noul approves food/centre boosts; kill and escape boosts
are the code's call (a "no" would forfeit the intercept). Every intervention is
recorded. Kills are counted strictly in the page: an enemy that dies
(dead_amt > 0) or vanishes while its head is within 400 of any point of our body.

Recording: one JSONL per episode under ~/.config/eve-bridge/slither-runs/.
"""

from __future__ import annotations

import argparse
import asyncio
import json
import math
import os
import re
import shutil
import socket
import statistics
import subprocess
import sys
import tempfile
import time
import urllib.request
from dataclasses import dataclass, field
from datetime import datetime
from pathlib import Path
from typing import Any, Callable, Optional

import numpy as np

CHROME = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
GAME_URL = "http://slither.io"
DEFAULT_MODEL_PATH = "~/.config/eve-bridge/models/laya-multilingual-mlx"
RUNS_DIR = "~/.config/eve-bridge/slither-runs"

N_HEADINGS = 16
V_CRUISE = 180.0           # world units / s at sp 5.79 (measured)
V_BOOST = 330.0            # world units / s while boosting (measured, sp 14)
SP_NORMAL = 5.79
TURN_RATE = 4.5            # rad / s at scale 1 (measured: 90° in 0.35 s); divided by sc
HORIZON = 0.6              # s the rollout looks ahead
DT = 0.05
STEPS = int(round(HORIZON / DT))
WALL_MARGIN = 300.0        # the wall is treated as a body at radius grd - this
DANGER_BASE = 60.0         # DANGER = this + our width. 150 left the whole menu unsafe for seconds in a crowd
                           # (15-42 % of ticks in the 2026-09-21 baseline runs) and both deaths came out of that box
BIG_HEAD_INFLATE = 60.0    # extra clearance demanded from bigger snakes' predicted heads
BOOST_MIN_LENGTH = 15      # score (what the game shows as length); a boost must be affordable
HUNT_MIN_LENGTH = 12       # kills are hunted from here (Guru: terminate above eat, even when small)
BOOST_CLEAR = 400.0        # the path must be clear this far to boost
BOOST_HEAD_AHEAD = 350.0   # ... and no bigger head within this, ahead
THREAT_CONE = math.radians(60)   # a heading within this of a closing bigger head is never safe
BOOST_CLUSTER_MASS = 60.0
BOOST_CLUSTER_CONE = math.radians(60)
ESCAPE_RANGE = 450.0       # a bigger head within this, closing on us, is fled with a boost
CHASE_SECS = 3.0           # a cut-off that has not ended in a kill by then gives way to eating ...
CHASE_COOLDOWN = 5.0       # ... and that target is left alone for this long
CLOSING_UNITS = 100.0      # a bigger head is closing when its predicted path brings it this much nearer in 1 s
HEAD_CONE = math.radians(20)
HEAD_CONE_RANGE = 400.0
CUTOFF_RANGE = 900.0
CUTOFF_LEAD = 100.0        # lay our body this far ahead of the target's head
CUTOFF_MARGIN = 0.1        # s we must arrive before its head does
TRAP_RANGE = 250.0
TRAP_MIN_SEGMENTS = 30
BURST_FOOD = 10.0          # a food piece this big is a dead snake (pieces of 13.5 measured; normal food 5)
CENTRE_FAR = 3000.0        # beyond this the centre becomes an explicit option
MOUSE_RADIUS = 150.0
TARGET_HZ = 20.0
MAX_OPTIONS = 8
MIN_SAFE_OPTIONS = 4
KILL_PRIORITY = 1000.0     # TERMINATE outranks EAT: a feasible kill scores above every food option
BURST_PRIORITY = 500.0     # a dead snake's remains outrank every ordinary food and the centre drift
CENTRE_BOOST_LENGTH = 15   # knob: boosting toward the centre burns length (tuned across the ten episodes)
BURST_IDLE_SECS = 1.5      # a burst not targeted for this long is closed out in the log
RELAX_FACTOR = 0.7         # while executing a cut-off, bodies may come this close (× DANGER) ...
RELAX_SECS = 0.6           # ... for at most this long
FREE_FOOD = 150.0          # food within this of the path is swept for free
COMMIT_SECS = 1.0          # keep a target at least this long unless it vanishes or danger appears
SWITCH_MARGIN = 1.15       # a new option must beat the committed one's score by this factor
MAX_TURN_PER_TICK = math.radians(25)
HARD_TURN = math.radians(67.5)   # beyond this a heading is a hard turn: offered only when danger is ahead
CONTROLS = ("Slither.io. The snake steers toward the mouse. Holding the mouse button boosts speed "
            "but burns length.")
# The tunable schema GPT reflects over: name -> (min, max, what it does). Values are module globals.
KNOBS: dict[str, tuple[float, float, str]] = {
    "DANGER_BASE": (30, 150, "shield clearance floor = this + our width (world units)"),
    "BOOST_CLEAR": (200, 600, "boost only when the chosen path is clear this far"),
    "BOOST_HEAD_AHEAD": (200, 600, "no boost when a bigger head is within this, ahead"),
    "BOOST_MIN_LENGTH": (10, 60, "no boost below this length (score)"),
    "CENTRE_BOOST_LENGTH": (15, 500, "boost toward the map centre only from this length"),
    "CHASE_SECS": (1.5, 5.0, "a cut-off that has not killed by then gives way to eating"),
    "CHASE_COOLDOWN": (2.0, 15.0, "a timed-out target is left alone this long"),
    "COMMIT_SECS": (0.3, 2.0, "a target is kept at least this long"),
    "SWITCH_MARGIN": (1.0, 1.5, "a new target must beat the committed one's score by this factor"),
    "MAX_TURN_DEG": (10, 45, "max commanded turn per tick, degrees"),
    "HARD_TURN_DEG": (45, 120, "headings beyond this are offered only when danger is ahead"),
    "HORIZON": (0.4, 1.0, "rollout look-ahead, seconds"),
    "WALL_MARGIN": (150, 500, "the wall is treated as a body this far inside the edge"),
    "KILL_PRIORITY": (0, 2000, "score added to feasible kills (TERMINATE above EAT)"),
    "BURST_PRIORITY": (0, 2000, "score added to death bursts (above ordinary food)"),
    "CUTOFF_RANGE": (400, 1200, "smaller heads within this are hunted"),
    "CUTOFF_LEAD": (50, 200, "lay our body this far ahead of the target's head"),
    "HUNT_MIN_LENGTH": (10, 100, "no hunting below this length"),
    "ESCAPE_RANGE": (250, 700, "a closing bigger head within this is fled"),
    "THREAT_CONE_DEG": (30, 90, "headings within this of a closing bigger head are never safe"),
    "RELAX_FACTOR": (0.5, 1.0, "body clearance floor while cutting off, as a factor of DANGER"),
    "CLEARANCE_WEIGHT": (0.0, 0.2, "score per unit of clearance (capped at 600)"),
}
RULES: dict[str, str] = {
    "hard_turn_filter": "hide hard turns and reverse unless danger is ahead",
    "closing_head_unsafe": "never a heading within THREAT_CONE of a closing bigger head",
    "relaxation": "cut-offs may pass bodies at RELAX_FACTOR x DANGER for RELAX_SECS",
    "chase_timeout": "a chase gives way after CHASE_SECS and cools down",
    "burst_priority": "death bursts outrank ordinary food and the centre",
    "escape_boost": "boost away from a closing bigger head",
    "swing_wide": "when chased, prefer wide turns so the chaser burns boost",
    "planner_conditioning": "the GPT planner's objective and target weight the candidates",
    "stuck_reflex": "a 1.5 s straight run toward the waypoint when circling with no net displacement",
}
RULE_STATE: dict[str, bool] = {name: True for name in RULES}
MAX_TURN_DEG = 25.0
HARD_TURN_DEG = 67.5
THREAT_CONE_DEG = 60.0
CLEARANCE_WEIGHT = 0.05


def knob_values() -> dict[str, float]:
    return {name: float(globals()[name]) for name in KNOBS}


def set_knob(name: str, value: float) -> tuple[bool, str]:
    """Apply one knob within its bounds; (False, reason) otherwise. Never evaluates text."""
    if name not in KNOBS:
        return False, f"unknown knob {name!r}"
    lo, hi, _doc = KNOBS[name]
    try:
        v = float(value)
    except (TypeError, ValueError):
        return False, f"{name}: {value!r} is not a number"
    if not math.isfinite(v) or not lo <= v <= hi:
        return False, f"{name}: {v} outside [{lo}, {hi}]"
    globals()[name] = v
    return True, f"{name} = {v:g}"


def set_rule(name: str, enabled: Any) -> tuple[bool, str]:
    if name not in RULES:
        return False, f"unknown rule {name!r}"
    if not isinstance(enabled, bool):
        return False, f"{name}: enabled must be true or false"
    RULE_STATE[name] = enabled
    return True, f"{name} = {'on' if enabled else 'off'}"


def load_settings(path: Optional[Path]) -> dict[str, Any]:
    """Knob values and rule states persisted across the series (a JSON file); unknown keys ignored."""
    if path is None or not path.exists():
        return {}
    data = json.loads(path.read_text())
    for name, value in (data.get("knobs") or {}).items():
        set_knob(name, value)
    for name, enabled in (data.get("rules") or {}).items():
        set_rule(name, enabled)
    return data


def save_settings(path: Path) -> None:
    path.write_text(json.dumps({"knobs": knob_values(), "rules": dict(RULE_STATE)}, indent=1))

FEATURE_JS = r"""(function () {
  var s = window.slither;
  var B = window.__eve || (window.__eve = {seen: {}, counted: {}, kills: 0});
  if (!s || !window.playing) { return JSON.stringify({playing: false, kills: B.kills}); }
  var grd = window.grd || 0, W = Math.round((s.sc || 1) * 29), NS = 16, TWO_PI = Math.PI * 2;
  var out = {playing: true, x: s.xx, y: s.yy, ang: s.ang, sp: s.sp, ssp: s.ssp || 5.79, sc: s.sc || 1,
             sct: s.sct || 0, fam: s.fam || 0, w: W, grd: grd, rank: window.rank || 0, kills: B.kills,
             npts: (s.pts || []).length};
  try {
    if (window.fpsls && window.fmlts && window.fpsls[s.sct] !== undefined) {
      out.score = Math.floor(15 * (window.fpsls[s.sct] + s.fam / window.fmlts[s.sct] - 1) - 5);
    }
  } catch (e) {}
  function nearBody(x, y) {
    var pts = s.pts || [], r2 = 400 * 400, dx, dy;
    for (var i = 0; i < pts.length; i++) {
      var p = pts[i]; if (!p) continue;
      dx = p.xx - x; dy = p.yy - y; if (dx * dx + dy * dy <= r2) return true;
    }
    dx = s.xx - x; dy = s.yy - y; return dx * dx + dy * dy <= r2;
  }
  var sec = [], heads = [], body = [], now = {};
  for (var q = 0; q < NS; q++) sec.push(9999);
  var sl = window.slithers || [];
  for (var i = 0; i < sl.length; i++) {
    var o = sl[i];
    if (!o || o === s || o.id === s.id) continue;
    var hx = o.xx - s.xx, hy = o.yy - s.yy, hd = Math.sqrt(hx * hx + hy * hy);
    now[o.id] = [o.xx, o.yy];
    if ((o.dead_amt || 0) > 0 && !B.counted[o.id]) { B.counted[o.id] = 1; if (nearBody(o.xx, o.yy)) B.kills++; }
    if ((o.dead_amt || 0) > 0) continue;
    if (hd <= 800) heads.push([Math.round(hx), Math.round(hy), Math.round(o.ang * 1000) / 1000,
                               Math.round((o.sp || 5.79) * 100) / 100, o.sct || 0, Math.round((o.sc || 1) * 100) / 100,
                               o.id]);
    if (hd > 700 + 3000) continue;
    var op = o.pts || [];
    for (var j = 0; j < op.length; j += 2) {
      var p = op[j]; if (!p || p.dying) continue;
      var dx = p.xx - s.xx, dy = p.yy - s.yy, d2 = dx * dx + dy * dy;
      if (d2 > 700 * 700) continue;
      var d = Math.sqrt(d2), rel = Math.atan2(dy, dx) - s.ang;
      var k = Math.round(rel / (TWO_PI / NS)); k = ((k % NS) + NS) % NS;
      if (d < sec[k]) sec[k] = Math.round(d);
      body.push(Math.round(dx), Math.round(dy));
    }
  }
  for (var id in B.seen) {
    if (!(id in now) && !B.counted[id]) { B.counted[id] = 1; var h = B.seen[id]; if (nearBody(h[0], h[1])) B.kills++; }
  }
  B.seen = now;
  out.kills = B.kills; out.sectors = sec; out.heads = heads; out.body = body;
  var fd = window.foods || [], grid = {}, bursts = {};
  for (var k2 = 0; k2 < fd.length; k2++) {
    var f = fd[k2]; if (!f) continue;
    var fx = f.xx - s.xx, fy = f.yy - s.yy, fd2 = fx * fx + fy * fy, sz = f.sz || 1;
    if (sz >= 10 && fd2 <= 900 * 900) {          // dead-snake pieces are ~13.5 (normal food ~5), measured ep1
      var bk = Math.floor(fx / 300) + ',' + Math.floor(fy / 300);
      var b = bursts[bk] || (bursts[bk] = [0, 0, 0, 0]);
      b[0] += fx * sz; b[1] += fy * sz; b[2] += sz; b[3]++;
    }
    if (fd2 > 600 * 600) continue;
    var gk = Math.floor(fx / 40) + ',' + Math.floor(fy / 40);
    var g = grid[gk] || (grid[gk] = [0, 0, 0]);
    g[0] += fx * sz; g[1] += fy * sz; g[2] += sz;
  }
  var clusters = [];
  for (var gk2 in grid) {
    var c = grid[gk2], m = c[2], cxx = c[0] / m, cyy = c[1] / m, cd = Math.max(20, Math.sqrt(cxx * cxx + cyy * cyy));
    clusters.push([Math.round(cxx), Math.round(cyy), Math.round(m * 10) / 10, m * m / cd]);
  }
  clusters.sort(function (a, b) { return b[3] - a[3]; });
  out.food = clusters.slice(0, 8).map(function (c) { return [c[0], c[1], c[2]]; });
  out.bursts = [];
  for (var bk2 in bursts) {
    var bb = bursts[bk2];
    out.bursts.push([Math.round(bb[0] / bb[2]), Math.round(bb[1] / bb[2]), Math.round(bb[2]), bb[3]]);
  }
  return JSON.stringify(out);
})()"""

RANK_JS = ("(function(){var m=(document.body.innerText||'').match(/Your rank:\\s*(\\d+)\\s*of\\s*(\\d+)/); "
           "return JSON.stringify({rank: window.rank || 0, dom: m ? [parseInt(m[1]), parseInt(m[2])] : null});})()")
SCREEN_JS = ("JSON.stringify({sx: window.screenX, sy: window.screenY, ow: window.outerWidth, "
             "oh: window.outerHeight, iw: window.innerWidth, ih: window.innerHeight})")
PLAY_JS = ("(function(){window.__eve = null; var n=document.getElementById('nick'); if(n) n.value='eve'; "
           "var b=document.getElementById('playh'); if(!b) return 'no play button'; "
           "var t=b.querySelector('div,.btn')||b; t.click(); b.click(); return 'clicked';})()")


# ---- geometry (pure) ------------------------------------------------------------------

def wrap(a: float) -> float:
    """Angle into (-pi, pi]."""
    a = math.fmod(a + math.pi, 2 * math.pi)
    if a <= 0:
        a += 2 * math.pi
    return a - math.pi


def heading_name(delta: float) -> str:
    deg = int(round(math.degrees(delta)))
    if deg == 0:
        return "straight"
    if abs(deg) == 180:
        return "reverse"
    return f"{'right' if deg > 0 else 'left'} {abs(deg)}°"


HEADING_DELTAS: tuple[float, ...] = tuple(wrap(k * 2 * math.pi / N_HEADINGS) for k in range(N_HEADINGS))


def canvas_centre(geometry: dict[str, Any]) -> tuple[float, float]:
    """The page viewport's centre in screen points from window.screenX/Y, outer/inner sizes."""
    chrome_h = float(geometry["oh"]) - float(geometry["ih"])
    return (float(geometry["sx"]) + float(geometry["iw"]) / 2.0,
            float(geometry["sy"]) + chrome_h + float(geometry["ih"]) / 2.0)


def screen_point(cx: float, cy: float, heading: float, radius: float = MOUSE_RADIUS) -> tuple[int, int]:
    """Where to put the mouse for an ABSOLUTE heading: ang grows clockwise, y down."""
    return int(round(cx + radius * math.cos(heading))), int(round(cy + radius * math.sin(heading)))


def wall_distance(x: float, y: float, heading: float, grd: float) -> float:
    """Distance from (x, y) to the circular map edge along `heading`; the centre is (grd, grd)."""
    px, py = x - grd, y - grd
    ux, uy = math.cos(heading), math.sin(heading)
    b = px * ux + py * uy
    c = px * px + py * py - grd * grd
    disc = b * b - c
    if disc <= 0:
        return 0.0
    return max(0.0, -b + math.sqrt(disc))


def units_per_sec(sp: float) -> float:
    return V_CRUISE * float(sp) / SP_NORMAL


# ---- candidates and the rollout (pure, numpy) ------------------------------------------

@dataclass
class Candidate:
    key: str
    kind: str                     # heading | cutoff | trap | burst | centre
    heading: float                # absolute, radians
    boost: bool
    speed: float
    bonus: float = 0.0            # code score beyond risk and food
    label: str = ""               # what the model reads (filled by describe)
    target_len: int = 0           # the smaller snake's segments, for kill candidates
    risk: float = math.inf        # min clearance along the rollout
    risk_what: str = ""
    gain: float = 0.0             # food mass swept
    score: float = 0.0
    safe: bool = True
    p_kill: float = 0.0
    ident: str = ""              # what the controller commits to: a cluster cell, an enemy id, the centre,
                                  # or "cruise" for every plain heading (a heading is not a target)
    mass: float = 0.0             # food mass of a cluster / burst candidate
    centre_dist: float = 0.0      # for the centre candidate's label
    delta: float = 0.0            # relative heading, for the hard-turn rule


@dataclass
class Plan:
    candidates: list[Candidate]   # every candidate, scored
    options: list[Candidate]      # the ones the model sees, in menu order
    best: Candidate               # the code's pick
    state: str
    danger: float
    length: int
    centre_dist: float
    nearest_big: Optional[tuple[float, float]] = None     # (distance, relative angle)
    nearest_small: Optional[tuple[float, float, int]] = None
    mode: str = "eat"             # terminate | eat: strict priority, kills first
    relaxed: bool = False         # the cut-off risk relaxation was in force this tick
    danger_ahead: bool = False


def rollout(ang: float, headings: np.ndarray, speeds: np.ndarray, omega: float) -> np.ndarray:
    """Positions (C, STEPS, 2) relative to the head for candidates turning toward `headings`
    at `omega` rad/s and moving at `speeds` units/s."""
    c = len(headings)
    h = np.full(c, float(ang))
    pos = np.zeros((c, 2))
    steps = max(1, int(round(HORIZON / DT)))
    out = np.zeros((c, steps, 2))
    max_turn = omega * DT
    for i in range(steps):
        diff = (headings - h + math.pi) % (2 * math.pi) - math.pi
        h = h + np.clip(diff, -max_turn, max_turn)
        pos = pos + np.stack([np.cos(h), np.sin(h)], axis=1) * (speeds * DT)[:, None]
        out[:, i] = pos
    return out


def clearance(paths: np.ndarray, state: dict[str, Any], width: float) -> tuple[np.ndarray, list[str]]:
    """Minimum clearance per candidate against enemy bodies, predicted enemy heads and the
    wall, and the name of what limited it."""
    c = paths.shape[0]
    best = np.full(c, 9999.0)
    what = ["open"] * c
    body = np.asarray(state.get("body") or [], dtype=float).reshape(-1, 2)
    if len(body):
        d = np.sqrt(((paths[:, :, None, :] - body[None, None, :, :]) ** 2).sum(-1)).min(axis=2)  # (C, S)
        m = d.min(axis=1) - width
        upd = m < best
        best = np.where(upd, m, best)
        for i in np.nonzero(upd)[0]:
            what[i] = "snake"
    my_sct = int(state.get("sct", 0))
    t = np.arange(1, paths.shape[1] + 1) * DT
    for hx, hy, hang, hsp, hsct, hsc, _hid in state.get("heads") or []:
        v = units_per_sec(hsp)
        pred = np.stack([hx + v * t * math.cos(hang), hy + v * t * math.sin(hang)], axis=1)   # (S, 2)
        d = np.sqrt(((paths - pred[None, :, :]) ** 2).sum(-1)).min(axis=1)                    # (C,)
        margin = width / 2 + float(hsc) * 29 / 2 + (BIG_HEAD_INFLATE if hsct >= my_sct else 0.0)
        m = d - margin
        upd = m < best
        best = np.where(upd, m, best)
        name = "big head" if hsct >= my_sct else "head"
        for i in np.nonzero(upd)[0]:
            what[i] = name
    grd = float(state.get("grd", 0))
    if grd > 0:
        centre = np.array([grd - float(state["x"]), grd - float(state["y"])])
        r = np.sqrt(((paths - centre[None, None, :]) ** 2).sum(-1)).max(axis=1)              # farthest from centre
        m = (grd - WALL_MARGIN) - r
        upd = m < best
        best = np.where(upd, m, best)
        for i in np.nonzero(upd)[0]:
            what[i] = "wall"
    return best, what


def food_gain(paths: np.ndarray, food: list, width: float) -> np.ndarray:
    """Food mass within FREE_FOOD (or a width) of each path: swept for free."""
    c = paths.shape[0]
    if not food:
        return np.zeros(c)
    f = np.asarray(food, dtype=float)
    d = np.sqrt(((paths[:, :, None, :] - f[None, None, :, :2]) ** 2).sum(-1)).min(axis=1)   # (C, F)
    return (d <= max(width, FREE_FOOD)) @ f[:, 2]


def cutoff_candidates(state: dict[str, Any], length: int) -> list[Candidate]:
    """Intercepts of smaller snakes: reach a point CUTOFF_LEAD ahead of the target's predicted
    head before it does. Kill score = P(intercept) × its length."""
    out: list[Candidate] = []
    my_sct = int(state.get("sct", 0))
    if length <= HUNT_MIN_LENGTH:
        return out
    ang = float(state["ang"])
    sc = float(state.get("sc", 1))
    heads = state.get("heads") or []
    food = state.get("food") or []
    for hx, hy, hang, hsp, hsct, _hsc, hid in heads:
        if hsct >= my_sct:
            continue
        hd = math.hypot(hx, hy)
        if hd > CUTOFF_RANGE or hd == 0:
            continue
        v_e = units_per_sec(hsp)
        dx, dy = math.cos(hang), math.sin(hang)
        stable = any(math.hypot(fx - hx, fy - hy) <= 300 and (fx - hx) * dx + (fy - hy) * dy > 0 for fx, fy, _m in food)
        best: Optional[tuple[float, float, float, bool, float]] = None
        for t in np.arange(0.2, max(0.4, CHASE_SECS - 0.19), 0.2):
            qx, qy = hx + dx * (v_e * t + CUTOFF_LEAD), hy + dy * (v_e * t + CUTOFF_LEAD)
            d_us = math.hypot(qx, qy)
            turn = abs(wrap(math.atan2(qy, qx) - ang)) / (TURN_RATE / sc)
            t_e = t + CUTOFF_LEAD / v_e
            t_cruise = d_us / V_CRUISE + turn
            t_boost = d_us / V_BOOST + turn
            if t_cruise <= t_e - CUTOFF_MARGIN:
                slack, boost = t_e - t_cruise, False
            elif t_boost <= t_e - CUTOFF_MARGIN:
                slack, boost = t_e - t_boost, True
            else:
                continue
            if best is None or slack > best[0]:
                best = (slack, qx, qy, boost, t_e)
        if best is None:
            continue
        slack, qx, qy, boost, _t_e = best
        p = min(1.0, slack / 0.8) * (1.0 if stable else 0.8)
        if any(bsct >= my_sct and math.hypot(bx - qx, by - qy) < 400 for bx, by, _a, _s, bsct, _c, _i in heads):
            p *= 0.5
        heading = math.atan2(qy, qx)
        out.append(Candidate(key=f"cut off snake {int(hsct)}", kind="cutoff", heading=heading, boost=boost,
                             speed=V_BOOST if boost else V_CRUISE, bonus=10.0 * p * max(1, hsct),
                             target_len=int(hsct), p_kill=p, ident=f"enemy:{hid}"))
    return out


def trap_candidates(state: dict[str, Any]) -> list[Candidate]:
    my_sct = int(state.get("sct", 0))
    if my_sct < TRAP_MIN_SEGMENTS:
        return []
    out: list[Candidate] = []
    width = float(state.get("w", 29))
    for hx, hy, _hang, _hsp, hsct, _hsc, hid in state.get("heads") or []:
        hd = math.hypot(hx, hy)
        if hsct >= my_sct or hd > TRAP_RANGE or hd == 0:
            continue
        r = 150.0 + width
        to_us = math.atan2(-hy, -hx)                  # from the target to us
        tangent = to_us + math.pi / 2                 # circle clockwise around it
        toward = math.atan2(hy, hx)
        heading = tangent if hd <= r * 1.2 else wrap(toward + 0.5 * wrap(tangent - toward))
        out.append(Candidate(key=f"trap snake {int(hsct)}", kind="trap", heading=heading, boost=False,
                             speed=V_CRUISE, bonus=5.0 * hsct, target_len=int(hsct), p_kill=0.5, ident=f"enemy:{hid}"))
        break
    return out


def burst_candidates(state: dict[str, Any], length: int) -> list[Candidate]:
    out: list[Candidate] = []
    my_sct = int(state.get("sct", 0))
    heads = state.get("heads") or []
    for bx, by, mass, _n in state.get("bursts") or []:
        d = math.hypot(bx, by)
        if d == 0:
            continue
        if any(hsct >= my_sct and math.hypot(hx - bx, hy - by) < d for hx, hy, _a, _s, hsct, _c, _i in heads):
            continue
        boost = d > 250 and length > BOOST_MIN_LENGTH
        out.append(Candidate(key=f"dead snake food {int(mass)}", kind="burst", heading=math.atan2(by, bx),
                             boost=boost, speed=V_BOOST if boost else V_CRUISE,
                             bonus=(BURST_PRIORITY if RULE_STATE["burst_priority"] else 0.0) + float(mass),
                             mass=float(mass), ident="burst"))      # one target: sweep it until it is gone
    return out


def cell_ident(state: dict[str, Any], dx: float, dy: float) -> str:
    """A stable identity for a spot in the world: its 100-unit cell in absolute coordinates."""
    return f"cell:{int((float(state['x']) + dx) // 100)},{int((float(state['y']) + dy) // 100)}"


def food_candidates(state: dict[str, Any], length: int) -> list[Candidate]:
    """The best clusters by mass² / distance, each an explicit option; boost toward big ones."""
    out: list[Candidate] = []
    ranked = sorted((c for c in state.get("food") or [] if math.hypot(c[0], c[1]) > 0),
                    key=lambda c: c[2] * c[2] / max(20.0, math.hypot(c[0], c[1])), reverse=True)
    for fx, fy, mass in ranked[:3]:
        d = math.hypot(fx, fy)
        boost = mass > BOOST_CLUSTER_MASS and d > 200 and length > BOOST_MIN_LENGTH
        out.append(Candidate(key=f"food {int(mass)} at {int(d)}", kind="food", heading=math.atan2(fy, fx),
                             boost=boost, speed=V_BOOST if boost else V_CRUISE, mass=float(mass),
                             bonus=10.0 * float(mass) * float(mass) / max(20.0, d), ident=cell_ident(state, fx, fy)))
    return out


def centre_candidate(state: dict[str, Any], length: int) -> Optional[Candidate]:
    grd = float(state.get("grd", 0))
    if grd <= 0:
        return None
    dx, dy = grd - float(state["x"]), grd - float(state["y"])
    d = math.hypot(dx, dy)
    if d <= 0:
        return None
    boost = length >= CENTRE_BOOST_LENGTH and d >= CENTRE_FAR
    return Candidate(key="map centre", kind="centre", heading=math.atan2(dy, dx), boost=boost, centre_dist=d,
                     speed=V_BOOST if boost else V_CRUISE, bonus=min(40.0, d / 100.0), ident="centre")


def closing_big_head(state: dict[str, Any]) -> Optional[tuple[float, float]]:
    """(distance, relative angle) of the nearest bigger head within ESCAPE_RANGE whose predicted path
    brings it CLOSING_UNITS nearer within a second — the thing that kills small snakes."""
    my_sct = int(state.get("sct", 0))
    ang = float(state["ang"])
    best = None
    for hx, hy, hang, hsp, hsct, _c, _i in state.get("heads") or []:
        if hsct < my_sct:
            continue
        d = math.hypot(hx, hy)
        if d > ESCAPE_RANGE:
            continue
        v = units_per_sec(hsp)
        d1 = math.hypot(hx + v * math.cos(hang), hy + v * math.sin(hang))
        if d - d1 >= CLOSING_UNITS and (best is None or d < best[0]):
            best = (d, wrap(math.atan2(hy, hx) - ang))
    return best


def boost_allowed(c: Candidate, state: dict[str, Any], length: int, policy: str = "normal") -> tuple[bool, str]:
    """The code gate on boosting (Guru: speed matters — boost whenever the corridor is clear and we are
    moving toward a target). The planner's policy widens (aggressive) or narrows (conserve) it; the
    model's noul then approves food/centre boosts."""
    if length <= BOOST_MIN_LENGTH:
        return False, f"length {length} <= {BOOST_MIN_LENGTH}"
    clear = BOOST_CLEAR * (0.75 if policy == "aggressive" else 1.0)
    if c.risk < clear:
        return False, f"path not clear ({int(c.risk)} < {int(clear)})"
    ang = float(state["ang"])
    my_sct = int(state.get("sct", 0))
    for hx, hy, _a, _s, hsct, _c, _i in state.get("heads") or []:
        hd = math.hypot(hx, hy)
        rel = abs(wrap(math.atan2(hy, hx) - c.heading))
        if hd <= HEAD_CONE_RANGE and rel <= HEAD_CONE:
            return False, "toward a head"
        if hsct >= my_sct and hd <= BOOST_HEAD_AHEAD and rel <= math.pi / 3:
            return False, f"big head {int(hd)} ahead"
    if c.kind == "cutoff":
        return True, "cut-off"
    if c.kind == "burst":
        return True, "dead snake food"
    if policy == "conserve" and c.kind not in ("cutoff", "burst"):
        threat = closing_big_head(state)
        if RULE_STATE["escape_boost"] and threat is not None and abs(wrap(c.heading - (ang + threat[1]))) >= math.pi / 2:
            return True, "escape"
        return False, "conserving"
    threat = closing_big_head(state)
    if RULE_STATE["escape_boost"] and threat is not None and abs(wrap(c.heading - (ang + threat[1]))) >= math.pi / 2:
        return True, "escape"                                # away from a bigger head that is closing
    if c.kind == "food":
        return True, "food cluster"
    if c.kind in ("centre", "waypoint"):
        if length >= CENTRE_BOOST_LENGTH or policy == "aggressive":
            return True, "to the centre" if c.kind == "centre" else "to the waypoint"
        return False, "walking to the centre"
    if policy == "aggressive" and c.kind == "heading":
        return True, "aggressive"
    for fx, fy, mass in state.get("food") or []:
        if mass > BOOST_CLUSTER_MASS and abs(wrap(math.atan2(fy, fx) - c.heading)) <= BOOST_CLUSTER_CONE:
            return True, "food cluster"
    return False, "nothing to gain"


def game_length(state: dict[str, Any]) -> int:
    score = state.get("score")
    if isinstance(score, (int, float)) and score >= 0:
        return int(score)
    return int(state.get("sct", 0))


def describe(c: Candidate, best: bool) -> str:
    if not c.safe:
        return f"Danger: {c.risk_what} {int(max(0, c.risk))}. Collision."
    if c.kind == "cutoff":
        text = f"Cut off smaller snake (len {c.target_len})" + (" — boost" if c.boost else "")
    elif c.kind == "trap":
        text = f"Trap: circle the small snake (len {c.target_len})"
    elif c.kind == "burst":
        text = f"Dead snake food (mass {int(c.mass)})" + (" — boost" if c.boost else "")
    elif c.kind == "centre":
        text = f"Head to the map centre (dist {int(c.centre_dist)})" + (" — boost" if c.boost else "")
    elif c.kind == "food":
        text = f"Food cluster (mass {int(c.mass)})" + (" — boost" if c.boost else "")
    else:
        text = f"Safe {int(min(c.risk, 9999))}."
        if c.gain > 0:
            text += f" Food {int(c.gain)} ahead."
        if c.boost:
            text += " Boost."
    if best:
        text += " Best."
    return text


def plan(state: dict[str, Any], relax: bool = False, exclude: Optional[set[str]] = None,
         directive: Optional["Directive"] = None) -> Plan:
    """Sample, roll out, score, and choose what the model sees. Pure. `relax`: a cut-off is in
    progress, so its candidate may pass bodies at RELAX_FACTOR × DANGER (never heads, never the wall)."""
    ang = float(state["ang"])
    sc = float(state.get("sc", 1))
    width = float(state.get("w", round(sc * 29)))
    length = game_length(state)
    danger = DANGER_BASE + width
    cands: list[Candidate] = []
    for delta in HEADING_DELTAS:
        for boost in (False, True):
            name = heading_name(delta)
            cands.append(Candidate(key=name + (" boost" if boost else ""), kind="heading", heading=ang + delta,
                                   boost=boost, speed=V_BOOST if boost else V_CRUISE, ident="cruise",
                                   delta=delta))
    skip = exclude or set()
    cands += [c for c in cutoff_candidates(state, length) if c.ident not in skip]
    cands += [c for c in trap_candidates(state) if c.ident not in skip]
    cands += burst_candidates(state, length)
    cands += food_candidates(state, length)
    centre = centre_candidate(state, length)
    if centre is not None:
        cands.append(centre)
    wpc = waypoint_candidate(state, directive, length)
    if wpc is not None:
        cands.append(wpc)
    headings = np.array([c.heading for c in cands])
    speeds = np.array([c.speed for c in cands])
    paths = rollout(ang, headings, speeds, TURN_RATE / max(sc, 0.5))
    risk, what = clearance(paths, state, width)
    gain = food_gain(paths, state.get("food") or [], width)
    my_sct = int(state.get("sct", 0))
    heads = state.get("heads") or []
    nearest_big = None
    nearest_small = None
    for hx, hy, _a, _s, hsct, _c, _i in heads:
        hd = math.hypot(hx, hy)
        rel = wrap(math.atan2(hy, hx) - ang)
        if hsct >= my_sct and (nearest_big is None or hd < nearest_big[0]):
            nearest_big = (hd, rel)
        if hsct < my_sct and (nearest_small is None or hd < nearest_small[0]):
            nearest_small = (hd, rel, int(hsct))
    threat = closing_big_head(state)
    chased = threat is not None
    for c, r, w, g in zip(cands, risk, what, gain, strict=True):
        c.risk, c.risk_what, c.gain = float(r), w, float(g)
        relaxing = relax and RULE_STATE["relaxation"] and c.kind == "cutoff" and w == "snake"
        floor = danger * RELAX_FACTOR if relaxing else danger
        c.safe = c.risk >= floor
        if chased and RULE_STATE["closing_head_unsafe"] and abs(wrap(c.heading - (ang + threat[1]))) <= math.radians(THREAT_CONE_DEG):
            c.safe, c.risk_what = False, "big head"          # SURVIVE: never toward a closing bigger head
            c.risk = min(c.risk, threat[0] - danger)
        if not c.safe:
            c.score = -1000.0 + c.risk
            continue
        c.score = c.gain + CLEARANCE_WEIGHT * min(c.risk, 600.0) + c.bonus   # wider clearance is worth food
        if c.kind in ("cutoff", "trap"):
            c.score += KILL_PRIORITY                          # TERMINATE above EAT, in every state
        if c.boost and c.kind == "heading":
            c.score -= 2.0                                    # a plain boost burns length for nothing
        if nearest_big is not None and nearest_big[0] <= 600:
            away = math.cos(wrap(c.heading - (ang + nearest_big[1])))   # -1 = straight away from it
            c.score += 3.0 * max(0.0, -away)                  # prefer the tail side
        if chased:
            away = math.cos(wrap(c.heading - (ang + threat[1])))
            c.score += 12.0 * max(0.0, -away)                 # SURVIVE: a closing bigger head outranks food
            if RULE_STATE["swing_wide"] and abs(wrap(c.heading - ang)) >= math.pi / 4:
                c.score += 4.0                                # swing wide: the chaser burns boost
    condition(cands, state, directive)
    mode = "terminate" if any(c.safe and c.kind in ("cutoff", "trap") for c in cands) else "eat"
    straight = [c for c in cands if c.kind == "heading" and not c.boost and abs(c.delta) <= math.radians(22.5)]
    danger_ahead = not any(c.safe for c in straight)
    hard = math.radians(HARD_TURN_DEG)
    offerable = [c for c in cands if danger_ahead or c.kind != "heading" or abs(c.delta) <= hard
                 or not RULE_STATE["hard_turn_filter"]]
    ranked = sorted(offerable, key=lambda c: c.score, reverse=True)
    best = ranked[0]
    safe = [c for c in ranked if c.safe]
    options: list[Candidate] = []
    for c in safe[:MIN_SAFE_OPTIONS]:
        options.append(c)
    for c in ranked:
        if len(options) >= MAX_OPTIONS:
            break
        if c not in options:
            options.append(c)
    options.sort(key=lambda c: c.score, reverse=True)
    for c in options:
        c.label = describe(c, c is best)
    grd = float(state.get("grd", 0))
    centre_dist = math.hypot(grd - float(state["x"]), grd - float(state["y"])) if grd > 0 else 0.0
    boosting = "yes" if float(state.get("sp", 0)) > float(state.get("ssp", SP_NORMAL)) + 1.0 else "no"
    big = (f"Nearest big head {int(nearest_big[0])} {side(nearest_big[1])}." if nearest_big is not None
           else "No bigger snake near.")
    small = (f"Smaller snake (len {nearest_small[2]}) {int(nearest_small[0])} {side(nearest_small[1])}."
             if nearest_small is not None else "No smaller snake near.")
    wall = min(c.risk for c in cands if c.risk_what == "wall") if any(c.risk_what == "wall" for c in cands) else 9999.0
    wall_text = "far" if wall >= 1000 else f"{int(wall)} away"
    text = (f"{CONTROLS} Length {length}. Boosting: {boosting}. {big} {small} Wall: {wall_text}. "
            f"Centre {int(centre_dist)} away. Kills {int(state.get('kills', 0))}.")
    return Plan(cands, options, best, text, danger, length, centre_dist, nearest_big, nearest_small, mode=mode,
                relaxed=relax, danger_ahead=danger_ahead)


def side(rel: float) -> str:
    deg = math.degrees(rel)
    if abs(deg) <= 22.5:
        return "ahead"
    if abs(deg) >= 157.5:
        return "behind"
    return "right" if deg > 0 else "left"


QUESTION_MOVE = "Choose the action: kill a smaller snake when you can, else food, always safe."
QUESTION_BOOST = ("Should the snake boost now by holding the mouse button? Boosting is faster but burns "
                  "length; boost to cut off a smaller snake, grab a big food cluster, or escape a bigger snake.")
BOOST_CRITERIA = {"false": "cruise at normal speed, keep the length",
                  "true": "hold the mouse button to boost now"}


def questions(p: Plan) -> dict[str, Any]:
    return {"move": {"type": "choice", "instructions": QUESTION_MOVE,
                     "criteria": {c.key: c.label for c in p.options}},
            "boost": {"type": "noul", "instructions": QUESTION_BOOST, "criteria": BOOST_CRITERIA}}


@dataclass
class Verdict:
    proposed: str
    executed: Candidate
    intervened: bool              # the shield rerouted an unsafe pick
    held: bool                    # commitment kept the previous target over the model's pick
    switched: bool                # the committed target changed this tick
    heading: float                # the smoothed absolute heading actually commanded
    boost: bool
    boost_reason: str


def nearest_safe(p: Plan, heading: float) -> Candidate:
    """The safe cruise heading closest in angle to `heading`; with none safe, the clearest."""
    safe = [c for c in p.candidates if c.safe and c.kind == "heading" and not c.boost]
    if safe:
        return min(safe, key=lambda c: abs(wrap(c.heading - heading)))
    return max(p.candidates, key=lambda c: c.risk)


def boost_decision(executed: Candidate, p: Plan, p_boost: float, state: dict[str, Any],
                   policy: str = "normal") -> tuple[bool, str]:
    ok, reason = boost_allowed(executed, state, p.length, policy)
    if not ok:
        return False, reason
    if reason in ("cut-off", "escape", "dead snake food", "aggressive", "to the waypoint"):
        return True, reason
    if p_boost >= 0.5:
        return True, reason
    return False, "model says no"


class BurstLog:
    """Per-burst accounting: pieces and mass when first targeted, length gained while on it,
    closed when the burst is gone or untargeted for BURST_IDLE_SECS."""

    def __init__(self) -> None:
        self.active: Optional[dict[str, Any]] = None

    def tick(self, executed: Candidate, state: dict[str, Any], length: int, now: float) -> Optional[str]:
        bursts = state.get("bursts") or []
        if executed.kind == "burst":
            if self.active is None:
                self.active = {"pieces": sum(int(b[3]) for b in bursts), "mass": int(sum(b[2] for b in bursts)),
                               "len0": length, "peak_mass": int(sum(b[2] for b in bursts)), "last": now}
            else:
                self.active["peak_mass"] = max(self.active["peak_mass"], int(sum(b[2] for b in bursts)))
                self.active["pieces"] = max(self.active["pieces"], sum(int(b[3]) for b in bursts))
                self.active["last"] = now
        if self.active is not None and (not bursts or now - self.active["last"] > BURST_IDLE_SECS):
            a, self.active = self.active, None
            return f"burst: {a['pieces']} pieces, {a['peak_mass']} mass, collected {max(0, length - a['len0'])}"
        return None


class Controller:
    """Commitment, hysteresis and smoothing on top of the shield: keep a target for COMMIT_SECS
    unless it vanishes or danger appears, switch only for a SWITCH_MARGIN better score, turn at
    most MAX_TURN_PER_TICK per tick, and reroute an unsafe pick to the NEAREST safe heading."""

    def __init__(self) -> None:
        self.target: str = ""
        self.since: float = 0.0
        self.cmd: Optional[float] = None
        self.changes = 0
        self.cutoff_since: Optional[float] = None    # when the current cut-off started (for the risk relaxation)
        self.cooldown: dict[str, float] = {}         # enemy ident -> until when it is left alone (chase timed out)
        self.timeouts = 0
        self.policy = "normal"                       # the planner's boost policy

    def decide(self, p: Plan, probabilities: dict[str, float], p_boost: float, state: dict[str, Any],
               now: float) -> Verdict:
        proposed = max((c.key for c in p.options), key=lambda k: probabilities.get(k, 0.0))
        pick = next(c for c in p.options if c.key == proposed)
        current = None
        if self.target:
            same = [c for c in p.candidates if c.ident == self.target and c.safe]
            if same:
                current = max(same, key=lambda c: c.score)
        held = False
        if current is not None and pick.ident != self.target and not p.danger_ahead:
            if now - self.since < COMMIT_SECS or pick.score < current.score * SWITCH_MARGIN:
                pick, held = current, True
        chosen = pick
        intervened = False
        if not chosen.safe:
            chosen = nearest_safe(p, pick.heading)
            intervened = True
        switched = chosen.ident != self.target
        if switched:
            self.target, self.since = chosen.ident, now
            self.changes += 1
        if chosen.kind in ("cutoff", "trap"):
            if self.cutoff_since is None or switched:
                self.cutoff_since = now
            elif RULE_STATE["chase_timeout"] and now - self.cutoff_since > CHASE_SECS:   # no kill: give way
                self.cooldown[chosen.ident] = now + CHASE_COOLDOWN
                self.timeouts += 1
        else:
            self.cutoff_since = None
        if self.cmd is None or intervened or p.danger_ahead:
            heading = chosen.heading
        else:
            step = math.radians(MAX_TURN_DEG)
            heading = self.cmd + max(-step, min(step, wrap(chosen.heading - self.cmd)))
        self.cmd = heading
        boost, reason = boost_decision(chosen, p, p_boost, state, self.policy)
        return Verdict(proposed, chosen, intervened, held, switched, heading, boost, reason)

    def excluded(self, now: float) -> set[str]:
        self.cooldown = {k: v for k, v in self.cooldown.items() if v > now}
        return set(self.cooldown)

    def relax(self, now: float) -> bool:
        """SURVIVE, relaxed: a cut-off in progress may pass bodies closer, for RELAX_SECS at most."""
        return self.cutoff_since is not None and now - self.cutoff_since <= RELAX_SECS

    def reset(self) -> None:
        self.target, self.since, self.cmd, self.changes, self.cutoff_since = "", 0.0, None, 0, None
        self.cooldown, self.timeouts = {}, 0


# ---- summaries (pure) ------------------------------------------------------------------

def percentiles(values: list[float]) -> tuple[float, float]:
    if not values:
        return (0.0, 0.0)
    v = sorted(values)
    p50 = v[len(v) // 2]
    p95 = v[min(len(v) - 1, int(math.ceil(0.95 * len(v))) - 1)]
    return (round(p50, 2), round(p95, 2))


@dataclass
class Episode:
    index: int
    path: Optional[Path]
    started: float
    ended: float = 0.0
    decisions: int = 0
    read_failures: int = 0
    kills: int = 0
    peak_length: int = 0
    final_length: int = 0
    peak_segments: int = 0
    boosts: int = 0
    interventions: int = 0
    holds: int = 0
    commitment_changes: int = 0
    food_eaten: int = 0
    mode_secs: dict[str, float] = field(default_factory=lambda: {"terminate": 0.0, "eat": 0.0})
    relaxed_ticks: int = 0
    chase_timeouts: int = 0
    bursts: list[str] = field(default_factory=list)
    objective_secs: dict[str, float] = field(default_factory=dict)
    reflex_counts: dict[str, int] = field(default_factory=dict)
    planner: dict[str, Any] = field(default_factory=dict)
    planner_decisions: list[dict[str, Any]] = field(default_factory=list)
    log_path: Optional[Path] = None
    died: bool = False
    rank: Optional[list[int]] = None
    best_rank: Optional[int] = None
    centre_dists: list[float] = field(default_factory=list)
    read_ms: list[float] = field(default_factory=list)
    plan_ms: list[float] = field(default_factory=list)
    model_ms: list[float] = field(default_factory=list)
    act_ms: list[float] = field(default_factory=list)
    loop_ms: list[float] = field(default_factory=list)

    def summary(self) -> dict[str, Any]:
        secs = max(0.0, self.ended - self.started)
        out: dict[str, Any] = {"episode": self.index, "peak_length": self.peak_length, "score": self.final_length,
                               "kills": self.kills, "survival_secs": round(secs, 1), "best_rank": self.best_rank,
                               "decisions": self.decisions,
                               "decisions_per_sec": round(self.decisions / secs, 2) if secs > 0 else 0.0,
                               "boost_fraction": round(self.boosts / self.decisions, 3) if self.decisions else 0.0,
                               "interventions": self.interventions, "holds": self.holds,
                               "commitment_changes_per_sec": round(self.commitment_changes / secs, 2) if secs > 0 else 0.0,
                               "food_per_min": round(self.food_eaten / (secs / 60.0), 1) if secs > 0 else 0.0,
                               "mode_secs": {k: round(v, 1) for k, v in self.mode_secs.items()},
                               "relaxed_ticks": self.relaxed_ticks,
                               "state_read_failures": self.read_failures,
                               "rank": self.rank, "chase_timeouts": self.chase_timeouts, "bursts": self.bursts,
                               "objective_secs": {k: round(v, 1) for k, v in self.objective_secs.items()},
                               "reflex_counts": dict(self.reflex_counts),
                               "reflex_per_min": round(sum(self.reflex_counts.values()) / (secs / 60.0), 1) if secs > 0 else 0.0,
                               "planner": dict(self.planner), "narrative": str(self.log_path) if self.log_path else "",
                               "centre_dist_final": round(self.centre_dists[-1]) if self.centre_dists else None,
                               "centre_dist_mean": round(statistics.mean(self.centre_dists)) if self.centre_dists else None,
                               "died": self.died, "recording": str(self.path) if self.path else ""}
        for name, values in (("read", self.read_ms), ("plan", self.plan_ms), ("model", self.model_ms),
                             ("act", self.act_ms), ("loop", self.loop_ms)):
            p50, p95 = percentiles(values)
            out[f"{name}_p50_ms"], out[f"{name}_p95_ms"] = p50, p95
        return out


def verdict_line(episodes: list[dict[str, Any]], wanted: int) -> str:
    """The gate's last line: OK only with >= 2 episodes of >= 100 decisions and no read failures."""
    need = max(2, wanted)
    if len(episodes) < need:
        return f"SLITHER_EVAL_INCOMPLETE: {len(episodes)} of {need} episodes ran"
    short = [e["episode"] for e in episodes if e["decisions"] < 100]
    if short:
        return f"SLITHER_EVAL_INCOMPLETE: episodes {short} logged under 100 decisions"
    failed = [e["episode"] for e in episodes if e["state_read_failures"]]
    if failed:
        return f"SLITHER_EVAL_INCOMPLETE: state reads failed in episodes {failed}"
    return "SLITHER_EVAL_OK"


# ---- the real backends ----------------------------------------------------------------

class Chrome:
    """A separate Chrome with remote debugging on the game page, spoken to over CDP (one
    websocket, kept open; calls serialised by a lock)."""

    def __init__(self, binary: str = CHROME, url: str = GAME_URL) -> None:
        self.binary = binary
        self.url = url
        self.port = self._free_port()
        self.profile = tempfile.mkdtemp(prefix="slither-chrome-")
        self.proc: Optional[subprocess.Popen] = None
        self.ws: Any = None
        self._id = 0
        self._lock = asyncio.Lock()

    @staticmethod
    def _free_port() -> int:
        s = socket.socket()
        s.bind(("127.0.0.1", 0))
        port = s.getsockname()[1]
        s.close()
        return port

    async def start(self) -> None:
        import websockets

        if not os.path.exists(self.binary):
            raise RuntimeError(f"Chrome not found at {self.binary}")
        self.proc = subprocess.Popen(
            [self.binary, f"--remote-debugging-port={self.port}", f"--user-data-dir={self.profile}",
             "--no-first-run", "--no-default-browser-check", "--window-size=1280,900",
             "--window-position=0,0", self.url],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        page = None
        for _ in range(80):
            try:
                tabs = json.load(urllib.request.urlopen(f"http://127.0.0.1:{self.port}/json", timeout=1))
                page = next((t for t in tabs if t.get("type") == "page"), None)
                if page:
                    break
            except (OSError, ValueError):
                pass
            await asyncio.sleep(0.25)
        if page is None:
            raise RuntimeError("Chrome did not expose a DevTools page within 20 s")
        self.ws = await websockets.connect(page["webSocketDebuggerUrl"], max_size=None)

    async def call(self, method: str, params: Optional[dict[str, Any]] = None) -> dict[str, Any]:
        async with self._lock:
            self._id += 1
            rid = self._id
            await self.ws.send(json.dumps({"id": rid, "method": method, "params": params or {}}))
            while True:
                msg = json.loads(await self.ws.recv())
                if msg.get("id") == rid:
                    if "error" in msg:
                        raise RuntimeError(f"CDP {method}: {msg['error']}")
                    return msg.get("result", {})

    async def evaluate(self, expression: str) -> Any:
        result = await self.call("Runtime.evaluate", {"expression": expression, "returnByValue": True})
        r = result.get("result", {})
        if r.get("subtype") == "error":
            raise RuntimeError(r.get("description", "JS error")[:200])
        return r.get("value")

    async def state(self) -> dict[str, Any]:
        raw = await self.evaluate(FEATURE_JS)
        if not isinstance(raw, str):
            raise RuntimeError("state read returned no JSON")
        return json.loads(raw)

    async def rank(self) -> dict[str, Any]:
        return json.loads(await self.evaluate(RANK_JS))

    async def geometry(self) -> dict[str, Any]:
        return json.loads(await self.evaluate(SCREEN_JS))

    async def play(self, wait: float = 20.0) -> bool:
        await self.call("Page.bringToFront")
        t0 = time.monotonic()
        while time.monotonic() - t0 < wait:
            if await self.evaluate("String(window.playing)") == "true":
                return True
            await self.evaluate(PLAY_JS)
            await asyncio.sleep(0.5)
        return False

    async def close(self) -> None:
        if self.ws is not None:
            try:
                await self.ws.close()
            except Exception:  # noqa: BLE001 — closing is best effort
                pass
        if self.proc is not None:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.proc.kill()
        shutil.rmtree(self.profile, ignore_errors=True)


class Mouse:
    """Real HID input through Quartz CGEventPost: a move (or a drag while the button is held)
    to steer, LeftMouseDown held to boost. `release()` is called in every finally."""

    def __init__(self, quartz: Any = None) -> None:
        if quartz is None:
            import Quartz

            quartz = Quartz
        self.q = quartz
        self.down = False
        self.pos = (0.0, 0.0)

    def _post(self, kind: Any, x: float, y: float) -> None:
        ev = self.q.CGEventCreateMouseEvent(None, kind, (float(x), float(y)), self.q.kCGMouseButtonLeft)
        self.q.CGEventPost(self.q.kCGHIDEventTap, ev)

    def steer(self, x: int, y: int) -> None:
        self.pos = (float(x), float(y))
        self._post(self.q.kCGEventLeftMouseDragged if self.down else self.q.kCGEventMouseMoved, x, y)

    def boost(self, on: bool) -> None:
        if on and not self.down:
            self._post(self.q.kCGEventLeftMouseDown, *self.pos)
            self.down = True
        elif not on and self.down:
            self._post(self.q.kCGEventLeftMouseUp, *self.pos)
            self.down = False

    def release(self) -> None:
        try:
            self._post(self.q.kCGEventLeftMouseUp, *self.pos)
        finally:
            self.down = False


class Model:
    """laya-mlx with two questions per predict (move choice + boost noul)."""

    def __init__(self, path: str = DEFAULT_MODEL_PATH) -> None:
        import laya_mlx

        p = os.path.expanduser(path)
        if not os.path.isdir(p):
            raise RuntimeError(f"no checkpoint at {p}")
        t0 = time.perf_counter()
        self.agent = laya_mlx.Agent(p, dtype="float16", device="gpu", batch_size=2, compile=True)
        self.load_ms = (time.perf_counter() - t0) * 1000
        warm = plan({"x": 30000, "y": 30000, "ang": 0.3, "sp": 5.8, "ssp": 5.8, "sct": 20, "sc": 1.0, "w": 29,
                     "grd": 32550, "score": 120, "heads": [[300, 40, 2.0, 5.79, 8, 1.0, 1]], "body": [300, 40, 320, 60],
                     "food": [[120, 10, 5.0], [-200, 80, 3.0]], "bursts": [], "kills": 0})
        t0 = time.perf_counter()
        self.predict(warm)
        self.warm_ms = (time.perf_counter() - t0) * 1000

    def predict(self, p: Plan) -> tuple[dict[str, float], float, float, int]:
        t0 = time.perf_counter()
        out = self.agent.predict(p.state, questions(p))
        ms = (time.perf_counter() - t0) * 1000
        probs = {k: float(v) for k, v in out["answers"]["move"]["probabilities"].items()}
        p_boost = float(out["answers"]["boost"]["noul"])
        for v in (*probs.values(), p_boost):
            if not math.isfinite(v) or not 0.0 <= v <= 1.0:
                raise ValueError("model returned an invalid probability")
        return probs, p_boost, ms, int(out["usage"]["input_tokens"])


# ---- the planner (gpt-6-astra, background), reflexes, the narrative log, reflection --------------
#
# The minecraft-agent split (rmalde/minecraft-agent async-planner.mjs, breath-reflex.mjs,
# hostile-defense.mjs): the PLANNER runs in its own task, never on the tick path, one call as
# soon as the previous returns plus a floor; it sets the objective, the target, a waypoint and
# a boost policy. The CONTROLLER (laya, 20 Hz) keeps choosing the concrete heading and boost
# among code-built candidates whose scores are conditioned on the directive; between planner
# updates it keeps executing the last one. REFLEXES in code override both. Everything is logged
# to a narrative .log beside the JSONL, and a between-episode REFLECTION (gpt-6-astra, high
# effort) reads that log and picks exactly one in-bounds knob or rule change from the schema.

PLANNER_MODEL = "gpt-6-astra"
PLANNER_EFFORT = "low"
PLANNER_MIN_GAP = 2.0          # s between the end of one planner call and the start of the next
PLANNER_TIMEOUT = 20.0
REFLECT_EFFORT = "high"
REFLECT_TIMEOUT = 90.0
OBJECTIVE_BONUS = 400.0        # the planner's target gets this: above food, below a feasible kill or a burst
OBJECTIVES = ("hunt", "eat", "burst", "centre", "escape")
BOOST_POLICIES = ("aggressive", "normal", "conserve")
TARGET_KINDS = ("snake", "cluster", "burst", "point", "none")
WALL_REFLEX = 250.0            # reflex: the edge closer than this along the commanded heading
HEAD_CROSS_SECS = 0.5          # reflex: a bigger head's predicted path crossing ours within this
STUCK_SECS = 3.0               # reflex: heading swung > 180° with net displacement under STUCK_DISPLACEMENT
STUCK_DISPLACEMENT = 150.0
STUCK_RUN_SECS = 1.5
NARRATIVE_STATE_SECS = 2.0
ENV_FILE = "~/.config/eve-bridge/env"

PLAN_SCHEMA: dict[str, Any] = {
    "type": "object", "additionalProperties": False,
    "required": ["objective", "target", "boost_policy", "waypoint", "note"],
    "properties": {
        "objective": {"type": "string", "enum": list(OBJECTIVES)},
        "target": {"type": "object", "additionalProperties": False, "required": ["kind", "id", "dx", "dy"],
                   "properties": {"kind": {"type": "string", "enum": list(TARGET_KINDS)},
                                  "id": {"type": "integer"}, "dx": {"type": "number"}, "dy": {"type": "number"}}},
        "boost_policy": {"type": "string", "enum": list(BOOST_POLICIES)},
        "waypoint": {"anyOf": [{"type": "null"},
                               {"type": "array", "items": {"type": "number"}, "minItems": 2, "maxItems": 2}]},
        "note": {"type": "string"},
    },
}
PLANNER_INSTRUCTIONS = """You are the planner for a slither.io snake played by a fast local controller. Every few seconds you get a
situation summary (world units, dx/dy relative to our head, y grows downward) and you answer with ONE
directive as JSON. The controller then picks the concrete heading and boost twenty times a second among
safe candidates, weighting your objective and target; code reflexes override you near walls, bodies and
bigger heads. Priorities: terminate (cut off a SMALLER snake so its head runs into our body) above eating,
eating above walking, surviving above all. A dead snake leaves a burst of large food: collect it. The map
centre is where the players are. Keep the snake moving toward something; boosting burns length, so
'aggressive' only when the corridor is clear and the gain is real. target.kind: snake (id from the
summary), cluster or burst (dx, dy from the summary), point (dx, dy), or none. waypoint: [dx, dy] to
travel to, or null. note: at most 80 characters, for the log."""

REFLECT_SCHEMA: dict[str, Any] = {
    "type": "object", "additionalProperties": False,
    "required": ["diagnosis", "change", "hypothesis", "metric_to_watch"],
    "properties": {
        "diagnosis": {"type": "string"},
        "change": {"type": "object", "additionalProperties": False, "required": ["kind", "name", "value", "enabled"],
                   "properties": {"kind": {"type": "string", "enum": ["knob", "rule"]}, "name": {"type": "string"},
                                  "value": {"type": "number"}, "enabled": {"type": "boolean"}}},
        "hypothesis": {"type": "string"},
        "metric_to_watch": {"type": "string"},
    },
}
REFLECT_INSTRUCTIONS = """You review one episode of a slither.io bot and pick EXACTLY ONE change for the next episode. You get the
episode's narrative log (planner directives, controller picks, reflex firings, shield interventions, the
death), the change log so far, the episode table so far, the tunable knobs (name, current value, bounds,
meaning) and the rules that can be switched. Diagnose what limited peak size (the headline) — how we died,
kills attempted vs landed, bursts seen vs collected, circling, boost use — and choose one knob value inside
its bounds or one rule toggle. No code, no text outside the schema. Prefer the change with the clearest
causal link to the diagnosis."""


def load_api_key(path: str = ENV_FILE) -> str:
    key = (os.environ.get("OPENAI_API_KEY") or "").strip()
    if key:
        return key
    try:
        for line in Path(path).expanduser().read_text(encoding="utf-8-sig").splitlines():
            line = line.strip()
            if line.startswith("OPENAI_API_KEY="):
                return line.split("=", 1)[1].strip().strip("'\"")
    except OSError:
        pass
    return ""


def make_responder(api_key: str) -> Callable[[dict[str, Any]], Any]:
    """responses.create as a dict-in/dict-out coroutine (the seam the tests fake), like
    computer_agent.make_response_creator."""
    from openai import AsyncOpenAI

    client = AsyncOpenAI(api_key=api_key)

    async def create(request: dict[str, Any]) -> dict[str, Any]:
        r = await client.responses.create(**request)
        return r.model_dump(exclude_none=True)

    return create


def response_text(response: dict[str, Any]) -> str:
    if response.get("status") not in (None, "completed"):
        raise RuntimeError(f"model did not complete (status {response.get('status')!r})")
    for item in response.get("output") or []:
        if item.get("type") != "message":
            continue
        for part in item.get("content") or []:
            if part.get("type") == "output_text" and part.get("text"):
                return str(part["text"])
    raise RuntimeError("model reply has no text")


@dataclass
class Directive:
    objective: str = "eat"
    target_kind: str = "none"
    target_id: int = 0
    target_dx: float = 0.0        # relative to our head at the time of the call
    target_dy: float = 0.0
    boost_policy: str = "normal"
    waypoint: Optional[tuple[float, float]] = None    # relative, at the time of the call
    note: str = ""
    at: float = 0.0
    latency_ms: float = 0.0
    origin: tuple[float, float] = (0.0, 0.0)          # our absolute head position when the call was made

    def target_abs(self) -> Optional[tuple[float, float]]:
        if self.target_kind in ("cluster", "burst", "point"):
            return (self.origin[0] + self.target_dx, self.origin[1] + self.target_dy)
        return None

    def waypoint_abs(self) -> Optional[tuple[float, float]]:
        if self.waypoint is None:
            return None
        return (self.origin[0] + self.waypoint[0], self.origin[1] + self.waypoint[1])


def parse_directive(text: str, origin: tuple[float, float], now: float, latency_ms: float) -> Directive:
    """Validate the planner's JSON against the enums; anything off-schema raises (never trusted)."""
    data = json.loads(text)
    if not isinstance(data, dict):
        raise ValueError("directive is not an object")
    objective = str(data.get("objective"))
    policy = str(data.get("boost_policy"))
    target = data.get("target") or {}
    kind = str(target.get("kind", "none"))
    if objective not in OBJECTIVES or policy not in BOOST_POLICIES or kind not in TARGET_KINDS:
        raise ValueError("directive outside the enums")
    wp = data.get("waypoint")
    waypoint = None
    if isinstance(wp, list) and len(wp) == 2 and all(isinstance(v, (int, float)) and math.isfinite(v) for v in wp):
        waypoint = (float(wp[0]), float(wp[1]))
    dx, dy = float(target.get("dx", 0) or 0), float(target.get("dy", 0) or 0)
    if not (math.isfinite(dx) and math.isfinite(dy)):
        raise ValueError("target is not finite")
    note = re.sub(r"\s+", " ", str(data.get("note", "")))[:80]
    return Directive(objective, kind, int(target.get("id", 0) or 0), dx, dy, policy, waypoint, note, now,
                     latency_ms, origin)


def situation_summary(state: dict[str, Any], p: Plan, directive: Optional[Directive], objective_secs: float,
                      interventions_5s: int, reflexes_5s: list[str], last_lines: list[str],
                      kills: int) -> dict[str, Any]:
    """What the planner sees: a compact, structured picture (pure)."""
    ang = float(state["ang"])
    my_sct = int(state.get("sct", 0))
    smaller, bigger = [], []
    for hx, hy, hang, hsp, hsct, _c, hid in state.get("heads") or []:
        d = math.hypot(hx, hy)
        v = units_per_sec(hsp)
        d1 = math.hypot(hx + v * math.cos(hang), hy + v * math.sin(hang))
        row = {"id": int(hid), "len": int(hsct), "dist": int(d), "rel_deg": int(math.degrees(wrap(math.atan2(hy, hx) - ang))),
               "closing_per_s": int(d - d1), "dx": int(hx), "dy": int(hy)}
        (smaller if hsct < my_sct else bigger).append(row)
    smaller.sort(key=lambda r: r["dist"])
    bigger.sort(key=lambda r: r["dist"])
    food = [{"dx": int(fx), "dy": int(fy), "mass": int(m)} for fx, fy, m in (state.get("food") or [])[:3]]
    bursts = [{"dx": int(bx), "dy": int(by), "mass": int(m), "pieces": int(n)} for bx, by, m, n in state.get("bursts") or []]
    grd = float(state.get("grd", 0))
    return {
        "us": {"length": p.length, "segments": my_sct, "speed": round(float(state.get("sp", 0)), 1),
               "boosting": float(state.get("sp", 0)) > float(state.get("ssp", SP_NORMAL)) + 1.0,
               "heading_deg": int(math.degrees(ang)) % 360, "kills": kills},
        "centre": {"dx": int(grd - float(state["x"])), "dy": int(grd - float(state["y"])), "dist": int(p.centre_dist)},
        "wall_dist": int(min((c.risk for c in p.candidates if c.risk_what == "wall"), default=9999)),
        "smaller_snakes": smaller[:3], "bigger_heads": bigger[:3], "food_clusters": food, "bursts": bursts,
        "shield_interventions_5s": interventions_5s, "reflexes_5s": reflexes_5s[-5:],
        "current": {"objective": directive.objective if directive else "none",
                    "target": directive.target_kind if directive else "none",
                    "boost_policy": directive.boost_policy if directive else "normal",
                    "running_secs": round(objective_secs, 1)},
        "last_log_lines": last_lines[-5:],
    }


class Planner:
    """Background task: one gpt-6-astra call after another (PLANNER_MIN_GAP floor), never on the
    tick path. `current` is the latest valid Directive; the controller reads it every tick."""

    def __init__(self, create: Callable[[dict[str, Any]], Any], *, log: Callable[[str, str], None],
                 model: str = PLANNER_MODEL, effort: str = PLANNER_EFFORT, min_gap: float = PLANNER_MIN_GAP,
                 timeout: float = PLANNER_TIMEOUT, clock: Callable[[], float] = time.monotonic,
                 sleep: Callable[[float], Any] = asyncio.sleep) -> None:
        self.create, self.log, self.model, self.effort = create, log, model, effort
        self.min_gap, self.timeout, self.clock, self.sleep = min_gap, timeout, clock, sleep
        self.current: Optional[Directive] = None
        self.calls = 0
        self.errors = 0
        self.latencies: list[float] = []
        self.decisions: list[dict[str, Any]] = []
        self.closed = False
        self.task: Optional[asyncio.Task] = None

    def request(self, situation: dict[str, Any]) -> dict[str, Any]:
        return {"model": self.model, "instructions": PLANNER_INSTRUCTIONS,
                "input": [{"type": "message", "role": "user",
                           "content": [{"type": "input_text", "text": json.dumps(situation, separators=(",", ":"))}]}],
                "reasoning": {"effort": self.effort},
                "text": {"format": {"type": "json_schema", "name": "directive", "schema": PLAN_SCHEMA, "strict": True}},
                "store": False, "timeout": self.timeout}

    async def step(self, get_situation: Callable[[], Optional[tuple[dict[str, Any], tuple[float, float]]]]) -> None:
        got = get_situation()
        if got is None:
            await self.sleep(0.25)
            return
        situation, origin = got
        t0 = self.clock()
        self.calls += 1
        try:
            response = await asyncio.wait_for(self.create(self.request(situation)), timeout=self.timeout + 5.0)
            latency = (self.clock() - t0) * 1000
            d = parse_directive(response_text(response), origin, self.clock(), latency)
        except asyncio.CancelledError:
            raise
        except Exception as e:  # noqa: BLE001 — a planner failure keeps the last directive
            self.errors += 1
            self.log("planner", f"error {type(e).__name__}: {str(e)[:120]}")
            await self.sleep(self.min_gap)
            return
        self.latencies.append(latency)
        self.current = d
        self.decisions.append({"t": round(d.at, 2), "objective": d.objective, "target": d.target_kind,
                               "id": d.target_id, "boost": d.boost_policy, "waypoint": d.waypoint, "note": d.note,
                               "latency_ms": round(latency)})
        self.log("planner", f"{d.objective} target={d.target_kind}#{d.target_id} boost={d.boost_policy} "
                            f"waypoint={None if d.waypoint is None else [int(v) for v in d.waypoint]} "
                            f"({latency:.0f} ms) — {d.note}")
        gap = self.min_gap - (self.clock() - t0)
        if gap > 0:
            await self.sleep(gap)

    async def run(self, get_situation: Callable[[], Optional[tuple[dict[str, Any], tuple[float, float]]]]) -> None:
        while not self.closed:
            await self.step(get_situation)

    def start(self, get_situation: Callable[[], Any]) -> None:
        self.task = asyncio.ensure_future(self.run(get_situation))

    async def close(self) -> None:
        self.closed = True
        if self.task is not None:
            self.task.cancel()
            try:
                await self.task
            except (asyncio.CancelledError, Exception):  # noqa: BLE001 — closing
                pass
            self.task = None

    def stats(self) -> dict[str, Any]:
        p50, p95 = percentiles(self.latencies)
        return {"planner_calls": self.calls, "planner_errors": self.errors, "planner_p50_ms": p50, "planner_p95_ms": p95}


def condition(cands: list[Candidate], state: dict[str, Any], d: Optional[Directive]) -> None:
    """Add OBJECTIVE_BONUS to the candidates the planner asked for (pure, in place). Kills and bursts
    keep their own priorities above this; the shield is untouched."""
    if d is None or not RULE_STATE["planner_conditioning"]:
        return
    x, y = float(state["x"]), float(state["y"])
    target = d.target_abs()
    for c in cands:
        if not c.safe:
            continue
        if d.objective == "hunt":
            if c.kind in ("cutoff", "trap") and (d.target_kind != "snake" or c.ident == f"enemy:{d.target_id}"):
                c.score += OBJECTIVE_BONUS
        elif d.objective == "burst" and c.kind == "burst":
            c.score += OBJECTIVE_BONUS
        elif d.objective == "eat" and c.kind in ("food", "burst"):
            if target is None or math.hypot(x + math.cos(c.heading) * 100 - target[0], y + math.sin(c.heading) * 100 - target[1]) < 400:
                c.score += OBJECTIVE_BONUS
        elif d.objective == "centre" and c.kind in ("centre", "waypoint"):
            c.score += OBJECTIVE_BONUS
        elif d.objective == "escape" and c.kind == "heading":
            big = [(hx, hy) for hx, hy, _a, _s, hsct, _c, _i in state.get("heads") or [] if hsct >= int(state.get("sct", 0))]
            if big:
                hx, hy = min(big, key=lambda h: math.hypot(*h))
                if math.cos(wrap(c.heading - math.atan2(hy, hx))) < -0.3:
                    c.score += OBJECTIVE_BONUS
        if c.kind == "waypoint" and d.objective != "escape":
            c.score += OBJECTIVE_BONUS * 0.5


def waypoint_candidate(state: dict[str, Any], d: Optional[Directive], length: int) -> Optional[Candidate]:
    if d is None or not RULE_STATE["planner_conditioning"]:
        return None
    wp = d.waypoint_abs()
    if wp is None:
        return None
    dx, dy = wp[0] - float(state["x"]), wp[1] - float(state["y"])
    dist = math.hypot(dx, dy)
    if dist < 80:
        return None
    boost = d.boost_policy == "aggressive" and length > BOOST_MIN_LENGTH
    return Candidate(key=f"waypoint {int(dist)}", kind="waypoint", heading=math.atan2(dy, dx), boost=boost,
                     speed=V_BOOST if boost else V_CRUISE, bonus=min(30.0, dist / 100.0), ident="waypoint",
                     centre_dist=dist)


# ---- reflexes (code, always on, override planner and controller) ----------------------------

@dataclass
class Reflex:
    rule: str
    heading: float
    boost: Optional[bool]        # None: leave the controller's boost alone


def reflex_wall(state: dict[str, Any], heading: float) -> Optional[Reflex]:
    grd = float(state.get("grd", 0))
    if grd <= 0:
        return None
    if wall_distance(float(state["x"]), float(state["y"]), heading, grd) < WALL_REFLEX:
        to_centre = math.atan2(grd - float(state["y"]), grd - float(state["x"]))
        return Reflex("wall", to_centre, False)
    return None


def reflex_head_crossing(state: dict[str, Any], heading: float, length: int) -> Optional[Reflex]:
    """A bigger head whose predicted path comes within collision reach of ours within HEAD_CROSS_SECS."""
    my_sct = int(state.get("sct", 0))
    width = float(state.get("w", 29))
    v_us = units_per_sec(float(state.get("sp", SP_NORMAL)))
    for hx, hy, hang, hsp, hsct, hsc, _i in state.get("heads") or []:
        if hsct < my_sct:
            continue
        v = units_per_sec(hsp)
        for t in (0.15, 0.3, 0.5):
            ex, ey = hx + v * t * math.cos(hang), hy + v * t * math.sin(hang)
            ux, uy = v_us * t * math.cos(heading), v_us * t * math.sin(heading)
            if math.hypot(ex - ux, ey - uy) <= width / 2 + float(hsc) * 29 / 2 + 40:
                away = math.atan2(-hy, -hx)
                return Reflex("head_crossing", away, length > BOOST_MIN_LENGTH)
    return None


def reflex_stuck(history: list[tuple[float, float, float, float]], now: float, target: float) -> Optional[Reflex]:
    """Heading swung more than 180° over STUCK_SECS with net displacement under STUCK_DISPLACEMENT:
    force a straight run toward `target`."""
    recent = [h for h in history if now - h[0] <= STUCK_SECS]
    if len(recent) < 10 or recent[-1][0] - recent[0][0] < STUCK_SECS * 0.8:
        return None
    net = math.hypot(recent[-1][1] - recent[0][1], recent[-1][2] - recent[0][2])
    if net >= STUCK_DISPLACEMENT:
        return None
    angs = [h[3] for h in recent]
    base = angs[0]
    spread = max(abs(wrap(a - base)) for a in angs)
    if spread < math.pi / 2:
        return None
    return Reflex("stuck", target, False)


class NarrativeLog:
    """The human-readable .log beside the JSONL: [planner] / [controller] / [reflex] / [shield] streams
    plus a state line every NARRATIVE_STATE_SECS. Keeps the last lines for the planner's input."""

    def __init__(self, path: Optional[Path], clock: Callable[[], float], started: float) -> None:
        self.path = path
        self.clock = clock
        self.started = started
        self.lines: list[str] = []
        self._f = path.open("a", encoding="utf-8") if path else None
        self._last_state = -1e9
        self._last_controller = ""
        self._last_controller_at = -1e9
        self._last_shield_at = -1e9

    def write(self, tag: str, text: str) -> None:
        t = self.clock() - self.started
        line = f"{t:7.2f} [{tag}] {text}"
        self.lines.append(line)
        if len(self.lines) > 400:
            del self.lines[:200]
        if self._f:
            self._f.write(line + "\n")
            self._f.flush()                      # readable while the episode plays

    def state(self, p: Plan, v: "Verdict", state: dict[str, Any], kills: int, d: Optional[Directive]) -> None:
        now = self.clock()
        if now - self._last_state < NARRATIVE_STATE_SECS:
            return
        self._last_state = now
        obj = d.objective if d else "none"
        self.write("state", f"obj={obj} mode={p.mode} len={p.length} boost={'on' if v.boost else 'off'} "
                            f"target={v.executed.key} ({v.boost_reason}) kills={kills} centre={int(p.centre_dist)} "
                            f"heads={len(state.get('heads') or [])} risk={int(v.executed.risk)}")

    def controller(self, v: "Verdict", prob: float) -> None:
        now = self.clock()
        if v.executed.key == self._last_controller and now - self._last_controller_at < 2.0:
            return
        if now - self._last_controller_at < 0.5:
            return
        self._last_controller, self._last_controller_at = v.executed.key, now
        self.write("controller", f"{v.executed.key} p={prob:.2f} risk={int(v.executed.risk)}"
                                 f"{' held' if v.held else ''}{' boost' if v.boost else ''}")

    def shield(self, v: "Verdict", p: Plan) -> None:
        now = self.clock()
        if not v.intervened or now - self._last_shield_at < 1.0:
            return
        self._last_shield_at = now
        proposed = next((c for c in p.options if c.key == v.proposed), None)
        rule = proposed.risk_what if proposed else "?"
        self.write("shield", f"rerouted {v.proposed} ({rule} {int(proposed.risk) if proposed else 0}) -> {v.executed.key}")

    def death(self, tail: list[dict[str, Any]]) -> None:
        heads = sorted({int(h) for r in tail for h in [r.get("heads", 0)]})
        last = tail[-1] if tail else {}
        menu = "; ".join(f"{k}: {v[:28]}" for k, v in list(last.get("menu", {}).items())[:4])
        self.write("death", f"died. heads seen in the last 1.5 s: {heads}; last executed {last.get('executed')} "
                            f"risk {last.get('risk')}; menu: {menu}")

    def close(self) -> None:
        if self._f:
            self._f.close()
            self._f = None


def knob_schema_text() -> str:
    lines = [f"- {name}: current {globals()[name]:g}, bounds [{lo:g}, {hi:g}] — {doc}" for name, (lo, hi, doc) in KNOBS.items()]
    lines += [f"- rule {name}: {'on' if RULE_STATE[name] else 'off'} — {doc}" for name, doc in RULES.items()]
    return "\n".join(lines)


def apply_change(change: dict[str, Any]) -> tuple[bool, str]:
    kind = change.get("kind")
    if kind == "knob":
        return set_knob(str(change.get("name")), change.get("value"))
    if kind == "rule":
        return set_rule(str(change.get("name")), change.get("enabled"))
    return False, f"unknown change kind {kind!r}"


async def reflect(create: Callable[[dict[str, Any]], Any], narrative: str, changelog: str, table: str,
                  planner_decisions: str, *, model: str = PLANNER_MODEL, effort: str = REFLECT_EFFORT,
                  timeout: float = REFLECT_TIMEOUT) -> dict[str, Any]:
    """One structured reflection call; the parsed answer, or {"error": …}. Applying it is the caller's."""
    body = (f"NARRATIVE LOG (tail):\n{narrative[-48000:]}\n\nPLANNER DECISIONS:\n{planner_decisions[-8000:]}\n\n"
            f"CHANGE LOG:\n{changelog[-12000:]}\n\nEPISODE TABLE:\n{table}\n\nKNOBS AND RULES:\n{knob_schema_text()}")
    req = {"model": model, "instructions": REFLECT_INSTRUCTIONS,
           "input": [{"type": "message", "role": "user", "content": [{"type": "input_text", "text": body}]}],
           "reasoning": {"effort": effort},
           "text": {"format": {"type": "json_schema", "name": "reflection", "schema": REFLECT_SCHEMA, "strict": True}},
           "store": False, "timeout": timeout}
    try:
        response = await asyncio.wait_for(create(req), timeout=timeout + 5.0)
        data = json.loads(response_text(response))
        if not isinstance(data, dict) or not isinstance(data.get("change"), dict):
            raise ValueError("reflection is not an object with a change")
        return data
    except Exception as e:  # noqa: BLE001 — a failed reflection is logged, the series goes on
        return {"error": f"{type(e).__name__}: {str(e)[:160]}"}


# ---- the loop ------------------------------------------------------------------------

async def run_episode(index: int, chrome: Any, model: Any, mouse: Any, *, max_secs: float,
                      runs_dir: Optional[Path], hz: float = TARGET_HZ, dry_run: bool = False,
                      clock: Callable[[], float] = time.monotonic,
                      sleep: Callable[[float], Any] = asyncio.sleep, perf: Callable[[], float] = time.perf_counter,
                      rank_every: float = 5.0, planner: Optional[Planner] = None) -> Episode:
    period = 1.0 / hz
    path = log_path = None
    if runs_dir is not None:
        runs_dir.mkdir(parents=True, exist_ok=True, mode=0o700)
        stamp = f"{datetime.now():%Y-%m-%d-%H%M%S}-ep{index}"
        path, log_path = runs_dir / f"{stamp}.jsonl", runs_dir / f"{stamp}.log"
    ep = Episode(index, path, clock())
    controller = Controller()
    burst_log = BurstLog()
    narrative = NarrativeLog(log_path, clock, ep.started)
    ep.log_path = log_path
    last_tick = ep.started
    cx, cy = canvas_centre(await chrome.geometry())
    f = path.open("a", encoding="utf-8") if path else None
    next_rank = ep.started
    latest: dict[str, Any] = {}                     # what the planner task reads: the latest state and plan
    history: list[tuple[float, float, float, float]] = []   # (t, x, y, ang) for the stuck reflex
    stuck_until = -1.0
    tail: list[dict[str, Any]] = []
    directive_since = ep.started
    last_objective = ""
    recent_interventions: list[float] = []
    recent_reflexes: list[tuple[float, str]] = []

    def get_situation() -> Optional[tuple[dict[str, Any], tuple[float, float]]]:
        if not latest:
            return None
        st, p = latest["state"], latest["plan"]
        now = clock()
        sit = situation_summary(st, p, planner.current if planner else None, now - directive_since,
                                sum(1 for t in recent_interventions if now - t <= 5.0),
                                [r for t, r in recent_reflexes if now - t <= 5.0], narrative.lines, ep.kills)
        return sit, (float(st["x"]), float(st["y"]))

    if planner is not None:
        planner.start(get_situation)
    try:
        while clock() - ep.started < max_secs:
            t_loop = clock()
            t0 = perf()
            try:
                state = await chrome.state()
            except Exception as e:  # noqa: BLE001 — a failed read is counted and skipped
                ep.read_failures += 1
                if f:
                    f.write(json.dumps({"t": round(t_loop - ep.started, 3), "read_error": str(e)[:200]}) + "\n")
                await sleep(period)
                continue
            read_ms = (perf() - t0) * 1000
            ep.kills = int(state.get("kills", ep.kills))
            if not state.get("playing"):
                ep.died = True
                narrative.death(tail)
                break
            d = planner.current if planner else None
            if d is not None and d.objective != last_objective:
                last_objective, directive_since = d.objective, t_loop
            t1 = perf()
            controller.policy = d.boost_policy if d else "normal"
            p = plan(state, relax=controller.relax(t_loop), exclude=controller.excluded(t_loop), directive=d)
            plan_ms = (perf() - t1) * 1000
            latest = {"state": state, "plan": p}
            probs, p_boost, model_ms, tokens = model.predict(p)
            v = controller.decide(p, probs, p_boost, state, t_loop)
            heading, boost = v.heading, v.boost
            # reflexes: wall, a bigger head crossing, no safe body clearance, stuck — code over everything
            fired: list[str] = []
            x_now, y_now = float(state["x"]), float(state["y"])
            history.append((t_loop, x_now, y_now, float(state["ang"])))
            if len(history) > 200:
                del history[:100]
            grd = float(state.get("grd", 0))
            to_centre = math.atan2(grd - y_now, grd - x_now) if grd > 0 else heading
            wp = d.waypoint_abs() if d else None
            run_target = math.atan2(wp[1] - y_now, wp[0] - x_now) if wp else to_centre
            if t_loop < stuck_until:
                heading, boost = run_target, False           # inside a stuck run: counted once, at its start
            elif RULE_STATE["stuck_reflex"]:
                stuck = reflex_stuck(history, t_loop, run_target)
                if stuck is not None:
                    stuck_until = t_loop + STUCK_RUN_SECS
                    heading, boost = stuck.heading, False
                    fired.append("stuck")
            if not v.executed.safe:
                fired.append("body_clearance")
            r = reflex_head_crossing(state, heading, p.length)
            if r is not None:
                heading, boost = r.heading, bool(r.boost)
                fired.append(r.rule)
            r = reflex_wall(state, heading)
            if r is not None:
                heading, boost = r.heading, False
                fired.append(r.rule)
            for rule in fired:
                ep.reflex_counts[rule] = ep.reflex_counts.get(rule, 0) + 1
                recent_reflexes.append((t_loop, rule))
                narrative.write("reflex", f"{rule}: heading {int(math.degrees(heading)) % 360}° boost={boost}")
            if v.intervened:
                recent_interventions.append(t_loop)
            x, y = screen_point(cx, cy, heading)
            t2 = perf()
            if not dry_run:
                mouse.steer(x, y)
                mouse.boost(boost)
            act_ms = (perf() - t2) * 1000
            ep.decisions += 1
            ep.read_ms.append(read_ms)
            ep.plan_ms.append(plan_ms)
            ep.model_ms.append(model_ms)
            ep.act_ms.append(act_ms)
            if ep.decisions > 1 and p.length > ep.final_length:
                ep.food_eaten += p.length - ep.final_length
            ep.final_length = p.length
            ep.peak_length = max(ep.peak_length, p.length)
            ep.peak_segments = max(ep.peak_segments, int(state.get("sct", 0)))
            ep.boosts += int(boost)
            ep.interventions += int(v.intervened)
            ep.holds += int(v.held)
            ep.commitment_changes += int(v.switched)
            obj = d.objective if d else "none"
            if ep.loop_ms:
                dt = max(0.0, t_loop - last_tick)
                ep.mode_secs[p.mode] = ep.mode_secs.get(p.mode, 0.0) + dt
                ep.objective_secs[obj] = ep.objective_secs.get(obj, 0.0) + dt
            last_tick = t_loop
            ep.relaxed_ticks += int(p.relaxed)
            ep.chase_timeouts = controller.timeouts
            burst_line = burst_log.tick(v.executed, state, p.length, t_loop)
            if burst_line:
                ep.bursts.append(burst_line)
                narrative.write("burst", burst_line)
                if f:
                    f.write(json.dumps({"t": round(t_loop - ep.started, 3), "burst": burst_line}) + "\n")
            ep.centre_dists.append(p.centre_dist)
            if clock() >= next_rank:
                next_rank = clock() + rank_every
                try:
                    rk = await chrome.rank()
                    dom = rk.get("dom")
                    if dom:
                        ep.rank = [int(dom[0]), int(dom[1])]
                        ep.best_rank = int(dom[0]) if ep.best_rank is None else min(ep.best_rank, int(dom[0]))
                    elif rk.get("rank"):
                        ep.rank = [int(rk["rank"]), 0]
                except Exception:  # noqa: BLE001 — the leaderboard is decoration
                    pass
            loop_ms = (perf() - t0) * 1000
            ep.loop_ms.append(loop_ms)
            record = {"t": round(t_loop - ep.started, 3), "n": ep.decisions, "kills": ep.kills, "length": p.length,
                      "sct": state.get("sct"), "heads": len(state.get("heads") or []), "state": p.state,
                      "menu": {c.key: c.label for c in p.options},
                      "scores": {c.key: round(c.score, 1) for c in p.options},
                      "probabilities": {k: round(pr, 4) for k, pr in probs.items()}, "p_boost": round(p_boost, 4),
                      "mode": p.mode, "objective": obj, "relaxed": p.relaxed, "proposed": v.proposed,
                      "executed": v.executed.key, "kind": v.executed.kind, "risk": round(v.executed.risk, 1),
                      "intervened": v.intervened, "held": v.held, "switched": v.switched,
                      "heading": round(heading, 3), "boost": boost, "boost_reason": v.boost_reason,
                      "reflex": fired, "centre_dist": round(p.centre_dist), "mouse": [x, y],
                      "read_ms": round(read_ms, 2), "plan_ms": round(plan_ms, 2), "model_ms": round(model_ms, 2),
                      "act_ms": round(act_ms, 3), "loop_ms": round(loop_ms, 2), "input_tokens": tokens}
            tail.append(record)
            tail = [r for r in tail if t_loop - ep.started - r["t"] <= 1.5]
            if f:
                f.write(json.dumps(record) + "\n")
            narrative.controller(v, probs.get(v.executed.key, 0.0))
            narrative.shield(v, p)
            narrative.state(p, v, state, ep.kills, d)
            if dry_run:
                print(f"  n={ep.decisions} kills={ep.kills} len={p.length} {obj}/{p.mode} {v.executed.key:<22} "
                      f"p={probs.get(v.executed.key, 0):.2f} boost={boost}({v.boost_reason}) reflex={fired} "
                      f"read={read_ms:.1f} plan={plan_ms:.1f} model={model_ms:.1f}ms | {p.state}")
            rest = period - (clock() - t_loop)
            if rest > 0:
                await sleep(rest)
    finally:
        ep.ended = clock()
        if planner is not None:
            await planner.close()
            ep.planner = planner.stats()
            ep.planner_decisions = list(planner.decisions)
        narrative.close()
        if f:
            f.close()
        if not dry_run:
            mouse.release()
    return ep


def print_table(episodes: list[dict[str, Any]]) -> None:
    print("\nep  PEAK  score  kills  surv_s  food/min  dec/s  commit/s  best_rank  planner n p50/p95  objectives (s)"
          "                   reflex/min  kill_s   eat_s  dec  boost  shield  centre_final  read p50/p95  plan p50/p95"
          "  model p50/p95  act p50/p95  loop p50/p95  read_fail")
    for e in episodes:
        rank = f"{e['best_rank']}/{e['rank'][1]}" if e.get("rank") and e.get("best_rank") else "-"
        pl = e.get("planner") or {}
        planner = f"{pl.get('planner_calls', 0):>3} {pl.get('planner_p50_ms', 0):>5}/{pl.get('planner_p95_ms', 0):<6}"
        objs = " ".join(f"{k[:4]}={v:.0f}" for k, v in sorted((e.get("objective_secs") or {}).items()))
        print(f"{e['episode']:>2}  {e['peak_length']:>4}  {e['score']:>5}  {e['kills']:>5}  {e['survival_secs']:>6}  "
              f"{e['food_per_min']:>8}  {e['decisions_per_sec']:>5}  {e['commitment_changes_per_sec']:>8}  {rank:<9}  "
              f"{planner:<18}  {objs:<34}  {e.get('reflex_per_min', 0):>10}  {e['mode_secs']['terminate']:>6}  "
              f"{e['mode_secs']['eat']:>6}  {e['decisions']:>4}  "
              f"{e['boost_fraction']:>5}  {e['interventions']:>6}  {str(e['centre_dist_final']):>12}  "
              f"{e['read_p50_ms']:>5}/{e['read_p95_ms']:<6} {e['plan_p50_ms']:>5}/{e['plan_p95_ms']:<6} "
              f"{e['model_p50_ms']:>6}/{e['model_p95_ms']:<6} {e['act_p50_ms']:>5}/{e['act_p95_ms']:<6} "
              f"{e['loop_p50_ms']:>5}/{e['loop_p95_ms']:<6} {e['state_read_failures']:>9}")
    if episodes:
        print(f"overall: PEAK {max(e['peak_length'] for e in episodes)}, kills {sum(e['kills'] for e in episodes)}, "
              f"{sum(e['decisions'] for e in episodes)} decisions, median decisions/s "
              f"{statistics.median(e['decisions_per_sec'] for e in episodes):.1f}")
    for e in episodes:
        if e.get("reflection"):
            r = e["reflection"]
            print(f"ep {e['episode']} reflection: {r.get('diagnosis', '')[:200]} | change: {r.get('applied', '')} | "
                  f"watch: {r.get('metric_to_watch', '')}")


def table_text(episodes: list[dict[str, Any]]) -> str:
    rows = ["ep peak score kills survival_s food/min dec/s commit/s best_rank bursts objectives reflex/min"]
    for e in episodes:
        rows.append(f"{e['episode']} {e['peak_length']} {e['score']} {e['kills']} {e['survival_secs']} {e['food_per_min']} "
                    f"{e['decisions_per_sec']} {e['commitment_changes_per_sec']} {e.get('best_rank')} "
                    f"{len(e.get('bursts') or [])} {e.get('objective_secs')} {e.get('reflex_per_min')}")
    return "\n".join(rows)


async def run_reflection(args: argparse.Namespace, create: Optional[Callable[[dict[str, Any]], Any]],
                         ep: Episode, episodes: list[dict[str, Any]]) -> dict[str, Any]:
    """Between episodes: one GPT reflection, one in-bounds change applied and persisted, logged."""
    if create is None or not args.reflect:
        return {}
    narrative = ep.log_path.read_text() if ep.log_path and ep.log_path.exists() else ""
    changelog_path = Path(args.changelog).expanduser() if args.changelog else None
    changelog = changelog_path.read_text() if changelog_path and changelog_path.exists() else ""
    decisions = "\n".join(json.dumps(d) for d in ep.planner_decisions)
    result = await reflect(create, narrative, changelog, table_text(episodes), decisions)
    entry: dict[str, Any] = {}
    if "error" in result:
        line = f"  reflection failed: {result['error']}"
        entry = {"diagnosis": "", "applied": f"none ({result['error']})", "metric_to_watch": ""}
    else:
        ok, applied = apply_change(result["change"])
        if ok and args.settings:
            save_settings(Path(args.settings).expanduser())
        entry = {"diagnosis": result.get("diagnosis", ""), "hypothesis": result.get("hypothesis", ""),
                 "metric_to_watch": result.get("metric_to_watch", ""),
                 "applied": applied if ok else f"REFUSED ({applied})", "change": result["change"]}
        line = (f"  GPT diagnosis: {entry['diagnosis']}\n  change: {entry['applied']} — hypothesis: {entry['hypothesis']}"
                f"\n  metric to watch: {entry['metric_to_watch']}")
    print(line, flush=True)
    if changelog_path:
        s = episodes[-1]
        with changelog_path.open("a", encoding="utf-8") as cf:
            cf.write(f"\nep {s['episode']} → peak {s['peak_length']}, score {s['score']}, kills {s['kills']}, "
                     f"survival {s['survival_secs']} s, food/min {s['food_per_min']}, bursts {len(s.get('bursts') or [])}, "
                     f"dec/s {s['decisions_per_sec']}, commit/s {s['commitment_changes_per_sec']}, "
                     f"planner {json.dumps(s.get('planner'))}, objectives {json.dumps(s.get('objective_secs'))}, "
                     f"reflex/min {s.get('reflex_per_min')}\n{line}\n")
    return entry


async def main_async(args: argparse.Namespace) -> int:
    print(f"slither eval: loading the model from {args.model}", flush=True)
    settings_path = Path(args.settings).expanduser() if args.settings else None
    loaded = load_settings(settings_path)
    if loaded:
        print(f"  settings from {settings_path}: {json.dumps(loaded.get('knobs', {}))} rules "
              f"{[k for k, on in (loaded.get('rules') or {}).items() if not on]} off", flush=True)
    model = Model(args.model)
    print(f"  model load {model.load_ms:.0f} ms, warm-up {model.warm_ms:.0f} ms", flush=True)
    create = None
    if not args.no_gpt:
        key = load_api_key()
        if key:
            create = make_responder(key)
            print(f"  planner: {PLANNER_MODEL} ({PLANNER_EFFORT} effort, ≥ {PLANNER_MIN_GAP:.0f} s apart); reflection "
                  f"{'on' if args.reflect else 'off'}", flush=True)
        else:
            print("  planner: OFF — no OPENAI_API_KEY in the environment or ~/.config/eve-bridge/env", flush=True)
    chrome = Chrome()
    mouse = Mouse()
    runs_dir = None if args.dry_run else Path(RUNS_DIR).expanduser()
    episodes: list[dict[str, Any]] = []
    try:
        print(f"  launching Chrome on {GAME_URL} (port {chrome.port})", flush=True)
        await chrome.start()
        await asyncio.sleep(4.0)
        wanted = 1 if args.dry_run else args.episodes
        secs = args.secs if args.dry_run else args.max_secs
        for index in range(1, wanted + 1):
            if not await chrome.play():
                print(f"SLITHER_EVAL_INCOMPLETE: episode {index}: the game did not start (playing stayed false)")
                return 1
            if args.dry_run:
                print(f"dry run for {secs:.0f} s: reading state, planning, printing; moving nothing", flush=True)
            else:
                for n in (3, 2, 1):
                    print(f"  episode {index}: taking the mouse in {n}…", flush=True)
                    await asyncio.sleep(1.0)
            planner = Planner(create, log=lambda tag, text: print(f"  [{tag}] {text}", flush=True)) if create else None
            ep = await run_episode(index, chrome, model, mouse, max_secs=secs, runs_dir=runs_dir,
                                   hz=args.hz, dry_run=args.dry_run, planner=planner)
            s = ep.summary()
            episodes.append(s)
            print(json.dumps(s), flush=True)
            if not args.dry_run:
                s["reflection"] = await run_reflection(args, create, ep, episodes)
            await asyncio.sleep(2.0)                      # the death screen
    except KeyboardInterrupt:
        print("interrupted", flush=True)
    finally:
        mouse.release()
        await chrome.close()
    if args.dry_run:
        print("DRY_RUN_DONE")
        return 0
    if args.series:
        series = Path(args.series).expanduser()
        with series.open("a", encoding="utf-8") as sf:
            for e in episodes:
                sf.write(json.dumps({**e, "note": args.note}) + "\n")
        episodes = [json.loads(ln) for ln in series.read_text().splitlines() if ln.strip()]
        for i, e in enumerate(episodes, 1):
            e["episode"] = i
    print_table(episodes)
    print("recordings:", *[e["recording"] for e in episodes])
    line = verdict_line(episodes, max(2, len(episodes)) if args.series else args.episodes)
    print(line)
    return 0 if line == "SLITHER_EVAL_OK" else 1


def main(argv: Optional[list[str]] = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    ap.add_argument("--episodes", type=int, default=3)
    ap.add_argument("--max-secs", type=float, default=300.0, help="wall cap per episode")
    ap.add_argument("--hz", type=float, default=TARGET_HZ, help="decision rate target")
    ap.add_argument("--dry-run", action="store_true", help="read, plan and print; move nothing")
    ap.add_argument("--secs", type=float, default=10.0, help="dry-run duration")
    ap.add_argument("--model", default=DEFAULT_MODEL_PATH)
    ap.add_argument("--series", default=None, help="JSONL of episode summaries accumulated across runs; the table "
                                                   "and the verdict cover the whole series")
    ap.add_argument("--note", default="", help="free text stored with this run's summaries (the change under test)")
    ap.add_argument("--settings", default=None, help="JSON with knob values and rule states, loaded at start and "
                                                     "rewritten when a reflection applies a change")
    ap.add_argument("--changelog", default=None, help="markdown change log the reflection appends to")
    ap.add_argument("--reflect", action="store_true", help="after each episode, one GPT reflection picks one change")
    ap.add_argument("--no-gpt", action="store_true", help="no planner and no reflection (code only)")
    args = ap.parse_args(argv)
    if sys.platform != "darwin":
        print("SLITHER_EVAL_INCOMPLETE: macOS only (laya-mlx, Quartz input on the real screen)")
        return 1
    return asyncio.run(main_async(args))


if __name__ == "__main__":
    sys.exit(main())
