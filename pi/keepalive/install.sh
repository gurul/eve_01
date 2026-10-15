#!/usr/bin/env bash
# Keeps the Pi on: Wi-Fi power saving off (now and on every connect), a Wi-Fi keepalive every minute, and the
# hardware watchdog (a frozen Pi reboots itself). Run on the Pi from this directory: bash install.sh
set -euo pipefail
sudo install -m 0755 eve-wifi-keepalive.sh /usr/local/sbin/
sudo install -m 0644 eve-wifi-keepalive.service eve-wifi-keepalive.timer /etc/systemd/system/
conn="$(nmcli -t -f NAME,TYPE connection show --active | awk -F: '$2 == "802-11-wireless" {print $1; exit}')"
[ -z "$conn" ] || sudo nmcli connection modify "$conn" 802-11-wireless.powersave 2
sudo /usr/sbin/iw dev wlan0 set power_save off 2>/dev/null || true
sudo install -d /etc/systemd/system.conf.d
printf '%s\n' '[Manager]' 'RuntimeWatchdogSec=15s' 'RebootWatchdogSec=2min' |
    sudo tee /etc/systemd/system.conf.d/eve-watchdog.conf >/dev/null
sudo systemctl daemon-reexec
sudo systemctl daemon-reload
sudo systemctl enable --now eve-wifi-keepalive.timer
echo "keepalive on; watchdog $(systemctl show -p RuntimeWatchdogUSec --value)"
