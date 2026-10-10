#!/bin/sh
# Optional first argument: a previous revision's NoteStore.swift to compare.
set -eu
cd "$(dirname "$0")/../.."
OUT=$(mktemp -d)
trap 'rm -rf "$OUT"' EXIT
swiftc -O -parse-as-library -swift-version 6 widget/Shared/NoteStore.swift widget/Tests/benchmark.swift -o "$OUT/current"
"$OUT/current" --prepare "$OUT/fixture"
if [ "$#" -gt 0 ]; then
    swiftc -O -parse-as-library -swift-version 6 "$1" widget/Tests/benchmark.swift -o "$OUT/before"
    "$OUT/before" before "$OUT/fixture"
fi
"$OUT/current" current "$OUT/fixture"
