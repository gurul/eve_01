#!/bin/sh
# Keep eve's generative UI on the newest OpenUI (owner, 2026-10-13: "make a hook so that whenever openui repo is
# updated so is our repo, so that we have best genui").
#
#   sh tools/openui_update.sh                 check now; update when OpenUI has a newer release
#   sh tools/openui_update.sh --dry-run       say what would change, change nothing
#   sh tools/openui_update.sh --schedule      run it every night at 04:50 (a LaunchAgent), then exit
#   sh tools/openui_update.sh --unschedule    stop the nightly run
#
# The card rules live on the Mac (bridge/src/eve_bridge/openui_prompt.txt) and the renderer in the phone app, so the
# two move together or not at all:
#   1. npm says whether @openuidev/react-lang, react-ui or react-headless has a newer version than Web/package.json pins.
#   2. In a worktree of its own (never the live checkout), the pins go up, then: pnpm install, the web tests, the type
#      check, gen-prompt + check-prompt, the app's logic tests, the bridge's chat tests and an app build.
#   3. All green and the iPhone reachable: the app is installed from that worktree, then the live checkout
#      fast-forwards to the commit and the daemon restarts, so the phone and the Mac change together.
#   4. Anything fails: nothing live changes; the branch is kept for a look. eve says which way it went (ntfy).
# It never pushes. Log: ~/Library/Logs/eve-openui-update.log.
set -eu

LIVE="$(cd "$(dirname "$0")/.." && pwd)"
WT="${EVE_OPENUI_WORKTREE:-$(dirname "$LIVE")/eve-openui-update}"
BRANCH="openui-update"
PKGS="@openuidev/react-lang @openuidev/react-ui @openuidev/react-headless"
LABEL="com.github.eve-bridge.openui-update"
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
LOG="$HOME/Library/Logs/eve-openui-update.log"
PY="$LIVE/bridge/.venv/bin/python"
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
    mkdir -p "$(dirname "$PLIST")"
    cat >"$PLIST" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>$LABEL</string>
  <key>ProgramArguments</key><array><string>/bin/sh</string><string>$LIVE/tools/openui_update.sh</string></array>
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
DRY=0
[ "${1:-}" = "--dry-run" ] && DRY=1

# 1. What is pinned, what is newest.
cd "$LIVE/ios/EveLink/Web"
changes=""
for p in $PKGS; do
  have="$(node -e "console.log(require('./package.json').dependencies['$p'] || '')")"
  want="$(npm view "$p" version 2>/dev/null || true)"
  [ -n "$want" ] || { say "npm did not answer for $p; trying again tomorrow"; exit 0; }
  if [ "$have" != "$want" ]; then changes="$changes $p@$want"; say "$p: $have -> $want"; fi
done
if [ -z "$changes" ]; then say "OpenUI is up to date"; exit 0; fi
if [ "$DRY" = 1 ]; then say "dry run: would update$changes"; exit 0; fi

# 2. A worktree of its own, at the live checkout's commit.
if [ -d "$WT/.git" ] || [ -f "$WT/.git" ]; then
  git -C "$WT" checkout -q -B "$BRANCH" "$(git -C "$LIVE" rev-parse HEAD)"
  git -C "$WT" reset -q --hard
else
  git -C "$LIVE" worktree add -q -B "$BRANCH" "$WT" HEAD
fi
fail() {
  say "FAILED: $1 (branch $BRANCH in $WT kept; nothing live changed)"
  tell "OpenUI update to$changes did not pass: $1. Nothing changed on your phone or Mac."
  exit 1
}
cd "$WT/ios/EveLink/Web"
for c in $changes; do
  name="${c%@*}"; ver="${c##*@}"
  node -e "const f='package.json',j=require('./'+f);j.dependencies['$name']='$ver';require('fs').writeFileSync(f,JSON.stringify(j,null,2)+'\n')"
done
pnpm install --no-frozen-lockfile >/dev/null 2>&1 || fail "pnpm install"
pnpm test >/dev/null 2>&1 || fail "the web tests"
pnpm exec tsc --noEmit >/dev/null 2>&1 || fail "the type check"
pnpm gen-prompt >/dev/null 2>&1 || fail "gen-prompt (the card rules)"
pnpm check-prompt >/dev/null 2>&1 || fail "check-prompt"
cd "$WT"
sh ios/EveLink/Tests/run.sh >/dev/null 2>&1 || fail "the app's tests"
( cd bridge && PYTHONPATH="$WT/bridge/src" HOME="${TMPDIR:-/tmp}" "$PY" -m pytest -q -p no:cacheprovider \
    tests/test_link_chat.py tests/test_link_topics.py >/dev/null 2>&1 ) || fail "the bridge's chat tests"
sh ios/EveLink/build.sh >/dev/null 2>&1 || fail "the app build"
git -C "$WT" checkout -q -- ios/EveLink/App/ChatWeb 2>/dev/null || true
git -C "$WT" add ios/EveLink/Web/package.json ios/EveLink/Web/pnpm-lock.yaml bridge/src/eve_bridge/openui_prompt.txt
git -C "$WT" commit -q -m "OpenUI:$changes

Nightly update (tools/openui_update.sh): web tests, type check, card rules
regenerated and checked, app tests, bridge chat tests and the app build passed."
say "committed $(git -C "$WT" rev-parse --short HEAD) on $BRANCH"

# 3. The phone and the Mac together.
if ! xcrun devicectl list devices 2>/dev/null | grep -q "available (paired)"; then
  say "the iPhone is not reachable; kept on $BRANCH for the next run"
  tell "OpenUI$changes passed every check. Your iPhone wasn't reachable, so it will install on the next run."
  exit 0
fi
sh ios/EveLink/build.sh --install >/dev/null 2>&1 || fail "installing the app on the iPhone"
git -C "$LIVE" merge -q --ff-only "$BRANCH" || fail "fast-forwarding the live checkout (it has new commits; merge by hand)"
launchctl kickstart -k "gui/$(id -u)/com.github.eve-bridge.daemon" >/dev/null 2>&1 || true
say "updated to$changes: app installed, daemon restarted"
tell "eve's cards now run on the newest OpenUI:$changes. App updated and eve restarted."
