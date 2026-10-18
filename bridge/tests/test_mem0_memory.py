"""mem0_memory.py: a local mem0 index fed each day's conversations natively, searched dated, forgettable."""

from __future__ import annotations

import inspect
import os
import sqlite3
import stat
import time
from datetime import datetime, timedelta
from pathlib import Path
from typing import Any

import pytest

from eve_bridge import mem0_memory
from eve_bridge.mem0_memory import Mem0Config, OwnerMemory, configured, mem0_config
from eve_bridge.recall import RecallConfig
from eve_bridge.transcripts import TranscriptConfig, Transcripts

DAY = "2026-09-20"


class FakeMem0:
    """mem0's Memory, in RAM: add/search/get_all/delete, recording what it was given."""

    def __init__(self, fail_adds: int = 0, hits: Any = None) -> None:
        self.added: list[tuple[list[dict[str, str]], dict[str, Any]]] = []
        self.fail_adds = fail_adds
        self.hits = hits
        self.rows: list[dict[str, Any]] = []
        self.deleted: list[str] = []

    def add(self, messages: list[dict[str, str]], **kw: Any) -> dict[str, Any]:
        if self.fail_adds:
            self.fail_adds -= 1
            raise RuntimeError("network")
        self.added.append((messages, kw))
        return {"results": []}

    def search(self, query: str, **kw: Any) -> dict[str, Any]:
        assert kw["filters"] == {"user_id": "owner"}
        return {"results": self.hits if self.hits is not None else []}

    def get_all(self, **kw: Any) -> dict[str, Any]:
        assert kw["filters"] == {"user_id": "owner"}
        return {"results": list(self.rows)}

    def delete(self, memory_id: str) -> dict[str, Any]:
        if memory_id not in {r["id"] for r in self.rows}:
            raise ValueError("not found")
        self.rows = [r for r in self.rows if r["id"] != memory_id]
        self.deleted.append(memory_id)
        return {"message": "ok"}


def rc_at(tmp_path: Path) -> RecallConfig:
    store = tmp_path / "memory"
    store.mkdir(parents=True, exist_ok=True)
    return RecallConfig(store=store, notes=tmp_path / "notes")


def index(tmp_path: Path, fake: Any = None, clock: Any = None) -> tuple[OwnerMemory, Any]:
    fake = fake if fake is not None else FakeMem0()
    kw = {"clock": clock} if clock is not None else {}
    mem = OwnerMemory(Mem0Config(enabled=True, home=rc_at(tmp_path).mem0_dir), factory=lambda _c: fake, **kw)
    return mem, fake


# ---- switches and config ------------------------------------------------------------------------------

def test_it_is_on_with_memory_and_off_with_its_own_switch(tmp_path: Path) -> None:
    rc = rc_at(tmp_path)
    assert configured(rc, {}).enabled is False
    assert configured(rc, {"EVE_MEMORY": "1"}).enabled is True
    assert configured(rc, {"EVE_RECORDS": "1"}).enabled is True                    # the older switch
    assert configured(rc, {"EVE_MEMORY": "1", "EVE_MEM0": "0"}).enabled is False
    assert configured(rc, {"EVE_MEM0": "1"}).enabled is False                      # never without memory
    assert configured(rc, {"EVE_MEMORY": "1"}).home == rc.mem0_dir == rc.store / "mem0"


def test_everything_is_stored_under_its_home_and_extraction_is_owner_facts_only(tmp_path: Path) -> None:
    home = tmp_path / "m"
    conf = mem0_config(Mem0Config(enabled=True, home=home))
    assert conf["vector_store"]["provider"] == "qdrant" and conf["vector_store"]["config"]["path"] == str(home / "qdrant")
    assert conf["history_db_path"] == str(home / "history.db")
    assert "host" not in conf["vector_store"]["config"] and "url" not in conf["vector_store"]["config"]
    instructions = conf["custom_instructions"]
    assert "owner only" in instructions and "is not a fact about the owner" in instructions


def test_shared_is_none_when_off_or_not_installed_and_one_instance_otherwise(tmp_path: Path,
                                                                            monkeypatch: pytest.MonkeyPatch) -> None:
    rc = rc_at(tmp_path)
    on = {"EVE_MEMORY": "1"}
    assert mem0_memory.shared(rc, {}) is None
    monkeypatch.setattr(mem0_memory, "available", lambda: False)
    assert mem0_memory.shared(rc, on) is None
    monkeypatch.setattr(mem0_memory, "available", lambda: True)
    first = mem0_memory.shared(rc, on)
    assert first is not None and first is mem0_memory.shared(rc, on) and first.home == rc.mem0_dir


# ---- search: dated, bounded, never slow -----------------------------------------------------------------

def test_search_is_dated_with_its_channel_and_drops_guesses_stars_and_repeats(tmp_path: Path) -> None:
    hits = [
        {"id": "1", "memory": "Sister lives by the sea", "score": 0.6,
         "metadata": {"day": "2026-09-20", "channel": "voice", "conv": "v-1"}},
        {"id": "2", "memory": "Likes jazz", "score": 0.5, "metadata": {"note": "sessions/2026-09-12/1015-x.md"}},
        {"id": "3", "memory": "Takes the train", "score": 0.4, "created_at": "2026-09-14T08:00:00+00:00"},
        {"id": "4", "memory": "A guess", "score": 0.1},
        {"id": "5", "memory": "★ (candidate) eve thinks they like opera", "score": 0.9},
        {"id": "6", "memory": "likes jazz.", "score": 0.45},
    ]
    mem, _ = index(tmp_path, FakeMem0(hits=hits))
    assert mem.search("where does my sister live") == [
        {"text": "Sister lives by the sea", "day": "2026-09-20", "channel": "voice"},
        {"text": "Likes jazz", "day": "2026-09-12", "channel": ""},
        {"text": "Takes the train", "day": "2026-09-14", "channel": ""},
    ]
    assert mem.search("   ") == []
    many = [{"id": str(i), "memory": f"fact {i}", "score": 0.9} for i in range(9)]
    assert len(index(tmp_path / "b", FakeMem0(hits=many))[0].search("x")) == mem0_memory.MAX_HITS == 5


def test_a_slow_search_is_abandoned_after_the_timeout(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    assert mem0_memory.SEARCH_TIMEOUT_SECS == 3.0

    class Slow(FakeMem0):
        def search(self, query: str, **kw: Any) -> dict[str, Any]:
            time.sleep(5)
            return {"results": [{"memory": "late", "score": 0.9}]}

    monkeypatch.setattr(mem0_memory, "SEARCH_TIMEOUT_SECS", 0.3)
    mem, _ = index(tmp_path, Slow())
    start = time.monotonic()
    assert mem.search("anything") == []
    assert time.monotonic() - start < 1.5
    assert mem.search("again") == []                                    # a second one waits its turn, then gives up
    start = time.monotonic()
    assert mem.search("third") == [] and time.monotonic() - start < 0.1   # two still stuck: no third thread


def test_a_failed_open_is_retried_after_ten_minutes(tmp_path: Path) -> None:
    now = [1000.0]
    opens: list[int] = []
    fake = FakeMem0(hits=[{"memory": "Likes jazz", "score": 0.9, "metadata": {"day": DAY}}])

    def factory(_c: Mem0Config) -> Any:
        opens.append(1)
        if len(opens) == 1:
            raise RuntimeError("no key yet")
        return fake

    mem = OwnerMemory(Mem0Config(enabled=True, home=tmp_path / "m"), factory=factory, clock=lambda: now[0])
    assert mem.search("jazz") == [] and len(opens) == 1
    now[0] += 599
    assert mem.search("jazz") == [] and len(opens) == 1                  # still inside the ten minutes
    now[0] += 2
    assert mem.search("jazz")[0]["text"] == "Likes jazz" and len(opens) == 2


def test_the_home_is_private_every_time_it_opens(tmp_path: Path) -> None:
    home = tmp_path / "m"

    def factory(cfg: Mem0Config) -> Any:
        (cfg.home / "qdrant" / "collection").mkdir(parents=True, exist_ok=True)
        os.chmod(cfg.home / "qdrant", 0o755)
        (cfg.home / "history.db").write_text("")
        os.chmod(cfg.home / "history.db", 0o644)
        return FakeMem0()

    os_umask = os.umask(0o022)
    try:
        mem = OwnerMemory(Mem0Config(enabled=True, home=home), factory=factory)
        mem.find(lambda _t: True)
    finally:
        os.umask(os_umask)
    mode = lambda p: stat.S_IMODE(os.stat(p).st_mode)      # noqa: E731
    assert mode(home) == 0o700 and mode(home / "qdrant") == 0o700 and mode(home / "qdrant" / "collection") == 0o700
    assert mode(home / "history.db") == 0o600


def test_mem0_unavailable_is_silence(tmp_path: Path) -> None:
    def broken(_c: Mem0Config) -> Any:
        raise ImportError("no mem0")

    mem = OwnerMemory(Mem0Config(enabled=True, home=tmp_path / "m"), factory=broken)
    t, _ = transcripts_with_a_day(tmp_path)
    assert mem.search("anything") == [] and mem.ingest_day(t, DAY) == 0
    assert mem.tidy() == 0 and mem.find(lambda _t: True) == []
    assert not (tmp_path / "m" / mem0_memory.INGESTED_CONVS).exists()      # nothing marked read


def test_a_forget_mem0_cannot_open_for_is_a_failure_never_a_done_zero(tmp_path: Path) -> None:
    """A forget that could not reach mem0 (the retry window, no key) returned 0, and forget_apply then
    reported the index as done while the memories stayed in Qdrant."""
    def broken(_c: Mem0Config) -> Any:
        raise ImportError("no mem0")

    mem = OwnerMemory(Mem0Config(enabled=True, home=tmp_path / "m"), factory=broken)
    with pytest.raises(mem0_memory.Mem0ForgetError):
        mem.forget(["x"])
    with pytest.raises(mem0_memory.Mem0ForgetError):
        mem.forget_matching(lambda _t: True)
    assert mem.forget([]) == 0                        # nothing asked: nothing to fail

    fake = FakeMem0()
    listing = OwnerMemory(Mem0Config(enabled=True, home=tmp_path / "l"), factory=lambda _c: fake)
    fake.get_all = lambda **_kw: (_ for _ in ()).throw(RuntimeError("qdrant"))    # type: ignore[method-assign]
    with pytest.raises(mem0_memory.Mem0ForgetError):
        listing.forget_matching(lambda _t: True)


# ---- ingest: each conversation, natively, once ------------------------------------------------------------

def at(hour: int, minute: int = 0, day: str = DAY) -> datetime:
    return datetime.fromisoformat(f"{day}T{hour:02d}:{minute:02d}:00").astimezone()


def conv_id(ch: str, when: datetime) -> str:
    return f"{ch[0]}-{int(when.timestamp() * 1000)}-abcd"


def transcripts_with_a_day(tmp_path: Path) -> tuple[Transcripts, dict[str, str]]:
    t = Transcripts(TranscriptConfig(enabled=True, root=tmp_path / "tx"), wall=lambda: at(23, day="2026-09-21"))
    assert t.enabled
    voice, text, silent = conv_id("voice", at(9)), conv_id("telegram", at(14)), conv_id("voice", at(18))
    t.append("voice", voice, "owner", "say", "my sister lives by the sea", now=at(9))
    t.append("voice", voice, "eve", "say", "Lovely, which town?", now=at(9, 1))
    t.append("voice", voice, "eve", "tool", "look left", tool="move_head", now=at(9, 1))
    t.append("voice", voice, "owner", "command", "wink", now=at(9, 2))
    t.append("voice", voice, "owner", "relay", "ask claude to run the tests", now=at(9, 2))
    t.append("voice", voice, "claude", "say", "tests pass", now=at(9, 3))
    t.close("voice", voice, now=at(9, 4))
    t.append("telegram", text, "owner", "say", "x" * 3000, now=at(14))
    t.append("telegram", text, "eve", "say", "noted", now=at(14, 1))
    t.append("voice", silent, "eve", "say", "hello?", now=at(18))
    return t, {"voice": voice, "telegram": text, "silent": silent}


def test_a_day_goes_in_as_user_and_assistant_messages_dated_with_channel_and_once(tmp_path: Path) -> None:
    mem, fake = index(tmp_path)
    t, convs = transcripts_with_a_day(tmp_path)
    assert mem.ingest_day(t, DAY) == 2                                  # the silent one is not sent
    (vmsgs, vkw), (tmsgs, tkw) = fake.added
    assert vmsgs == [{"role": "user", "content": "my sister lives by the sea"},
                     {"role": "assistant", "content": "Lovely, which town?"}]           # no tool/command/relay/claude
    assert vkw == {"user_id": "owner", "metadata": {"day": DAY, "channel": "voice", "conv": convs["voice"]}}
    assert tkw["metadata"]["channel"] == "telegram" and len(tmsgs[0]["content"]) == mem0_memory.MESSAGE_CHARS
    assert mem.ingest_day(t, DAY) == 0 and len(fake.added) == 2         # read once
    done = (rc_at(tmp_path).mem0_dir / mem0_memory.INGESTED_CONVS).read_text().split()
    assert sorted(done) == sorted(convs.values())
    assert stat.S_IMODE(os.stat(rc_at(tmp_path).mem0_dir / mem0_memory.INGESTED_CONVS).st_mode) == 0o600


def test_a_line_kept_as_outside_words_goes_in_as_a_placeholder(tmp_path: Path) -> None:
    """Round-4 review: mem0 ingested a reply written after a read (``out``) whole, so a page's words could become a
    memory of the owner. It keeps its place, with its words left out, as the dream's day_text does."""
    from eve_bridge import transcripts as transcripts_mod

    mem, fake = index(tmp_path)
    t = Transcripts(TranscriptConfig(enabled=True, root=tmp_path / "tx"), wall=lambda: at(23))
    conv = conv_id("telegram", at(10))
    t.append("telegram", conv, "owner", "say", "what does the page say?", now=at(10))
    t.append("telegram", conv, "eve", "say", "The page says the owner's bank is Example Bank, account 4410.",
             now=at(10) + timedelta(seconds=1), outside=True)
    assert mem.ingest_day(t, DAY) == 1
    sent = fake.added[0][0]
    assert sent == [{"role": "user", "content": "what does the page say?"},
                    {"role": "assistant", "content": transcripts_mod.OUTSIDE_LEFT_OUT}]
    units = OwnerMemory.line_units([{"who": "eve", "kind": "say", "text": "Example Bank", "out": True, "ts": ""},
                                    {"who": "owner", "kind": "say", "text": "ok", "ts": ""}])
    assert units[0][0][0]["content"] == transcripts_mod.OUTSIDE_LEFT_OUT


def test_a_long_conversation_goes_in_in_chunks_of_forty(tmp_path: Path) -> None:
    mem, fake = index(tmp_path)
    t = Transcripts(TranscriptConfig(enabled=True, root=tmp_path / "tx"), wall=lambda: at(23))
    conv = conv_id("telegram", at(10))
    for i in range(90):
        t.append("telegram", conv, "owner" if i % 2 == 0 else "eve", "say", f"line {i}", now=at(10) + timedelta(seconds=i))
    assert mem.ingest_day(t, DAY) == 1
    assert [len(m) for m, _ in fake.added] == [40, 40, 10]


def test_a_conversation_that_failed_is_tried_again(tmp_path: Path) -> None:
    mem, fake = index(tmp_path, FakeMem0(fail_adds=1))
    t, convs = transcripts_with_a_day(tmp_path)
    assert mem.ingest_day(t, DAY) == 1                                  # voice failed; telegram went in
    assert [kw["metadata"]["conv"] for _, kw in fake.added] == [convs["telegram"]]
    assert mem.ingest_day(t, DAY) == 1 and fake.added[-1][1]["metadata"]["conv"] == convs["voice"]


# ---- tidy, find, forget -------------------------------------------------------------------------------------

def rows() -> list[dict[str, Any]]:
    return [{"id": "a", "memory": "Likes jazz", "created_at": "2026-09-01"},
            {"id": "b", "memory": "likes  JAZZ.", "created_at": "2026-09-02"},
            {"id": "c", "memory": "★ (candidate) Likes opera", "created_at": "2026-09-03"},
            {"id": "d", "memory": "Sister lives by the sea", "created_at": "2026-09-04"}]


def test_tidy_drops_exact_duplicates_and_star_candidates_keeping_the_oldest(tmp_path: Path) -> None:
    mem, fake = index(tmp_path)
    fake.rows = rows()
    assert mem.tidy() == 2
    assert sorted(fake.deleted) == ["b", "c"] and [r["id"] for r in fake.rows] == ["a", "d"]
    assert mem.tidy() == 0


def history_db(path: Path) -> None:
    """The two tables mem0ai 2.2.0 keeps in history.db (mem0/memory/storage.py), with rows."""
    con = sqlite3.connect(str(path))
    con.execute("CREATE TABLE history (id TEXT PRIMARY KEY, memory_id TEXT, old_memory TEXT, new_memory TEXT, "
                "event TEXT, created_at DATETIME, updated_at DATETIME, is_deleted INTEGER, actor_id TEXT, role TEXT)")
    con.execute("CREATE TABLE messages (id TEXT PRIMARY KEY, session_scope TEXT, role TEXT, content TEXT, "
                "name TEXT, created_at DATETIME)")
    con.executemany("INSERT INTO history (id, memory_id, old_memory, new_memory, event) VALUES (?, ?, ?, ?, ?)",
                    [("h1", "d", None, "Sister lives by the sea", "ADD"),
                     ("h2", "d", "Sister lives by the sea", None, "DELETE"),
                     ("h3", "a", None, "Likes jazz", "ADD")])
    con.execute("INSERT INTO messages VALUES ('m1', 'user_id=owner', 'user', 'my sister lives by the sea', NULL, '')")
    con.commit()
    con.close()


def test_forget_deletes_the_memory_and_its_history_rows(tmp_path: Path) -> None:
    mem, fake = index(tmp_path)
    fake.rows = rows()
    home = rc_at(tmp_path).mem0_dir
    home.mkdir(parents=True)
    history_db(home / "history.db")
    assert b"sister lives by the sea" in (home / "history.db").read_bytes().lower()       # positive control
    found = mem.find(lambda text: "sister" in text.casefold())
    assert found == [{"id": "d", "text": "Sister lives by the sea"}]
    assert mem.forget([f["id"] for f in found] + ["missing", ""]) == 1
    assert fake.deleted == ["d"]
    con = sqlite3.connect(str(home / "history.db"))
    left = con.execute("SELECT memory_id FROM history").fetchall()
    msgs = con.execute("SELECT COUNT(*) FROM messages").fetchone()[0]
    con.close()
    assert left == [("a",)] and msgs == 0                               # other memories' rows stay
    assert b"sister lives by the sea" not in (home / "history.db").read_bytes().lower()
    assert mem.forget([]) == 0


def test_forget_without_a_history_database_still_deletes(tmp_path: Path) -> None:
    mem, fake = index(tmp_path)
    fake.rows = rows()
    assert mem.forget(["a"]) == 1 and fake.deleted == ["a"]


# ---- the real import, and what is gone ------------------------------------------------------------------

def test_telemetry_is_off_and_mem0_home_is_ours_before_the_real_import(tmp_path: Path,
                                                                       monkeypatch: pytest.MonkeyPatch) -> None:
    import importlib.util
    import sys

    if importlib.util.find_spec("mem0") is None:
        pytest.skip("mem0ai is not installed")
    for name in [m for m in sys.modules if m == "mem0" or m.startswith("mem0.")]:
        monkeypatch.delitem(sys.modules, name)                          # a fresh import reads the switch again
    monkeypatch.delenv("MEM0_TELEMETRY", raising=False)
    monkeypatch.delenv("MEM0_DIR", raising=False)
    monkeypatch.delenv("OPENAI_API_KEY", raising=False)                # only the OpenRouter key (2026-10-08)
    monkeypatch.setenv("OPENROUTER_API_KEY", "or-test-not-real")
    home = tmp_path / "mem0"
    real = mem0_memory.open_mem0(Mem0Config(enabled=True, home=home))
    from mem0.memory import telemetry

    assert telemetry.MEM0_TELEMETRY is False
    assert os.environ["MEM0_DIR"] == str(home / "home") and (home / "qdrant").is_dir()
    assert not (tmp_path / ".mem0").exists()
    assert real.custom_instructions == mem0_memory.CUSTOM_INSTRUCTIONS         # the key mem0 actually reads


def test_the_retired_ingest_is_gone() -> None:
    assert hasattr(OwnerMemory, "ingest_day")                                # positive control
    for name in ("loop", "ingest_pending", "pending_notes"):
        assert not hasattr(OwnerMemory, name), name
    for name in ("INGESTED_FILE", "NOTE_PREFIX", "note_body", "INGEST_INTERVAL_SECS", "DEFAULT_HOME"):
        assert not hasattr(mem0_memory, name), name
    assert "sessions_dir" not in inspect.getsource(mem0_memory)


def test_extraction_and_embeddings_go_to_openrouter_with_no_openai_key(tmp_path: Path,
                                                                      monkeypatch: pytest.MonkeyPatch) -> None:
    """Both of mem0's clients are OpenRouter's, on the owner's key (2026-10-08; until then they were pinned to
    OpenAI). A real mem0 is built with no OpenAI key at all and its own call paths are driven: the extraction goes
    out as openai/gpt-6-luna, the embeddings as openai/text-embedding-3-small, and mem0's config holds no key."""
    import importlib.util

    if importlib.util.find_spec("mem0") is None:
        pytest.skip("mem0ai is not installed")
    from openai.resources.chat.completions import Completions

    monkeypatch.delenv("OPENAI_API_KEY", raising=False)
    monkeypatch.setenv("OPENROUTER_API_KEY", "or-test-not-real")
    sent: list[dict[str, Any]] = []

    class Reply:
        choices = [type("C", (), {"message": type("M", (), {"content": "{}", "tool_calls": None})()})()]
        usage = None

    def fake_create(self: Any, *args: Any, **kwargs: Any) -> Any:
        sent.append({"base_url": str(self._client.base_url), **kwargs})
        return Reply()

    monkeypatch.setattr(Completions, "create", fake_create)
    import httpx2

    bodies: list[bytes] = []

    def embed_reply(request: Any) -> Any:
        bodies.append(request.content)
        return httpx2.Response(200, json={"object": "list", "model": "x", "usage": {"prompt_tokens": 1, "total_tokens": 1},
                                          "data": [{"object": "embedding", "index": 0, "embedding": [0.1] * 4}]})

    real = mem0_memory.open_mem0(Mem0Config(enabled=True, home=tmp_path / "b"))
    assert str(real.llm.client.base_url).startswith("https://openrouter.ai/api/v1")
    assert str(real.embedding_model.client.base_url).startswith("https://openrouter.ai/api/v1")
    assert mem0_config(Mem0Config(enabled=True, home=tmp_path / "b"))["llm"]["config"]["api_key"] == \
        mem0_memory.PLACEHOLDER_KEY                                       # never a real key in mem0's config
    real.llm.generate_response(messages=[{"role": "user", "content": "hi"}])
    assert sent and sent[-1]["base_url"].startswith("https://openrouter.ai/api/v1")
    assert sent[-1]["model"] == "gpt-6-luna"                            # the SDK's builder adds "openai/"
    assert sent[-1]["reasoning_effort"] == "low"
    assert sent[-1]["max_completion_tokens"] == mem0_memory.EXTRACT_MAX_OUTPUT_TOKENS
    import openai

    from eve_bridge import openrouter

    real.embedding_model.client = openrouter.client(http_client=openai.DefaultHttpxClient(
        transport=httpx2.MockTransport(embed_reply)))
    real.embedding_model.embed("hello")
    assert bodies and b'"model":"openai/text-embedding-3-small"' in bodies[-1]
