"""Conversation-aware eye choice: Jev picks one of eleven eye labels (default), or local Laya does.

Two models, one contract — ``predict(state) -> {"label", "probabilities" (in LABELS order), "ms"}``:

* ``JevEyeModel`` (owner, 2026-09-24: "use jev for that too"): TypeSafe's Jev, asked ONE ``choice`` over the
  eleven labels — its native shape (it ranks options; ask each model in the shape it is built for). Measured
  on this Mac 2026-09-24 (tools/jev_eyes_eval.py, the 84 hand-written cases in tests/fixtures/emotion, both
  pickers asked THIS eleven-label question and folded onto the sets' six labels the same way): Jev 62.5% on
  the 48 scenario tests and 72.2% on the 36 confirmation cases, against 47.9% and 52.8% for untouched Laya;
  200 ms p50 and about $0.00002 a call. 14 of Jev's 32 misses are the sets' "startled" read as
  "surprised", which is the fold, not the eye. Jev is a network call: the state is the last turn or two of
  conversation, and it leaves the Mac only while the owner has expressions enabled.
* ``LiveEyeModel`` (``EVE_EXPRESSION_BACKEND=laya``): the original local Laya weights, eleven binary
  questions. No weights are trained here. Kept for a Mac without network or without a Jev key.
"""

from __future__ import annotations

import math
import os
import time
from typing import Any, Callable, Mapping, Optional

CRITERIA = {
    "calm": "neutral facts, routine questions or instructions",
    "happy": "pleased, content or gently celebrating",
    "curious": "wondering, exploring or wanting to learn",
    "affection": "love, gratitude, friendship or reassurance",
    "surprised": "shocked or amazed by something unexpected",
    "sad": "sadness, grief, loss or disappointment",
    "worried": "fear, anxiety, concern or danger",
    "skeptical": "doubt, disbelief or questioning a claim",
    "frustrated": "annoyed or fed up with a problem",
    "excited": "very enthusiastic, thrilled or eagerly anticipating",
    "wink": "playful teasing, a joke or asking for a wink",
}
LABELS = tuple(CRITERIA)
QUESTION = {
    label: {
        "type": "noul",
        "instructions": f"Does the last message express {description}?",
        "criteria": {"false": "No", "true": "Yes"},
    }
    for label, description in CRITERIA.items()
}


EYE_CHOICE = {
    "eye": {
        "type": "choice",
        "instructions": "Which expression fits Eve's eyes for the last message?",
        "criteria": dict(CRITERIA),
    }
}
WARM_STATE = "The room is quiet and nothing has changed."


class JevEyeModel:
    """Jev picks the eye label: one ``choice`` question over CRITERIA, the conversation as the state.

    ``env`` (default ``os.environ``) supplies the route and key (jev.route_config: EVE_JEV_ROUTE and its
    key); ``opener`` is jev.make_predict's socket seam for tests. Raises jev.JevError at construction when no
    key is set, so the worker reports "no Jev key" instead of failing on every turn."""

    def __init__(self, env: Optional[Mapping[str, str]] = None, *, opener: Optional[Callable[..., Any]] = None,
                 timeout_s: float = 0.0) -> None:
        from . import jev

        source = os.environ if env is None else env
        url, key, self.model = jev.route_config(source)
        self._predict = jev.make_predict(url, key, self.model, timeout_s=timeout_s or jev.timeout_from_env(source),
                                         opener=opener)

    def predict(self, state: str) -> dict[str, Any]:
        start = time.perf_counter()
        body = self._predict(state, EYE_CHOICE)
        answer = (body.get("answers") or {}).get("eye") if isinstance(body, dict) else None
        probs = answer.get("probabilities") if isinstance(answer, dict) else None
        if not isinstance(probs, dict):
            raise ValueError("jev returned no eye probabilities")
        p = [float(probs.get(label) or 0.0) for label in LABELS]
        total = sum(p)
        if not math.isfinite(total) or total <= 0 or any(x < 0 for x in p):
            raise ValueError("jev returned unusable eye probabilities")
        p = [x / total for x in p]                         # LiveExpressions.label wants a distribution over LABELS
        return {
            "label": LABELS[max(range(len(p)), key=p.__getitem__)],
            "probabilities": p,
            "ms": (time.perf_counter() - start) * 1000,
        }


class ConversationContext:
    def __init__(self):
        self.turns = {}

    def state(self, who, text, now):
        # Bounded chronological context: previous turn first, current speaker last.
        speaker = "Eve" if who == "assistant" else "Eve's observation" if who == "diary" else "User"
        parts = []
        for role, (at, previous) in self.turns.items():
            if now - at <= 90 and role != who:
                parts.append(f"{'User' if role == 'user' else 'Eve'}: {previous[-400:]}")
        parts.append(f"{speaker}: {text}")
        if who in ("user", "assistant"):
            self.turns[who] = (now, text)
        return "\n".join(parts)


class LiveEyeModel:
    def __init__(self, path):
        import laya_mlx
        import mlx.core as mx

        self.agent = laya_mlx.Agent(str(path), dtype="float16", device="gpu", compile=False)
        self.agent.model.scorer.set_dtype(mx.float32)
        mx.eval(self.agent.model.scorer.parameters())

    def predict(self, state):
        import numpy as np
        from laya_mlx.agent import collate_items

        start = time.perf_counter()
        items, _ = self.agent.prepare(state, QUESTION)
        if any(len(item["ids"]) >= self.agent.cfg["max_len"] for item in items):
            raise ValueError("eye context exceeds token budget")
        logits, _ = self.agent.forward(collate_items(items, self.agent.tok.pad_token_id))
        logits = np.asarray(logits).astype(np.float64)
        scores = logits[:, 1] - logits[:, 0]
        if not np.isfinite(scores).all():
            raise ValueError("nonfinite eye scores")
        p = np.exp(scores - scores.max())
        p /= p.sum()
        return {
            "label": LABELS[int(p.argmax())],
            "probabilities": p.tolist(),
            "ms": (time.perf_counter() - start) * 1000,
        }
