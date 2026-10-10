"""tools/slither_eval.py without Chrome, the model or Quartz: the rollout and clearance
against bodies, predicted heads and the wall; food gain; cut-off, trap, burst, food and
centre candidates; the phase machine and the hard-turn rule; the menu (with the controls
sentence); the boost gate; the controller's commitment, hysteresis, smoothing and
nearest-safe rerouting; the screen-point mapping; summaries, the verdict line and the
loop with injected fakes (mouse released on every exit)."""

from __future__ import annotations

import asyncio
import json
import math
from pathlib import Path

import numpy as np
import pytest
import slither_eval as se  # tools/ is on the pytest path (pyproject.toml)

# ---- fakes ---------------------------------------------------------------------------


def state(**over) -> dict:
    """A head at the map centre, heading right (ang 0), nothing around, length 120."""
    base = {"playing": True, "x": 32550.0, "y": 32550.0, "ang": 0.0, "sp": 5.79, "ssp": 5.79, "sct": 20, "fam": 0.0,
            "sc": 1.0, "w": 29, "grd": 32550.0, "score": 120, "rank": 0, "kills": 0, "npts": 30,
            "heads": [], "body": [], "food": [], "bursts": []}
    base.update(over)
    return base


def head(dx, dy, ang=0.0, sp=5.79, sct=8, sc=1.0, hid=1) -> list:
    return [dx, dy, ang, sp, sct, sc, hid]


def probs_for(p: se.Plan, key: str, p_top: float = 0.6) -> dict[str, float]:
    rest = (1.0 - p_top) / max(1, len(p.options) - 1)
    return {c.key: (p_top if c.key == key else rest) for c in p.options}


def option(p: se.Plan, kind: str) -> se.Candidate:
    return next(c for c in p.options if c.kind == kind)


# ---- geometry and the rollout ----------------------------------------------------------


def test_heading_names_and_deltas_cover_sixteen_headings() -> None:
    names = [se.heading_name(d) for d in se.HEADING_DELTAS]
    assert names[0] == "straight" and names[8] == "reverse" and names[4] == "right 90°" and names[12] == "left 90°"
    assert len(set(names)) == 16 and all(-math.pi < d <= math.pi for d in se.HEADING_DELTAS)


def test_screen_point_uses_clockwise_angles_with_y_down() -> None:
    assert se.screen_point(640, 512, 0.0, 150) == (790, 512)                 # heading right
    assert se.screen_point(640, 512, math.pi / 2, 150) == (640, 662)         # +90° = down on screen
    cx, cy = se.canvas_centre({"sx": 0, "sy": 33, "ow": 1280, "oh": 872, "iw": 1280, "ih": 785})
    assert (cx, cy) == (640.0, 33 + 87 + 392.5)                              # measured live 2026-09-21


def test_rollout_turns_at_the_turn_rate_and_moves_at_the_speed() -> None:
    paths = se.rollout(0.0, np.array([0.0, math.pi / 2]), np.array([se.V_CRUISE, se.V_CRUISE]), se.TURN_RATE)
    assert paths.shape == (2, se.STEPS, 2)
    straight = paths[0, -1]
    assert straight[0] == pytest.approx(se.V_CRUISE * se.HORIZON) and straight[1] == pytest.approx(0.0)
    turn = paths[1]
    assert turn[-1, 1] > 0 and turn[-1, 0] < straight[0]                    # curved down-right, shorter reach
    # a 90° turn completes in ~0.35 s at scale 1 (measured): by step 8 the tangent points down
    dy = turn[8, 1] - turn[7, 1]
    dx = turn[8, 0] - turn[7, 0]
    assert abs(math.atan2(dy, dx) - math.pi / 2) < 0.15


def test_clearance_sees_bodies_predicted_heads_and_the_wall() -> None:
    s = state(body=[120, 0, 130, 5])                                         # a body 120 ahead
    paths = se.rollout(0.0, np.array([0.0, math.pi]), np.array([se.V_CRUISE] * 2), se.TURN_RATE)
    risk, what = se.clearance(paths, s, 29.0)
    assert risk[0] < 0 and what[0] == "snake"                                # straight runs into it within 0.6 s
    assert risk[1] > risk[0] and what[1] == "snake"                          # turning away clears it
    # a bigger head 400 to the right, heading left across our path, is a moving obstacle
    s = state(heads=[head(300, 300, -math.pi / 2, sct=40, sc=1.4)])
    risk, what = se.clearance(paths, s, 29.0)
    assert what[0] == "big head" and risk[0] < 200
    # the wall: 350 from the edge, heading out
    s = state(x=32550.0 + 32550.0 - 350.0)
    risk, what = se.clearance(paths, s, 29.0)
    assert what[0] == "wall" and risk[0] < 0 and risk[1] > 0


def test_food_gain_sweeps_the_free_corridor() -> None:
    paths = se.rollout(0.0, np.array([0.0]), np.array([se.V_CRUISE]), se.TURN_RATE)
    assert se.food_gain(paths, [[60, 100, 5.0], [60, 400, 9.0]], 29.0) == pytest.approx(5.0)   # 100 off the path: free
    assert se.food_gain(paths, [], 29.0).tolist() == [0.0]


# ---- candidates ---------------------------------------------------------------------------


def test_cutoff_candidate_targets_a_smaller_snake_we_can_beat_to_the_point() -> None:
    s = state(heads=[head(400, 300, -math.pi / 2, sct=8, hid=42)])        # small, crossing ahead of us
    [c] = se.cutoff_candidates(s, 120)
    assert c.kind == "cutoff" and c.target_len == 8 and c.ident == "enemy:42" and 0 < c.p_kill <= 1
    assert -math.pi / 2 < c.heading < 0                                      # toward a point ahead of its head
    assert se.cutoff_candidates(state(heads=[head(400, 300, -math.pi / 2, sct=40)]), 120) == []   # bigger: never
    assert se.cutoff_candidates(s, 12) == [] and se.cutoff_candidates(s, 13)   # hunting starts at length 13
    far = state(heads=[head(1500, 0, math.pi, sct=8)])
    assert se.cutoff_candidates(far, 120) == []                             # out of range
    fleeing = state(heads=[head(800, 0, 0.0, sp=14.0, sct=8)])              # boosting away: unreachable
    assert se.cutoff_candidates(fleeing, 120) == []


def test_trap_burst_food_and_centre_candidates() -> None:
    s = state(sct=40, heads=[head(150, 100, 0.0, sct=10, hid=5)])
    [t] = se.trap_candidates(s)
    assert t.kind == "trap" and t.ident == "enemy:5" and not t.boost
    assert se.trap_candidates(state(sct=20, heads=[head(150, 100, 0.0, sct=10)])) == []   # too small to coil
    s = state(bursts=[[600, 100, 80, 3]], heads=[head(500, 100, 0.0, sct=50)])
    assert se.burst_candidates(s, 120) == []                                # a bigger head is closer to it
    [b] = se.burst_candidates(state(bursts=[[600, 100, 80, 3]]), 120)
    assert b.kind == "burst" and b.boost and b.mass == 80 and b.ident == "burst" and b.bonus > se.BURST_PRIORITY
    # a burst outranks ordinary food and the centre, and stays below a feasible kill
    p = se.plan(state(x=32550.0 + 4000.0, score=200, bursts=[[600, 100, 80, 3]], food=[[200, 0, 90.0]]))
    assert p.best.kind == "burst"
    hunt = se.plan(state(score=200, bursts=[[600, 100, 80, 3]], heads=[head(400, 300, -math.pi / 2, sct=8, hid=1)]))
    assert hunt.best.kind == "cutoff"
    foods = se.food_candidates(state(food=[[100, 0, 5.0], [400, 0, 70.0], [-300, 50, 2.0], [50, 50, 1.0]]), 120)
    assert [c.mass for c in foods] == [70.0, 5.0, 1.0] and foods[0].boost and not foods[1].boost
    c = se.centre_candidate(state(x=32550.0 + 4000.0), 120)
    assert c is not None and c.kind == "centre" and c.boost and c.heading == pytest.approx(math.pi)
    assert not se.centre_candidate(state(x=32550.0 + 4000.0), se.CENTRE_BOOST_LENGTH - 1).boost   # too small: walk
    near = se.centre_candidate(state(x=32550.0 + 500.0), 120)
    assert near is not None and not near.boost and near.bonus == 5.0        # a drift, not a boost
    assert c.bonus == 40.0 and c.centre_dist == 4000.0                       # capped: kills outrank the walk
    assert se.centre_candidate(state(), 120) is None


# ---- the plan: phases, hard turns, the menu -----------------------------------------------


def test_terminate_outranks_eat_whenever_a_kill_is_feasible() -> None:
    kill_state = state(score=200, heads=[head(400, 300, -math.pi / 2, sct=8, hid=1)], food=[[200, 0, 90.0]])
    p = se.plan(kill_state)
    assert p.mode == "terminate"
    assert p.best.kind == "cutoff" and option(p, "cutoff").score > option(p, "food").score
    small = se.plan(state(score=20, heads=[head(400, 300, -math.pi / 2, sct=8, hid=1)], food=[[200, 0, 90.0]]))
    assert small.mode == "terminate" and small.best.kind == "cutoff"        # even when small, from length 12 up
    tiny = se.plan(state(score=12, heads=[head(400, 300, -math.pi / 2, sct=8, hid=1)], food=[[200, 0, 90.0]]))
    assert tiny.mode == "eat" and tiny.best.kind == "food"                  # a boost is not affordable yet
    assert se.plan(state(score=200, food=[[200, 0, 30.0]])).mode == "eat"    # nothing to kill: eat
    assert se.plan(state(score=200, heads=[head(400, 300, -math.pi / 2, sct=40)])).mode == "eat"   # bigger: never


def test_cutoff_relaxation_passes_bodies_closer_for_a_short_window() -> None:
    danger = se.DANGER_BASE + 29
    s = state(score=200, heads=[head(400, 300, -math.pi / 2, sct=8, hid=1)])
    base = se.plan(s)
    cut = next(c for c in base.candidates if c.kind == "cutoff")
    # a body point beside the intercept path so its clearance (distance − width) lands between DANGER and 0.7 × DANGER
    d = 0.85 * danger + 29
    px = 120 * math.cos(cut.heading) + d * math.cos(cut.heading + math.pi / 2)
    py = 120 * math.sin(cut.heading) + d * math.sin(cut.heading + math.pi / 2)
    blocked = state(score=200, heads=[head(400, 300, -math.pi / 2, sct=8, hid=1)], body=[int(px), int(py)])
    strict = next(c for c in se.plan(blocked).candidates if c.kind == "cutoff")
    relaxed = next(c for c in se.plan(blocked, relax=True).candidates if c.kind == "cutoff")
    assert strict.risk_what == "snake" and not strict.safe and relaxed.safe and se.plan(blocked, relax=True).relaxed
    # the relaxation never applies to a head in the way
    at_head = state(score=200, heads=[head(400, 300, -math.pi / 2, sct=8, hid=1), head(int(px), int(py), 0.0, sct=50, hid=2)])
    assert not next(c for c in se.plan(at_head, relax=True).candidates if c.kind == "cutoff").safe
    ctl = se.Controller()
    p = se.plan(s)
    ctl.decide(p, probs_for(p, cut.key), 0.0, s, 10.0)
    assert ctl.relax(10.5) and not ctl.relax(10.7)                          # 0.6 s window from the cut-off's start
    ctl.decide(p, probs_for(p, "straight"), 0.0, s, 12.0)
    assert not ctl.relax(12.0)


def test_chase_that_cannot_kill_in_three_seconds_gives_way_and_cools_down() -> None:
    s = state(score=200, heads=[head(400, 300, -math.pi / 2, sct=8, hid=1)], food=[[200, 0, 90.0]])
    ctl = se.Controller()
    p = se.plan(s)
    cut = option(p, "cutoff")
    for now in (0.0, 1.0, 2.0, 3.0):
        v = ctl.decide(p, probs_for(p, cut.key), 0.0, s, now)
        assert v.executed.kind == "cutoff" and ctl.excluded(now) == set()
    ctl.decide(p, probs_for(p, cut.key), 0.0, s, 3.2)                        # past CHASE_SECS without a kill
    assert ctl.excluded(3.2) == {"enemy:1"} and ctl.timeouts == 1
    cooled = se.plan(s, exclude=ctl.excluded(3.2))
    assert cooled.mode == "eat" and not any(c.kind == "cutoff" for c in cooled.candidates)
    assert ctl.excluded(3.2 + se.CHASE_COOLDOWN + 0.1) == set()             # and the target comes back
    # intercepts later than CHASE_SECS are not offered at all
    slow = state(score=200, heads=[head(850, 0, math.pi / 2, sp=5.79, sct=8, hid=2)])
    assert all(c.p_kill <= 1 for c in se.cutoff_candidates(slow, 200))


def test_a_closing_bigger_head_outranks_food_and_is_fled() -> None:
    s = state(food=[[300, 0, 90.0]], heads=[head(-300, 0, 0.0, sct=60, sc=1.6)])   # big head 300 behind, closing
    p = se.plan(s)
    assert se.closing_big_head(s) is not None
    assert abs(se.wrap(p.best.heading)) < math.pi / 2                       # away from it
    behind = state(food=[[-300, 0, 90.0]], heads=[head(-300, 0, 0.0, sct=60, sc=1.6)])   # the food sits under its head
    assert se.plan(behind).best.kind != "food"                                 # survive: not into a closing head
    assert not next(c for c in se.plan(behind).candidates if c.kind == "food").safe
    calm = state(food=[[300, 0, 90.0]], heads=[head(-300, 0, math.pi, sct=60, sc=1.6)])
    assert se.closing_big_head(calm) is None and se.plan(calm).best.kind == "food"


def test_plan_offers_no_hard_turns_unless_danger_is_ahead() -> None:
    p = se.plan(state())
    assert not p.danger_ahead
    assert all(abs(c.delta) <= se.HARD_TURN for c in p.options if c.kind == "heading")
    assert not any(c.key.startswith("reverse") for c in p.options)
    blocked = se.plan(state(body=[200, 0, 210, 10, 200, -40, 200, 40]))
    assert blocked.danger_ahead
    assert any(abs(c.delta) > se.HARD_TURN for c in blocked.options)
    assert sum(c.safe for c in blocked.options) >= se.MIN_SAFE_OPTIONS


def test_plan_state_text_names_the_controls_and_the_situation() -> None:
    p = se.plan(state(heads=[head(-300, 0, 0.0, sct=50, sc=1.5), head(200, 200, 0.0, sct=5, hid=2)],
                      x=32550.0 - 2000.0, sp=12.0, kills=2))
    assert p.state.startswith("Slither.io. The snake steers toward the mouse. Holding the mouse button boosts "
                              "speed but burns length.")
    assert "Length 120." in p.state and "Boosting: yes." in p.state and "Kills 2." in p.state
    assert "Nearest big head 300 behind." in p.state and "Smaller snake (len 5) 282 right." in p.state
    assert "Centre 2000 away." in p.state and "Wall: far." in p.state
    assert 1 <= len(p.options) <= se.MAX_OPTIONS and all(c.label for c in p.options)
    q = se.questions(p)
    assert q["move"]["type"] == "choice" and list(q["move"]["criteria"]) == [c.key for c in p.options]
    assert q["boost"]["type"] == "noul" and "hold" in q["boost"]["instructions"].lower()
    assert any(lbl.endswith("Best.") for lbl in q["move"]["criteria"].values())


def test_describe_fragments() -> None:
    c = se.Candidate("x", "cutoff", 0.0, True, se.V_BOOST, target_len=12, risk=600, safe=True)
    assert se.describe(c, False) == "Cut off smaller snake (len 12) — boost"
    c = se.Candidate("x", "trap", 0.0, False, se.V_CRUISE, target_len=9, risk=600, safe=True)
    assert se.describe(c, True) == "Trap: circle the small snake (len 9) Best."
    c = se.Candidate("x", "heading", 0.0, False, se.V_CRUISE, risk=80, risk_what="big head", safe=False)
    assert se.describe(c, False) == "Danger: big head 80. Collision."
    c = se.Candidate("x", "centre", 0.0, True, se.V_BOOST, bonus=35.0, centre_dist=3500.0, risk=900, safe=True)
    assert se.describe(c, False) == "Head to the map centre (dist 3500) — boost"


# ---- the boost gate ----------------------------------------------------------------------


def test_boost_gate_every_condition_with_positive_controls() -> None:
    clear = se.Candidate("x", "heading", 0.0, True, se.V_BOOST, risk=900, safe=True)
    assert se.boost_allowed(clear, state(), 10) == (False, "length 10 <= 15")
    assert se.boost_allowed(clear, state(), 120) == (False, "nothing to gain")
    assert se.boost_allowed(clear, state(food=[[300, 20, 80.0]]), 120) == (True, "food cluster")
    assert se.boost_allowed(clear, state(food=[[300, 20, 30.0]]), 120) == (False, "nothing to gain")
    food = se.Candidate("x", "food", 0.0, True, se.V_BOOST, risk=900, safe=True)
    assert se.boost_allowed(food, state(food=[[300, 20, 30.0]]), 120) == (True, "food cluster")   # toward a target
    ahead = state(heads=[head(300, 50, math.pi, sct=60, sc=1.6)])                # a bigger head 300 ahead
    assert se.boost_allowed(food, ahead, 120)[1] in ("big head 304 ahead", "toward a head")
    beside = state(heads=[head(200, 200, math.pi, sct=60, sc=1.6)])              # a bigger head 45° off, 283 away
    assert se.boost_allowed(food, beside, 120) == (False, "big head 282 ahead")
    tight = se.Candidate("x", "heading", 0.0, True, se.V_BOOST, risk=300, safe=True)
    assert se.boost_allowed(tight, state(food=[[300, 20, 80.0]]), 120)[1].startswith("path not clear")
    at_head = state(food=[[300, 20, 80.0]], heads=[head(300, 0, math.pi, sct=5)])
    assert se.boost_allowed(clear, at_head, 120) == (False, "toward a head")
    cut = se.Candidate("x", "cutoff", 1.0, True, se.V_BOOST, risk=900, safe=True)
    assert se.boost_allowed(cut, state(), 120) == (True, "cut-off")
    chased = state(heads=[head(-250, 0, 0.0, sct=60, sc=1.6)])                # a bigger head 250 behind, closing
    assert se.boost_allowed(clear, chased, 120) == (True, "escape")
    idle = state(heads=[head(-250, 0, math.pi, sct=60, sc=1.6)])              # the same head going away
    assert se.boost_allowed(clear, idle, 120) == (False, "nothing to gain")
    toward = se.Candidate("x", "heading", math.pi, True, se.V_BOOST, risk=900, safe=True)
    assert se.boost_allowed(toward, chased, 120)[1] != "escape"               # never an escape INTO it
    centre = se.Candidate("x", "centre", 0.0, True, se.V_BOOST, risk=900, safe=True)
    assert se.boost_allowed(centre, state(), 120) == (True, "to the centre")
    assert se.boost_allowed(centre, state(), 16)[1] in ("to the centre", "walking to the centre")   # the knob decides


def test_boost_decision_lets_the_model_veto_food_but_not_kills() -> None:
    p = se.plan(state(score=200, heads=[head(400, 300, -math.pi / 2, sct=8, hid=1)]))
    cut = option(p, "cutoff")
    assert se.boost_decision(cut, p, 0.1, p and state(score=200, heads=[head(400, 300, -math.pi / 2, sct=8, hid=1)]))[0] \
        == cut.boost or cut.risk < se.BOOST_CLEAR
    p2 = se.plan(state(food=[[400, 20, 80.0]]))
    food = option(p2, "food")
    assert se.boost_decision(food, p2, 0.9, state(food=[[400, 20, 80.0]])) == (True, "food cluster")
    assert se.boost_decision(food, p2, 0.2, state(food=[[400, 20, 80.0]])) == (False, "model says no")


# ---- the controller -------------------------------------------------------------------------


def test_controller_commits_for_a_second_then_switches_only_for_a_better_score() -> None:
    s = state(food=[[400, 0, 40.0], [-100, 350, 42.0]])
    p = se.plan(s)
    foods = [c for c in p.options if c.kind == "food"]
    assert len(foods) == 2
    a, b = foods
    ctl = se.Controller()
    v = ctl.decide(p, probs_for(p, a.key), 0.0, s, 0.0)
    assert v.executed is a and v.switched and not v.held and ctl.changes == 1
    v = ctl.decide(p, probs_for(p, b.key), 0.0, s, 0.5)                    # the model flips: held
    assert v.executed is a and v.held and not v.switched and ctl.changes == 1
    v = ctl.decide(p, probs_for(p, b.key), 0.0, s, 1.5)                    # after a second, but b is not 15% better
    assert v.executed is a and v.held
    b.score = a.score * 2.0
    v = ctl.decide(p, probs_for(p, b.key), 0.0, s, 1.6)
    assert v.executed is b and v.switched and ctl.changes == 2


def test_controller_smooths_the_heading_and_reroutes_to_the_nearest_safe_heading() -> None:
    s = state()
    p = se.plan(s)
    ctl = se.Controller()
    right = next(c for c in p.options if c.key == "right 67°")
    ctl.decide(p, probs_for(p, "straight"), 0.0, s, 0.0)
    right.score = p.best.score * 2.0                                       # clearly better: the hold releases
    v = ctl.decide(p, probs_for(p, right.key), 0.0, s, 2.0)
    assert v.executed is right
    assert v.heading == pytest.approx(se.MAX_TURN_PER_TICK)                # 25° per tick toward 67°
    # an unsafe pick reroutes to the nearest safe heading, not the globally best one
    blocked = se.plan(state(body=[200, 0, 210, 10, 200, -40, 200, 40]))
    unsafe = next(c for c in blocked.candidates if not c.safe and c.kind == "heading")
    blocked.options.append(unsafe)                                         # offered (danger is ahead) and picked
    v = se.Controller().decide(blocked, probs_for(blocked, unsafe.key), 0.0, s, 0.0)
    assert v.intervened and v.executed.safe and v.executed.kind == "heading"
    nearest = min((c for c in blocked.candidates if c.safe and c.kind == "heading" and not c.boost),
                  key=lambda c: abs(se.wrap(c.heading - unsafe.heading)))
    assert v.executed is nearest and v.heading == pytest.approx(nearest.heading)   # no smoothing under danger


# ---- summaries, verdict line, the loop with fakes ------------------------------------------


def test_episode_summary_arithmetic() -> None:
    ep = se.Episode(1, Path("/tmp/x.jsonl"), started=10.0, ended=70.0, decisions=1200, kills=3, peak_length=340,
                    final_length=300, boosts=300, interventions=7, holds=100, commitment_changes=30, food_eaten=250,
                    mode_secs={"terminate": 20.0, "eat": 40.0}, model_ms=[10.0] * 95 + [50.0] * 5,
                    loop_ms=[12.0] * 100, centre_dists=[3000.0, 1000.0], rank=[5, 200], best_rank=4, died=True)
    s = ep.summary()
    assert list(s)[:4] == ["episode", "peak_length", "score", "kills"] and s["kills"] == 3 and s["score"] == 300
    assert s["decisions_per_sec"] == 20.0 and s["boost_fraction"] == 0.25 and s["food_per_min"] == 250.0
    assert s["commitment_changes_per_sec"] == 0.5 and s["mode_secs"] == {"terminate": 20.0, "eat": 40.0}
    assert (s["model_p50_ms"], s["model_p95_ms"]) == (10.0, 10.0) and s["loop_p95_ms"] == 12.0
    assert s["centre_dist_final"] == 1000 and s["centre_dist_mean"] == 2000 and s["rank"] == [5, 200]
    assert se.percentiles([]) == (0.0, 0.0) and se.percentiles([3.0, 1.0, 2.0]) == (2.0, 3.0)


def test_burst_log_reports_pieces_mass_and_collected() -> None:
    log = se.BurstLog()
    burst = se.Candidate("dead snake food 80", "burst", 0.0, True, se.V_BOOST)
    cruise = se.Candidate("straight", "heading", 0.0, False, se.V_CRUISE)
    s = state(bursts=[[300, 0, 80, 4]])
    assert log.tick(burst, s, 100, 0.0) is None
    assert log.tick(burst, state(bursts=[[200, 0, 45, 2]]), 130, 0.5) is None         # half eaten
    line = log.tick(cruise, state(), 145, 1.0)                                          # gone: closed out
    assert line == "burst: 4 pieces, 80 mass, collected 45" and log.active is None
    assert log.tick(burst, s, 100, 5.0) is None
    assert log.tick(cruise, s, 100, 5.5) is None                                        # still there, briefly untargeted
    assert log.tick(cruise, s, 110, 7.0) == "burst: 4 pieces, 80 mass, collected 10"    # idle too long


def test_verdict_line_requires_two_full_episodes_without_read_failures() -> None:
    good = {"episode": 1, "decisions": 150, "state_read_failures": 0}
    assert se.verdict_line([good, {**good, "episode": 2}], 2) == "SLITHER_EVAL_OK"
    assert se.verdict_line([good, {**good, "episode": 2}], 3).startswith("SLITHER_EVAL_INCOMPLETE: 2 of 3")
    assert se.verdict_line([good, {**good, "episode": 2, "decisions": 99}], 2) == \
        "SLITHER_EVAL_INCOMPLETE: episodes [2] logged under 100 decisions"
    assert se.verdict_line([good, {**good, "episode": 2, "state_read_failures": 1}], 2) == \
        "SLITHER_EVAL_INCOMPLETE: state reads failed in episodes [2]"


class FakeChrome:
    def __init__(self, states: list) -> None:
        self.states = list(states)

    async def geometry(self) -> dict:
        return {"sx": 0, "sy": 33, "ow": 1280, "oh": 872, "iw": 1280, "ih": 785}

    async def state(self) -> dict:
        item = self.states.pop(0) if self.states else {"playing": False, "kills": 1}
        if isinstance(item, Exception):
            raise item
        return item

    async def rank(self) -> dict:
        return {"rank": 7, "dom": [7, 150]}


class FakeModel:
    def __init__(self, kind: str, p_boost: float) -> None:
        self.kind, self.p_boost = kind, p_boost
        self.plans: list = []

    def predict(self, p: se.Plan):
        self.plans.append(p)
        key = next((c.key for c in p.options if c.kind == self.kind), p.options[0].key)
        return probs_for(p, key), self.p_boost, 9.0, 300


class FakeQuartz:
    kCGEventMouseMoved, kCGEventLeftMouseDragged, kCGEventLeftMouseDown, kCGEventLeftMouseUp = "move", "drag", "down", "up"
    kCGMouseButtonLeft, kCGHIDEventTap = 0, "hid"

    def __init__(self) -> None:
        self.events: list[tuple] = []

    def CGEventCreateMouseEvent(self, src, kind, point, button):  # noqa: N802 — Quartz's name
        return (kind, point)

    def CGEventPost(self, tap, ev):  # noqa: N802
        self.events.append(ev)


def test_loop_steers_boosts_records_and_releases_the_button(tmp_path: Path) -> None:
    chase = state(food=[[400, 20, 80.0]])
    fed = state(food=[[400, 20, 80.0]], score=130)
    chrome = FakeChrome([chase, RuntimeError("socket hiccup"), fed, {"playing": False, "kills": 1}])
    model = FakeModel("food", 0.9)
    q = FakeQuartz()
    mouse = se.Mouse(q)
    t = [0.0]

    def clock() -> float:
        t[0] += 0.02
        return t[0]

    async def sleep(_s: float) -> None:
        pass

    ep = asyncio.run(se.run_episode(1, chrome, model, mouse, max_secs=60, runs_dir=tmp_path, clock=clock,
                                    sleep=sleep))
    assert ep.decisions == 2 and ep.read_failures == 1 and ep.died and ep.kills == 1
    assert ep.boosts == 2 and ep.food_eaten == 10 and ep.peak_length == 130 and ep.rank == [7, 150]
    kinds = [e[0] for e in q.events]
    assert kinds == ["move", "down", "drag", "up"] and mouse.down is False   # held across ticks, released at exit
    assert q.events[0][1][0] > 640 and abs(q.events[0][1][1] - 512) < 20    # toward the food, from the centre
    lines = ep.path.read_text().splitlines()
    assert len(lines) == 3 and '"read_error"' in lines[1]
    assert '"kind": "food"' in lines[0] and '"boost": true' in lines[0] and "steers toward the mouse" in lines[0]
    assert ep.mode_secs["eat"] > 0 and ep.commitment_changes == 1


def test_dry_run_moves_nothing_and_a_crash_still_releases(tmp_path: Path) -> None:
    q = FakeQuartz()
    ep = asyncio.run(se.run_episode(1, FakeChrome([state(), {"playing": False}]), FakeModel("heading", 0.9),
                                    se.Mouse(q), max_secs=60, runs_dir=None, dry_run=True))
    assert ep.decisions == 1 and q.events == [] and ep.path is None

    class Boom:
        def predict(self, p):
            raise RuntimeError("metal lost")
    q = FakeQuartz()
    with pytest.raises(RuntimeError, match="metal lost"):
        asyncio.run(se.run_episode(1, FakeChrome([state()]), Boom(), se.Mouse(q), max_secs=60, runs_dir=None))
    assert [e[0] for e in q.events] == ["up"]                                # the finally released the button


# ---- planner, reflexes, narrative log, reflection (the minecraft-agent split) ----------------


def test_situation_summary_is_compact_and_relative() -> None:
    s = state(score=200, kills=2, heads=[head(400, 300, -math.pi / 2, sct=8, hid=7), head(-300, 0, 0.0, sct=60, sc=1.6, hid=9)],
              food=[[200, 0, 30.0]], bursts=[[600, 100, 80, 3]])
    p = se.plan(s)
    sit = se.situation_summary(s, p, None, 0.0, 1, ["wall"], ["a", "b"], 2)
    assert sit["us"]["length"] == 200 and sit["us"]["kills"] == 2 and sit["centre"]["dist"] == 0
    [small] = sit["smaller_snakes"]
    assert small["id"] == 7 and small["len"] == 8 and small["dist"] == 500 and 0 < small["rel_deg"] < 90
    [big] = sit["bigger_heads"]
    assert big["id"] == 9 and big["closing_per_s"] > 100                    # heading at us at cruise speed
    assert sit["food_clusters"] == [{"dx": 200, "dy": 0, "mass": 30}] and sit["bursts"][0]["mass"] == 80
    assert sit["current"]["objective"] == "none" and sit["last_log_lines"] == ["a", "b"]
    assert len(json.dumps(sit)) < 2000


def test_parse_directive_validates_enums_and_geometry() -> None:
    text = json.dumps({"objective": "hunt", "target": {"kind": "snake", "id": 7, "dx": 0, "dy": 0},
                       "boost_policy": "aggressive", "waypoint": [100, -50], "note": "x" * 200})
    d = se.parse_directive(text, (1000.0, 2000.0), 5.0, 800.0)
    assert d.objective == "hunt" and d.target_id == 7 and d.boost_policy == "aggressive" and len(d.note) == 80
    assert d.waypoint_abs() == (1100.0, 1950.0) and d.target_abs() is None and d.latency_ms == 800.0
    with pytest.raises(ValueError):
        se.parse_directive(json.dumps({"objective": "attack", "target": {"kind": "none", "id": 0, "dx": 0, "dy": 0},
                                       "boost_policy": "normal", "waypoint": None, "note": ""}), (0, 0), 0, 0)
    with pytest.raises(ValueError):
        se.parse_directive(json.dumps({"objective": "eat", "target": {"kind": "point", "id": 0, "dx": float("inf"), "dy": 0},
                                       "boost_policy": "normal", "waypoint": None, "note": ""}), (0, 0), 0, 0)
    assert se.PLAN_SCHEMA["additionalProperties"] is False and set(se.PLAN_SCHEMA["required"]) == set(se.PLAN_SCHEMA["properties"])


def test_condition_weights_the_planners_target_below_kills_and_bursts() -> None:
    s = state(score=200, heads=[head(400, 300, -math.pi / 2, sct=8, hid=7), head(500, -300, math.pi, sct=6, hid=8)],
              food=[[200, 0, 30.0], [-300, 200, 25.0]])
    d = se.Directive(objective="hunt", target_kind="snake", target_id=8, origin=(32550.0, 32550.0))
    p = se.plan(s, directive=d)
    cuts = {c.ident: c.score for c in p.candidates if c.kind == "cutoff"}
    assert cuts["enemy:8"] > cuts["enemy:7"] and p.best.ident == "enemy:8"
    eat = se.Directive(objective="eat", target_kind="cluster", target_dx=-300, target_dy=200, origin=(32550.0, 32550.0))
    p2 = se.plan(state(score=200, food=[[200, 0, 30.0], [-300, 200, 25.0]]), directive=eat)
    foods = sorted((c for c in p2.candidates if c.kind == "food"), key=lambda c: c.score, reverse=True)
    assert foods[0].mass == 25.0                                            # the planner's cluster, not the bigger one
    burst = se.plan(state(score=200, bursts=[[600, 100, 80, 3]], food=[[200, 0, 30.0]]), directive=eat)
    assert burst.best.kind == "burst"                                       # a burst still outranks the planner's food
    wp = se.Directive(objective="centre", waypoint=(1000.0, 0.0), origin=(32550.0, 32550.0), boost_policy="aggressive")
    p3 = se.plan(state(score=200), directive=wp)
    assert p3.best.kind == "waypoint" and p3.best.boost
    se.RULE_STATE["planner_conditioning"] = False
    try:
        assert se.plan(state(score=200), directive=wp).best.kind != "waypoint"
    finally:
        se.RULE_STATE["planner_conditioning"] = True


def test_boost_policy_widens_or_narrows_the_gate() -> None:
    clear = se.Candidate("x", "heading", 0.0, True, se.V_BOOST, risk=350, safe=True)
    assert se.boost_allowed(clear, state(), 120, "normal")[1].startswith("path not clear")
    assert se.boost_allowed(clear, state(), 120, "aggressive") == (True, "aggressive")
    food = se.Candidate("x", "food", 0.0, True, se.V_BOOST, risk=900, safe=True)
    assert se.boost_allowed(food, state(), 120, "conserve") == (False, "conserving")
    cut = se.Candidate("x", "cutoff", 0.0, True, se.V_BOOST, risk=900, safe=True)
    assert se.boost_allowed(cut, state(), 120, "conserve") == (True, "cut-off")


def test_reflexes_wall_head_crossing_and_stuck() -> None:
    edge = state(x=32550.0 + 32550.0 - 200.0)
    r = se.reflex_wall(edge, 0.0)
    assert r is not None and r.rule == "wall" and abs(se.wrap(r.heading - math.pi)) < 0.01 and r.boost is False
    assert se.reflex_wall(edge, math.pi) is None
    crossing = state(heads=[head(60, 60, -math.pi / 2, sct=60, sc=1.6)])    # a big head 85 away, cutting across
    r = se.reflex_head_crossing(crossing, 0.0, 120)
    assert r is not None and r.rule == "head_crossing" and r.boost is True
    assert abs(se.wrap(r.heading - math.atan2(-60, -60))) < 0.01            # straight away from it
    assert se.reflex_head_crossing(state(heads=[head(60, 60, -math.pi / 2, sct=5)]), 0.0, 120) is None   # smaller: no
    assert se.reflex_head_crossing(state(heads=[head(600, 0, math.pi, sct=60)]), 0.0, 120) is None       # too far
    circling = [(t * 0.1, 30 * math.cos(t * 0.5), 30 * math.sin(t * 0.5), (t * 0.5) % (2 * math.pi)) for t in range(40)]
    r = se.reflex_stuck(circling, 3.9, 1.0)
    assert r is not None and r.rule == "stuck" and r.heading == 1.0
    straight = [(t * 0.1, 18 * t, 0.0, 0.0) for t in range(40)]
    assert se.reflex_stuck(straight, 3.9, 1.0) is None


def test_planner_runs_in_the_background_with_a_floor_and_keeps_the_last_directive_on_errors() -> None:
    t = [0.0]

    def clock() -> float:
        return t[0]

    async def sleep(s: float) -> None:
        t[0] += s

    answers = [json.dumps({"objective": "hunt", "target": {"kind": "snake", "id": 3, "dx": 0, "dy": 0},
                           "boost_policy": "normal", "waypoint": None, "note": "cut the small one"}),
               "not json"]
    requests: list[dict] = []

    async def create(req: dict) -> dict:
        requests.append(req)
        t[0] += 0.5
        return {"status": "completed", "output": [{"type": "message", "content": [{"type": "output_text", "text": answers.pop(0)}]}]}

    lines: list[str] = []
    pl = se.Planner(create, log=lambda tag, text: lines.append(f"[{tag}] {text}"), clock=clock, sleep=sleep, min_gap=2.0)
    sit = ({"us": {"length": 50}}, (100.0, 200.0))
    asyncio.run(pl.step(lambda: sit))
    assert pl.current is not None and pl.current.objective == "hunt" and pl.current.target_id == 3
    assert pl.current.origin == (100.0, 200.0) and pl.calls == 1 and t[0] == pytest.approx(2.0)   # the 2 s floor
    req = requests[0]
    assert req["model"] == "gpt-6-astra" and req["reasoning"] == {"effort": "low"} and req["store"] is False
    assert req["text"]["format"]["schema"] == se.PLAN_SCHEMA and "slither" in req["instructions"].lower()
    asyncio.run(pl.step(lambda: sit))
    assert pl.errors == 1 and pl.current.objective == "hunt" and pl.calls == 2   # bad JSON: last directive kept
    assert lines[0].startswith("[planner] hunt target=snake#3") and "error" in lines[1]
    assert pl.stats()["planner_calls"] == 2 and pl.stats()["planner_p50_ms"] == 500.0
    asyncio.run(pl.step(lambda: None))                                         # nothing to plan yet: no call
    assert pl.calls == 2


def test_narrative_log_streams(tmp_path: Path) -> None:
    t = [0.0]
    log = se.NarrativeLog(tmp_path / "ep.log", lambda: t[0], 0.0)
    s = state(score=50)
    p = se.plan(s)
    v = se.Controller().decide(p, probs_for(p, p.best.key), 0.0, s, 0.0)
    log.write("planner", "eat target=none")
    log.controller(v, 0.6)
    log.state(p, v, s, 0, None)
    t[0] = 0.3
    log.controller(v, 0.6)                                                     # same pick, too soon: no line
    log.death([{"heads": 2, "executed": "straight", "risk": -5, "menu": {"straight": "Danger: big head 0. Collision."}}])
    log.close()
    text = (tmp_path / "ep.log").read_text().splitlines()
    assert text[0].endswith("[planner] eat target=none")
    assert "[controller] " in text[1] and "[state] obj=none mode=eat len=50" in text[2]
    assert text[3].startswith("   0.30 [death] died. heads seen in the last 1.5 s: [2]")
    assert len(text) == 4 and log.lines == text


def test_reflection_applies_one_in_bounds_change_and_refuses_the_rest() -> None:
    async def create(req: dict) -> dict:
        assert req["reasoning"] == {"effort": "high"} and req["store"] is False and "KNOBS AND RULES" in req["input"][0]["content"][0]["text"]
        body = {"diagnosis": "died boosting into a bigger head", "hypothesis": "wider head margin",
                "metric_to_watch": "survival",
                "change": {"kind": "knob", "name": "BOOST_HEAD_AHEAD", "value": 450, "enabled": True}}
        return {"status": "completed", "output": [{"type": "message", "content": [{"type": "output_text", "text": json.dumps(body)}]}]}

    before = se.BOOST_HEAD_AHEAD
    try:
        result = asyncio.run(se.reflect(create, "log", "changes", "table", "decisions"))
        assert result["diagnosis"].startswith("died") and se.apply_change(result["change"]) == (True, "BOOST_HEAD_AHEAD = 450")
        assert se.BOOST_HEAD_AHEAD == 450
        assert se.apply_change({"kind": "knob", "name": "BOOST_HEAD_AHEAD", "value": 5000}) == (False, "BOOST_HEAD_AHEAD: 5000.0 outside [200, 600]")
        assert se.apply_change({"kind": "knob", "name": "os.system", "value": 1})[0] is False
        assert se.apply_change({"kind": "rule", "name": "swing_wide", "enabled": False}) == (True, "swing_wide = off")
        assert se.RULE_STATE["swing_wide"] is False
        assert se.apply_change({"kind": "rule", "name": "swing_wide", "enabled": "yes"})[0] is False
    finally:
        se.set_knob("BOOST_HEAD_AHEAD", before)
        se.set_rule("swing_wide", True)

    async def broken(req: dict) -> dict:
        raise RuntimeError("no network")
    assert asyncio.run(se.reflect(broken, "", "", "", ""))["error"].startswith("RuntimeError")


def test_settings_round_trip(tmp_path: Path) -> None:
    before = se.knob_values()
    try:
        se.set_knob("CHASE_SECS", 2.5)
        se.set_rule("relaxation", False)
        se.save_settings(tmp_path / "s.json")
        se.set_knob("CHASE_SECS", 3.0)
        se.set_rule("relaxation", True)
        loaded = se.load_settings(tmp_path / "s.json")
        assert se.CHASE_SECS == 2.5 and se.RULE_STATE["relaxation"] is False and loaded["knobs"]["CHASE_SECS"] == 2.5
        assert se.load_settings(tmp_path / "missing.json") == {}
    finally:
        for k, v in before.items():
            se.set_knob(k, v)
        se.set_rule("relaxation", True)
