"""eve lite's daemon boots under its own process firewall and trips it nowhere (docs/plans/eve-lite.md M1, WP-9).

Each boot is ``lite.run_daemon`` in a fresh Python process, as launchd runs it: the lite LaunchAgent's PATH, a fake
OpenRouter key, a temporary HOME. run_daemon installs the firewall in enforce mode before it builds anything (an audit
hook cannot be removed, so this test process never installs one). Everything is the real part, with three changes: the
page and the LAN door listen on ephemeral loopback ports, the network is cut (a host name lookup for anything but this
Mac fails, and is recorded), and the OpenRouter exchange of the sign-in boot answers from an httpx MockTransport.

* READY: memory, the chat log, the watcher, Composio's bridge (a token file is there), the brain, the chat door, the
  calls, WebRTC, the page, the LAN door (web.json says it was on, so scutil runs for the certificate's name), the IPC
  socket, the spend figures and the dream. A browser signs in and reads its status. Then SIGTERM, and exit 0.
* NEEDS_OPENROUTER, then the sign-in: the page's OpenRouter callback saves the key (0600) and eve exits 75.

Both must hold: zero refused process starts and zero protected-folder accesses (profile.firewall_counts is {} at
READY and after the stop), no fake developer tool ever ran (each leaves a mark), none of BLE, full eve's daemon, the
Mac's audio or Quartz was imported, and the only hosts eve tried are the ones the spec says leave the Mac (M9).
Positive controls, in the same process once the daemon stopped: a harmless program and a file in ~/Documents are
refused, so the firewall really was on and enforcing.
"""

from __future__ import annotations

import json
import os
import shutil
import stat
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Any, Iterator

import pytest

SRC = Path(__file__).resolve().parents[1] / "src"
CANARY_KEY = "CANARY-NOT-A-KEY-7f3c-openrouter"
LITE_PATH = "/usr/bin:/bin:/usr/sbin:/sbin"                       # the lite LaunchAgent's PATH (spec M8)
SHIMS = ("git", "python3", "clang", "cc", "make", "xcrun", "install_name_tool", "osascript", "screencapture",
         "codex", "claude", "cloudflared", "adb")
EGRESS = {"openrouter.ai", "connect.composio.dev", "login.composio.dev"}   # what the spec lets leave the Mac (M9)

BOOT = r'''
import http.client as httpclient
import json, os, signal, socket, stat, subprocess, sys, threading
from urllib.parse import parse_qs, urlsplit

ASKED = []
_real_getaddrinfo = socket.getaddrinfo


def local_only(host, *args, **kwargs):
    name = host.decode() if isinstance(host, bytes) else host
    if name in (None, "localhost", "127.0.0.1", "::1"):
        return _real_getaddrinfo(host, *args, **kwargs)
    ASKED.append(str(name))
    raise socket.gaierror(socket.EAI_NONAME, "no network in this test")


socket.getaddrinfo = local_only

from eve_bridge import profile

profile.apply(os.environ)                       # as cli.main does, right after the env file
import httpx

from eve_bridge import lite, onboarding

BANNED = ("bleak", "eve_bridge.ble", "eve_bridge.daemon", "sounddevice", "Quartz")
SOCK = os.environ["EVE_BRIDGE_SOCK"]
FLOW = os.environ["BOOT_FLOW"]
KEY = os.environ["BOOT_CANARY"]
REPORT = {}


def ipc(req):
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as s:
        s.settimeout(20)
        s.connect(SOCK)
        s.sendall(json.dumps(req).encode() + b"\n")
        data = b""
        while not data.endswith(b"\n"):
            chunk = s.recv(65536)
            if not chunk:
                break
            data += chunk
    return json.loads(data)


def web(port, method, path, body=None, cookie=""):
    host = f"localhost:{port}"
    headers = {"Host": host, "User-Agent": "Mozilla/5.0 (Macintosh) AppleWebKit/605.1.15 Version/18.0 Safari/605.1.15"}
    data = None
    if body is not None:
        data = json.dumps(body).encode()
        headers.update({"Origin": f"http://{host}", "Content-Type": "application/json"})
    if cookie:
        headers["Cookie"] = cookie
    conn = httpclient.HTTPConnection("127.0.0.1", port, timeout=20)
    try:
        conn.request(method, path, body=data, headers=headers)
        reply = conn.getresponse()
        return (reply.status, {k.lower(): v for k, v in reply.getheaders()}, reply.read(),
                reply.headers.get_all("Set-Cookie") or [])
    finally:
        conn.close()


def mac_cookie(port):
    login = ipc({"evt": "web_login"})
    status, _, _, cookies = web(port, "POST", "/api/mac/login", {"code": login["code"]})
    REPORT["signed_in"] = status
    return cookies[0].split(";", 1)[0]


def check_ready(daemon):
    try:
        status = ipc({"evt": "status"})
        REPORT["status"] = status
        port = status["port"]
        REPORT["health"] = json.loads(web(port, "GET", "/healthz")[2])
        REPORT["index"] = web(port, "GET", "/")[0]
        cookie = mac_cookie(port)
        REPORT["web_status"] = web(port, "GET", "/api/web/status", cookie=cookie)[0]
        REPORT["devices"] = len(ipc({"evt": "devices"})["devices"])
        REPORT["state"] = daemon.state
        REPORT["ready_counts"] = profile.firewall_counts()
        REPORT["ready_banned"] = [m for m in BANNED if m in sys.modules]
        REPORT["sock_mode"] = stat.S_IMODE(os.stat(SOCK).st_mode)
    except Exception as e:
        REPORT["check_error"] = repr(e)
    finally:
        os.kill(os.getpid(), signal.SIGTERM)


def check_sign_in(daemon):
    try:
        status = ipc({"evt": "status"})
        REPORT["status"] = status
        port = status["port"]
        cookie = mac_cookie(port)
        start = web(port, "GET", "/onboard/openrouter/start", cookie=cookie)
        state = parse_qs(urlsplit(start[1]["location"]).query)["state"][0]
        REPORT["callback"] = web(port, "GET", f"/onboard/openrouter/callback?code=code-1234&state={state}")[0]
        REPORT["state"] = daemon.state
        REPORT["ready_counts"] = profile.firewall_counts()
        REPORT["ready_banned"] = [m for m in BANNED if m in sys.modules]
    except Exception as e:
        REPORT["check_error"] = repr(e)
        os.kill(os.getpid(), signal.SIGTERM)    # eve restarts by itself after the sign-in; this only ends a failure


def provider(request):
    if str(request.url) == onboarding.OPENROUTER_KEYS_URL:
        return httpx.Response(200, json={"key": KEY})
    return httpx.Response(404)


options = {"web_options": {"local_ports": (0,), "lan_ports": (0,), "lan_hosts": ("127.0.0.1",), "restart_delay": 0.0},
           "on_ready": lambda d: threading.Thread(target={"ready": check_ready, "sign-in": check_sign_in}[FLOW],
                                                  args=(d,), daemon=True).start()}
if FLOW == "sign-in":
    options["openrouter_login"] = onboarding.OpenRouterLogin(
        http=httpx.AsyncClient(transport=httpx.MockTransport(provider)))
REPORT["exit"] = lite.run_daemon(None, **options)
REPORT["stopped_counts"] = profile.firewall_counts()
REPORT["sock_left"] = os.path.exists(SOCK)
REPORT["asked"] = sorted(set(ASKED))
REPORT["banned"] = [m for m in BANNED if m in sys.modules]
# the positive controls: the firewall is on and enforcing
try:
    subprocess.run(["/usr/bin/true"], check=False)
    REPORT["control_exec"] = "ran"
except PermissionError:
    REPORT["control_exec"] = "refused"
try:
    with open(os.path.join(os.environ["HOME"], "Documents", "private.txt"), encoding="utf-8") as f:
        f.read()
    REPORT["control_open"] = "read"
except PermissionError:
    REPORT["control_open"] = "refused"
REPORT["control_counts"] = profile.firewall_counts()
print(json.dumps(REPORT))
'''


@pytest.fixture
def box(tmp_path: Path) -> Iterator[dict[str, Path]]:
    """A home with a private file in ~/Documents and a built web page; a folder of fake developer tools first on the
    PATH, each of which leaves a mark if it ever runs; and a short folder for the IPC socket (macOS keeps a socket's
    path in 104 bytes, and tmp_path is longer)."""
    home, bin_dir, marks, web = tmp_path / "home", tmp_path / "bin", tmp_path / "marks", tmp_path / "web"
    for folder in (home / "Documents", bin_dir, marks, web / "assets"):
        folder.mkdir(parents=True)
    (home / "Documents" / "private.txt").write_text("private\n", encoding="utf-8")
    (web / "index.html").write_bytes(b"<!doctype html><title>eve</title><div id=root></div>")
    for name in SHIMS:
        fake = bin_dir / name
        fake.write_text(f'#!/bin/sh\ntouch "{marks}/{name}"\n')
        fake.chmod(0o755)
    base = tempfile.gettempdir()
    short = Path(tempfile.mkdtemp(prefix="evl-", dir=base if len(base) <= 60 else "/tmp"))
    try:
        yield {"home": home, "bin": bin_dir, "marks": marks, "web": web, "sock": short / "ipc.sock",
               "tmp": tmp_path}
    finally:
        shutil.rmtree(short, ignore_errors=True)


def boot(box: dict[str, Path], flow: str, *, key: bool) -> tuple[dict[str, Any], str]:
    env = {"PATH": f"{box['bin']}:{LITE_PATH}", "HOME": str(box["home"]), "PYTHONPATH": str(SRC),
           "TMPDIR": str(box["tmp"]), "EVE_PROFILE": "lite", "EVE_BRIDGE_SOCK": str(box["sock"]),
           "EVE_WEB_DIR": str(box["web"]), "BOOT_FLOW": flow, "BOOT_CANARY": CANARY_KEY}
    if key:
        env["OPENROUTER_API_KEY"] = CANARY_KEY
    done = subprocess.run([sys.executable, "-c", BOOT], capture_output=True, text=True, env=env, timeout=180,
                          cwd=str(box["home"]))
    assert done.returncode == 0, done.stderr[-4000:]
    report = json.loads(done.stdout.strip().splitlines()[-1])
    assert "check_error" not in report, (report["check_error"], done.stderr[-4000:])
    return report, done.stderr


def assert_clean(report: dict[str, Any], box: dict[str, Path], log: str) -> None:
    """Nothing refused at READY or by the stop, no fake tool ran, nothing banned was imported, nothing but the spec's
    hosts was tried; and the controls show the firewall was enforcing all along."""
    assert report["ready_counts"] == {} and report["stopped_counts"] == {}, (report, log[-3000:])
    assert "firewall: refused" not in log.split("lite: stopped")[0]
    assert sorted(os.listdir(box["marks"])) == []
    assert report["ready_banned"] == [] and report["banned"] == []
    assert set(report["asked"]) <= EGRESS, report["asked"]
    assert not report["sock_left"]
    assert CANARY_KEY not in log
    # positive controls
    assert report["control_exec"] == "refused" and report["control_open"] == "refused"
    assert {"exec:true", "path:~/Documents"} <= set(report["control_counts"])
    assert "firewall: on (enforce)" in log


def test_a_ready_boot_trips_the_firewall_nowhere(box: dict[str, Path]) -> None:
    config = box["home"] / ".config" / "eve-bridge"
    config.mkdir(parents=True)
    (config / "web.json").write_text(json.dumps({"version": 1, "lan": True}))          # a phone was added before
    (config / "composio-connect.json").write_text(json.dumps({
        "version": 1, "client_id": "eve-lite-test-client", "access_token": "placeholder-access",
        "refresh_token": "placeholder-refresh", "expires_at": 4102444800}))
    os.chmod(config / "composio-connect.json", 0o600)
    report, log = boot(box, "ready", key=True)
    assert report["exit"] == 0
    assert report["state"] == "READY"
    status = report["status"]
    assert status["state"] == "READY" and status["openrouter"] is True and status["composio"] is True
    assert status["page"] is True and status["lan"] is True and status["lan_port"] > 0 and status["voice"] is True
    assert status["firewall"] == {}
    assert report["health"] == {"ok": True, "state": "READY"} and report["index"] == 200
    assert report["signed_in"] == 200 and report["web_status"] == 200 and report["devices"] == 1
    assert report["sock_mode"] == 0o600
    assert_clean(report, box, log)


def test_a_setup_boot_signs_in_and_restarts_with_exit_75(box: dict[str, Path]) -> None:
    report, log = boot(box, "sign-in", key=False)
    assert report["status"]["state"] == "NEEDS_OPENROUTER" and report["status"]["openrouter"] is False
    assert report["state"] == "NEEDS_OPENROUTER"
    assert report["callback"] == 303
    assert report["exit"] == 75                                           # launchd starts it again, into READY
    env_file = box["home"] / ".config" / "eve-bridge" / "env"
    assert f"OPENROUTER_API_KEY={CANARY_KEY}" in env_file.read_text(encoding="utf-8").splitlines()
    assert stat.S_IMODE(os.stat(env_file).st_mode) == 0o600
    assert report["asked"] == []                                          # the exchange went to the stand-in only
    assert_clean(report, box, log)
