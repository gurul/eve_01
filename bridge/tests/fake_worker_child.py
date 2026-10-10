"""A stand-in for desktop_worker.py's line protocol, spawned by test_worker_client.py.

Not a test module (no test_ prefix). Prints a ready line, then answers one
JSON request per stdin line: observe → an image plus the context line; code
"hang" → sleeps 100 s; "exit" → writes boom to stderr and exits 3; "big" → a
100 KB text item; "corner" → the fail-safe error; anything else → "ran <code>". Every reply
carries a "timing" dict; a lane_first request answers a fixed no-match route, or
hangs (never replies) when the goal is "hang".
"""

import json
import sys
import time

sys.stdout.write(json.dumps({"ready": True, "width": 100, "height": 50}) + "\n")
sys.stdout.flush()
for line in sys.stdin:
    req = json.loads(line)
    rid = req.get("id")
    if req.get("operation") == "observe":
        out = [{"type": "input_image", "detail": "original", "image_url": "data:image/png;base64,AAAA"},
               {"type": "input_text", "text": "frontmost: Warp — 'zsh'; screen 100x50; 14:02"}]
        reply = {"id": rid, "output": out, "timing": {"capture": 1.0, "exec": 2.0}}
    elif req.get("operation") == "lane_first":
        if req.get("goal") == "hang":
            continue                                    # never answers: the client must time out
        if req.get("goal") == "slow":
            time.sleep(0.6)                             # answers late: the client must skip it as stale
        reply = {"id": rid, "lane_first": {"status": "none", "reason": "no_match"}}
    else:
        code = req.get("code", "")
        if code == "hang":
            time.sleep(100)
            continue
        if code == "exit":
            sys.stderr.write("boom\n")
            sys.stderr.flush()
            sys.exit(3)
        if code == "big":
            reply = {"id": rid, "output": [{"type": "input_text", "text": "x" * (100 * 1024)}]}
        elif code == "corner":
            reply = {"id": rid, "error": {"code": "failsafe", "message": "desktop fail-safe: the mouse hit a screen corner"}}
        else:
            reply = {"id": rid, "output": [{"type": "input_text", "text": f"ran {code}"}],
                     "timing": {"exec": 1.0}}
    sys.stdout.write(json.dumps(reply) + "\n")
    sys.stdout.flush()
