#!/bin/sh
# A PNG contact sheet of all the expressions on the voice-mode face (Tests/Face/render.swift), at 0.3, 2 and 3 s.
#   sh ios/EveLink/Tests/Face/render.sh [out.png] [--phone] [--only label,label]
#   default out: ios/EveLink/build/expressions.png; --phone draws at true phone scale (1 px per point).
set -eu
cd "$(dirname "$0")/../../../.."
OUT=$(mktemp -d)
trap 'rm -rf "$OUT"' EXIT HUP INT TERM
swiftc -O -parse-as-library -o "$OUT/render" ios/EveLink/Shared/PCM.swift ios/EveLink/Shared/FaceExpression.swift \
  ios/EveLink/App/VoiceLevels.swift ios/EveLink/App/StackChanFace.swift ios/EveLink/Tests/Captions/Sink.swift \
  ios/EveLink/Tests/Face/render.swift
case "${1:-}" in
  ""|--*) "$OUT/render" ios/EveLink/build/expressions.png "$@" ;;
  *) "$OUT/render" "$@" ;;
esac
