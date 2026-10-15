#!/usr/bin/env bash
# The branch's diff against origin/main (tracked changes and new files) must not contain any pattern in the local,
# untracked pattern file (one extended regex per line), so the patterns themselves never reach the public repo.
# A positive control runs first: a line built from the first pattern's own text must be caught.
set -euo pipefail
PATTERNS="${1:-.unlazy/leak-patterns.txt}"
[ -s "$PATTERNS" ] || { echo "no pattern file at $PATTERNS"; exit 1; }
control="$(head -1 "$PATTERNS" | sed 's/\\//g; s/\[0-9\]+/1/g')"
printf 'x %s x\n' "$control" | grep -qEf "$PATTERNS" || { echo "FAIL: positive control not caught"; exit 1; }
diff="$( { git diff origin/main -- . ':(exclude).unlazy'; git ls-files --others --exclude-standard -z | xargs -0 cat; } )"
hits="$(printf '%s\n' "$diff" | grep -nEf "$PATTERNS" || true)"
if [ -n "$hits" ]; then
    echo "FAIL: $(printf '%s\n' "$hits" | wc -l | tr -d ' ') line(s) match a private pattern"
    exit 1
fi
echo LEAKS_CLEAN
