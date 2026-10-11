"""The camera through eve's live chat brain, over Eve Link's chat door (GATES.md G4). The camera moves, and two
messages land in the owner's app chat.

1. "camera left" goes to POST /api/link/turn: the daemon's log must show the code path ("obsbot: by code"), and the
   camera's pan, read over USB, must go up (left) by about the code path's turn.
2. A request in other words goes the same way: the model must move the camera to the right, read back the same way.
3. The camera is centered again.

Prints OBSBOT_CHAT_OK only when all of it holds.
"""

from __future__ import annotations

import http.client
import json
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "src"))
sys.path.insert(0, str(Path(__file__).resolve().parent))

import obsbot_live_check as live  # noqa: E402

from eve_bridge import obsbot_panel  # noqa: E402
from eve_bridge.obsbot_usb import UsbCamera  # noqa: E402

LOG = Path("~/Library/Logs/eve-bridge.log").expanduser()
FREE_FORM = "could you swing the webcam roughly 30 degrees to the right of where it's aimed now"


def fail(why: str) -> None:
    print(f"FAIL: {why}")
    sys.exit(1)


def turn(port: int, token: str, text: str) -> str:
    """One app turn; eve's words from the SSE stream."""
    c = http.client.HTTPConnection("127.0.0.1", port, timeout=120)
    body = json.dumps({"text": text, "client_id": f"camcheck{time.time_ns()}"})
    c.request("POST", "/api/link/turn", body=body, headers={"Authorization": f"Bearer {token}",
                                                            "Content-Type": "application/json"})
    r = c.getresponse()
    if r.status != 200:
        fail(f"{text!r}: HTTP {r.status} {r.read()[:200]!r}")
    words = []
    for raw in r:
        line = raw.decode("utf-8", "replace").strip()
        if not line.startswith("data:"):
            continue
        data = line[5:].strip()
        if data == "[DONE]":
            break
        try:
            chunk = json.loads(data)
            words.append(chunk["choices"][0]["delta"].get("content") or "")
        except (ValueError, KeyError, IndexError):
            continue
    c.close()
    return "".join(words).strip()


def log_since(offset: int) -> str:
    with LOG.open("rb") as f:
        f.seek(offset)
        return f.read().decode("utf-8", "replace")


def main() -> None:
    token = json.loads(live.TOKEN_PATH.read_text())["token"]
    port = None
    for p in live.daemon_ports():
        try:
            status, raw, _ = live.call(p, token, "GET", "/api/link/controls", timeout=5)
        except OSError:
            continue
        if status == 200 and b'"obsbot": true' in raw.replace(b'"obsbot":true', b'"obsbot": true'):
            port = p
            break
    if port is None:
        fail("no daemon port lists the camera")
    cam = UsbCamera()
    if not cam.wake():
        fail(f"the camera can't be read or woken over USB: {cam.why}")
    cam.point(0, 0)
    time.sleep(2.5)
    start = cam.read()

    mark = LOG.stat().st_size
    said = turn(port, token, "camera left")
    time.sleep(0.5)
    after = cam.read()
    if "obsbot: by code, turn -> done" not in log_since(mark):
        fail(f"'camera left' was not done by code (eve said {said!r})")
    moved = after["pan"] - start["pan"]
    if not obsbot_panel.TURN_DEGREES - 4 <= moved <= obsbot_panel.TURN_DEGREES + 4:
        fail(f"'camera left' moved pan by {moved:.0f}° (eve said {said!r})")
    print(f"code path: 'camera left' -> pan {start['pan']:.0f}° -> {after['pan']:.0f}°; eve said: {said}")

    mark = LOG.stat().st_size
    said = turn(port, token, FREE_FORM)
    time.sleep(0.5)
    final = cam.read()
    if "obsbot: by code" in log_since(mark):
        fail("the free-form request was taken by the code path, not the model")
    moved = final["pan"] - after["pan"]
    if not -40 <= moved <= -20:
        fail(f"the model moved pan by {moved:.0f}°, not ~30° right (eve said {said!r})")
    print(f"the model: {FREE_FORM!r} -> pan {after['pan']:.0f}° -> {final['pan']:.0f}°; eve said: {said}")

    cam.point(0, 0)
    print("OBSBOT_CHAT_OK")


if __name__ == "__main__":
    main()
