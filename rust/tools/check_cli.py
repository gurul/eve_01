#!/usr/bin/env python3
"""Compare the command tree of the Python and Rust eve-bridge CLIs.

Walks `--help` recursively for both, in the full profile and in eve lite
(EVE_PROFILE=lite), reading the `{a,b,c}` choice list each level prints, and
fails if the Rust binary is missing any command path the Python one offers.

The Python side runs from this worktree's bridge/src with the interpreter in
EVE_PY (default: the eve checkout's venv). The Rust side runs
target/debug/eve-bridge, which `cargo build -p eve-bridge` produces.
"""
import os
import re
import subprocess
import sys
from pathlib import Path

RUST = Path(__file__).resolve().parent.parent
BRIDGE = RUST.parent / "bridge"
PY = os.environ.get("EVE_PY", str(Path.home() / "Documents/personal/eve/bridge/.venv/bin/python"))
RS = RUST / "target" / "debug" / "eve-bridge"
CHOICES = re.compile(r"\{([a-z0-9_,-]+)\}")


def run(cmd, env):
    out = subprocess.run(cmd, capture_output=True, text=True, timeout=60, env=env, cwd=BRIDGE)
    return out.stdout + out.stderr


def tree(base, env, path=(), depth=0):
    text = run([*base, *path, "--help"], env)
    found = {path}
    usage = text.split("\n\n", 1)[0]
    m = CHOICES.search(usage)
    if m and depth < 3:
        for c in m.group(1).split(","):
            found |= tree(base, env, (*path, c), depth + 1)
    return found


def main():
    if not RS.exists():
        print(f"{RS} missing: run cargo build -p eve-bridge")
        return 1
    missing = []
    for profile in ("full", "lite"):
        env = {**os.environ, "PYTHONPATH": str(BRIDGE / "src")}
        env.pop("EVE_PROFILE", None)
        if profile == "lite":
            env["EVE_PROFILE"] = "lite"
        py = tree([PY, "-m", "eve_bridge"], env)
        rs = tree([str(RS)], env)
        if len(py) < 10:
            print(f"{profile}: python tree too small ({len(py)}); the walk is broken")
            return 1
        gap = sorted(py - rs)
        print(f"{profile}: python {len(py)} command paths, rust {len(rs)}, missing {len(gap)}")
        missing += [f"{profile}: {' '.join(p)}" for p in gap]
    for m in missing:
        print("missing", m)
    if missing:
        return 1
    print("cli parity passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
