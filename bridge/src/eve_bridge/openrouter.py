"""One key for every model eve calls: the owner's OpenRouter key.

Owner, 2026-10-08: "make everything use my openrouter key, remove open ai and claude key". Before this, the chat,
the planner, the camera, memory, notes and the calls each built an OpenAI client on ``OPENAI_API_KEY``, and the app
builder an Anthropic client on ``ANTHROPIC_API_KEY``. Now every one of them gets its client here, and every client
points at OpenRouter with ``OPENROUTER_API_KEY``.

* ``client()`` / ``async_client()`` are the openai SDK's own clients with OpenRouter's base URL, the key and
  eve's attribution headers. They rewrite the request's ``model`` as it is sent (``model``), so the call sites
  keep their model names as they were ("gpt-6-luna") and OpenRouter gets "openai/gpt-6-luna". Responses, streaming,
  Chat Completions, embeddings, transcription and speech all go through the same request builder, so one override
  covers them all (verified live 2026-10-08: function tools, encrypted reasoning, json_schema strict, images, PDFs
  and streaming work on OpenRouter's Responses API).
* ``anthropic_client()`` is the anthropic SDK's AsyncAnthropic on OpenRouter's Anthropic-shaped endpoint
  (``https://openrouter.ai/api``), the same key, with the same rewrite ("claude-opus-5-5" → "anthropic/claude-opus-5.5").
* What OpenRouter does not have: ``previous_response_id`` (refused: "not supported on this proxy"), so a chain
  resends its history with ``store: False`` and the encrypted reasoning; OpenAI's realtime WebSockets (the desk voice
  and the call's live ears, both left on ``OPENAI_API_KEY`` as opt-ins); OpenAI's Decisions beta and the diarizing
  transcriber. docs/openrouter.md has the whole list.

**Claude goes to Anthropic directly** (owner, 2026-11-09: "replace anthropic use cases with this instead of open
router for eve ... anthropic models"). With ``ANTHROPIC_API_KEY`` set, a Claude model is sent to Anthropic's own API on
that key: ``anthropic_client()`` is the SDK on api.anthropic.com, and a Chat Completions call on a Claude model from
``client()``/``async_client()`` goes to Anthropic's OpenAI-compatible endpoint (``ANTHROPIC_CHAT_URL``), with the
model's Anthropic id ("anthropic/claude-haiku-5.5" → "claude-haiku-5-5") and without OpenRouter's own body fields
(``OPENROUTER_ONLY``). A Responses API call on a Claude model stays on OpenRouter (Anthropic has no Responses API).
``EVE_ANTHROPIC_DIRECT=0`` sends everything to OpenRouter as before. Every other model is unchanged.

Spend (spend.py) is recorded at each call site as before, under the provider ``openrouter``; the model name in the
row is the call site's own, so pricing.py's tables still read it.
"""

from __future__ import annotations

import os
import re
from typing import Any, Optional

KEY_ENV = "OPENROUTER_API_KEY"
ANTHROPIC_KEY_ENV = "ANTHROPIC_API_KEY"
DIRECT_SWITCH = "EVE_ANTHROPIC_DIRECT"                  # on (default) while ANTHROPIC_API_KEY is set; 0 turns it off
ANTHROPIC_CHAT_URL = "https://api.anthropic.com/v1/chat/completions"
# OpenRouter's own body fields: Anthropic's endpoint does not know them.
OPENROUTER_ONLY = ("provider", "usage", "reasoning", "transforms", "plugins", "models", "route")
BASE_URL = "https://openrouter.ai/api/v1"
ANTHROPIC_BASE_URL = "https://openrouter.ai/api"       # the anthropic SDK adds /v1/messages itself
PROVIDER = "openrouter"                                 # the spend ledger's provider name for these calls
HEADERS = {"HTTP-Referer": "https://github.com/gurul/eve_01", "X-Title": "eve"}

# Bare OpenAI model names: gpt-*, chatgpt-*, the o-series (o3, o4-mini), embeddings, transcribers, tts, whisper.
_OPENAI = re.compile(r"^(?:gpt-|chatgpt-|o\d|text-embedding-|whisper|tts-|computer-use-)", re.I)
# Anthropic's API ids write the version with dashes ("claude-opus-5-5"); OpenRouter's with a dot ("claude-opus-5.5").
_CLAUDE_VERSION = re.compile(r"^(claude-[a-z]+)-(\d+)-(\d+)$", re.I)


class NoKey(RuntimeError):
    """OPENROUTER_API_KEY is not set: no model call can be made."""


# Which upstream host OpenRouter should try first, per model family (fallbacks stay allowed). 2026-10-08: OpenRouter
# mixed google/gemini-3.1-flash-lite across Google AI Studio (0.72 s, 100% up) and Vertex "global" (0% up for the
# last 5 minutes, 72.7% over the day, one 504), and every web search waited on the sick one.
PROVIDER_ORDER: dict[str, tuple[str, ...]] = {"google/": ("google-ai-studio",)}


def provider_prefs(model_id: str) -> Optional[dict[str, Any]]:
    """OpenRouter's ``provider`` field for a chat completion on ``model_id``: the preferred hosts first, fallbacks on;
    None when there is no preference."""
    for prefix, order in PROVIDER_ORDER.items():
        if model_id.startswith(prefix):
            return {"order": list(order), "allow_fallbacks": True}
    return None


def key(env: Any = None) -> str:
    """The OpenRouter key, stripped, or ""."""
    e = os.environ if env is None else env
    return (e.get(KEY_ENV) or "").strip()


def available(env: Any = None) -> bool:
    """Whether eve can call a model at all: OPENROUTER_API_KEY is set."""
    return bool(key(env))


def model(name: str) -> str:
    """The OpenRouter id for a model name. An id that already names its provider ("openai/…", "deepgram/aura-2")
    is left as it is; a bare OpenAI name gets "openai/"; a Claude name gets "anthropic/" and a dotted version."""
    n = (name or "").strip()
    if not n or "/" in n:
        return n
    if _OPENAI.match(n):
        return "openai/" + n
    if n.lower().startswith("claude-"):
        m = _CLAUDE_VERSION.match(n)
        return f"anthropic/{m.group(1)}-{m.group(2)}.{m.group(3)}" if m else "anthropic/" + n
    return n


def anthropic_key(env: Any = None) -> str:
    """The Anthropic key when Claude goes to Anthropic directly, else "" (no key, or EVE_ANTHROPIC_DIRECT off)."""
    e = os.environ if env is None else env
    if str(e.get(DIRECT_SWITCH, "")).strip().lower() in ("0", "false", "no", "off"):
        return ""
    return (e.get(ANTHROPIC_KEY_ENV) or "").strip()


def anthropic_model(name: str) -> str:
    """Anthropic's own id for a Claude model name: "anthropic/claude-haiku-5.5" or "claude-haiku-5-5" →
    "claude-haiku-5-5"."""
    n = (name or "").strip()
    if n.lower().startswith("anthropic/"):
        n = n.split("/", 1)[1]
    m = re.match(r"^(claude-[a-z]+)-(\d+)\.(\d+)$", n, re.I)
    return f"{m.group(1)}-{m.group(2)}-{m.group(3)}" if m else n


def is_claude(name: str) -> bool:
    return model(name).lower().startswith("anthropic/claude")


def _rewrite(options: Any) -> None:
    """Put the provider's id in a request's body, in place, and send a Claude Chat Completions call to Anthropic when
    its key is set. Idempotent: a retry re-runs it on the same body."""
    data = getattr(options, "json_data", None)
    if not (isinstance(data, dict) and isinstance(data.get("model"), str)):
        return
    direct = anthropic_key()
    url = str(getattr(options, "url", "") or "")
    if direct and is_claude(data["model"]) and (url.endswith("/chat/completions") or url == ANTHROPIC_CHAT_URL):
        body = {k: v for k, v in data.items() if k not in OPENROUTER_ONLY}
        extra = getattr(options, "extra_json", None)       # extra_body: merged into the body after this
        if isinstance(extra, dict):
            options.extra_json = {k: v for k, v in extra.items() if k not in OPENROUTER_ONLY}
        body["model"] = anthropic_model(model(data["model"]))
        options.json_data = body
        options.url = ANTHROPIC_CHAT_URL
        headers = dict(getattr(options, "headers", None) or {})
        headers["Authorization"] = f"Bearer {direct}"
        options.headers = headers
        return
    mapped = model(data["model"])
    if mapped != data["model"]:
        options.json_data = {**data, "model": mapped}


class _Rewrites:
    """Mixed into an SDK client: every request it builds carries the OpenRouter model id."""

    def _build_request(self, options: Any, *, retries_taken: int = 0) -> Any:  # type: ignore[override]
        _rewrite(options)
        return super()._build_request(options, retries_taken=retries_taken)  # type: ignore[misc]


def _key_or_raise(api_key: Optional[str]) -> str:
    k = (api_key or "").strip() or key()
    if not k:
        raise NoKey(f"{KEY_ENV} is not set")
    return k


def _merge_headers(kwargs: dict[str, Any]) -> dict[str, Any]:
    kwargs["default_headers"] = {**HEADERS, **(kwargs.get("default_headers") or {})}
    return kwargs


_classes: dict[tuple[str, int], type] = {}


def _cls(name: str, base: Any) -> Any:
    """The SDK class with the model rewrite mixed in. A stand-in that is not a class (a test's fake SDK module) is
    returned as it is: it has no request builder to rewrite."""
    if not isinstance(base, type):
        return base
    if (name, id(base)) not in _classes:
        _classes[(name, id(base))] = type(name, (_Rewrites, base), {})
    return _classes[(name, id(base))]


def client(api_key: Optional[str] = None, **kwargs: Any) -> Any:
    """A blocking openai SDK client on OpenRouter (``timeout``, ``max_retries`` pass through). Raises NoKey."""
    import openai

    k = _key_or_raise(api_key)
    return _cls("OpenRouterOpenAI", openai.OpenAI)(api_key=k, base_url=BASE_URL, **_merge_headers(kwargs))


def async_client(api_key: Optional[str] = None, **kwargs: Any) -> Any:
    """An asyncio openai SDK client on OpenRouter. Raises NoKey."""
    import openai

    k = _key_or_raise(api_key)
    return _cls("OpenRouterAsyncOpenAI", openai.AsyncOpenAI)(api_key=k, base_url=BASE_URL, **_merge_headers(kwargs))


class _AnthropicIds:
    """Mixed into the anthropic SDK on Anthropic's own API: every request carries Anthropic's model id."""

    def _build_request(self, options: Any, *, retries_taken: int = 0) -> Any:  # type: ignore[override]
        data = getattr(options, "json_data", None)
        if isinstance(data, dict) and isinstance(data.get("model"), str):
            options.json_data = {**data, "model": anthropic_model(data["model"])}
        return super()._build_request(options, retries_taken=retries_taken)  # type: ignore[misc]


def anthropic_client(api_key: Optional[str] = None, **kwargs: Any) -> Any:
    """An asyncio anthropic SDK client: on Anthropic's own API when ``ANTHROPIC_API_KEY`` is set (and api_key is not
    given), else on OpenRouter's Anthropic endpoint. Raises NoKey (and ImportError without the anthropic SDK, the
    ``miniapp`` extra)."""
    import anthropic

    direct = "" if api_key else anthropic_key()
    if direct:
        base = anthropic.AsyncAnthropic
        cls = base if not isinstance(base, type) else _classes.setdefault(
            ("AnthropicDirect", id(base)), type("AnthropicDirect", (_AnthropicIds, base), {}))
        return cls(api_key=direct, **kwargs)
    k = _key_or_raise(api_key)
    return _cls("OpenRouterAsyncAnthropic", anthropic.AsyncAnthropic)(
        api_key=k, base_url=ANTHROPIC_BASE_URL, **_merge_headers(kwargs))
