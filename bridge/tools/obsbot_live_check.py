"""The OBSBOT camera through the live daemon, over the same HTTP door Eve Link uses (GATES.md G6).

Reads the link token from ~/.config/eve-bridge/stick-link.json (never printed), finds the daemon's link port among
its listening sockets, then: the controls list has the camera; a status read names it; a move reads back within 3°;
center reads back near 0; the picture streams at least one whole JPEG, and a WebRTC viewer gets a steady picture
(gap p95 under 200 ms) while that stream viewer keeps getting frames. Prints OBSBOT_LIVE_OK only when all hold.
The camera moves.
"""

from __future__ import annotations

import asyncio
import http.client
import json
import os
import re
import subprocess
import sys
import time
from pathlib import Path

TOKEN_PATH = Path(os.environ.get("EVE_BRIDGE_CONFIG_DIR", "~/.config/eve-bridge")).expanduser() / "stick-link.json"


def fail(why: str) -> None:
    print(f"FAIL: {why}")
    sys.exit(1)


def daemon_ports() -> list[int]:
    pid = subprocess.run(["pgrep", "-f", "eve_bridge.cli daemon"], capture_output=True, text=True).stdout.split()
    if not pid:
        fail("the eve daemon isn't running")
    out = subprocess.run(["lsof", "-nP", "-a", "-p", pid[0], "-iTCP", "-sTCP:LISTEN"], capture_output=True,
                         text=True).stdout
    return sorted({int(p) for p in re.findall(r"127\.0\.0\.1:(\d+) \(LISTEN\)", out)})


def call(port: int, token: str, method: str, path: str, body: dict | None = None, timeout: float = 40):
    c = http.client.HTTPConnection("127.0.0.1", port, timeout=timeout)
    data = json.dumps(body).encode() if body is not None else None
    headers = {"Authorization": f"Bearer {token}"}
    if data is not None:
        headers["Content-Type"] = "application/json"
    c.request(method, path, body=data, headers=headers)
    r = c.getresponse()
    return r.status, r.read(), c


def main() -> None:
    token = json.loads(TOKEN_PATH.read_text()).get("token", "")
    if not token:
        fail("no link token")
    port = None
    for p in daemon_ports():
        try:
            status, raw, _ = call(p, token, "GET", "/api/link/controls", timeout=5)
        except OSError:
            continue
        if status == 200 and b'"lights"' in raw:
            port = p
            break
    if port is None:
        fail("no daemon port answers /api/link/controls")
    listing = json.loads(raw)
    if listing.get("obsbot") is not True:
        fail(f"the controls list has no camera: {sorted(k for k, v in listing.items() if v)}")
    print(f"listing: obsbot true (port {port})")

    def panel(body: dict) -> dict:
        status, out, _ = call(port, token, "POST", "/api/link/controls/obsbot", body)
        if status != 200:
            fail(f"{body} -> HTTP {status}")
        return json.loads(out)

    st = panel({"action": "status"})
    if not st.get("ok") or not str(st.get("camera", "")).startswith("Tiny"):
        fail(f"status: {st}")
    print(f"status: {st['line']}")
    if st.get("awake") is False:
        w = panel({"action": "wake"})
        print(f"wake: {w['line']}")
    mv = panel({"action": "move", "pan": 25, "pitch": 5})
    if not mv.get("ok") or abs(mv["pan"] - 25) > 3 or abs(mv["pitch"] - 5) > 3:
        fail(f"move: {mv}")
    print(f"move: {mv['line']}")
    # hold-to-turn: three beats, then a stop; the pan must change
    before = panel({"action": "status"})["pan"]
    hold = f"live-check-{time.time_ns()}"            # a fresh id per press, as the app's: an ended id is refused
    for _ in range(4):
        panel({"action": "hold", "dir": "left", "hold": hold})
        time.sleep(0.15)
    panel({"action": "stop", "hold": hold})
    time.sleep(0.5)
    after = panel({"action": "status"})["pan"]
    if after == before:
        fail(f"hold left: pan stayed {before}")
    late = panel({"action": "hold", "dir": "left", "hold": hold})
    if not late.get("stopped"):
        fail(f"a late beat was not refused: {late}")
    print(f"hold: pan {before} -> {after}, late beat refused")
    ce = panel({"action": "center"})
    if not ce.get("ok") or abs(ce["pan"]) > 3 or abs(ce["pitch"]) > 3:
        fail(f"center: {ce}")
    print(f"center: {ce['line']} pan {ce['pan']} pitch {ce['pitch']}")

    c = http.client.HTTPConnection("127.0.0.1", port, timeout=15)
    c.request("GET", "/api/link/controls/obsbot/stream", headers={"Authorization": f"Bearer {token}"})
    r = c.getresponse()
    if r.status != 200:
        fail(f"stream: HTTP {r.status} {r.read()[:200]!r}")
    buf, start = b"", time.monotonic()
    while time.monotonic() - start < 10:
        buf += r.read1(65536) if hasattr(r, "read1") else r.read(4096)
        a = buf.find(b"\xff\xd8")
        z = buf.find(b"\xff\xd9", a + 2) if a >= 0 else -1
        if a >= 0 and z >= 0:
            print(f"stream: first JPEG {z + 2 - a} bytes after {time.monotonic() - start:.2f} s")
            break
    else:
        fail("stream: no whole JPEG in 10 s")
    # keep this viewer open: the WebRTC viewer below must share the camera with it, not take it
    rtc = asyncio.run(rtc_viewer(port, token, 6.0, r))
    c.close()
    frames, p95, stream_frames = rtc
    if frames < 30 or p95 > 0.2:
        fail(f"webrtc: {frames} frames in 6 s, gap p95 {p95:.3f} s")
    if stream_frames < 20:
        fail(f"the stream viewer got {stream_frames} frames while WebRTC watched: the viewers fought")
    print(f"webrtc: {frames} frames in 6 s, gap p95 {p95 * 1000:.0f} ms; the stream viewer got {stream_frames} "
          "at the same time")
    panel({"action": "close"})
    print("OBSBOT_LIVE_OK")


async def rtc_viewer(port: int, token: str, secs: float, stream) -> tuple[int, float, int]:
    """A WebRTC viewer as the app makes one, while ``stream`` (an open tunnel-format response) keeps reading."""
    from aiortc import RTCPeerConnection, RTCSessionDescription

    from eve_bridge import pika

    pc = RTCPeerConnection()
    ch = pc.createDataChannel("video", ordered=False, maxPacketLifeTime=150)
    arrivals: list[float] = []
    ch.on("message", lambda m: arrivals.append(time.monotonic()) if isinstance(m, bytes) else None)
    await pc.setLocalDescription(await pc.createOffer())
    status, raw, _ = await asyncio.to_thread(call, port, token, "POST", "/api/link/controls/obsbot",
                                             {"action": "rtc", "sdp": pc.localDescription.sdp})
    answer = json.loads(raw)
    if status != 200 or not answer.get("ok"):
        fail(f"webrtc offer: HTTP {status} {answer}")
    await pc.setRemoteDescription(RTCSessionDescription(sdp=answer["sdp"], type="answer"))

    def read_stream() -> int:
        n, buf, end = 0, b"", time.monotonic() + secs
        while time.monotonic() < end:
            data = stream.read1(65536)
            if not data:
                break
            jpegs, buf = pika.split_jpegs(buf + data)
            n += len(jpegs)
        return n

    stream_frames = await asyncio.to_thread(read_stream)
    await pc.close()
    gaps = sorted(b - a for a, b in zip(arrivals[2:], arrivals[3:], strict=False))   # after the first frames settle
    p95 = gaps[int(len(gaps) * 0.95)] if gaps else 9.9
    return len(arrivals), p95, stream_frames


if __name__ == "__main__":
    main()
