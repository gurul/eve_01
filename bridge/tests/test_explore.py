"""Idle explorer — schedule, change detector, budget, notes file, daemon edge.

No board, no Mac, no network: the Explorer is ticked with a hand-rolled
clock, thumbnails come from the pure gray path, the note client is a fake,
and the daemon methods run against the same stub transport test_vision uses.
"""

from __future__ import annotations

import asyncio
import base64
import logging
from datetime import datetime
from pathlib import Path
from types import MethodType, SimpleNamespace

import pytest

from eve_bridge import explore
from eve_bridge.caption_pager import CaptionPager
from eve_bridge.explore import (
    LOOK_INTERVAL_SECS,
    PAN_PITCHES,
    PAN_YAWS,
    WAYPOINTS,
    YAW_RANGE,
    ChangeDetector,
    ExploreConfig,
    Explorer,
    ExploreRefused,
    Look,
    Mode,
    Note,
    NoteTaker,
    Rest,
    TokenBucket,
    append_note,
    build_look_cmd,
    build_mode_cmd,
    configured,
    downscale_gray,
    frame_image,
    frame_thumb,
    mean_abs_diff,
    normalise,
    note_line,
    read_notes,
)
from eve_bridge.vision import Frame

W, H = 160, 120


def _frame(level: int = 0, seq: int = 1) -> Frame:
    return Frame(seq=seq, w=W, h=H, fmt="gray", data=bytes([level]) * (W * H))


def _cfg(**kw) -> ExploreConfig:
    base = dict(enabled=True, after_secs=600.0, notes_per_hour=6.0, model="fake",
                notes_dir=Path("/nonexistent"), cycle_wait_secs=900.0)
    base.update(kw)
    return ExploreConfig(**base)


def _explorer(**kw) -> Explorer:
    return Explorer(_cfg(), now=0.0, **kw)


IDLE = 601.0   # past after_secs


def _note_at(actions, level: int, yaw: int, pitch: int) -> bool:
    """The actions are exactly one Note of this frame at this gaze. The
    detector's measurements (thumb, diff, surprise) ride along on the Note
    and are asserted separately where they matter."""
    if len(actions) != 1 or not isinstance(actions[0], Note):
        return False
    n = actions[0]
    return n.frame == _frame(level) and (n.yaw, n.pitch) == (yaw, pitch)


def _tick(e: Explorer, now: float, idle: float = IDLE, frame=None, **kw):
    args = dict(listening=False, frame=frame, connected=True)
    args.update(kw)
    return e.tick(now, idle, **args)


# ---- wire builders ----------------------------------------------------------

def test_build_look_clamps_to_wire_ranges() -> None:
    assert build_look_cmd(-200, 0, 6000) == {"cmd": "look", "yaw": YAW_RANGE[0], "pitch": 5, "hold": 6000}
    assert build_look_cmd(200, 999, 6000) == {"cmd": "look", "yaw": YAW_RANGE[1], "pitch": 85, "hold": 6000}
    assert build_look_cmd(20.4, 100, -1) == {"cmd": "look", "yaw": 20, "pitch": 85, "hold": 0}


def test_build_mode() -> None:
    assert build_mode_cmd(True) == {"cmd": "mode", "explore": True}
    assert build_mode_cmd(False) == {"cmd": "mode", "explore": False}


# ---- config -----------------------------------------------------------------

def test_configured_defaults() -> None:
    c = configured({})
    # Exploring is explicit: the idle start ships off (owner, 2026-09-21); EVE_EXPLORE=1 turns it on.
    assert not c.enabled and c.after_secs == 600.0 and c.notes_per_hour == 6.0
    assert configured({"EVE_EXPLORE": "1"}).enabled
    assert c.model == "gpt-6-luna" and c.cycle_wait_secs == 900.0
    assert c.notes_dir == Path("~/.config/eve-bridge/notes").expanduser()


def test_configured_env_knobs(tmp_path: Path) -> None:
    c = configured({
        "EVE_EXPLORE": "0", "EVE_EXPLORE_AFTER_MIN": "2.5",
        "EVE_NOTES_PER_HOUR": "12", "EVE_NOTES_MODEL": "gpt-x",
        "EVE_NOTES_DIR": str(tmp_path), "EVE_EXPLORE_CYCLE_MIN": "1",
    })
    assert not c.enabled
    assert (c.after_secs, c.notes_per_hour, c.model, c.notes_dir, c.cycle_wait_secs) == (
        150.0, 12.0, "gpt-x", tmp_path, 60.0)


def test_configured_bad_number_falls_back() -> None:
    assert configured({"EVE_NOTES_PER_HOUR": "lots"}).notes_per_hour == 6.0
    assert configured({"EVE_EXPLORE_AFTER_MIN": "-3"}).after_secs == 600.0


# ---- state machine ----------------------------------------------------------

def test_starts_only_when_idle_and_unblocked() -> None:
    e = _explorer()
    assert _tick(e, 1.0, idle=599.0) == []
    assert _tick(e, 3.0, listening=True) == []
    assert _tick(e, 4.0, connected=False) == []
    assert e.state == Explorer.OFF
    actions = _tick(e, 5.0)
    assert actions == [Mode(True, "idle 10 min"), Look(*WAYPOINTS[0], 6000)]
    assert e.active


def test_disabled_config_never_starts() -> None:
    e = Explorer(_cfg(enabled=False), now=0.0)
    assert _tick(e, 5.0) == []
    assert e.state == Explorer.OFF


@pytest.mark.parametrize("kw,reason", [
    (dict(idle=0.0), "activity"),
    (dict(listening=True), "listen key"),
    (dict(connected=False), "board disconnected"),
])
def test_activity_stops_exploring(kw, reason) -> None:
    e = _explorer()
    _tick(e, 0.0)
    assert _tick(e, 1.0, **kw) == [Mode(False, reason)]
    assert e.state == Explorer.OFF
    # And it does not restart until the idle clock climbs back past after_secs.
    assert _tick(e, 2.0, idle=100.0) == []


def test_waypoint_cadence_one_look_per_six_seconds() -> None:
    e = _explorer(notes_enabled=False)
    looks: list[tuple[float, Look]] = []
    now = 0.0
    while e.state != Explorer.RESTING:
        for a in _tick(e, now):
            if isinstance(a, Look):
                looks.append((now, a))
        now += 1.0
    assert [(lk.yaw, lk.pitch) for _, lk in looks] == list(WAYPOINTS)
    assert all(lk.hold_ms == 6000 for _, lk in looks)
    times = [t for t, _ in looks]
    assert [b - a for a, b in zip(times[:-1], times[1:], strict=True)] == [6.0] * (len(WAYPOINTS) - 1)
    assert e.cycles == 1


def test_cycle_end_rests_with_the_board_still_exploring() -> None:
    """A finished pan cycle sends nothing over the wire: the board stays in
    explore mode and looks around on its own until the next cycle."""
    e = Explorer(_cfg(cycle_wait_secs=100.0), now=0.0, notes_enabled=False)
    now = 0.0
    last: list = []
    while e.state != Explorer.RESTING:
        last = _tick(e, now)
        now += 1.0
    assert last == [Rest("cycle complete, board looks around on its own; next pan in 2 min")]
    assert not any(isinstance(a, Mode) for a in last)
    assert e.on_board and not e.active
    rest_started = now - 1.0
    assert _tick(e, rest_started + 50.0) == []
    actions = _tick(e, rest_started + 100.0)
    assert actions[0] == Mode(True, "idle 10 min") and isinstance(actions[1], Look)


@pytest.mark.parametrize("kw,reason", [
    ({"idle": 0.0}, "activity"),
    ({"listening": True}, "listen key"),
    ({"connected": False}, "board disconnected"),
])
def test_activity_while_resting_leaves_explore_mode(kw, reason) -> None:
    """The board was left exploring through the rest; a blocker must hand
    the head back to the persona with an explicit mode-off."""
    e = _explorer(notes_enabled=False)
    now = 0.0
    while e.state != Explorer.RESTING:
        _tick(e, now)
        now += 1.0
    assert _tick(e, now, **kw) == [Mode(False, reason)]
    assert e.state == Explorer.OFF and not e.on_board


def test_on_board_tracks_exploring_and_resting() -> None:
    e = Explorer(_cfg(cycle_wait_secs=0.0), now=0.0, notes_enabled=False)
    assert not e.on_board
    _tick(e, 0.0)
    assert e.on_board and e.active
    now = 1.0
    while e.state != Explorer.RESTING:
        _tick(e, now)
        now += 1.0
    assert e.on_board and not e.active
    e.reset()
    assert not e.on_board


def test_frame_sampled_two_seconds_after_look_once_per_waypoint() -> None:
    e = _explorer()
    _tick(e, 0.0)
    assert not e.wants_frame(1.9)
    assert _tick(e, 1.0, frame=_frame(10)) == []          # too early: ignored
    assert e.wants_frame(2.0)
    actions = _tick(e, 2.0, frame=_frame(10))
    assert _note_at(actions, 10, *WAYPOINTS[0])
    assert actions[0].thumb is not None and actions[0].surprise is None   # bank still cold
    assert not e.wants_frame(3.0)
    assert _tick(e, 3.0, frame=_frame(200)) == []         # already sampled here
    # Next waypoint, new sample window.
    assert _tick(e, 6.0) == [Look(*WAYPOINTS[1], 6000)]
    assert _note_at(_tick(e, 8.0, frame=_frame(10)), 10, *WAYPOINTS[1])


def test_unchanged_view_spends_no_note_on_the_next_cycle() -> None:
    e = Explorer(_cfg(cycle_wait_secs=0.0), now=0.0)
    e.bucket = TokenBucket(1000.0, 0.0)
    notes = 0
    now = 0.0
    for _ in range(2):
        while True:
            actions = _tick(e, now, frame=_frame(50))
            notes += sum(isinstance(a, Note) for a in actions)
            now += 1.0
            if e.state == Explorer.RESTING:
                break
        now += 1.0
    assert notes == len(WAYPOINTS)              # first cycle: every waypoint is new
    assert e.skipped_same == len(WAYPOINTS)     # second cycle: nothing moved


def test_changed_view_spends_a_note() -> None:
    e = Explorer(_cfg(cycle_wait_secs=0.0), now=0.0)
    e.bucket = TokenBucket(1000.0, 0.0)
    _tick(e, 0.0)
    assert _note_at(_tick(e, 2.0, frame=_frame(50)), 50, *WAYPOINTS[0])
    # Run the cycle out, rest is zero, restart at the same waypoint with a new view.
    now = 3.0
    while e.state != Explorer.RESTING:
        _tick(e, now)
        now += 1.0
    _tick(e, now)                                   # restart at the first waypoint
    assert e.active and e.waypoint == WAYPOINTS[0]
    assert _note_at(_tick(e, now + 2.0, frame=_frame(120)), 120, *WAYPOINTS[0])


def test_budget_exhausted_skips_note_but_keeps_reference_unchanged() -> None:
    e = _explorer()
    e.bucket = TokenBucket(0.0, 0.0)
    _tick(e, 0.0)
    assert _tick(e, 2.0, frame=_frame(50)) == []
    assert e.skipped_budget == 1
    assert e.detector.measure(0, _frame(50)) is None   # nothing was kept


def test_notes_disabled_pans_without_notes() -> None:
    e = _explorer(notes_enabled=False)
    _tick(e, 0.0)
    assert _tick(e, 2.0, frame=_frame(50)) == []
    assert e.notes == 0


# ---- change detector --------------------------------------------------------

def test_downscale_gray_box_average() -> None:
    w, h = 4, 2
    luma = bytes([0, 0, 100, 100, 0, 0, 100, 100])
    assert downscale_gray(w, h, luma, tw=2, th=1) == bytes([0, 100])
    with pytest.raises(ValueError):
        downscale_gray(w, h, luma[:-1], tw=2, th=1)


def test_mean_abs_diff() -> None:
    assert mean_abs_diff(bytes([0, 10]), bytes([10, 30])) == 15.0
    with pytest.raises(ValueError):
        mean_abs_diff(b"\x00", b"\x00\x00")


def test_change_detector_first_visit_then_threshold() -> None:
    d = ChangeDetector(threshold=12.0)
    assert d.measure(0, _frame(50)) is None
    d.keep(0, _frame(50))
    assert d.measure(0, _frame(55)) == 5.0 <= d.threshold
    assert d.measure(0, _frame(70)) > d.threshold
    assert d.measure(1, _frame(50)) is None          # a different waypoint has its own memory


def test_change_detector_undecodable_counts_as_changed() -> None:
    d = ChangeDetector(thumb=lambda f: None)
    d.keep(0, _frame(50))
    assert d.measure(0, _frame(50)) is None


def test_frame_thumb_jpeg_without_imageio_is_none(monkeypatch) -> None:
    monkeypatch.setattr(explore.sys, "platform", "linux")
    assert explore.frame_thumb(Frame(seq=1, w=4, h=2, fmt="jpeg", data=b"\xff\xd8")) is None


# ---- manual explore (the owner asked) ---------------------------------------

def test_request_starts_at_once_and_ignores_idle() -> None:
    e = _explorer()
    assert _tick(e, 1.0, idle=0.0) == []                      # idle: nothing
    actions = e.request(1.0, "requested by cli")
    assert actions == [Mode(True, "requested by cli"), Look(*WAYPOINTS[0])]
    assert e.active and e.manual and e.started_reason == "requested by cli"
    # activity (a hook event, a running session) does not end a manual explore
    assert _tick(e, 2.0, idle=0.0) == []
    assert _tick(e, 7.0, idle=0.0) == [Look(*WAYPOINTS[1])]
    assert e.status()["manual"] is True and e.status()["waypoint"] == list(WAYPOINTS[1])


def test_manual_explore_survives_rest_and_pans_again() -> None:
    e = Explorer(_cfg(cycle_wait_secs=30.0), now=0.0)
    e.request(0.0, "requested by voice")
    now = 0.0
    while e.active:
        now += 6.0
        _tick(e, now, idle=0.0)
    assert e.state == Explorer.RESTING and e.manual and e.cycles == 1
    assert _tick(e, now + 10.0, idle=0.0) == []
    assert _tick(e, now + 31.0, idle=0.0) == [Mode(True, "requested by voice"), Look(*WAYPOINTS[0])]
    assert e.manual


@pytest.mark.parametrize("kw,reason", [
    ({"listening": True}, "listen key"),
    ({"connected": False}, "board disconnected"),
])
def test_hard_blockers_end_a_manual_explore_and_clear_manual(kw, reason) -> None:
    e = _explorer()
    e.request(0.0, "requested by cli")
    assert _tick(e, 1.0, idle=0.0, **kw) == [Mode(False, reason)]
    assert e.state == Explorer.OFF and not e.manual
    # ... and afterwards the idle rule applies again
    assert _tick(e, 2.0, idle=0.0) == []


@pytest.mark.parametrize("kw,reason", [
    ({"listening": True}, "listen key"),
    ({"connected": False}, "board disconnected"),
])
def test_request_is_refused_on_a_hard_blocker(kw, reason) -> None:
    e = _explorer()
    with pytest.raises(ExploreRefused, match=reason):
        e.request(0.0, "requested by cli", **kw)
    assert e.state == Explorer.OFF and not e.manual


def test_request_while_resting_sends_only_a_look() -> None:
    e = _explorer()
    now = 0.0
    _tick(e, now)                            # idle start
    while e.active:
        now += 6.0
        _tick(e, now)
    assert e.state == Explorer.RESTING
    assert e.request(now, "requested by cli") == [Look(*WAYPOINTS[0])]
    assert e.active and e.manual


def test_request_while_exploring_restarts_the_pan_without_mode() -> None:
    e = _explorer()
    _tick(e, 0.0)
    _tick(e, 6.0)                            # at waypoint 1
    assert e.waypoint == WAYPOINTS[1]
    assert e.request(6.0, "requested by cli") == [Look(*WAYPOINTS[0])]
    assert e.waypoint == WAYPOINTS[0] and e.manual


def test_dismiss_sends_mode_off_only_when_on_board() -> None:
    e = _explorer()
    assert e.dismiss("touch") == []          # nothing to undo
    e.request(0.0, "requested by cli")
    assert e.dismiss("touch (voice)") == [Mode(False, "touch (voice)")]
    assert e.state == Explorer.OFF and not e.manual
    assert e.dismiss("again") == []


def test_request_works_when_idle_start_is_disabled() -> None:
    e = Explorer(_cfg(enabled=False), now=0.0)
    assert _tick(e, 1.0) == []               # never starts on its own
    assert e.request(1.0, "requested by cli")[0] == Mode(True, "requested by cli")
    assert _tick(e, 2.0, idle=0.0) == []
    assert e.active


def test_status_when_off() -> None:
    e = _explorer()
    assert e.status() == {"state": "off", "manual": False, "reason": "", "waypoint": None,
                          "cycles": 0, "notes": 0}


# ---- token bucket -----------------------------------------------------------

def test_token_bucket_starts_full_then_refills_at_rate() -> None:
    b = TokenBucket(6.0, now=0.0)
    assert all(b.take(0.0) for _ in range(6))
    assert not b.take(0.0)
    assert not b.take(599.0)
    assert b.take(600.0)                      # one per ten minutes
    assert b.available(600.0) == 0.0
    assert b.available(3600.0 * 2) == 6.0     # never above capacity


def test_token_bucket_zero_rate_never_grants() -> None:
    b = TokenBucket(0.0, now=0.0)
    assert not b.take(0.0) and not b.take(10_000.0)


# ---- notes file -------------------------------------------------------------

def test_note_line_format() -> None:
    when = datetime(2026, 9, 5, 14, 7)
    assert note_line(when, 20, 40, "A lamp is on.") == "- 14:07 yaw=+20 pitch=40 — A lamp is on."
    assert note_line(when, -45, 60, "x").startswith("- 14:07 yaw=-45 pitch=60 — ")


def test_append_note_creates_private_dir_and_dated_file(tmp_path: Path) -> None:
    d = tmp_path / "notes"
    when = datetime(2026, 9, 5, 14, 7)
    p = append_note(d, when, 20, 40, "one")
    append_note(d, when.replace(minute=9), 0, 60, "two")
    assert p == d / "2026-09-05.md"
    assert p.read_text() == "- 14:07 yaw=+20 pitch=40 — one\n- 14:09 yaw=+0 pitch=60 — two\n"
    import os
    import stat
    if os.name != "nt":
        assert stat.S_IMODE(d.stat().st_mode) == 0o700


def test_read_notes_last_n_across_days(tmp_path: Path) -> None:
    append_note(tmp_path, datetime(2026, 9, 4, 9, 0), 0, 40, "old")
    append_note(tmp_path, datetime(2026, 9, 5, 9, 0), 0, 40, "a")
    append_note(tmp_path, datetime(2026, 9, 5, 9, 1), 0, 40, "b")
    assert read_notes(tmp_path, last=2) == [
        "2026-09-05 - 09:00 yaw=+0 pitch=40 — a",
        "2026-09-05 - 09:01 yaw=+0 pitch=40 — b",
    ]
    assert len(read_notes(tmp_path, last=0)) == 3
    assert read_notes(tmp_path, last=0)[0].startswith("2026-09-04")
    assert read_notes(tmp_path / "absent") == []


def test_notes_cli_prints(tmp_path: Path, capsys) -> None:
    append_note(tmp_path, datetime(2026, 9, 5, 9, 0), 0, 40, "hello")
    assert explore.run_notes_cli(5, _cfg(notes_dir=tmp_path)) == 0
    assert "hello" in capsys.readouterr().out
    assert explore.run_notes_cli(5, _cfg(notes_dir=tmp_path / "none")) == 0
    assert "no notes yet" in capsys.readouterr().out


# ---- note taker (fake client, no network) -----------------------------------

class _FakeClient:
    def __init__(self, text: str = "A quiet desk.", fail: Exception | None = None, delay: float = 0.0):
        self.text, self.fail, self.delay = text, fail, delay
        self.calls: list[tuple[int, str]] = []

    def describe(self, image: bytes, mime: str) -> str:
        import time
        self.calls.append((len(image), mime))
        if self.delay:
            time.sleep(self.delay)
        if self.fail is not None:
            raise self.fail
        return self.text


def test_frame_image_formats() -> None:
    jpeg = Frame(seq=1, w=4, h=2, fmt="jpeg", data=b"\xff\xd8\xff\xd9")
    assert frame_image(jpeg) == (b"\xff\xd8\xff\xd9", "image/jpeg")
    png, mime = frame_image(_frame(7))
    assert mime == "image/png" and png.startswith(b"\x89PNG")


def test_data_url() -> None:
    assert explore.data_url(b"abc", "image/png") == "data:image/png;base64," + base64.b64encode(b"abc").decode()


def test_note_taker_appends_and_logs(tmp_path: Path, caplog) -> None:
    client = _FakeClient("Two mugs by the window.")
    taker = NoteTaker(client, tmp_path, wall=lambda: datetime(2026, 9, 5, 15, 30))
    with caplog.at_level(logging.INFO, logger="eve_bridge.explore"):
        path = asyncio.run(taker.take(Note(_frame(9), 20, 40)))
    taker.stop()
    assert path == tmp_path / "2026-09-05.md"
    assert path.read_text() == "- 15:30 yaw=+20 pitch=40 — Two mugs by the window.\n"
    assert client.calls == [(len(frame_image(_frame(9))[0]), "image/png")]
    assert f"explore: note -> {path}" in caplog.text
    assert taker.taken == 1


def test_note_taker_failure_skips_and_logs_once_per_ten_minutes(tmp_path: Path, caplog) -> None:
    clock = {"t": 0.0}
    taker = NoteTaker(_FakeClient(fail=RuntimeError("boom")), tmp_path, clock=lambda: clock["t"])
    with caplog.at_level(logging.WARNING, logger="eve_bridge.explore"):
        assert asyncio.run(taker.take(Note(_frame(), 0, 40))) is None
        clock["t"] = 100.0
        assert asyncio.run(taker.take(Note(_frame(), 0, 40))) is None
        clock["t"] = 700.0
        assert asyncio.run(taker.take(Note(_frame(), 0, 40))) is None
    taker.stop()
    assert taker.failed == 3
    assert caplog.text.count("explore: note failed") == 2
    assert "RuntimeError: boom" in caplog.text
    assert not list(tmp_path.glob("*.md"))


def test_note_taker_timeout(tmp_path: Path, caplog) -> None:
    taker = NoteTaker(_FakeClient(delay=0.2), tmp_path, timeout=0.02)
    with caplog.at_level(logging.WARNING, logger="eve_bridge.explore"):
        assert asyncio.run(taker.take(Note(_frame(), 0, 40))) is None
    taker.stop()
    assert "timed out" in caplog.text


def test_make_note_client_without_key_is_none(caplog) -> None:
    with caplog.at_level(logging.WARNING, logger="eve_bridge.explore"):
        assert explore.make_note_client(_cfg(), environ={}) is None
    assert "OPENROUTER_API_KEY not set" in caplog.text


# ---- daemon edge --------------------------------------------------------------

class _StubBle:
    def __init__(self, connected: bool = True) -> None:
        self.connected = connected
        self.sent: list[dict] = []

    async def send(self, obj: dict, codec=None) -> bool:
        self.sent.append(obj)
        return True


def _daemon(connected: bool = True, running: int = 0, waiting: int = 0,
            notes: NoteTaker | None = None) -> SimpleNamespace:
    from eve_bridge.daemon import Daemon

    d = SimpleNamespace(
        ble=_StubBle(connected),
        state=SimpleNamespace(running_count=running, waiting_count=waiting),
        _listen_sent=None,
        _explore_cfg=_cfg(),
        _explorer=Explorer(_cfg(), now=0.0, notes_enabled=notes is not None),
        _notes=notes,
        _last_activity_at=0.0,
        _explore_raw_frame=None,
    )
    d._thought_pager = CaptionPager()
    d._on_caption = lambda msg: d.ble.sent.append(msg)
    for name in ("_note_activity", "_idle_secs", "_explore_step", "_run_explore_action", "_stop_explore",
                 "_request_explore", "_dismiss_explore", "_clear_thought", "_flush_thought_pager",
                 "_show_thought"):
        setattr(d, name, MethodType(getattr(Daemon, name), d))
    return d


def test_daemon_sends_mode_and_look_when_idle() -> None:
    async def go() -> list[dict]:
        d = _daemon()
        await d._explore_step(700.0)
        return d.ble.sent

    assert asyncio.run(go()) == [
        {"cmd": "mode", "explore": True},
        {"cmd": "look", "yaw": WAYPOINTS[0][0], "pitch": WAYPOINTS[0][1], "hold": 6000},
    ]


def test_daemon_idle_clock_resets_while_a_session_runs() -> None:
    d = _daemon(running=1)
    assert d._idle_secs(700.0) == 0.0
    d.state.running_count = 0
    assert d._idle_secs(701.0) == 1.0
    assert asyncio.run(d._explore_step(701.0)) is None
    assert d.ble.sent == []


def test_daemon_stops_on_activity_and_sends_mode_off() -> None:
    async def go() -> list[dict]:
        d = _daemon()
        await d._explore_step(700.0)
        d._note_activity()           # a hook event / touch / listen key
        await d._explore_step(701.0)
        return d.ble.sent

    sent = asyncio.run(go())
    assert sent[-1] == {"cmd": "mode", "explore": False}
    assert len(sent) == 3


def test_daemon_sends_nothing_while_disconnected() -> None:
    async def go() -> list[dict]:
        d = _daemon(connected=False)
        await d._explore_step(700.0)
        return d.ble.sent

    assert asyncio.run(go()) == []


def test_daemon_samples_frame_and_takes_note(tmp_path: Path) -> None:
    client = _FakeClient("A desk lamp.")
    taker = NoteTaker(client, tmp_path, wall=lambda: datetime(2026, 9, 5, 16, 0))

    async def go() -> list[dict]:
        d = _daemon(notes=taker)
        await d._explore_step(700.0)
        d._explore_raw_frame = {"seq": 1, "w": W, "h": H, "fmt": "gray",
                                "b64": base64.b64encode(bytes(W * H)).decode()}
        await d._explore_step(702.0)
        await asyncio.sleep(0.05)   # let the note task run
        assert d._explore_raw_frame is None
        return d.ble.sent

    sent = asyncio.run(go())
    taker.stop()
    assert sent[-1]["cmd"] == "look"
    yaw, pitch = WAYPOINTS[0]
    assert (tmp_path / "2026-09-05.md").read_text() == f"- 16:00 yaw={yaw:+d} pitch={pitch} — A desk lamp.\n"


def test_daemon_stop_explore_leaves_mode_on_shutdown() -> None:
    async def go() -> list[dict]:
        d = _daemon()
        await d._explore_step(700.0)
        await d._stop_explore("daemon stopping")
        assert not d._explorer.active
        await d._stop_explore("again")          # idempotent
        return d.ble.sent

    sent = asyncio.run(go())
    assert sent[-1] == {"cmd": "mode", "explore": False}
    assert sent.count({"cmd": "mode", "explore": False}) == 1


# ---- surprise: what this waypoint usually looks like -------------------------

def _room(seed: int, level: int = 0, block=None) -> Frame:
    """A frame of a plausible room: a bright half, a dark half, a bright band
    across the middle, plus a couple of luma units of sensor wobble that move
    with ``seed``. ``level`` shifts the whole frame (the lights changing);
    ``block`` paints (x0, y0, x1, y1, value) over it (something moved)."""
    data = bytearray(W * H)
    for y in range(H):
        for x in range(W):
            base = 70 if x < W // 2 else 150
            if 50 <= y < 70:
                base = 200
            data[y * W + x] = max(0, min(255, base + level + ((x + y + seed * 3) % 3)))
    if block is not None:
        x0, y0, x1, y1, value = block
        for y in range(y0, y1):
            for x in range(x0, x1):
                data[y * W + x] = value
    return Frame(seq=seed, w=W, h=H, fmt="gray", data=bytes(data))


def _noise(level: int, seed: int) -> Frame:
    """Kept for the callers that only need "a frame that differs a bit"."""
    return _room(seed, level=level - 100)


def test_surprise_is_none_until_the_bank_has_five_frames() -> None:
    d = ChangeDetector()
    for i in range(5):
        assert d.surprise(0, frame_thumb(_noise(100, i))) is None
    assert d.banked(0) == 5
    assert d.surprise(0, frame_thumb(_noise(100, 6))) is not None


def test_surprise_stays_low_for_the_usual_view_and_jumps_for_a_change() -> None:
    d = ChangeDetector()
    for i in range(12):
        d.surprise(0, frame_thumb(_noise(100, i)))
    usual = d.surprise(0, frame_thumb(_room(99)))
    # Something the size of a mug appears low on the left, where the wall has
    # never been anything but flat.
    odd = d.surprise(0, frame_thumb(_room(100, block=(10, 80, 50, 115, 240))))
    assert usual is not None and odd is not None
    assert usual < 1.5 < odd


def test_a_flickering_cell_stops_counting_as_surprise() -> None:
    """The noisy-TV case: a region that changes wildly every look raises its
    own spread, so it cannot keep buying attention."""
    def flicker(seed: int) -> Frame:
        # A screen in the corner: black one look, white the next.
        return _room(seed, block=(10, 80, 50, 115, 0 if seed % 2 else 255))

    d = ChangeDetector()
    for i in range(20):
        d.surprise(0, frame_thumb(flicker(i)))
    flickering = d.surprise(0, frame_thumb(flicker(21)))
    # The same-sized change where the wall has always been still scores far higher.
    still = d.surprise(0, frame_thumb(_room(22, block=(100, 80, 140, 115, 240))))
    assert flickering < 1.5 < still


def test_surprise_ignores_the_lights_going_down() -> None:
    d = ChangeDetector()
    for i in range(12):
        d.surprise(0, frame_thumb(_room(i)))
    dimmer = _room(99, level=-45)                  # same room, much darker
    assert d.surprise(0, frame_thumb(dimmer)) < 1.5


def test_banks_are_per_waypoint() -> None:
    d = ChangeDetector()
    for i in range(12):
        d.surprise(0, frame_thumb(_room(i)))
    assert d.banked(0) == 12 and d.banked(1) == 0
    assert d.surprise(1, frame_thumb(_room(3))) is None


def test_normalise_centres_on_mid_grey() -> None:
    assert set(normalise(bytes([10]) * 16)) == {128}
    assert set(normalise(bytes([200]) * 16)) == {128}


def test_every_considered_frame_is_banked_even_when_no_note_is_spent() -> None:
    e = _explorer()
    e.bucket = TokenBucket(0.0, 0.0)              # no budget: nothing is ever spent
    _tick(e, 0.0)
    for i in range(3):
        _tick(e, 2.0 + i * 6.0, frame=_noise(100, i))
        _tick(e, 6.0 + i * 6.0)
    assert e.notes == 0
    assert sum(e.detector.banked(w) for w in range(len(WAYPOINTS))) >= 3


# ---- the pan plan against the neck it has to run on -----------------------------------------

def test_the_pan_plan_stays_inside_the_servos_own_limit() -> None:
    """The BSP configures the yaw servo with angleLimit +-128 degrees, and the
    firmware clamps to +-120 (body.cpp YAW_MAX). Every waypoint and everything
    build_look_cmd can emit has to fit inside that, or the head is asking for a
    place it cannot go."""
    assert YAW_RANGE == (-120, 120)
    for yaw, pitch in WAYPOINTS:
        assert YAW_RANGE[0] <= yaw <= YAW_RANGE[1], (yaw, pitch)
        assert 5 <= pitch <= 85, (yaw, pitch)
    for yaw in (-1000, -121, 0, 121, 1000):
        assert YAW_RANGE[0] <= build_look_cmd(yaw, 40)["yaw"] <= YAW_RANGE[1]


def test_the_pan_sweeps_the_whole_range_in_small_steps() -> None:
    """Both extremes are visited at both heights, and no two consecutive
    waypoints are far apart — the head never swings the full span inside one
    hold, which it could not finish in six seconds."""
    yaws = [w[0] for w in WAYPOINTS]
    assert min(yaws) == YAW_RANGE[0] and max(yaws) == YAW_RANGE[1]
    for pitch in PAN_PITCHES:
        row = [y for y, p in WAYPOINTS if p == pitch]
        assert min(row) == YAW_RANGE[0] and max(row) == YAW_RANGE[1]
        assert len(row) == len(PAN_YAWS)
    # Inside a cycle every move is small; the one long swing back to the first
    # waypoint happens after the rest, with minutes to make it.
    steps = [abs(b[0] - a[0]) for a, b in zip(WAYPOINTS[:-1], WAYPOINTS[1:], strict=True)]
    assert max(steps) <= 30


def test_a_cycle_is_still_a_sane_length() -> None:
    assert 60.0 <= len(WAYPOINTS) * LOOK_INTERVAL_SECS <= 240.0


def test_the_rows_straddle_wherever_level_turns_out_to_be() -> None:
    """PITCH_LEVEL in the firmware is 45 and its own comment calls that a
    guess — it depends on how the head was zeroed. So the plan must not assume
    it: one row has to sit below any plausible level and one above, or a head
    zeroed slightly low spends the whole pan on the ceiling (bench 2026-09-06,
    which is exactly what happened with rows at 40 and 60)."""
    assert len(PAN_PITCHES) >= 3
    assert min(PAN_PITCHES) <= 25, "no row looks down at the desk"
    assert max(PAN_PITCHES) >= 65, "no row looks up at the room"
    assert min(PAN_PITCHES) >= 5 and max(PAN_PITCHES) <= 85, "outside the neck's travel"
    for guess_at_level in (35, 45, 55):
        assert any(p < guess_at_level for p in PAN_PITCHES), guess_at_level
        assert any(p > guess_at_level for p in PAN_PITCHES), guess_at_level
