"""think.py: the slow brain's request shape, config, and answer parsing."""

from __future__ import annotations

import asyncio
import json

import pytest

from eve_bridge import system_context, think, websearch
from eve_bridge.think import (
    ANSWER_MAX_CHARS,
    CONTEXT_HEADER,
    CONTEXT_MAX_CHARS,
    INSTRUCTIONS,
    OpenAIThinker,
    ThinkConfig,
    configured,
    make_thinker,
    parse_answer,
    question_items,
    request,
)


def test_request_reasons_hard_searches_the_web_and_keeps_nothing() -> None:
    cfg = ThinkConfig(model="gpt-6-astra", effort="high", search=websearch.SearchConfig(engine="openrouter-exa"))
    body = request(cfg, question_items("  what is\n the  answer "))
    assert body["model"] == "gpt-6-astra"
    assert body["reasoning"] == {"effort": "high"}
    assert body["tools"] == [websearch.WEB_SEARCH_TOOL] and body["tool_choice"] == "auto"   # Exa through OpenRouter
    assert body["store"] is False and body["include"] == ["reasoning.encrypted_content"]
    assert body["input"] == [{"type": "message", "role": "user",
                              "content": [{"type": "input_text", "text": "what is the answer"}]}]
    assert body["instructions"].startswith(INSTRUCTIONS) and "spoken" in INSTRUCTIONS
    # without an OpenRouter key the hosted search is offered, so nothing goes dark
    hosted = request(ThinkConfig(model="m", search=websearch.SearchConfig(engine="openai")), question_items("q"))
    assert hosted["tools"] == [{"type": "web_search"}]


def test_a_think_that_searches_gets_the_sources_and_answers_in_a_later_round() -> None:
    requests: list[dict] = []
    searched: list[str] = []
    replies = iter([
        {"status": "completed", "output": [
            {"type": "reasoning", "id": "rs1", "encrypted_content": "abc"},
            {"type": "function_call", "name": "web_search", "call_id": "c1", "arguments": "{\"query\": \"lakers score\"}"}]},
        {"status": "completed", "output": [
            {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "Lakers lost 104 to 112."}]}]},
    ])

    async def create(req: dict) -> dict:
        requests.append(req)
        return next(replies)

    def search(query: str) -> dict:
        searched.append(query)
        return {"ok": True, "answer": "Warriors 112, Lakers 104", "sources": [{"url": "https://espn.com"}]}

    cfg = ThinkConfig(model="m", search=websearch.SearchConfig(engine="openrouter-exa"))
    out = asyncio.run(OpenAIThinker(cfg, create=create, search=search)("who won the lakers game"))
    assert out == {"ok": True, "answer": "Lakers lost 104 to 112."} and searched == ["lakers score"]
    second = requests[1]["input"]
    assert [i["type"] for i in second] == ["message", "reasoning", "function_call", "function_call_output"]
    assert json.loads(second[-1]["output"])["sources"] == [{"url": "https://espn.com"}] and second[-1]["call_id"] == "c1"
    # a model that only ever searches is cut off after MAX_SEARCH_ROUNDS and the empty answer is an error
    forever = {"status": "completed", "output": [
        {"type": "function_call", "name": "web_search", "call_id": "c", "arguments": "{\"query\": \"x\"}"}]}

    async def loop(req: dict) -> dict:
        return forever

    with pytest.raises(RuntimeError):
        asyncio.run(OpenAIThinker(cfg, create=loop, search=search)("q"))


def test_configured_defaults_to_astra_at_high_effort() -> None:
    cfg = configured({}, backend_model="gpt-5-mini")                  # thinking is astra, whatever voice uses
    assert cfg == ThinkConfig(enabled=True, model="gpt-6-astra", effort="high", timeout_secs=90.0,
                              search=websearch.SearchConfig(engine="off"))       # no OpenRouter key: no search
    assert configured({"OPENROUTER_API_KEY": "r"}, backend_model="m").search.engine == "openrouter-perplexity"
    cfg = configured({"EVE_THINK_MODEL": "gpt-5.5", "EVE_THINK_EFFORT": "xhigh",
                      "EVE_THINK_TIMEOUT_SECS": "120"}, backend_model="gpt-6-astra")
    assert (cfg.model, cfg.effort, cfg.timeout_secs) == ("gpt-5.5", "xhigh", 120.0)
    # nonsense falls back, and is clamped
    cfg = configured({"EVE_THINK_EFFORT": "max", "EVE_THINK_TIMEOUT_SECS": "1"}, backend_model="m")
    assert (cfg.effort, cfg.timeout_secs) == ("high", 10.0)
    assert configured({"EVE_THINK": "0"}, backend_model="m").enabled is False


def test_make_thinker_is_none_without_a_key_or_when_off() -> None:
    assert make_thinker(ThinkConfig(model="m"), {}) is None                                  # no key
    assert make_thinker(ThinkConfig(enabled=False, model="m"), {"OPENROUTER_API_KEY": "k"}) is None
    thinker = make_thinker(ThinkConfig(model="m"), {"OPENROUTER_API_KEY": "k"})
    assert thinker is not None and thinker.config.model == "m"


def test_parse_answer_flattens_and_clips() -> None:
    assert parse_answer("  Forty-two.\n\nBecause.  ") == "Forty-two. Because."
    long = parse_answer("x " * ANSWER_MAX_CHARS)
    assert len(long) == ANSWER_MAX_CHARS and long.endswith("…")
    with pytest.raises(ValueError):
        parse_answer("   ")


CLOCK = "\n\nSystem context:\nLocal clock when this context was generated: fixed."


def test_no_context_keeps_the_body_byte_identical(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(system_context, "context", lambda *a, **k: CLOCK)
    cfg = ThinkConfig(model="m", search=websearch.SearchConfig(engine="openai"))
    items = question_items("q")
    plain = request(cfg, items)
    assert plain["instructions"] == INSTRUCTIONS + CLOCK            # the context-blind body, as before
    for empty in ("", "   \n  "):
        assert json.dumps(request(cfg, items, empty)) == json.dumps(plain)
    assert plain["store"] is False


def test_context_goes_after_the_instructions_before_the_clock_and_is_capped(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(system_context, "context", lambda *a, **k: CLOCK)
    cfg = ThinkConfig(model="m", search=websearch.SearchConfig(engine="openai"))
    body = request(cfg, question_items("plan my week"), "Owner works late on weekdays.")
    instr = body["instructions"]
    assert instr == INSTRUCTIONS + CONTEXT_HEADER + "Owner works late on weekdays." + CLOCK
    assert instr.index(INSTRUCTIONS) < instr.index("works late") < instr.index("System context:")
    assert body["store"] is False and body["input"] == question_items("plan my week")
    # a huge context is capped at CONTEXT_MAX_CHARS; the head is kept
    huge = request(cfg, question_items("q"), "A" + "x" * 20000 + "TAIL")["instructions"]
    ctx = huge[len(INSTRUCTIONS) + len(CONTEXT_HEADER): -len(CLOCK)]
    assert len(ctx) == CONTEXT_MAX_CHARS and ctx.startswith("Ax") and ctx.endswith("…") and "TAIL" not in huge


def test_the_thinker_sends_the_context_every_round(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(think.system_context, "context", lambda *a, **k: CLOCK)
    seen: list[str] = []
    replies = iter([
        {"status": "completed", "output": [
            {"type": "function_call", "name": "web_search", "call_id": "c1", "arguments": "{\"query\": \"x\"}"}]},
        {"status": "completed", "output": [
            {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "Tuesday."}]}]},
    ])

    async def create(req: dict) -> dict:
        seen.append(req["instructions"])
        assert req["store"] is False
        return next(replies)

    cfg = ThinkConfig(model="m", search=websearch.SearchConfig(engine="openrouter-exa"))
    thinker = OpenAIThinker(cfg, create=create, search=lambda q: {"ok": True})
    out = asyncio.run(thinker("which day suits me", context="Owner is free on Tuesdays."))
    assert out == {"ok": True, "answer": "Tuesday."}
    assert len(seen) == 2 and all("free on Tuesdays" in s for s in seen)
    # and a caller that passes no context still works, context-blind
    seen.clear()
    replies = iter([{"status": "completed", "output": [
        {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "Yes."}]}]}])
    assert asyncio.run(thinker("q"))["answer"] == "Yes." and seen == [INSTRUCTIONS + CLOCK]
