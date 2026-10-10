#!/bin/sh
# Build Eve Link for the owner's iPhone. Install it with
#   sh ios/EveLink/build.sh --install
set -eu
cd "$(dirname "$0")"
# Always bundle the current chat source, including in a fresh worktree.
# Fail before xcodegen if dependencies or the web build fail.
(cd Web && pnpm install --frozen-lockfile && pnpm build)
xcodegen generate --quiet
DERIVED="${DERIVED:-$PWD/build}"
xcodebuild -project EveLink.xcodeproj -scheme EveLink -destination 'generic/platform=iOS' \
  -derivedDataPath "$DERIVED" -allowProvisioningUpdates -quiet build
echo "built: $DERIVED/Build/Products/Debug-iphoneos/EveLink.app"
if [ "${1:-}" = "--install" ]; then
  DEVICE="${EVE_IPHONE:-$(xcrun devicectl list devices 2>/dev/null | awk '/physical/ && /iPhone/ && /available/ {for (i=1;i<=NF;i++) if ($i ~ /^[0-9A-F]{8}-[0-9A-F]{16}$/) print $i; exit}')}"
  [ -n "$DEVICE" ] || { echo "no paired iPhone available (unlock it, same network or cable)" >&2; exit 1; }
  xcrun devicectl device install app --device "$DEVICE" "$DERIVED/Build/Products/Debug-iphoneos/EveLink.app"
fi
