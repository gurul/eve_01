#!/usr/bin/env bash
# Adds eve's Muse relay to a Pi that already runs Meta's musegadget service.
# Run on the Pi from this directory: bash install.sh ACCOUNT
# ACCOUNT is the SSH login eve uses; it joins the musegadget socket's group.
set -euo pipefail

ACCOUNT="${1:?usage: install.sh ACCOUNT}"
DEST=/opt/eve-muse
DROPIN=/etc/systemd/system/musegadget.service.d/eve-relay.conf
PY=/opt/musegadget/venv/bin/python

[ -x "$PY" ] || { echo "musegadget is not installed (no $PY)"; exit 1; }
RUN_AS="$(systemctl show -p Environment musegadget | sed -n 's/.*MUSEGADGET_RUN_AS=\([^ ]*\).*/\1/p')"
[ -n "$RUN_AS" ] || { echo "musegadget.service has no MUSEGADGET_RUN_AS"; exit 1; }

sudo install -d -m 0755 "$DEST"
sudo install -m 0644 eve_muse_relay.py ask.py "$DEST/"
sudo ln -sf "$PY" "$DEST/venv-python"

sudo install -d -m 0755 "$(dirname "$DROPIN")"
printf '%s\n' '[Service]' 'ExecStart=' \
    "ExecStart=$PY $DEST/eve_muse_relay.py" | sudo tee "$DROPIN" >/dev/null

# The socket is root:<RUN_AS group> 0660; the SSH account joins that group.
sudo usermod -aG "$RUN_AS" "$ACCOUNT"

sudo systemctl daemon-reload
sudo systemctl restart musegadget
echo "eve Muse relay installed; $ACCOUNT can ask through $DEST/ask.py"
