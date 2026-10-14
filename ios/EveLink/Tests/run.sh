#!/bin/sh
# Eve Link's testable logic on the Mac (Tests/main.swift).
set -eu
cd "$(dirname "$0")/../../.."
OUT=$(mktemp -d)
trap 'rm -rf "$OUT"' EXIT HUP INT TERM
swiftc -O -o "$OUT/link_test" ios/EveLink/Shared/PCM.swift ios/EveLink/Shared/PhoneTurn.swift ios/EveLink/Shared/Rendezvous.swift \
  ios/EveLink/Shared/ChatBridge.swift ios/EveLink/Shared/ChatTransport.swift ios/EveLink/Shared/WorkClient.swift ios/EveLink/Shared/WorkSessions.swift ios/EveLink/Shared/ControlsClient.swift ios/EveLink/Shared/MemoryText.swift ios/EveLink/Shared/NotesText.swift ios/EveLink/Shared/PikaDrive.swift ios/EveLink/Shared/ObsbotControl.swift \
  ios/EveLink/Shared/Pairing.swift ios/EveLink/Shared/Today.swift ios/EveLink/Shared/Updates.swift ios/EveLink/Shared/PhoneLocation.swift ios/EveLink/Shared/FaceExpression.swift \
  ios/EveLink/Tests/main.swift
"$OUT/link_test"
# Voice mode's captions and expressions: the real App/VoiceLevels.swift on its playback clock (about 11 s of waiting).
swiftc -O -o "$OUT/captions_test" ios/EveLink/Shared/PCM.swift ios/EveLink/Shared/FaceExpression.swift ios/EveLink/App/VoiceLevels.swift \
  ios/EveLink/Tests/Captions/Sink.swift ios/EveLink/Tests/Captions/main.swift
"$OUT/captions_test" | tail -1 | grep -q CAPTIONS_OK && echo CAPTIONS_OK
# The face's motion: the real App/StackChanFace.swift FaceMotion (SwiftUI builds on the Mac too).
swiftc -O -o "$OUT/face_test" ios/EveLink/Shared/PCM.swift ios/EveLink/Shared/FaceExpression.swift ios/EveLink/App/VoiceLevels.swift \
  ios/EveLink/App/StackChanFace.swift ios/EveLink/Tests/Captions/Sink.swift ios/EveLink/Tests/Face/main.swift
"$OUT/face_test" | tail -1 | grep -q FACE_OK && echo FACE_OK
