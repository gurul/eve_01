# Muse

eve can hand your chat to Meta's **Muse**. While Muse is on, what you type goes
to Muse and its answers come back in the chat under the title **Muse**. Switch
back and you are talking to eve again.

Muse runs on your **Raspberry Pi**, not on the Mac. The Pi runs Meta's own
[Muse Linux Device SDK](https://github.com/facebookincubator/muse-gadget-sdk/tree/main/linux),
which keeps the Pi connected to Muse, and a small eve relay that returns Muse's
answers. eve on the Mac reaches the Pi over SSH.

> The Muse Gadget SDK is for personal tinkering. Meta says it can change, break
> or stop working without warning.

## Switching

- **In Eve Link**, the **eve / Muse** switch sits in the composer, left of the
  History button. It names who your next message goes to; tap it to switch.
  The composer then says "Ask Muse". The switch shows only when this Mac has
  Muse set up.
- **By words**, in any chat: `muse on` (or "switch to Muse", "talk to Muse")
  and `muse off` (or "back to eve", "switch to eve").

While Muse is on:

- `eve: ...` (or "hey eve ...") talks to eve for that one message.
- eve's own code words still reach eve: `stop`, `stealth`, `/apps`, `/spend`,
  `/watch` and a screenshot request.
- Photos and files go to eve. Muse gets text only.

Only one relay is on at a time. `muse on` turns off the Claude and Codex
relays; `claude on` or `codex <folder>` turns Muse off.

## Setup

You need a Raspberry Pi with Bluetooth (3B+, 4, 5 or Zero 2 W) on the same
network as the Mac, an SDK token from
[gadgets.muse.ai](https://gadgets.muse.ai/settings/sdk-tokens), and the Muse
app on your phone.

1. **SSH from the Mac to the Pi with a key.** On the Pi, turn SSH on
   (`sudo systemctl enable --now ssh`) and add the Mac's public key to
   `~/.ssh/authorized_keys`. Check it from the Mac:
   `ssh -o BatchMode=yes <user>@<pi> true`.

2. **Install Meta's SDK on the Pi.** Read `install.sh` first. Give Muse its own
   account without `sudo`: the SDK lets Muse run commands on the Pi as that
   account.

   ```bash
   sudo useradd -m -s /bin/bash muse
   bash install.sh --run-as muse --sdk-token mgst_…
   ```

3. **Pair.** In the Muse app, turn on **Settings > Devices > Developer mode**,
   then **Add Device** and pick the `MuseGadgetXXXXXX` name the installer
   printed. Accept the community-device warning; for Wi-Fi, pick the network
   shown. Run `sudo musegadget pair` to pair again later.

4. **Add eve's relay on the Pi.** Copy `pi/muse/` from this repository to the
   Pi and run, from that folder:

   ```bash
   bash install.sh <your SSH user>
   ```

   It copies the relay to `/opt/eve-muse`, makes the `musegadget` service start
   through it (a systemd drop-in), adds your SSH user to the service's socket
   group, and restarts the service.

5. **Point eve at the Pi.** In `~/.config/eve-bridge/env` on the Mac:

   ```bash
   EVE_MUSE_PI=<user>@<pi address>
   ```

   Restart the daemon. Then open Eve Link: the switch appears left of History.

6. **Keep the Pi on.** Copy `pi/keepalive/` to the Pi and run `bash install.sh`
   there. It turns Wi-Fi power saving off (a sleeping radio made the Pi
   unreachable from the Mac until the Pi itself sent something), pings the
   router every minute and reconnects Wi-Fi after three misses in a row, and
   turns on the hardware watchdog, so a frozen Pi reboots itself. The Muse
   service already starts at boot and restarts if it stops.

   Give the Pi a fixed address in your router (a DHCP reservation), because
   `EVE_MUSE_PI` names it by address.

## How it works

1. The Pi's `musegadget` service stays connected to your Muse (a Noise session
   to your Muse VM), across reboots.
2. eve's relay (`pi/muse/eve_muse_relay.py`) runs that service unchanged and
   adds one request to its local socket: send a message and wait for the
   answer. It follows Muse's chat events with the SDK's own `MuseTurn` and
   returns the reply text. It takes one question at a time and waits up to two
   minutes.
3. On the Mac, `muse_pi.py` runs
   `ssh <pi> /opt/eve-muse/venv-python /opt/eve-muse/ask.py --session-id <id>`
   with your message on stdin, and reads one JSON line back.
4. eve talks to Muse in one Muse side chat, its id kept in
   `~/.config/eve-bridge/muse-session`, so Muse remembers the conversation. You
   see that chat in the Muse app too.

## Settings and files

| Setting or file | Where | Meaning |
|---|---|---|
| `EVE_MUSE_PI` | Mac, `~/.config/eve-bridge/env` | `user@host` of the Pi. Unset: Muse is off and the switch is hidden. |
| `~/.config/eve-bridge/muse-session` | Mac | The id of eve's Muse side chat. Delete it to start a fresh chat. |
| `/var/lib/musegadget/` | Pi | Meta's SDK state: the SDK token, the device identity and the pairing (root only). |
| `/opt/eve-muse/` | Pi | eve's relay and `ask.py`. |
| `eve-wifi-keepalive.timer` | Pi | The Wi-Fi keepalive, every minute (`journalctl -u eve-wifi-keepalive` shows reconnects). |
| `/etc/systemd/system.conf.d/eve-watchdog.conf` | Pi | The hardware watchdog: 15 s. |
| `/etc/systemd/system/musegadget.service.d/eve-relay.conf` | Pi | Starts the service through eve's relay. Delete it and restart the service to undo. |

## Data

What you send while Muse is on, and Muse's answers, go to Meta's Muse service
through the Pi. eve keeps them in its transcript as relay lines; they are not
in eve's own model history. eve logs only lengths and outcomes, never your words.

## Checks

- `bash pi/muse/check_roundtrip.sh` asks Muse two questions through the Pi and
  checks the answers and that the second sees the first. It needs `EVE_MUSE_PI`
  set and posts two test messages to a fresh Muse side chat.
- `bridge/tests/test_muse_pi.py` covers the client, the switch words, one relay
  at a time and the app's `/api/link/muse`.
