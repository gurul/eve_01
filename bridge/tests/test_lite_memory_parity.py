"""lite.build_memory and lite.start_dream are full eve's Daemon._build_memory and Daemon._start_dream, copied
(docs/plans/eve-lite.md M1, WP-9): eve lite never imports daemon.py, so it keeps its own copy of the steps, and this file
holds the copy to the original.

* Statement for statement: each function's body, without its docstring, is the daemon's with what the daemon reads off
  itself (``self._recall_cfg``, ``self._shutdown``, ``self._background``, ``getattr(self, "_memory")``) taken as
  arguments instead. A change to either side fails here until the other follows. Positive control: a copy with one
  step changed is caught.
* File for file: on two temporary homes seeded alike (a retired debrief store and mem0 folder for the move), the daemon's
  steps and lite's build the same memory (the same folders, the same transcripts, the same index) and leave the same
  files, under full eve and under eve lite (where no git runs, so the records keep no history).
* The dream: both start the same Dreamer, held as background work that the shutdown event ends.

The daemon is imported here, in the test process, only to read its two methods. Nothing reaches the network: the
index opens lazily and nothing here searches it, and the dream's client is a stand-in.
"""

from __future__ import annotations

import ast
import asyncio
import copy
import inspect
import json
import textwrap
import types
from pathlib import Path
from typing import Any, Callable, Optional

import pytest

from eve_bridge import dream, lite, recall, records, toolchain
from eve_bridge.daemon import Daemon

# What the daemon reads off itself, and what lite's copies take as arguments instead.
SELF_TO_ARGUMENT = {"_recall_cfg": "cfg", "_shutdown": "shutdown", "_background": "background"}
MEMORY_SWITCHES = ("EVE_MEMORY", "EVE_RECORDS", "EVE_MEMORY_DIR", "EVE_NOTES_DIR", "EVE_DEBRIEF_DIR", "EVE_MEM0",
                   "EVE_MEM0_EMBEDDER", "EVE_MEMORY_HYBRID", "EVE_MEMORY_RETRY", "EVE_MEMORY_TIMEARGS",
                   "EVE_PROFILE", "OPENROUTER_API_KEY")
PLACEHOLDER_KEY = "CANARY-NOT-A-KEY-7f3c"


# ---- statement for statement ---------------------------------------------------------------------------------------

class _Arguments(ast.NodeTransformer):
    """``self._recall_cfg`` → ``cfg`` and the like: what the daemon reads off itself, as lite's argument."""

    def visit_Attribute(self, node: ast.Attribute) -> ast.AST:
        if isinstance(node.value, ast.Name) and node.value.id == "self" and node.attr in SELF_TO_ARGUMENT:
            return ast.copy_location(ast.Name(id=SELF_TO_ARGUMENT[node.attr], ctx=node.ctx), node)
        return self.generic_visit(node)


def _function(fn: Callable[..., Any]) -> ast.FunctionDef:
    tree = ast.parse(textwrap.dedent(inspect.getsource(fn)))
    node = tree.body[0]
    assert isinstance(node, ast.FunctionDef)
    return node


def _reads_itself(stmt: ast.stmt) -> bool:
    """A statement that only takes a part off the daemon: ``cfg = self._recall_cfg``,
    ``memory = getattr(self, "_memory", None)``. lite's copies are handed these as arguments."""
    if not (isinstance(stmt, ast.Assign) and len(stmt.targets) == 1 and isinstance(stmt.targets[0], ast.Name)):
        return False
    value = stmt.value
    if isinstance(value, ast.Attribute):
        return isinstance(value.value, ast.Name) and value.value.id == "self"
    return (isinstance(value, ast.Call) and isinstance(value.func, ast.Name) and value.func.id == "getattr"
            and bool(value.args) and isinstance(value.args[0], ast.Name) and value.args[0].id == "self")


def steps(fn: ast.FunctionDef, *, daemon: bool) -> list[str]:
    """The function's body as comparable statements: no docstring; the daemon's reads of itself left out and its
    attributes written as lite's arguments."""
    body = list(fn.body)
    if body and isinstance(body[0], ast.Expr) and isinstance(body[0].value, ast.Constant) \
            and isinstance(body[0].value.value, str):
        body = body[1:]
    if daemon:
        body = [_Arguments().visit(copy.deepcopy(stmt)) for stmt in body if not _reads_itself(stmt)]
    return [ast.dump(stmt, include_attributes=False) for stmt in body]


def differences(original: ast.FunctionDef, copied: ast.FunctionDef) -> list[str]:
    """The statements where the copy differs from the original, as source text; [] when it is the same."""
    a, b = steps(original, daemon=True), steps(copied, daemon=False)
    if a == b:
        return []
    out = [f"{len(a)} statements in daemon.py, {len(b)} in lite.py"] if len(a) != len(b) else []
    for i, (x, y) in enumerate(zip(a, b, strict=False)):     # the shorter one's length: the count is said above
        if x != y:
            out.append(f"statement {i + 1} differs")
    return out


@pytest.mark.parametrize("original, copied", [(Daemon._build_memory, lite.build_memory),
                                              (Daemon._start_dream, lite.start_dream)],
                         ids=["build_memory", "start_dream"])
def test_the_copy_is_the_daemons_statement_for_statement(original: Callable[..., Any],
                                                         copied: Callable[..., Any]) -> None:
    assert differences(_function(original), _function(copied)) == [], (
        "lite.py's copy no longer matches daemon.py: make the same change on both sides")


def test_the_statement_check_catches_a_changed_step() -> None:
    """Positive controls: the copy with the index worker off, with ensure_repo left out, and with the dream unheld
    each differ from the daemon's."""
    original, copied = _function(Daemon._build_memory), _function(lite.build_memory)

    worker_off = copy.deepcopy(copied)
    for node in ast.walk(worker_off):
        if isinstance(node, ast.keyword) and node.arg == "background":
            node.value = ast.Constant(value=False)
    assert differences(original, worker_off)

    no_repo = copy.deepcopy(copied)
    no_repo.body = [s for s in no_repo.body if "ensure_repo" not in ast.dump(s)]
    assert differences(original, no_repo)

    dream_copy = _function(lite.start_dream)
    unheld = copy.deepcopy(dream_copy)
    unheld.body = [s for s in unheld.body if "discard" not in ast.dump(s)]
    assert differences(_function(Daemon._start_dream), unheld)
    assert differences(original, copied) == []                        # and the real copy is still the same


# ---- file for file --------------------------------------------------------------------------------------------------

def _seed(home: Path) -> None:
    """A home as an older eve left it: the retired debrief store and mem0 folder, which the move takes in."""
    legacy = home / ".config" / "eve-bridge" / "debrief"
    (legacy / "records").mkdir(parents=True)
    (legacy / "records" / "people.md").write_text("# People\n\n- Sam likes green tea.\n", encoding="utf-8")
    (legacy / "sessions").mkdir()
    (legacy / "sessions" / "2026-09-01.md").write_text("a session\n", encoding="utf-8")
    old_mem0 = home / ".config" / "eve-bridge" / "mem0"
    old_mem0.mkdir(parents=True)
    (old_mem0 / "placeholder.txt").write_text("an index file\n", encoding="utf-8")


def _tree(home: Path) -> dict[str, Optional[bytes]]:
    """Every path under ``home``, relative, with each file's bytes (None for a folder). Inside a git folder only the
    names count (git writes its own clock into some of its files), and the move's marker counts without its time."""
    out: dict[str, Optional[bytes]] = {}
    for path in sorted(home.rglob("*")):
        rel = path.relative_to(home).as_posix()
        if path.is_dir():
            out[rel] = None
        elif "/.git/" in f"/{rel}":
            out[rel] = b"<git>"
        elif path.name == ".migrated":
            counts = json.loads(path.read_text(encoding="utf-8"))
            counts.pop("at", None)
            out[rel] = json.dumps(counts, sort_keys=True).encode()
        else:
            out[rel] = path.read_bytes()
    return out


def _shape(memory: Any, home: Path) -> Any:
    """What a memory is made of, with every path relative to its home."""
    if memory is None:
        return None

    def rel(path: Any) -> Optional[str]:
        return None if path is None else Path(path).relative_to(home).as_posix()

    cfg = memory.cfg
    index = memory.index
    return {
        "type": type(memory).__name__,
        "store": rel(cfg.store), "notes": rel(cfg.notes), "records": rel(cfg.records_dir),
        "transcripts_dir": rel(cfg.transcripts_dir), "mem0": rel(cfg.mem0_dir), "archive": rel(cfg.archive_dir),
        "transcripts": (type(memory.transcripts).__name__, rel(memory.transcripts.root),
                        rel(memory.transcripts.archive)),
        "index": None if index is None else (type(index).__name__, rel(index.home), index.cfg.embedder),
        "switches": (memory.hybrid, memory.timeargs, memory.retry, memory.ranked is None),
        "chat_log": memory.chat_log,
    }


def _daemon_build(cfg: recall.RecallConfig) -> Any:
    return Daemon._build_memory(types.SimpleNamespace(_recall_cfg=cfg))


def _build_on(home: Path, build: Callable[[recall.RecallConfig], Any], monkeypatch: pytest.MonkeyPatch,
              env: dict[str, str]) -> tuple[Any, dict[str, Optional[bytes]]]:
    _seed(home)
    monkeypatch.setenv("HOME", str(home))
    for name in MEMORY_SWITCHES:
        monkeypatch.delenv(name, raising=False)
    for name, value in env.items():
        monkeypatch.setenv(name, value)
    memory = build(recall.configured())
    return _shape(memory, home), _tree(home)


EDITIONS = {
    # eve lite's memory switches as profile.LITE_FORCED sets them
    "lite": {"EVE_PROFILE": "lite", "EVE_MEMORY": "1", "EVE_MEM0_EMBEDDER": "openrouter",
             "OPENROUTER_API_KEY": PLACEHOLDER_KEY},
    "full": {"EVE_MEMORY": "1", "OPENROUTER_API_KEY": PLACEHOLDER_KEY},
}


@pytest.mark.parametrize("edition", sorted(EDITIONS))
def test_both_build_the_same_memory_on_a_temporary_home(tmp_path: Path, monkeypatch: pytest.MonkeyPatch,
                                                        edition: str) -> None:
    env = EDITIONS[edition]
    daemon_shape, daemon_tree = _build_on(tmp_path / "daemon-home", _daemon_build, monkeypatch, env)
    lite_shape, lite_tree = _build_on(tmp_path / "lite-home", lite.build_memory, monkeypatch, env)
    assert daemon_shape is not None and daemon_shape["type"] == "Memory"
    assert lite_shape == daemon_shape
    assert lite_tree == daemon_tree
    # the move took the old store in, on both sides alike: its records, its sessions to the archive, the old index
    assert ".config/eve-bridge/memory/records/people.md" in lite_tree
    assert ".config/eve-bridge/memory/archive/sessions/2026-09-01.md" in lite_tree
    assert ".config/eve-bridge/memory/mem0/placeholder.txt" in lite_tree
    assert ".config/eve-bridge/debrief/records/people.md" not in lite_tree
    git = ".config/eve-bridge/memory/records/.git"
    if edition == "lite":
        assert git not in lite_tree                    # eve lite runs no git: the records keep no history
    elif toolchain.git_usable():
        assert git in lite_tree                        # control: full eve's records are a repository of their own


def test_memory_off_builds_nothing_on_either_side(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    for side, build in (("daemon", _daemon_build), ("lite", lite.build_memory)):
        home = tmp_path / side
        shape, tree = _build_on(home, build, monkeypatch, {"EVE_MEMORY": "0"})
        assert shape is None
        assert ".config/eve-bridge/memory" not in tree, side            # with memory off nothing is touched
        assert ".config/eve-bridge/debrief/records/people.md" in tree, side


# ---- the dream ------------------------------------------------------------------------------------------------------

class _Dreamer:
    """dream.Dreamer as the daemon starts it: what it was given, and a loop that ends with the shutdown event."""

    made: list[tuple[Any, ...]] = []

    def __init__(self, cfg: Any, transcripts: Any, client: Any, index: Any) -> None:
        _Dreamer.made.append((cfg, transcripts, client, index))

    async def loop(self, shutdown: asyncio.Event) -> None:
        await shutdown.wait()


def test_both_start_the_same_dream(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(dream, "Dreamer", _Dreamer)
    client = object()
    monkeypatch.setattr(records, "make_client", lambda *a, **k: client)
    _Dreamer.made = []
    memory = types.SimpleNamespace(transcripts=object(), index=object())
    cfg = recall.RecallConfig(store=tmp_path / "memory", notes=tmp_path / "notes")

    async def go() -> list[Any]:
        seen = []
        for start in ("daemon", "lite"):
            shutdown, background = asyncio.Event(), set()
            if start == "daemon":
                me = types.SimpleNamespace(_memory=memory, _recall_cfg=cfg, _shutdown=shutdown, _background=background)
                task = Daemon._start_dream(me)
            else:
                task = lite.start_dream(memory, cfg, shutdown, background)
            assert task is not None and task.get_name() == "memory-dream"
            assert background == {task}                               # held, so shutdown drains it
            shutdown.set()
            await asyncio.wait_for(task, 5)
            await asyncio.sleep(0)
            assert background == set()                                # and let go once it ended
            seen.append(task.get_name())
        return seen

    assert asyncio.run(go()) == ["memory-dream", "memory-dream"]
    assert _Dreamer.made == [(cfg, memory.transcripts, client, memory.index)] * 2


def test_no_memory_or_no_client_is_no_dream_on_either_side(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    cfg = recall.RecallConfig(store=tmp_path / "memory", notes=tmp_path / "notes")
    memory = types.SimpleNamespace(transcripts=object(), index=None)

    async def go() -> list[Any]:
        out = []
        for given, client in ((None, object()), (memory, None)):
            monkeypatch.setattr(records, "make_client", lambda *a, client=client, **k: client)
            me = types.SimpleNamespace(_memory=given, _recall_cfg=cfg, _shutdown=asyncio.Event(), _background=set())
            out.append((Daemon._start_dream(me), lite.start_dream(given, cfg, asyncio.Event(), set())))
        return out

    assert asyncio.run(go()) == [(None, None), (None, None)]
