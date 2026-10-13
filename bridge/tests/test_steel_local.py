"""eve's browser on local Steel (steel_local.py): the lane's Steel mode, the server start, sessions and release."""

from __future__ import annotations

import asyncio
import os
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import pytest

from eve_bridge import steel_local as sl
from eve_bridge.browser_lane import AttachError, BrowserLane, BrowserLaneConfig, configured


class FakeSteel:
    """Answers steel_local's HTTP calls: /v1/health, /v1/sessions, /v1/sessions/release."""

    def __init__(self, up: bool = True, ws: str = "ws://127.0.0.1:3000/") -> None:
        self.up, self.ws = up, ws
        self.calls: list[tuple[str, str, Any]] = []

    def __call__(self, method: str, url: str, body: Any = None, timeout: float = 0.0) -> dict:
        self.calls.append((method, url.split("/v1/", 1)[-1], body))
        if not self.up:
            raise OSError("connection refused")
        if url.endswith("/v1/health"):
            return {"status": "ok"}
        if url.endswith("/v1/sessions"):
            return {"id": "00000000-0000-4000-8000-000000000001", "websocketUrl": self.ws}
        return {"success": True}


def cfg(tmp_path: Path, url: str = sl.DEFAULT_URL) -> sl.SteelConfig:
    return sl.SteelConfig(url=url, steel_dir=tmp_path / "steel-browser", profile=tmp_path / "profile")


# ---- configuration --------------------------------------------------------------------------------------

def test_steel_is_off_unless_asked() -> None:
    c = configured({})
    assert not c.steel and not c.attach


def test_steel_turns_the_lane_on_and_wins_over_own_and_attach() -> None:
    c = configured({"EVE_BROWSER_STEEL": "1", "EVE_BROWSER_OWN": "1", "EVE_BROWSER_ATTACH": "1"})
    assert c.steel and c.attach and not c.own
    assert configured({"EVE_BROWSER_OWN": "1"}).own                          # own mode is unchanged without it


def test_steel_settings_have_local_defaults_and_can_be_moved() -> None:
    d = sl.configured({})
    assert d.url == "http://127.0.0.1:3000" and d.profile == Path("~/.config/eve-bridge/steel-profile").expanduser()
    m = sl.configured({"EVE_STEEL_URL": "http://localhost:3100/", "EVE_STEEL_DIR": "/opt/steel",
                       "EVE_STEEL_PROFILE": "/tmp/p"})
    assert (m.url, m.steel_dir, m.profile) == ("http://localhost:3100", Path("/opt/steel"), Path("/tmp/p"))


# ---- the server -----------------------------------------------------------------------------------------

def test_a_running_server_is_used_and_nothing_is_started(tmp_path: Path) -> None:
    started: list[Any] = []
    sl.ensure_server(cfg(tmp_path), http=FakeSteel(), popen=lambda *a, **k: started.append(a))
    assert started == []


def test_a_server_that_is_down_is_started_on_loopback_with_eves_profile(tmp_path: Path) -> None:
    c = cfg(tmp_path)
    c.steel_dir.mkdir()
    (c.steel_dir / sl.LAUNCHER).write_text("#!/bin/sh\n")
    steel = FakeSteel(up=False)
    started: list[dict] = []

    def popen(argv: list[str], **kw: Any) -> None:
        started.append({"argv": argv, **kw})
        steel.up = True                                                       # it comes up after the start

    sl.ensure_server(c, http=steel, popen=popen, sleep=lambda s: None)
    (s,) = started
    assert s["argv"] == [str(c.steel_dir / sl.LAUNCHER)] and s["start_new_session"] is True
    assert s["env"]["HOST"] == "127.0.0.1" and s["env"]["PORT"] == "3000"
    assert s["env"]["CHROME_USER_DATA_DIR"] == str(c.profile)
    assert c.profile.is_dir() and (c.profile.stat().st_mode & 0o777) == 0o700


def test_a_server_that_never_answers_is_an_error_naming_the_log(tmp_path: Path) -> None:
    c = cfg(tmp_path)
    c.steel_dir.mkdir()
    (c.steel_dir / sl.LAUNCHER).write_text("#!/bin/sh\n")
    t = iter(range(0, 1000))
    with pytest.raises(sl.SteelError, match="eve-steel.log"):
        sl.ensure_server(c, http=FakeSteel(up=False), popen=lambda *a, **k: None, sleep=lambda s: None,
                         clock=lambda: float(next(t)), wait_secs=5)


def test_no_checkout_means_a_plain_error_and_no_start(tmp_path: Path) -> None:
    started: list[Any] = []
    with pytest.raises(sl.SteelError, match="docs/steel.md"):
        sl.ensure_server(cfg(tmp_path), http=FakeSteel(up=False), popen=lambda *a, **k: started.append(a))
    assert started == []


@pytest.mark.parametrize("url", ["http://192.168.1.20:3000", "https://api.steel.dev"])
def test_steel_off_this_mac_is_refused_before_any_request(tmp_path: Path, url: str) -> None:
    steel = FakeSteel()
    with pytest.raises(sl.SteelError, match="must be on this Mac"):
        sl.ensure_server(cfg(tmp_path, url), http=steel)
    assert steel.calls == []


# ---- sessions -------------------------------------------------------------------------------------------

def test_a_session_returns_its_local_websocket(tmp_path: Path) -> None:
    steel = FakeSteel()
    assert sl.start_session(cfg(tmp_path), http=steel) == "ws://127.0.0.1:3000/"
    assert ("POST", "sessions", {"blockAds": True}) in steel.calls


def test_a_visible_session_asks_for_a_window(tmp_path: Path) -> None:
    steel = FakeSteel()
    sl.start_session(cfg(tmp_path), http=steel, visible=True)
    assert ("POST", "sessions", {"blockAds": True, "headless": False}) in steel.calls


@pytest.mark.parametrize("ws", ["", "http://127.0.0.1:3000/", "ws://203.0.113.9:3000/"])
def test_a_session_without_a_local_websocket_is_refused(tmp_path: Path, ws: str) -> None:
    with pytest.raises(sl.SteelError, match="websocket"):
        sl.start_session(cfg(tmp_path), http=FakeSteel(ws=ws))


def test_release_is_quiet_when_steel_is_gone(tmp_path: Path) -> None:
    sl.release(cfg(tmp_path), http=FakeSteel(up=False))                      # no exception


# ---- the lane -------------------------------------------------------------------------------------------

def test_the_lane_attaches_to_the_steel_session(monkeypatch: pytest.MonkeyPatch) -> None:
    import playwright.sync_api as psa

    seen: dict[str, Any] = {}
    page = SimpleNamespace(bring_to_front=lambda: None, title=lambda: "t")
    ctx = SimpleNamespace(new_page=lambda: page)
    browser = SimpleNamespace(contexts=[ctx], is_connected=lambda: True)

    def connect(endpoint: str, timeout: int) -> Any:
        seen["endpoint"] = endpoint
        return browser

    pw = SimpleNamespace(chromium=SimpleNamespace(connect_over_cdp=connect), stop=lambda: None)
    monkeypatch.setattr(psa, "sync_playwright", lambda: SimpleNamespace(start=lambda: pw))
    monkeypatch.setattr(sl, "start_session", lambda c: "ws://127.0.0.1:3000/")
    lane = BrowserLane(BrowserLaneConfig(attach=True, steel=True))
    p = lane._ensure()
    assert seen["endpoint"] == "ws://127.0.0.1:3000/" and p.page is page and lane._context is ctx


def test_steel_not_reachable_is_an_attach_error_with_its_reason(monkeypatch: pytest.MonkeyPatch) -> None:
    def down(c: Any) -> str:
        raise sl.SteelError("Steel did not answer within 40 s")

    monkeypatch.setattr(sl, "start_session", down)
    with pytest.raises(AttachError, match="did not answer"):
        BrowserLane(BrowserLaneConfig(attach=True, steel=True))._ensure()


def test_closing_the_steel_lane_closes_eves_tab_and_releases_the_session(monkeypatch: pytest.MonkeyPatch) -> None:
    calls: list[str] = []
    monkeypatch.setattr(sl, "release", lambda c: calls.append("release"))
    lane = BrowserLane(BrowserLaneConfig(attach=True, steel=True))
    lane._browser = SimpleNamespace(close=lambda: calls.append("browser.close"))
    lane._context = SimpleNamespace(close=lambda: calls.append("context.close"))
    lane._pw = SimpleNamespace(stop=lambda: calls.append("playwright.stop"))
    lane._page = SimpleNamespace(page=SimpleNamespace(close=lambda: calls.append("eve tab.close")))
    lane._close()
    assert calls == ["eve tab.close", "playwright.stop", "release"]


def test_closing_eves_own_chrome_never_touches_steel(monkeypatch: pytest.MonkeyPatch) -> None:
    calls: list[str] = []
    monkeypatch.setattr(sl, "release", lambda c: calls.append("release"))
    lane = BrowserLane(BrowserLaneConfig(attach=True, own=True))
    lane._browser = SimpleNamespace()
    lane._pw = SimpleNamespace(stop=lambda: None)
    lane._close()
    assert calls == []


def test_meet_keeps_eves_own_chrome_when_steel_drives_the_rest(tmp_path: Path) -> None:
    from eve_bridge import meet

    env = {"EVE_MEET": "1", "EVE_BROWSER_STEEL": "1"}
    meeter = meet.make_meeter(meet.configured(env), tmp_path, environ=env)
    assert meeter is not None
    cfg = meeter.make_page()._lane.config                                   # builds a page; connects nothing
    assert cfg.own and not cfg.steel and cfg.attach                           # Meet needs audio: never Steel


# ---- live: the local Steel server on this Mac -----------------------------------------------------------

@pytest.mark.live
@pytest.mark.skipif(os.environ.get("EVE_LIVE") != "1", reason="needs EVE_LIVE=1 and local Steel")
def test_live_the_lane_reads_a_page_through_local_steel_starting_it_when_down() -> None:
    c = sl.configured()
    if not (c.steel_dir / sl.LAUNCHER).is_file():
        pytest.skip(f"no steel-browser checkout at {c.steel_dir}")
    lane = BrowserLane(BrowserLaneConfig(attach=True, steel=True))

    async def go() -> dict:
        try:
            await lane.connect()
            await lane.open_url("https://example.com")
            return await lane.outline()
        finally:
            await lane.close()

    out = asyncio.run(go())
    assert out["title"] == "Example Domain" and out["site"] == "example.com"
