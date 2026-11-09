"""openrouter.py: one key for every model call (owner, 2026-10-08), and a guard that keeps it that way."""

from __future__ import annotations

import ast
import asyncio
import json
from pathlib import Path

import pytest

from eve_bridge import openrouter

SRC = Path(__file__).resolve().parents[1] / "src" / "eve_bridge"


# ---- model ids ---------------------------------------------------------------------------------------------

@pytest.mark.parametrize("name, wire", [
    ("gpt-6-luna", "openai/gpt-6-luna"),
    ("gpt-6.1-sol", "openai/gpt-6.1-sol"),
    ("gpt-5.4-nano", "openai/gpt-5.4-nano"),
    ("o4-mini", "openai/o4-mini"),
    ("text-embedding-3-small", "openai/text-embedding-3-small"),
    ("gpt-4o-transcribe", "openai/gpt-4o-transcribe"),
    ("gpt-4o-mini-transcribe", "openai/gpt-4o-mini-transcribe"),
    ("claude-opus-5-5", "anthropic/claude-opus-5.5"),           # the dot, not the dash
    ("openai/gpt-6-astra", "openai/gpt-6-astra"),               # already prefixed: unchanged
    ("anthropic/claude-opus-5.5", "anthropic/claude-opus-5.5"),
    ("deepgram/aura-2", "deepgram/aura-2"),
    ("google/gemini-3.1-flash-lite", "google/gemini-3.1-flash-lite"),
    ("", ""),
])
def test_model_ids_get_their_openrouter_prefix(name: str, wire: str) -> None:
    assert openrouter.model(name) == wire
    assert openrouter.model(wire) == wire                       # idempotent: a retry re-maps the same body


def test_the_key_gate_reads_only_the_openrouter_key() -> None:
    assert openrouter.available({"OPENROUTER_API_KEY": "or-test"})
    assert not openrouter.available({"OPENROUTER_API_KEY": "  "})
    assert not openrouter.available({"OPENAI_API_KEY": "openai-test", "ANTHROPIC_API_KEY": "anthropic-test"})
    assert openrouter.key({"OPENROUTER_API_KEY": " or-test "}) == "or-test"


def test_no_client_is_built_without_the_key(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("OPENROUTER_API_KEY", raising=False)
    monkeypatch.setenv("OPENAI_API_KEY", "sk-test")                # an OpenAI key is not a fallback
    with pytest.raises(openrouter.NoKey):
        openrouter.client()
    with pytest.raises(openrouter.NoKey):
        openrouter.async_client()


# ---- the clients ---------------------------------------------------------------------------------------------

def _capturing_client(sent: list, reply: dict):
    import httpx2
    import openai

    def handler(request):
        sent.append(request)
        return httpx2.Response(200, json=reply)

    return openrouter.client("or-test", http_client=openai.DefaultHttpxClient(transport=httpx2.MockTransport(handler)))


def test_every_request_goes_to_openrouter_with_the_prefixed_model() -> None:
    sent: list = []
    c = _capturing_client(sent, {"object": "list", "model": "x", "usage": {"prompt_tokens": 1, "total_tokens": 1},
                                 "data": [{"object": "embedding", "index": 0, "embedding": [0.5]}]})
    c.embeddings.create(model="text-embedding-3-small", input=["hi"], dimensions=1536, encoding_format="float")
    [req] = sent
    assert str(req.url) == "https://openrouter.ai/api/v1/embeddings"
    assert req.headers["authorization"] == "Bearer or-test"
    assert req.headers["x-title"] == "eve"
    assert b'"model":"openai/text-embedding-3-small"' in req.content
    assert "api.openai.com" not in str(c.base_url)


def test_a_multipart_transcription_gets_the_prefixed_model_too() -> None:
    sent: list = []
    c = _capturing_client(sent, {"text": "hi"})
    c.audio.transcriptions.create(model="gpt-4o-transcribe", file=("a.wav", b"RIFF", "audio/wav"), language="en")
    [req] = sent
    assert str(req.url) == "https://openrouter.ai/api/v1/audio/transcriptions"
    assert b"openai/gpt-4o-transcribe" in req.content and b"\r\n\r\ngpt-4o-transcribe\r\n" not in req.content


def test_the_async_client_points_at_openrouter() -> None:
    c = openrouter.async_client("or-test")
    assert str(c.base_url).startswith("https://openrouter.ai/api/v1")
    asyncio.run(c.close())


def test_the_anthropic_client_is_on_openrouter_with_the_dotted_opus_id() -> None:
    pytest.importorskip("anthropic")
    import httpx2

    sent: list = []

    def handler(request):
        sent.append(request)
        return httpx2.Response(200, json={
            "id": "m1", "type": "message", "role": "assistant", "model": "anthropic/claude-opus-5.5",
            "content": [{"type": "text", "text": "hi"}], "stop_reason": "end_turn", "stop_sequence": None,
            "usage": {"input_tokens": 3, "output_tokens": 1}})

    async def go():
        import anthropic

        c = openrouter.anthropic_client("or-test", http_client=anthropic.DefaultAsyncHttpxClient(
            transport=httpx2.MockTransport(handler)))
        assert str(c.base_url).startswith("https://openrouter.ai/api")
        await c.messages.create(model="claude-opus-5-5", max_tokens=5, messages=[{"role": "user", "content": "x"}])

    asyncio.run(go())
    [req] = sent
    assert str(req.url) == "https://openrouter.ai/api/v1/messages"
    assert b'"model":"anthropic/claude-opus-5.5"' in req.content


def test_the_brain_and_the_app_builder_build_their_clients_here(monkeypatch: pytest.MonkeyPatch) -> None:
    from eve_bridge import computer_agent

    monkeypatch.setenv("OPENROUTER_API_KEY", "or-test")
    monkeypatch.delenv("OPENAI_API_KEY", raising=False)
    c = computer_agent._openai_client(None, None)
    assert str(c.base_url).startswith("https://openrouter.ai/api/v1")
    asyncio.run(c.close())


# ---- the guard: no production module reads the old keys or calls OpenAI/Anthropic directly ---------------------

KEYS = {"OPENAI_API_KEY", "ANTHROPIC_API_KEY"}
DIRECT_HOSTS = ("https://api.openai.com", "https://api.anthropic.com")
CONSTRUCTORS = {"OpenAI", "AsyncOpenAI", "Anthropic", "AsyncAnthropic"}
# The only modules allowed to: each one is an explicit, documented exception (docs/openrouter.md).
ALLOWED = {
    "codex_auth.py",     # Codex CLI's optional `api` login mode (the default is the ChatGPT plan login)
    "voice_agent.py",    # the desk voice: OpenAI's realtime Live API, until its rebuild (wave 2)
    "phone_call.py",     # the call's live ears: OpenAI realtime transcription, opt-in (EVE_CALL_LIVE_STT=1)
    "spend_sync.py",     # the providers' own cost figures, on optional admin keys (never a model call)
    "decisions.py",      # OpenAI's Decisions beta: dev eval tools only; the daemon never builds a client
    "openrouter.py",     # Claude on Anthropic's own API with ANTHROPIC_API_KEY (owner, 2026-11-09)
}


def offences(source: str) -> list[str]:
    """What in this module's CODE reads an OpenAI/Anthropic key or talks to them directly: a key name or a direct
    host as a string literal that is not a docstring, or an SDK client constructed by name. Prose is ignored."""
    tree = ast.parse(source)
    docstrings = set()
    for node in ast.walk(tree):
        body = getattr(node, "body", None)
        if isinstance(body, list):
            for stmt in body:
                if (isinstance(stmt, ast.Expr) and isinstance(stmt.value, ast.Constant)
                        and isinstance(stmt.value.value, str)):
                    docstrings.add(id(stmt.value))
    found = []
    for node in ast.walk(tree):
        if isinstance(node, ast.Constant) and isinstance(node.value, str) and id(node) not in docstrings:
            if node.value in KEYS:
                found.append(f"line {node.lineno}: key {node.value}")
            elif node.value.startswith(DIRECT_HOSTS):
                found.append(f"line {node.lineno}: host {node.value}")
        elif isinstance(node, ast.Call):
            f = node.func
            name = f.id if isinstance(f, ast.Name) else f.attr if isinstance(f, ast.Attribute) else ""
            if name in CONSTRUCTORS:
                found.append(f"line {node.lineno}: {name}()")
    return found


def test_the_guard_catches_a_known_positive_control() -> None:
    # What every module looked like before 2026-10-08: each pattern must be caught, or the guard proves nothing.
    bad = '''
import os, openai, anthropic
key = os.environ.get("OPENAI_API_KEY")
client = openai.OpenAI(api_key=key)
other = anthropic.AsyncAnthropic()
URL = "https://api.openai.com/v1/responses"
'''
    found = offences(bad)
    assert any("OPENAI_API_KEY" in f for f in found)
    assert any("OpenAI()" in f for f in found)
    assert any("AsyncAnthropic()" in f for f in found)
    assert any("api.openai.com" in f for f in found)
    # prose is not code: a docstring or a comment that names the old key is fine
    assert offences('"""Set OPENAI_API_KEY for the desk voice."""\n# OPENAI_API_KEY\nx = 1\n') == []


def test_no_production_module_reads_the_openai_or_anthropic_key_except_the_allowed() -> None:
    files = sorted(SRC.rglob("*.py"))
    assert len(files) > 100                                       # the package, not an empty folder
    bad = {str(p.relative_to(SRC)): found for p in files
           if p.name not in ALLOWED and (found := offences(p.read_text(encoding="utf-8")))}
    assert bad == {}
    # each allowed module still needs its exception; one that no longer does should leave the list
    for name in ALLOWED:
        assert offences((SRC / name).read_text(encoding="utf-8")), name


def test_google_models_prefer_ai_studio_and_keep_fallbacks() -> None:
    """2026-10-08: Vertex "global" was 0% up for 5 minutes and every web search waited on it."""
    from eve_bridge import openrouter as orr
    from eve_bridge import websearch as ws

    assert orr.provider_prefs("google/gemini-3.1-flash-lite") == {"order": ["google-ai-studio"], "allow_fallbacks": True}
    assert orr.provider_prefs("openai/gpt-6-luna") is None
    body = ws.request(ws.SearchConfig(engine="openrouter-perplexity", model="google/gemini-3.1-flash-lite"), "who won")
    assert body["provider"] == {"order": ["google-ai-studio"], "allow_fallbacks": True}
    other = ws.request(ws.SearchConfig(engine="openrouter-perplexity", model="openai/gpt-5.4-nano"), "who won")
    assert "provider" not in other


# ---- Claude on Anthropic directly (owner, 2026-11-09) -------------------------------------------------------------

def test_claude_ids_for_anthropic() -> None:
    assert openrouter.anthropic_model("anthropic/claude-haiku-5.5") == "claude-haiku-5-5"
    assert openrouter.anthropic_model("claude-opus-5-5") == "claude-opus-5-5"
    assert openrouter.is_claude("claude-opus-5-5") and openrouter.is_claude("anthropic/claude-haiku-5.5")
    assert not openrouter.is_claude("gpt-6-luna") and not openrouter.is_claude("google/gemini-3.8-flash")


def test_the_direct_key_and_its_switch() -> None:
    assert openrouter.anthropic_key({"ANTHROPIC_API_KEY": " a-key "}) == "a-key"
    assert openrouter.anthropic_key({}) == ""
    assert openrouter.anthropic_key({"ANTHROPIC_API_KEY": "a-key", "EVE_ANTHROPIC_DIRECT": "0"}) == ""


def test_a_claude_chat_completion_goes_to_anthropic_and_others_stay(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("ANTHROPIC_API_KEY", "an-test")
    sent: list = []
    reply = {"id": "msg_1", "object": "chat.completion", "created": 1, "model": "x",
             "choices": [{"index": 0, "finish_reason": "stop", "message": {"role": "assistant", "content": "ok"}}]}
    c = _capturing_client(sent, reply)
    c.chat.completions.create(model="anthropic/claude-haiku-5.5", max_tokens=5,
                              messages=[{"role": "user", "content": "x"}],
                              extra_body={"provider": {"order": ["anthropic"]}, "usage": {"include": True},
                                          "reasoning": {"enabled": False}})
    c.chat.completions.create(model="gpt-6-luna", max_tokens=5, messages=[{"role": "user", "content": "x"}])
    claude, luna = sent
    assert str(claude.url) == "https://api.anthropic.com/v1/chat/completions"
    assert claude.headers["authorization"] == "Bearer an-test"
    body = json.loads(claude.content)
    assert body["model"] == "claude-haiku-5-5" and not {"provider", "usage", "reasoning"} & set(body)
    assert str(luna.url) == "https://openrouter.ai/api/v1/chat/completions"
    assert luna.headers["authorization"] == "Bearer or-test"
    monkeypatch.setenv("EVE_ANTHROPIC_DIRECT", "0")
    c.chat.completions.create(model="claude-haiku-5-5", max_tokens=5, messages=[{"role": "user", "content": "x"}])
    assert str(sent[-1].url) == "https://openrouter.ai/api/v1/chat/completions"


def test_the_anthropic_client_goes_to_anthropic_with_its_own_ids(monkeypatch: pytest.MonkeyPatch) -> None:
    pytest.importorskip("anthropic")
    import httpx2

    monkeypatch.setenv("ANTHROPIC_API_KEY", "an-test")
    sent: list = []

    def handler(request):
        sent.append(request)
        return httpx2.Response(200, json={
            "id": "m1", "type": "message", "role": "assistant", "model": "claude-opus-5-5",
            "content": [{"type": "text", "text": "hi"}], "stop_reason": "end_turn", "stop_sequence": None,
            "usage": {"input_tokens": 3, "output_tokens": 1}})

    async def go():
        import anthropic

        c = openrouter.anthropic_client(http_client=anthropic.DefaultAsyncHttpxClient(
            transport=httpx2.MockTransport(handler)))
        await c.messages.create(model="anthropic/claude-opus-5.5", max_tokens=5,
                                messages=[{"role": "user", "content": "x"}])

    asyncio.run(go())
    [req] = sent
    assert str(req.url) == "https://api.anthropic.com/v1/messages"
    assert req.headers["x-api-key"] == "an-test" and b'"model":"claude-opus-5-5"' in req.content
