#!/usr/bin/env python3
"""Verify the frozen local evaluation inputs without executing their contents."""
import hashlib
import json
from pathlib import Path

base = Path(__file__).resolve().parent
record = json.loads((base / "FREEZE.json").read_text())
required = {"cases.json", "BAR.md", "check.py", "REVIEW.md", "verify-freeze.py"}
if set(record["sha256"]) != required:
    raise SystemExit("freeze file set mismatch")
for name, expected in record["sha256"].items():
    actual = hashlib.sha256((base / name).read_bytes()).hexdigest()
    if actual != expected:
        raise SystemExit(f"freeze digest mismatch: {name}")
print("confirmation freeze verified: " + str(len(required)) + " files")
