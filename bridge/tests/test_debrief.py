"""Debrief (eve_bridge/debrief.py, docs/debrief.md): eve searches and reads the owner's debrief store."""

from __future__ import annotations

import asyncio
import json
import re
from pathlib import Path

import pytest

import eve_bridge.debrief as app_mod
from eve_bridge import app_registry, tool_gate

# A stand-in for the store's search script: it echoes its argv as JSON, so a test sees exactly what eve passed.
STUB = """import json, sys
args = sys.argv[1:]
if "fail" in args:
    sys.exit(3)
if "slow" in args:
    import time; time.sleep(5)
print(json.dumps(args))
print("progress", file=sys.stderr)
"""


@pytest.fixture()
def store(tmp_path: Path) -> Path:
    (tmp_path / ".system").mkdir()
    (tmp_path / app_mod.SCRIPT).write_text(STUB)
    return tmp_path


def on(store: Path) -> dict[str, str]:
    return {"EVE_DEBRIEF": "1", "EVE_DEBRIEF_DIR": str(store)}


def argv(result: dict) -> list[str]:
    assert result["ok"], result
    return json.loads(result["text"])


def test_off_by_default_on_with_its_switch_and_a_store(store: Path, tmp_path: Path) -> None:
    assert app_mod.make({}) is None
    assert app_mod.make({"EVE_DEBRIEF": "0", "EVE_DEBRIEF_DIR": str(store)}) is None
    assert app_mod.make(on(store)) is not None
    assert app_mod.make({"EVE_DEBRIEF": "1", "EVE_DEBRIEF_DIR": str(tmp_path / "none")}) is None   # no script


def test_the_store_is_found_from_its_variables() -> None:
    assert app_mod.store_dir({"EVE_DEBRIEF_DIR": "/a", "DEBRIEF_DIR": "/b"}) == Path("/a")
    assert app_mod.store_dir({"DEBRIEF_DIR": "/b"}) == Path("/b")
    assert app_mod.store_dir({}) == Path(app_mod.DEFAULT_DIR).expanduser()


def test_search_passes_the_query_after_a_double_dash(store: Path) -> None:
    hub = app_mod.make(on(store))
    got = argv(asyncio.run(hub.handle("debrief_search", {"query": "  -voice   pe ", "n": None})))
    assert got == ["--dir", str(store), "search", "-n", str(app_mod.DEFAULT_N), "--", "-voice pe"]
    got = argv(asyncio.run(hub.handle("debrief_search", {"query": "x", "n": 99})))
    assert got[got.index("-n") + 1] == str(app_mod.MAX_N)
    assert asyncio.run(hub.handle("debrief_search", {"query": "  ", "n": None}))["ok"] is False


def test_get_takes_only_ids(store: Path) -> None:
    hub = app_mod.make(on(store))
    assert argv(asyncio.run(hub.handle("debrief_get", {"ids": [757, 744]})))[-3:] == ["get", "757", "744"]
    assert len(argv(asyncio.run(hub.handle("debrief_get", {"ids": list(range(20))})))) == 3 + app_mod.MAX_IDS
    for bad in ([], ["1; rm"], [True], [-1], "757"):
        assert asyncio.run(hub.handle("debrief_get", {"ids": bad}))["ok"] is False


def test_a_failure_or_a_timeout_is_a_reason(store: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    hub = app_mod.make(on(store))
    assert asyncio.run(hub.handle("debrief_search", {"query": "fail", "n": 1}))["ok"] is False
    monkeypatch.setattr(app_mod, "TIMEOUT_SECS", 0.5)
    out = asyncio.run(hub.handle("debrief_search", {"query": "slow", "n": 1}))
    assert out["ok"] is False and "longer" in out["reason"]


def test_long_output_is_clipped() -> None:
    assert len(app_mod._clip("x" * (app_mod.MAX_OUT_CHARS + 50))) < app_mod.MAX_OUT_CHARS + 20


def test_an_unknown_tool_is_refused(store: Path) -> None:
    assert asyncio.run(app_mod.make(on(store)).handle("debrief_explode", {}))["ok"] is False


def test_its_tools_are_its_own_and_strict(store: Path) -> None:
    hub = app_mod.make(on(store))
    names = tuple(t["name"] for t in hub.tools())
    assert names == app_mod.TOOL_NAMES == ("debrief_search", "debrief_get")
    for tool in hub.tools():
        params = tool["parameters"]
        assert params["additionalProperties"] is False and set(params["required"]) == set(params["properties"])
    named = set(re.findall(r"\bdebrief_[a-z_]+", hub.instructions()))
    assert named and named <= set(app_mod.TOOL_NAMES)


def test_it_is_registered_as_reads_that_taint() -> None:
    app = app_registry.get("debrief")
    assert app is not None and app_registry.check(app) == []
    assert app.switch == "EVE_DEBRIEF" and not app.default_on and not app.acting and not app.clean
    assert app.policies() == {"debrief_search": "read", "debrief_get": "read"}
    for name in app_mod.TOOL_NAMES:      # a note can quote outside words: never clean, never asks
        assert name not in tool_gate.CLEAN and tool_gate.policy(name) == "read"


@pytest.mark.skipif(not (app_mod.store_dir() / app_mod.SCRIPT).is_file(), reason="no debrief store on this Mac")
def test_the_real_store_answers_a_search() -> None:
    hub = app_mod.Debrief(app_mod.store_dir())
    out = asyncio.run(hub.handle("debrief_search", {"query": "eve", "n": 2}))
    assert out["ok"] and re.search(r"^\[\d+\]", out["text"], re.M)


def test_the_phone_sheet_is_a_status_line(store: Path) -> None:
    (store / "2026-11-08-a.md").write_text("x")
    hub = app_mod.make(on(store))
    sheet = asyncio.run(hub.panel({"action": "status"}))
    assert app_registry.check_panel(sheet) == [] and sheet["status"] == "On" and "1 notes" in sheet["line"]
    assert asyncio.run(hub.panel({"action": "power", "on": True}))["ok"] is False
