"""Daemon._handle_ipc's dispatch table: every event reaches its own _ipc_<evt> method through the class."""

from __future__ import annotations

import asyncio
import inspect
from types import MethodType, SimpleNamespace

from eve_bridge import daemon
from eve_bridge.daemon import IPC_HANDLERS, Daemon


def _stub() -> SimpleNamespace:
    d = SimpleNamespace(_note_activity=lambda: None)
    d._handle_ipc = MethodType(Daemon._handle_ipc, d)
    return d


def test_every_entry_is_the_matching_async_method() -> None:
    for evt, handler in IPC_HANDLERS.items():
        assert handler is getattr(Daemon, f"_ipc_{evt}"), evt
        assert inspect.iscoroutinefunction(handler), evt
        assert list(inspect.signature(handler).parameters) == ["self", "req"], evt


def test_unknown_and_malformed_events_are_refused_as_before() -> None:
    d = _stub()
    assert asyncio.run(d._handle_ipc({"evt": "nope"})) == {"ok": False, "error": "unknown evt: 'nope'"}
    assert asyncio.run(d._handle_ipc({})) == {"ok": False, "error": "unknown evt: None"}
    assert asyncio.run(d._handle_ipc({"evt": ["pretooluse"]})) == {"ok": False, "error": "unknown evt: ['pretooluse']"}


def test_dispatch_goes_through_the_class_so_a_stub_needs_only_what_the_handler_uses(monkeypatch) -> None:
    seen = []

    async def fake(self, req):
        seen.append((self, req["evt"]))
        return {"ok": True}

    monkeypatch.setitem(daemon.IPC_HANDLERS, "trace", fake)
    d = _stub()
    assert asyncio.run(d._handle_ipc({"evt": "trace"})) == {"ok": True}
    assert seen == [(d, "trace")]
