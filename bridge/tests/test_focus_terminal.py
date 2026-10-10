import asyncio

from eve_bridge import focus_terminal as ft
from eve_bridge.focus_terminal import _DEFAULT_ACTIVATE_TARGETS, activate_targets


def test_default_targets_without_env(monkeypatch):
    monkeypatch.delenv("EVE_FOCUS_APPS", raising=False)
    assert activate_targets() == _DEFAULT_ACTIVATE_TARGETS


def test_env_overrides_order_and_keeps_known_bundle_ids(monkeypatch):
    monkeypatch.setenv("EVE_FOCUS_APPS", "cmux, Warp, MyTerm")
    assert activate_targets() == [
        ("cmux", "com.cmuxterm.app"),      # known → keeps its bundle id
        ("Warp", "dev.warp.Warp"),
        ("MyTerm", None),                  # unknown → name activation
    ]


def test_env_matches_known_names_case_insensitively(monkeypatch):
    monkeypatch.setenv("EVE_FOCUS_APPS", "warp,composer")
    assert activate_targets() == [
        ("Warp", "dev.warp.Warp"),
        ("Composer", None),
    ]


def test_env_blank_or_garbage_falls_back_to_defaults(monkeypatch):
    monkeypatch.setenv("EVE_FOCUS_APPS", "  ")
    assert activate_targets() == _DEFAULT_ACTIVATE_TARGETS
    monkeypatch.setenv("EVE_FOCUS_APPS", ", ,")
    assert activate_targets() == _DEFAULT_ACTIVATE_TARGETS


# ---- focus_session_terminal decision flow ------------------------------------
#
# The scripts are stubbed at the two seams the module already has —
# _app_running (pgrep) and _osascript — so each test states which apps are
# running and what System Events / the app would answer, then asserts what
# was raised. Scripts are identified by a distinctive substring.


CWD = "/Users/someone/code/claude-pet"


def _script_kind(script: str) -> str:
    if 'tell application "iTerm2"' in script:
        return "iterm"
    if 'tell application "Terminal"' in script:
        return "terminal"
    if "AXRaise" in script and "needle" in script:
        return "ax-raise"
    if "AXMinimized" in script and "no-windows" in script:
        return "unminimize"
    if "tell application id" in script:
        return "activate-id"
    if "tell application (item 1 of argv)" in script:
        return "activate-name"
    raise AssertionError(f"unknown script:\n{script}")


class Harness:
    """running: process names pgrep would find. answers: kind -> reply."""

    def __init__(self, running: set[str], answers: dict[str, str | None]) -> None:
        self.running = running
        self.answers = answers
        self.calls: list[tuple[str, tuple[str, ...]]] = []

    async def app_running(self, name: str) -> bool:
        return name in self.running

    async def osascript(self, script: str, *args: str) -> str | None:
        kind = _script_kind(script)
        self.calls.append((kind, args))
        return self.answers.get(kind)

    def kinds(self) -> list[str]:
        return [k for k, _ in self.calls]


def _run(monkeypatch, running: set[str], answers: dict[str, str | None], cwd: str = CWD) -> Harness:
    h = Harness(running, answers)
    monkeypatch.delenv("EVE_FOCUS_APPS", raising=False)
    monkeypatch.setattr(ft, "_app_running", h.app_running)
    monkeypatch.setattr(ft, "_osascript", h.osascript)
    asyncio.run(ft.focus_session_terminal(cwd))
    return h


def test_nothing_running_makes_no_osascript_calls(monkeypatch) -> None:
    h = _run(monkeypatch, set(), {})
    assert h.calls == []


def test_iterm_match_wins_and_stops(monkeypatch) -> None:
    h = _run(monkeypatch, {"iTerm2", "stable"}, {"iterm": "matched"})
    assert h.kinds() == ["iterm"]
    assert h.calls[0][1] == ("claude-pet",)


def test_window_title_match_raises_that_window(monkeypatch) -> None:
    h = _run(monkeypatch, {"stable"}, {"ax-raise": "raised"})
    assert h.kinds() == ["ax-raise"]
    assert h.calls[0][1] == ("stable", "claude-pet")


def test_frontmost_with_visible_window_stands_down(monkeypatch) -> None:
    h = _run(monkeypatch, {"stable"}, {"ax-raise": "frontmost", "unminimize": "visible"})
    assert h.kinds() == ["ax-raise", "unminimize"]


def test_frontmost_but_minimized_restores_the_window(monkeypatch) -> None:
    # The 2026-09-05 bug: Warp active, its only window in the Dock, every tap
    # logged "already frontmost — assuming it's on screen" and did nothing.
    h = _run(monkeypatch, {"stable"}, {"ax-raise": "frontmost", "unminimize": "unminimized"})
    assert h.kinds() == ["ax-raise", "unminimize"]
    assert h.calls[1][1] == ("stable",)


def test_frontmost_with_no_window_on_this_space_activates_by_bundle_id(monkeypatch) -> None:
    h = _run(monkeypatch, {"stable"},
             {"ax-raise": "frontmost", "unminimize": "no-windows", "activate-id": "activated"})
    assert h.kinds() == ["ax-raise", "unminimize", "activate-id"]
    assert h.calls[2][1] == ("dev.warp.Warp-Stable",)


def test_frontmost_ax_error_still_activates(monkeypatch) -> None:
    h = _run(monkeypatch, {"stable"},
             {"ax-raise": "frontmost", "unminimize": None, "activate-id": "activated"})
    assert h.kinds() == ["ax-raise", "unminimize", "activate-id"]


def test_frontmost_name_only_app_activates_by_name(monkeypatch) -> None:
    h = _run(monkeypatch, {"Composer"},
             {"ax-raise": "frontmost", "unminimize": "no-windows", "activate-name": "activated"})
    assert h.kinds() == ["ax-raise", "unminimize", "activate-name"]
    assert h.calls[2][1] == ("Composer",)


def test_not_frontmost_no_match_activates_first_running_and_unminimizes(monkeypatch) -> None:
    h = _run(monkeypatch, {"stable", "cmux"},
             {"ax-raise": "no-match", "activate-id": "activated", "unminimize": "visible"})
    # one AX pass per running app, then activate the first, then the Dock check
    assert h.kinds() == ["ax-raise", "ax-raise", "activate-id", "unminimize"]
    assert h.calls[2][1] == ("dev.warp.Warp-Stable",)


def test_second_app_title_match_beats_first_app_activate(monkeypatch) -> None:
    class Ordered(Harness):
        async def osascript(self, script: str, *args: str) -> str | None:
            kind = _script_kind(script)
            self.calls.append((kind, args))
            if kind == "ax-raise":
                return "raised" if args[0] == "cmux" else "no-match"
            return self.answers.get(kind)

    h = Ordered({"stable", "cmux"}, {})
    monkeypatch.delenv("EVE_FOCUS_APPS", raising=False)
    monkeypatch.setattr(ft, "_app_running", h.app_running)
    monkeypatch.setattr(ft, "_osascript", h.osascript)
    asyncio.run(ft.focus_session_terminal(CWD))
    assert h.kinds() == ["ax-raise", "ax-raise"]


def test_empty_cwd_skips_title_matching(monkeypatch) -> None:
    h = _run(monkeypatch, {"stable"}, {"activate-id": "activated", "unminimize": "visible"}, cwd="")
    assert h.kinds() == ["activate-id", "unminimize"]
