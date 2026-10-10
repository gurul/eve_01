"""Exercise role selection at the outgoing request boundary, without network or owner's data."""

from __future__ import annotations

import asyncio
import dataclasses
import json
from pathlib import Path
from typing import Any

import pytest

from eve_bridge import chief_reflect, watch, web_reader, websearch
from eve_bridge.chief_card import Card, Phase, PhaseResult


def completion(data: Any) -> dict[str, Any]:
    return {"choices": [{"message": {"content": json.dumps(data)}}]}


def test_web_search_uses_synthesis_model_in_http_request() -> None:
    sent = []

    class Reply:
        def __enter__(self):
            return self

        def __exit__(self, *args):
            pass

        def read(self):
            return json.dumps({"choices": [{"message": {"content": "The museum opens at 9."}}]}).encode()

    def opener(req, **kwargs):
        sent.append(json.loads(req.data))
        return Reply()

    cfg = websearch.configured({"OPENROUTER_API_KEY": "test-key"})
    out = websearch.search("Museum hours", cfg, key="test-key", opener=opener)
    assert out["ok"] and out["answer"] == "The museum opens at 9."
    assert len(sent) == 1
    assert sent[0]["model"] == "openai/gpt-6-luna"
    assert "temperature" not in sent[0]
    assert sent[0]["reasoning"] == {"effort": "low"} and sent[0]["max_tokens"] >= 2048
    assert sent[0]["tools"][0]["type"] == "openrouter:web_search"


def test_page_synthesis_uses_same_research_model_and_keeps_read_sources() -> None:
    sent = []

    def ask(body):
        sent.append(body)
        return completion({"answered": True, "answer": "The museum opens at 9.", "sources": [1]})

    answer, sources, _ = web_reader.answer_parts(
        "Museum hours", [{"url": "https://museum.example/hours", "title": "Hours", "markdown": "Open at 9."}], ask=ask)
    assert answer == "The museum opens at 9." and sources == ["https://museum.example/hours"]
    assert len(sent) == 1 and sent[0]["model"] == "openai/gpt-6-luna"
    assert "temperature" not in sent[0]
    assert sent[0]["reasoning"] == {"effort": "low"} and sent[0]["max_tokens"] >= 2048


def test_synthesis_model_override_keeps_its_previous_budget() -> None:
    cfg = websearch.configured({"OPENROUTER_API_KEY": "test-key", "EVE_WEB_SEARCH_MODEL": "google/gemini-3.1-flash-lite"})
    body = websearch.request(cfg, "Museum hours")
    assert body["model"] == "google/gemini-3.1-flash-lite"
    assert body["max_tokens"] == 400 and body["temperature"] == 0 and "reasoning" not in body


def test_gemini_synthesis_override_keeps_compatible_sampling_and_reasoning() -> None:
    cfg = websearch.configured({"OPENROUTER_API_KEY": "test-key", "EVE_WEB_SEARCH_MODEL": "google/gemini-3.8-flash"})
    body = websearch.request(cfg, "Museum hours")
    assert body["model"] == "google/gemini-3.8-flash" and body["temperature"] == 0
    assert body["reasoning"] == {"effort": "low"} and body["max_tokens"] >= 2048


@pytest.mark.parametrize("mode", ["page", "search", "vision"])
@pytest.mark.parametrize("override", [False, True])
def test_watch_requests_keep_their_role_independent_of_synthesis(
        tmp_path: Path, mode: str, override: bool) -> None:
    sent = []
    env = {"OPENROUTER_API_KEY": "test-key", "EVE_WEB_SEARCH_MODEL": "openai/gpt-6-luna"}
    if override:
        env.update({"EVE_WATCH_MODEL": "google/gemini-3.1-flash-lite",
                    "EVE_WATCH_VISION_MODEL": "anthropic/claude-haiku-5.5"})
    cfg = dataclasses.replace(watch.configured(env), enabled=True, path=tmp_path / "watches.json", browser=True)

    def ask(body):
        sent.append(body)
        return completion({"price": 19.99, "currency": "USD", "available": True, "note": "In stock."})

    watcher = watch.Watcher(cfg, ask=ask, fetch=lambda *args, **kwargs: (200, "<p>Price: $19.99</p>"),
                            render=lambda *args, **kwargs: ("<div id='root'></div>", b"jpeg"), offload=False)
    item = watch.Watch(id="w1", kind="search" if mode == "search" else "page", label="Mug", condition="below",
                       value=20, target="mug price" if mode == "search" else "https://shop.example/mug",
                       via="browser" if mode == "vision" else "")
    out = asyncio.run(watcher.read(item))
    assert out.value == 19.99 and len(sent) == 1
    body = sent[0]
    expected_model = ("anthropic/claude-haiku-5.5" if mode == "vision" else "google/gemini-3.1-flash-lite") \
        if override else "deepseek/deepseek-v4.1-flash"
    assert body["model"] == expected_model
    if override:
        assert "reasoning" not in body
    else:
        assert body["reasoning"] == {"enabled": False}
    assert ("temperature" in body) is (not override or mode != "vision")
    assert body["max_tokens"] == (300 if mode == "search" else 200)
    if mode == "vision":
        assert body["messages"][1]["content"][1]["type"] == "image_url"
    elif mode == "search":
        assert body["tools"][0]["type"] == "openrouter:web_search"


@pytest.mark.parametrize("model,reasoning,temperature", [
    (None, True, False),
    ("openai/gpt-6.1-sol", True, False),
    ("google/gemini-3.8-flash", False, True),
    ("anthropic/claude-haiku-5.5", False, False),
])
def test_reflector_sends_medium_reasoning_and_allows_model_overrides(
        tmp_path: Path, model: str | None, reasoning: bool, temperature: bool) -> None:
    sent = []

    def ask(body):
        sent.append(body)
        return completion({"lesson": "Check the museum's hours page.", "retry": True})

    reflector = chief_reflect.Reflector(chief_reflect.Memory(tmp_path / "lessons"), ask=ask, offload=False,
                                        **({"model": model} if model else {}))
    phase = Phase(1, "research", "find museum opening hours", executor="web")
    card = Card(id="c1", rev=1, kind="job", title="Museum hours", purpose="", end_state="opening hours found",
                done_checks=(), phases=(phase,))
    out = asyncio.run(reflector.reflect(card, phase, PhaseResult("failed"), [], executor="web", trial=1,
                                       why=["result_failed"]))
    assert out and out.retry and len(sent) == 1
    body = sent[0]
    assert body["model"] == (model or "openai/gpt-6-luna")
    if reasoning:
        assert body["reasoning"] == {"effort": "medium"}
        assert body["max_tokens"] >= 2048
    else:
        assert "reasoning" not in body and body["max_tokens"] == 200
    assert ("temperature" in body) is temperature
    assert reflector.memory.for_phase("c1", 1) == ["Check the museum's hours page."]
