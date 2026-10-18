"""firetv.py: the connection (connect, get-state, reconnect once, mDNS), the state read, power, keys, volume, mute,
apps, YouTube search, typing, the screenshot, the code path, the tools, construction and the CLI.

The fake answers in the TV's own formats (read off a Fire TV on 2026-10-04), with documentation addresses
(192.0.2.x) and made-up serials."""

from __future__ import annotations

import asyncio
import inspect
import json
import re
import shlex
import stat
from pathlib import Path
from typing import Any, Optional

import pytest

from eve_bridge import firetv as F

HOST = "192.0.2.40"
SERIAL = f"{HOST}:5555"
NEW_HOST = "192.0.2.41"
MDNS = "adb-TESTSERIAL0001"
PNG = F.PNG_SIGNATURE + b"\x00\x00\x00\rIHDR" + b"\x00" * 32
HOME_FOCUS = "  mCurrentFocus=Window{fd3feb u0 com.amazon.tv.launcher/com.amazon.tv.launcher.ui.HomeActivity_vNext}"
YT_FOCUS = "  mCurrentFocus=Window{1a2b3c u0 com.amazon.firetv.youtube/dev.cobalt.app.MainActivity}"
DREAM_FOCUS = "  mCurrentFocus=Window{4c7b09a u0 com.amazon.ftv.screensaver/android.service.dreams.DreamActivity}"
HDMI_FOCUS = ("  mCurrentFocus=Window{77aa u0 com.amazon.tv.inputpreference.service/"
              "com.amazon.tv.inputpreference.player.PassthroughPlayerActivity}")


def run(coro: Any) -> Any:
    return asyncio.run(coro)


async def _no_sleep(_: float) -> None:
    return None


def audio_block(volume: int, muted: bool, max_index: int = 100) -> str:
    return ("- STREAM_MUSIC:\n"
            f"   Muted: {'true' if muted else 'false'}\n"
            "   Muted Internally: false\n"
            "   Min: 0\n"
            f"   Max: {max_index}\n"
            f"   streamVolume:{volume}\n"
            f"   Current: 2 (speaker): {volume}, 4 (headset): 10, 400 (hdmi): 100, 40000000 (default): 25\n"
            "   Devices: speaker\n")


def state_text(wake: str = "Awake", display: str = "ON", focus: str = HOME_FOCUS, volume: Optional[int] = 44,
               muted: bool = False, max_index: int = 100) -> str:
    return ("@@POWER\n"
            f"  mWakefulness={wake}\n"
            f"Display Power: state={display}\n"
            "@@FOCUS\n"
            f"{focus}\n"
            "@@AUDIO\n"
            + (audio_block(volume, muted, max_index) if volume is not None else "")
            + "@@END\n")


class FakeAdb:
    """The adb CLI and the TV behind it: records every argv, answers like the real thing."""

    def __init__(self) -> None:
        self.calls: list[list[str]] = []
        self.at = SERIAL                    # where the TV really answers
        self.authorized = True
        self.wake = "Awake"
        self.display = "ON"
        self.focus = HOME_FOCUS
        self.volume: Optional[int] = 44
        self.max_index = 100
        self.muted = False
        self.lose_next = 0                  # the next N shell calls answer "not found", as ``adb shell`` says it
        self.lost_answer = ""               # or this, when set (e.g. "adb: device offline")
        self.timeout_next = 0
        self.mdns_rows = [(MDNS, SERIAL)]
        self.broken: set[str] = set()       # components whose am start fails
        self.resolve: dict[str, str] = {}   # package -> component from resolve-activity
        self.installed = ["com.amazon.tv.launcher", "com.amazon.firetv.youtube", "com.netflix.ninja",
                          "com.example.coolgame", "com.example.coolgame.helper", "com.example.weatherly"]
        self.wake_works = True
        self.png = PNG
        self.model = "AFTTEST1"
        self.serialno = "TESTSERIAL0001"

    # -- the runner --
    async def __call__(self, argv: list[str], timeout: float) -> tuple[int, str]:
        self.calls.append(list(argv))
        args = argv[1:]
        if args[:1] == ["connect"]:
            if args[1] != self.at:
                return 0, f"failed to connect to '{args[1]}': Connection refused\n"
            return 0, f"connected to {args[1]}\n"
        if args[:1] == ["disconnect"]:
            return 0, f"disconnected {args[1]}\n"
        if args[:2] == ["mdns", "services"]:
            rows = "".join(f"{n}\t_adb._tcp\t{hp}\n{n}-Xy12Ab\t_adb-tls-connect._tcp\t{hp.split(':')[0]}:41234\n"
                           for n, hp in self.mdns_rows)
            return 0, "List of discovered mdns services\n" + rows
        if args[:1] == ["-s"] and args[2] == "get-state":
            if args[1] != self.at:
                return 1, f"error: device '{args[1]}' not found\n"
            return (0, "device\n") if self.authorized else (1, "error: device unauthorized.\n")
        if args[:1] == ["-s"] and args[2] == "shell":
            if self.timeout_next:
                self.timeout_next -= 1
                return F.RC_TIMEOUT, ""
            if self.lose_next:
                self.lose_next -= 1
                # ``adb shell`` says "adb: ...", not "error: ..." like get-state (adb 1.0.41, 2026-10-04)
                return 1, self.lost_answer or f"adb: device '{args[1]}' not found\n"
            return self.shell(args[3])
        return 1, "unknown\n"

    async def capture(self, argv: list[str], timeout: float) -> tuple[int, bytes]:
        self.calls.append(list(argv))
        if self.lose_next:
            self.lose_next -= 1
            return 1, b"error: device offline\n"
        return 0, self.png

    # -- the TV --
    def shell(self, script: str) -> tuple[int, str]:
        if script == F.STATE_SCRIPT:
            return 0, state_text(self.wake, self.display, self.focus, self.volume, self.muted, self.max_index)
        if "resolve-activity" in script:
            pkg = script.split()[-1]
            comp = self.resolve.get(pkg, "")
            lean = f"priority=0 preferredOrder=0 match=0x108000\n{comp}\n" if comp else "No activity found\n"
            return 0, lean + "@@\nNo activity found\n"
        if script.startswith("getprop"):
            return 0, f"{self.model}\n@@\n{self.serialno}\n"
        words = shlex.split(script)
        if words[:2] == ["input", "keyevent"]:
            codes = [int(c) for c in words[2:]]
            for c in codes:
                if c == F.KEYCODE_WAKEUP and self.wake_works:
                    self.wake, self.display = "Awake", "ON"
                elif c == F.KEYCODE_SLEEP:
                    self.wake, self.display = "Asleep", "OFF"
                elif c == F.KEYCODE_VOLUME_MUTE:
                    self.muted = not self.muted
            return 0, ""
        if words[:2] == ["input", "text"]:
            return 0, ""
        if words[:3] == ["cmd", "media_session", "volume"]:
            n = int(words[words.index("--set") + 1])
            self.volume = n
            self.muted = n == 0
            return 0, ""
        if words[:3] == ["cmd", "media_session", "dispatch"]:
            return 0, ""
        if words[:2] == ["am", "start"]:
            comp = words[words.index("-n") + 1]
            if comp in self.broken:
                return 0, (f"Starting: Intent {{ cmp={comp} }}\nError type 3\n"
                           f"Error: Activity class {{{comp}}} does not exist.\n")
            self.focus = f"  mCurrentFocus=Window{{abc u0 {comp}}}"
            return 0, f"Starting: Intent {{ cmp={comp} }}\n"
        if script == "pm list packages":
            return 0, "".join(f"package:{p}\n" for p in self.installed)
        return 127, f"/system/bin/sh: {words[0] if words else ''}: not found\n"

    # -- what was sent --
    def shells(self) -> list[str]:
        return [c[4] for c in self.calls if len(c) > 4 and c[3] == "shell"]

    def sent(self) -> list[str]:
        return [s for s in self.shells() if s != F.STATE_SCRIPT]


def tv(fake: Optional[FakeAdb] = None, path: Optional[Path] = None, **cfg: Any) -> tuple[F.FireTV, FakeAdb]:
    fake = fake or FakeAdb()
    conf = {"host": HOST, "port": 5555, "mdns": MDNS, **cfg}
    return F.FireTV(conf, runner=fake, capture=fake.capture, adb="adb", path=path, sleep=_no_sleep), fake


# ---- the connection ----

def test_connect_then_get_state_once_then_reuses_it() -> None:
    t, fake = tv()
    assert run(t.status())["ok"]
    assert run(t.status())["ok"]
    verbs = [c[1] if c[1] != "-s" else c[3] for c in fake.calls]
    assert verbs.count("connect") == 1 and verbs.count("get-state") == 1


def test_already_connected_counts_as_connected() -> None:
    t, fake = tv()

    async def already(argv: list[str], timeout: float) -> tuple[int, str]:
        if argv[1] == "connect":
            return 0, f"already connected to {SERIAL}\n"
        return await fake(argv, timeout)
    t.runner = already
    assert run(t.ensure()).ok


@pytest.mark.parametrize("reply", ["failed to connect to '192.0.2.40:5555': Connection refused",
                                   "unable to connect to 192.0.2.40:5555: No route to host",
                                   "cannot connect to 192.0.2.40:5555: Operation timed out", ""])
def test_connect_failure_words_with_exit_code_zero(reply: str) -> None:
    async def runner(argv: list[str], timeout: float) -> tuple[int, str]:
        if argv[1] == "mdns":
            return 0, "List of discovered mdns services\n"
        return 0, reply
    t = F.FireTV({"host": HOST, "port": 5555, "mdns": MDNS}, runner=runner, adb="adb", sleep=_no_sleep)
    r = run(t.ensure())
    assert not r.ok and r.why == "unreachable" and r.line == F.UNREACHABLE_LINE


def test_unauthorized_is_its_own_outcome_and_says_the_allow_prompt() -> None:
    t, fake = tv()
    fake.authorized = False
    out = run(t.status())
    assert out["ok"] is False and out["authorized"] is False and out["reachable"] is True
    assert "Allow USB debugging" in out["line"] and "Always allow" in out["line"]
    assert not any(c[1] == "mdns" for c in fake.calls)            # nothing to rediscover: the TV answered


def test_offline_device_is_unreachable_after_one_rediscovery() -> None:
    async def runner(argv: list[str], timeout: float) -> tuple[int, str]:
        if argv[1] == "connect":
            return 0, f"connected to {SERIAL}"
        if argv[1] == "mdns":
            return 0, "List of discovered mdns services\n"
        if argv[1] == "-s" and argv[3] == "get-state":
            return 1, "error: device offline\n"
        return 0, ""
    t = F.FireTV({"host": HOST, "port": 5555, "mdns": MDNS}, runner=runner, adb="adb", sleep=_no_sleep)
    r = run(t.ensure())
    assert not r.ok and r.why == "offline"


def test_lost_mid_command_disconnects_reconnects_and_retries_once() -> None:
    t, fake = tv()
    run(t.ensure())
    fake.lose_next = 1
    assert run(t.status())["ok"]
    verbs = [c[1] for c in fake.calls]
    assert "disconnect" in verbs and verbs.count("connect") == 2
    assert not any("kill-server" in c for c in fake.calls)


def test_lost_twice_fails_fast_without_a_third_try() -> None:
    t, fake = tv()
    run(t.ensure())
    fake.lose_next = 5
    out = run(t.status())
    assert out["ok"] is False
    assert len([s for s in fake.shells() if s == F.STATE_SCRIPT]) == 2


def test_a_timeout_is_an_outcome_and_reconnects() -> None:
    t, fake = tv()
    run(t.ensure())
    fake.timeout_next = 1
    assert run(t.status())["ok"]


def test_adb_shells_own_offline_words_reconnect_too() -> None:
    t, fake = tv()
    run(t.ensure())
    fake.lose_next, fake.lost_answer = 1, "adb: device offline\n"
    assert run(t.status())["ok"]
    assert [c[1] for c in fake.calls].count("disconnect") == 1


def test_a_lost_tv_is_never_stuck_ready() -> None:
    # the adb server restarted under eve: every shell says "not found" until eve connects again
    t, fake = tv()
    run(t.ensure())
    fake.lose_next = 1
    assert run(t.control("power_on"))["line"] == "The TV is already on."
    assert not F._lost(1, "adb: error: something on the TV failed")


def test_a_timeout_never_types_or_presses_twice() -> None:
    t, fake = tv()
    run(t.ensure())
    fake.timeout_next = 1
    out = run(t.type_text("lofi"))
    assert out["ok"] is False and out["line"] == F.TIMEOUT_LINE
    assert len([s for s in fake.shells() if s.startswith("input text")]) == 1
    fake.timeout_next = 1
    assert run(t.press("down", 3))["line"] == F.TIMEOUT_LINE
    assert len([s for s in fake.shells() if s.startswith("input keyevent")]) == 1
    fake.timeout_next = 1
    assert run(t.control("play_pause"))["line"] == F.TIMEOUT_LINE
    # the next command connects afresh and works
    assert run(t.press("down"))["ok"]
    # "not found" before the command ran: it never landed, so it is sent again
    fake.lose_next = 1
    assert run(t.type_text("lofi"))["ok"]
    assert len([s for s in fake.shells() if s.startswith("input text")]) == 3


def test_a_state_read_without_the_state_connects_afresh_next_time() -> None:
    t, fake = tv()
    run(t.ensure())
    real = fake.shell
    fake.shell = lambda script: (0, "") if script == F.STATE_SCRIPT else real(script)  # type: ignore[method-assign]
    r, st = run(t.state())
    assert st is None and t._ready is False
    fake.shell = real  # type: ignore[method-assign]
    assert run(t.status())["ok"]


def test_a_cancelled_adb_call_is_killed(tmp_path: Path) -> None:
    import os

    pid_file = tmp_path / "pid"

    async def go() -> None:
        task = asyncio.ensure_future(F.adb_runner(["/bin/sh", "-c", f"echo $$ > {pid_file}; exec sleep 30"], 60))
        for _ in range(100):
            await asyncio.sleep(0.02)
            if pid_file.exists() and pid_file.read_text().strip():
                break
        task.cancel()
        with pytest.raises(asyncio.CancelledError):
            await task

    run(go())
    pid = int(pid_file.read_text())
    with pytest.raises(ProcessLookupError):
        os.kill(pid, 0)


def test_mdns_rediscovery_moves_to_the_new_address_and_saves_it(tmp_path: Path) -> None:
    p = tmp_path / "firetv.json"
    F.save({"host": HOST, "port": 5555, "mdns": MDNS}, p)
    fake = FakeAdb()
    fake.at = f"{NEW_HOST}:5555"                 # DHCP gave the TV a new address
    fake.mdns_rows = [("adb-SOMEONEELSE9", "192.0.2.77:5555"), (MDNS, f"{NEW_HOST}:5555")]
    t, _ = tv(fake, path=p)
    out = run(t.status())
    assert out["ok"] and t.serial == f"{NEW_HOST}:5555"
    assert F.load(p)["host"] == NEW_HOST
    assert stat.S_IMODE(p.stat().st_mode) == 0o600


def test_parse_mdns_keeps_plain_adb_only() -> None:
    text = ("List of discovered mdns services\n"
            f"{MDNS}\t_adb._tcp\t{SERIAL}\n"
            f"{MDNS}-Ab12Cd\t_adb-tls-connect._tcp\t{HOST}:41234\n")
    assert F.parse_mdns(text) == [(MDNS, HOST, 5555)]


def test_no_adb_says_how_to_install() -> None:
    t = F.FireTV({"host": HOST, "port": 5555}, runner=FakeAdb(), adb="", sleep=_no_sleep)
    out = run(t.control("home"))
    assert out["ok"] is False and "brew install --cask android-platform-tools" in out["line"]


def test_find_adb_override_first() -> None:
    assert F.find_adb({"EVE_ADB": "/opt/x/adb"}) == "/opt/x/adb"


def test_real_runner_times_out_as_an_outcome() -> None:
    rc, out = run(F.adb_runner(["/bin/sleep", "5"], 0.2))
    assert rc == F.RC_TIMEOUT and out == ""
    rc, out = run(F.adb_runner(["/nonexistent/adb-binary"], 1.0))
    assert rc == F.RC_NO_ADB and "could not start" in out


# ---- the state ----

def test_parse_state_awake_home_volume() -> None:
    st = F.parse_state(state_text())
    assert (st.power, st.app, st.volume, st.muted) == ("on", "home", 44, False)
    assert F.said_state(st) == "TV on — home, volume 44%"


def test_parse_state_screensaver_is_on() -> None:
    st = F.parse_state(state_text("Dreaming", "ON", DREAM_FOCUS))
    assert st.power == "screensaver" and st.app == "screensaver"


@pytest.mark.parametrize("wake,display", [("Asleep", "OFF"), ("Dozing", "DOZE"), ("Awake", "OFF")])
def test_parse_state_standby(wake: str, display: str) -> None:
    st = F.parse_state(state_text(wake, display))
    assert st.power == "standby" and F.said_state(st) == "TV in standby, volume 44%."
    # with no volume to read it says only the power
    assert F.said_state(F.parse_state(state_text(wake, display, volume=None))) == "TV in standby."


def test_parse_state_foreground_names() -> None:
    assert F.parse_state(state_text(focus=YT_FOCUS)).app == "YouTube"
    assert F.parse_state(state_text(focus=HDMI_FOCUS)).app == "an HDMI input"
    resumed = "    mResumedActivity: ActivityRecord{1fceaad u0 com.netflix.ninja/.MainActivity t18}"
    st = F.parse_state(state_text(focus=resumed))
    assert st.app == "Netflix" and st.activity == "com.netflix.ninja.MainActivity"
    assert F.parse_state(state_text(focus="  mCurrentFocus=null")).app == ""


def test_parse_state_projectivy_is_home() -> None:
    focus = "  mCurrentFocus=Window{9d1e u0 com.spocky.projengmenu/com.spocky.projengmenu.ui.home.MainActivity}"
    assert F.parse_state(state_text(focus=focus)).app == "home"
    settings = ("  mCurrentFocus=Window{9d1f u0 com.spocky.projengmenu/"
                "com.spocky.projengmenu.ui.guidedActions.activities.settings.AppSettingsActivity}")
    assert F.parse_state(state_text(focus=settings)).app == "Projectivy settings"


def test_parse_state_missing_volume_is_unknown_not_guessed() -> None:
    st = F.parse_state(state_text(volume=None))
    assert st.volume is None and st.muted is None and "volume unknown" in F.said_state(st)


def test_parse_state_scales_to_the_tvs_max() -> None:
    assert F.parse_state(state_text(volume=6, max_index=15)).volume == 40


def test_state_script_is_one_round_trip_and_never_grep_m1() -> None:
    assert "-m1" not in F.STATE_SCRIPT and "-m 1" not in F.STATE_SCRIPT
    for part in ("mWakefulness=", "Display Power: state=", "mCurrentFocus", "mResumedActivity", "STREAM_MUSIC"):
        assert part in F.STATE_SCRIPT
    t, fake = tv()
    run(t.status())
    assert len(fake.shells()) == 1


def test_sanitise_strips_controls_and_caps() -> None:
    assert F.clean("Evil\nApp\x1b[31m‮Name\x00") == "Evil App [31m Name"
    assert len(F.clean("x" * 500)) == F.CLEAN_CAP
    weird = "  mCurrentFocus=Window{1 u0 com.evil.app\x1b/.Main}"
    assert "\x1b" not in F.parse_state(state_text(focus=weird)).app


# ---- power ----

def test_power_on_sends_wakeup_and_polls_until_awake() -> None:
    t, fake = tv()
    fake.wake, fake.display = "Asleep", "OFF"
    out = run(t.control("power_on"))
    assert out["ok"] and out["line"] == "TV on — home." and out["power"] == "on"
    assert fake.sent() == ["input keyevent 224"]


def test_power_on_when_already_on_sends_nothing() -> None:
    t, fake = tv()
    assert run(t.control("power_on"))["line"] == "The TV is already on."
    assert fake.sent() == []


def test_power_on_that_does_not_take_reports_what_the_tv_says() -> None:
    t, fake = tv()
    fake.wake, fake.display, fake.wake_works = "Asleep", "OFF", False
    out = run(t.control("power_on"))
    assert out["ok"] is False and "still reports standby" in out["line"]


def test_power_on_unreachable_says_deep_standby() -> None:
    async def runner(argv: list[str], timeout: float) -> tuple[int, str]:
        return (0, "List of discovered mdns services\n") if argv[1] == "mdns" else (0, "failed to connect to x")
    t = F.FireTV({"host": HOST, "port": 5555, "mdns": MDNS}, runner=runner, adb="adb", sleep=_no_sleep)
    out = run(t.control("power_on"))
    assert out["ok"] is False and "deep standby" in out["line"] and "remote" in out["line"]


def test_power_off_sends_sleep() -> None:
    t, fake = tv()
    out = run(t.control("power_off"))
    assert out["ok"] and out["line"] == "TV off."
    assert fake.sent() == ["input keyevent 223"]


def test_screensaver_counts_as_on_for_power_off() -> None:
    t, fake = tv()
    fake.wake, fake.focus = "Dreaming", DREAM_FOCUS
    assert run(t.control("power_off"))["ok"] and fake.sent() == ["input keyevent 223"]


# ---- keys ----

def test_navigation_is_batched_in_one_call_in_order() -> None:
    t, fake = tv()
    out = run(t.control("down", repeat=3))
    assert out["ok"] and out["line"] == "Pressed down 3 times."
    assert fake.sent() == ["input keyevent 20 20 20"]
    run(t.control("select"))
    assert fake.sent()[-1] == "input keyevent 23"


def test_navigation_repeat_is_capped() -> None:
    t, fake = tv()
    out = run(t.control("right", repeat=99))
    assert out["pressed"] == F.MAX_REPEAT and fake.sent() == ["input keyevent " + " ".join(["22"] * F.MAX_REPEAT)]
    run(t.control("left", repeat=0))
    assert fake.sent()[-1] == "input keyevent 21"


@pytest.mark.parametrize("action,key", [("play_pause", "play-pause"), ("play", "play"), ("pause", "pause"),
                                        ("next", "next"), ("previous", "previous"), ("rewind", "rewind"),
                                        ("fast_forward", "fast-forward")])
def test_media_keys_go_through_media_session_never_input(action: str, key: str) -> None:
    t, fake = tv()
    assert run(t.control(action))["ok"]
    assert fake.sent() == [f"cmd media_session dispatch {key}"]


# ---- volume and mute ----

def test_volume_absolute_maps_percent_to_index_and_reads_back() -> None:
    t, fake = tv()
    fake.max_index = 15
    fake.volume = 6
    out = run(t.control("volume", volume=60))
    assert fake.sent() == ["cmd media_session volume --stream 3 --set 9"]
    assert out["ok"] and out["volume"] == 60 and out["line"] == "TV volume 60%."


def test_volume_relative_reads_then_sets_and_clamps() -> None:
    t, fake = tv()
    assert run(t.control("volume", volume_change=10))["volume"] == 54
    fake.volume = 97
    assert run(t.control("volume", volume_change=10))["volume"] == 100
    fake.volume = 3
    assert run(t.control("volume", volume_change=-10))["volume"] == 0
    assert run(t.control("volume", volume=250))["volume"] == 100


def test_volume_reports_the_read_value_not_the_asked_one() -> None:
    t, fake = tv()
    real_shell = fake.shell

    def stuck(script: str) -> tuple[int, str]:
        if "--set" in script:
            return 0, ""                     # the TV ignored it
        return real_shell(script)
    fake.shell = stuck  # type: ignore[method-assign]
    out = run(t.control("volume", volume=30))
    assert out["volume"] == 44 and out["line"] == "TV volume 44%."


def test_volume_unreadable_refuses_to_guess() -> None:
    t, fake = tv()
    fake.volume = None
    out = run(t.control("volume", volume_change=10))
    assert out["ok"] is False and "couldn't read" in out["line"]
    assert fake.sent() == []


def test_mute_toggles_only_when_needed() -> None:
    t, fake = tv()
    out = run(t.control("mute"))
    assert out["ok"] and out["muted"] is True and fake.sent() == ["input keyevent 164"]
    assert run(t.control("mute"))["line"] == "The TV is already muted."
    assert fake.sent() == ["input keyevent 164"]
    out = run(t.control("unmute"))
    assert out["ok"] and out["muted"] is False and out["line"] == "TV unmuted, volume 44%."
    assert fake.sent() == ["input keyevent 164", "input keyevent 164"]
    assert not any("dispatch mute" in s for s in fake.sent())     # that one is the microphone's key


class LossyMuteTv(FakeAdb):
    """A TV whose mute key does not take (verification/Eve/FireTv.lean ``code_violates_*``): ``key`` is what the
    key answers ("dropped": rc 0 and no effect; "refused": rc != 0), and after it the readback does not come back
    ("timeout": no answer and the reconnect fails; "no_muted": an answer without the ``Muted:`` line; "read": a
    true reading)."""

    def __init__(self, key: str, readback: str) -> None:
        super().__init__()
        self.key, self.readback = key, readback

    def shell(self, script: str) -> tuple[int, str]:
        if script != f"input keyevent {F.KEYCODE_VOLUME_MUTE}":
            return super().shell(script)
        if self.readback == "timeout":
            self.timeout_next = 1                   # the readback gets no answer...
            self.at, self.mdns_rows = "10.0.0.99:5555", []   # ...and the reconnect finds no TV
        elif self.readback == "no_muted":
            self.volume = None                      # the readback answers without the audio block
        return (0, "") if self.key == "dropped" else (1, "java.lang.SecurityException: injecting denied\n")


def test_mute_lost_and_unread_is_not_ok() -> None:
    # FireTv.lean unreadTrace / code_violates_unread_ok: unmuted, "mute", the key is answered but lost, the readback
    # times out and the reconnect fails. Before the fix this said ok while the TV stayed unmuted.
    t, fake = tv(LossyMuteTv("dropped", "timeout"))
    out = run(t.control("mute"))
    assert fake.muted is False and fake.sent() == ["input keyevent 164"]
    assert out["ok"] is False and "couldn't" in out["line"]


def test_mute_refused_key_is_not_ok() -> None:
    # FireTv.lean code_violates_refused_key: the key is refused (rc != 0) and the readback has no Muted: line.
    t, fake = tv(LossyMuteTv("refused", "no_muted"))
    out = run(t.control("mute"))
    assert fake.muted is False and fake.sent() == ["input keyevent 164"]
    assert out["ok"] is False and "refused" in out["line"]


def test_mute_dropped_key_with_a_readback_says_so() -> None:
    # positive control for the readback: the key is answered but lost, the TV reads back unmuted: not ok
    t, fake = tv(LossyMuteTv("dropped", "read"))
    out = run(t.control("mute"))
    assert out["ok"] is False and "still reports unmuted" in out["line"]


# ---- apps, search, typing ----

def test_open_app_by_alias_pins_the_component() -> None:
    t, fake = tv()
    out = run(t.control("open_app", app="Amazon Prime"))
    assert out["ok"] and out["line"] == "Opened Prime Video."
    assert fake.sent() == ["am start -n com.amazon.firebat/com.amazon.firebatcore.deeplink.DeepLinkRoutingActivity"]


def test_open_app_says_what_it_interrupted() -> None:
    t, fake = tv()
    fake.focus = YT_FOCUS
    out = run(t.control("open_app", app="netflix"))
    assert out["interrupted"] == "YouTube" and "YouTube was on" in out["line"]


def test_open_app_wakes_a_sleeping_tv_first() -> None:
    t, fake = tv()
    fake.wake, fake.display = "Asleep", "OFF"
    out = run(t.control("open_app", app="Disney+"))
    assert out["line"].startswith("Turned the TV on. Opened Disney+")
    assert fake.sent()[0] == "input keyevent 224"


def test_open_app_reresolves_when_the_activity_moved() -> None:
    t, fake = tv()
    old = F.find_app("netflix").component  # type: ignore[union-attr]
    fake.broken.add(old)
    fake.resolve["com.netflix.ninja"] = "com.netflix.ninja/.NewMainActivity"
    out = run(t.control("open_app", app="Netflix"))
    assert out["ok"] and fake.sent()[-1] == "am start -n com.netflix.ninja/.NewMainActivity"
    assert any("LEANBACK_LAUNCHER com.netflix.ninja" in s for s in fake.sent())


def test_open_unknown_app_by_installed_package() -> None:
    t, fake = tv()
    fake.resolve["com.example.weatherly"] = "com.example.weatherly/.TvActivity"
    out = run(t.control("open_app", app="Weatherly"))
    assert out["ok"] and fake.sent()[-1] == "am start -n com.example.weatherly/.TvActivity"


def test_open_unknown_app_not_installed_or_ambiguous() -> None:
    t, fake = tv()
    assert "no app called" in run(t.control("open_app", app="Tubi"))["line"]
    assert "More than one" in run(t.control("open_app", app="coolgame"))["line"]
    assert not any(s.startswith("am start") for s in fake.sent())


def test_youtube_search_encodes_and_pins_youtube() -> None:
    t, fake = tv()
    out = run(t.control("youtube_search", query="lofi beats & rain 'live'"))
    assert out["ok"]
    words = shlex.split(fake.sent()[-1])
    assert words[:2] == ["am", "start"] and words[words.index("-n") + 1] == F.YOUTUBE.component
    url = words[words.index("-d") + 1]
    assert url == "https://www.youtube.com/results?search_query=lofi+beats+%26+rain+%27live%27"


def test_type_text_escapes_spaces_and_quotes_for_the_remote_shell() -> None:
    t, fake = tv()
    out = run(t.control("type_text", text="it's a \"test\"; rm -rf $HOME"))
    assert out["ok"] and out["line"] == "Typed 27 characters on the TV."
    words = shlex.split(fake.sent()[-1])
    assert words == ["input", "text", "it's%sa%s\"test\";%srm%s-rf%s$HOME"]
    assert fake.sent()[-1].startswith("input text '")             # one quoted word: the TV's shell runs nothing


@pytest.mark.parametrize("text,why", [("café", "plain English"), ("x" * 201, "too long"), ("", "Nothing"),
                                      ("line\nbreak", "plain English")])
def test_type_text_refuses(text: str, why: str) -> None:
    t, fake = tv()
    out = run(t.control("type_text", text=text))
    assert out["ok"] is False and why in out["line"] and fake.calls == []


# ---- the screenshot ----

def test_screenshot_saves_a_png(tmp_path: Path) -> None:
    t, fake = tv()
    out = run(t.screenshot(out_dir=tmp_path))
    assert out["ok"] and Path(out["path"]).read_bytes().startswith(F.PNG_SIGNATURE)
    assert ["adb", "-s", SERIAL, "exec-out", "screencap", "-p"] in fake.calls


def test_screenshot_rejects_what_is_not_a_png(tmp_path: Path) -> None:
    t, fake = tv()
    fake.png = b"error: something\n"
    out = run(t.screenshot(out_dir=tmp_path))
    assert out["ok"] is False and list(tmp_path.iterdir()) == []


# ---- the code path ----

@pytest.mark.parametrize("text,cmd", [
    ("turn on the tv", F.Command("power_on")),
    ("Turn the TV off", F.Command("power_off")),
    ("tv on", F.Command("power_on")),
    ("tv off", F.Command("power_off")),
    ("hey eve, turn on the fire tv please", F.Command("power_on")),
    ("switch off the television", F.Command("power_off")),
    ("tv volume up", F.Command("volume", change=10)),
    ("turn the tv up", F.Command("volume", change=10)),
    ("turn the tv down a bit", F.Command("volume", change=-5)),
    ("turn the tv down", F.Command("volume", change=-10)),
    ("tv volume 30", F.Command("volume", volume=30)),
    ("set the tv volume to 30", F.Command("volume", volume=30)),
    ("set the volume on the tv to 25%", F.Command("volume", volume=25)),
    ("mute the tv", F.Command("mute")),
    ("unmute the tv", F.Command("unmute")),
    ("pause the tv", F.Command("pause")),
    ("play the tv", F.Command("play")),
    ("resume the tv", F.Command("play")),
    ("tv home", F.Command("home")),
    ("go home on the tv", F.Command("home")),
    ("tv down 3 times", F.Command("down", repeat=3)),
    ("tv up twice", F.Command("up", repeat=2)),
    ("tv left", F.Command("left")),
    ("press up on the tv", F.Command("up")),
    ("what's on fire tv", F.Command("status")),
    ("press ok on the tv", F.Command("select")),
    ("open youtube on the tv", F.Command("open_app", app="youtube")),
    ("put netflix on the tv", F.Command("open_app", app="netflix")),
    ("put disney+ on the telly", F.Command("open_app", app="disney plus")),
    ("search lofi beats on youtube on the tv", F.Command("youtube_search", query="lofi beats")),
    ("what's on the tv", F.Command("status")),
    ("is the tv on?", F.Command("status")),
    ("tv status", F.Command("status")),
])
def test_match(text: str, cmd: F.Command) -> None:
    assert F.match(text) == cmd
    assert tv()[0].match(text) == cmd


@pytest.mark.parametrize("text", [
    "pause", "stop", "pause the music", "lights off", "play music on the tv", "put the music on the tv",
    "play spotify on the tv", "turn on the lights", "what is a tv", "open youtube", "play daft punk on spotify",
    "play daft punk on the tv", "turn on the tv in ten minutes", "tv volume 400", "volume up", "mute",
    "why is the tv so loud", "tv up", "tv down", "what's on tv", "what is on tv tonight",
])
def test_match_leaves_the_rest_to_the_model(text: str) -> None:
    assert F.match(text) is None


def test_run_by_code_says_what_happened() -> None:
    t, _ = tv()
    assert run(t.run(F.Command("status"))) == "TV on — home, volume 44%"
    assert run(t.run(F.Command("volume", volume=20))) == "TV volume 20%."
    assert run(t.run(F.Command("down", repeat=2))) == "Pressed down 2 times."


# ---- the tools ----

def test_tools_are_strict_and_complete() -> None:
    for t in F.TOOLS:
        params = t["parameters"]
        assert t["strict"] is True and params["additionalProperties"] is False
        assert sorted(params["required"]) == sorted(params["properties"])
    assert F.TOOL_NAMES == ("firetv_status", "firetv_control", "firetv_screenshot")
    actions = F.TOOLS[1]["parameters"]["properties"]["action"]["enum"]
    assert "stop" not in actions and {"power_on", "type_text", "youtube_search"} <= set(actions)
    assert json.loads(json.dumps(F.TOOLS)) == tv()[0].tools()


def test_handle_routes(tmp_path: Path) -> None:
    t, fake = tv()
    t.shots_dir = tmp_path
    assert run(t.handle("firetv_status", {}))["volume"] == 44
    args = {"action": "volume", "app": None, "query": None, "text": None, "volume": None, "volume_change": -10,
            "repeat": None}
    assert run(t.handle("firetv_control", args))["volume"] == 34
    args = {**args, "action": "up", "volume_change": None, "repeat": 2}
    assert run(t.handle("firetv_control", args))["line"] == "Pressed up 2 times."
    shot = run(t.handle("firetv_screenshot", {}))
    assert set(shot) == {"ok", "path", "shows"} and shot["ok"]
    assert shot["shows"] == "TV on — home, volume 34%"       # the model can't see the picture: it gets this
    assert run(t.handle("firetv_control", {**args, "action": "explode"}))["ok"] is False
    assert run(t.handle("nope", {}))["ok"] is False


def test_instructions_name_the_limits() -> None:
    text = tv()[0].instructions()
    assert "HDMI" in text and "interrupted" in text and "firetv_screenshot" in text and "don't see it" in text
    voice = tv()[0].instructions(screenshot=False)
    assert "HDMI" in voice and "screenshot" not in voice


# ---- construction and the CLI ----

def test_make_firetv_off_switch_and_not_paired(tmp_path: Path) -> None:
    p = tmp_path / "firetv.json"
    assert F.make_firetv(p, environ={}) is None
    F.save({"host": HOST, "port": 5555, "mdns": MDNS}, p)
    for off in ("0", "false", "no", "OFF"):
        assert F.make_firetv(p, environ={"EVE_FIRETV": off}) is None
    hub = F.make_firetv(p, environ={"EVE_ADB": "/x/adb"}, runner=FakeAdb())
    assert isinstance(hub, F.FireTV) and hub.adb == "/x/adb"
    assert F.enabled({}) and not F.enabled({"EVE_FIRETV": "0"})


def test_config_is_written_private(tmp_path: Path) -> None:
    p = F.save({"host": HOST, "port": 5555}, tmp_path / "sub" / "firetv.json")
    assert stat.S_IMODE(p.stat().st_mode) == 0o600
    assert F.load(p) == {"host": HOST, "port": 5555, "mdns": ""}
    (tmp_path / "bad.json").write_text("{}")
    assert F.load(tmp_path / "bad.json") == {}


def test_cli_pair_by_mdns_then_status(tmp_path: Path) -> None:
    p = tmp_path / "firetv.json"
    fake = FakeAdb()
    lines: list[str] = []
    env = {"EVE_ADB": "adb"}
    assert F.cli(["status"], path=p, out=lines.append, runner=fake, environ=env) == 1 and "firetv pair" in lines[0]
    lines.clear()
    assert F.cli(["pair"], path=p, out=lines.append, runner=fake, environ=env) == 0
    saved = F.load(p)
    assert saved["host"] == HOST and saved["mdns"] == MDNS and saved["model"] == "AFTTEST1"
    assert stat.S_IMODE(p.stat().st_mode) == 0o600
    lines.clear()
    assert F.cli(["status"], path=p, out=lines.append, runner=fake, environ=env) == 0
    assert lines == ["TV on — home, volume 44%", f"AFTTEST1 at {SERIAL}"]
    assert F.cli(["forget"], path=p, out=lines.append) == 0 and not p.exists()


def test_cli_pair_unauthorized_and_by_host(tmp_path: Path) -> None:
    p = tmp_path / "firetv.json"
    fake = FakeAdb()
    fake.authorized = False
    lines: list[str] = []
    env = {"EVE_ADB": "adb"}
    assert F.cli(["pair"], path=p, out=lines.append, runner=fake, environ=env) == 1
    assert "Allow USB debugging" in lines[-1] and not p.exists()
    fake.authorized = True
    fake.mdns_rows = []
    assert F.cli(["pair", "--host", HOST], path=p, out=lines.append, runner=fake, environ=env) == 0
    assert F.load(p)["mdns"] == "adb-TESTSERIAL0001"              # from ro.serialno when mDNS is quiet


def test_cli_keys_volume_and_open(tmp_path: Path) -> None:
    p = tmp_path / "firetv.json"
    F.save({"host": HOST, "port": 5555, "mdns": MDNS}, p)
    fake = FakeAdb()
    lines: list[str] = []
    env = {"EVE_ADB": "adb"}
    assert F.cli(["key", "down", "--times", "2"], path=p, out=lines.append, runner=fake, environ=env) == 0
    assert F.cli(["volume", "up"], path=p, out=lines.append, runner=fake, environ=env) == 0
    assert F.cli(["open", "prime", "video"], path=p, out=lines.append, runner=fake, environ=env) == 0
    assert F.cli(["key", "power"], path=p, out=lines.append, runner=fake, environ=env) == 1
    assert lines[:3] == ["Pressed down 2 times.", "TV volume 54%.", "Opened Prime Video."]


# ---- guards ----

def test_never_a_raw_binder_call() -> None:
    needle = "service" + " " + "call"
    src = inspect.getsource(F)
    assert needle not in src.lower()


def test_never_the_power_toggle_key() -> None:
    src = inspect.getsource(F)
    assert not re.search(r"keyevent['\",\s]+26\b", src) and "KEYCODE_POWER" not in src
    t, fake = tv()
    fake.wake, fake.display = "Asleep", "OFF"
    for action in ("power_on", "power_off", "power_on", "home", "mute", "unmute"):
        run(t.control(action))
    sent = " | ".join(fake.sent())
    assert not re.search(r"keyevent(?: \d+)* 26(?: |$)", sent)
    assert "input keyevent 224" in sent and "input keyevent 223" in sent


# ---- the Mini App's remote (FireTV.panel) ----

def test_panel_status_carries_the_state_the_line_and_the_app_list() -> None:
    t, fake = tv()
    fake.focus = YT_FOCUS
    out = run(t.panel({"action": "status"}))
    assert out["ok"] and out["power"] == "on" and out["app"] == "YouTube" and out["volume"] == 44
    assert out["muted"] is False and out["now"] == "TV on — YouTube, volume 44%"
    assert out["apps"] == [a.name for a in F.APPS] and out["apps"][:4] == ["YouTube", "Netflix", "Prime Video",
                                                                           "Disney+"]
    assert fake.sent() == []                                         # a status sends nothing to the TV


def test_panel_no_action_is_a_status() -> None:
    t, fake = tv()
    assert run(t.panel({}))["now"] == "TV on — home, volume 44%" and fake.sent() == []


@pytest.mark.parametrize("action,sent", [("up", "input keyevent 19"), ("down", "input keyevent 20"),
                                         ("left", "input keyevent 21"), ("right", "input keyevent 22"),
                                         ("select", "input keyevent 23"), ("back", "input keyevent 4"),
                                         ("home", "input keyevent 3"), ("menu", "input keyevent 82"),
                                         ("play_pause", "cmd media_session dispatch play-pause"),
                                         ("rewind", "cmd media_session dispatch rewind"),
                                         ("fast_forward", "cmd media_session dispatch fast-forward")])
def test_panel_keys_go_through_control_and_answer_with_fresh_state(action: str, sent: str) -> None:
    t, fake = tv()
    out = run(t.panel({"action": action}))
    assert out["ok"] and out["line"] and out["now"] == "TV on — home, volume 44%"
    assert fake.sent() == [sent]
    assert fake.shells()[-1] == F.STATE_SCRIPT                       # the state is read AFTER the key


def test_panel_dpad_taps_with_a_count_are_one_input_call_capped() -> None:
    t, fake = tv()
    assert run(t.panel({"action": "down", "repeat": 3}))["line"] == "Pressed down 3 times."
    run(t.panel({"action": "right", "repeat": 99}))
    run(t.panel({"action": "play_pause", "repeat": 5}))           # only the d-pad repeats
    assert fake.sent() == ["input keyevent 20 20 20", "input keyevent " + " ".join(["22"] * F.MAX_REPEAT),
                           "cmd media_session dispatch play-pause"]


def test_panel_power_on_and_off() -> None:
    t, fake = tv()
    fake.wake, fake.display = "Asleep", "OFF"
    on = run(t.panel({"action": "power_on"}))
    assert on["ok"] and on["power"] == "on" and on["now"].startswith("TV on")
    off = run(t.panel({"action": "power_off"}))
    assert off["ok"] and off["power"] == "standby" and off["line"] == "TV off."
    assert fake.sent() == [f"input keyevent {F.KEYCODE_WAKEUP}", f"input keyevent {F.KEYCODE_SLEEP}"]


def test_panel_volume_from_the_slider_and_the_buttons() -> None:
    t, fake = tv()
    fake.max_index = 15
    fake.volume = 6
    out = run(t.panel({"action": "volume", "volume": 60}))
    assert out["ok"] and out["volume"] == 60 and out["now"] == "TV on — home, volume 60%"
    assert fake.sent() == ["cmd media_session volume --stream 3 --set 9"]
    fake.max_index, fake.volume = 100, 44
    assert run(t.panel({"action": "volume_change", "change": 10}))["volume"] == 54
    assert run(t.panel({"action": "volume_change", "change": -10}))["volume"] == 44
    assert run(t.panel({"action": "volume", "volume": 250}))["volume"] == 100     # clamped, never past the top


@pytest.mark.parametrize("body,line", [({"action": "volume"}, "What volume?"),
                                       ({"action": "volume", "volume": True}, "What volume?"),
                                       ({"action": "volume", "volume": "60"}, "What volume?"),
                                       ({"action": "volume_change"}, "Louder or quieter?"),
                                       ({"action": "volume_change", "change": 0}, "Louder or quieter?"),
                                       ({"action": "open_app", "app": "com.example.coolgame"},
                                        "That app isn't on the remote."),
                                       ({"action": "open_app"}, "That app isn't on the remote."),
                                       ({"action": "type_text", "text": "hello"}, "Not something the TV remote does."),
                                       ({"action": "youtube_search", "query": "cats"},
                                        "Not something the TV remote does."),
                                       ({"action": "next"}, "Not something the TV remote does.")])
def test_panel_refuses_what_a_remote_card_does_not_send(body: dict, line: str) -> None:
    t, fake = tv()
    out = run(t.panel(body))
    assert out["ok"] is False and out["line"] == line and out["apps"]
    assert fake.calls == []                                          # not even a connect


def test_panel_mute_and_unmute_answer_with_the_read_back() -> None:
    t, fake = tv()
    out = run(t.panel({"action": "mute"}))
    assert out["ok"] and out["muted"] is True and out["now"] == "TV on — home, volume 44% (muted)"
    assert run(t.panel({"action": "mute"}))["line"] == "The TV is already muted."
    out = run(t.panel({"action": "unmute"}))
    assert out["ok"] and out["muted"] is False
    assert fake.sent() == [f"input keyevent {F.KEYCODE_VOLUME_MUTE}"] * 2


def test_panel_open_app_by_the_tables_name_only() -> None:
    t, fake = tv()
    out = run(t.panel({"action": "open_app", "app": "Disney+"}))
    assert out["ok"] and out["line"] == "Opened Disney+." and out["app"] == "Disney+"
    assert fake.sent() == ["am start -n com.disney.disneyplus/com.bamtechmedia.dominguez.main.MainActivity"]


def test_panel_a_gone_tv_says_so_without_a_second_wait() -> None:
    t, fake = tv()
    fake.at = "192.0.2.99"                                           # the TV answers nowhere we know
    fake.mdns_rows = []
    out = run(t.panel({"action": "up"}))
    assert out == {"ok": False, "line": F.UNREACHABLE_LINE, "apps": [a.name for a in F.APPS]}
    connects = [c for c in fake.calls if c[1:2] == ["connect"]]
    assert len(connects) == 2                                        # the press's own try and its one retry; no state read
    status = run(t.panel({"action": "status"}))
    assert status["ok"] is False and status["now"] is None and status["line"] == F.UNREACHABLE_LINE


def test_panel_a_failed_action_still_shows_the_fresh_state() -> None:
    t, fake = tv()
    fake.volume = None                                               # the volume can't be read: nothing is set
    out = run(t.panel({"action": "volume_change", "change": 10}))
    assert out["ok"] is False and out["line"] == "I couldn't read the TV's volume, so I didn't change it."
    assert out["power"] == "on" and out["now"] == "TV on — home, volume unknown"
    assert fake.sent() == []


# ---- the stale adb server (2026-10-08: "No route to host" for hours while the TV was on) ----------------------

@pytest.fixture(autouse=True)
def _no_real_probe(monkeypatch: pytest.MonkeyPatch) -> None:
    async def silent(host: str, port: int, timeout: float) -> bool:
        return False
    monkeypatch.setattr(F, "tcp_probe", silent)


def _stale_server_tv(probe_answers: bool, clock: Optional[list[float]] = None):
    calls: list[list[str]] = []
    state = {"restarted": False}

    async def runner(argv: list[str], timeout: float) -> tuple[int, str]:
        calls.append(argv)
        if argv[1] == "kill-server":
            state["restarted"] = True
            return 0, ""
        if argv[1] == "connect":
            if state["restarted"]:
                return 0, f"connected to {SERIAL}"
            return 0, f"failed to connect to '{SERIAL}': No route to host"
        if argv[1] == "mdns":
            return 0, "List of discovered mdns services\n"
        if argv[1] == "-s" and argv[3] == "get-state":
            return 0, "device\n"
        return 0, ""

    async def probe(host: str, port: int, timeout: float) -> bool:
        return probe_answers

    now = clock if clock is not None else [1000.0]
    t = F.FireTV({"host": HOST, "port": 5555, "mdns": MDNS}, runner=runner, adb="adb", sleep=_no_sleep,
                 probe=probe, clock=lambda: now[0])
    return t, calls, state


def test_a_stale_adb_server_is_restarted_once_when_the_tv_port_answers() -> None:
    t, calls, _ = _stale_server_tv(probe_answers=True)
    r = run(t.ensure())
    assert r.ok
    verbs = [c[1] for c in calls]
    assert verbs.count("kill-server") == 1 and "start-server" in verbs
    assert verbs.index("kill-server") > verbs.index("disconnect")       # only after the normal recovery failed


def test_the_adb_server_is_left_alone_when_the_tv_itself_is_gone() -> None:
    t, calls, _ = _stale_server_tv(probe_answers=False)
    r = run(t.ensure())
    assert not r.ok and r.why == "unreachable"
    assert not any(c[1] in ("kill-server", "start-server") for c in calls)


def test_the_adb_server_is_restarted_at_most_once_per_ten_minutes() -> None:
    clock = [1000.0]
    t, calls, state = _stale_server_tv(probe_answers=True, clock=clock)
    assert run(t.ensure()).ok
    state["restarted"] = False                       # the server goes stale again
    t._ready = False
    clock[0] += 60
    assert not run(t.ensure()).ok
    assert [c[1] for c in calls].count("kill-server") == 1
    clock[0] += F.ADB_RESTART_EVERY
    assert run(t.ensure()).ok
    assert [c[1] for c in calls].count("kill-server") == 2


def test_an_unauthorized_tv_never_restarts_the_server() -> None:
    async def runner(argv: list[str], timeout: float) -> tuple[int, str]:
        if argv[1] == "connect":
            return 0, f"failed to authenticate to {SERIAL}"
        return 0, ""

    async def probe(host: str, port: int, timeout: float) -> bool:
        raise AssertionError("probed an unauthorized TV")

    t = F.FireTV({"host": HOST, "port": 5555}, runner=runner, adb="adb", sleep=_no_sleep, probe=probe)
    assert run(t.ensure()).why == "unauthorized"


def test_the_real_probe_tells_an_open_port_from_a_closed_one() -> None:
    async def go() -> tuple[bool, bool]:
        server = await asyncio.start_server(lambda r, w: w.close(), "127.0.0.1", 0)
        port = server.sockets[0].getsockname()[1]
        try:
            opened = await _REAL_PROBE("127.0.0.1", port, 1.0)
        finally:
            server.close()
            await server.wait_closed()
        closed = await _REAL_PROBE("127.0.0.1", port, 1.0)
        return opened, closed

    assert run(go()) == (True, False)


_REAL_PROBE = F.tcp_probe          # taken at import, before the autouse fixture swaps it out
