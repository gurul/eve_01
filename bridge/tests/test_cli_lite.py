"""eve lite's command line (cli.py, docs/plans/eve-lite.md WP-10), and full eve's beside it.

* The profile: cli.main applies EVE_PROFILE right after the env file; a value that is neither full nor lite exits 78
  before any command runs. Under lite the command line is eve lite's alone (a positive set), and full eve's is as it
  was: none of eve lite's commands, none of full eve's in lite.
* The daemon: under full eve `daemon` still builds full eve's Daemon (a spy), and importing the CLI no longer loads
  full eve's daemon or BLE (with a control); under lite it runs lite.run_daemon on lite's switches.
* `web open` signs this Mac's browser in without ever printing its one-time code: the code comes over the IPC socket,
  sits only in a 0600 page in the user's own temporary folder, and the page is deleted once the browser used it, once
  it ran out, or on Ctrl-C and SIGTERM. Once through lite.py's own IPC handler and MacLogin, end to end.
* `install`, `uninstall`, `update`, `status` (yes or no, nothing else), `web devices|revoke|lan`, `doctor` (which
  reports a blocked Local Network) and `memory forget` in lite.
* `lite uninstall` leaves what install-lite.sh --uninstall leaves, and the installer itself reads the folder it kept
  as eve lite's (its own marker, checked by running its dry run), with controls.

Every daemon here is a stand-in on a real Unix socket in a short temporary folder; launchctl, osadecompile and open
never run (recorders stand in for them), and HOME is a temporary folder throughout.
"""

from __future__ import annotations

import argparse
import asyncio
import contextlib
import hashlib
import io
import json
import logging
import os
import re
import shutil
import signal
import stat
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path
from types import SimpleNamespace
from typing import Any, Callable, Iterator, Optional
from unittest import mock

import pytest

from eve_bridge import _service_launchd, cli, installer, ipc, lite, lite_web, profile, service, web_auth
from eve_bridge import daemon as daemon_mod
from eve_bridge import update as update_mod

CANARY = "CANARY-NOT-A-KEY-7f3c"          # a one-time code and a key that are not token-shaped (AGENTS rule 2)
PORT = 48780
REPO = Path(__file__).resolve().parents[2]
INSTALLER = REPO / "install-lite.sh"
SERVICE_PATH = "/usr/bin:/bin:/usr/sbin:/sbin"
LITE_LABEL = "com.github.eve-bridge.lite"
FULL_LABEL = "com.github.eve-bridge.daemon"
BANNED = ("eve_bridge.daemon", "eve_bridge.ble", "bleak")
RUN = subprocess.run                      # the real one: the `programs` fixture replaces subprocess.run


# ---- the environment, the socket and a stand-in daemon --------------------------------------------------------------

@pytest.fixture(autouse=True)
def _no_launchd(monkeypatch: pytest.MonkeyPatch) -> None:
    """No test here reaches launchd: launchctl is not found unless a test puts its recorder in (``programs``)."""
    which = shutil.which
    monkeypatch.setattr(shutil, "which", lambda name, *a, **k: None if name == "launchctl" else which(name, *a, **k))


@pytest.fixture
def lite_env(tmp_path: Path) -> Iterator[Path]:
    """eve lite's process environment over a fresh HOME, put back whole afterwards: profile.apply rewrites it."""
    home = tmp_path / "home"
    (home / ".config" / "eve-bridge").mkdir(parents=True)
    scratch = tmp_path / "t"
    scratch.mkdir()
    with mock.patch.dict(os.environ, {"HOME": str(home), "EVE_PROFILE": "lite", "TMPDIR": str(scratch)}):
        for name in ("EVE_ENV_FILE", "EVE_BRIDGE_SOCK", "EVE_WEB_DIR"):
            os.environ.pop(name, None)
        yield home


@pytest.fixture
def full_env(tmp_path: Path) -> Iterator[Path]:
    """Full eve's environment (no EVE_PROFILE) over a fresh HOME."""
    home = tmp_path / "home"
    home.mkdir()
    with mock.patch.dict(os.environ, {"HOME": str(home)}):
        for name in ("EVE_PROFILE", "EVE_ENV_FILE", "EVE_SERIAL_PORT", "EVE_SAVE_FRAMES", "EVE_BRIDGE_SOCK"):
            os.environ.pop(name, None)
        yield home


@pytest.fixture
def sock() -> Iterator[Path]:
    """An IPC socket's path in a short folder: macOS keeps a socket's path in 104 bytes, and tmp_path is longer."""
    base = tempfile.gettempdir()
    folder = Path(tempfile.mkdtemp(prefix="evc-", dir=base if len(base) <= 60 else "/tmp"))
    try:
        yield folder / "ipc.sock"
    finally:
        shutil.rmtree(folder, ignore_errors=True)


@pytest.fixture
def fast(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(cli, "IPC_POLL_SECS", 0.01)


@pytest.fixture
def keep_logging() -> Iterator[None]:
    """cli._run_daemon and lite.setup_logging configure the root logger: put it back afterwards."""
    root = logging.getLogger()
    level, handlers = root.level, list(root.handlers)
    try:
        yield
    finally:
        root.setLevel(level)
        root.handlers[:] = handlers


@contextlib.contextmanager
def serving(handler: Callable[[dict[str, Any]], Any], path: Path) -> Iterator[None]:
    """``handler`` answering on a real IPC socket at ``path`` (ipc.IPCServer, on a loop of its own thread)."""
    loop = asyncio.new_event_loop()
    server = ipc.IPCServer(handler, socket_path=str(path))
    started = threading.Event()

    def run() -> None:
        asyncio.set_event_loop(loop)
        loop.run_until_complete(server.start())
        started.set()
        loop.run_forever()

    thread = threading.Thread(target=run, name="fake-lite-ipc", daemon=True)
    thread.start()
    assert started.wait(10)
    try:
        yield
    finally:
        asyncio.run_coroutine_threadsafe(server.stop(), loop).result(10)
        loop.call_soon_threadsafe(loop.stop)
        thread.join(10)
        loop.close()


class Daemon:
    """lite.py's IPC as the CLI sees it: `web_login` hands out ``code`` for ``port`` (after ``page_down`` answers that
    the page is not up yet), `web_login_used` says whether the browser used it, and other events answer what the test
    gave in ``answers``. Every request is kept in ``seen``."""

    def __init__(self, *, code: Any = CANARY, port: Any = PORT, page_down: int = 0,
                 answers: Optional[dict[str, dict[str, Any]]] = None) -> None:
        self.code, self.port, self.page_down = code, port, page_down
        self.answers = answers or {}
        self.used = False
        self.seen: list[dict[str, Any]] = []

    async def __call__(self, req: dict[str, Any]) -> dict[str, Any]:
        self.seen.append(dict(req))
        evt = req.get("evt")
        if evt == "web_login":
            if self.page_down > 0:
                self.page_down -= 1
                return {"ok": False, "error": lite.PAGE_DOWN}
            return {"ok": True, "code": self.code, "port": self.port}
        if evt == "web_login_used":
            return {"ok": True, "used": self.used and req.get("code") == self.code}
        if evt in self.answers:
            return self.answers[evt]
        return {"ok": False, "error": lite.UNKNOWN_EVENT}

    def events(self) -> list[str]:
        return [str(r.get("evt")) for r in self.seen]


class Browser:
    """_open_in_browser's stand-in: reads the page as a browser would, notes its mode and folder, and signs in with the
    code its address carries (``sign_in``), as the page's POST to /api/mac/login does."""

    def __init__(self, sign_in: Optional[Callable[[str], Any]] = None, opens: bool = True) -> None:
        self.sign_in, self.opens = sign_in, opens
        self.paths: list[str] = []
        self.modes: list[int] = []
        self.pages: list[str] = []
        self.codes: list[str] = []

    def __call__(self, path: str) -> bool:
        self.paths.append(path)
        self.modes.append(stat.S_IMODE(os.stat(path).st_mode))
        page = Path(path).read_text(encoding="utf-8")
        self.pages.append(page)
        url = json.loads(re.search(r"location\.replace\((\"[^\"]+\")\)", page).group(1))
        self.codes.append(url.split("#login=", 1)[1])
        if self.opens and self.sign_in is not None:
            self.sign_in(self.codes[-1])
        return self.opens


def _signs_in(daemon: Daemon) -> Callable[[str], None]:
    def sign_in(code: str) -> None:
        daemon.used = code == daemon.code
    return sign_in


class Programs:
    """subprocess.run for the programs eve lite's CLI starts: launchctl (``listed`` is what `list` shows), osadecompile
    (``script`` is what an Eve.app holds) and open. Each call is recorded; nothing runs."""

    def __init__(self, *, listed: tuple[str, ...] = (), script: str = "", fail: tuple[str, ...] = ()) -> None:
        self.calls: list[list[str]] = []
        self.listed, self.script, self.fail = listed, script, fail

    def __call__(self, argv: Any, *args: Any, **kwargs: Any) -> subprocess.CompletedProcess:
        argv = [str(a) for a in argv]
        self.calls.append(argv)
        program = os.path.basename(argv[0])
        assert program in ("launchctl", "osadecompile", "open"), f"{program} would have run"
        key = " ".join(argv[:3]) if program == "open" else f"{program} {argv[1]}" if program == "launchctl" else program
        if any(key.startswith(f) for f in self.fail):
            return subprocess.CompletedProcess(argv, 1, "", "failed")
        if program == "launchctl" and argv[1] == "list":
            out = "PID\tStatus\tLabel\n" + "".join(f"-\t0\t{label}\n" for label in self.listed)
            return subprocess.CompletedProcess(argv, 0, out, "")
        if program == "osadecompile":
            return subprocess.CompletedProcess(argv, 0 if self.script else 1, self.script, "")
        return subprocess.CompletedProcess(argv, 0, "", "")


@pytest.fixture
def programs(monkeypatch: pytest.MonkeyPatch) -> Programs:
    fake = Programs(listed=(LITE_LABEL,))
    monkeypatch.setattr(subprocess, "run", fake)
    monkeypatch.setattr(shutil, "which", lambda name, *a, **k: "/bin/launchctl" if name == "launchctl" else None)
    return fake


# ---- the profile, and which command line runs -----------------------------------------------------------------------

@pytest.mark.parametrize("where", ["environment", "env file"])
def test_a_profile_that_is_neither_full_nor_lite_exits_78_before_any_command(
        full_env: Path, where: str, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]) -> None:
    if where == "environment":
        os.environ["EVE_PROFILE"] = "lightweight"
    else:
        env = full_env / ".config" / "eve-bridge" / "env"
        env.parent.mkdir(parents=True)
        env.write_text("EVE_PROFILE=lightweight\n")
    monkeypatch.setattr(installer, "show_status", lambda **k: pytest.fail("a command ran"))
    assert cli.main(["status"]) == cli.EX_CONFIG == 78
    err = capsys.readouterr().err
    assert "EVE_PROFILE must be 'lite' or 'full'" in err and "lightweight" in err


def test_full_eves_command_line_has_none_of_eve_lites_commands(full_env: Path,
                                                               capsys: pytest.CaptureFixture[str]) -> None:
    for words in (["web", "open"], ["doctor"], ["lite", "uninstall"]):
        with pytest.raises(SystemExit) as stop:
            cli.main(words)
        assert stop.value.code == 2
        assert f"invalid choice: '{words[0]}'" in capsys.readouterr().err


@pytest.mark.parametrize("command", ["celebrate", "species", "diag", "hud", "unpair", "key-check", "ears-check",
                                     "move", "head-cal", "link", "notify", "apps", "telegram-check", "check-update",
                                     "audit", "learning", "lights", "codex-home", "chrome-profile", "vault", "app"])
def test_eve_lites_command_line_has_none_of_full_eves_others(lite_env: Path, command: str,
                                                             capsys: pytest.CaptureFixture[str]) -> None:
    with pytest.raises(SystemExit) as stop:
        cli.main([command])
    assert stop.value.code == 2
    assert f"invalid choice: '{command}'" in capsys.readouterr().err


def test_eve_lites_commands_are_its_own(lite_env: Path, capsys: pytest.CaptureFixture[str]) -> None:
    """The positive set, and that full eve's status (its hooks and its agent) is not what lite's runs."""
    parser, _ = cli._lite_parser()
    choices = next(a for a in parser._actions if isinstance(a, argparse._SubParsersAction)).choices
    assert list(choices) == ["daemon", "install", "uninstall", "status", "update", "web", "doctor", "lite", "memory"]
    assert cli.main([]) == 1
    assert "eve lite on this Mac" in capsys.readouterr().out


def test_web_open_takes_the_seconds_the_installer_gives_it() -> None:
    parser, _ = cli._lite_parser()
    assert parser.parse_args(["web", "open", "--wait", "90"]).wait == 90.0
    assert parser.parse_args(["web", "open"]).wait == cli.WEB_OPEN_WAIT_SECS
    for bad in ("-1", "nan", "inf", "soon"):
        with pytest.raises(SystemExit):
            parser.parse_args(["web", "open", "--wait", bad])


# ---- the daemon -----------------------------------------------------------------------------------------------------

class _Built(Exception):
    pass


def test_daemon_under_full_eve_still_builds_full_eves_daemon(full_env: Path, sock: Path, keep_logging: None,
                                                             monkeypatch: pytest.MonkeyPatch) -> None:
    built: list[dict[str, Any]] = []

    class DaemonSpy:
        def __init__(self, *args: Any, **kwargs: Any) -> None:
            built.append(kwargs)
            raise _Built

    monkeypatch.setattr(daemon_mod, "Daemon", DaemonSpy)
    monkeypatch.setattr(lite, "run_daemon", lambda *a, **k: pytest.fail("eve lite's daemon ran under full eve"))
    with pytest.raises(_Built):
        cli.main(["daemon", "--socket", str(sock)])
    assert built == [{"socket_path": str(sock), "device_name_prefix": "Claude", "device_address": None,
                      "serial_port": None, "save_frames": None}]
    assert not hasattr(cli, "Daemon")               # read from daemon.py when the command runs, not at import


def test_importing_the_cli_loads_no_part_of_full_eves_daemon(tmp_path: Path) -> None:
    """The lazy import: `eve-bridge status`, `web open` and the rest no longer load BLE and full eve's daemon. Control:
    importing the daemon in the same process loads them, so the check sees them."""
    code = ("import json, sys\nimport eve_bridge.cli\n"
            f"banned = {BANNED!r}\n"
            "before = sorted(m for m in banned if m in sys.modules)\n"
            "import eve_bridge.daemon\n"
            "print(json.dumps([before, sorted(m for m in banned if m in sys.modules)]))\n")
    env = {k: v for k, v in os.environ.items() if k != "EVE_PROFILE"}
    done = subprocess.run([sys.executable, "-c", code], capture_output=True, text=True, timeout=120, check=False,
                          env={**env, "HOME": str(tmp_path)})
    assert done.returncode == 0, done.stderr
    before, after = json.loads(done.stdout.strip().splitlines()[-1])
    assert before == []
    assert set(after) == set(BANNED)


def test_daemon_under_eve_lite_runs_lite_daemon_on_lite_switches(lite_env: Path, keep_logging: None,
                                                                 monkeypatch: pytest.MonkeyPatch,
                                                                 caplog: pytest.LogCaptureFixture) -> None:
    env_file = lite_env / ".config" / "eve-bridge" / "env"
    env_file.write_text(f"EVE_PROFILE=lite\nEVE_TELEGRAM_TOKEN={CANARY}-tg\nOPENROUTER_API_KEY={CANARY}-or\n")
    seen: dict[str, Any] = {}

    def run_daemon(args: Any = None, **options: Any) -> int:
        seen["args"], seen["env"] = args, dict(os.environ)
        return lite.EXIT_RESTART

    monkeypatch.setattr(lite, "run_daemon", run_daemon)
    monkeypatch.setattr(lite, "setup_logging", lambda level="INFO": seen.setdefault("level", level))
    monkeypatch.setattr(daemon_mod, "Daemon", lambda *a, **k: pytest.fail("full eve's daemon was built in eve lite"))
    caplog.set_level(logging.INFO)
    assert cli.main(["daemon", "--log-level", "DEBUG"]) == lite.EXIT_RESTART == 75
    env = seen["env"]
    assert env["EVE_TELEGRAM"] == "app" and env["EVE_MINIAPP"] == "0"                 # forced
    assert env["EVE_BRIDGE_SOCK"] == str(lite_env / ".eve-lite" / "ipc.sock")         # lite's own socket
    assert "EVE_TELEGRAM_TOKEN" not in env and env["OPENROUTER_API_KEY"] == f"{CANARY}-or"
    assert seen["args"].socket is None and seen["args"].log_level == "DEBUG" and seen["level"] == "DEBUG"
    said = [r.getMessage() for r in caplog.records if r.name == "eve_bridge.cli"]
    assert said and said[-1].startswith("profile: eve lite:")
    assert "EVE_TELEGRAM_TOKEN" in said[-1].split("removed ", 1)[1].split(", ")
    assert "OPENROUTER_API_KEY" not in said[-1] and CANARY not in caplog.text


# ---- web open -------------------------------------------------------------------------------------------------------

def _pages(lite_env: Path) -> list[Path]:
    return sorted((lite_env.parent / "t").iterdir())


def test_web_open_signs_the_browser_in_and_never_shows_the_code(
        lite_env: Path, sock: Path, fast: None, monkeypatch: pytest.MonkeyPatch,
        capsys: pytest.CaptureFixture[str], caplog: pytest.LogCaptureFixture) -> None:
    os.environ["EVE_BRIDGE_SOCK"] = str(sock)
    daemon = Daemon()
    browser = Browser(_signs_in(daemon))
    monkeypatch.setattr(cli, "_open_in_browser", browser)
    caplog.set_level(logging.DEBUG)
    with serving(daemon, sock):
        assert cli.main(["web", "open", "--wait", "5"]) == 0
    out, err = capsys.readouterr()
    assert out == f"eve is open in your browser: http://localhost:{PORT}\n"
    # the code reached the page and nowhere else on this side: not the screen, the log or the page's name
    assert browser.codes == [CANARY]
    assert f'location.replace("http://localhost:{PORT}/#login={CANARY}")' in browser.pages[0]   # control: it is there
    assert CANARY not in out + err + caplog.text and CANARY not in browser.paths[0]
    page = Path(browser.paths[0])
    assert browser.modes == [0o600]
    assert page.parent == lite_env.parent / "t" and page.name.startswith("eve-open-") and page.suffix == ".html"
    assert not page.exists() and _pages(lite_env) == []             # deleted once the browser signed in
    assert daemon.events()[0] == "web_login" and set(daemon.events()[1:]) == {"web_login_used"}
    assert all(r.get("code") == CANARY for r in daemon.seen[1:])


def test_web_open_through_lite_daemons_own_ipc(lite_env: Path, sock: Path, fast: None,
                                              monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str],
                                              caplog: pytest.LogCaptureFixture) -> None:
    """End to end with lite.py's IPC handler and web_auth.MacLogin: the code LiteDaemon issues is the one the page
    carries, the browser's sign-in consumes it, `web_login_used` then says so, and the code is never printed."""
    os.environ["EVE_BRIDGE_SOCK"] = str(sock)
    profile.apply(os.environ)
    daemon = lite.LiteDaemon(socket_path=str(sock))
    daemon.local = SimpleNamespace(running=True, port=PORT)       # the Mac's page, up on its port
    browser = Browser(daemon.mac_login.consume)
    monkeypatch.setattr(cli, "_open_in_browser", browser)
    caplog.set_level(logging.DEBUG)
    with serving(daemon.handle_ipc, sock):
        assert cli.main(["web", "open", "--wait", "5"]) == 0
    code = browser.codes[0]
    assert web_auth.TOKEN.fullmatch(code) and daemon.mac_login.used(code)
    assert not daemon.mac_login.consume(code)                      # single use: the browser had it
    out, err = capsys.readouterr()
    assert out == f"eve is open in your browser: http://localhost:{PORT}\n"
    assert code not in out + err + caplog.text
    assert _pages(lite_env) == []


def test_web_open_waits_for_a_page_that_is_starting(lite_env: Path, sock: Path, fast: None,
                                                    monkeypatch: pytest.MonkeyPatch) -> None:
    os.environ["EVE_BRIDGE_SOCK"] = str(sock)
    daemon = Daemon(page_down=3)
    monkeypatch.setattr(cli, "_open_in_browser", Browser(_signs_in(daemon)))
    with serving(daemon, sock):
        assert cli.main(["web", "open", "--wait", "5"]) == 0
    assert daemon.events()[:4] == ["web_login"] * 4


def test_web_open_says_why_when_the_page_stays_down(lite_env: Path, sock: Path, fast: None,
                                                    monkeypatch: pytest.MonkeyPatch,
                                                    capsys: pytest.CaptureFixture[str]) -> None:
    os.environ["EVE_BRIDGE_SOCK"] = str(sock)
    daemon = Daemon(page_down=10_000)
    monkeypatch.setattr(cli, "_open_in_browser", lambda path: pytest.fail("a page was opened"))
    with serving(daemon, sock):
        assert cli.main(["web", "open", "--wait", "0.3"]) == 1
    assert lite.PAGE_DOWN in capsys.readouterr().err
    assert _pages(lite_env) == []


def test_web_open_waits_for_a_loaded_service_then_says_eve_is_not_answering(
        lite_env: Path, sock: Path, fast: None, monkeypatch: pytest.MonkeyPatch,
        capsys: pytest.CaptureFixture[str]) -> None:
    """launchd has eve lite (just installed, or restarting into a sign-in) but nothing answers yet: it waits."""
    os.environ["EVE_BRIDGE_SOCK"] = str(sock)
    monkeypatch.setattr(_service_launchd, "lite_loaded", lambda: True)
    monkeypatch.setattr(cli, "_open_in_browser", lambda path: pytest.fail("a page was opened"))
    started = time.monotonic()
    assert cli.main(["web", "open", "--wait", "0.4"]) == 1
    assert time.monotonic() - started >= 0.4
    captured = capsys.readouterr()
    assert captured.out == "" and cli.LITE_NOT_ANSWERING in captured.err
    assert _pages(lite_env) == []


@pytest.mark.parametrize("left", ["nothing", "a stale socket file"])
def test_web_open_does_not_wait_for_an_eve_that_is_not_coming(lite_env: Path, sock: Path, fast: None, left: str,
                                                              monkeypatch: pytest.MonkeyPatch,
                                                              capsys: pytest.CaptureFixture[str]) -> None:
    """Nothing answers and launchd has no eve lite loaded (Eve.app opened after `uninstall --service`, or after a
    crash left its socket behind): it says so at once instead of waiting."""
    os.environ["EVE_BRIDGE_SOCK"] = str(sock)
    if left != "nothing":
        sock.write_text("")
    loaded: list[bool] = []
    monkeypatch.setattr(_service_launchd, "lite_loaded", lambda: loaded.append(False) or False)
    started = time.monotonic()
    assert cli.main(["web", "open", "--wait", "30"]) == 1
    assert time.monotonic() - started < 5 and loaded == [False]
    assert cli.LITE_NOT_LOADED in capsys.readouterr().err
    assert _pages(lite_env) == []


def test_web_open_asks_launchd_only_when_nothing_answers(lite_env: Path, sock: Path, fast: None,
                                                         monkeypatch: pytest.MonkeyPatch) -> None:
    """A daemon that answers (even "not up yet") is coming whatever launchd says: one started by hand counts."""
    os.environ["EVE_BRIDGE_SOCK"] = str(sock)
    monkeypatch.setattr(_service_launchd, "lite_loaded", lambda: pytest.fail("launchd was asked"))
    daemon = Daemon(page_down=2)
    monkeypatch.setattr(cli, "_open_in_browser", Browser(_signs_in(daemon)))
    with serving(daemon, sock):
        assert cli.main(["web", "open", "--wait", "5"]) == 0


@pytest.mark.parametrize("code, port", [("<script>x</script>", PORT), ("short", PORT), (None, PORT),
                                        (CANARY, 0), (CANARY, True), (CANARY, "48780")])
def test_web_open_refuses_an_answer_that_is_not_a_code_and_a_port(lite_env: Path, sock: Path, fast: None,
                                                                  code: Any, port: Any,
                                                                  monkeypatch: pytest.MonkeyPatch,
                                                                  capsys: pytest.CaptureFixture[str]) -> None:
    os.environ["EVE_BRIDGE_SOCK"] = str(sock)
    monkeypatch.setattr(cli, "_open_in_browser", lambda path: pytest.fail("a page was opened"))
    with serving(Daemon(code=code, port=port), sock):
        assert cli.main(["web", "open", "--wait", "1"]) == 1
    assert cli.LITE_NO_CODE in capsys.readouterr().err
    assert _pages(lite_env) == []


def test_web_open_deletes_the_page_when_the_code_runs_out(lite_env: Path, sock: Path, fast: None,
                                                          monkeypatch: pytest.MonkeyPatch,
                                                          capsys: pytest.CaptureFixture[str]) -> None:
    os.environ["EVE_BRIDGE_SOCK"] = str(sock)
    monkeypatch.setattr(web_auth, "LOGIN_SECS", 0.3)
    browser = Browser()                                            # opens the page, never signs in
    monkeypatch.setattr(cli, "_open_in_browser", browser)
    with serving(Daemon(), sock):
        assert cli.main(["web", "open", "--wait", "1"]) == 0
    out, err = capsys.readouterr()
    assert out.startswith("eve is open in your browser:") and cli.LITE_LOGIN_UNUSED in err
    assert browser.paths and not Path(browser.paths[0]).exists()


def test_web_open_deletes_the_page_when_no_browser_opens(lite_env: Path, sock: Path, fast: None,
                                                         monkeypatch: pytest.MonkeyPatch,
                                                         capsys: pytest.CaptureFixture[str]) -> None:
    os.environ["EVE_BRIDGE_SOCK"] = str(sock)
    browser = Browser(opens=False)
    monkeypatch.setattr(cli, "_open_in_browser", browser)
    with serving(Daemon(), sock):
        assert cli.main(["web", "open", "--wait", "1"]) == 1
    out, err = capsys.readouterr()
    assert out == "" and cli.LITE_NO_BROWSER in err
    assert browser.paths and not Path(browser.paths[0]).exists()


def test_web_open_deletes_the_page_on_ctrl_c(lite_env: Path, sock: Path, fast: None,
                                             monkeypatch: pytest.MonkeyPatch) -> None:
    os.environ["EVE_BRIDGE_SOCK"] = str(sock)
    browser = Browser()
    monkeypatch.setattr(cli, "_open_in_browser", browser)

    def interrupted(*args: Any) -> bool:
        raise KeyboardInterrupt

    monkeypatch.setattr(cli, "_login_used", interrupted)
    with serving(Daemon(), sock), pytest.raises(KeyboardInterrupt):
        cli.main(["web", "open", "--wait", "1"])
    assert browser.paths and not Path(browser.paths[0]).exists()


def test_sigterm_and_sighup_end_web_open_through_its_cleanup() -> None:
    if threading.current_thread() is not threading.main_thread():
        pytest.skip("a signal handler is set in the main thread only; the signal would end this runner")
    before = signal.getsignal(signal.SIGTERM), signal.getsignal(signal.SIGHUP)
    for sig in (signal.SIGTERM, signal.SIGHUP):
        cleaned = []
        with pytest.raises(SystemExit) as stop:
            try:
                with cli._stop_on_signals():
                    os.kill(os.getpid(), sig)
                    time.sleep(5)                                 # the handler raises at the next instruction
            finally:
                cleaned.append(True)
        assert stop.value.code == 128 + sig and cleaned == [True]
    assert (signal.getsignal(signal.SIGTERM), signal.getsignal(signal.SIGHUP)) == before


def test_the_login_page_goes_on_at_once_and_keeps_no_history() -> None:
    url = f"http://localhost:{PORT}/#login={CANARY}"
    page = cli._login_page(url).decode()
    assert f'<meta http-equiv="refresh" content="0; url={url}">' in page
    assert f'location.replace("{url}")' in page
    assert '<meta name="referrer" content="no-referrer">' in page


def test_the_page_goes_in_this_users_own_temporary_folder(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("TMPDIR", str(tmp_path))
    assert cli._user_tmp() == str(tmp_path)
    if sys.platform != "darwin":
        return
    for unset in (None, str(tmp_path / "gone")):                  # no TMPDIR (launchd, AppleScript), or a stale one
        if unset is None:
            monkeypatch.delenv("TMPDIR")
        else:
            monkeypatch.setenv("TMPDIR", unset)
        mine = cli._user_tmp()
        assert mine and mine != "/tmp" and Path(mine).is_dir()
        st = os.stat(mine)
        assert st.st_uid == os.getuid() and stat.S_IMODE(st.st_mode) & 0o077 == 0


def _launch_services(home: Path, handlers: list[dict[str, Any]]) -> None:
    path = home / cli.LAUNCH_SERVICES
    path.parent.mkdir(parents=True, exist_ok=True)
    import plistlib

    path.write_bytes(plistlib.dumps({"LSHandlers": handlers}, fmt=plistlib.FMT_BINARY))


def test_the_page_opens_in_the_default_browser_by_its_bundle_id(tmp_path: Path,
                                                                monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("HOME", str(tmp_path))
    fake = Programs()
    monkeypatch.setattr(subprocess, "run", fake)
    page = str(tmp_path / "eve-open-x.html")
    assert cli._default_browser() is None and cli._open_in_browser(page)
    assert fake.calls == [[cli.OPEN, page]]                         # no record: Safari, the default
    _launch_services(tmp_path, [{"LSHandlerURLScheme": "mailto", "LSHandlerRoleAll": "com.apple.mail"},
                                {"LSHandlerURLScheme": "http", "LSHandlerRoleAll": "com.google.chrome"}])
    fake.calls.clear()
    assert cli._default_browser() == "com.google.chrome" and cli._open_in_browser(page)
    assert fake.calls == [[cli.OPEN, "-b", "com.google.chrome", page]]
    fake.calls.clear()
    fake.fail = ("/usr/bin/open -b",)                              # that browser is gone: plain open
    assert cli._open_in_browser(page)
    assert fake.calls == [[cli.OPEN, "-b", "com.google.chrome", page], [cli.OPEN, page]]
    fake.calls.clear()
    fake.fail = ("/usr/bin/open",)
    assert not cli._open_in_browser(page)


@pytest.mark.parametrize("bundle", ["-a", "--args", "not a bundle", "nodots", "", 42])
def test_a_browser_record_that_is_no_bundle_id_is_not_passed_to_open(tmp_path: Path, bundle: Any,
                                                                      monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("HOME", str(tmp_path))
    _launch_services(tmp_path, [{"LSHandlerURLScheme": "http", "LSHandlerRoleAll": bundle}])
    assert cli._default_browser() is None
    (tmp_path / cli.LAUNCH_SERVICES).write_bytes(b"not a plist")
    assert cli._default_browser() is None


# ---- install, uninstall, update -------------------------------------------------------------------------------------

def test_lite_install_says_the_hooks_step_is_not_used(lite_env: Path, monkeypatch: pytest.MonkeyPatch,
                                                      capsys: pytest.CaptureFixture[str]) -> None:
    monkeypatch.setattr(installer, "install_hooks", lambda **k: pytest.fail("Claude Code's hooks were installed"))
    monkeypatch.setattr(installer, "uninstall_hooks", lambda **k: pytest.fail("Claude Code's hooks were touched"))
    assert cli.main(["install"]) == 0
    assert cli.main(["uninstall"]) == 0
    out = capsys.readouterr().out
    assert cli.LITE_NO_HOOKS in out and cli.LITE_NO_HOOKS_GONE in out


def test_lite_install_service_installs_eve_lites_unit_only(lite_env: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    calls: list[str] = []
    monkeypatch.setattr(sys, "platform", "darwin")
    monkeypatch.setattr(_service_launchd, "install_lite", lambda: calls.append("install_lite") or 0)
    monkeypatch.setattr(_service_launchd, "uninstall_lite", lambda: calls.append("uninstall_lite") or 0)
    monkeypatch.setattr(service, "install_service", lambda **k: pytest.fail("full eve's service was installed"))
    monkeypatch.setattr(service, "uninstall_service", lambda **k: pytest.fail("full eve's service was removed"))
    assert cli.main(["install", "--service"]) == 0
    assert cli.main(["uninstall", "--service"]) == 0
    assert calls == ["install_lite", "uninstall_lite"]


def test_lite_install_service_is_for_a_mac(lite_env: Path, monkeypatch: pytest.MonkeyPatch,
                                           capsys: pytest.CaptureFixture[str]) -> None:
    monkeypatch.setattr(sys, "platform", "linux")
    monkeypatch.setattr(_service_launchd, "install_lite", lambda: pytest.fail("installed off a Mac"))
    assert cli.main(["install", "--service"]) == 2
    assert "eve lite runs on a Mac" in capsys.readouterr().err


@pytest.mark.parametrize("words", [["update"], ["update", "-y"]])
def test_lite_update_refuses_with_the_install_line(lite_env: Path, words: list[str], monkeypatch: pytest.MonkeyPatch,
                                                   capsys: pytest.CaptureFixture[str]) -> None:
    monkeypatch.setattr(update_mod, "run_update", lambda **k: pytest.fail("a git self-update ran in eve lite"))
    assert cli.main(words) == 2
    err = capsys.readouterr().err
    assert cli.LITE_INSTALL_LINE in err and "does not update itself" in err


def test_lite_memory_forget_is_the_memory_stores_own(lite_env: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    seen: list[Any] = []
    monkeypatch.setattr(cli, "_run_memory_store", lambda args: seen.append(args) or 0)
    assert cli.main(["memory", "forget", "blue pot"]) == 0
    assert seen[0].memory_cmd == "forget" and seen[0].query == "blue pot"
    with pytest.raises(SystemExit):
        cli.main(["memory", "reindex"])                           # not in eve lite


# ---- status ---------------------------------------------------------------------------------------------------------

STATUS = {"ok": True, "state": "READY", "openrouter": True, "chat": True, "composio": False, "apps": False,
          "voice": True, "page": True, "port": PORT, "lan": True, "lan_port": 48781, "local_network": "blocked",
          "restarting": False, "firewall": {"exec:git": 3}}


def _status_rows(out: str) -> dict[str, str]:
    lines = out.splitlines()
    assert lines[0] == "eve lite on this Mac:"
    return {line.rsplit(None, 1)[0].strip(): line.rsplit(None, 1)[1] for line in lines[1:]}


def test_lite_status_is_yes_or_no_for_each_part(lite_env: Path, sock: Path, monkeypatch: pytest.MonkeyPatch,
                                                capsys: pytest.CaptureFixture[str]) -> None:
    os.environ["EVE_BRIDGE_SOCK"] = str(sock)
    os.environ["OPENROUTER_API_KEY"] = CANARY
    monkeypatch.setattr(_service_launchd, "lite_installed", lambda: True)
    monkeypatch.setattr(_service_launchd, "lite_loaded", lambda: True)
    with serving(Daemon(answers={"status": STATUS}), sock):
        assert cli.main(["status"]) == 0
    out = capsys.readouterr().out
    assert _status_rows(out) == {"login service installed": "yes", "login service loaded": "yes",
                                 "eve answering": "yes", "OpenRouter signed in": "yes", "Composio signed in": "no",
                                 "chat on": "yes", "voice ready": "yes", "phone access on": "yes"}
    assert not re.search(r"\d", out) and CANARY not in out        # no port, count, address or key


def test_lite_status_reads_the_sign_ins_from_this_mac_when_eve_is_down(lite_env: Path, sock: Path,
                                                                       monkeypatch: pytest.MonkeyPatch,
                                                                       capsys: pytest.CaptureFixture[str]) -> None:
    os.environ["EVE_BRIDGE_SOCK"] = str(sock)
    (lite_env / ".config" / "eve-bridge" / "env").write_text(f"EVE_PROFILE=lite\nOPENROUTER_API_KEY={CANARY}\n")
    (lite_env / ".config" / "eve-bridge" / "composio-connect.json").write_text("{}")
    monkeypatch.setattr(_service_launchd, "lite_installed", lambda: True)
    monkeypatch.setattr(_service_launchd, "lite_loaded", lambda: False)
    assert cli.main(["status"]) == 0
    out = capsys.readouterr().out
    assert _status_rows(out) == {"login service installed": "yes", "login service loaded": "no",
                                 "eve answering": "no", "OpenRouter signed in": "yes", "Composio signed in": "yes",
                                 "chat on": "no", "voice ready": "no", "phone access on": "no"}
    assert CANARY not in out


# ---- web devices, revoke, lan ---------------------------------------------------------------------------------------

DEVICES = {"ok": True, "devices": [
    {"id": "a1b2c3d4", "label": "iPhone · Safari", "host": "kitchen-mac.local", "created": 1760000000,
     "last_seen": 1760003600},
    {"id": "0f0f0f0f", "label": "Android\x1b[31m · Chrome", "host": "192.168.1.20", "created": 1760007200,
     "last_seen": None},
]}


def test_web_devices_lists_each_browser_and_no_terminal_codes(lite_env: Path, sock: Path,
                                                              capsys: pytest.CaptureFixture[str]) -> None:
    os.environ["EVE_BRIDGE_SOCK"] = str(sock)
    with serving(Daemon(answers={"devices": DEVICES}), sock):
        assert cli.main(["web", "devices"]) == 0
    out = capsys.readouterr().out
    assert out.startswith("2 browsers can open eve:")
    assert "a1b2c3d4  iPhone · Safari" in out and "kitchen-mac.local" in out and "Android[31m · Chrome" in out
    assert "\x1b" not in out and "last seen ?" in out
    assert "eve-bridge web revoke <id>" in out


@pytest.mark.parametrize("listed", [[], None, {"id": "a1b2c3d4"}, "a1b2c3d4", ["a1b2c3d4"]])
def test_web_devices_with_none_says_how_to_add_one(lite_env: Path, sock: Path, listed: Any,
                                                   capsys: pytest.CaptureFixture[str]) -> None:
    os.environ["EVE_BRIDGE_SOCK"] = str(sock)
    with serving(Daemon(answers={"devices": {"ok": True, "devices": listed}}), sock):
        assert cli.main(["web", "devices"]) == 0
    assert "No browser can open eve yet" in capsys.readouterr().out


def test_web_revoke_signs_browsers_out_by_id_or_all(lite_env: Path, sock: Path,
                                                    capsys: pytest.CaptureFixture[str]) -> None:
    os.environ["EVE_BRIDGE_SOCK"] = str(sock)
    daemon = Daemon(answers={"revoke": {"ok": True, "revoked": ["a1b2c3d4"]}})
    with serving(daemon, sock):
        assert cli.main(["web", "revoke", "A1B2C3D4"]) == 0
        assert cli.main(["web", "revoke", "all"]) == 0
        daemon.answers["revoke"] = {"ok": True, "revoked": []}
        assert cli.main(["web", "revoke", "a1b2c3d4"]) == 1         # no such browser: nothing changed
        assert cli.main(["web", "revoke", "all"]) == 0
    assert [r["id"] for r in daemon.seen] == ["a1b2c3d4", "all", "a1b2c3d4", "all"]
    out = capsys.readouterr().out
    assert out.count("Signed out of eve: a1b2c3d4") == 2 and "No browser has the id a1b2c3d4" in out


def test_web_revoke_reads_only_a_list_of_ids(lite_env: Path, sock: Path, capsys: pytest.CaptureFixture[str]) -> None:
    os.environ["EVE_BRIDGE_SOCK"] = str(sock)
    with serving(Daemon(answers={"revoke": {"ok": True, "revoked": "a1b2c3d4"}}), sock):
        assert cli.main(["web", "revoke", "a1b2c3d4"]) == 1
    assert "Signed out" not in capsys.readouterr().out


@pytest.mark.parametrize("target", ["nope", "a1b2c3d", "a1b2c3d4e", "../x", ""])
def test_web_revoke_asks_nothing_for_an_id_that_is_none(lite_env: Path, sock: Path, target: str,
                                                        capsys: pytest.CaptureFixture[str]) -> None:
    os.environ["EVE_BRIDGE_SOCK"] = str(sock)
    daemon = Daemon()
    with serving(daemon, sock):
        assert cli.main(["web", "revoke", target]) == 2
    assert daemon.seen == [] and "give a browser's id" in capsys.readouterr().err


def test_web_lan_turns_phone_access_on_and_off(lite_env: Path, sock: Path,
                                               capsys: pytest.CaptureFixture[str]) -> None:
    os.environ["EVE_BRIDGE_SOCK"] = str(sock)
    daemon = Daemon(answers={"lan": {"ok": True, "lan": True}})
    with serving(daemon, sock):
        assert cli.main(["web", "lan", "on"]) == 0
        assert cli.main(["web", "lan", "off"]) == 1                 # it answered that the door is still open
        daemon.answers["lan"] = {"ok": True, "lan": False}
        assert cli.main(["web", "lan", "off"]) == 0
        daemon.answers["lan"] = {"ok": False, "error": lite_web.LAN_FAILED}
        assert cli.main(["web", "lan", "on"]) == 1
    assert [r["on"] for r in daemon.seen] == [True, False, False, True]
    out, err = capsys.readouterr()
    assert "Phone access is on" in out and "Phone access is off" in out and lite_web.LAN_FAILED in err


def test_a_refusal_that_reads_as_a_key_is_never_echoed(lite_env: Path, sock: Path,
                                                       capsys: pytest.CaptureFixture[str]) -> None:
    os.environ["EVE_BRIDGE_SOCK"] = str(sock)
    leak = f"failed for {CANARY}-{CANARY}"                        # one run of 43 URL-safe characters
    with serving(Daemon(answers={"lan": {"ok": False, "error": leak}}), sock):
        assert cli.main(["web", "lan", "on"]) == 1
    err = capsys.readouterr().err
    assert leak not in err and cli.LITE_REFUSED in err


def test_web_commands_say_so_when_eve_is_not_running(lite_env: Path, sock: Path,
                                                     capsys: pytest.CaptureFixture[str]) -> None:
    os.environ["EVE_BRIDGE_SOCK"] = str(sock)
    for words in (["web", "devices"], ["web", "revoke", "all"], ["web", "lan", "on"]):
        assert cli.main(words) == 1
        assert cli.LITE_NOT_ANSWERING in capsys.readouterr().err


# ---- doctor ---------------------------------------------------------------------------------------------------------

def _healthy(home: Path, monkeypatch: pytest.MonkeyPatch) -> dict[str, Path]:
    """An eve lite install as it should be, in ``home``: the env file, the login service, the page and the private
    files; its daemon's status is STATUS, all well."""
    folder = home / ".config" / "eve-bridge"
    env = folder / "env"
    env.write_text(f"EVE_PROFILE=lite\nOPENROUTER_API_KEY={CANARY}\n")
    for name in ("composio-connect.json", "web-devices.json", "web.json"):
        (folder / name).write_text("{}")
    tls = folder / "web-tls"
    tls.mkdir(mode=0o700)
    for name in ("ca.key", "leaf.key", "ca.pem", "leaf.pem"):
        (tls / name).write_text("x")
    for path in [env, folder / "composio-connect.json", folder / "web-devices.json", folder / "web.json",
                 tls / "ca.key", tls / "leaf.key"]:
        path.chmod(0o600)
    plist = _service_launchd.lite_plist_path()
    plist.parent.mkdir(parents=True)
    plist.write_bytes(_service_launchd._build_lite_plist())
    web = home / "web"
    web.mkdir()
    (web / "index.html").write_text("<!doctype html>")
    os.environ["EVE_WEB_DIR"] = str(web)
    monkeypatch.setattr(_service_launchd, "lite_loaded", lambda: True)
    return {"env": env, "tls": tls, "plist": plist, "web": web}


HEALTHY = {**STATUS, "composio": True, "local_network": "ok", "firewall": {}}


def _doctor(sock: Path, status: Optional[dict[str, Any]], capsys: pytest.CaptureFixture[str]) -> tuple[int, str]:
    if status is None:
        rc = cli.main(["doctor"])
    else:
        with serving(Daemon(answers={"status": status}), sock):
            rc = cli.main(["doctor"])
    return rc, capsys.readouterr().out


def _marks(out: str) -> dict[str, str]:
    return {line[6:].split(":", 1)[0]: line[2:4] for line in out.splitlines()[1:-1]}


def test_doctor_on_a_healthy_install_finds_nothing_to_fix(lite_env: Path, sock: Path,
                                                          monkeypatch: pytest.MonkeyPatch,
                                                          capsys: pytest.CaptureFixture[str]) -> None:
    os.environ["EVE_BRIDGE_SOCK"] = str(sock)
    _healthy(lite_env, monkeypatch)
    rc, out = _doctor(sock, HEALTHY, capsys)
    assert rc == 0, out
    assert out.splitlines()[-1] == "Nothing to fix." and cli.FIX not in out
    marks = _marks(out)
    assert {label for label, mark in marks.items() if mark == cli.OK} == {
        "profile", "login service", "eve", "OpenRouter", "Composio", "the Mac's page", "phone access",
        "Local Network", "voice", "firewall", "web page", "private files"}
    assert {label for label, mark in marks.items() if mark == cli.INFO} == {
        f"Python {sys.version.split()[0]}", "Xcode's command line tools"}
    assert f"http://localhost:{PORT}" in out and CANARY not in out


def test_doctor_reports_a_turned_down_openrouter_key(lite_env: Path, sock: Path, monkeypatch: pytest.MonkeyPatch,
                                                     capsys: pytest.CaptureFixture[str]) -> None:
    """The daemon's status says the key was turned down (a 401 from /api/v1/key, which it polls): doctor marks it
    to fix and points to a new sign-in, rather than the plain "signed in" a present-but-dead key used to get."""
    os.environ["EVE_BRIDGE_SOCK"] = str(sock)
    _healthy(lite_env, monkeypatch)
    rc, out = _doctor(sock, {**HEALTHY, "openrouter_rejected": True}, capsys)
    assert rc == 1
    assert _marks(out)["OpenRouter"] == cli.FIX
    assert "turned down" in out and "Sign in with OpenRouter again" in out
    assert out.splitlines()[-1] == "1 to fix, marked !!."


def test_doctor_reports_a_blocked_local_network(lite_env: Path, sock: Path, monkeypatch: pytest.MonkeyPatch,
                                                capsys: pytest.CaptureFixture[str]) -> None:
    """The spec's manual gates M5 and M6: with Local Network denied, `eve-bridge doctor` reports blocked."""
    os.environ["EVE_BRIDGE_SOCK"] = str(sock)
    _healthy(lite_env, monkeypatch)
    rc, out = _doctor(sock, {**HEALTHY, "local_network": "blocked"}, capsys)
    assert rc == 1
    assert _marks(out)["Local Network"] == cli.FIX
    assert "Local Network: blocked" in out and "Privacy & Security, Local Network" in out
    assert out.splitlines()[-1] == "1 to fix, marked !!."


def test_doctor_names_what_the_firewall_refused(lite_env: Path, sock: Path, monkeypatch: pytest.MonkeyPatch,
                                                capsys: pytest.CaptureFixture[str]) -> None:
    os.environ["EVE_BRIDGE_SOCK"] = str(sock)
    _healthy(lite_env, monkeypatch)
    rc, out = _doctor(sock, {**HEALTHY, "firewall": {"exec:git": 2, "path:~/Documents": 1}}, capsys)
    assert rc == 0
    assert "firewall: refused git (2), ~/Documents (1)." in out and _marks(out)["firewall"] == cli.INFO


def test_doctor_with_eve_down_says_where_to_look(lite_env: Path, sock: Path, monkeypatch: pytest.MonkeyPatch,
                                                capsys: pytest.CaptureFixture[str]) -> None:
    os.environ["EVE_BRIDGE_SOCK"] = str(sock)
    _healthy(lite_env, monkeypatch)
    rc, out = _doctor(sock, None, capsys)
    assert rc == 1
    assert "eve: not answering on this Mac" in out and _marks(out)["OpenRouter"] == cli.OK   # read from the env file
    assert "Local Network" not in out                                  # only eve itself knows


def test_doctor_finds_what_is_wrong_on_this_mac(lite_env: Path, sock: Path, monkeypatch: pytest.MonkeyPatch,
                                                capsys: pytest.CaptureFixture[str]) -> None:
    os.environ["EVE_BRIDGE_SOCK"] = str(sock)
    parts = _healthy(lite_env, monkeypatch)
    parts["env"].write_text(f"OPENROUTER_API_KEY={CANARY}\n")       # the profile line gone
    parts["env"].chmod(0o644)                                        # and readable by other accounts
    (parts["tls"] / "ca.key").chmod(0o640)
    (parts["web"] / "index.html").unlink()
    full = _service_launchd.agent_path(FULL_LABEL)
    full.write_bytes(b"full eve's")
    rc, out = _doctor(sock, HEALTHY, capsys)
    assert rc == 1
    marks = _marks(out)
    assert marks["profile"] == marks["private files"] == marks["web page"] == cli.FIX
    assert "~/.config/eve-bridge/env, ~/.config/eve-bridge/web-tls/ca.key" in out
    assert f"full eve's login service is installed for this user ({FULL_LABEL})" in out
    assert out.splitlines()[-1] == "4 to fix, marked !!." and CANARY not in out


def test_doctor_checks_the_plist_pins_the_profile(lite_env: Path, sock: Path, monkeypatch: pytest.MonkeyPatch,
                                                  capsys: pytest.CaptureFixture[str]) -> None:
    import plistlib

    os.environ["EVE_BRIDGE_SOCK"] = str(sock)
    parts = _healthy(lite_env, monkeypatch)
    unit = plistlib.loads(parts["plist"].read_bytes())
    del unit["EnvironmentVariables"]["EVE_PROFILE"]
    parts["plist"].write_bytes(plistlib.dumps(unit))
    rc, out = _doctor(sock, HEALTHY, capsys)
    assert rc == 1 and "login service: it does not pin EVE_PROFILE=lite" in out
    for argv in ({"a": 1}, "python", [], [str(lite_env / "no-such-python")]):   # a plist edited by hand
        unit = plistlib.loads(_service_launchd._build_lite_plist())
        unit["ProgramArguments"] = argv
        parts["plist"].write_bytes(plistlib.dumps(unit))
        rc, out = _doctor(sock, HEALTHY, capsys)
        assert rc == 1 and "login service: the Python it starts is gone" in out


# ---- lite uninstall -------------------------------------------------------------------------------------------------

def _installed(home: Path, *, env: Optional[str] = f"EVE_PROFILE=lite\nOPENROUTER_API_KEY={CANARY}\n",
               app: bool = True, full: bool = False) -> dict[str, Path]:
    """eve lite installed in ``home``, as install-lite.sh leaves it, with a memory to keep or delete."""
    p = {"plist": _service_launchd.lite_plist_path(), "lite_home": home / ".eve-lite",
         "log": home / "Library" / "Logs" / "eve-lite.log", "app": home / "Applications" / "Eve.app",
         "env": home / ".config" / "eve-bridge" / "env", "kept": home / ".config" / "eve-bridge" / cli.LITE_KEPT,
         "memory": home / ".config" / "eve-bridge" / "memory" / "notes.md",
         "full": _service_launchd.agent_path(FULL_LABEL)}
    p["plist"].parent.mkdir(parents=True, exist_ok=True)
    p["plist"].write_bytes(_service_launchd._build_lite_plist())
    (p["lite_home"] / "venv" / "bin").mkdir(parents=True)
    (p["lite_home"] / "venv" / "bin" / "eve-bridge").write_text("#!/bin/sh\n")
    p["log"].parent.mkdir(parents=True)
    p["log"].write_text("a log line\n")
    if app:
        (p["app"] / "Contents").mkdir(parents=True)
    p["memory"].parent.mkdir(parents=True, exist_ok=True)
    p["memory"].write_text("what eve remembers\n")
    if env is not None:
        p["env"].write_text(env)
        p["env"].chmod(0o600)
    if full:
        p["full"].write_bytes(b"full eve's")
    return p


def _eve_app_script(home: Path) -> str:
    return f'do shell script "EVE_PROFILE=lite PYTHONNOUSERSITE=1 \'{home}/.eve-lite/venv/bin/eve-bridge\' web open"'


def _installer(home: Path, *args: str) -> subprocess.CompletedProcess:
    """install-lite.sh itself, run in ``home``: only its dry runs, which write nothing and start no service."""
    assert "--dry-run" in args
    return RUN(["/bin/bash", str(INSTALLER), *args], capture_output=True, text=True, timeout=120, check=False,
               stdin=subprocess.DEVNULL, env={"HOME": str(home), "PATH": SERVICE_PATH, "TMPDIR": str(home.parent / "t")})


needs_installer = pytest.mark.skipif(sys.platform != "darwin" or not INSTALLER.is_file() or not Path("/bin/bash").exists(),
                                     reason="install-lite.sh runs on a Mac")


def _tree(home: Path) -> dict[str, tuple[int, bytes]]:
    return {str(p.relative_to(home)): (p.stat().st_mode, p.read_bytes() if p.is_file() else b"")
            for p in sorted(home.rglob("*"))}


@needs_installer
def test_lite_uninstall_keeps_the_memory_on_no_and_the_installer_takes_the_folder_back(
        lite_env: Path, programs: Programs, capsys: pytest.CaptureFixture[str]) -> None:
    p = _installed(lite_env)
    programs.script = _eve_app_script(lite_env)
    asked: list[str] = []
    assert cli._lite_uninstall(dry_run=False, ask=lambda q: asked.append(q) or "n") == 0
    assert len(asked) == 1 and "~/.config/eve-bridge" in asked[0]
    for gone in ("plist", "lite_home", "log", "app"):
        assert not os.path.lexists(p[gone]), gone
    assert p["memory"].read_text() == "what eve remembers\n"
    assert p["env"].read_text() == f"OPENROUTER_API_KEY={CANARY}\n" and stat.S_IMODE(p["env"].stat().st_mode) == 0o600
    digest = hashlib.sha256(p["env"].read_bytes()).hexdigest()
    assert f"env_sha256={digest}" in p["kept"].read_text().splitlines()
    assert stat.S_IMODE(p["kept"].stat().st_mode) == 0o600
    assert ["launchctl", "bootout", f"gui/{os.getuid()}/{LITE_LABEL}"] in programs.calls
    assert not any(FULL_LABEL in " ".join(call) for call in programs.calls)
    out = capsys.readouterr().out
    assert out.splitlines()[-1].endswith("the install line takes them back.") and CANARY not in out
    # install-lite.sh reads the folder as the one eve lite kept: past its env-file check, to its release check
    done = _installer(lite_env, "--dry-run")
    assert "is not from a release" in done.stderr, done.stderr
    # control: the same folder with its env file changed afterwards is not eve lite's to take back
    p["env"].write_text(p["env"].read_text() + "EVE_SOMETHING=1\n")
    done = _installer(lite_env, "--dry-run")
    assert "changed after eve lite was uninstalled" in done.stderr, done.stderr


@needs_installer
def test_lite_uninstall_of_an_env_file_holding_only_the_profile(lite_env: Path, programs: Programs) -> None:
    p = _installed(lite_env, env="EVE_PROFILE=lite\n", app=False)
    assert cli._lite_uninstall(dry_run=False, ask=lambda q: "") == 0
    assert not p["env"].exists()
    assert "env_sha256=none" in p["kept"].read_text().splitlines()
    assert "is not from a release" in _installer(lite_env, "--dry-run").stderr


def test_lite_uninstall_deletes_the_memory_on_yes(lite_env: Path, programs: Programs,
                                                  capsys: pytest.CaptureFixture[str]) -> None:
    p = _installed(lite_env)
    programs.script = _eve_app_script(lite_env)
    assert cli._lite_uninstall(dry_run=False, ask=lambda q: "yes") == 0
    assert not (lite_env / ".config" / "eve-bridge").exists()
    for gone in ("plist", "lite_home", "log", "app"):
        assert not os.path.lexists(p[gone]), gone
    assert capsys.readouterr().out.splitlines()[-1] == "eve lite is uninstalled, with its memory and settings."


def test_lite_uninstall_with_no_terminal_keeps_the_memory(lite_env: Path, programs: Programs,
                                                          monkeypatch: pytest.MonkeyPatch) -> None:
    p = _installed(lite_env)
    monkeypatch.setattr(sys, "stdin", io.StringIO("yes\n"))         # not a terminal: never read
    assert cli._lite_uninstall(dry_run=False) == 0
    assert p["memory"].exists() and p["kept"].exists()


def test_lite_uninstall_beside_full_eve_never_offers_the_folder(lite_env: Path, programs: Programs,
                                                                capsys: pytest.CaptureFixture[str]) -> None:
    p = _installed(lite_env, env=f"EVE_PROFILE=lite\nEVE_MEMORY=1\nOPENROUTER_API_KEY={CANARY}\n", full=True)
    p["kept"].write_text("env_sha256=none\n")
    assert cli._lite_uninstall(dry_run=False, ask=lambda q: pytest.fail("offered full eve's folder")) == 0
    assert p["full"].read_bytes() == b"full eve's"
    assert p["env"].read_text() == f"EVE_MEMORY=1\nOPENROUTER_API_KEY={CANARY}\n"
    assert not p["kept"].exists() and p["memory"].exists()
    assert not any(FULL_LABEL in " ".join(call) for call in programs.calls)
    assert capsys.readouterr().out.splitlines()[-1] == "eve lite is uninstalled. Full eve stays as it is."


def test_lite_uninstall_leaves_an_eve_app_it_did_not_make(lite_env: Path, programs: Programs) -> None:
    p = _installed(lite_env)
    programs.script = 'do shell script "open -a Notes"'
    assert cli._lite_uninstall(dry_run=False, ask=lambda q: "n") == 0
    assert p["app"].is_dir()
    assert [cli.OSADECOMPILE, str(p["app"])] in programs.calls


def test_lite_uninstall_removes_links_not_what_they_point_to(lite_env: Path, tmp_path: Path,
                                                             programs: Programs) -> None:
    """An Eve.app or a log that is a link: the link goes, never the folder or file it points to."""
    p = _installed(lite_env, app=False)
    real_app, real_log = tmp_path / "elsewhere" / "Eve.app", tmp_path / "elsewhere" / "kept.log"
    (real_app / "Contents").mkdir(parents=True)
    real_log.write_text("someone's log\n")
    p["app"].parent.mkdir(parents=True, exist_ok=True)
    p["app"].symlink_to(real_app)
    p["log"].unlink()
    p["log"].symlink_to(real_log)
    programs.script = _eve_app_script(lite_env)
    assert cli._lite_uninstall(dry_run=False, ask=lambda q: "n") == 0
    assert not os.path.lexists(p["app"]) and not os.path.lexists(p["log"])
    assert (real_app / "Contents").is_dir() and real_log.read_text() == "someone's log\n"


def test_lite_uninstall_says_what_it_could_not_remove(lite_env: Path, programs: Programs,
                                                      monkeypatch: pytest.MonkeyPatch,
                                                      capsys: pytest.CaptureFixture[str]) -> None:
    p = _installed(lite_env, app=False)

    def refuse(path: Any, *args: Any, **kwargs: Any) -> None:
        raise PermissionError(1, "Operation not permitted")

    monkeypatch.setattr(shutil, "rmtree", refuse)                    # ~/.eve-lite stays
    assert cli._lite_uninstall(dry_run=False, ask=lambda q: "n") == 1
    captured = capsys.readouterr()
    assert "~/.eve-lite was not removed (PermissionError)" in captured.err
    assert not p["plist"].exists() and p["kept"].exists()          # the rest still went, and the folder was marked
    assert captured.out.splitlines()[-1].endswith("the install line takes them back.")


@needs_installer
@pytest.mark.parametrize("state", ["lite", "kept", "full", "none"])
def test_lite_uninstall_dry_run_writes_nothing_and_decides_as_the_installer(
        lite_env: Path, programs: Programs, state: str, capsys: pytest.CaptureFixture[str]) -> None:
    """Whether the folder is eve lite's to offer for deletion is decided as install-lite.sh --uninstall decides it,
    in four states: its env file says lite, an earlier uninstall kept it, full eve is beside it, or neither."""
    env = {"lite": "EVE_PROFILE=lite\n", "kept": "OPENROUTER_API_KEY=x\n", "full": "EVE_PROFILE=lite\n",
           "none": "OPENROUTER_API_KEY=x\n"}[state]
    p = _installed(lite_env, env=env, app=False, full=state == "full")
    if state == "kept":
        p["kept"].write_text(f"env_sha256={hashlib.sha256(env.encode()).hexdigest()}\n")
    before = _tree(lite_env)
    assert cli._lite_uninstall(dry_run=True) == 0
    assert _tree(lite_env) == before
    out = capsys.readouterr().out
    assert out.splitlines()[-1] == "Nothing was written."
    for path in ("~/Library/LaunchAgents/com.github.eve-bridge.lite.plist", "~/.eve-lite", "~/Library/Logs/eve-lite.log"):
        assert f"  {path}\n" in out
    asks = "Then it would ask before deleting ~/.config/eve-bridge" in out
    assert asks == (state in ("lite", "kept"))
    installer_says = _installer(lite_env, "--uninstall", "--dry-run")
    assert installer_says.returncode == 0, installer_says.stderr
    assert ("Then it would ask before deleting ~/.config/eve-bridge" in installer_says.stdout) == asks
    assert _tree(lite_env) == before
