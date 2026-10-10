#!/usr/bin/env python3
"""Score a fresh synthetic confirmation set after model selection; never fit on it."""

from __future__ import annotations

import argparse
import copy
import json
import sys
from datetime import datetime, timezone
from pathlib import Path

import numpy as np
from emotion_eval import BASE, DATASET, tfidf_predictions, write_json
from emotion_head_tune import ARTIFACT, load_model

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "bridge/src"))

from eve_bridge.emotion_policy import (  # noqa: E402
    LABELS,
    LayaExpressionModel,
    metrics,
    selection_metrics,
    sha256,
)

CONFIRM = ROOT / "bridge/tests/fixtures/emotion/confirmation.json"
REPORT = ROOT / "docs/stackchan/laya-emotion/confirmation-results.json"


def load_confirmation():
    data = json.loads(CONFIRM.read_text())
    original = json.loads(DATASET.read_text())["rows"]
    known = {" ".join(r["text"].casefold().split()) for r in original}
    seen, ids = set(), set()
    for row in data["rows"]:
        text = " ".join(row["text"].casefold().split())
        if not text or text in known or text in seen or row["id"] in ids or row["label"] not in LABELS:
            raise ValueError("invalid or overlapping confirmation case")
        seen.add(text)
        ids.add(row["id"])
    if len(ids) != 36 or {r["label"] for r in data["rows"]} != set(LABELS):
        raise ValueError("confirmation set incomplete")
    return data["rows"], original


def summarize(records, rows, original, threshold):
    y = [LABELS.index(r["label"]) for r in rows]
    result = {}
    for variant in ("baseline", "head"):
        p = [r[variant]["probabilities"] for r in records]
        result[variant] = metrics(p, y)
        if variant == "head":
            result["selective"] = selection_metrics(p, y, threshold)
    pred = tfidf_predictions([r for r in original if r["split"] == "train"], rows)
    result["tfidf"] = metrics(np.eye(len(LABELS))[pred], y)
    return result


def validate_report(report):
    rows, original = load_confirmation()
    manifest = json.loads((ARTIFACT / "manifest.json").read_text())
    if (sha256(CONFIRM) != report["confirmation_sha256"]
            or sha256(ARTIFACT / "manifest.json") != report["model_manifest_sha256"]
            or [r["id"] for r in report["records"]] != [r["id"] for r in rows]):
        raise ValueError("confirmation evidence mismatch")
    if summarize(report["records"], rows, original, manifest["threshold"]) != report["scores"]:
        raise ValueError("confirmation score mismatch")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("run", "verify"))
    args = parser.parse_args()
    if args.command == "run":
        if REPORT.exists():
            raise ValueError("confirmation already scored; preserve its first result")
        rows, original = load_confirmation()
        data_hash, model_hash = sha256(CONFIRM), sha256(ARTIFACT / "manifest.json")
        # Commit evidence before inference; selection is already sealed in manifest.
        write_json(ARTIFACT / "confirmation-seal.json", {
            "confirmation_sha256": data_hash, "model_manifest_sha256": model_hash,
            "sealed_at": datetime.now(timezone.utc).isoformat()})
        base, head = LayaExpressionModel(BASE), load_model(ARTIFACT)
        records = [{"id": r["id"], "baseline": base.predict(r["text"]), "head": head.predict(r["text"])} for r in rows]
        report = {"created_at": datetime.now(timezone.utc).isoformat(), "confirmation_sha256": data_hash,
                  "model_manifest_sha256": model_hash, "records": records,
                  "scores": summarize(records, rows, original, head.threshold),
                  "limits": "New scenarios, but still synthetic, English, and labeled by the implementing assistant. Not human validation."}
        write_json(REPORT, report)
        print(json.dumps(report["scores"], indent=2))
    else:
        report = json.loads(REPORT.read_text())
        validate_report(report)
        bad = copy.deepcopy(report)
        bad["scores"]["head"]["accuracy"] += 0.01
        try:
            validate_report(bad)
        except ValueError:
            pass
        else:
            raise AssertionError("tampered confirmation score accepted")
        rows, _ = load_confirmation()
        base, head = LayaExpressionModel(BASE), load_model(ARTIFACT)
        for row, record in zip(rows, report["records"], strict=True):
            for name, model in (("baseline", base), ("head", head)):
                if not np.allclose(model.predict(row["text"])["probabilities"], record[name]["probabilities"], atol=2e-4, rtol=2e-4):
                    raise ValueError("confirmation replay mismatch")
        print("CONFIRMATION_REPLAY_OK")


if __name__ == "__main__":
    main()
