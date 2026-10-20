# The mem0 side of eve-mem0-memory's PythonMem0. mem0 (mem0ai) is a Python library whose store is Qdrant's local
# mode and a SQLite history database, both written in Python's own formats, so the Rust index keeps this process as
# its opened Memory: spawning it is open_mem0, each request one call on the Memory.
#
#   python3 -c <this file> <init as JSON>
#
# init: {"config": the Memory.from_config dict (mem0_config, built in Rust), "home": str, "mem0_dir": str,
#        "embedder": "openrouter" | "gemma", "key_env": str, "base_url": str, "headers": {str: str},
#        "models": {name: OpenRouter id}, "extract_max_output_tokens": int}
#
# Protocol (one JSON object per line on stdout; nothing else is written there):
#   first line   {"status": "ready"} | {"status": "failed", "kind": str, "error": str}
#   then         stdin {"op": ..., ...} -> {"result": ...} | {"kind": str, "error": str}  (class name and str())
#                ops: search, get_all, add, delete, embed; generate, probe and unflag_reasoning for the tests
#   any time     {"log": "warning", "message": str}          a line for the daemon's log
#                {"spend": "chat" | "embedding", "model": str, "reply": {...}}   one model call, for the spend meter
# The process exits when stdin closes.

import json
import os
import sys

_out = None


def send(obj):
    _out.write(json.dumps(obj, default=str) + "\n")
    _out.flush()


def failure(exc):
    return {"kind": type(exc).__name__, "error": str(exc)}


def warn(message):
    send({"log": "warning", "message": message})


class NoKey(RuntimeError):
    """OPENROUTER_API_KEY is not set: no model call can be made."""


def _as_dict(obj):
    """A reply as a dict: a dict as is, an SDK object through model_dump, else {} (spend._as_dict)."""
    if isinstance(obj, dict):
        return obj
    dump = getattr(obj, "model_dump", None)
    if callable(dump):
        try:
            out = dump(exclude_none=True)
            return out if isinstance(out, dict) else {}
        except Exception:  # noqa: BLE001
            return {}
    return {}


def _usage_of(reply):
    """A reply's ``usage`` as a dict, without dumping the whole reply (an embeddings reply is mostly floats)."""
    usage = reply.get("usage") if isinstance(reply, dict) else getattr(reply, "usage", None)
    return usage if isinstance(usage, dict) else _as_dict(usage)


def spent(kind, reply, model):
    """One model call, sent to the Rust side's spend meter. Never raises: the index must never break for a meter."""
    try:
        payload = _as_dict(reply) if kind == "chat" else {"usage": _usage_of(reply)}
        send({"spend": kind, "model": model, "reply": payload})
    except Exception as e:  # noqa: BLE001
        warn("spend: could not read a reply's usage (%s)" % type(e).__name__)


def openrouter_client(init):
    """openrouter.client(): a blocking openai SDK client on OpenRouter, every request carrying the OpenRouter model
    id (the names mem0 sends are mapped by the Rust side's openrouter.model). Raises NoKey."""
    import openai

    key = (os.environ.get(init["key_env"]) or "").strip()
    if not key:
        raise NoKey("%s is not set" % init["key_env"])
    models = init["models"]

    class OpenRouterOpenAI(openai.OpenAI):
        def _build_request(self, options, *, retries_taken=0):
            data = getattr(options, "json_data", None)
            if isinstance(data, dict) and isinstance(data.get("model"), str):
                mapped = models.get(data["model"], data["model"])
                if mapped != data["model"]:
                    options.json_data = {**data, "model": mapped}
            return super()._build_request(options, retries_taken=retries_taken)

    return OpenRouterOpenAI(api_key=key, base_url=init["base_url"], default_headers=dict(init["headers"]))


def open_mem0(init):
    """The real mem0 Memory, with telemetry off and its home inside ours. Raises when that cannot be had."""
    os.makedirs(init["home"], exist_ok=True)
    os.environ["MEM0_TELEMETRY"] = "False"          # read once, at import: must be set before it
    os.environ["MEM0_DIR"] = init["mem0_dir"]        # where mem0 keeps its config.json, instead of ~/.mem0
    from mem0 import Memory
    from mem0.memory import telemetry

    if telemetry.MEM0_TELEMETRY:
        raise RuntimeError("mem0 telemetry is on (mem0 was imported before eve could turn it off)")
    if init["embedder"] == "gemma":
        os.environ.setdefault("HF_HUB_DISABLE_TELEMETRY", "1")  # the model itself: local_files_only, never fetched
        return meter(on_gemma(on_openrouter(Memory.from_config(init["config"]), init, embeddings=False)), init)
    return meter(on_openrouter(Memory.from_config(init["config"]), init), init)


def on_openrouter(memory, init, embeddings=True):
    """mem0's two clients, fact extraction (``memory.llm.client``) and embeddings (``memory.embedding_model.client``),
    replaced with OpenRouter's. mem0ai 2.2 checks OPENROUTER_API_KEY in the environment on each extraction call and
    then sends no ``store`` field, which is what OpenRouter wants. Raises without a key. ``embeddings=False`` (the
    gemma embedder) moves extraction only: the embeddings stay on this Mac."""
    limit = init["extract_max_output_tokens"]
    try:
        memory.llm.client = openrouter_client(init)
        # mem0 2.2 drops its configured output limit for reasoning models. Restore the
        # extraction budget on this instance, before the spend wrapper observes calls.
        completions = memory.llm.client.chat.completions
        create = completions.create

        def bounded_create(*args, **kwargs):
            if "max_completion_tokens" not in kwargs and "max_tokens" not in kwargs:
                kwargs["max_completion_tokens"] = limit
            return create(*args, **kwargs)

        completions.create = bounded_create
        if embeddings:
            memory.embedding_model.client = openrouter_client(init)
    except AttributeError as e:
        raise RuntimeError("mem0: its model clients cannot be moved to OpenRouter") from e
    return memory


GEMMA_QUERY = "query"                          # its prompts: "task: search result | query: "
GEMMA_DOCUMENT = "document"                    # and "title: none | text: "


def on_gemma(embedder):
    """EmbeddingGemma with the task prompts its card asks for. mem0 2.2's huggingface provider encodes with no
    prompt; here a search (``memory_action="search"``) is encoded as a query and everything mem0 stores (``add``,
    ``update``) as a document, vectors normalised. Takes the Memory, or mem0's embedder itself."""
    emb = getattr(embedder, "embedding_model", embedder)
    model = emb.model

    def encode(texts, memory_action):
        prompt = GEMMA_QUERY if memory_action == "search" else GEMMA_DOCUMENT
        return model.encode(texts, prompt_name=prompt, normalize_embeddings=True, convert_to_numpy=True)

    def embed(text, memory_action=None):
        return encode(text, memory_action).tolist()

    def embed_batch(texts, memory_action="add"):
        return encode(list(texts), memory_action).tolist() if texts else []

    emb.embed = embed
    emb.embed_batch = embed_batch
    return embedder


def meter(memory, init):
    """Route mem0's two model calls through the daily spend meter. mem0 owns its clients, so the ``create`` of its
    fact extraction (``memory.llm.client``) and of its embeddings (``memory.embedding_model.client``) is wrapped on
    those instances: each reply is sent to the meter, then returned unchanged. A client shaped otherwise (another
    mem0 version) is left as it is, with one log line: the index must never break for want of a meter."""
    def wrap(owner, kind, what):
        original = getattr(owner, "create", None)
        if not callable(original) or getattr(original, "_eve_metered", False):
            warn("mem0: %s calls are not metered (no create to wrap)" % what)
            return

        def create(*args, **kwargs):
            reply = original(*args, **kwargs)
            spent(kind, reply, str(kwargs.get("model") or ""))
            return reply

        create._eve_metered = True
        owner.create = create

    try:
        wrap(memory.llm.client.chat.completions, "chat", "extraction")
        if init["embedder"] != "gemma":                # local embeddings cost nothing and have no client
            wrap(memory.embedding_model.client.embeddings, "embedding", "embedding")
    except AttributeError:
        warn("mem0: its model calls are not metered (this mem0 has no client where eve looks)")
    return memory


def handle(mem, req):
    op = req.get("op")
    if op == "search":
        return mem.search(req["query"], filters=req["filters"], top_k=req["top_k"])
    if op == "get_all":
        return mem.get_all(filters=req["filters"], top_k=req["top_k"])
    if op == "add":
        return mem.add(req["messages"], user_id=req["user_id"], metadata=req["metadata"])
    if op == "delete":
        return mem.delete(req["memory_id"])
    if op == "embed":
        embed = getattr(getattr(mem, "embedding_model", None), "embed", None)
        if embed is None:
            return {"embedded": False}
        embed(req["text"], req.get("memory_action"))
        return {"embedded": True}
    if op == "generate":
        return mem.llm.generate_response(messages=req["messages"])
    if op == "unflag_reasoning":                    # tests: the positive control for the sampling settings
        mem.llm.config.is_reasoning_model = False
        return None
    if op == "probe":
        from mem0.memory import telemetry

        return {"telemetry": bool(telemetry.MEM0_TELEMETRY), "mem0_dir": os.environ.get("MEM0_DIR"),
                "custom_instructions": getattr(mem, "custom_instructions", None),
                "llm_base_url": str(getattr(getattr(mem.llm, "client", None), "base_url", "")),
                "embedder_base_url": str(getattr(getattr(mem.embedding_model, "client", None), "base_url", ""))}
    raise ValueError("unknown op %r" % (op,))


def main():
    global _out
    # Libraries print progress and warnings to stdout; the protocol owns the real stdout, all else goes to stderr.
    _out = os.fdopen(os.dup(1), "w", buffering=1, encoding="utf-8")
    os.dup2(2, 1)
    init = json.loads(sys.argv[1])
    try:
        mem = open_mem0(init)
    except Exception as exc:  # noqa: BLE001 — reported to the Rust side, which logs it as open_mem0's raise
        send({"status": "failed", **failure(exc)})
        return
    send({"status": "ready"})
    # Requests are UTF-8 whatever the locale (the owner's words are not ASCII; a launchd job has no LANG).
    for raw in sys.stdin.buffer:
        line = raw.decode("utf-8")
        if not line.strip():
            continue
        try:
            send({"result": handle(mem, json.loads(line))})
        except Exception as exc:  # noqa: BLE001 — one failed call; the caller decides what it means
            send(failure(exc))


if __name__ == "__main__":
    main()
