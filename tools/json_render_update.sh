#!/bin/sh
# Keep eve's cards on the newest json-render (owner, 2026-11-17: "setup an auto json render update", "whenever the
# git repo is updated our system should also be updated").
#
#   sh tools/json_render_update.sh                 check now; update when json-render has a newer release
#   sh tools/json_render_update.sh --dry-run       say what would change, change nothing
#   sh tools/json_render_update.sh --verify [X]    run every check for version X (default: the pinned one) in the
#                                                  worktree, then stop: no commit, no install, nothing live changes
#   sh tools/json_render_update.sh --schedule      run it every night at 04:50 (a LaunchAgent), then exit
#   sh tools/json_render_update.sh --unschedule    stop the nightly run
#
# "Updated" means a release: the newest vX.Y.Z tag of github.com/vercel-labs/json-render. The Swift package the phone
# draws cards with is pinned to that tag, and the npm packages of the same version give eve lite's web chat and the
# card rules the brain reads, so all of them move together or not at all:
#   1. The newest tag is compared with the pin in ios/EveLink/Packages/EveCards/Package.swift. npm must have
#      @json-render/core and @json-render/react at that version too, or the run waits for the next night.
#   2. In a worktree of its own (never the live checkout) every pin goes up, then: pnpm install, the card rules, the
#      Swift catalog and the parity cards are generated again, the web tests, the type check and the web build, the
#      Swift tests of EveCards and EveChat, the app's logic tests, the bridge's card tests and an app build.
#   3. All green and the iPhone reachable: the app is installed from that worktree, then the live checkout
#      fast-forwards to the commit and the daemon restarts, so the phone and the Mac change together.
#   4. Anything fails: nothing live changes; the branch is kept for a look. eve says which way it went (ntfy).
# It never pushes. Log: ~/Library/Logs/eve-json-render-update.log.
set -eu

LIVE="$(cd "$(dirname "$0")/.." && pwd)"
WT="${EVE_JSON_RENDER_WORKTREE:-$(dirname "$LIVE")/eve-json-render-update}"
BRANCH="json-render-update"
REPO="https://github.com/vercel-labs/json-render"
PKGS="@json-render/core @json-render/react"
LABEL="com.github.eve-bridge.json-render-update"
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
LOG="$HOME/Library/Logs/eve-json-render-update.log"
PY="${EVE_PYTHON:-$LIVE/bridge/.venv/bin/python}"
export PATH="/opt/homebrew/bin:/usr/local/bin:$PATH"

say() { printf '%s %s\n' "$(date '+%Y-%m-%d %H:%M:%S')" "$*"; }
tell() {   # one line to the owner's phone through eve's own notifications (notify.py); never fails the run
  "$PY" - "$1" <<'EOF' >/dev/null 2>&1 || true
import sys
from eve_bridge import notify
token = notify.read_token()
if token:
    notify.post_once(token, sys.argv[1], notify.configured().base)
EOF
}

case "${1:-}" in
  --schedule)
    # launchd started as /bin/sh may not read ~/Documents ("Operation not permitted"). Started through the venv's
    # python, as the daemon's agent is, the shell it runs can (docs/refresh.md).
    mkdir -p "$(dirname "$PLIST")"
    cat >"$PLIST" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>$LABEL</string>
  <key>ProgramArguments</key><array><string>$PY</string><string>-c</string><string>import subprocess, sys; sys.exit(subprocess.call(["/bin/sh", "$LIVE/tools/json_render_update.sh"]))</string></array>
  <key>StartCalendarInterval</key><dict><key>Hour</key><integer>4</integer><key>Minute</key><integer>50</integer></dict>
  <key>StandardOutPath</key><string>$LOG</string>
  <key>StandardErrorPath</key><string>$LOG</string>
</dict></plist>
EOF
    launchctl bootout "gui/$(id -u)/$LABEL" >/dev/null 2>&1 || true
    launchctl bootstrap "gui/$(id -u)" "$PLIST"
    echo "scheduled: every night at 04:50 ($PLIST); log $LOG"
    exit 0 ;;
  --unschedule)
    launchctl bootout "gui/$(id -u)/$LABEL" >/dev/null 2>&1 || true
    rm -f "$PLIST"
    echo "unscheduled"
    exit 0 ;;
esac
DRY=0; VERIFY=0
[ "${1:-}" = "--dry-run" ] && DRY=1
[ "${1:-}" = "--verify" ] && VERIFY=1

# 1. What is pinned, what is newest.
have="$(sed -n 's|.*vercel-labs/json-render", exact: "\([0-9.]*\)".*|\1|p' "$LIVE/ios/EveLink/Packages/EveCards/Package.swift" | head -1)"
[ -n "$have" ] || { say "FAILED: no json-render pin found in EveCards/Package.swift"; exit 1; }
if [ "$VERIFY" = 1 ]; then
  want="${2:-$have}"
  say "verify: every check for json-render $want (pinned: $have); nothing will be committed or installed"
else
  want="$(git ls-remote --tags --refs "$REPO" 2>/dev/null \
    | sed -n 's|.*refs/tags/v\([0-9][0-9]*\.[0-9][0-9]*\.[0-9][0-9]*\)$|\1|p' | sort -V | tail -1)"
  [ -n "$want" ] || { say "GitHub did not answer for json-render's tags; trying again tomorrow"; exit 0; }
  if [ "$have" = "$want" ] || [ "$(printf '%s\n%s\n' "$have" "$want" | sort -V | tail -1)" != "$want" ]; then
    say "json-render is up to date ($have)"; exit 0
  fi
  say "json-render: $have -> $want"
  for p in $PKGS; do
    if [ "$(npm view "$p@$want" version 2>/dev/null || true)" != "$want" ]; then
      say "npm does not have $p@$want yet; trying again tomorrow"; exit 0
    fi
  done
  if [ "$DRY" = 1 ]; then say "dry run: would update json-render $have -> $want"; exit 0; fi
fi

# 2. A worktree of its own, at the live checkout's commit.
if [ -d "$WT/.git" ] || [ -f "$WT/.git" ]; then
  git -C "$WT" checkout -q -B "$BRANCH" "$(git -C "$LIVE" rev-parse HEAD)"
  git -C "$WT" reset -q --hard
else
  git -C "$LIVE" worktree add -q -B "$BRANCH" "$WT" HEAD
fi
STEP="$(mktemp)"
trap 'rm -f "$STEP"' EXIT HUP INT TERM
fail() {
  say "FAILED: $1 (branch $BRANCH in $WT kept; nothing live changed)"
  [ -s "$STEP" ] && tail -15 "$STEP" | sed 's/^/    /'
  [ "$VERIFY" = 1 ] || tell "The json-render update to $want did not pass: $1. Nothing changed on your phone or Mac."
  exit 1
}
step() {   # step "<what it is>" command...: run it quietly; on a failure say which step and show its last lines
  what="$1"; shift
  "$@" >"$STEP" 2>&1 || fail "$what"
}

cd "$WT"
for f in ios/EveLink/Packages/EveCards/Package.swift ios/EveLink/Packages/EveChat/Package.swift; do
  sed -i '' "s|\(vercel-labs/json-render\", exact: \"\)[0-9.]*\"|\1$want\"|" "$f"
  grep -q "vercel-labs/json-render\", exact: \"$want\"" "$f" || fail "setting the pin in $f"
done
sed -i '' "s|(pinned to [0-9][0-9.]*)|(pinned to $want)|" docs/app-chat.md
cd "$WT/ios/EveLink/Web"
# The npm pins, and pnpm's minimum release age: a release younger than that age installs only when it is listed in
# pnpm-workspace.yaml's minimumReleaseAgeExclude, so the json-render entries there are written for this version.
node - "$want" $PKGS <<'EOF' >"$STEP" 2>&1 || fail "setting the npm pins"
const fs = require("fs");
const [version, ...names] = process.argv.slice(2);
const pkg = JSON.parse(fs.readFileSync("package.json", "utf8"));
for (const name of names) {
  if (!(name in pkg.dependencies)) throw new Error(`${name} is not a dependency`);
  pkg.dependencies[name] = version;
}
fs.writeFileSync("package.json", JSON.stringify(pkg, null, 2) + "\n");
const lines = fs.readFileSync("pnpm-workspace.yaml", "utf8").split("\n").filter((l) => !/^  - '@json-render\//.test(l));
const at = lines.indexOf("minimumReleaseAgeExclude:");
if (at < 0) throw new Error("no minimumReleaseAgeExclude list");
lines.splice(at + 1, 0, ...names.map((name) => `  - '${name}@${version}'`));
fs.writeFileSync("pnpm-workspace.yaml", lines.join("\n"));
EOF
step "pnpm install" pnpm install --no-frozen-lockfile
step "gen-json-prompt (the card rules)" pnpm gen-json-prompt
step "gen-swift-catalog (the phone's catalog)" pnpm gen-swift-catalog
step "gen-card-fixtures (the parity cards)" pnpm gen-card-fixtures
step "the web tests" pnpm test
step "the type check" pnpm exec tsc --noEmit
step "the web build" pnpm build:web
for p in EveCards EveChat; do
  cd "$WT/ios/EveLink/Packages/$p"
  step "resolving json-render $want for $p" swift package resolve
  step "the Swift tests of $p" swift test
done
cd "$WT"
step "the app's tests" sh ios/EveLink/Tests/run.sh
cd "$WT/bridge"
step "the bridge's card tests" env PYTHONPATH="$WT/bridge/src" HOME="${TMPDIR:-/tmp}" "$PY" -m pytest -q -p no:cacheprovider \
  tests/test_link_chat.py tests/test_card_engine.py tests/test_link_topics.py
cd "$WT"
step "the app build" sh ios/EveLink/build.sh
if [ "$VERIFY" = 1 ]; then
  say "verify: every check passed for json-render $want; nothing committed, nothing installed"
  exit 0
fi
git -C "$WT" add ios/EveLink/Web/package.json ios/EveLink/Web/pnpm-lock.yaml ios/EveLink/Web/pnpm-workspace.yaml \
  ios/EveLink/Packages/EveCards/Package.swift ios/EveLink/Packages/EveCards/Package.resolved \
  ios/EveLink/Packages/EveChat/Package.swift ios/EveLink/Packages/EveChat/Package.resolved \
  ios/EveLink/Packages/EveCards/Sources/EveCardCore/Resources/card-catalog.json \
  ios/EveLink/Packages/EveCards/Tests/EveCardCoreTests/Fixtures/card-parity.json \
  bridge/src/eve_bridge/json_render_prompt.txt docs/app-chat.md
git -C "$WT" commit -q -m "json-render: $have -> $want

Nightly update (tools/json_render_update.sh): the card rules, the Swift catalog
and the parity cards generated again; web tests, type check, web build, the
Swift tests of EveCards and EveChat, app tests, bridge card tests and the app
build passed."
say "committed $(git -C "$WT" rev-parse --short HEAD) on $BRANCH"

# 3. The phone and the Mac together.
if ! xcrun devicectl list devices 2>/dev/null | grep -q "available (paired)"; then
  say "the iPhone is not reachable; kept on $BRANCH for the next run"
  tell "json-render $want passed every check. Your iPhone wasn't reachable, so it will install on the next run."
  exit 0
fi
step "installing the app on the iPhone" sh ios/EveLink/build.sh --install
git -C "$LIVE" merge -q --ff-only "$BRANCH" >"$STEP" 2>&1 || fail "fast-forwarding the live checkout (it has new commits; merge by hand)"
launchctl kickstart -k "gui/$(id -u)/com.github.eve-bridge.daemon" >/dev/null 2>&1 || true
say "updated json-render $have -> $want: app installed, daemon restarted"
tell "eve's cards now run on json-render $want. App updated and eve restarted."
