"""polar_lane: browser-only web tasks go to Polar's own agent through its local dispatch listener. Polar only:
no fallback to Steel or Codex, a consequential task asks the owner first, everything else goes read-only."""

from __future__ import annotations

import asyncio
import io
import json
import socket
import threading
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import pytest

from eve_bridge import polar_lane
from eve_bridge.polar_lane import PolarAgent, PolarClient, PolarConfig, PolarError


def cfg(tmp: Path = Path("/nonexistent"), timeout: float = 60.0) -> PolarConfig:
    return PolarConfig(enabled=True, data_dir=tmp, app="/Applications/Polar.app", timeout_secs=timeout)


def text(role: str, body: str) -> dict[str, Any]:
    return {"data": {"role": role, "content": [{"type": "text", "text": body}]}}


def tool(name: str) -> dict[str, Any]:
    return {"data": {"role": "assistant", "content": [{"type": "tool_call", "name": name, "arguments": {}}]}}


class FakeClient:
    """Polar's client, scripted: the run's statuses, then its transcript."""

    def __init__(self, statuses: list[str], messages: list[dict[str, Any]], *, dispatch_error: str = "") -> None:
        self.statuses, self.msgs = list(statuses), messages
        self.sent: list[str] = []
        self.stopped: list[str] = []
        self.dispatch_error = dispatch_error

    def dispatch(self, query: str) -> str:
        if self.dispatch_error:
            raise PolarError(self.dispatch_error)
        self.sent.append(query)
        return "th-1"

    def run_status(self, tid: str) -> str:
        return self.statuses.pop(0) if len(self.statuses) > 1 else self.statuses[0]

    def messages(self, tid: str) -> list[dict[str, Any]]:
        return self.msgs

    def stop(self, tid: str) -> None:
        self.stopped.append(tid)


def agent(client: Any, *, ask: Any = None, ready: Any = None, timeout: float = 60.0) -> tuple[PolarAgent, list]:
    events: list = []
    a = PolarAgent(cfg(timeout=timeout), events.append, ask, client=client, ready=ready or (lambda: None),
                   poll_secs=0.01)
    return a, events


def test_a_read_only_task_goes_with_the_read_only_line_and_returns_polars_answer() -> None:
    c = FakeClient(["running", "completed"], [text("user", "q"), tool("create_tab"), text("assistant", "Example Domain")])
    a, events = agent(c)
    assert asyncio.run(a.run("open example.com in the browser and tell me the page title")) == "Example Domain"
    assert len(c.sent) == 1 and c.sent[0].endswith(polar_lane.READ_ONLY)
    kinds = [e.kind for e in events]
    assert kinds[0] == "started" and kinds[-1] == "final" and "progress" in kinds
    assert any("create tab" in e.text for e in events if e.kind == "progress")


def test_a_consequential_task_asks_first_and_a_no_sends_nothing() -> None:
    asked: list[str] = []

    async def ask(q: str) -> str:
        asked.append(q)
        return "no"

    c = FakeClient(["completed"], [text("assistant", "done")])
    a, _ = agent(c, ask=ask)
    out = asyncio.run(a.run("buy the cheapest AA batteries on amazon"))
    assert asked and "Polar" in asked[0] and out.startswith("Not done")
    assert c.sent == []


def test_a_consequential_task_with_a_yes_goes_with_the_approved_line() -> None:
    async def ask(q: str) -> str:
        return "yes"

    c = FakeClient(["completed"], [text("assistant", "posted")])
    a, _ = agent(c, ask=ask)
    assert asyncio.run(a.run("post 'hello' on my twitter")) == "posted"
    assert c.sent[0].endswith(polar_lane.ALLOWED) and polar_lane.READ_ONLY not in c.sent[0]


def test_a_consequential_task_with_nobody_to_ask_is_not_done() -> None:
    c = FakeClient(["completed"], [text("assistant", "sent")])
    a, _ = agent(c, ask=None)
    assert asyncio.run(a.run("send an email to the landlord")).startswith("Not done")
    assert c.sent == []


def test_a_run_past_its_time_is_stopped() -> None:
    c = FakeClient(["running"], [tool("create_tab")])
    a, events = agent(c, timeout=0.05)
    a.cfg = PolarConfig(enabled=True, data_dir=Path("/x"), app="", timeout_secs=0.05)
    out = asyncio.run(a.run("open example.com and read it"))
    assert "didn't finish" in out and c.stopped == ["th-1"] and events[-1].kind == "error"


def test_cancel_stops_polars_run() -> None:
    c = FakeClient(["running"], [])
    a, events = agent(c)

    async def go() -> str:
        job = asyncio.create_task(a.run("open example.com and read it"))
        await asyncio.sleep(0.05)
        a.cancel("owner said stop")
        return await job

    assert asyncio.run(go()) == "Stopped." and c.stopped == ["th-1"] and events[-1].kind == "cancelled"


def test_polar_unreachable_says_why_and_never_falls_back() -> None:
    def not_ready() -> None:
        raise PolarError("Polar is not signed in. Open Polar and sign in")

    c = FakeClient(["completed"], [])
    a, events = agent(c, ready=not_ready)
    out = asyncio.run(a.run("open example.com and read it"))
    assert "not signed in" in out and c.sent == [] and events[-1].kind == "error"


def test_a_failed_run_reports_its_status() -> None:
    c = FakeClient(["failed"], [text("assistant", "The site blocked me.")])
    a, _ = agent(c)
    out = asyncio.run(a.run("open example.com and read it"))
    assert "failed" in out and "blocked" in out


def test_polar_gone_mid_run_is_reported_not_waited_out(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(polar_lane, "POLL_GIVE_UP_SECS", 0.05)

    class Gone(FakeClient):
        def run_status(self, tid: str) -> str:
            raise PolarError("Polar's eve connection is not answering")

    c = Gone(["running"], [])
    a, events = agent(c, timeout=60)
    out = asyncio.run(a.run("open example.com and read it"))
    assert out.startswith("Lost touch with Polar") and events[-1].kind == "error"


# ---- the real client, against a fake listener -------------------------------------------------


class Listener:
    """A TCP listener speaking Polar's protocol: one JSON line in, one JSON line out."""

    def __init__(self, tmp: Path, replies: dict[str, Any]) -> None:
        self.replies, self.got = replies, []
        self.sock = socket.socket()
        self.sock.bind(("127.0.0.1", 0))
        self.sock.listen()
        (tmp / "local-dispatch.json").write_text(json.dumps(
            {"host": "127.0.0.1", "port": self.sock.getsockname()[1], "pid": 1, "authToken": "TOKEN-123"}))
        threading.Thread(target=self.serve, daemon=True).start()

    def serve(self) -> None:
        while True:
            try:
                conn, _ = self.sock.accept()
            except OSError:
                return
            with conn:
                buf = b""
                while not buf.endswith(b"\n"):
                    buf += conn.recv(65536)
                req = json.loads(buf)
                self.got.append(req)
                conn.sendall(json.dumps(self.replies[req["method"]]).encode() + b"\n")


def test_the_wire_format_carries_the_token_and_reads_results(tmp_path: Path) -> None:
    lst = Listener(tmp_path, {
        "status": {"result": {"isAuthenticated": True, "isReady": True}},
        "dispatch": {"result": {"threadId": "th-9", "runId": "run-1", "status": "queued"}},
        "token": {"result": {"apiBaseURL": "https://polar.example", "token": "JWT"}},
        "stop": {"error": {"message": "nothing running"}},
    })
    seen: list[Any] = []

    class Resp(io.BytesIO):
        def __enter__(self) -> "Resp":
            return self

        def __exit__(self, *a: Any) -> None:
            pass

    def urlopen(req: Any, timeout: float = 0) -> Resp:
        seen.append(req)
        if req.full_url.endswith("/messages"):
            return Resp(json.dumps({"messages": [text("assistant", "hi")]}).encode())
        return Resp(json.dumps({"latest_run_status": "completed"}).encode())

    c = PolarClient(tmp_path, urlopen=urlopen)
    assert c.status()["isReady"]
    assert c.dispatch("open example.com") == "th-9"
    assert lst.got[1] == {"authToken": "TOKEN-123", "method": "dispatch", "query": "open example.com"}
    assert c.run_status("th-9") == "completed"
    assert polar_lane.assistant_parts(c.messages("th-9"))[0] == ["hi"]
    assert seen[0].full_url == "https://polar.example/api/v1/threads/th-9"
    assert seen[0].get_header("Authorization") == "Bearer JWT"
    assert seen[0].get_header("User-agent") == polar_lane.USER_AGENT        # Cloudflare refuses Python's default
    with pytest.raises(PolarError, match="refused stop") as err:
        c.stop("th-9")
    assert "TOKEN" not in str(err.value)
    lst.sock.close()


def test_no_listener_file_is_a_plain_polar_error(tmp_path: Path) -> None:
    with pytest.raises(PolarError, match="not open"):
        PolarClient(tmp_path).status()


# ---- starting Polar ----------------------------------------------------------------------------


class Status:
    def __init__(self, answers: list[Any]) -> None:
        self.answers = answers

    def status(self) -> dict[str, Any]:
        a = self.answers.pop(0) if len(self.answers) > 1 else self.answers[0]
        if isinstance(a, Exception):
            raise a
        return a


def test_polar_not_running_is_started_with_the_connection() -> None:
    ran: list[list[str]] = []

    def run(cmd: list[str], **kw: Any) -> Any:
        ran.append(cmd)
        return SimpleNamespace(stdout="")                  # pgrep: Polar is not running

    client = Status([PolarError("not open"), PolarError("not open"), {"isAuthenticated": True, "isReady": True}])
    polar_lane.ensure_ready(cfg(Path("/tmp/polar-x")), client, run=run, sleep=lambda s: None)  # type: ignore[arg-type]
    assert ran[-1] == ["open", "-g", "--env", "POLAR_USER_DATA_DIR=/tmp/polar-x", "-a", "/Applications/Polar.app"]


def test_polar_open_without_the_connection_is_never_quit() -> None:
    ran: list[list[str]] = []

    def run(cmd: list[str], **kw: Any) -> Any:
        ran.append(cmd)
        return SimpleNamespace(stdout="52832\n")           # pgrep: the owner's Polar is up

    with pytest.raises(PolarError, match="Quit Polar"):
        polar_lane.ensure_ready(cfg(), Status([PolarError("not open")]), run=run)  # type: ignore[arg-type]
    assert all(c[0] == "pgrep" for c in ran)


def test_a_polar_just_started_is_waited_for_until_ready() -> None:
    client = Status([{"isAuthenticated": True, "isReady": False}, {"isAuthenticated": True, "isReady": False},
                     {"isAuthenticated": True, "isReady": True}])
    polar_lane.ensure_ready(cfg(), client, sleep=lambda s: None)  # type: ignore[arg-type]
    assert client.answers == [{"isAuthenticated": True, "isReady": True}]


def test_a_polar_never_ready_says_so_after_the_wait() -> None:
    t = iter(range(0, 1000, 10))
    with pytest.raises(PolarError, match="not ready"):
        polar_lane.ensure_ready(cfg(), Status([{"isAuthenticated": True, "isReady": False}]),  # type: ignore[arg-type]
                                sleep=lambda s: None, clock=lambda: float(next(t)))


def test_signed_out_polar_says_so() -> None:
    with pytest.raises(PolarError, match="not signed in"):
        polar_lane.ensure_ready(cfg(), Status([{"isAuthenticated": False, "isReady": True}]))  # type: ignore[arg-type]


# ---- the daemon's wiring -----------------------------------------------------------------------


def test_wiring_polar_on_routes_browser_only_goals_to_polar(monkeypatch: pytest.MonkeyPatch) -> None:
    from eve_bridge.daemon import Daemon

    monkeypatch.setenv("EVE_BROWSER_POLAR", "1")
    host = SimpleNamespace(_chrome_lane=None, _telegram=None)
    wiring = Daemon._chrome_body(host, lambda: None, lambda ev: None, None)
    route = wiring["route_body"]
    assert asyncio.run(route("search amazon for AA batteries")) == "polar"
    assert asyncio.run(route("open spotify and play jazz")) == "codex"
    assert wiring["make_auto"]().provider == "polar"


def test_wiring_polar_on_builds_no_steel_or_chrome_lane(monkeypatch: pytest.MonkeyPatch) -> None:
    from eve_bridge.daemon import Daemon

    monkeypatch.setenv("EVE_BROWSER_POLAR", "1")
    monkeypatch.setenv("EVE_BROWSER_STEEL", "1")
    assert Daemon._make_chrome_lane(SimpleNamespace()) is None


def test_wiring_polar_off_changes_nothing(monkeypatch: pytest.MonkeyPatch) -> None:
    from eve_bridge.daemon import Daemon

    monkeypatch.delenv("EVE_BROWSER_POLAR", raising=False)
    assert Daemon._chrome_body(SimpleNamespace(_chrome_lane=None), lambda: None, lambda ev: None, None) == {}


def test_wiring_a_reading_task_the_web_reader_cannot_answer_goes_to_polar_not_codex(
        monkeypatch: pytest.MonkeyPatch) -> None:
    """Owner, 2026-10-24: "only browser for when it can't use anything else". Search and the web reader come first;
    with Polar on, the browser they fall back to is Polar's agent. With Polar off, the fallback stays Codex."""
    from eve_bridge import web_reader
    from eve_bridge.daemon import Daemon

    import dataclasses

    real = web_reader.configured({})
    monkeypatch.setattr(web_reader, "configured", lambda *a, **k: dataclasses.replace(real, enabled=True))
    codex = object()
    host = SimpleNamespace(_chrome_lane=None, _telegram=None, _reader_router=lambda goal: "web")

    monkeypatch.setenv("EVE_BROWSER_POLAR", "1")
    wiring = Daemon._bodies(host, lambda: codex, lambda ev: None, None)
    assert asyncio.run(wiring["route_body"]("what are the opening hours on spl.org")) == "web"
    assert wiring["bodies"]["web"]()._make_fallback().provider == "polar"

    monkeypatch.delenv("EVE_BROWSER_POLAR")
    wiring = Daemon._bodies(host, lambda: codex, lambda ev: None, None)
    assert wiring["bodies"]["web"]()._make_fallback() is codex
