"""EVE_MEM0_EMBEDDER: openrouter (the default, unchanged) or gemma (EmbeddingGemma, local, its own store)."""

from __future__ import annotations

import logging
from pathlib import Path
from typing import Any

import pytest

from eve_bridge import mem0_memory
from eve_bridge.mem0_memory import Mem0Config, configured, mem0_config
from eve_bridge.recall import RecallConfig

ON = {"EVE_MEMORY": "1"}


def rc_at(tmp_path: Path) -> RecallConfig:
    store = tmp_path / "memory"
    store.mkdir(parents=True, exist_ok=True)
    return RecallConfig(store=store, notes=tmp_path / "notes")


def test_the_default_keeps_remote_embeddings_with_low_effort_extraction(tmp_path: Path) -> None:
    rc = rc_at(tmp_path)
    for env in (ON, {**ON, "EVE_MEM0_EMBEDDER": ""}, {**ON, "EVE_MEM0_EMBEDDER": "openrouter"},
                {**ON, "EVE_MEM0_EMBEDDER": "something-else"}):
        cfg = configured(rc, env)
        assert cfg == Mem0Config(enabled=True, home=rc.mem0_dir)
        assert cfg.embedder == "openrouter" and cfg.embed_model == "text-embedding-3-small"
    home = rc.mem0_dir
    assert mem0_config(configured(rc, ON)) == {
        "llm": {"provider": "openai", "config": {
            "model": "gpt-6-luna", "api_key": mem0_memory.PLACEHOLDER_KEY, "reasoning_effort": "low",
            "is_reasoning_model": True, "max_tokens": mem0_memory.EXTRACT_MAX_OUTPUT_TOKENS}},
        "embedder": {"provider": "openai", "config": {"model": "text-embedding-3-small",
                                                      "api_key": mem0_memory.PLACEHOLDER_KEY}},
        "vector_store": {"provider": "qdrant", "config": {
            "collection_name": "eve", "path": str(home / "qdrant"), "on_disk": True, "embedding_model_dims": 1536}},
        "history_db_path": str(home / "history.db"),
        "custom_instructions": mem0_memory.CUSTOM_INSTRUCTIONS,
    }


def test_gemma_is_a_separate_768_store_under_its_own_home_read_from_the_cache_only(tmp_path: Path) -> None:
    rc = rc_at(tmp_path)
    cfg = configured(rc, {**ON, "EVE_MEM0_EMBEDDER": " Gemma "})
    assert cfg.enabled and cfg.embedder == "gemma" and cfg.home == rc.mem0_dir / "gemma"
    conf = mem0_config(cfg)
    emb = conf["embedder"]
    assert emb["provider"] == "huggingface"
    assert emb["config"]["model"] == "unsloth/embeddinggemma-300m" and emb["config"]["embedding_dims"] == 768
    assert emb["config"]["model_kwargs"] == {"revision": mem0_memory.GEMMA_REVISION, "local_files_only": True}
    assert len(mem0_memory.GEMMA_REVISION) == 40                       # a pinned commit, not a branch
    assert "huggingface_base_url" not in emb["config"]                 # local, not a hosted endpoint
    vs = conf["vector_store"]["config"]
    assert vs["collection_name"] == "eve_gemma" and vs["embedding_model_dims"] == 768
    assert vs["path"] == str(rc.mem0_dir / "gemma" / "qdrant") != str(rc.mem0_dir / "qdrant")
    assert conf["history_db_path"] == str(rc.mem0_dir / "gemma" / "history.db")
    assert conf["llm"] == mem0_config(configured(rc, ON))["llm"]          # extraction unchanged
    assert configured(rc, {**ON, "EVE_MEM0_EMBEDDER": "gemma", "EVE_MEM0": "0"}).enabled is False


def test_shared_gives_gemma_its_own_instance(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    rc = rc_at(tmp_path)
    monkeypatch.setattr(mem0_memory, "available", lambda: True)
    a = mem0_memory.shared(rc, ON)
    b = mem0_memory.shared(rc, {**ON, "EVE_MEM0_EMBEDDER": "gemma"})
    assert a is not None and b is not None and a is not b
    assert a.home == rc.mem0_dir and b.home == rc.mem0_dir / "gemma"


class FakeST:
    """A SentenceTransformer that records the prompt each encode was given."""

    def __init__(self) -> None:
        self.calls: list[tuple[Any, str, bool]] = []

    def encode(self, texts: Any, prompt_name: str, normalize_embeddings: bool, convert_to_numpy: bool) -> Any:
        self.calls.append((texts, prompt_name, normalize_embeddings))
        one = [1.0] * 3
        return Arr(one if isinstance(texts, str) else [one for _ in texts])


class Arr:
    """What encode returns (a numpy array): only .tolist() is used."""

    def __init__(self, v: Any) -> None:
        self.v = v

    def tolist(self) -> Any:
        return self.v


class FakeEmbedder:
    def __init__(self) -> None:
        self.model = FakeST()


def test_gemma_encodes_a_search_as_a_query_and_everything_stored_as_a_document() -> None:
    emb = FakeEmbedder()
    memory = type("M", (), {"embedding_model": emb})()
    assert mem0_memory.on_gemma(memory) is memory
    assert emb.embed("where does my sister live", "search") == [1.0, 1.0, 1.0]
    emb.embed("sister moved to the coast", "add")
    emb.embed("sister moved inland", "update")
    emb.embed("no action given")
    assert emb.embed_batch(["a", "b"], "add") == [[1.0] * 3, [1.0] * 3]
    emb.embed_batch(["c"], "search")
    assert emb.embed_batch([], "add") == []
    prompts = [c[1] for c in emb.model.calls]
    assert prompts == ["query", "document", "document", "document", "document", "query"]
    assert all(c[2] for c in emb.model.calls)                          # normalised
    on_embedder_itself = FakeEmbedder()
    mem0_memory.on_gemma(on_embedder_itself).embed("x", "search")
    assert on_embedder_itself.model.calls[0][1] == "query"


def test_gemma_keeps_extraction_on_openrouter_and_metered_and_meters_no_embeddings(
        tmp_path: Path, monkeypatch: pytest.MonkeyPatch, caplog: pytest.LogCaptureFixture) -> None:
    monkeypatch.setenv("OPENROUTER_API_KEY", "or-test-not-real")
    emb = FakeEmbedder()
    llm = type("L", (), {"client": None})()
    memory = type("M", (), {"llm": llm, "embedding_model": emb})()
    mem0_memory.on_openrouter(memory, embeddings=False)
    assert str(llm.client.base_url).startswith("https://openrouter.ai/api/v1")
    assert not hasattr(emb, "client")                                   # embeddings never leave the Mac
    with caplog.at_level(logging.WARNING, logger="eve_bridge.mem0_memory"):
        mem0_memory.meter(memory, Mem0Config(enabled=True, home=tmp_path, embedder="gemma"))
    assert getattr(llm.client.chat.completions.create, "_eve_metered", False)
    assert "not metered" not in caplog.text


def test_warm_opens_the_store_and_embeds_once_and_never_raises(tmp_path) -> None:
    """The local embedder loads in ~3 s, more than a search's cap: the daemon warms it at start (2026-10-09)."""
    from eve_bridge import mem0_memory as m

    calls: list = []

    class Emb:
        def embed(self, text, action):
            calls.append((text, action))
            return [0.0]

    class Mem:
        embedding_model = Emb()

    cfg = m.Mem0Config(enabled=True, home=tmp_path / "mem0")
    om = m.OwnerMemory(cfg, factory=lambda c: Mem())
    om.warm()
    assert calls == [("warm", "search")]

    def broken(c):
        raise RuntimeError("no model")

    m.OwnerMemory(cfg, factory=broken).warm()          # logged, not raised
