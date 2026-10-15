#!/usr/bin/env bash
# shellcheck disable=SC2016  # the stand-ins expand their variables when they run, not here
# Tests eve-wifi-keepalive.sh with stand-ins for ip, ping, nmcli, iw, systemctl and journalctl. Run: bash test_keepalive.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT
mkdir -p "$T/bin"
stub() { printf '#!/usr/bin/env bash\n%s\n' "$2" > "$T/bin/$1"; chmod +x "$T/bin/$1"; }
stub ip       'echo "default via 10.0.0.1 dev wlan0"'
stub ping     'echo "ping $*" >> "$T/log"; for a in "$@"; do last="$a"; done; [ ! -e "$T/down-$last" ]'
stub nmcli    'echo "nmcli $*" >> "$T/log"; [ "$1" = "-t" ] && echo "HOME:802-11-wireless"; true'
stub iw       'true'
stub sleep    'true'
stub systemctl 'echo "systemctl $*" >> "$T/log"'
stub journalctl 'printf "%s\n" "Accepted publickey for u from 10.0.0.35 port 50000 ssh2: ED25519 SHA256:x" "Accepted publickey for u from 10.0.0.35 port 50001 ssh2: ED25519 SHA256:x" "Accepted publickey for u from 10.0.0.77 port 1 ssh2: ED25519 SHA256:x"'
export T PATH="$T/bin:$PATH" EVE_KEEPALIVE_STATE="$T/state" EVE_KEEPALIVE_MCAST="$T/mcast"
run() { : > "$T/log"; bash "$HERE/eve-wifi-keepalive.sh" > "$T/out" 2>&1; }
fail() { echo "FAIL: $1"; cat "$T/out" "$T/log"; exit 1; }
reconnects() { grep -c "connection up" "$T/log"; }

# Healthy: the counter moves every minute -> no reconnect, and each SSH client gets one hello.
for n in 100 140 190 230 260; do echo "$n" > "$T/mcast"; run; [ "$(reconnects)" = 0 ] || fail "reconnected while healthy"; done
[ "$(grep -c 'ping -c1 -W1 10.0.0.35' "$T/log")" = 1 ] || fail "no single hello to 10.0.0.35"
grep -q 'ping -c1 -W1 10.0.0.77' "$T/log" || fail "no hello to 10.0.0.77"

# Broadcast stalls: counter stands still. Runs 1 and 2 still -> wait; run 3 still -> reconnect (positive control).
echo 260 > "$T/mcast"
run; [ "$(reconnects)" = 0 ] || fail "reconnected after 1 still minute"
run; [ "$(reconnects)" = 0 ] || fail "reconnected after 2 still minutes"
run; [ "$(reconnects)" = 1 ] || fail "did not reconnect after 3 still minutes"
grep -q "no broadcast" "$T/out" || fail "no reason logged"
# After the reconnect the count starts over.
run; [ "$(reconnects)" = 0 ] || fail "reconnected again straight after a reconnect"

# Router down 3 times -> reconnect, then NetworkManager restart while it stays down.
touch "$T/down-10.0.0.1"; rm -rf "$T/state"
run; run; [ "$(reconnects)" = 0 ] || fail "reconnected before 3 router misses"
run; [ "$(reconnects)" = 1 ] || fail "no reconnect after 3 router misses"
grep -q "systemctl restart NetworkManager" "$T/log" || fail "NetworkManager not restarted"

# No counter file (other hardware) -> no broadcast check, no crash.
rm -f "$T/down-10.0.0.1" "$T/mcast"; rm -rf "$T/state"
for _ in 1 2 3 4; do run || fail "exit $? without a counter"; done
[ "$(reconnects)" = 0 ] || fail "reconnected without a counter"
echo KEEPALIVE_TESTS_OK
