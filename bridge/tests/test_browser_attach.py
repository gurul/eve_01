"""Attach mode: the lane drives the owner's running Chrome through DevToolsActivePort, in its own tab only."""

from __future__ import annotations

import asyncio
import json
import subprocess
import threading
import time
import urllib.request
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import pytest

from eve_bridge import browser_lane as bl
from eve_bridge.browser_lane import (
    AttachError,
    BrowserLane,
    BrowserLaneConfig,
    configured,
    devtools_endpoint,
)


def test_endpoint_is_read_like_chrome_devtools_mcp(tmp_path: Path) -> None:
    (tmp_path / "DevToolsActivePort").write_text("64687\n/devtools/browser/600e426b-7753\n")
    assert devtools_endpoint(tmp_path) == "ws://127.0.0.1:64687/devtools/browser/600e426b-7753"


@pytest.mark.parametrize("content", ["", "64687\n", "abc\n/devtools/browser/x\n", "70000\n/devtools/browser/x\n",
                                     "64687\n/json/version\n"])
def test_a_missing_or_malformed_file_means_no_endpoint(tmp_path: Path, content: str) -> None:
    assert devtools_endpoint(tmp_path) is None                               # no file at all
    (tmp_path / "DevToolsActivePort").write_text(content)
    assert devtools_endpoint(tmp_path) is None


def test_attach_is_off_unless_asked(tmp_path: Path) -> None:
    assert configured({}).attach is False
    cfg = configured({"EVE_BROWSER_ATTACH": "1", "EVE_CHROME_DIR": str(tmp_path)})
    assert cfg.attach is True and cfg.chrome_dir == tmp_path


def test_attach_without_remote_debugging_says_how_to_switch_it_on(tmp_path: Path) -> None:
    lane = BrowserLane(BrowserLaneConfig(attach=True, chrome_dir=tmp_path))
    with pytest.raises(AttachError, match="chrome://inspect/#remote-debugging"):
        lane._ensure()


def test_closing_in_attach_mode_closes_only_eves_tab_never_the_owners_browser() -> None:
    calls: list[str] = []
    lane = BrowserLane(BrowserLaneConfig(attach=True))
    lane._browser = SimpleNamespace(close=lambda: calls.append("browser.close"))
    lane._context = SimpleNamespace(close=lambda: calls.append("context.close"))
    lane._pw = SimpleNamespace(stop=lambda: calls.append("playwright.stop"))
    lane._page = SimpleNamespace(page=SimpleNamespace(close=lambda: calls.append("eve tab.close")))
    lane._close()
    assert calls == ["eve tab.close", "playwright.stop"]               # never the context, never the browser
    assert lane._browser is lane._context is lane._pw is lane._page is None


def test_enter_is_a_key() -> None:
    assert bl.KEYS["enter"] == bl.KEYS["return"] == "Enter"


# ---- live: a stand-in Chrome, never the owner's -----------------------------------------------------

def _standin_chrome() -> str:
    root = Path.home() / "Library" / "Caches" / "ms-playwright"
    found = sorted(root.glob("chromium-*/chrome-mac*/*.app/Contents/MacOS/*"))
    return str(found[-1]) if found else ""


@pytest.mark.live
def test_live_attach_to_a_standin_chrome_works_in_its_own_tab_and_leaves_the_owners(tmp_path: Path) -> None:
    exe = _standin_chrome()
    if not exe:
        pytest.skip("Playwright's Chromium is not installed")
    page = tmp_path / "owner.html"
    page.write_text("<title>owner</title><input aria-label='Search here' type='search'>")
    proc = subprocess.Popen([exe, "--headless=new", "--remote-debugging-port=0", f"--user-data-dir={tmp_path / 'ud'}",
                             "--no-first-run", page.as_uri()], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        port_file = tmp_path / "ud" / "DevToolsActivePort"
        for _ in range(60):
            if port_file.exists() and devtools_endpoint(tmp_path / "ud"):
                break
            time.sleep(0.1)
        port = port_file.read_text().split()[0]

        def tabs() -> list[str]:
            with urllib.request.urlopen(f"http://127.0.0.1:{port}/json/list") as r:
                return sorted(t["url"] for t in json.load(r) if t["type"] == "page")

        owners = tabs()

        async def go() -> str:
            lane = BrowserLane(BrowserLaneConfig(attach=True, chrome_dir=tmp_path / "ud"))
            try:
                await lane.open_url(page.as_uri())
                assert len(tabs()) == len(owners) + 1                       # eve's own new tab

                def type_into() -> str:
                    p = lane._ensure()
                    box = next(c for c in p.snapshot().elements if c.editable)
                    return p.focus_and_type(box, "hello")
                return await lane._run(type_into)
            finally:
                await lane.close()

        said = asyncio.run(go())
        assert said.startswith("typed 5 characters")
        time.sleep(0.3)
        assert tabs() == owners                                              # the owner's tabs, untouched
        assert proc.poll() is None                                           # and their browser still running
    finally:
        proc.terminate()
        proc.wait(timeout=5)


def test_a_protected_file_falls_back_to_the_port_and_chromes_own_prompt(tmp_path: Path, monkeypatch) -> None:
    """macOS app-data protection refuses other apps a read of Chrome's folder: the endpoint is then the debugging
    port (Chrome asks the owner "Allow remote debugging?" per connection), and only while it is listening."""
    (tmp_path / "DevToolsActivePort").write_text("9222\n/devtools/browser/x\n")

    def refused(self, *a, **k):
        raise PermissionError("Operation not permitted")

    monkeypatch.setattr(Path, "read_text", refused)
    monkeypatch.setattr(bl, "_listening", lambda port: port == 9333)
    assert devtools_endpoint(tmp_path, 9333) == "ws://127.0.0.1:9333/devtools/browser"
    assert devtools_endpoint(tmp_path, 9222) is None                           # nothing listening: off


def test_the_debug_port_is_configurable() -> None:
    assert configured({}).debug_port == 9222
    assert configured({"EVE_CHROME_DEBUG_PORT": "9333"}).debug_port == 9333
    assert configured({"EVE_CHROME_DEBUG_PORT": "nope"}).debug_port == 9222


class _GrowingPage:
    """A web app that says Loading… first, then fills in."""

    def __init__(self, texts: list[str], url: str = "https://mail.google.com/mail/u/0/#inbox") -> None:
        self.texts, self.url = list(texts), url

    def inner_text(self, selector: str, timeout: int = 0) -> str:
        return self.texts.pop(0) if len(self.texts) > 1 else self.texts[0]

    def title(self) -> str:
        return "Inbox"


def _lane_on(page: _GrowingPage, monkeypatch) -> BrowserLane:
    lane = BrowserLane(BrowserLaneConfig())
    monkeypatch.setattr(lane, "_ensure", lambda: SimpleNamespace(page=page))
    monkeypatch.setattr(bl, "READ_POLL_SECS", 0.0)
    return lane


def test_page_text_waits_for_a_loading_app_to_fill_in(monkeypatch) -> None:
    full = "Inbox " + "message " * 80
    lane = _lane_on(_GrowingPage(["Loading…", "Loading… Gmail", full, full]), monkeypatch)
    got = lane._page_text()
    assert got["text"] == " ".join(full.split()) and got["signed_out"] is False


def test_page_text_notices_a_sign_in_redirect(monkeypatch) -> None:
    lane = _lane_on(_GrowingPage(["Sign in to GitHub " * 30],
                                 url="https://github.com/login?return_to=https%3A%2F%2Fgithub.com%2Fnotifications"),
                    monkeypatch)
    assert lane._page_text()["signed_out"] is True
    for url in ("https://accounts.google.com/v3/signin/identifier?x=1", "https://www.amazon.com/ap/signin?x=1"):
        assert bl.SIGN_IN.search(url)
    for url in ("https://mail.google.com/mail/u/0/#inbox", "https://news.ycombinator.com/", "https://github.com/notifications"):
        assert not bl.SIGN_IN.search(url)


# ---- which Chrome profile ----------------------------------------------------------------------------

LIST_ACCOUNTS = ('["gaia.l.a.r",[["gaia.l.a",1,"Guru","owner@work.example","https://x/photo.jpg",1,1,0,null,1,"1"],'
                 '["gaia.l.a",1,"Guru","OWNER@gmail.com","https://x/p2.jpg",0,0,0,null,1,"2"]]]')
PROFILES = {"owner@gmail.com": "personal-ctx", "owner@work.example": "work-ctx", "student@school.example": "school-ctx"}


def test_accounts_are_read_primary_first_and_deduplicated() -> None:
    assert bl.accounts_in(LIST_ACCOUNTS) == ["owner@work.example", "owner@gmail.com"]
    assert bl.accounts_in("") == [] and bl.accounts_in("[]") == []


@pytest.mark.parametrize("request_text,expected", [
    ("check my work inbox", "owner@work.example"),
    ("open canvas on my school account", "student@school.example"),
    ("how many unread in gmail", "owner@gmail.com"),
    ("use student@school.example and open canvas", "student@school.example"),
    ("what's the weather", ""),                                  # nothing named, no default: the first profile
])
def test_a_request_names_its_profile(request_text: str, expected: str) -> None:
    assert bl.profile_for(request_text, PROFILES) == expected


def test_the_configured_default_applies_only_when_it_is_open() -> None:
    assert bl.profile_for("what's the weather", PROFILES, "owner@work.example") == "owner@work.example"
    assert bl.profile_for("what's the weather", PROFILES, "someone@else.com") == ""
    assert configured({"EVE_CHROME_PROFILE": " Owner@Work.Example "}).chrome_profile == "owner@work.example"


class _Ctx:
    """A Chrome profile: its cookies for Google's account list, and (for the old request path) the request."""
    def __init__(self, body: str, fetch: Any = None) -> None:
        self.body, self.sid = body, f"sid-{id(self)}"
        _BODIES[self.sid] = body
        get = (lambda url, timeout=0: SimpleNamespace(text=lambda: fetch(url, f"SID={self.sid}", timeout))) if fetch \
            else (lambda url, timeout=0: SimpleNamespace(text=lambda: self.body))
        self.request = SimpleNamespace(get=get)

    def cookies(self, urls: Any = None) -> list[dict[str, str]]:
        return [{"name": "SID", "value": self.sid}]


_BODIES: dict[str, str] = {}


def _by_cookie(url: str, header: str, timeout: float) -> str:
    return _BODIES[header.removeprefix("SID=")]


def _attached(*contexts: Any, fetch: Any = _by_cookie) -> BrowserLane:
    lane = BrowserLane(BrowserLaneConfig(attach=True))
    lane._browser = SimpleNamespace(contexts=list(contexts))
    lane._fetch_accounts = fetch
    return lane


def test_profiles_map_primary_accounts_to_contexts_and_a_missing_one_is_refused() -> None:
    work, personal = _Ctx(LIST_ACCOUNTS), _Ctx('[[["x",1,"G","owner@gmail.com"]]]')
    lane = _attached(work, personal)
    assert lane._profiles() == {"owner@work.example": work, "owner@gmail.com": personal}
    assert lane._pick_profile("owner@gmail.com") is personal
    assert lane._pick_profile("") is work
    with pytest.raises(AttachError, match="no open Chrome window for student@school.example"):
        lane._pick_profile("student@school.example")


class _SlowGoogle:
    """Google's account list answering in `secs`, counting how many requests are in flight at once."""
    def __init__(self, secs: float, hang: tuple[str, ...] = ()) -> None:
        self.secs, self.hang, self.calls, self.in_flight, self.most = secs, hang, 0, 0, 0
        self.lock = threading.Lock()

    def __call__(self, url: str, header: str, timeout: float) -> str:
        with self.lock:
            self.calls += 1
            self.in_flight += 1
            self.most = max(self.most, self.in_flight)
        try:
            body = _BODIES[header.removeprefix("SID=")]
            time.sleep(5.0 if body in self.hang else self.secs)
            return body
        finally:
            with self.lock:
                self.in_flight -= 1


def test_profile_lookups_run_at_once_and_are_cached_for_the_connection() -> None:
    """Production, 2026-09-23/24: one Google request per profile, one after another, 8 s timeout each —
    19 s before a task began. Every profile is now asked at once, and asked once per connection."""
    google = _SlowGoogle(0.3)
    a, b, c = (_Ctx(LIST_ACCOUNTS, google), _Ctx('[[["x",1,"G","owner@gmail.com"]]]', google),
               _Ctx('[[["x",1,"G","student@school.example"]]]', google))
    lane = _attached(a, b, c, fetch=google)
    t0 = time.perf_counter()
    found = lane._profiles()
    secs = time.perf_counter() - t0
    assert found == {"owner@work.example": a, "owner@gmail.com": b, "student@school.example": c}
    assert google.most == 3, f"lookups were not concurrent (at most {google.most} in flight)"
    assert secs < 0.6, f"three 0.3 s lookups took {secs:.2f} s: one after another"
    assert lane._profiles() is found and google.calls == 3          # cached: no second round
    lane._close()                                                     # a new connection asks again
    lane._browser = SimpleNamespace(contexts=[a, b, c])
    assert lane._profile_map is None


def test_a_profile_that_does_not_answer_costs_only_the_lookup_budget(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(bl, "PROFILE_LOOKUP_BUDGET_SECS", 0.3)
    silent = '[[["x",1,"G","slow@example.org"]]]'
    google = _SlowGoogle(0.01, hang=(silent,))
    work, slow = _Ctx(LIST_ACCOUNTS, google), _Ctx(silent, google)
    lane = _attached(work, slow, fetch=google)
    t0 = time.perf_counter()
    assert lane._profiles() == {"owner@work.example": work}
    assert time.perf_counter() - t0 < 1.0


def test_with_one_chrome_profile_there_is_nothing_to_look_up() -> None:
    google = _SlowGoogle(0.0)
    only = _Ctx(LIST_ACCOUNTS, google)
    lane = _attached(only, fetch=google)
    lane._context = only
    lane._ensure = lambda: SimpleNamespace(page=None)
    assert lane._use_profile("check my work inbox") == "" and google.calls == 0


def test_a_lost_tab_in_the_owners_chrome_is_replaced_by_a_new_one_never_theirs() -> None:
    """Regression: once attached, a dead eve tab fell through to pages[0] — the owner's own first tab."""
    opened: list[str] = []

    class Page:
        def __init__(self, name: str, alive: bool = True) -> None:
            self.name, self.alive = name, alive

        def title(self) -> str:
            if not self.alive:
                raise RuntimeError("closed")
            return self.name

        def bring_to_front(self) -> None:
            pass

    owners = [Page("owner's inbox"), Page("owner's bank")]
    ctx = SimpleNamespace(pages=owners, new_page=lambda: opened.append("new") or Page("eve's new tab"))
    lane = BrowserLane(BrowserLaneConfig(attach=True))
    lane._browser, lane._context = SimpleNamespace(), ctx
    lane._page = SimpleNamespace(page=Page("eve's old tab", alive=False))
    got = lane._ensure()
    assert got.page.name == "eve's new tab" and opened == ["new"]


def test_chrome_with_no_window_gets_one_before_connecting(monkeypatch) -> None:
    calls: list[str] = []
    counts = iter(["0", "1"])

    def run(argv, capture_output=True, text=True, timeout=0):
        script = argv[-1]
        calls.append("make" if "make new window" in script else "count")
        return SimpleNamespace(stdout=next(counts) if "count windows" in script else "")

    monkeypatch.setattr(bl.time, "sleep", lambda s: None)
    assert bl.ensure_chrome_window(run) is True and calls == ["count", "make", "count"]


def test_chrome_with_a_window_is_left_alone() -> None:
    calls: list[str] = []

    def run(argv, capture_output=True, text=True, timeout=0):
        calls.append(argv[-1])
        return SimpleNamespace(stdout="2")

    assert bl.ensure_chrome_window(run) is True and len(calls) == 1 and "make new window" not in calls[0]



OWN_CHROME = (bl.CHROME_BINARY + " --user-data-dir=/Users/me/.config/eve-bridge/chrome "
              "--remote-debugging-port=0 --no-first-run")
HELPER = "/Applications/Google Chrome.app/Contents/Frameworks/x/Helpers/Google Chrome Helper.app/Contents/MacOS/Google Chrome Helper"


def ps_run(tables: list[str], calls: list[list[str]]):
    rows = iter(tables)

    def run(argv, capture_output=True, text=True, timeout=0):
        calls.append(argv)
        return SimpleNamespace(stdout=next(rows) if argv[0] == "ps" else "")
    return run


def test_only_eves_own_chrome_running_starts_the_owners_by_path() -> None:
    calls: list[list[str]] = []
    run = ps_run([OWN_CHROME + "\n" + HELPER, OWN_CHROME, OWN_CHROME + "\n" + bl.CHROME_BINARY], calls)
    assert bl.ensure_owner_chrome(run, sleep=lambda s: None) is True
    assert ["open", "-na", bl.CHROME_APP] in calls   # -n: plain `open -a` would only raise eve's Chrome


def test_the_owners_chrome_already_running_is_left_alone() -> None:
    calls: list[list[str]] = []
    run = ps_run([OWN_CHROME + "\n" + bl.CHROME_BINARY + " --flag-switches-begin"], calls)
    assert bl.ensure_owner_chrome(run, sleep=lambda s: None) is True
    assert all(c[0] == "ps" for c in calls)


def test_helpers_and_eves_chrome_do_not_count_as_the_owners() -> None:
    assert bl.owner_chrome_running(ps_run([OWN_CHROME + "\n" + HELPER], [])) is False
    default = bl.CHROME_BINARY + " --user-data-dir=" + str(bl.Path(bl.DEFAULT_CHROME_DIR).expanduser())
    assert bl.owner_chrome_running(ps_run([default], [])) is True


def test_chrome_that_never_starts_reports_false() -> None:
    def run(argv, capture_output=True, text=True, timeout=0):
        return SimpleNamespace(stdout=OWN_CHROME if argv[0] == "ps" else "")
    assert bl.ensure_owner_chrome(run, sleep=lambda s: None, wait_secs=0.01) is False

# -- eve's own Chrome (EVE_BROWSER_OWN) ------------------------------------------------------------------

def test_own_chrome_is_off_unless_asked_and_turns_the_lane_on_by_itself(tmp_path: Path) -> None:
    assert configured({}).own is False
    cfg = configured({"EVE_BROWSER_OWN": "1", "EVE_OWN_CHROME_DIR": str(tmp_path)})
    assert cfg.own is True and cfg.attach is True and cfg.own_dir == tmp_path


def test_own_chrome_starts_on_its_own_folder_never_the_owners() -> None:
    cmd = bl.own_chrome_command(Path("/x/eve-chrome"), "https://accounts.google.com/")
    assert cmd[:3] == ["open", "-na", "Google Chrome"]
    assert "--user-data-dir=/x/eve-chrome" in cmd and "--remote-debugging-port=0" in cmd
    assert cmd[-1] == "https://accounts.google.com/"
    assert not any("Application Support/Google/Chrome" in a for a in cmd)


def test_a_running_own_chrome_is_reused_without_starting_another(tmp_path: Path, monkeypatch) -> None:
    (tmp_path / "DevToolsActivePort").write_text("51234\n/devtools/browser/abc\n")
    monkeypatch.setattr(bl, "_listening", lambda port: port == 51234)
    ran: list[Any] = []
    assert bl.start_own_chrome(tmp_path, run=lambda *a, **k: ran.append(a)) == "ws://127.0.0.1:51234/devtools/browser/abc"
    assert ran == []


def test_a_stale_port_file_is_not_believed_and_chrome_is_started(tmp_path: Path, monkeypatch) -> None:
    (tmp_path / "DevToolsActivePort").write_text("40000\n/devtools/browser/old\n")   # a Chrome that has quit
    monkeypatch.setattr(bl, "_listening", lambda port: port == 51234)
    ran: list[list[str]] = []

    def run(cmd, **kw):
        ran.append(cmd)
        assert not (tmp_path / "DevToolsActivePort").exists()           # the stale file went first
        (tmp_path / "DevToolsActivePort").write_text("51234\n/devtools/browser/new\n")
        return SimpleNamespace(returncode=0, stdout="", stderr="")

    assert bl.start_own_chrome(tmp_path, run=run, poll_secs=0) == "ws://127.0.0.1:51234/devtools/browser/new"
    assert len(ran) == 1 and f"--user-data-dir={tmp_path}" in ran[0]


def test_own_chrome_that_never_comes_up_is_an_attach_error(tmp_path: Path, monkeypatch) -> None:
    monkeypatch.setattr(bl, "_listening", lambda port: False)
    with pytest.raises(AttachError, match="did not start"):
        bl.start_own_chrome(tmp_path, run=lambda *a, **k: None, wait_secs=0.05, poll_secs=0.01)


def test_own_chrome_never_waits_on_an_allow_dialog() -> None:
    lane = BrowserLane(BrowserLaneConfig(attach=True, own=True))
    asked: list[str] = []

    async def answer() -> str:
        asked.append("asked")
        return "auto_allowed"

    def ensure() -> None:
        lane._context = object()

    lane._ensure = ensure  # type: ignore[method-assign]
    asyncio.run(lane.connect(answer))
    assert lane.connected and asked == []


def test_own_chrome_uses_its_one_profile_without_a_lookup() -> None:
    lane = BrowserLane(BrowserLaneConfig(attach=True, own=True, chrome_profile="someone@example.com"))
    first = object()
    lane._browser = SimpleNamespace(contexts=[first, object()])
    lane._profiles = lambda: pytest.fail("no profile lookup in eve's own Chrome")  # type: ignore[method-assign]
    assert lane._pick_profile("someone@example.com") is first


def test_signing_in_opens_eves_chrome_with_no_port() -> None:
    cmd = bl.sign_in_command(Path("/x/eve-chrome"), "https://accounts.google.com/")
    assert "--user-data-dir=/x/eve-chrome" in cmd and cmd[-1] == "https://accounts.google.com/"
    assert not any(a.startswith("--remote-debugging") for a in cmd)


def test_a_sign_in_chrome_left_open_says_to_quit_it(tmp_path: Path, monkeypatch) -> None:
    monkeypatch.setattr(bl, "_listening", lambda port: False)
    monkeypatch.setattr(bl, "own_chrome_running", lambda own_dir, run=None: True)   # open, but with no port
    with pytest.raises(AttachError, match="open for signing in"):
        bl.start_own_chrome(tmp_path, run=lambda *a, **k: None, wait_secs=0.05, poll_secs=0.01)


def test_own_chrome_running_looks_for_its_own_folder_only() -> None:
    seen: list[list[str]] = []
    run = lambda cmd, **k: seen.append(cmd) or SimpleNamespace(stdout="")   # noqa: E731
    assert bl.own_chrome_running(Path("/x/eve-chrome"), run) is False
    assert seen == [["pgrep", "-f", "--", "--user-data-dir=/x/eve-chrome"]]
