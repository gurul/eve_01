"""Ask the Muse through eve's relay on this Pi and print the answer as JSON lines.

Reads the message from stdin. eve on the Mac runs this over SSH:

    ssh <pi> /opt/eve-muse/venv-python /opt/eve-muse/ask.py --session-id eve <<< "hello"

Each ``{"progress": "..."}`` line (Muse saying what it is doing) is printed as
it arrives; the last line is the answer, ``{"ok", "reply"}`` or
``{"ok": false, "error"}``.
"""

from __future__ import annotations

import argparse
import json
import socket
import sys

SOCKET = "/run/musegadget/musegadget.sock"


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--session-id", default=None)
    # Longest quiet spell on the socket; the relay itself gives up after 15 minutes.
    parser.add_argument("--timeout", type=float, default=960.0)
    args = parser.parse_args()
    message = sys.stdin.read()
    request = {"message": message, "wait": True, "stream": True}
    if args.session_id:
        request["session_id"] = args.session_id
    reply = None
    try:
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
            sock.settimeout(args.timeout)
            sock.connect(SOCKET)
            sock.sendall(json.dumps(request).encode() + b"\n")
            for line in sock.makefile("rb"):
                item = json.loads(line)
                if isinstance(item, dict) and "progress" in item and "ok" not in item:
                    print(json.dumps(item), flush=True)
                    continue
                reply = item
                break
        if not isinstance(reply, dict):
            reply = {"ok": False, "error": "could not reach the Muse relay: no answer"}
    except (OSError, ValueError) as exc:
        reply = {"ok": False, "error": f"could not reach the Muse relay: {exc}"}
    print(json.dumps(reply), flush=True)
    return 0 if reply.get("ok") else 1


if __name__ == "__main__":
    sys.exit(main())
