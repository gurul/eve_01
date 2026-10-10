# Pika: drive the robot car from Eve Link

Pika is the owner's ELEGOO robot car ([gurul/pika](https://github.com/gurul/pika)). With it set up, Eve Link's
**Apps** tab has a **Pika** row. It opens one screen: the car's live video fills it, and four arrows and a red
Stop sit over the video. Hold an arrow to drive. Lift your finger and the car stops.

The phone never talks to the car. Every press goes through the tunnel to the Mac daemon (`pika.py`), with the
link token, and the daemon drives the car on the home Wi-Fi.

## Setting it up

It is off by default. It needs both of these:

1. `EVE_PIKA=1` in the daemon's environment.
2. `~/.config/eve-bridge/pika.json`, which holds no secrets:

   ```json
   {"host": "elegoo-car.local"}
   ```

   `host` is the name the car repo's `tools/car.py` uses by default (mDNS). If mDNS is blocked, use the car's
   address instead, for example `"192.168.1.50"`. Optional keys, with their defaults: `port` (100, the camera
   board's command relay), `video_port` (81, its MJPEG stream), `speed` (120) and `turn_speed` (140), the same
   values as the car's `calibration.json`; `rtc` (true: the video over WebRTC, below; false: the tunnel stream
   only) and `stun` (`"stun:stun.cloudflare.com:3478"`; `""` for none, and then WebRTC works on the home Wi-Fi
   only).

Restart the daemon after either change. Without both, the Apps tab shows no Pika, and its paths answer 404.

## Safety

- **Hold to drive only.** While an arrow is held, the app sends a beat about every 150 ms. Each beat renews the
  move.
- **The car stops by itself.** Eve sends only the firmware's timed move (`N=2` with `T` = 450 ms). The UNO stops
  450 ms after the last frame it got, so if the Mac, the Wi-Fi or the daemon goes away, the car still stops. Eve
  never sends the untimed moves.
- **The daemon stops the car** on release, on Stop, when no beat came for 450 ms, when the screen closes or the
  app goes to the background, when the car's connection drops, and on any error. A stop is written to the car at
  once and is never queued behind another command.
- **A stopped press stays stopped.** Each press has its own id. A late beat of a press that already ended (the
  phone's requests can cross in the tunnel) is refused, so it never starts the car again. Lift and press again.
- The camera board also stops the UNO when its client disconnects (its own firmware).

## One controller at a time

The car's camera board serves **one** command client and **one** video viewer.

- Eve takes the command slot on the first press and lets it go 5 seconds after the last command with nothing
  held, and at once when the screen closes.
- If something else has the car (the car repo's browser controller, for example), the screen says so in one line:
  "Pika is busy: another controller has it." Close the other one and press again.
- The video is relayed through the Mac to one viewer. Opening the screen again takes the video over, and closing
  the screen lets it go. Nothing reaches the car while the screen is closed: the car is usually off.

## The video: WebRTC, with the tunnel as the fallback

Every other Eve Link call crosses a Cloudflare tunnel, and so did the video at first. Measured from the Mac on
2026-10-10, the tunnel stream's first frame took 0.79 s (0.14 s from the camera direct), its frames arrived in
bursts (gap p95 564 ms against 209 ms), and a request's round trip through it was 145 ms median.

So the screen first tries **WebRTC** (`pika_rtc.py`, `App/PikaRTC.swift`). The app offers one data channel,
`video`, unordered with a 150 ms packet lifetime, so a lost frame is skipped rather than holding up the ones
behind it. The offer and the answer go through the link's own authenticated control (`{"action": "rtc"}`), with
every candidate in them (no trickle). On the home Wi-Fi the phone then reaches the Mac directly; off it, through
each side's public address (STUN). Nothing is relayed through a server. Each binary message is the Mac's send
stamp (8 bytes, big-endian ms) then the camera's JPEG exactly as it came, never re-encoded, so WebRTC adds no
encoder delay. A text message is a line for the owner (the camera is off or busy). The Mac drops frames, never
queues them, while the channel is behind.

When WebRTC can't connect within a few seconds (no `aiortc` on the Mac, `"rtc": false`, a network that blocks
it), the screen falls back to the tunnel stream, and tries WebRTC again whenever that stream ends. Both paths
share the camera's one viewer slot: the newest takes it.

Measuring it. Each frame carries the Mac's clock: WebRTC's 8-byte stamp, and on the tunnel stream an `X-Stamp`
header in each part. The phone subtracts it from its own clock on arrival and, every 10 s, posts a summary
(`{"action": "stats", "path": "webrtc"|"tunnel", "n", "p50", "p95", "fps"}`) that the daemon logs as
`pika: video stats ...`. The two clocks differ, but by the same offset on both paths, so the difference between
the paths is exact.

## How it works

| Call | What it does |
|---|---|
| `POST /api/link/controls/pika` | `{"action": "drive", "dir": "forward"\|"back"\|"left"\|"right", "hold": "<id>"}` (a press or a beat), `{"action": "stop", "hold"}`, `{"action": "close"}`, `{"action": "status"}`. Answers `{"ok", "line", "stopped"?}`. Also `{"action": "rtc", "sdp": <offer>}` → `{"ok", "rtc", "sdp": <answer>}`, and `{"action": "stats", ...}` (above). |
| `GET /api/link/controls/pika/stream` | The fallback video as multipart JPEGs (`boundary=frame`), each part stamped `X-Stamp`, the newest frame only. 503 with one line when the camera is off or busy. |

Both need the link token and refuse any `Origin`, as every `/api/link/` path does. The protocol is ported from
the car repo into `pika.py` (eve does not import that checkout), and each part names its source file there.

Tests: `bridge/tests/test_pika.py` runs against a fake car and a fake camera on loopback: the exact frame bytes,
every stop, the idle release, the busy car, the video relay, and the auth on both paths.
`bridge/tests/test_pika_rtc.py` plays the app with a real aiortc peer: the frames arrive byte-for-byte with the
Mac's stamp, WebRTC and the tunnel share the one viewer slot, close ends the peer and the camera socket, a newer
offer replaces the older peer, and a camera that is off is one line on the channel.
