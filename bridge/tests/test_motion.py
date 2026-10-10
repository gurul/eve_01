"""motion.py: the host half of the board's motion vocabulary.

The board is the authority on what is safe, so these tests are about the wire
shape and about the host telling the truth — not about enforcing limits. The
limits are tested where they live, in firmware/claude_pet_stackchan/host/
motion_test.cpp, against the same header the board runs.
"""

from __future__ import annotations

import pytest

from eve_bridge.motion import (
    KEEP,
    KEYS,
    MAX_KEYS,
    OSC,
    describe,
    keys_command,
    osc_command,
    predict,
    stop_command,
)

# What the firmware will accept (firmware/claude_pet_stackchan/src/motion.h:54,60);
# the board re-checks all of it.
PERIOD_MIN_MS, PERIOD_MAX_MS = 650, 8000
CYCLES_MAX = 12


def test_every_preset_makes_one_wire_line_of_the_right_shape() -> None:
    for name in sorted(set(OSC) | set(KEYS)):
        cmd = osc_command(name) if name in OSC else keys_command(name)
        assert cmd["cmd"] == "move"
        assert cmd["kind"] in ("osc", "keys")
        if cmd["kind"] == "keys":
            assert 1 <= len(cmd["keys"]) <= MAX_KEYS
            assert all(len(k) == 4 for k in cmd["keys"])
        else:
            assert cmd["period_ms"] >= PERIOD_MIN_MS
            assert cmd["yaw_amp"] or cmd["pitch_amp"], name


def test_the_dance_is_two_axes_a_quarter_cycle_apart() -> None:
    """That phase is what makes it an ellipse rather than a diagonal line — the
    difference between "up and down side to side" and one wobble."""
    cmd = osc_command("dance")
    assert cmd["yaw_amp"] > 0 and cmd["pitch_amp"] > 0
    assert cmd["phase_deg"] == 90


def test_faster_means_a_quicker_beat_not_a_refusal() -> None:
    """Amplitude yields on the board, the beat is kept. A request to go faster
    must therefore only shorten the period here."""
    base = osc_command("dance")
    fast = osc_command("dance", speed=1.6)
    slow = osc_command("dance", speed=0.6)
    assert fast["period_ms"] < base["period_ms"] < slow["period_ms"]
    assert fast["yaw_amp"] == base["yaw_amp"], "the host does not pre-shrink; the board does"


def test_speed_is_bounded_so_a_model_cannot_ask_for_a_blur() -> None:
    for wild in (0.01, -5, 99):
        cmd = osc_command("dance", speed=wild)
        assert PERIOD_MIN_MS / 4 <= cmd["period_ms"] <= PERIOD_MAX_MS * 4


def test_no_centre_means_centre_on_wherever_the_head_is() -> None:
    """A rhythm should grow out of the pose it is in, not snap somewhere first."""
    assert "center_yaw" not in osc_command("dance")
    assert "center_pitch" not in osc_command("dance")
    aimed = osc_command("dance", center_yaw=-30, center_pitch=40)
    assert aimed["center_yaw"] == -30 and aimed["center_pitch"] == 40


def test_the_host_table_asks_for_what_the_board_actually_runs() -> None:
    """dance once asked for 28 degrees and the board reduced it to 25, so the
    host predicted a swing it never delivered. Checked against the shipped
    header by firmware/.../host/motion_test.cpp and tools' admit_check."""
    assert OSC["dance"]["yaw_amp"] == 25


def test_overrides_beat_the_preset_and_a_bad_name_is_refused() -> None:
    cmd = osc_command("nod", pitch_amp=6, cycles=2)
    assert cmd["pitch_amp"] == 6 and cmd["cycles"] == 2
    with pytest.raises(ValueError):
        osc_command("moonwalk")
    with pytest.raises(ValueError):
        keys_command("moonwalk")


def test_a_gesture_keeps_an_axis_alone_with_the_sentinel() -> None:
    cmd = keys_command("doubletake")
    assert any(k[2] == KEEP for k in cmd["keys"]), "a doubletake is yaw only"
    assert KEYS["perk"][0][1] == KEEP, "perk is pitch only"


def test_raw_keys_are_accepted_capped_and_defaulted() -> None:
    cmd = keys_command(keys=[[0, 127, 35], [900, 127, 45, 400]])
    assert cmd["keys"][0] == [0, 127, 35, 500], "a missing speed gets a default"
    assert cmd["keys"][1] == [900, 127, 45, 400]
    long = keys_command(keys=[[i * 100, 0, 45, 400] for i in range(20)])
    assert len(long["keys"]) == MAX_KEYS
    with pytest.raises(ValueError):
        keys_command(keys=[[0, 1]])
    with pytest.raises(ValueError):
        keys_command(keys=[])


def test_stop_is_one_short_line() -> None:
    assert stop_command() == {"cmd": "move", "kind": "stop"}


def test_predict_reports_the_speed_the_board_budgets_against() -> None:
    p = predict(osc_command("dance"))
    assert p["kind"] == "osc"
    assert p["ms"] == OSC["dance"]["cycles"] * OSC["dance"]["period_ms"]
    # 2*pi*f*A, the same form as the firmware's own velocity budget
    assert 150 < p["peak_yaw_dps"] < 240, p
    k = predict(keys_command("shrug"))
    assert k["kind"] == "keys" and k["keys"] == 3 and k["ms"] > 700


def test_describe_says_something_a_person_can_read() -> None:
    text = describe(osc_command("dance"))
    assert "yaw" in text and "°" in text and "ms" in text
    assert "keys" in describe(keys_command("shrug"))


def test_every_preset_stays_inside_the_periods_and_cycles_the_board_accepts() -> None:
    """Not a safety check — the board does that. This catches a table edit that
    would be silently reduced, which is how a preset stops matching its name."""
    for name, p in OSC.items():
        assert PERIOD_MIN_MS <= p["period_ms"] <= PERIOD_MAX_MS, name
        if p.get("pitch_period_ms"):
            assert PERIOD_MIN_MS <= p["pitch_period_ms"] <= PERIOD_MAX_MS, name
        assert 1 <= p.get("cycles", 4) <= CYCLES_MAX, name
        assert p["yaw_amp"] * 2 * 3.1416 * (1000 / p["period_ms"]) < 240, name
