#!/usr/bin/env bash
# Keeps the Pi reachable for eve (docs/muse.md, "Keeping the Pi on"). Run every minute by its timer. Three checks:
#
# 1. The router answers a ping. Three misses in a row reconnect Wi-Fi; if that does not bring the router back,
#    NetworkManager restarts.
# 2. The Pi still hears broadcast traffic. The Pi's Wi-Fi can stop receiving broadcast and multicast frames while
#    unicast keeps working (seen 2026-10-15: 0 frames in 30 s on a busy LAN; 38 right after a reconnect). The Pi
#    then still reaches the router, but nothing can find it, because the Mac's "who has this address?" (ARP) is a
#    broadcast. So when the receive counter for multicast stands still for STALL_MINUTES runs, reconnect Wi-Fi.
# 3. The Pi says hello to the machines that SSH into it (the Mac): a ping each, so their ARP caches keep the Pi's
#    address and they can reach it even in the minutes before check 2 fires.
set -uo pipefail
IFACE="${EVE_KEEPALIVE_IFACE:-wlan0}"
STATE_DIR="${EVE_KEEPALIVE_STATE:-/run/eve-wifi-keepalive}"
MCAST="${EVE_KEEPALIVE_MCAST:-/sys/class/net/$IFACE/statistics/multicast}"
STALL_MINUTES="${EVE_KEEPALIVE_STALL_MINUTES:-3}"
PEER_DAYS="${EVE_KEEPALIVE_PEER_DAYS:-14}"
mkdir -p "$STATE_DIR"

reconnect() {
    local conn
    conn="$(nmcli -t -f NAME,TYPE connection show --active | awk -F: '$2 == "802-11-wireless" {print $1; exit}')"
    conn="${conn:-$(nmcli -t -f NAME,TYPE connection show | awk -F: '$2 == "802-11-wireless" {print $1; exit}')}"
    echo "reconnecting Wi-Fi (${conn:-unknown}): $1"
    iw dev "$IFACE" set power_save off 2>/dev/null || true
    [ -n "$conn" ] && nmcli connection up "$conn" >/dev/null 2>&1
    sleep 10
    iw dev "$IFACE" set power_save off 2>/dev/null || true
    rm -f "$STATE_DIR/misses" "$STATE_DIR/mcast"
}

router_ok() {
    local gw
    gw="$(ip route | awk '/^default/ {print $3; exit}')"
    [ -n "$gw" ] && ping -c2 -W3 "$gw" >/dev/null 2>&1
}

# 1. The router.
if router_ok; then
    rm -f "$STATE_DIR/misses"
else
    misses=$(( $(cat "$STATE_DIR/misses" 2>/dev/null || echo 0) + 1 ))
    echo "$misses" > "$STATE_DIR/misses"
    echo "router unreachable, miss $misses"
    if [ "$misses" -ge 3 ]; then
        reconnect "router unreachable 3 times"
        if ! router_ok; then
            echo "still unreachable; restarting NetworkManager"
            systemctl restart NetworkManager
        fi
    fi
    exit 0
fi

# 2. Broadcast reception. The state file holds "<counter> <runs it has stood still>".
now="$(cat "$MCAST" 2>/dev/null || echo)"
if [ -n "$now" ]; then
    last=""; still=0
    [ -f "$STATE_DIR/mcast" ] && read -r last still < "$STATE_DIR/mcast"
    if [ "$now" = "$last" ]; then still=$(( ${still:-0} + 1 )); else still=0; fi
    echo "$now $still" > "$STATE_DIR/mcast"
    if [ "$still" -ge "$STALL_MINUTES" ]; then
        reconnect "no broadcast or multicast received for $still minutes"
        exit 0
    fi
fi

# 3. Hello to recent SSH clients.
journalctl -u ssh -u sshd --since "-${PEER_DAYS} days" --grep "^Accepted " -o cat 2>/dev/null |
    sed -nE 's/^Accepted [^ ]+ for [^ ]+ from ([0-9]+\.[0-9]+\.[0-9]+\.[0-9]+) .*/\1/p' | sort -u |
    { while read -r peer; do ping -c1 -W1 "$peer" >/dev/null 2>&1 & done; wait; }
exit 0
