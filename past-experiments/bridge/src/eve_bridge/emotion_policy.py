"""Experimental Laya expression selector. No daemon hook or hardware writes.

The fixed question chooses Eve's response, not the person's hidden emotional state.
Only the existing Laya scorer is trained. The encoder and decision transformer stay
frozen. Calibration and temporal arbitration are independent of animation rendering.
"""

from __future__ import annotations

import hashlib
import json
import math
import threading
import time
from dataclasses import dataclass
from pathlib import Path

import numpy as np

LABELS = ("calm", "happy", "curious", "affection", "surprised", "startled")
QUESTION = {"expression": {
    "type": "choice",
    "instructions": "Which expression should Eve show in response to this situation?",
    "criteria": {
        "calm": "neutral quiet attention",
        "happy": "celebrate good news or success",
        "curious": "interest in learning or exploring",
        "affection": "warmth, comfort, or companionship",
        "surprised": "brief wonder at an unexpected harmless event",
        "startled": "brief alarm at a sudden nearby physical disturbance",
    },
}}
CHIRPS = {"calm": None, "happy": "warble", "curious": "curious", "affection": "warble",
          "surprised": "surprise", "startled": "startle"}
SCHEMA_HASH = hashlib.sha256(json.dumps(QUESTION, sort_keys=True).encode()).hexdigest()


def sha256(path: Path) -> str:
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def probabilities(logits, temperature: float = 1.0) -> np.ndarray:
    z = np.asarray(logits, dtype=np.float64)
    if not math.isfinite(temperature) or temperature <= 0 or not np.isfinite(z).all():
        raise ValueError("nonfinite logits or invalid temperature")
    z = z / temperature
    z -= z.max(axis=-1, keepdims=True)
    p = np.exp(z)
    return p / p.sum(axis=-1, keepdims=True)


def metrics(p, labels) -> dict:
    p, y = np.asarray(p, dtype=float), np.asarray(labels, dtype=int)
    if (p.shape != (len(y), len(LABELS)) or len(y) == 0 or not np.isfinite(p).all()
            or (p < 0).any() or not np.allclose(p.sum(axis=1), 1)
            or (y < 0).any() or (y >= len(LABELS)).any()):
        raise ValueError("invalid probabilities or labels")
    pred = p.argmax(axis=1)
    confusion = np.zeros((len(LABELS), len(LABELS)), dtype=int)
    np.add.at(confusion, (y, pred), 1)
    f1 = [2 * confusion[i, i] / max(1, confusion[i].sum() + confusion[:, i].sum())
          for i in range(len(LABELS))]
    conf, correct = p.max(axis=1), pred == y
    ece = 0.0
    for low, high in zip(np.linspace(0, 1, 11)[:-1], np.linspace(0, 1, 11)[1:], strict=True):
        mask = (conf >= low) & ((conf < high) if high < 1 else (conf <= high))
        if mask.any():
            ece += mask.mean() * abs(conf[mask].mean() - correct[mask].mean())
    return {"n": len(y), "accuracy": float(correct.mean()), "macro_f1": float(np.mean(f1)),
            "nll": float(-np.log(np.maximum(p[np.arange(len(y)), y], 1e-12)).mean()),
            "brier": float(((p - np.eye(len(LABELS))[y]) ** 2).sum(axis=1).mean()),
            "ece_10_bins": float(ece), "confusion": confusion.tolist(),
            "per_class_f1": dict(zip(LABELS, f1, strict=True))}


def fit_temperature(logits, labels) -> float:
    # Log-spaced, predetermined search. Calibration split only; argmax is unchanged.
    return float(min(np.geomspace(0.1, 20, 161),
                     key=lambda t: metrics(probabilities(logits, t), labels)["nll"]))


def fit_threshold(p, labels, *, max_error: float = 0.05, min_accepted: int = 8) -> float:
    p, labels = np.asarray(p), np.asarray(labels)
    metrics(p, labels)  # validate before searching
    for threshold in sorted(set(p.max(axis=1).tolist())):
        accepted = p.max(axis=1) >= threshold
        if accepted.sum() >= min_accepted and (p.argmax(axis=1)[accepted] != labels[accepted]).mean() <= max_error:
            return float(threshold)
    return 1.01  # explicit abstain-all; never fabricate evidence by lowering precision


def selection_metrics(p, labels, threshold: float) -> dict:
    p, labels = np.asarray(p), np.asarray(labels)
    metrics(p, labels)
    selected = p.max(axis=1) >= threshold
    count = int(selected.sum())
    wrong = int((p.argmax(axis=1)[selected] != labels[selected]).sum())
    return {"accepted": count, "wrong": wrong, "coverage": count / len(labels),
            "precision": (count - wrong) / count if count else None}


def load_dataset(path: Path) -> list[dict]:
    data = json.loads(path.read_text())
    if data.get("provenance") != "assistant-authored synthetic scenarios; no human validation":
        raise ValueError("dataset provenance missing")
    rows = data["rows"]
    ids, texts, families = set(), set(), {}
    counts = {(split, label): 0 for split in ("train", "dev", "calibration", "test") for label in LABELS}
    for row in rows:
        key = (row["split"], row["label"])
        text = " ".join(row["text"].casefold().split())
        if key not in counts or not text or row["id"] in ids or text in texts:
            raise ValueError("invalid label, split, empty text, or duplicate")
        family = row["family"]
        if family in families and families[family] != row["split"]:
            raise ValueError("scenario family leaks across splits")
        ids.add(row["id"])
        texts.add(text)
        families[family] = row["split"]
        counts[key] += 1
    if not all(counts.values()):
        raise ValueError("each split must contain each expression")
    return rows


@dataclass(frozen=True)
class Expression:
    label: str
    chirp: str | None
    reason: str


class ExpressionController:
    """Pure proposal arbitration; no serial commands. Times use monotonic seconds.

    Event id must represent a new semantic event, not each transcript fragment.
    Calm is also the fallback; it is not evidence the model recognized neutrality.
    """

    def __init__(self, threshold: float, *, dwell_s: float = 1.2, ttl_s: float = 4.0,
                 chirp_cooldown_s: float = 8.0):
        if not (math.isfinite(threshold) and 0 <= threshold <= 1.01):
            raise ValueError("invalid threshold")
        if not all(math.isfinite(x) and x > 0 for x in (dwell_s, ttl_s, chirp_cooldown_s)):
            raise ValueError("timings must be finite and positive")
        self.threshold, self.dwell, self.ttl, self.cooldown = threshold, dwell_s, ttl_s, chirp_cooldown_s
        self.label = "calm"
        self.changed = self.last_chirp = -math.inf
        self.last_event_at = -math.inf
        self.event_id = None

    def update(self, p, *, event_id: str, event_at: float, now: float, phase: str = "idle",
               muted: bool = False, fresh_physical_event: bool = False) -> Expression:
        if not (math.isfinite(now) and math.isfinite(event_at)):
            raise ValueError("timestamps must be finite")
        if now - self.last_event_at > self.ttl:
            self.label = "calm"
        # Agent phases remain authoritative until a real firmware overlay exists.
        if phase != "idle":
            self.label = "calm"
            return Expression("calm", None, "phase owns expression")
        if event_at > now or now - event_at > self.ttl:
            return Expression(self.label, None, "stale or future event")
        if event_id == self.event_id or event_at <= self.last_event_at:
            return Expression(self.label, None, "duplicate or out-of-order event")
        try:
            arr = np.asarray(p, dtype=float)
            metrics(arr[None, :], [0])
        except (ValueError, TypeError, IndexError):
            return Expression(self.label, None, "invalid model output")
        self.event_id, self.last_event_at = event_id, event_at
        proposal = LABELS[int(arr.argmax())] if arr.max() >= self.threshold else "calm"
        if proposal == "startled" and not fresh_physical_event:
            proposal = "calm"  # dialogue about a bang is not a new physical reflex
        if proposal == self.label or now - self.changed < self.dwell:
            return Expression(self.label, None, "held")
        self.label, self.changed = proposal, now
        chirp = CHIRPS[proposal]
        if muted or now - self.last_chirp < self.cooldown:
            chirp = None
        if chirp is not None:
            self.last_chirp = now
        return Expression(proposal, chirp, "new expression")


class LayaExpressionModel:
    """Local research model. Base weights are read-only; scorer is a separate artifact."""

    def __init__(self, base: Path, artifact: Path | None = None):
        import laya_mlx
        import mlx.core as mx

        self.lock = threading.Lock()
        self.agent = laya_mlx.Agent(str(base.expanduser()), dtype="float16", device="gpu", compile=False)
        self.temperature, self.threshold = 1.0, 1.01
        if artifact:
            manifest = json.loads((artifact / "manifest.json").read_text())
            if manifest["schema_sha256"] != SCHEMA_HASH:
                raise ValueError("expression schema mismatch")
            if manifest["base_sha256"] != sha256(base.expanduser() / "model.safetensors"):
                raise ValueError("base checkpoint mismatch")
            if manifest["scorer_sha256"] != sha256(artifact / "scorer.safetensors"):
                raise ValueError("scorer checksum mismatch")
            self.agent.model.scorer.load_weights(str(artifact / "scorer.safetensors"))
            self.temperature, self.threshold = manifest["temperature"], manifest["threshold"]
        # Same float32 scorer arithmetic is used during training, cached eval, and live inference.
        self.agent.model.scorer.set_dtype(mx.float32)
        mx.eval(self.agent.model.scorer.parameters())

    def features(self, text: str):
        """Option marker features after the frozen encoder + decision transformer."""
        import mlx.core as mx
        from laya_mlx.agent import collate_items

        if not isinstance(text, str) or not text.strip():
            raise ValueError("nonempty text required")
        items, _ = self.agent.prepare(text, QUESTION)
        if len(items[0]["ids"]) >= self.agent.cfg["max_len"]:
            raise ValueError("context exceeds expression token budget")
        batch = {k: mx.array(v) for k, v in collate_items(items, self.agent.tok.pad_token_id).items()}
        model = self.agent.model
        h = model.encoder(batch["input_ids"], batch["attention_mask"])
        h = h + model.type_emb(batch["qtype"])[:, None, :]
        h = model.head(h, batch["attention_mask"][:, None, None, :].astype(mx.bool_))
        features = h[mx.arange(h.shape[0])[:, None], batch["marker_pos"]].astype(mx.float32)
        mx.eval(features)
        return features

    def predict(self, text: str) -> dict:
        import mlx.core as mx

        with self.lock:
            t0 = time.perf_counter()
            logits = self.agent.model.scorer(self.features(text)).squeeze(-1)
            mx.eval(logits)
            p = probabilities(np.asarray(logits)[0], self.temperature)
            return {"label": LABELS[int(p.argmax())], "probabilities": p.tolist(),
                    "ms": (time.perf_counter() - t0) * 1000}
