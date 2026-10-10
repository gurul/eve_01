"""The EmbeddingGemma recall eval (BAR.md). One arm per process, so its peak RAM is its own:

    uv run --extra mem0 --extra gemma python tools/evals/embeddinggemma/run_eval.py gemma
    OPENROUTER_API_KEY=... uv run --extra mem0 python tools/evals/embeddinggemma/run_eval.py openrouter

Each arm's embedder is built the way mem0_memory.py builds it (mem0's EmbedderFactory on mem0_config's embedder
section, then on_openrouter's client or on_gemma's prompts), not through the daemon and with no store. The 40
snippets are embedded as stored memories ("add"), each question as a search ("search"); rank = cosine over all
40. Writes results-<arm>.json beside this file. Nothing here touches ~/.config/eve-bridge.
"""

from __future__ import annotations

import json
import math
import os
import resource
import statistics
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
WARMUP = "what time is it"


def load(name: str) -> list[dict]:
    return [json.loads(line) for line in (HERE / name).read_text(encoding="utf-8").splitlines() if line.strip()]


def build(arm: str):
    os.environ["MEM0_TELEMETRY"] = "False"
    from mem0.utils.factory import EmbedderFactory

    from eve_bridge import mem0_memory, openrouter
    from eve_bridge.mem0_memory import Mem0Config, mem0_config

    home = Path("/nonexistent-eval-home")        # mem0_config only names paths; nothing is opened
    cfg = Mem0Config(enabled=True, home=home, embedder=arm,
                     embed_model=mem0_memory.GEMMA_MODEL if arm == "gemma" else mem0_memory.DEFAULT_EMBED_MODEL)
    section = mem0_config(cfg)["embedder"]
    t0 = time.perf_counter()
    emb = EmbedderFactory.create(section["provider"], section["config"], None)
    if arm == "gemma":
        mem0_memory.on_gemma(emb)
    else:
        emb.client = openrouter.client()
    return emb, time.perf_counter() - t0


def cos(a: list[float], b: list[float]) -> float:
    dot = sum(x * y for x, y in zip(a, b, strict=True))
    return dot / (math.sqrt(sum(x * x for x in a)) * math.sqrt(sum(y * y for y in b)) or 1.0)


def pct(xs: list[float], p: float) -> float:
    s = sorted(xs)
    k = (len(s) - 1) * p
    lo, hi = math.floor(k), math.ceil(k)
    return s[lo] + (s[hi] - s[lo]) * (k - lo)


def main(arm: str) -> None:
    corpus, questions = load("corpus.jsonl"), load("questions.jsonl")
    emb, build_s = build(arm)
    t0 = time.perf_counter()
    emb.embed(WARMUP, "search")                   # first call: a cold model's first pass, the network's first hop
    first_s = time.perf_counter() - t0
    t0 = time.perf_counter()
    docs = emb.embed_batch([c["text"] for c in corpus], "add")
    corpus_s = time.perf_counter() - t0
    lat, hits1, hits5, rr, rows = [], 0, 0, 0.0, []
    for q in questions:
        t0 = time.perf_counter()
        qv = emb.embed(q["question"], "search")
        lat.append(time.perf_counter() - t0)
        ranked = sorted(range(len(corpus)), key=lambda i: -cos(qv, docs[i]))
        ids = [corpus[i]["id"] for i in ranked]
        first = min(ids.index(a) for a in q["answers"]) + 1
        hits1 += first == 1
        hits5 += first <= 5
        rr += 1.0 / first
        rows.append({"id": q["id"], "rank": first, "top5": ids[:5]})
    rss = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss          # bytes on macOS
    out = {
        "arm": arm, "dims": len(docs[0]), "questions": len(questions),
        "recall_at_5": hits5, "recall_at_1": hits1, "mrr": round(rr / len(questions), 4),
        "query_ms_median": round(statistics.median(lat) * 1000, 1), "query_ms_p95": round(pct(lat, 0.95) * 1000, 1),
        "build_s": round(build_s, 2), "first_call_s": round(first_s, 2), "corpus_embed_s": round(corpus_s, 2),
        "peak_rss_mb": round(rss / (1 << 20) if sys.platform == "darwin" else rss / 1024, 0),
        "device": str(getattr(getattr(emb, "model", None), "device", "remote")),
        "per_question": rows,
    }
    (HERE / f"results-{arm}.json").write_text(json.dumps(out, indent=1) + "\n", encoding="utf-8")
    print(json.dumps({k: v for k, v in out.items() if k != "per_question"}))


if __name__ == "__main__":
    if len(sys.argv) != 2 or sys.argv[1] not in ("openrouter", "gemma"):
        sys.exit("usage: run_eval.py openrouter|gemma")
    main(sys.argv[1])
