"""The permission path as a golden master: every combination of its inputs, and exactly what it did.

daemon._handle_pretooluse / _handle_read_pretooluse / _handle_permission_request decide whether a tool call
runs. Their inputs are few and discrete — the tool, the matcher class of the command, the Telegram relay,
the permission mode, the robot link, the Jev risk gate's mode and verdict, the owner's phone answer, and
"ask every call" — so the whole cross product is small enough to run in milliseconds. For each case the
test records the response, the audit entries, what reached the phone, and what was relayed, and compares
them with tests/fixtures/permission_matrix.json.gz (gzip, for size), which was recorded from the code before the 2026-09-23
dispatch refactor. Any behaviour change in this security path fails here, case by case.

Regenerate (only for a deliberate policy change, reviewed in the diff):
    EVE_WRITE_GOLDEN=1 .venv/bin/python -m pytest tests/test_permission_matrix.py
"""

from __future__ import annotations

import asyncio
import gzip
import itertools
import json
import os
import re
from pathlib import Path
from types import MethodType, SimpleNamespace
from typing import Any, Optional

from eve_bridge.daemon import Daemon
from eve_bridge.matchers import MatcherConfig

GOLDEN = Path(__file__).parent / "fixtures" / "permission_matrix.json.gz"

TOOLS = ("Bash", "Edit", "AskUserQuestion")
HINTS = ("ls -la", "rm -rf build", "make build", "")
RELAY = (False, True)
MODES = ("default", "bypassPermissions")
LINK = (True, False)
RISK = ("none", "shadow", "ask-safe", "ask-risky", "ask-error", "ask-raise")
ANSWERS = ("allow", "deny", None)
ASK_ALL = (False, True)
READ_PATHS = ("/repo/src/a.py", "/elsewhere/b.py", "")


class Relay:
    def __init__(self, on: bool, answer: Optional[str], ask_all: bool) -> None:
        self.claude = on
        self.answer = answer
        self.config = SimpleNamespace(ask_permissions=ask_all)
        self.relayed: list[list[str]] = []
        self.asked: list[list[str]] = []

    def relay_tool_call(self, tool: str, hint: str) -> None:
        self.relayed.append([tool, hint])

    async def decide_permission(self, tool: str, hint: str, cwd: str = "", *, always: bool = False) -> Optional[str]:
        self.asked.append([tool, hint, str(always)])
        return self.answer


def _verdict(kind: str) -> Any:
    return SimpleNamespace(decision=kind, why=f"because {kind}",
                           answer=SimpleNamespace(nouls=lambda: {"destroys": 0.5}, ms=12.0))


def _host(relay: Relay, connected: bool, risk: str) -> SimpleNamespace:
    audit: list[list[Any]] = []
    shadow: list[str] = []

    def record(**kw: Any) -> None:
        audit.append([kw.get("decision"), kw.get("source"), kw.get("matcher"), "jev" in kw])

    risk_value: Any = None
    if risk == "shadow":
        risk_value = ("shadow", lambda t, h, c: _verdict("safe"))
    elif risk == "ask-raise":
        def raises(t: str, h: str, c: str) -> Any:
            raise TimeoutError("the risk model timed out")
        risk_value = ("ask", raises)
    elif risk.startswith("ask-"):
        risk_value = ("ask", lambda t, h, c, k=risk[4:]: _verdict(k))

    h = SimpleNamespace(
        _telegram=relay,
        ble=SimpleNamespace(connected=connected),
        matchers=MatcherConfig(auto_allow=(re.compile(r"^ls\b"),), always_ask=(re.compile(r"^rm\b"),)),
        audit=SimpleNamespace(record=record),
        state=SimpleNamespace(note_tool=lambda s, t: None, session_start=lambda s, cwd=None: None, sessions={}),
        _command_risk=lambda: risk_value,
        audit_log=audit,
        shadow_log=shadow,
    )

    async def fake_shadow(self: Any, ask_cmd: Any, tool: str, hint: str, cwd: str, kw: dict) -> None:
        shadow.append(hint)

    h._shadow_command_risk = MethodType(fake_shadow, h)
    for name in ("_handle_pretooluse", "_handle_read_pretooluse", "_handle_permission_request", "_ensure_session"):
        setattr(h, name, MethodType(getattr(Daemon, name), h))
    return h


async def _one(call: str, req: dict[str, Any], relay: Relay, connected: bool, risk: str) -> dict[str, Any]:
    h = _host(relay, connected, risk)
    resp = await getattr(h, call)(req)
    await asyncio.sleep(0)                                  # let a shadow task run
    return {"resp": resp, "audit": h.audit_log, "asked": relay.asked, "relayed": relay.relayed,
            "shadow": h.shadow_log}


def _cases() -> list[tuple[str, str, dict[str, Any], dict[str, Any]]]:
    out = []
    for tool, hint, on, mode, link, risk, answer, ask_all in itertools.product(
            TOOLS, HINTS, RELAY, MODES, LINK, RISK, ANSWERS, ASK_ALL):
        req = {"tool_use_id": "t1", "session_id": "s1", "tool_name": tool, "hint": hint, "cwd": "/repo",
               "permission_mode": mode}
        key = f"pre|{tool}|{hint}|relay={on}|{mode}|link={link}|{risk}|ans={answer}|all={ask_all}"
        out.append((key, "_handle_pretooluse", req, dict(on=on, answer=answer, ask_all=ask_all, link=link, risk=risk)))
    for path, on, mode, link in itertools.product(READ_PATHS, RELAY, MODES, LINK):
        req = {"tool_use_id": "t2", "session_id": "s1", "tool_name": "Read", "hint": path, "cwd": "/repo",
               "permission_mode": mode}
        key = f"read|{path}|relay={on}|{mode}|link={link}"
        out.append((key, "_handle_pretooluse", req, dict(on=on, answer=None, ask_all=False, link=link, risk="none")))
    for tool, on, answer in itertools.product(("Edit", "Bash", "AskUserQuestion"), RELAY, ANSWERS):
        req = {"tool_name": tool, "hint": "/repo/x.py", "cwd": "/repo", "session_id": "s1"}
        key = f"perm|{tool}|relay={on}|ans={answer}"
        out.append((key, "_handle_permission_request", req, dict(on=on, answer=answer, ask_all=False, link=True,
                                                                  risk="none")))
    out.append(("pre|missing-id", "_handle_pretooluse", {"tool_name": "Bash", "hint": "ls"},
                dict(on=False, answer=None, ask_all=False, link=True, risk="none")))
    return out


def _run_all() -> dict[str, Any]:
    async def go() -> dict[str, Any]:
        results = {}
        for key, call, req, o in _cases():
            relay = Relay(o["on"], o["answer"], o["ask_all"])
            results[key] = await _one(call, req, relay, o["link"], o["risk"])
        return results
    return json.loads(json.dumps(asyncio.run(go())))


def _golden() -> dict[str, Any]:
    return json.loads(gzip.decompress(GOLDEN.read_bytes()))


def test_the_permission_path_matches_its_golden_master() -> None:
    got = _run_all()
    if os.environ.get("EVE_WRITE_GOLDEN") == "1":
        GOLDEN.parent.mkdir(parents=True, exist_ok=True)
        GOLDEN.write_bytes(gzip.compress((json.dumps(got, sort_keys=True, separators=(",", ":")) + "\n").encode(), mtime=0))
    want = _golden()
    assert set(got) == set(want), sorted(set(got) ^ set(want))[:5]
    diff = [k for k in want if got[k] != want[k]]
    assert not diff, f"{len(diff)} cases changed, e.g. {diff[0]}: {got[diff[0]]} != {want[diff[0]]}"


def test_the_matrix_covers_every_outcome_it_should() -> None:
    """A golden master that never reaches a branch proves nothing about it: every audited outcome of the path,
    and each response shape, occurs somewhere in the recorded matrix."""
    golden = _golden()
    sources = {(a[0], a[1]) for r in golden.values() for a in r["audit"]}
    for expected in [("allow", "auto_allow"), ("allow", "telegram_relay"), ("allow", "telegram"),
                     ("deny", "telegram"), (None, "ble_disconnected"), (None, "defer"), ("allow", "jev_safe"),
                     (None, "jev_safe"), ("allow", "jev_error_asked"), ("deny", "jev_error_asked"),
                     (None, "jev_error_deferred"), (None, "jev_risky_deferred")]:
        assert expected in sources, expected
    responses = {json.dumps(r["resp"], sort_keys=True) for r in golden.values()}
    for shape in ({"ok": True}, {"ok": True, "decision": "allow"}, {"ok": True, "decision": "deny"},
                  {"ok": False, "error": "missing tool_use_id"}):
        assert json.dumps(shape, sort_keys=True) in responses, shape
    assert any(r["shadow"] for r in golden.values())                 # the shadow gate ran
    assert any(r["relayed"] for r in golden.values())                # a question reached the phone


def test_a_failed_verdict_never_reaches_the_allow_branch() -> None:
    """Fail closed (2026-10-26): when Jev returns an error or the asker raises, a relayed Bash command is allowed
    only by the owner's own "allow" on the phone, which was asked with "[Jev: unavailable]"; silence defers."""
    golden = _golden()
    judged = [(k, r) for k, r in golden.items()
              if k.startswith("pre|Bash|") and ("|ask-error|" in k or "|ask-raise|" in k) and r["audit"]
              and r["audit"][-1][1] in ("jev_error_asked", "jev_error_deferred")]
    assert len(judged) >= 2 * 2 * 3 * 2 * 2          # 2 kinds x 2 modes x 3 answers x 2 links x 2 ask-all, per hint
    for key, r in judged:
        assert r["asked"] and r["asked"][-1][1].endswith(" [Jev: unavailable]"), key
        if "ans=allow" not in key:
            assert r["resp"].get("decision") != "allow", key
        assert all(a[1] != "jev_safe" for a in r["audit"]), key
    assert not any(a[1] == "jev_error" for r in golden.values() for a in r["audit"])   # the old allow is gone
