#!/usr/bin/env bash
# Keeps the Pi on the network for eve (docs/muse.md, "Keeping the Pi on"). Run every minute by its timer:
# a ping to the router keeps the Pi known on the LAN; three misses in a row reconnect Wi-Fi, and if that does
# not bring the router back, NetworkManager restarts.
set -uo pipefail
STATE=/run/eve-wifi-keepalive.misses
gw="$(ip route | awk '/^default/ {print $3; exit}')"
if [ -n "$gw" ] && ping -c2 -W3 "$gw" >/dev/null 2>&1; then
    rm -f "$STATE"
    exit 0
fi
misses=$(( $(cat "$STATE" 2>/dev/null || echo 0) + 1 ))
echo "$misses" > "$STATE"
echo "router unreachable (${gw:-no route}), miss $misses"
[ "$misses" -ge 3 ] || exit 0
conn="$(nmcli -t -f NAME,TYPE connection show --active | awk -F: '$2 == "802-11-wireless" {print $1; exit}')"
conn="${conn:-$(nmcli -t -f NAME,TYPE connection show | awk -F: '$2 == "802-11-wireless" {print $1; exit}')}"
echo "reconnecting Wi-Fi (${conn:-unknown})"
/usr/sbin/iw dev wlan0 set power_save off 2>/dev/null || true
[ -n "$conn" ] && nmcli connection up "$conn" >/dev/null 2>&1
sleep 10
gw="$(ip route | awk '/^default/ {print $3; exit}')"
if [ -z "$gw" ] || ! ping -c2 -W3 "$gw" >/dev/null 2>&1; then
    echo "still unreachable; restarting NetworkManager"
    systemctl restart NetworkManager
fi
rm -f "$STATE"
