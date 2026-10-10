#!/usr/bin/env python3
"""Control the live Laya worker; --check exercises model -> USB -> eye ACKs; original sound behavior is preserved."""

import argparse
import json
import time
from pathlib import Path

from eve_bridge.ipc import make_transport


def request(evt="expressions", **kwargs):
    with make_transport().sync_connect(5) as sock:
        sock.sendall((json.dumps({"evt": evt, **kwargs}) + "\n").encode())
        result = json.loads(sock.makefile("rb").readline())
    if not result.get("ok"):
        raise RuntimeError(result)
    return result


def wait_for(predicate, seconds=20):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        result = request()
        if predicate(result):
            return result
        time.sleep(0.15)
    raise AssertionError("timed out waiting for live device condition: " + json.dumps(result))


def check():
    request(action="on")
    wait_for(lambda r: r.get("connected") and r["expressions"]["ready"], 60)
    muted = request("sound", action="status")["sound"] == "off"
    evidence = []
    for phase, expected, text in [
        ("speaking", "happy", "I had a really nice day and everything went well."),
        ("speaking", "sad", "My dog died yesterday and I miss him terribly."),
        ("speaking", "skeptical", "Are you sure that's true? Those numbers don't look right."),
        ("speaking", "excited", "I GOT INTO MY DREAM UNIVERSITY! I CAN'T WAIT!"),
        ("speaking", "wink", "Give me a wink, Eve!"),
        ("listening", "affection", "I love having you around, Eve. You're my friend."),
    ]:
        event = request(action="audition", phase=phase, text=text)["id"]
        applied = phase != "listening"
        wait_for(
            lambda r, event=event, applied=applied: any(
                b["id"] == event and b["active"] and b["applied"] == applied
                for b in r["expressions"]["board_history"]
            )
        )
        result = wait_for(
            lambda r, event=event: any(
                b["id"] == event and not b["active"] for b in r["expressions"]["board_history"]
            )
        )
        rows = [b for b in result["expressions"]["board_history"] if b["id"] == event]
        assert result["expressions"]["last"]["id"] == event
        assert result["expressions"]["last"]["label"] == expected
        if expected == "wink":
            assert sum(bool(b.get("wink")) for b in rows) == 1
            assert any(b.get("wink_closed") for b in rows)
            closed_at = next(i for i, b in enumerate(rows) if b.get("wink_closed"))
            assert any(b["active"] and not b.get("wink_closed") for b in rows[closed_at + 1:])
        assert result["expressions"]["last"]["chirp_requested"] is False
        assert all(not b["chirp"] and b["muted"] == muted for b in rows)
        assert any(b["applied"] for b in rows) == applied
        evidence.append({"case": phase, "model": result["expressions"]["last"], "board": rows})
        time.sleep(2.5)  # finish the six-second audition
    assert (request("sound", action="status")["sound"] == "off") == muted
    path = Path(".test-artifacts/laya-live-device.json")
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(evidence, indent=2) + "\n")
    print(json.dumps(evidence, indent=2))
    print("LIVE_LAYA_DEVICE_OK")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "action", nargs="?", default="status", choices=["status", "on", "off", "react", "audition"]
    )
    parser.add_argument("text", nargs="?", default="")
    parser.add_argument("--phase", choices=["speaking", "listening", "idle"], default="speaking")
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    if args.check:
        check()
    else:
        print(json.dumps(request(action=args.action, text=args.text, phase=args.phase), indent=2))


if __name__ == "__main__":
    main()
