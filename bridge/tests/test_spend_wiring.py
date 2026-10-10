"""Every place eve spends money writes one line into the daily spend meter (spend.py). Each test drives the real
call site with a fake provider and reads the line back. The chat (test_telegram.py) and the voice
(test_voice_agent.py) and the app builder (test_miniapp.py) are tested beside their own harnesses."""

from __future__ import annotations

import asyncio
import io
import json
import types
import wave
from datetime import date
from pathlib import Path
from typing import Any

import pytest

from eve_bridge import spend

M = 1_000_000


def lines(folder: Path) -> list[dict[str, Any]]:
    return spend.day_rows(date.today().isoformat(), folder)


def one(folder: Path) -> dict[str, Any]:
    got = lines(folder)
    assert len(got) == 1, got
    return got[0]


class Reply:
    """What the openai SDK hands back: attributes plus model_dump."""

    def __init__(self, text: str = '{"intent": "none", "confidence": 0.1}', usage: Any = None, **extra: Any) -> None:
        self.output_text = text
        self.text = text
        self.status = "completed"
        self.incomplete_details = None
        self.usage = usage if usage is not None else {"input_tokens": M, "output_tokens": 0}
        self._extra = extra

    def model_dump(self, exclude_none: bool = False) -> dict[str, Any]:
        return {"usage": self.usage, **self._extra}


class FakeSdk:
    """client.responses.create / client.audio.transcriptions.create, recording each call."""

    def __init__(self, reply: Reply) -> None:
        self.calls: list[dict[str, Any]] = []

        def create(**kw: Any) -> Reply:
            self.calls.append(kw)
            return reply

        self.responses = types.SimpleNamespace(create=create)
        self.audio = types.SimpleNamespace(transcriptions=types.SimpleNamespace(create=create))


# ---- thinking, the browser & Mac tasks ---------------------------------------------------------------------

def test_think_records_each_round(_spend_ledger_in_tmp: Path) -> None:
    from eve_bridge import think, websearch

    async def create(req: dict[str, Any]) -> dict[str, Any]:
        return {"output": [{"type": "message", "content": [{"type": "output_text", "text": "Forty two."}]}],
                "usage": {"input_tokens": M, "output_tokens": 0}}

    cfg = think.ThinkConfig(model="gpt-6-sol", search=websearch.SearchConfig(engine="openai"))
    assert asyncio.run(think.OpenAIThinker(cfg, create=create)("why?"))["ok"]
    row = one(_spend_ledger_in_tmp)
    assert (row["f"], row["m"], row["usd"]) == ("thinking", "gpt-6-sol", pytest.approx(2.0))


def test_every_computer_agent_model_call_is_metered(_spend_ledger_in_tmp: Path) -> None:
    from eve_bridge.computer_agent import AgentConfig, ComputerAgent

    async def create(req: dict[str, Any]) -> dict[str, Any]:
        return {"id": "r", "usage": {"input_tokens": M, "output_tokens": 0}}

    agent = ComputerAgent(create, config=AgentConfig(model="gpt-6-astra"))
    asyncio.run(agent._create({"model": "gpt-6-astra"}, 1))
    row = one(_spend_ledger_in_tmp)
    assert (row["f"], row["p"], row["usd"]) == ("browser & Mac tasks", "openrouter", pytest.approx(10.0))


# ---- search, jev -------------------------------------------------------------------------------------------

class Resp(io.BytesIO):
    def __enter__(self) -> "Resp":
        return self

    def __exit__(self, *exc: Any) -> None:
        self.close()


def test_a_web_search_records_openrouters_own_cost(_spend_ledger_in_tmp: Path) -> None:
    from eve_bridge import websearch

    body = {"choices": [{"message": {"content": "Sunny."}}],
            "usage": {"prompt_tokens": 900, "completion_tokens": 40, "cost": 0.0056}}
    cfg = websearch.SearchConfig(engine="openrouter-perplexity")
    assert websearch.search("weather", cfg, key="r", opener=lambda req, timeout: Resp(json.dumps(body).encode()))["ok"]
    row = one(_spend_ledger_in_tmp)
    assert (row["p"], row["f"], row["usd"], row["src"]) == ("openrouter", "search", 0.0056, "reported")
    assert row["m"] == websearch.DEFAULT_MODEL and row["tok"] == {"in": 900, "out": 40}


@pytest.mark.parametrize("url, usage, provider, usd, src", [
    ("https://api.typesafe.ai/v1/systemone", {"input_tokens": M}, "typesafe", 0.042, "priced"),
    ("https://openrouter.ai/api/alpha/decisions", {"input_tokens": 10, "cost": 0.001}, "openrouter", 0.001, "reported"),
])
def test_a_jev_decision_is_metered_on_its_routes_bill(_spend_ledger_in_tmp: Path, url: str, usage: dict,
                                                      provider: str, usd: float, src: str) -> None:
    from eve_bridge import jev

    payload = {"answers": {"q": {"choice": "a"}}, "usage": usage}
    predict = jev.make_predict(url, "k", "typesafe/jev-1.13", opener=lambda req, timeout: Resp(json.dumps(payload).encode()))
    predict({}, {"q": {}})
    row = one(_spend_ledger_in_tmp)
    assert (row["p"], row["f"], row["usd"], row["src"]) == (provider, "jev", pytest.approx(usd), src)


# ---- the synchronous clients: intent, camera, exploring, memory, room notes -------------------------------

def test_the_voice_intent_classifier_is_metered(_spend_ledger_in_tmp: Path) -> None:
    from eve_bridge.intent import OpenAIIntentClient

    c = OpenAIIntentClient("gpt-5.4-nano", api_key="sk-test")
    c._client = FakeSdk(Reply())
    c.classify("bye")
    assert (one(_spend_ledger_in_tmp)["f"], one(_spend_ledger_in_tmp)["usd"]) == ("voice", pytest.approx(0.20))


def test_the_camera_describe_and_locate_are_metered(_spend_ledger_in_tmp: Path) -> None:
    from eve_bridge.scene import OpenAISceneClient

    c = OpenAISceneClient("gpt-5.4-nano", api_key="sk-test")
    c._client = FakeSdk(Reply("not json"))
    c.describe(b"x", "image/jpeg", None)
    with pytest.raises(ValueError):                                 # not a locate answer; the call still cost
        c.locate(b"x", "image/jpeg", "mug")
    got = lines(_spend_ledger_in_tmp)
    assert [r["f"] for r in got] == ["camera", "camera"] and got[0]["usd"] == pytest.approx(0.20)


def test_exploring_notes_and_the_diary_are_metered(_spend_ledger_in_tmp: Path) -> None:
    from eve_bridge.diary import OpenAIDiaryClient
    from eve_bridge.explore import OpenAINoteClient

    note = OpenAINoteClient("gpt-5-mini", api_key="sk-test")
    note._client = FakeSdk(Reply("a room"))
    note.describe(b"x", "image/jpeg")
    diary = OpenAIDiaryClient("gpt-5-mini", api_key="sk-test")
    diary._client = FakeSdk(Reply("{}"))
    diary.think(b"x", "image/jpeg", "ctx")
    diary.examine(b"x", "image/jpeg", "ctx")
    diary.reflect("p")
    got = lines(_spend_ledger_in_tmp)
    assert [r["f"] for r in got] == ["exploring"] * 4 and got[0]["usd"] == pytest.approx(0.25)


def test_the_dream_is_metered_as_memory(_spend_ledger_in_tmp: Path) -> None:
    from eve_bridge import records

    c = records.OpenAIReconcileClient("gpt-5.4-nano", api_key="sk-test")
    c._client = FakeSdk(Reply("{}"))
    c.reconcile("day")
    assert (one(_spend_ledger_in_tmp)["f"], one(_spend_ledger_in_tmp)["m"]) == ("memory", "gpt-5.4-nano")


def wav(seconds: float) -> bytes:
    buf = io.BytesIO()
    with wave.open(buf, "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(16000)
        w.writeframes(b"\x00\x00" * int(16000 * seconds))
    return buf.getvalue()


def test_room_notes_transcription_by_tokens_or_by_the_clip_and_the_summary(_spend_ledger_in_tmp: Path) -> None:
    from eve_bridge.notes import OpenAINotesClient

    c = OpenAINotesClient(api_key="sk-test")
    c._client = FakeSdk(Reply("hello", usage={"type": "tokens", "input_tokens": M, "output_tokens": 0}))
    c.transcribe(wav(1.0))
    c._client = FakeSdk(Reply("hello", usage={}))
    c.transcribe(wav(60.0))                                      # no token usage: the clip's length at $0.003/min
    c._client = FakeSdk(Reply('{"summary": ""}'))
    c.summarize("we talked")
    a, b, s = lines(_spend_ledger_in_tmp)
    assert (a["f"], a["m"], a["usd"]) == ("transcription", "gpt-4o-mini-transcribe", pytest.approx(1.25))
    assert b["usd"] == pytest.approx(0.003)
    assert (s["f"], s["m"]) == ("room notes", "gpt-6-luna")


# ---- mem0 --------------------------------------------------------------------------------------------------

def test_mem0s_extraction_and_embeddings_are_metered(_spend_ledger_in_tmp: Path) -> None:
    from eve_bridge import mem0_memory

    completions = types.SimpleNamespace(create=lambda **kw: {"usage": {"prompt_tokens": M, "completion_tokens": 0}})
    embeddings = types.SimpleNamespace(create=lambda **kw: types.SimpleNamespace(
        usage=types.SimpleNamespace(model_dump=lambda exclude_none=False: {"prompt_tokens": M, "total_tokens": M}),
        data=[]))
    memory = types.SimpleNamespace(
        llm=types.SimpleNamespace(client=types.SimpleNamespace(chat=types.SimpleNamespace(completions=completions))),
        embedding_model=types.SimpleNamespace(client=types.SimpleNamespace(embeddings=embeddings)))
    cfg = mem0_memory.Mem0Config()
    assert mem0_memory.meter(memory, cfg) is memory
    mem0_memory.meter(memory, cfg)                               # wrapped once, not twice
    memory.llm.client.chat.completions.create(model="gpt-5.4-nano", messages=[])
    memory.embedding_model.client.embeddings.create(model="text-embedding-3-small", input=["x"])
    a, b = lines(_spend_ledger_in_tmp)
    assert (a["f"], a["p"], a["m"], a["usd"]) == ("memory", "openrouter", "gpt-5.4-nano", pytest.approx(0.20))
    assert (b["m"], b["usd"]) == ("text-embedding-3-small", pytest.approx(0.02))


def test_a_mem0_without_the_expected_clients_is_left_alone(_spend_ledger_in_tmp: Path) -> None:
    from eve_bridge import mem0_memory

    odd = types.SimpleNamespace()
    assert mem0_memory.meter(odd, mem0_memory.Mem0Config()) is odd


# ---- lessons, codex -----------------------------------------------------------------------

def test_a_lesson_turn_and_its_reference_search_are_metered(_spend_ledger_in_tmp: Path, monkeypatch) -> None:
    import urllib.request

    from eve_bridge import watch
    from eve_bridge.learning import search as lsearch
    from eve_bridge.learning.tutor import LiveTutor

    monkeypatch.setattr(watch, "http_request", lambda url, **kw: (200, json.dumps(
        {"query": "q", "results": [], "total_results": 0, "page": 0})))
    replies = {"https://openrouter.ai/api/v1/chat/completions": {
                   "usage": {"prompt_tokens": M, "completion_tokens": 0},
                   "choices": [{"finish_reason": "stop", "message": {"content": "{}"}}]}}
    monkeypatch.setattr(urllib.request, "urlopen",
                        lambda req, timeout=0: Resp(json.dumps(replies[req.full_url]).encode()))
    monkeypatch.setenv("OPENROUTER_API_KEY", "sk-test")
    monkeypatch.setenv("TINYFISH_API_KEY", "tf-test")
    monkeypatch.delenv("EVE_LEARNING_PROVIDER", raising=False)
    monkeypatch.delenv("EVE_LEARNING_MODEL", raising=False)
    lsearch.search_problems("fractions", "grade 5")
    try:
        LiveTutor()("next", {"topic": "fractions", "level": "5", "mode": "learn", "problem": "", "ideas": [],
                             "events": [], "stuck": False})
    except ValueError:
        pass                                                       # the fake reply is not a lesson; it was metered
    search, turn = lines(_spend_ledger_in_tmp)
    assert (search["p"], search["f"], search["usd"], search["note"]) == ("tinyfish", "lessons", 0.0,
                                                                         "0 pages, free")
    assert (turn["f"], turn["p"], turn["m"], turn["usd"]) == ("lessons", "openrouter", "openai/gpt-6-astra",
                                                               pytest.approx(10.0))


def test_codex_is_counted_without_a_price(_spend_ledger_in_tmp: Path) -> None:
    from eve_bridge import codex_computer

    agent = codex_computer.CodexComputerAgent(on_event=lambda e: None, ask_user=None, binary="/nonexistent/codex")
    asyncio.run(agent.run("open notes"))                           # cannot start: still one task counted
    row = one(_spend_ledger_in_tmp)
    assert (row["p"], row["m"], row["f"], row["usd"]) == ("chatgpt", "codex", "codex", None)
    assert row["note"].startswith("ChatGPT plan, not per-call")
