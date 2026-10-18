"""Head calibration page: tap an extreme, watch eve go there, confirm the axis or flip it.

``eve-bridge head-cal`` serves one page on 127.0.0.1 and opens it. Each button holds the head at one
extreme (up, down, your left, your right) or at centre for a few seconds, through the daemon's ``move`` IPC.
Under each axis are two buttons: "looks right" and "flip". A flip goes to the board as
``{"cmd":"axis",...}`` (daemon ``axis`` IPC) and is saved there, so it survives reboots with no reflash.

Why it exists: on 2026-09-27 the pitch servo turned out to run opposite to the code's convention, and settling
that took hours of reading camera frames while the owner and the robot both moved. Two held poses and the
owner saying which one is the table settled it in a minute. This page is that minute, on demand.

Directions are the owner's, facing eve: "your left" is the code's +yaw (body.cpp: +yaw turns the head to the
robot's right, which is the viewer's left), "up" is pitch 85, "down" is pitch 5.
"""

from __future__ import annotations

import json
import sys
import threading
import webbrowser
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Any, Callable, Optional

from .hooks._client import post

HOLD_MS = 6000                     # each extreme is held this long (a keys move fits inside 8 s, motion.h)
POSES: dict[str, tuple[int, int]] = {
    "up": (0, 85),
    "down": (0, 5),
    "left": (90, 45),              # the owner's left, facing eve
    "right": (-90, 45),
    "center": (0, 45),
}

Ipc = Callable[[dict[str, Any]], Optional[dict[str, Any]]]


def pose_keys(name: str) -> list[list[int]]:
    """A keyframe move that goes to the pose and holds it for HOLD_MS."""
    yaw, pitch = POSES[name]
    return [[0, yaw, pitch, 300], [HOLD_MS // 2, yaw, pitch, 300], [HOLD_MS, yaw, pitch, 300]]


def handle(path: str, body: dict[str, Any], ipc: Ipc) -> tuple[int, dict[str, Any]]:
    """The page's API, kept apart from HTTP so a test can drive it with a fake daemon."""
    if path == "/api/move":
        name = str(body.get("pose") or "")
        if name not in POSES:
            return 400, {"ok": False, "error": f"unknown pose {name!r}"}
        resp = ipc({"evt": "move", "kind": "keys", "keys": pose_keys(name)})
        if resp is None:
            return 502, {"ok": False, "error": "the daemon is not reachable"}
        return 200, {"ok": bool(resp.get("ok")), "error": resp.get("error"), "pose": name}
    if path == "/api/axis":
        req: dict[str, Any] = {"evt": "axis"}
        for k in ("pitch_rev", "yaw_rev"):
            if k in body:
                req[k] = bool(body[k])
        resp = ipc(req)
        if resp is None:
            return 502, {"ok": False, "error": "the daemon is not reachable"}
        return 200, resp
    return 404, {"ok": False, "error": "not found"}


PAGE = """<!doctype html><html lang=en><meta charset=utf-8>
<meta name=viewport content="width=device-width,initial-scale=1">
<title>Head calibration</title>
<style>
:root{--bg:#f6f5f2;--fg:#1d1d1b;--mute:#6b6a65;--card:#fff;--line:#dedcd6;--ok:#1f7a4d;--warn:#b4532a;--acc:#2f5bd3}
@media (prefers-color-scheme:dark){:root{--bg:#161615;--fg:#eceae4;--mute:#9b9a93;--card:#1f1f1d;--line:#34332f;--ok:#4cc38a;--warn:#e58a5f;--acc:#7c9dff}}
*{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--fg);font:16px/1.45 system-ui,sans-serif}
main{max-width:560px;margin:0 auto;padding:24px 16px}h1{font-size:22px;margin:0 0 4px}p{color:var(--mute);margin:0 0 20px}
.card{background:var(--card);border:1px solid var(--line);border-radius:14px;padding:16px;margin-bottom:16px}
.card h2{font-size:15px;margin:0 0 12px;display:flex;justify-content:space-between}
.state{font-weight:500;color:var(--mute)}.state.rev{color:var(--warn)}
.row{display:grid;grid-template-columns:1fr 1fr;gap:10px}
button{font:inherit;padding:14px 10px;border-radius:10px;border:1px solid var(--line);background:var(--bg);color:var(--fg);cursor:pointer}
button:focus-visible{outline:2px solid var(--acc);outline-offset:2px}button:active{transform:scale(.98)}
button.go{font-size:18px;font-weight:600}button.ok{color:var(--ok)}button.flip{color:var(--warn)}
.act{margin-top:10px}#status{min-height:1.4em;color:var(--mute);text-align:center;margin-top:8px}
</style>
<main>
<h1>Head calibration</h1>
<p>Tap an extreme and watch eve. If it went the right way, tap "looks right". If it went the opposite way, tap "flip": it is saved on the robot.</p>
<section class=card><h2>Pitch <span id=pitch class=state>…</span></h2>
<div class=row><button class=go data-pose=up>Up (ceiling)</button><button class=go data-pose=down>Down (table)</button></div>
<div class="row act"><button class=ok data-ok=pitch>Looks right</button><button class=flip data-flip=pitch_rev>Flip pitch</button></div></section>
<section class=card><h2>Yaw <span id=yaw class=state>…</span></h2>
<div class=row><button class=go data-pose=left>Your left</button><button class=go data-pose=right>Your right</button></div>
<div class="row act"><button class=ok data-ok=yaw>Looks right</button><button class=flip data-flip=yaw_rev>Flip yaw</button></div></section>
<button class=go data-pose=center style="width:100%">Centre</button>
<div id=status role=status aria-live=polite></div>
</main>
<script>
const st=document.getElementById('status');let axis={};
async function api(p,b){const r=await fetch(p,{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(b||{})});return r.json()}
function show(a){if(!a||!a.ok){st.textContent=(a&&a.error)||'no answer';return}axis=a;
 for(const k of ['pitch','yaw']){const el=document.getElementById(k),r=a[k+'_rev'];el.textContent=r?'flipped':'as built';el.className='state'+(r?' rev':'')}}
api('/api/axis').then(show);
document.querySelectorAll('[data-pose]').forEach(b=>b.onclick=async()=>{st.textContent='Moving: '+b.textContent+'…';
 const r=await api('/api/move',{pose:b.dataset.pose});st.textContent=r.ok?'Holding '+b.textContent.toLowerCase()+' for 6 s':(r.error||'the robot refused the move')});
document.querySelectorAll('[data-flip]').forEach(b=>b.onclick=async()=>{const k=b.dataset.flip,body={};body[k]=!axis[k];
 const r=await api('/api/axis',body);show(r);if(r.ok)st.textContent=b.textContent+': saved on the robot. Tap the extreme again to check.'});
document.querySelectorAll('[data-ok]').forEach(b=>b.onclick=()=>{st.textContent=b.dataset.ok[0].toUpperCase()+b.dataset.ok.slice(1)+' confirmed.'});
</script></html>"""


def allowed(headers: Any, port: int) -> bool:
    """Only this page may call the API (learning/server.py's guard): the Host must be this server's, which stops a
    rebound DNS name, and an Origin, when the browser sends one, must be too, which stops another site's POST."""
    here = f"127.0.0.1:{port}"
    return headers.get("Host") == here and headers.get("Origin", f"http://{here}") == f"http://{here}"


def serve(port: int = 8766, socket_path: Optional[str] = None, open_browser: bool = True) -> int:
    def ipc(req: dict[str, Any]) -> Optional[dict[str, Any]]:
        return post(req, socket_path=socket_path, timeout=10.0)

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_a: Any) -> None:
            pass

        def _send(self, code: int, body: bytes, ctype: str) -> None:
            self.send_response(code)
            self.send_header("Content-Type", ctype)
            self.send_header("Cache-Control", "no-store")
            self.end_headers()
            self.wfile.write(body)

        def do_GET(self) -> None:  # noqa: N802
            if not allowed(self.headers, port):
                return self._send(403, b'{"ok": false, "error": "local access only"}', "application/json")
            self._send(200, PAGE.encode(), "text/html; charset=utf-8")

        def do_POST(self) -> None:  # noqa: N802
            if not allowed(self.headers, port):
                return self._send(403, b'{"ok": false, "error": "local access only"}', "application/json")
            n = int(self.headers.get("Content-Length") or 0)
            try:
                body = json.loads(self.rfile.read(n) or b"{}")
            except ValueError:
                body = {}
            code, out = handle(self.path, body if isinstance(body, dict) else {}, ipc)
            self._send(code, json.dumps(out).encode(), "application/json")

    server = ThreadingHTTPServer(("127.0.0.1", port), Handler)
    url = f"http://127.0.0.1:{port}/"
    print(f"head calibration at {url} (Ctrl-C to stop)")
    if open_browser:
        threading.Timer(0.3, lambda: webbrowser.open(url)).start()
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()
    return 0


if __name__ == "__main__":
    sys.exit(serve())
