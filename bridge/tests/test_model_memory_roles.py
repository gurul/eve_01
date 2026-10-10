"""Capture real SDK request bodies: memory, compaction, notes and diary roles stay separate."""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import pytest

from eve_bridge import diary, explore, meet, mem0_memory, notes, openrouter, records


@pytest.fixture
def requests_sent(monkeypatch: pytest.MonkeyPatch):
    import httpx2
    import openai

    sent: list[dict[str, Any]] = []
    make_client = openrouter.client

    def handle(request):
        body = json.loads(request.content)
        sent.append(body)
        assert str(request.url).startswith(openrouter.BASE_URL)
        if request.url.path.endswith("/embeddings"):
            reply = {"object": "list", "model": body["model"],
                     "data": [{"object": "embedding", "index": 0, "embedding": [0.1] * 4}]}
        elif request.url.path.endswith("/chat/completions"):
            reply = {"id": "test", "object": "chat.completion", "created": 0, "model": body["model"],
                     "choices": [{"index": 0, "finish_reason": "stop",
                                  "message": {"role": "assistant", "content": "{}"}}]}
        else:
            reply = {"id": "test", "object": "response", "created_at": 0, "status": "completed",
                     "model": body["model"], "output": [{"id": "message", "type": "message",
                     "role": "assistant", "status": "completed", "content": [
                         {"type": "output_text", "text": "{}", "annotations": []}]}]}
        return httpx2.Response(200, json=reply)

    def client(api_key=None, **kwargs):
        return make_client(api_key or "test-provider-key", **kwargs, http_client=openai.DefaultHttpxClient(
            transport=httpx2.MockTransport(handle)))

    monkeypatch.setattr(openrouter, "client", client)
    return sent


@pytest.mark.parametrize("overrides, models", [
    ({}, ["openai/gpt-6-luna", "openai/gpt-6-luna"]),
    ({"model": "openai/gpt-6.1-sol", "consolidation_model": "deepseek/deepseek-v4.1-flash"},
     ["openai/gpt-6.1-sol", "deepseek/deepseek-v4.1-flash"]),
])
def test_records_extract_and_compact_with_independent_models(requests_sent, overrides, models) -> None:
    client = records.OpenAIReconcileClient(**overrides)
    assert client.reconcile("day's facts") == "{}"
    assert client.consolidate("existing records") == "{}"
    assert [r["model"] for r in requests_sent] == models
    for request, name in zip(requests_sent, ["dream_day", "dream_consolidate"], strict=True):
        assert request["reasoning"] == {"effort": "low"}
        assert request["store"] is False
        assert request["text"]["format"]["name"] == name
        assert request["text"]["format"]["strict"] is True
        assert request["max_output_tokens"] == records.MAX_OUTPUT_TOKENS


@pytest.mark.parametrize("override, model", [
    ({}, "openai/gpt-6-luna"),
    ({"summary_model": "gpt-6.1-sol"}, "openai/gpt-6.1-sol"),
])
def test_room_notes_summary_and_reduce_use_summary_model(requests_sent, override, model) -> None:
    client = notes.OpenAINotesClient(**override)
    assert client.summarize("transcript") == "{}"
    assert client.combine("partial summaries") == "{}"
    assert [r["model"] for r in requests_sent] == [model, model]
    assert [r["instructions"] for r in requests_sent] == [notes.SUMMARY_PROMPT, notes.COMBINE_PROMPT]
    for request in requests_sent:
        assert request["reasoning"] == {"effort": "low"}
        assert request["text"]["format"]["schema"] == notes.SUMMARY_SCHEMA
        assert request["max_output_tokens"] == notes.SUMMARY_MAX_OUTPUT_TOKENS


@pytest.mark.parametrize("override, model", [
    (None, "openai/gpt-6-luna"),
    ("  ", "openai/gpt-6-luna"),
    (" gpt-6.1-sol ", "openai/gpt-6.1-sol"),
])
def test_meet_summary_preserves_model_override(requests_sent, override, model) -> None:
    env = {"OPENROUTER_API_KEY": "test-provider-key"}
    if override is not None:
        env["EVE_MEET_SUMMARY_MODEL"] = override
    summarize = meet.make_summarizer(env)
    assert summarize is not None and summarize("transcript") == {}
    [request] = requests_sent
    assert request["model"] == model
    assert request["reasoning"] == {"effort": "low"}
    assert request["text"]["format"]["schema"] == meet.MEET_SCHEMA


@pytest.mark.parametrize("camera_model", [explore.DEFAULT_MODEL, "google/gemini-3.8-flash"])
def test_diary_reflection_is_independent_of_camera_override(requests_sent, camera_model) -> None:
    client = diary.make_diary_client(camera_model, {"OPENROUTER_API_KEY": "test-provider-key"})
    assert client is not None
    assert client.think(b"image", "image/jpeg", "context") == "{}"
    assert client.examine(b"image", "image/jpeg", "context") == "{}"
    assert client.reflect("diary entries") == "{}"
    assert [r["model"] for r in requests_sent] == [
        openrouter.model(camera_model), openrouter.model(camera_model), "openai/gpt-6-luna"]
    assert [r["reasoning"] for r in requests_sent[:2]] == [{"effort": "low"}, {"effort": "low"}]
    reflection = requests_sent[-1]
    assert reflection["reasoning"] == {"effort": "medium"}
    assert reflection["max_output_tokens"] == diary.REFLECT_MAX_OUTPUT_TOKENS
    assert reflection["instructions"] == diary.REFLECT_PROMPT


def test_diary_explicit_reflection_override_is_preserved(requests_sent) -> None:
    client = diary.OpenAIDiaryClient("google/gemini-3.8-flash", reflection_model="gpt-6.1-sol")
    client.reflect("diary entries")
    [request] = requests_sent
    assert request["model"] == "openai/gpt-6.1-sol"
    assert request["reasoning"] == {"effort": "medium"}


@pytest.mark.parametrize("model", [mem0_memory.DEFAULT_MODEL, "openai/gpt-6.1-sol"])
def test_real_mem0_sends_low_effort_and_budget_without_changing_embeddings(
        tmp_path: Path, monkeypatch: pytest.MonkeyPatch, requests_sent, model: str) -> None:
    monkeypatch.setenv("MEM0_TELEMETRY", "False")
    monkeypatch.setenv("MEM0_DIR", str(tmp_path / "mem0-home"))
    pytest.importorskip("mem0", reason="install bridge[mem0] to verify the optional SDK")
    monkeypatch.delenv("OPENAI_API_KEY", raising=False)
    monkeypatch.setenv("OPENROUTER_API_KEY", "test-provider-key")
    cfg = mem0_memory.Mem0Config(enabled=True, home=tmp_path / "mem0", model=model)
    memory = mem0_memory.open_mem0(cfg)
    memory.llm.generate_response(messages=[{"role": "user", "content": "remember a fact"}])
    memory.embedding_model.embed("a fact")
    extraction, embedding = requests_sent
    assert extraction["model"] == openrouter.model(model)
    assert extraction["reasoning_effort"] == "low"
    assert extraction["max_completion_tokens"] == mem0_memory.EXTRACT_MAX_OUTPUT_TOKENS
    assert "temperature" not in extraction and "top_p" not in extraction
    assert embedding["model"] == "openai/text-embedding-3-small"
    assert mem0_memory.mem0_config(cfg)["vector_store"]["config"]["embedding_model_dims"] == 1536
    # Positive control for the absence check: without our explicit SDK flag the
    # very same transport sees mem0's default sampling parameters.
    memory.llm.config.is_reasoning_model = False
    memory.llm.generate_response(messages=[{"role": "user", "content": "a second fact"}])
    assert "temperature" in requests_sent[-1] and "top_p" in requests_sent[-1]


def test_local_embeddings_keep_the_same_extraction_settings() -> None:
    remote = mem0_memory.mem0_config(mem0_memory.Mem0Config())
    local = mem0_memory.mem0_config(mem0_memory.Mem0Config(embedder="gemma"))
    assert local["llm"] == remote["llm"]
    assert local["llm"]["config"]["reasoning_effort"] == "low"
    assert local["llm"]["config"]["is_reasoning_model"] is True
    assert local["embedder"]["provider"] == "huggingface"
