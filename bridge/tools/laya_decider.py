"""The local Laya checkpoint behind decider.Decider, for the routing evals' `--model laya` arms.

tools/head_eval.py and tools/route_eval.py score laya against Jev and the rules on the head-move
and request-routing sets. The daemon never loads Laya through a Decider: the computer-use click
lane's Laya backend is gone, and Laya's live job is the eye expressions (live_expressions.py,
which loads the same checkpoint its own way). Needs the bridge's `[laya]` extra (laya-mlx,
Apple silicon) and the checkpoint at live_expressions.DEFAULT_MODEL.
"""

from __future__ import annotations

import os
import sys
import time
from pathlib import Path
from typing import Optional

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "src"))

from eve_bridge.decider import WARMUP_CONTEXT, WARMUP_OBJECTIVE, WARMUP_OPTIONS, Decider  # noqa: E402

CHECKPOINT_FILES = ("model.safetensors", "rl_agent_config.json", "encoder/config.json", "tokenizer/tokenizer.json")


def model_path() -> str:
    """The checkpoint directory the eye expressions use (live_expressions.DEFAULT_MODEL)."""
    from eve_bridge.live_expressions import DEFAULT_MODEL

    return str(DEFAULT_MODEL)


def load(path: Optional[str] = None, *, style: str = "compact") -> Decider:
    """Import laya_mlx, build the Agent, warm it up once. Raises RuntimeError with one line."""
    t0 = time.perf_counter()
    path = os.path.expanduser(path or model_path())
    if not os.path.isdir(path):
        raise RuntimeError(f"laya checkpoint directory not found: {path}")
    missing = [name for name in CHECKPOINT_FILES if not os.path.isfile(os.path.join(path, name))]
    if missing:
        raise RuntimeError(f"laya checkpoint at {path} is incomplete: missing {', '.join(missing)}")
    try:
        import laya_mlx
    except Exception as e:  # noqa: BLE001 — any import failure (mlx, tokenizers, numpy) is one reason
        raise RuntimeError(f"laya_mlx is not importable ({type(e).__name__}: {e}); "
                           "install the bridge's [laya] extra") from None
    try:
        agent = laya_mlx.Agent(path, dtype="float16", device="gpu", batch_size=1, compile=True, cache_prompts=False)
    except Exception as e:  # noqa: BLE001 — a bad checkpoint, a missing Metal device: one line each
        raise RuntimeError(f"laya checkpoint failed to load from {path}: {type(e).__name__}: {e}") from None
    tok = agent.tok

    def tokenize(text: str) -> int:
        return len(tok(text, add_special_tokens=False)["input_ids"])

    cfg = getattr(agent, "cfg", {}) or {}
    decider = Decider(agent.predict, tokenize=tokenize, max_len=int(cfg.get("max_len", 512)),
                      head_max_len=int(cfg.get("head_max_len", 192)), style=style)
    decider.load_ms = (time.perf_counter() - t0) * 1000.0
    t1 = time.perf_counter()
    warm = decider.choose(WARMUP_OBJECTIVE, app="Calendar", context=WARMUP_CONTEXT, options=WARMUP_OPTIONS)
    decider.warm_ms = (time.perf_counter() - t1) * 1000.0
    if warm.error:
        raise RuntimeError(f"laya warm-up predict failed: {warm.error}")
    decider.available = True
    return decider
