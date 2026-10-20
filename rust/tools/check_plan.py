#!/usr/bin/env python3
"""Check that plan.json maps every tracked Python module in bridge/src to exactly one crate."""
import json
import subprocess
import sys
from pathlib import Path

RUST = Path(__file__).resolve().parent.parent
plan = json.loads((RUST / "plan.json").read_text())
mods = [m for c in plan for m in c["modules"]]
files = subprocess.run(
    ["git", "ls-files", "bridge/src/eve_bridge"], cwd=RUST.parent, capture_output=True, text=True, check=True
).stdout.split()
py = []
for f in files:
    if not f.endswith(".py"):
        continue
    m = f.split("eve_bridge/", 1)[1][:-3].replace("/", ".")
    py.append("eve_bridge" if m == "__init__" else m.removesuffix(".__init__"))
missing = sorted(set(py) - set(mods))
extra = sorted(set(mods) - set(py))
dupes = sorted({m for m in mods if mods.count(m) > 1})
print(f"{len(py)} python modules, {len(mods)} planned, {len(plan)} crates")
if missing or extra or dupes or not py:
    print("missing:", missing, "extra:", extra, "duplicated:", dupes)
    sys.exit(1)
print("plan coverage passed")
