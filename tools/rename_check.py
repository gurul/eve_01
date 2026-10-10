#!/usr/bin/env python3
"""The rename to eve is complete: no tracked text or path outside vendor/ says the old name.

Two names stay, because they belong to someone else: Apple's PlistBuddy tool,
and the upstream project this was forked from (claude-desktop-buddy). Four files stay byte-for-byte:
the blind eval sets whose sha256 was committed before they were scored. Rewriting one would
make its verdict a claim about a set nobody scored.
The check proves it can fail first, on a planted positive, before it trusts a clean result.
Prints RENAME_CLEAN and exits 0 only when every assertion holds.
"""
from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
OLD = re.compile("bu" + "ddy", re.IGNORECASE)   # split so this file does not find itself
FROZEN = {
    "bridge/tests/fixtures/chief/capture_holdout.json",
    "bridge/tests/fixtures/chief/replan_holdout.json",
    "bridge/tests/fixtures/grade_holdout.json",
    "bridge/tests/fixtures/memory_eval/questions.json",
}
# The chat bundle carries ci-info's list of CI vendors; one of them is a CI service that shares the old name.
KEPT = re.compile(r"PlistBu" + r"ddy|claude-desktop-bu" + r"ddy"
                  + r"|name:`Bu" + r"ddy`,constant:`BU" + r"DDY`,env:`BU" + r"DDY_WORKSPACE_ID`,pr:`BU" + r"DDY_EXECUTION_PULL_REQUEST_ID`")


def offenders(text: str) -> list[str]:
    return [m.group(0) for m in OLD.finditer(KEPT.sub("", text))]


def main() -> int:
    planted = "the " + "Bu" + "ddy app, beside PlistBu" + "ddy"
    if offenders(planted) != ["Bu" + "ddy"]:
        print("control failed: the checker does not catch a planted positive")
        return 1

    files = subprocess.run(["git", "ls-files", "-z"], cwd=ROOT, capture_output=True, check=True).stdout
    paths = [p for p in files.decode().split("\0") if p and not p.startswith("vendor/") and p not in FROZEN]
    bad_paths = [p for p in paths if offenders(p)]
    bad_text: list[str] = []
    for p in paths:
        try:
            data = (ROOT / p).read_bytes()
        except (FileNotFoundError, IsADirectoryError):
            continue
        if b"\0" in data[:8192]:
            continue   # binary
        text = data.decode("utf-8", errors="replace")
        for n, line in enumerate(text.splitlines(), 1):
            if offenders(line):
                bad_text.append(f"{p}:{n}: {line.strip()[:120]}")
    for p in bad_paths[:20]:
        print("path:", p)
    for t in bad_text[:40]:
        print("text:", t)
    if bad_paths or bad_text:
        print(f"{len(bad_paths)} paths and {len(bad_text)} lines still carry the old name")
        return 1
    print(f"RENAME_CLEAN ({len(paths)} tracked files checked)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
