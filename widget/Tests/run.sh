#!/bin/sh
# Foundation-only file-reader regressions; no App Group or live daemon needed.
set -eu
cd "$(dirname "$0")/../.."
OUT=$(mktemp -d)
trap 'rm -rf "$OUT"' EXIT
swiftc -O -parse-as-library -swift-version 6 widget/Shared/NoteStore.swift widget/Tests/main.swift -o "$OUT/notes-test"
"$OUT/notes-test"
