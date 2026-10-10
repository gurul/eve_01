#!/usr/bin/env python3
"""Reproducible local Laya scorer tuning; no cloud calls or robot commands.

Run from the repository root. Split assignment precedes every model run. Dev selects
hyperparameters; calibration fits temperature and abstention; test is read for scoring
only after the selected artifact is sealed. Synthetic scenarios are diagnostic only.
"""

from __future__ import annotations

import argparse
import copy
import importlib.metadata
import json
import platform
import re
import sys
import time
from collections import Counter
from datetime import datetime, timezone
from pathlib import Path

import numpy as np

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "bridge/src"))

from eve_bridge.emotion_policy import (  # noqa: E402
    LABELS,
    SCHEMA_HASH,
    LayaExpressionModel,
    fit_temperature,
    fit_threshold,
    load_dataset,
    metrics,
    probabilities,
    selection_metrics,
    sha256,
)

DATASET = ROOT / "bridge/tests/fixtures/emotion/scenarios.json"
BASE = Path("~/.config/eve-bridge/models/laya-multilingual-mlx").expanduser()
REPORT = ROOT / "docs/stackchan/laya-emotion/results.json"
ARTIFACT = ROOT / ".test-artifacts/laya-emotion"
# Frozen before the first run; no test-driven retries or prompt search.
GRID = [(lr, wd) for lr in (1e-5, 5e-5, 2e-4) for wd in (0.01, 0.1)]
EPOCHS, BATCH_SIZE, SEED = 24, 16, 20260921
CODE = [Path(__file__), ROOT / "bridge/src/eve_bridge/emotion_policy.py"]


def write_json(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2, allow_nan=False) + "\n")


def tfidf_predictions(train, test):
    """Small data baseline: unigram TF-IDF cosine similarity to training class centroids."""
    def tokens(text):
        return re.findall(r"[a-z]{2,}", text.casefold())

    document_counts = Counter(word for row in train for word in set(tokens(row["text"])))
    vocab = {word: i for i, word in enumerate(sorted(document_counts))}
    idf = np.array([np.log((len(train) + 1) / (document_counts[w] + 1)) + 1 for w in vocab])

    def vector(row):
        v = np.zeros(len(vocab))
        for word, count in Counter(tokens(row["text"])).items():
            if word in vocab:
                v[vocab[word]] = count
        v *= idf
        return v / max(np.linalg.norm(v), 1e-12)

    centroids = np.stack([np.mean([vector(r) for r in train if r["label"] == label], axis=0)
                          for label in LABELS])
    centroids /= np.maximum(np.linalg.norm(centroids, axis=1, keepdims=True), 1e-12)
    return np.stack([vector(r) @ centroids.T for r in test]).argmax(axis=1)


def paired_family_interval(rows, before, after):
    """Descriptive paired bootstrap; paraphrases are resampled together, 2,000 draws."""
    families = sorted({r["family"] for r in rows})
    y = np.array([LABELS.index(r["label"]) for r in rows])
    difference = (np.argmax(after, axis=1) == y).astype(float) - (np.argmax(before, axis=1) == y)
    deltas = np.array([difference[[r["family"] == f for r in rows]].mean() for f in families])
    rng = np.random.default_rng(SEED)
    samples = rng.choice(deltas, size=(2000, len(deltas)), replace=True).mean(axis=1)
    return {"families": len(families), "accuracy_delta": float(difference.mean()),
            "percentile_95": np.percentile(samples, [2.5, 97.5]).tolist(),
            "scope": "paired family bootstrap of synthetic scenarios; not a population guarantee"}


def run(args):
    import mlx.core as mx
    import mlx.nn as nn
    import mlx.optimizers as optim
    from mlx.utils import tree_flatten, tree_map

    rows = load_dataset(args.dataset)
    splits = {s: [r for r in rows if r["split"] == s] for s in ("train", "dev", "calibration", "test")}
    artifact = args.artifact.resolve()
    if (artifact / "manifest.json").exists():
        raise ValueError("artifact already sealed; choose a new --artifact directory for a new experiment")
    artifact.mkdir(parents=True, exist_ok=True)
    started = time.perf_counter()
    model = LayaExpressionModel(args.base)
    scorer = model.agent.model.scorer
    original = tree_map(lambda x: mx.array(x), scorer.parameters())
    mx.eval(original)
    before_weights = {name: np.array(value) for name, value in tree_flatten(original)}

    def encode(split):
        result = []
        for row in splits[split]:
            result.append(model.features(row["text"]))
        x = mx.concatenate(result, axis=0)
        y = mx.array([LABELS.index(r["label"]) for r in splits[split]])
        mx.eval(x, y)
        print(f"encoded {split}: {len(result)} scenarios", flush=True)
        return x, y

    cache = {s: encode(s) for s in ("train", "dev", "calibration")}
    original_logits = {}
    for split, (x, _) in cache.items():
        original_logits[split] = np.asarray(scorer(x).squeeze(-1))
    xtrain, ytrain = cache["train"]
    xdev, ydev = cache["dev"]

    def loss_fn(module, x, y):
        return nn.losses.cross_entropy(module(x).squeeze(-1), y, reduction="mean")

    value_grad = nn.value_and_grad(scorer, loss_fn)
    trials, best = [], None
    for lr, wd in GRID:
        scorer.update(tree_map(lambda x: mx.array(x), original))
        optimizer = optim.AdamW(learning_rate=lr, weight_decay=wd)
        rng = np.random.default_rng(SEED)
        trial_best = None
        for epoch in range(1, EPOCHS + 1):
            order = rng.permutation(len(ytrain))
            for start in range(0, len(order), BATCH_SIZE):
                idx = mx.array(order[start:start + BATCH_SIZE])
                loss, grads = value_grad(scorer, xtrain[idx], ytrain[idx])
                grads, _ = optim.clip_grad_norm(grads, max_norm=1.0)
                optimizer.update(scorer, grads)
                mx.eval(scorer.parameters(), optimizer.state, loss)
                if not np.isfinite(float(loss)):
                    raise ValueError("nonfinite training loss")
            z = np.array(scorer(xdev).squeeze(-1))
            score = metrics(probabilities(z), np.array(ydev))
            # Macro-F1 primary, NLL tie-break. No calibration or test selection.
            rank = (-score["macro_f1"], score["nll"])
            if trial_best is None or rank < trial_best["rank"]:
                trial_best = {"rank": rank, "lr": lr, "weight_decay": wd, "epoch": epoch,
                              "dev": score, "dev_logits": z.tolist(),
                              "weights": tree_map(lambda x: mx.array(x), scorer.parameters())}
                mx.eval(trial_best["weights"])
        trials.append({k: v for k, v in trial_best.items() if k not in ("rank", "weights")})
        if best is None or trial_best["rank"] < best["rank"]:
            best = trial_best
        print(f"lr={lr:g} wd={wd:g} epoch={trial_best['epoch']} dev_f1={trial_best['dev']['macro_f1']:.3f}", flush=True)

    scorer.update(best["weights"])
    xcal, ycal = cache["calibration"]
    cal_logits = np.array(scorer(xcal).squeeze(-1))
    temperature = fit_temperature(cal_logits, np.array(ycal))
    threshold = fit_threshold(probabilities(cal_logits, temperature), np.array(ycal))
    base_temperature = fit_temperature(original_logits["calibration"], np.array(ycal))
    base_threshold = fit_threshold(probabilities(original_logits["calibration"], base_temperature), np.array(ycal))
    scorer.save_weights(str(artifact / "scorer.safetensors"))
    changed = sum(int(np.count_nonzero(np.array(v) != before_weights[k]))
                  for k, v in tree_flatten(scorer.parameters()))
    manifest = {"schema_sha256": SCHEMA_HASH, "base_sha256": sha256(args.base / "model.safetensors"),
                "dataset_sha256": sha256(args.dataset), "scorer_sha256": sha256(artifact / "scorer.safetensors"),
                "temperature": temperature, "threshold": threshold, "labels": LABELS,
                "selected": {k: best[k] for k in ("lr", "weight_decay", "epoch")},
                "trainable_parameters": sum(v.size for _, v in tree_flatten(scorer.parameters())),
                "changed_parameters": changed, "training": "supervised cross-entropy on existing scorer only",
                "seed": SEED, "batch_size": BATCH_SIZE, "max_epochs": EPOCHS,
                "production_enabled": False, "sealed_before_test": datetime.now(timezone.utc).isoformat()}
    write_json(artifact / "manifest.json", manifest)

    # Only now evaluate the test rows. Never feed these results back into fitting.
    xtest, _ = encode("test")
    cache["test"] = (xtest, mx.array([LABELS.index(r["label"]) for r in splits["test"]]))
    tuned_logits = {s: np.array(scorer(x).squeeze(-1)) for s, (x, _) in cache.items()}
    scorer.update(original)
    original_logits["test"] = np.array(scorer(xtest).squeeze(-1))
    # Load the written artifact through the actual runtime, not the in-memory trainer.
    deployed = LayaExpressionModel(args.base, artifact)
    warmup = deployed.predict(splits["train"][0]["text"])
    latencies, max_delta = [], 0.0
    for row, z in zip(splits["test"], tuned_logits["test"], strict=True):
        answer = deployed.predict(row["text"])
        max_delta = max(max_delta, float(np.max(np.abs(np.array(answer["probabilities"]) - probabilities(z, temperature)))))
        latencies.append(answer["ms"])
    if max_delta > 1e-5:
        raise ValueError(f"saved scorer differs from tuning path: {max_delta}")

    records = []
    scores = {}
    for split in splits:
        y = [LABELS.index(r["label"]) for r in splits[split]]
        baseline = probabilities(original_logits[split])
        base_cal = probabilities(original_logits[split], base_temperature)
        tuned = probabilities(tuned_logits[split], temperature)
        scores[split] = {"baseline": metrics(baseline, y), "baseline_calibrated": metrics(base_cal, y),
                         "tuned_uncalibrated": metrics(probabilities(tuned_logits[split]), y),
                         "tuned": metrics(tuned, y),
                         "baseline_selective": selection_metrics(base_cal, y, base_threshold),
                         "tuned_selective": selection_metrics(tuned, y, threshold)}
        for row, before, after in zip(splits[split], original_logits[split], tuned_logits[split], strict=True):
            records.append({"id": row["id"], "baseline_logits": before.tolist(), "tuned_logits": after.tolist()})
    tfidf_pred = tfidf_predictions(splits["train"], splits["test"])
    tfidf = metrics(np.eye(len(LABELS))[tfidf_pred], [LABELS.index(r["label"]) for r in splits["test"]])
    report = {"created_at": datetime.now(timezone.utc).isoformat(), "dataset": str(args.dataset.relative_to(ROOT))
              if args.dataset.is_relative_to(ROOT) else str(args.dataset),
              "dataset_sha256": sha256(args.dataset), "base": str(args.base), "artifact": str(artifact.relative_to(ROOT))
              if artifact.is_relative_to(ROOT) else str(artifact), "manifest": manifest,
              "manifest_sha256": sha256(artifact / "manifest.json"),
              "code_sha256": {str(p.relative_to(ROOT)): sha256(p) for p in CODE},
              "runtime": {"platform": platform.platform(), "python": platform.python_version(),
                          "laya_mlx": importlib.metadata.version("laya-mlx"), "mlx": importlib.metadata.version("mlx"),
                          "device": str(mx.metal.device_info())},
              "baseline_temperature": base_temperature, "baseline_threshold": base_threshold,
              "trials": trials, "scores": scores, "tfidf_test": tfidf,
              "paired_test_interval": paired_family_interval(splits["test"], original_logits["test"], tuned_logits["test"]),
              "records": records, "latency": {"n": len(latencies), "samples_ms": latencies,
                  "warmup_ms": warmup["ms"], "p50_ms": float(np.percentile(latencies, 50)),
                  "p95_ms": float(np.percentile(latencies, 95)), "max_ms": max(latencies),
                  "scope": "warm batch-one text preparation, frozen model, tuned scorer, calibrated probabilities; no audio/transport/render"},
              "reload_max_probability_delta": max_delta, "elapsed_s": time.perf_counter() - started,
              "limits": ["Synthetic labels authored by the same assistant as implementation.",
                         "Paraphrases share families; sample count is not independent human evidence.",
                         "No human expression-recognition study or on-device end-to-end timing.",
                         "English scenarios only; multilingual checkpoint does not establish multilingual quality.",
                         "No production activation; firmware conversation overlay still required."]}
    write_json(args.report, report)
    print(json.dumps({"test": scores["test"], "tfidf_accuracy": tfidf["accuracy"],
                      "latency_p95_ms": report["latency"]["p95_ms"], "artifact": str(artifact)}, indent=2))


def verify_report(report: dict, *, live: bool = True):
    """Recompute from row logits; validate artifacts; optionally repeat real inference."""
    dataset = ROOT / report["dataset"]
    rows = load_dataset(dataset)
    if sha256(dataset) != report["dataset_sha256"]:
        raise ValueError("dataset hash mismatch")
    for path, digest in report["code_sha256"].items():
        if sha256(ROOT / path) != digest:
            raise ValueError("code hash mismatch")
    artifact = ROOT / report["artifact"]
    manifest = report["manifest"]
    if (sha256(artifact / "manifest.json") != report["manifest_sha256"]
            or json.loads((artifact / "manifest.json").read_text()) != manifest
            or sha256(artifact / "scorer.safetensors") != manifest["scorer_sha256"]
            or sha256(Path(report["base"]) / "model.safetensors") != manifest["base_sha256"]
            or manifest["schema_sha256"] != SCHEMA_HASH or manifest["changed_parameters"] <= 0):
        raise ValueError("model artifact mismatch or unchanged scorer")
    records = {r["id"]: r for r in report["records"]}
    if len(records) != len(report["records"]) or set(records) != {r["id"] for r in rows}:
        raise ValueError("missing or duplicate evaluation records")
    for split, claimed in report["scores"].items():
        selected = [r for r in rows if r["split"] == split]
        y = [LABELS.index(r["label"]) for r in selected]
        before = np.array([records[r["id"]]["baseline_logits"] for r in selected])
        after = np.array([records[r["id"]]["tuned_logits"] for r in selected])
        p0, p1 = probabilities(before, report["baseline_temperature"]), probabilities(after, manifest["temperature"])
        actual = {"baseline": metrics(probabilities(before), y), "baseline_calibrated": metrics(p0, y),
                  "tuned_uncalibrated": metrics(probabilities(after), y), "tuned": metrics(p1, y),
                  "baseline_selective": selection_metrics(p0, y, report["baseline_threshold"]),
                  "tuned_selective": selection_metrics(p1, y, manifest["threshold"])}
        if actual != claimed:
            raise ValueError(f"metrics mismatch: {split}")
        if split == "calibration":
            if fit_temperature(after, y) != manifest["temperature"] or fit_threshold(p1, y) != manifest["threshold"]:
                raise ValueError("calibration not fitted on calibration split")
    if set(report["scores"]) != {"train", "dev", "calibration", "test"}:
        raise ValueError("missing split metrics")
    winner = min(report["trials"], key=lambda t: (-t["dev"]["macro_f1"], t["dev"]["nll"]))
    dev_y = [LABELS.index(r["label"]) for r in rows if r["split"] == "dev"]
    if len(report["trials"]) != len(GRID):
        raise ValueError("incomplete hyperparameter sweep")
    for trial, (lr, wd) in zip(report["trials"], GRID, strict=True):
        if (trial["lr"] != lr or trial["weight_decay"] != wd
                or metrics(probabilities(trial["dev_logits"]), dev_y) != trial["dev"]):
            raise ValueError("trial dev metric mismatch")
    if any(winner[k] != manifest["selected"][k] for k in ("lr", "weight_decay", "epoch")):
        raise ValueError("selection disagrees with dev-only criterion")
    train, test = [r for r in rows if r["split"] == "train"], [r for r in rows if r["split"] == "test"]
    tfidf = metrics(np.eye(len(LABELS))[tfidf_predictions(train, test)], [LABELS.index(r["label"]) for r in test])
    if tfidf != report["tfidf_test"]:
        raise ValueError("TF-IDF baseline mismatch")
    interval = paired_family_interval(test, [records[r["id"]]["baseline_logits"] for r in test],
                                     [records[r["id"]]["tuned_logits"] for r in test])
    if interval != report["paired_test_interval"]:
        raise ValueError("family interval mismatch")
    samples = report["latency"]["samples_ms"]
    if len(samples) != report["latency"]["n"] or any(not np.isfinite(x) or x <= 0 for x in samples):
        raise ValueError("invalid latency samples")
    for name, pct in (("p50_ms", 50), ("p95_ms", 95)):
        if float(np.percentile(samples, pct)) != report["latency"][name]:
            raise ValueError("latency summary mismatch")
    if live:
        model = LayaExpressionModel(Path(report["base"]), artifact)
        for row in rows:
            if row["split"] == "test":
                p = model.predict(row["text"])["probabilities"]
                expected = probabilities(records[row["id"]]["tuned_logits"], manifest["temperature"])
                if not np.allclose(p, expected, atol=1e-5, rtol=1e-5):
                    raise ValueError(f"live replay mismatch: {row['id']}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("run", "verify", "predict"))
    parser.add_argument("--dataset", type=Path, default=DATASET)
    parser.add_argument("--base", type=Path, default=BASE)
    parser.add_argument("--artifact", type=Path, default=ARTIFACT)
    parser.add_argument("--report", type=Path, default=REPORT)
    parser.add_argument("--text")
    args = parser.parse_args()
    args.base, args.dataset = args.base.expanduser().resolve(), args.dataset.resolve()
    if args.command == "run":
        run(args)
    elif args.command == "verify":
        report = json.loads(args.report.read_text())
        verify_report(report)
        # Negative controls: the exact same verifier must reject corrupted evidence.
        for key in ("accuracy", "macro_f1", "nll"):
            corrupted = copy.deepcopy(report)
            corrupted["scores"]["test"]["tuned"][key] += 0.01
            try:
                verify_report(corrupted, live=False)
            except ValueError:
                continue
            raise AssertionError("tampered metric accepted")
        print("real-model replay and tamper controls passed")
    else:
        if not args.text:
            parser.error("predict requires --text")
        model = LayaExpressionModel(args.base, args.artifact)
        answer = model.predict(args.text)
        answer["accepted"] = max(answer["probabilities"]) >= model.threshold
        answer["threshold"] = model.threshold
        print(json.dumps(answer, indent=2))


if __name__ == "__main__":
    main()
