# The laya-mlx side of eve-browser-intent's LayaKindEngine. laya-mlx runs on MLX in Python only, so the Rust
# engine keeps this process as its loaded model: spawning it is `_load_locked`, killing it is `_drop_locked`.
#
#   python3 -c <this file> serve  <model> <revision> <subfolder> <cache limit bytes>
#   python3 -c <this file> locate <model> <revision> <subfolder>
#
# Protocol (one JSON object per line on stdout; nothing else is written there):
#   first line   {"status": "ready"} | {"status": "unavailable", "error": str} | {"status": "not_cached"}
#                | {"status": "load_failed", "error": str} | (locate) {"status": "cached", "path": str}
#   then, serve  stdin {"op": "fits", "question": {...}}            -> {"fits": bool}
#                stdin {"op": "predict", "state": {...}, "questions": {...}} -> {"result": <predict's return>}
#                any failure                                         -> {"error": str}
# The process exits when stdin closes.

import json
import math
import os
import sys

# Libraries print progress and warnings to stdout; the protocol owns the real stdout, everything else goes to stderr.
_out = os.fdopen(os.dup(1), "w", buffering=1)
os.dup2(2, 1)


def _plain(o):
    # JSON-native values pass as they are. Anything else (a numpy scalar, a non-finite float) is sent as its repr, so
    # the Rust schema check rejects it the way browser_intent.validate_choice's `type(v) in {int, float}` and
    # `math.isfinite` checks do.
    if o is None or isinstance(o, (bool, str)):
        return o
    if type(o) is int:
        return o
    if type(o) is float:
        return o if math.isfinite(o) else repr(o)
    if isinstance(o, dict):
        return {(k if isinstance(k, str) else repr(k)): _plain(v) for k, v in o.items()}
    if isinstance(o, (list, tuple)):
        return [_plain(v) for v in o]
    return repr(o)


def send(obj):
    _out.write(json.dumps(_plain(obj)) + "\n")
    _out.flush()


def cached_checkpoint(model, revision, subfolder):
    """The pinned checkpoint's local snapshot, found without the network; None when it is not downloaded."""
    try:
        from huggingface_hub import snapshot_download

        # Only the v10s files: the repository's other checkpoints and assets are never downloaded, and without the
        # pattern huggingface_hub calls a v10s-only cache incomplete.
        return snapshot_download(model, revision=revision, local_files_only=True,
                                 allow_patterns=[f"{subfolder}/*"])
    except Exception:  # noqa: BLE001 — not cached, or no huggingface_hub: abstain
        return None


def main():
    mode, model, revision, subfolder = sys.argv[1:5]
    if mode == "locate":
        path = cached_checkpoint(model, revision, subfolder)
        send({"status": "cached", "path": path} if path else {"status": "not_cached"})
        return
    cache_limit = int(sys.argv[5])
    try:
        import laya_mlx
        import mlx.core as mx
    except Exception as exc:  # noqa: BLE001 — laya-mlx not installed: abstain, and keep abstaining
        send({"status": "unavailable", "error": str(exc)})
        return
    path = cached_checkpoint(model, revision, subfolder)
    if not path:
        # Never download from inside a request; the checkpoint is fetched once, by hand.
        send({"status": "not_cached"})
        return
    try:
        mx.set_cache_limit(cache_limit)
        # No cache_prompts: laya-mlx keys its prompt cache on the whole instruction, and the instruction holds
        # the request itself, so a new request never hits it.
        agent = laya_mlx.load(path, subfolder=subfolder, revision=revision, dtype="float16")
    except Exception as exc:  # noqa: BLE001 — a checkpoint that will not load abstains
        send({"status": "load_failed", "error": str(exc)})
        return
    send({"status": "ready"})

    def prefix_fits(question):
        """The question and its options fit the checkpoint's head budget (LayaBrowse goal_engine._prefix_fits):
        refuse rather than let laya-mlx truncate silently."""
        from laya_mlx.common import render_options

        q = agent._to_internal(question)
        tok = agent.tok

        def length(s):
            return len(tok(s, add_special_tokens=False)["input_ids"])
        options = [length(" " + o.replace(tok.mask_token, " ")) for o in render_options(q)]
        instruction = length(f"{q['t']} question: {q['ins']}".replace(tok.mask_token, " "))
        return (all(n <= 48 for n in options)
                and sum(n + 1 for n in options) + max(16, instruction) <= agent.cfg["head_max_len"])

    for line in sys.stdin:
        if not line.strip():
            continue
        try:
            request = json.loads(line)
            if request.get("op") == "fits":
                send({"fits": bool(prefix_fits(request["question"]))})
            elif request.get("op") == "predict":
                send({"result": agent.predict(request["state"], request["questions"])})
            else:
                send({"error": f"unknown op {request.get('op')!r}"})
        except Exception as exc:  # noqa: BLE001 — reported to the engine, which abstains
            send({"error": str(exc)})


main()
