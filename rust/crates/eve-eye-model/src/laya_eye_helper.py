# The laya-mlx side of eve-eye-model's LiveEyeModel. laya-mlx runs on MLX in Python only, so the Rust model keeps
# this process as its loaded agent: spawning it is LiveEyeModel.__init__, each request is LiveEyeModel.predict.
#
#   python3 -c <this file> <model path> <QUESTION as JSON>
#
# Protocol (one JSON object per line on stdout; nothing else is written there):
#   first line   {"status": "ready"} | {"status": "failed", "kind": str, "error": str}
#   then         stdin {"state": str} -> {"probabilities": [float, ...]} (in QUESTION's order)
#                any failure           -> {"kind": str, "error": str}  (the exception's class name and str())
# The process exits when stdin closes.

import json
import os
import sys

# Libraries print progress and warnings to stdout; the protocol owns the real stdout, everything else goes to stderr.
_out = os.fdopen(os.dup(1), "w", buffering=1)
os.dup2(2, 1)


def send(obj):
    _out.write(json.dumps(obj) + "\n")
    _out.flush()


def failure(exc):
    return {"kind": type(exc).__name__, "error": str(exc)}


class LiveEyeModel:
    def __init__(self, path, question):
        import laya_mlx
        import mlx.core as mx

        self.question = question
        self.agent = laya_mlx.Agent(str(path), dtype="float16", device="gpu", compile=False)
        self.agent.model.scorer.set_dtype(mx.float32)
        mx.eval(self.agent.model.scorer.parameters())

    def probabilities(self, state):
        import numpy as np
        from laya_mlx.agent import collate_items

        items, _ = self.agent.prepare(state, self.question)
        if any(len(item["ids"]) >= self.agent.cfg["max_len"] for item in items):
            raise ValueError("eye context exceeds token budget")
        logits, _ = self.agent.forward(collate_items(items, self.agent.tok.pad_token_id))
        logits = np.asarray(logits).astype(np.float64)
        scores = logits[:, 1] - logits[:, 0]
        if not np.isfinite(scores).all():
            raise ValueError("nonfinite eye scores")
        p = np.exp(scores - scores.max())
        p /= p.sum()
        return p.tolist()


def main():
    path, question = sys.argv[1], json.loads(sys.argv[2])
    try:
        model = LiveEyeModel(path, question)
    except Exception as exc:  # noqa: BLE001 — reported to the Rust side, which raises it as the constructor did
        send({"status": "failed", **failure(exc)})
        return
    send({"status": "ready"})
    for line in sys.stdin:
        if not line.strip():
            continue
        try:
            send({"probabilities": model.probabilities(json.loads(line)["state"])})
        except Exception as exc:  # noqa: BLE001 — one failed turn, the next one is asked again
            send(failure(exc))


main()
