#!/usr/bin/env python3
"""Second, explicitly exploratory experiment: adapt Laya's decision transformer.

The scorer-only study's test set has already been inspected. This experiment uses
the same dev-only selection, but its original test results are exploratory. A new,
sealed confirmation set must be used after selection for an independent check.
"""

from __future__ import annotations

import argparse
import json
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

import numpy as np

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "bridge/src"))

from emotion_eval import BASE, DATASET, SEED, write_json  # noqa: E402

from eve_bridge.emotion_policy import (  # noqa: E402
    LABELS,
    QUESTION,
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

ARTIFACT = ROOT / ".test-artifacts/laya-emotion-head"
REPORT = ROOT / "docs/stackchan/laya-emotion/head-results.json"
GRID = [(1e-5, 0.01), (3e-5, 0.01), (1e-4, 0.01)]
EPOCHS = 12


def head_module(model):
    import mlx.nn as nn

    class TrainHead(nn.Module):
        def __init__(self):
            super().__init__()
            self.head, self.scorer = model.head, model.scorer

        def __call__(self, hidden, mask, positions):
            import mlx.core as mx

            h = self.head(hidden, mask[:, None, None, :])
            markers = h[mx.arange(h.shape[0])[:, None], positions].astype(mx.float32)
            return self.scorer(markers).squeeze(-1)

    return TrainHead()


def load_model(artifact):
    import mlx.core as mx

    manifest = json.loads((artifact / "manifest.json").read_text())
    model = LayaExpressionModel(Path(manifest["base"]))
    if (manifest["schema_sha256"] != SCHEMA_HASH
            or sha256(Path(manifest["base"]) / "model.safetensors") != manifest["base_sha256"]):
        raise ValueError("base or schema mismatch")
    for component in ("head", "scorer"):
        path = artifact / f"{component}.safetensors"
        if sha256(path) != manifest[f"{component}_sha256"]:
            raise ValueError("trained component checksum mismatch")
        getattr(model.agent.model, component).load_weights(str(path))
    model.temperature, model.threshold = manifest["temperature"], manifest["threshold"]
    mx.eval(model.agent.model.parameters())
    return model


def run(args):
    import mlx.core as mx
    import mlx.nn as nn
    import mlx.optimizers as optim
    from laya_mlx.agent import collate_items
    from mlx.utils import tree_flatten, tree_map

    rows = load_dataset(DATASET)
    splits = {s: [r for r in rows if r["split"] == s] for s in ("train", "dev", "calibration", "test")}
    artifact = args.artifact.resolve()
    if (artifact / "manifest.json").exists():
        raise ValueError("artifact already sealed; use a new directory")
    artifact.mkdir(parents=True, exist_ok=True)
    model = LayaExpressionModel(BASE)
    net = head_module(model.agent.model)
    net.head.set_dtype(mx.float32)
    initial = tree_map(lambda x: mx.array(x), net.parameters())
    mx.eval(initial)
    started = time.perf_counter()
    cache = {}
    for split in ("train", "dev", "calibration"):
        items = [model.agent.prepare(r["text"], QUESTION)[0][0] for r in splits[split]]
        batch = collate_items(items, model.agent.tok.pad_token_id)
        hidden = []
        for i in range(len(items)):
            ids = mx.array(batch["input_ids"][i:i + 1])
            mask = mx.array(batch["attention_mask"][i:i + 1])
            h = model.agent.model.encoder(ids, mask)
            h = h + model.agent.model.type_emb(mx.array([0]))[:, None, :]
            mx.eval(h)
            hidden.append(h.astype(mx.float32))
        cache[split] = (mx.concatenate(hidden), mx.array(batch["attention_mask"]), mx.array(batch["marker_pos"]),
                        mx.array([LABELS.index(r["label"]) for r in splits[split]]))
        mx.eval(cache[split])
        print(f"cached frozen encoder {split}: {len(items)}", flush=True)

    def loss_fn(module, hidden, mask, pos, y):
        return nn.losses.cross_entropy(module(hidden, mask, pos), y, reduction="mean")

    grad_fn = nn.value_and_grad(net, loss_fn)
    trials, best = [], None
    for lr, wd in GRID:
        net.update(tree_map(lambda x: mx.array(x), initial))
        optimizer = optim.AdamW(learning_rate=lr, weight_decay=wd)
        trial = None
        rng = np.random.default_rng(SEED)
        for epoch in range(1, EPOCHS + 1):
            for idx in np.array_split(rng.permutation(len(splits["train"])), 12):
                part = tuple(x[mx.array(idx)] for x in cache["train"])
                loss, grads = grad_fn(net, *part)
                grads, _ = optim.clip_grad_norm(grads, 1.0)
                optimizer.update(net, grads)
                mx.eval(net.parameters(), optimizer.state, loss)
                if not np.isfinite(float(loss)):
                    raise ValueError("nonfinite loss")
            h, mask, pos, y = cache["dev"]
            z = np.array(net(h, mask, pos))
            score = metrics(probabilities(z), np.array(y))
            rank = (-score["macro_f1"], score["nll"])
            if trial is None or rank < trial["rank"]:
                trial = {"lr": lr, "weight_decay": wd, "epoch": epoch, "dev": score, "dev_logits": z.tolist(),
                         "rank": rank, "weights": tree_map(lambda x: mx.array(x), net.parameters())}
                mx.eval(trial["weights"])
            print(f"lr={lr:g} epoch={epoch} loss={float(loss):.3f} dev_f1={score['macro_f1']:.3f}", flush=True)
        trials.append({k: v for k, v in trial.items() if k not in ("rank", "weights")})
        if best is None or trial["rank"] < best["rank"]:
            best = trial

    net.update(best["weights"])
    h, mask, pos, ycal = cache["calibration"]
    zcal = np.array(net(h, mask, pos))
    temperature = fit_temperature(zcal, np.array(ycal))
    threshold = fit_threshold(probabilities(zcal, temperature), np.array(ycal))
    for component in ("head", "scorer"):
        getattr(net, component).save_weights(str(artifact / f"{component}.safetensors"))
    manifest = {"schema_sha256": SCHEMA_HASH, "base": str(BASE), "base_sha256": sha256(BASE / "model.safetensors"),
                "dataset_sha256": sha256(DATASET), "temperature": temperature, "threshold": threshold,
                "trainable_parameters": sum(v.size for _, v in tree_flatten(net.parameters())),
                "selected": {k: best[k] for k in ("lr", "weight_decay", "epoch")},
                "seed": SEED, "batch_size": 8, "max_epochs": EPOCHS,
                "production_enabled": False, "sealed_at": datetime.now(timezone.utc).isoformat(),
                "training": "supervised cross-entropy; encoder and type embedding frozen; decision transformer and scorer float32",
                **{f"{c}_sha256": sha256(artifact / f"{c}.safetensors") for c in ("head", "scorer")}}
    write_json(artifact / "manifest.json", manifest)
    # Sealed model is now the only source of held-out predictions.
    deployed = load_model(artifact)
    deployed.predict(splits["train"][0]["text"])
    records, scores, times = [], {}, []
    for split in splits:
        ps = []
        for row in splits[split]:
            answer = deployed.predict(row["text"])
            ps.append(answer["probabilities"])
            records.append({"id": row["id"], **answer})
            if split == "test":
                times.append(answer["ms"])
        labels = [LABELS.index(r["label"]) for r in splits[split]]
        scores[split] = {"model": metrics(ps, labels), "selective": selection_metrics(ps, labels, threshold)}
        if split == "calibration" and not np.allclose(ps, probabilities(zcal, temperature), atol=2e-4, rtol=2e-4):
            raise ValueError("unpadded inference diverges from padded training")
    report = {"created_at": datetime.now(timezone.utc).isoformat(), "manifest": manifest,
              "manifest_sha256": sha256(artifact / "manifest.json"), "artifact": str(artifact.relative_to(ROOT)),
              "code_sha256": {str(p.relative_to(ROOT)): sha256(p) for p in (
                  Path(__file__), ROOT / "bridge/tools/emotion_eval.py", ROOT / "bridge/src/eve_bridge/emotion_policy.py")},
              "trials": trials, "scores": scores, "records": records,
              "latency": {"samples_ms": times, "p50_ms": float(np.percentile(times, 50)),
                          "p95_ms": float(np.percentile(times, 95))},
              "calibration_logits": zcal.tolist(), "elapsed_s": time.perf_counter() - started,
              "limits": ["Same synthetic development/calibration data as first experiment.",
                         "Original test set already inspected after scorer study; exploratory, not independent confirmation.",
                         "No physical robot activation or human validation."]}
    write_json(args.report, report)
    print(json.dumps({"test": scores["test"], "manifest": manifest}, indent=2), flush=True)


def verify(args):
    report = json.loads(args.report.read_text())
    rows = load_dataset(DATASET)
    manifest = report["manifest"]
    artifact = ROOT / report["artifact"]
    if (sha256(DATASET) != manifest["dataset_sha256"]
            or sha256(artifact / "manifest.json") != report["manifest_sha256"]
            or json.loads((artifact / "manifest.json").read_text()) != manifest):
        raise ValueError("manifest or data mismatch")
    for path, digest in report["code_sha256"].items():
        if sha256(ROOT / path) != digest:
            raise ValueError("code mismatch")
    records = {r["id"]: r for r in report["records"]}
    if len(records) != len(rows) or len(records) != len(report["records"]) or set(records) != {r["id"] for r in rows}:
        raise ValueError("missing or duplicate records")
    for split in ("train", "dev", "calibration", "test"):
        subset = [r for r in rows if r["split"] == split]
        y = [LABELS.index(r["label"]) for r in subset]
        p = [records[r["id"]]["probabilities"] for r in subset]
        actual = {"model": metrics(p, y), "selective": selection_metrics(p, y, manifest["threshold"])}
        if actual != report["scores"][split]:
            raise ValueError("metrics mismatch")
        if split == "calibration":
            t = fit_temperature(report["calibration_logits"], y)
            threshold = fit_threshold(probabilities(report["calibration_logits"], t), y)
            if t != manifest["temperature"] or threshold != manifest["threshold"]:
                raise ValueError("calibration mismatch")
    dev_y = [LABELS.index(r["label"]) for r in rows if r["split"] == "dev"]
    for trial, (lr, wd) in zip(report["trials"], GRID, strict=True):
        if trial["lr"] != lr or trial["weight_decay"] != wd or metrics(probabilities(trial["dev_logits"]), dev_y) != trial["dev"]:
            raise ValueError("trial mismatch")
    winner = min(report["trials"], key=lambda t: (-t["dev"]["macro_f1"], t["dev"]["nll"]))
    if any(winner[k] != manifest["selected"][k] for k in ("lr", "weight_decay", "epoch")):
        raise ValueError("selection mismatch")
    model = load_model(artifact)
    for row in rows:
        if row["split"] == "test":
            current = model.predict(row["text"])["probabilities"]
            if not np.allclose(current, records[row["id"]]["probabilities"], atol=2e-4, rtol=2e-4):
                raise ValueError("live model mismatch")
    print("HEAD_MODEL_REPLAY_OK")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("run", "verify", "predict"))
    parser.add_argument("--artifact", type=Path, default=ARTIFACT)
    parser.add_argument("--report", type=Path, default=REPORT)
    parser.add_argument("--text")
    args = parser.parse_args()
    if args.command == "run":
        run(args)
    elif args.command == "verify":
        verify(args)
    else:
        if not args.text:
            parser.error("predict requires --text")
        model = load_model(args.artifact)
        answer = model.predict(args.text)
        answer["accepted"] = max(answer["probabilities"]) >= model.threshold
        print(json.dumps(answer, indent=2))


if __name__ == "__main__":
    main()
