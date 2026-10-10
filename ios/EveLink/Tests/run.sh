#!/bin/sh
# Eve Link's testable logic on the Mac (Tests/main.swift).
set -eu
cd "$(dirname "$0")/../../.."
OUT=$(mktemp -d)
trap 'rm -rf "$OUT"' EXIT HUP INT TERM
swiftc -O -o "$OUT/link_test" ios/EveLink/Shared/PCM.swift ios/EveLink/Shared/PhoneTurn.swift ios/EveLink/Shared/Rendezvous.swift \
  ios/EveLink/Shared/ChatBridge.swift ios/EveLink/Shared/ChatTransport.swift ios/EveLink/Shared/WorkClient.swift ios/EveLink/Shared/ControlsClient.swift ios/EveLink/Shared/MemoryText.swift ios/EveLink/Shared/NotesText.swift \
  ios/EveLink/Shared/Pairing.swift ios/EveLink/Shared/Today.swift \
  ios/EveLink/Tests/main.swift
"$OUT/link_test"
# Voice mode's captions: the real App/VoiceLevels.swift on its playback clock (about 10 s of waiting).
swiftc -O -o "$OUT/captions_test" ios/EveLink/Shared/PCM.swift ios/EveLink/App/VoiceLevels.swift \
  ios/EveLink/Tests/Captions/Sink.swift ios/EveLink/Tests/Captions/main.swift
"$OUT/captions_test" | tail -1 | grep -q CAPTIONS_OK && echo CAPTIONS_OK
