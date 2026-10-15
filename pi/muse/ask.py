"""Ask the Muse through eve's relay on this Pi and print the answer as one JSON line.

Reads the message from stdin. eve on the Mac runs this over SSH:

    ssh <pi> /opt/eve-muse/venv-python /opt/eve-muse/ask.py --session-id eve <<< "hello"
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
    parser.add_argument("--timeout", type=float, default=150.0)
    args = parser.parse_args()
    message = sys.stdin.read()
    request = {"message": message, "wait": True}
    if args.session_id:
        request["session_id"] = args.session_id
    try:
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
            sock.settimeout(args.timeout)
            sock.connect(SOCKET)
            sock.sendall(json.dumps(request).encode() + b"\n")
            line = sock.makefile("rb").readline()
        reply = json.loads(line)
    except (OSError, ValueError) as exc:
        reply = {"ok": False, "error": f"could not reach the Muse relay: {exc}"}
    print(json.dumps(reply))
    return 0 if reply.get("ok") else 1


if __name__ == "__main__":
    sys.exit(main())
