"""Privacy guards for the whole memory, end to end: a word said to eve stays on this Mac, and forget reaches it.

The owner's rule (owner, 2026-09-23: "nothing should leak out ever") is checked here across every module at
once, not one at a time. A HOME in tmp_path, every store variable pointed inside it, and one random sentinel
word said on both channels. The real Transcripts, records, Memory, Dreamer and migration run; only the model
clients and mem0's own Memory are fakes, each of which records what it was asked to send.

What must hold:

- the sentinel is on disk only under the tmp HOME (and, after forget, nowhere in eve's stores at all,
  git history included);
- no log record carries it;
- the claude-mem sink never receives it;
- every request body eve builds for OpenAI asks it not to store the text (``store=False``) where the API
  has the flag: the Telegram brain, the slow brain (think.py), the dream's client, mem0's extraction call;
- the default memory folder is outside the eve repository, and no transcript file is tracked by git.

Every leak check has a positive control that plants a leak and proves the check sees it. The Live voice
connection has no store flag in its session payload; the test pins that it never asks for storage.
"""

from __future__ import annotations

import asyncio
import json
import logging
import os
import pwd
import secrets
import subprocess
import sys
import tempfile
import types
from pathlib import Path
from types import SimpleNamespace
from typing import Any, Iterable, Optional

import pytest
from test_telegram import CFG as TG_CFG
from test_telegram import FakeApi, FakeCreate, Rig, call, say, tap_update
from test_voice_agent import FakeAgent, FakeConnection, _eventually, _session, _tool_call

from eve_bridge import claude_mem, dream, mem0_memory, recall, records, telegram, think, transcripts
from eve_bridge.daemon import Daemon
from eve_bridge.memory_bus import MemoryBus

REPO = Path(__file__).resolve().parents[2]


# ---- the leak detectors (each has a positive control below) -------------------------------------------

def files_holding(root: Path, word: str) -> list[Path]:
    """Every regular file under `root` whose bytes contain `word`. Symlinks are not followed."""
    needle = word.encode("utf-8")
    out: list[Path] = []
    for base, _dirs, names in os.walk(root, followlinks=False):
        for name in names:
            p = Path(base) / name
            if p.is_symlink() or not p.is_file():
                continue
            try:
                if needle in p.read_bytes():
                    out.append(p)
            except OSError:
                continue
    return out


def stray_copies(scan_root: Path, home: Path, word: str) -> list[Path]:
    """Files under `scan_root` holding `word` that are NOT under `home`."""
    return [p for p in files_holding(scan_root, word) if home not in p.parents]


def repo_files_holding(word: str) -> list[str]:
    """Tracked and untracked (not ignored) files of the eve repository that contain `word`."""
    listed = subprocess.run(["git", "-C", str(REPO), "ls-files", "-co", "--exclude-standard", "-z"],
                            capture_output=True, check=True).stdout.decode().split("\0")
    needle = word.encode("utf-8")
    hits = []
    for rel in filter(None, listed):
        p = REPO / rel
        try:
            if p.is_file() and not p.is_symlink() and needle in p.read_bytes():
                hits.append(rel)
        except OSError:
            continue
    return hits


def logged(records_: Iterable[logging.LogRecord], word: str) -> list[str]:
    """Log records whose rendered message, arguments or traceback carry `word`."""
    out = []
    for r in records_:
        text = " ".join([r.getMessage(), repr(r.args), r.exc_text or "", str(r.exc_info or "")])
        if word in text:
            out.append(r.getMessage())
    return out


def asks_to_store(bodies: Iterable[dict[str, Any]]) -> list[dict[str, Any]]:
    """Request bodies that do not say ``store: False``."""
    return [b for b in bodies if not (isinstance(b, dict) and b.get("store") is False)]


def git_objects_holding(repo: Path, word: str) -> bool:
    """Whether any object in the repository's database, reachable or not, holds `word`."""
    dump = subprocess.run(["git", "-C", str(repo), "cat-file", "--batch-all-objects", "--batch"],
                          capture_output=True, check=True).stdout
    return word.encode("utf-8") in dump


SYNTHETIC_TRANSCRIPTS = "bridge/tests/fixtures/memory_eval/transcripts/"   # the memory eval's invented persona


def synthetic(repo: Path) -> bool:
    """The memory eval's transcripts are an invented person's 60 days: its persona.json says so."""
    persona = repo / SYNTHETIC_TRANSCRIPTS / ".." / "persona.json"
    try:
        return str(json.loads(persona.read_text(encoding="utf-8")).get("note", "")).startswith("Synthetic persona")
    except (OSError, ValueError):
        return False


def tracked_transcripts(paths: Iterable[str], repo: Optional[Path] = None) -> list[str]:
    """Repository paths that would be a transcript file: a .jsonl under a folder named transcripts. The memory
    eval's synthetic transcripts are left out while their persona.json says they are invented."""
    skip = SYNTHETIC_TRANSCRIPTS if repo is not None and synthetic(repo) else None
    return [p for p in paths if p.endswith(".jsonl") and "transcripts" in Path(p).parts
            and not (skip and p.startswith(skip))]


def deep_store_flags(value: Any) -> list[Any]:
    """Every value of a "store" key anywhere in a nested payload."""
    found: list[Any] = []
    if isinstance(value, dict):
        for k, v in value.items():
            if k == "store":
                found.append(v)
            found.extend(deep_store_flags(v))
    elif isinstance(value, (list, tuple)):
        for v in value:
            found.extend(deep_store_flags(v))
    return found


# ---- fakes ------------------------------------------------------------------------------------------

class FakeMem0:
    """mem0's Memory in RAM: each thing the owner said becomes one memory, verbatim (the worst case)."""

    def __init__(self) -> None:
        self.rows: list[dict[str, Any]] = []
        self._n = 0

    def add(self, messages: list[dict[str, str]], **kw: Any) -> dict[str, Any]:
        for m in messages:
            if m.get("role") == "user":
                self._n += 1
                self.rows.append({"id": f"m{self._n}", "memory": m["content"], "metadata": dict(kw["metadata"]),
                                  "created_at": f"2026-01-01T00:00:{self._n:02d}"})
        return {"results": []}

    def search(self, query: str, **kw: Any) -> dict[str, Any]:
        words = query.lower().split()
        hits = [dict(r, score=0.9) for r in self.rows if all(w in r["memory"].lower() for w in words)]
        return {"results": hits[: kw.get("top_k", 5)]}

    def get_all(self, **kw: Any) -> dict[str, Any]:
        return {"results": [dict(r) for r in self.rows]}

    def delete(self, memory_id: str) -> dict[str, Any]:
        self.rows = [r for r in self.rows if r["id"] != memory_id]
        return {"message": "ok"}


def fake_openai(sent: list[dict[str, Any]], replies: dict[str, str]) -> types.ModuleType:
    """An ``openai`` module whose OpenAI().responses.create records its kwargs and answers by schema name."""

    class Responses:
        def create(self, **kw: Any) -> Any:
            sent.append(kw)
            return SimpleNamespace(output_text=replies[kw["text"]["format"]["name"]])

    mod = types.ModuleType("openai")
    mod.OpenAI = lambda **kw: SimpleNamespace(responses=Responses())  # type: ignore[attr-defined]
    return mod


# ---- the world: a private HOME, every store inside it -------------------------------------------------

@pytest.fixture
def world(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> SimpleNamespace:
    home = tmp_path / "home"
    home.mkdir()
    cfg_dir = home / ".config" / "eve-bridge"
    monkeypatch.setenv("HOME", str(home))
    monkeypatch.setenv("EVE_MEMORY", "1")
    monkeypatch.setenv("EVE_MEMORY_DIR", str(cfg_dir / "memory"))
    monkeypatch.setenv("EVE_DEBRIEF_DIR", str(cfg_dir / "debrief"))
    monkeypatch.setenv("EVE_NOTES_DIR", str(cfg_dir / "notes"))
    monkeypatch.setenv("MEM0_DIR", str(cfg_dir / "memory" / "mem0" / "home"))
    for name in ("EVE_RECORDS", "EVE_MEM0", "EVE_CLAUDE_MEM", "EVE_TELEGRAM"):
        monkeypatch.delenv(name, raising=False)
    tmp = home / "tmp"
    tmp.mkdir()
    monkeypatch.setattr(tempfile, "tempdir", str(tmp))          # any temp file lands inside HOME too
    monkeypatch.setattr(mem0_memory, "_shared", {})
    return SimpleNamespace(root=tmp_path, home=home, cfg_dir=cfg_dir)


def _git(repo: Path, *args: str) -> None:
    subprocess.run(["git", "-C", str(repo), "-c", "user.name=test", "-c", "user.email=test@localhost",
                    "-c", "commit.gpgsign=false", *args], check=True, capture_output=True)


def _legacy_store(path: Path, word: str) -> None:
    """The retired debrief store the migration moves: a session note and a star that carry the word, in git."""
    (path / "sessions" / "2026-09-01").mkdir(parents=True)
    (path / "sessions" / "2026-09-01" / "0900-abcd.md").write_text(
        f"# A session\n\n- the owner mentioned {word} at the gym\n- nothing else\n", encoding="utf-8")
    (path / "HIGHLIGHTS.md").write_text(
        f"# Highlights\n\n## From talking\n\n- ★ keep {word} in mind (2026-09-01)\n", encoding="utf-8")
    subprocess.run(["git", "init", "-q", str(path)], check=True, capture_output=True)
    _git(path, "add", "-A")
    _git(path, "commit", "-q", "-m", "old notes")


# ---- the positive controls: every detector sees a planted leak ----------------------------------------

def test_every_leak_detector_catches_a_planted_leak(tmp_path: Path, caplog: pytest.LogCaptureFixture) -> None:
    word = "plant" + secrets.token_hex(6)
    home = tmp_path / "home"
    (home / "inside").mkdir(parents=True)
    (home / "inside" / "ok.txt").write_text(word, encoding="utf-8")
    assert stray_copies(tmp_path, home, word) == []                       # inside HOME is allowed
    (tmp_path / "elsewhere").mkdir()
    planted = tmp_path / "elsewhere" / "leak.txt"
    planted.write_text(f"x {word} y", encoding="utf-8")
    assert stray_copies(tmp_path, home, word) == [planted]

    caplog.set_level(logging.DEBUG)
    logging.getLogger("eve_bridge.telegram").info("clean line")
    assert logged(caplog.records, word) == []
    logging.getLogger("eve_bridge.telegram").info("said: %s", word)
    assert logged(caplog.records, word) == [f"said: {word}"]

    assert asks_to_store([{"store": False}]) == []
    assert asks_to_store([{"store": True}, {"model": "m"}]) == [{"store": True}, {"model": "m"}]

    assert tracked_transcripts(["bridge/src/x.py", "docs/transcripts.md"]) == []
    assert tracked_transcripts(["memory/transcripts/2026-09-23.jsonl"]) == ["memory/transcripts/2026-09-23.jsonl"]
    assert deep_store_flags({"a": [{"store": True}]}) == [True]

    repo = tmp_path / "repo"
    repo.mkdir()
    subprocess.run(["git", "init", "-q", str(repo)], check=True, capture_output=True)
    (repo / "f.md").write_text(word, encoding="utf-8")
    _git(repo, "add", "-A")
    _git(repo, "commit", "-q", "-m", "c")
    (repo / "f.md").write_text("gone", encoding="utf-8")
    _git(repo, "commit", "-qam", "d")
    assert git_objects_holding(repo, word)                                # history still has it

    sent: list[dict[str, Any]] = []
    bus = MemoryBus()
    sink = claude_mem.ClaudeMemSink(bus, url="http://127.0.0.1:9")
    orig = claude_mem._post
    claude_mem._post = lambda url, body, timeout: sent.append(body) or {}  # type: ignore[assignment]
    try:
        sink.start()
        bus.publish("/eve/memory/remember", {"text": f"a line with {word}", "time": 1.0})
        sink.stop(wait=5.0)
    finally:
        claude_mem._post = orig  # type: ignore[assignment]
    assert any(word in json.dumps(b) for b in sent)                       # the sink check sees a leak


def test_the_default_memory_folder_is_outside_the_repo_and_no_transcript_is_tracked() -> None:
    real_home = Path(pwd.getpwuid(os.getuid()).pw_dir)
    default = Path(recall.DEFAULT_STORE.replace("~", str(real_home), 1)).resolve()
    assert recall.DEFAULT_STORE == transcripts.DEFAULT_MEMORY_DIR
    assert default != REPO and REPO not in default.parents
    # the control: the transcript guard refuses a folder inside this repository (for the git rule, or first
    # for a synced parent such as ~/Documents, depending on where the checkout lives)
    assert transcripts.refusal(REPO / "bridge" / "memory-probe") != ""
    tracked = subprocess.run(["git", "-C", str(REPO), "ls-files"], capture_output=True, check=True,
                             text=True).stdout.splitlines()
    assert tracked and tracked_transcripts(tracked, REPO) == []
    # the controls: a transcript anywhere else is still caught, and the eval's are caught without the persona note
    assert tracked_transcripts(["bridge/memory/transcripts/2026-01-01.jsonl"], REPO) != []
    assert tracked_transcripts([SYNTHETIC_TRANSCRIPTS + "2026-08-02.jsonl"]) != []


# ---- the whole path -----------------------------------------------------------------------------------

def _tool_output(request: dict[str, Any], call_id: str) -> dict[str, Any]:
    for item in request["input"]:
        if isinstance(item, dict) and item.get("type") == "function_call_output" and item.get("call_id") == call_id:
            return json.loads(item["output"])
    raise AssertionError(f"no output for {call_id}")


def test_a_word_said_to_eve_stays_on_this_mac_and_forget_reaches_every_store(
        world: SimpleNamespace, monkeypatch: pytest.MonkeyPatch, caplog: pytest.LogCaptureFixture) -> None:
    caplog.set_level(logging.DEBUG)
    word = "quokka" + secrets.token_hex(6)             # random per run: no file anywhere can hold it by chance
    _legacy_store(world.cfg_dir / "debrief", word)

    # mem0: the real OwnerMemory, its Memory faked; the config it is built from is kept for the store check
    mem0s: list[FakeMem0] = []
    configs: list[dict[str, Any]] = []

    def open_fake(cfg: mem0_memory.Mem0Config) -> FakeMem0:
        configs.append(mem0_memory.mem0_config(cfg))
        mem0s.append(FakeMem0())
        return mem0s[-1]

    monkeypatch.setattr(mem0_memory, "open_mem0", open_fake)
    monkeypatch.setattr(mem0_memory, "available", lambda: True)

    # claude-mem: a live sink on a bus, its HTTP post captured
    posted: list[dict[str, Any]] = []
    monkeypatch.setattr(claude_mem, "_post", lambda url, body, timeout: posted.append(body) or {})
    bus = MemoryBus()
    sink = claude_mem.ClaudeMemSink(bus, url="http://127.0.0.1:9")
    sink.start()

    # the daemon's own build: migration, records repo, transcripts, index, facade
    memory = Daemon._build_memory(SimpleNamespace(_recall_cfg=recall.configured()))
    assert memory is not None and memory.index is not None and memory.transcripts.enabled
    cfg = memory.cfg
    assert world.home in cfg.store.parents
    assert (cfg.archive_dir / ".git").is_dir() and (cfg.records_dir / "starred.md").is_file()

    # the slow brain, its requests captured
    thought: list[dict[str, Any]] = []

    async def think_create(req: dict[str, Any]) -> dict[str, Any]:
        thought.append(req)
        return {"output": [{"type": "message", "content": [{"type": "output_text", "text": "Forty-two."}]}]}

    thinker = think.OpenAIThinker(think.ThinkConfig(model="test-model"), create=think_create)

    # 1. Telegram: the owner says it, eve repeats it, the owner stars it, the slow brain is asked about it
    tg = Rig(FakeApi(), FakeCreate(
        say(f"Got it, {word}."),
        call("remember", {"claim": f"the gym locker is {word}"}, "r1"),     # a star ends the turn: a reaction
        call("think_hard", {"question": f"is {word} a good locker code?"}, "t1"), say("It is fine."),
        call("memory_search", {"query": word, "days_back": None}, "s1"), say("Found it.")),
        config=TG_CFG, memory=memory, thinker=thinker)

    async def texting() -> None:
        await tg.inlet._turn(telegram.Inbound(TG_CFG_OWNER, TG_CFG_OWNER, f"my gym locker is {word}", message_id=1))
        await tg.inlet._turn(telegram.Inbound(TG_CFG_OWNER, TG_CFG_OWNER, "keep that one for good, please", message_id=2))
        await tg.inlet._turn(telegram.Inbound(TG_CFG_OWNER, TG_CFG_OWNER, "think about it", message_id=3))
        await tg.inlet._turn(telegram.Inbound(TG_CFG_OWNER, TG_CFG_OWNER, "what is my locker?", message_id=4))

    asyncio.run(texting())
    assert word in json.dumps(_tool_output(tg.create.requests[-1], "s1"))      # memory_search found it

    # 2. Voice: the owner says it out loud, asks the slow brain and memory about it
    conn = FakeConnection([])
    session, _, _ = _session(conn, [FakeAgent(None, None)], memory=memory, thinker=thinker)

    async def talking() -> None:
        task = asyncio.create_task(session.run())
        await _eventually(lambda: session._started.is_set())
        session._close_turn("user", f"the bike lock is {word} too")
        session._close_turn("assistant", "Noted.")
        conn.feed(_tool_call("think_hard", "v1", question=f"should the bike lock be {word}?"))
        await _eventually(lambda: len(conn.tool_outputs()) >= 1, timeout=5.0)
        conn.feed(_tool_call("memory_search", "v2", query=word, days_back=None))
        await _eventually(lambda: len(conn.tool_outputs()) >= 2, timeout=5.0)
        conn.feed(_tool_call("end_conversation", "v9"), None)
        await task

    asyncio.run(talking())
    assert word in json.dumps(conn.tool_outputs()[1])
    started = [kw["session"] for name, kw in conn.sent if name == "session.start"]
    assert started and all(flag is False for flag in deep_store_flags(started))   # Live: never asks to store

    # 3. The dream: the real OpenAIReconcileClient over a fake openai module
    day = memory.transcripts.day_of(memory.transcripts.now())
    reconcile_sent: list[dict[str, Any]] = []
    monkeypatch.setitem(sys.modules, "openai", fake_openai(reconcile_sent, {
        "dream_day": json.dumps({
            "records": [{"id": "gym", "type": "place", "aliases": [f"{word} locker"],
                         "facts": [f"The gym locker code is {word}"]},
                        {"id": "tea", "type": "preference", "aliases": [], "facts": ["Likes green tea"]}],
            "profile": {"life_context": [f"Uses locker {word} at the gym"], "acting": [], "talking": []},
            "forget": [],
            "journal": {"title": f"The {word} locker", "happened": [f"The owner shared {word}"], "learned": [],
                        "open": [], "corrections": []}}),
        "dream_consolidate": json.dumps({"records": [], "merged": []}),
    }))
    client = records.OpenAIReconcileClient("test-model", api_key="sk-test")
    report = asyncio.run(dream.Dreamer(cfg, memory.transcripts, client, memory.index).dream(day))
    assert report.dreamt and "reconcile" not in report.failed and report.ingested >= 2

    # before forget: the word is in every layer (the control for the absence checks after it)
    layers = {
        "transcripts": cfg.transcripts_dir,
        "record": cfg.records_dir / "gym.md",
        "profile": cfg.records_dir / "profile.md",
        "starred": cfg.records_dir / "starred.md",
        "journal": cfg.records_dir / "days" / f"{day}.md",
        "archive": cfg.archive_dir,
    }
    for name, where in layers.items():
        assert files_holding(where, word) if where.is_dir() else word in where.read_text(encoding="utf-8"), name
    assert any(word in r["memory"] for r in mem0s[0].rows)
    assert git_objects_holding(cfg.records_dir, word) and git_objects_holding(cfg.archive_dir, word)

    # 4. Forget, through the Telegram brain: preview, then apply in a new message
    tg.create.responses += [call("forget_preview", {"query": word}, "f1"), say("I found it. Forget it all?")]
    asyncio.run(tg.inlet._turn(telegram.Inbound(TG_CFG_OWNER, TG_CFG_OWNER, f"forget {word}", message_id=5)))
    preview = _tool_output(tg.create.requests[-1], "f1")
    assert preview["ok"] and preview["total"] > 0 and word not in json.dumps(preview)   # counts, never words
    tg.create.responses += [call("forget_apply", {"token": preview["token"]}, "f2"), say("Forgotten.")]

    async def approved() -> None:
        """Today's block still carries the slow brain's line from step 1, so the forget is the owner's yes first
        (tool_gate.carried): the gate's Yes is tapped."""
        asked = len(tg.api.keyboards)
        turn = asyncio.ensure_future(tg.inlet._turn(telegram.Inbound(TG_CFG_OWNER, TG_CFG_OWNER, "yes, do it",
                                                                     message_id=6)))
        for _ in range(1000):
            if len(tg.api.keyboards) > asked or turn.done():
                break
            await asyncio.sleep(0.005)
        assert len(tg.api.keyboards) > asked and tg.api.titled[-1][0] == telegram.GATE_TITLE
        message_id, key = tg.api.key("Yes")
        tg.inlet._dispatch(tap_update(key, message_id=message_id, uid=TG_CFG_OWNER, update_id=95))
        await turn

    asyncio.run(approved())
    applied = _tool_output(tg.create.requests[-1], "f2")
    assert applied["ok"] and set(applied["history_squashed"]) == {"records", "archive"}
    sink.stop(wait=5.0)

    # after forget: gone from every store, from git history, from the index
    for name, where in layers.items():
        if where.is_dir():
            assert files_holding(where, word) == [], name
        elif where.exists():
            assert word not in where.read_text(encoding="utf-8"), name
    assert files_holding(cfg.store, word) == []
    assert not git_objects_holding(cfg.records_dir, word) and not git_objects_holding(cfg.archive_dir, word)
    assert not any(word in r["memory"] for r in mem0s[0].rows)

    # the word never left HOME on disk, never reached a log, never reached claude-mem
    assert stray_copies(world.root, world.home, word) == []
    assert repo_files_holding(word) == []
    assert logged(caplog.records, word) == []
    assert posted == [] or not any(word in json.dumps(b) for b in posted)

    # every OpenAI body eve built says store=False
    assert len(tg.create.requests) >= 10 and asks_to_store(tg.create.requests) == []
    assert len(thought) == 2 and asks_to_store(thought) == []
    assert any(word in b["instructions"] for b in thought)       # the context did carry it: the check is live
    assert len(reconcile_sent) == 2 and asks_to_store(reconcile_sent) == []
    # mem0's two clients are replaced with OpenRouter's (2026-10-08); its config holds no key, only a placeholder,
    # and mem0 sends no `store` field on its OpenRouter path (the extraction asks nothing to be kept)
    assert configs and all(c["llm"]["config"].get("api_key") == mem0_memory.PLACEHOLDER_KEY
                           and "store" not in c["llm"]["config"] for c in configs)


TG_CFG_OWNER = next(iter(TG_CFG.owner_ids))
