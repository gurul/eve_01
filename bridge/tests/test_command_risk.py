"""The Auto Mode gate: Jev judges a relayed Bash command before it runs (typed_ask.py, daemon.py)."""

from __future__ import annotations

import asyncio
from types import MethodType, SimpleNamespace
from typing import Any

from eve_bridge import typed_ask as ta
from eve_bridge.daemon import Daemon


def answers(**nouls: float) -> dict[str, Any]:
    return {"answers": {k: {"type": "noul", "noul": v} for k, v in nouls.items()}}


def test_the_questions_are_absolute_nouls_over_a_redacted_command() -> None:
    q = ta.jev_command_questions()
    assert set(q) == {"destroys", "escapes", "publishes", "secrets"}
    assert all(v["type"] == "noul" and "Do NOT count" in v["instructions"] for v in q.values())
    seen: list[tuple[Any, dict[str, Any]]] = []

    def predict(state: Any, questions: dict[str, Any]) -> dict[str, Any]:
        seen.append((state, questions))
        return answers(destroys=0.9, escapes=0.1, publishes=0.2, secrets=0.0)

    a = ta.ask_jev_command(predict, "Bash", "rm -rf build/  --token=abcdefghijkl", "/Users/g/secret-repo",
                           clock=lambda: 0.0)
    state, questions = seen[0]
    assert questions == q
    assert state["tool"] == "Bash" and state["folder"] == "secret-repo" and "/Users" not in str(state)   # the name, never the path
    assert state["command"] == "rm -rf build/ --token=<redacted>"                      # whitespace folded, secret gone
    assert state["about"] == ta.COMMAND_ABOUT
    assert (a.destroys, a.escapes, a.publishes, a.secrets, a.error) == (0.9, 0.1, 0.2, 0.0, "")


def test_secrets_never_leave_in_a_command() -> None:
    red = ta.redact_command
    assert red('curl -H "Authorization: Bearer abcdefgh12345678" https://api.example.com') == \
        'curl -H "Authorization: Bearer <redacted>" https://api.example.com'
    assert red("export OPENAI_API_KEY=sk-abcdefghijklmnop && python x.py") == "export OPENAI_API_KEY=<redacted> && python x.py"
    assert red("AWS_SECRET_ACCESS_KEY='abc/def+ghi' aws s3 ls") == "AWS_SECRET_ACCESS_KEY=<redacted> aws s3 ls"
    assert red("gh auth login --with-token ghp_abcdefghijklmnopqrstuvwxyz") == "gh auth login --with-token <redacted>"
    assert red("mysql --password=hunter2 db") == "mysql --password=<redacted> db"
    assert red("psql -p 5432 --password mypw") == "psql -p 5432 --password <redacted>"
    # positive control: an ordinary command is exactly itself, so the redactor is not hiding everything
    for plain in ("pytest -q tests/", "git commit -m 'fix KEY handling'", "ls -la ~/.config", "cat .env.example"):
        assert red(plain) == plain
    # a long blob (a SHA, a JWT) goes; the judgement does not depend on it
    assert red("git log 1234567890abcdef1234567890abcdef12345678") == "git log <redacted>"


def test_any_gate_reached_is_risky_and_an_error_is_unknown() -> None:
    g = ta.CommandGates(destroys=0.7, escapes=0.7, publishes=0.7, secrets=0.7)
    safe = ta.CommandAnswer(0.1, 0.2, 0.3, 0.05)
    assert ta.decide_command(safe, g) == ta.CommandVerdict("safe", "", safe)
    two = ta.CommandAnswer(0.9, 0.1, 0.75, 0.0)
    v = ta.decide_command(two, g)
    assert v.decision == "risky" and v.why == "destroys data, publishes or sends"
    edge = ta.CommandAnswer(0.0, 0.7, 0.0, 0.0)                       # at the gate counts
    assert ta.decide_command(edge, g).decision == "risky"
    err = ta.CommandAnswer(0.0, 0.0, 0.0, 0.0, error="HTTP 503")
    assert ta.decide_command(err, g) == ta.CommandVerdict("unknown", "HTTP 503", err)

    def boom(state: Any, questions: dict[str, Any]) -> dict[str, Any]:
        raise RuntimeError("no network")

    a = ta.ask_jev_command(boom, "Bash", "ls", "/r", clock=lambda: 0.0)
    assert a.error.startswith("RuntimeError") and ta.decide_command(a, g).decision == "unknown"
    assert ta.ask_jev_command(lambda s, q: {}, "Bash", "ls", "/r", clock=lambda: 0.0).error == "predict returned no answers"
    asker = ta.make_jev_command_asker(lambda s, q: answers(destroys=0.0, escapes=0.0, publishes=0.95, secrets=0.0),
                                      clock=lambda: 0.0, gates=g)
    assert asker("Bash", "git push --force", "/r").decision == "risky"


def test_fit_command_gates_allows_no_miss() -> None:
    risky = [ta.CommandAnswer(0.6, 0.0, 0.0, 0.0), ta.CommandAnswer(0.0, 0.0, 0.85, 0.0)]
    safe = [ta.CommandAnswer(0.5, 0.1, 0.0, 0.0), ta.CommandAnswer(0.0, 0.0, 0.8, 0.0)]
    g = ta.fit_command_gates(risky + safe, [True, True, False, False])
    assert all(ta.decide_command(a, g).decision == "risky" for a in risky)
    assert g.destroys == 0.6 and g.publishes == 0.8          # the strictest gate that still catches every risky one
    assert g.escapes == 0.9 and g.secrets == 0.9             # unused properties sit high: they cry no wolf
    assert ta.decide_command(safe[1], g).decision == "risky"  # the one unavoidable wolf: 0.8 sits under the 0.85 miss


# ---- the daemon ------------------------------------------------------------------------------------------

class Inlet:
    def __init__(self, decision: Any = None, on: bool = True) -> None:
        self.claude, self.decision, self.asked = on, decision, []

    def relay_tool_call(self, tool: str, hint: str) -> None:
        pass

    async def decide_permission(self, tool: str, hint: str, cwd: str = "", *, always: bool = False) -> Any:
        self.asked.append((tool, hint, cwd, always))
        return self.decision


class Audit:
    def __init__(self) -> None:
        self.rows: list[dict[str, Any]] = []

    def record(self, **kw: Any) -> None:
        self.rows.append(kw)


def daemon_with(inlet: Inlet, risk: Any, classify: str = "default") -> Any:
    d = SimpleNamespace(_telegram=inlet, audit=Audit(), matchers=SimpleNamespace(),
                        state=SimpleNamespace(note_tool=lambda *a: None),
                        ble=SimpleNamespace(connected=False), _ensure_session=lambda req: None,
                        _command_risk=lambda: risk)
    d._handle_pretooluse = MethodType(Daemon._handle_pretooluse, d)
    d._shadow_command_risk = MethodType(Daemon._shadow_command_risk, d)
    return d, classify


REQ = {"tool_use_id": "t1", "session_id": "s1", "tool_name": "Bash", "hint": "git push --force origin main", "cwd": "/r"}


def run(d: Any, classify: str, req: dict[str, Any]) -> dict[str, Any]:
    import eve_bridge.daemon as dm

    original = dm.classify_command
    dm.classify_command = lambda hint, matchers: classify
    try:
        async def go() -> dict[str, Any]:
            out = await d._handle_pretooluse(req)
            pending = [t for t in asyncio.all_tasks() if t is not asyncio.current_task()]
            if pending:
                await asyncio.gather(*pending)
            return out
        return asyncio.run(go())
    finally:
        dm.classify_command = original


def asker_saying(decision: str, why: str = "", calls: list[Any] | None = None) -> Any:
    def ask(tool: str, command: str, cwd: str) -> ta.CommandVerdict:
        if calls is not None:
            calls.append((tool, command, cwd))
        a = ta.CommandAnswer(0.9 if decision == "risky" else 0.1, 0.0, 0.0, 0.0, ms=230.0,
                             error="HTTP 503" if decision == "unknown" else "")
        return ta.CommandVerdict(decision, why or ("HTTP 503" if decision == "unknown" else ""), a)
    return ask


def test_off_is_todays_relay() -> None:
    d, classify = daemon_with(Inlet(None), None)
    assert run(d, classify, REQ) == {"ok": True, "decision": "allow"}
    assert d.audit.rows[-1]["source"] == "telegram_relay" and d._telegram.asked == []
    # bypass mode with the gate off: no decision, as before
    d, classify = daemon_with(Inlet(None), None)
    assert run(d, classify, {**REQ, "permission_mode": "bypassPermissions"}) == {"ok": True}
    assert d.audit.rows[-1]["source"] == "ble_disconnected"


def test_shadow_allows_and_logs_the_verdict() -> None:
    calls: list[Any] = []
    d, classify = daemon_with(Inlet(None), ("shadow", asker_saying("risky", "destroys data", calls)))
    assert run(d, classify, REQ) == {"ok": True, "decision": "allow"}
    assert calls == [("Bash", "git push --force origin main", "/r")] and d._telegram.asked == []
    sources = [r["source"] for r in d.audit.rows]
    assert sources == ["telegram_relay", "jev_shadow"]
    shadow = d.audit.rows[-1]
    assert shadow["decision"] is None and shadow["jev"]["verdict"] == "risky" and shadow["jev"]["destroys"] == 0.9
    assert shadow["jev"]["why"] == "destroys data" and shadow["jev"]["ms"] == 230


def test_ask_routes_a_risky_command_to_the_phone() -> None:
    for decision, expect in (("allow", {"ok": True, "decision": "allow"}), ("deny", {"ok": True, "decision": "deny"})):
        d, classify = daemon_with(Inlet(decision), ("ask", asker_saying("risky", "destroys data, publishes or sends")))
        assert run(d, classify, {**REQ, "permission_mode": "bypassPermissions"}) == expect   # bypass too
        assert d._telegram.asked == [("Bash", "git push --force origin main [Jev: destroys data, publishes or sends]",
                                      "/r", True)]
        assert d.audit.rows[-1]["source"] == "telegram" and d.audit.rows[-1]["jev"]["verdict"] == "risky"
    # silence defers to Claude Code's own flow, never denies
    d, classify = daemon_with(Inlet(None), ("ask", asker_saying("risky", "destroys data")))
    assert run(d, classify, REQ) == {"ok": True}
    assert d.audit.rows[-1]["source"] == "jev_risky_deferred" and d.audit.rows[-1]["decision"] is None
    # a safe verdict is allowed as before; in bypass mode Claude Code allows on its own (no decision)
    d, classify = daemon_with(Inlet("deny"), ("ask", asker_saying("safe")))
    assert run(d, classify, {**REQ, "hint": "pytest -q"}) == {"ok": True, "decision": "allow"}
    assert d._telegram.asked == [] and d.audit.rows[-1]["source"] == "jev_safe"
    d, classify = daemon_with(Inlet("deny"), ("ask", asker_saying("safe")))
    assert run(d, classify, {**REQ, "hint": "pytest -q", "permission_mode": "bypassPermissions"}) == {"ok": True}
    assert d.audit.rows[-1]["source"] == "jev_safe" and d.audit.rows[-1]["decision"] is None
    # the model failing is allowed and logged, never a denial and never a traceback
    d, classify = daemon_with(Inlet("deny"), ("ask", asker_saying("unknown")))
    assert run(d, classify, REQ) == {"ok": True, "decision": "allow"}
    assert d._telegram.asked == [] and d.audit.rows[-1]["source"] == "jev_error"


def test_the_regex_class_asks_before_jev_is_consulted() -> None:
    calls: list[Any] = []
    d, classify = daemon_with(Inlet("allow"), ("ask", asker_saying("safe", calls=calls)), classify="ask")
    assert run(d, classify, {**REQ, "hint": "sudo rm -rf /"}) == {"ok": True, "decision": "allow"}
    assert calls == [] and d._telegram.asked == [("Bash", "sudo rm -rf /", "/r", True)]
    assert d.audit.rows[-1]["source"] == "telegram"
    # the regex allow class short-circuits before any of this
    d, classify = daemon_with(Inlet("deny"), ("ask", asker_saying("risky", calls=calls)), classify="allow")
    assert run(d, classify, {**REQ, "hint": "ls"}) == {"ok": True, "decision": "allow"}
    assert calls == [] and d.audit.rows[-1]["source"] == "auto_allow"
    # a tool that is not Bash is never judged
    calls.clear()
    d, classify = daemon_with(Inlet(None), ("ask", asker_saying("risky", calls=calls)))
    assert run(d, classify, {**REQ, "tool_name": "Edit", "hint": "src/app.py"}) == {"ok": True, "decision": "allow"}
    assert calls == []
