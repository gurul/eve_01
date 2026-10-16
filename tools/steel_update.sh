#!/bin/sh
# Keep eve's local Steel browser on the newest steel-browser (owner, 2026-10-13: "setup a hook for steel opensource
# repo like we did for openui"). The same shape as tools/openui_update.sh.
#
#   sh tools/steel_update.sh                 check now; update when steel-browser has new commits
#   sh tools/steel_update.sh --dry-run       say what would change, change nothing
#   sh tools/steel_update.sh --schedule      run it every night at 05:10 (a LaunchAgent), then exit
#   sh tools/steel_update.sh --unschedule    stop the nightly run
#
# The Steel checkout (EVE_STEEL_DIR, default ~/Documents/personal/steel-browser) runs eve's own branch `eve`: upstream
# plus eve's small patch (the reported session timeout and steel-local.sh, docs/steel.md). Each run:
#   1. fetches upstream and stops when `eve` already contains origin/main;
#   2. in a worktree of its own merges origin/main into a copy of `eve` (a merge, never a rebase), installs with
#      Homebrew's node@22, builds the API and smoke-tests it on a spare port (127.0.0.1:3100, DevTools 9334, a
#      throwaway profile): a session opens and the scrape route reads example.com;
#   3. all green: fast-forwards `eve`, rebuilds the live checkout and stops the running server, so eve starts the
#      new build on its next web task (steel_local.ensure_server); also updates the steel CLI;
#   4. anything fails: nothing live changes; eve says what failed (ntfy).
# It never pushes. Log: ~/Library/Logs/eve-steel-update.log.
set -eu

EVE="$(cd "$(dirname "$0")/.." && pwd)"
STEEL="${EVE_STEEL_DIR:-$HOME/Documents/personal/steel-browser}"
WT="$(dirname "$STEEL")/steel-browser-update"
LABEL="com.github.eve-bridge.steel-update"
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
LOG="$HOME/Library/Logs/eve-steel-update.log"
PY="$EVE/bridge/.venv/bin/python"
export PATH="/opt/homebrew/opt/node@22/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"

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
    # launchd started as /bin/sh may not read ~/Documents ("Operation not permitted", 2026-10-16: no run had ever
    # worked). Started through the venv's python, as the daemon's agent is, the shell it runs can (docs/refresh.md).
    mkdir -p "$(dirname "$PLIST")"
    cat >"$PLIST" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>$LABEL</string>
  <key>ProgramArguments</key><array><string>$PY</string><string>-c</string><string>import subprocess, sys; sys.exit(subprocess.call(["/bin/sh", "$EVE/tools/steel_update.sh"]))</string></array>
  <key>StartCalendarInterval</key><dict><key>Hour</key><integer>5</integer><key>Minute</key><integer>10</integer></dict>
  <key>StandardOutPath</key><string>$LOG</string>
  <key>StandardErrorPath</key><string>$LOG</string>
</dict></plist>
EOF
    launchctl bootout "gui/$(id -u)/$LABEL" >/dev/null 2>&1 || true
    launchctl bootstrap "gui/$(id -u)" "$PLIST"
    echo "scheduled: every night at 05:10 ($PLIST); log $LOG"
    exit 0 ;;
  --unschedule)
    launchctl bootout "gui/$(id -u)/$LABEL" >/dev/null 2>&1 || true
    rm -f "$PLIST"
    echo "unscheduled"
    exit 0 ;;
esac
DRY=0
[ "${1:-}" = "--dry-run" ] && DRY=1

[ -d "$STEEL/.git" ] || { say "no steel-browser checkout at $STEEL"; exit 1; }
[ "$(git -C "$STEEL" branch --show-current)" = "eve" ] || { say "$STEEL is not on branch eve; leaving it alone"; exit 1; }
[ -z "$(git -C "$STEEL" status --porcelain --untracked-files=no)" ] || { say "$STEEL has local edits; leaving it alone"; exit 1; }

# 1. Anything new upstream?
git -C "$STEEL" fetch -q origin main || { say "could not fetch steel-browser; trying again tomorrow"; exit 0; }
new="$(git -C "$STEEL" rev-list --count eve..origin/main)"
if [ "$new" = "0" ]; then say "steel-browser is up to date"; exit 0; fi
say "steel-browser has $new new upstream commit(s): $(git -C "$STEEL" log --oneline -1 origin/main)"
if [ "$DRY" = 1 ]; then say "dry run: would merge $new commit(s) into eve"; exit 0; fi

fail() {
  say "FAILED: $1 (worktree $WT kept; the running Steel is unchanged)"
  tell "Steel update ($new new commits) did not pass: $1. eve's browser is unchanged."
  exit 1
}

# 2. Merge and test in a worktree of its own.
if [ -e "$WT/.git" ]; then
  git -C "$WT" checkout -q -B eve-update eve && git -C "$WT" reset -q --hard
else
  git -C "$STEEL" worktree add -q -B eve-update "$WT" eve
fi
cd "$WT"
git -c user.name="eve" -c user.email="eve@localhost" merge -q --no-edit origin/main >/dev/null 2>&1 \
  || { git merge --abort >/dev/null 2>&1 || true; fail "the merge conflicts with eve's patch"; }
npm ci --no-audit --no-fund >/dev/null 2>&1 || fail "npm ci"
(cd api && npm run build >/dev/null 2>&1) || fail "the API build"
profile="$(mktemp -d "${TMPDIR:-/tmp}/steel-smoke.XXXX")"
(cd api && HOST=127.0.0.1 PORT=3100 CHROME_USER_DATA_DIR="$profile" FILTER_CHROME_ARGS="--remote-debugging-port=9222" \
  CHROME_ARGS="--remote-debugging-port=9334" node ./build/index.js >"$profile/server.log" 2>&1) &
server=$!
ok=0
for _ in $(seq 1 40); do curl -s -m 2 http://127.0.0.1:3100/v1/health | grep -q ok && { ok=1; break; }; sleep 1; done
if [ "$ok" = 1 ]; then
  curl -s -m 30 -X POST http://127.0.0.1:3100/v1/sessions -H 'content-type: application/json' -d '{}' \
    | grep -q '"websocketUrl"' || ok=0
  curl -s -m 60 -X POST http://127.0.0.1:3100/v1/scrape -H 'content-type: application/json' \
    -d '{"url":"https://example.com","format":["markdown"]}' | grep -q "Example Domain" || ok=0
fi
kill -INT "$server" 2>/dev/null || true
sleep 2
pkill -f "remote-debugging-port=9334" 2>/dev/null || true
rm -rf "$profile"
[ "$ok" = 1 ] || fail "the smoke test (session and scrape on a spare port)"

# 3. Live: fast-forward eve's branch, rebuild, and let eve start the new build on its next web task.
git -C "$STEEL" merge -q --ff-only eve-update || fail "fast-forwarding $STEEL"
(cd "$STEEL" && npm ci --no-audit --no-fund >/dev/null 2>&1 && cd api && npm run build >/dev/null 2>&1) \
  || fail "rebuilding $STEEL"
pid="$(lsof -tnP -iTCP:3000 -sTCP:LISTEN 2>/dev/null | head -1 || true)"
if [ -n "$pid" ]; then
  kill -INT "$pid" 2>/dev/null || true                    # SIGTERM alone did not stop it (docs/steel.md)
  sleep 3
  pkill -f "remote-debugging-port=9333" 2>/dev/null || true
fi
"$HOME/.steel/bin/steel" update >/dev/null 2>&1 || true
say "updated steel-browser to $(git -C "$STEEL" log --oneline -1); the next web task starts it"
tell "eve's browser (Steel) is updated to the newest steel-browser ($new new commits)."
