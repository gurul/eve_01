"""Actual Decisions route with a file RPC to Codex's permitted Calculator UI connection.

Run with --rpc-dir; a CUA operator services request.json with screenshot/state or click.
No synthesized screenshots, fallback clicks, or changes to the production gates.
"""
from __future__ import annotations

import argparse
import json
import sys
import time
from dataclasses import replace
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "src"))

from PIL import Image, ImageChops  # noqa: E402

from eve_bridge import decisions  # noqa: E402
from eve_bridge.ax_candidates import ax_walk, snapshot_from_raw  # noqa: E402
from eve_bridge.decision_lane import route  # noqa: E402
from eve_bridge.envfile import load_env_file  # noqa: E402

CASES = [
    ("click seven", "7", False),
    ("click eight", "78", False),
    ("click Add", "78+", False),
    ("click two", "78+2", False),
    ("click Equals", "80", False),
    ("what is the result?", None, True),
    ("type hello", None, True),
    ("open Safari", None, True),
]


def display(state):
    import re

    values = re.findall(r"text Description: Edit field, Value: (.*)", state)
    return "".join(values).replace(" ", "").replace("\u202f", "").replace("\u00a0", "")


class Adapter:
    def __init__(self, rpc_dir, shots):
        self.rpc_dir, self.shots = rpc_dir, shots
        self.n = 0
        self.captures = []
        self.actions = []

    def rpc(self, kind, **data):
        self.n += 1
        request = {"id": self.n, "kind": kind, **data}
        tmp = self.rpc_dir / "pending.json"
        tmp.write_text(json.dumps(request))
        tmp.replace(self.rpc_dir / "request.json")
        until = time.monotonic() + 60
        while time.monotonic() < until:
            reply = self.rpc_dir / f"reply-{self.n}.json"
            if reply.exists():
                result = json.loads(reply.read_text())
                if result.get("error"):
                    raise RuntimeError(result["error"])
                return result
            time.sleep(.05)
        raise TimeoutError("CUA operator did not respond")

    def capture(self):
        result = self.rpc("capture")
        source = Path(result["image"])
        dest = self.shots / f"capture-{self.n:02d}.png"
        dest.write_bytes(source.read_bytes())
        row = {"image": str(dest), "state": result["state"], "display": display(result["state"])}
        self.captures.append(row)
        return row

    def observe(self):
        row = self.capture()
        from AppKit import NSWorkspace

        apps = [a for a in NSWorkspace.sharedWorkspace().runningApplications()
                if a.bundleIdentifier() == "com.apple.calculator"]
        if len(apps) != 1:
            raise RuntimeError("Calculator target unavailable")
        pid = apps[0].processIdentifier()
        raw = ax_walk(pid)
        if raw["bundle"] != "com.apple.calculator":
            raise RuntimeError("Calculator target changed")
        x, y, w, h = raw["root"]["frame"]
        snap = snapshot_from_raw(raw, seq=self.n)
        # The transport captures only this app; use real AX rectangles translated to app points.
        snap = replace(snap, elements=tuple(replace(c, frame=(round(c.frame[0]-x), round(c.frame[1]-y),
                                                               c.frame[2], c.frame[3])) for c in snap.elements),
                       _by_id={}, _refs={})
        image = Image.open(row["image"]).convert("RGB").resize((round(w), round(h)))
        return snap, image

    def frontmost_pid(self):
        from AppKit import NSWorkspace

        # CUA input is bound to this named app, rather than the global foreground window.
        apps = [a for a in NSWorkspace.sharedWorkspace().runningApplications()
                if a.bundleIdentifier() == "com.apple.calculator"]
        return apps[0].processIdentifier() if len(apps) == 1 else 0

    @staticmethod
    def image_matches(before, after):
        box = (0, 32, before.width, before.height)
        return before.size == after.size and ImageChops.difference(before.crop(box), after.crop(box)).getbbox() is None

    def click_candidate(self, candidate):
        result = self.rpc("click", label=candidate.label, role=candidate.role)
        self.actions.append({"label": candidate.label, **result})
        return result.get("result", "clicked")

    @staticmethod
    def settle(seconds):
        # CUA's next observation waits for the UI to settle.
        pass


def classification(result, before, after, expected, no_action):
    if no_action:
        return "safe_handoff" if not result["clicked"] and before == after and result["status"] == "none" else "failed"
    if result["status"] == "complete" and after == expected:
        return "verified_success"
    if after == expected and result["clicked"]:
        return "observed_success_unverified"
    return "failed_or_abstained"


def check_report(path):
    data = json.loads(path.read_text())
    assert data["backend"] == "real Decisions route; CUA capture/input; actual macOS AX"
    assert len(data["cases"]) == len(CASES)
    for row, (goal, expected, no_action) in zip(data["cases"], CASES, strict=True):
        assert row["goal"] == goal and row["expected"] == expected and row["no_action"] == no_action
        assert row["classification"] == classification(row["route"], row["before"]["display"],
                                                       row["after"]["display"], expected, no_action)
        for key in ("before", "after"):
            assert display(row[key]["state"]) == row[key]["display"]
            with Image.open(row[key]["image"]) as im:
                assert im.width > 100 and im.height > 100
        assert len(row["actions"]) == len(row["route"]["clicked"])
    print("LIVE_REPORT_CHECKED")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check-report", type=Path)
    parser.add_argument("--rpc-dir", type=Path)
    parser.add_argument("--out", type=Path)
    args = parser.parse_args()
    if args.check_report:
        check_report(args.check_report)
        return
    if not args.rpc_dir or not args.out:
        parser.error("--rpc-dir and --out are required")
    load_env_file()
    import os

    args.rpc_dir.mkdir(parents=True, exist_ok=True)
    client = decisions.Client(os.environ["OPENAI_API_KEY"])
    shots = args.out.parent / "decisions-live-demo"
    shots.mkdir(exist_ok=True)
    adapter = Adapter(args.rpc_dir, shots.resolve())
    report = {"backend": "real Decisions route; CUA capture/input; actual macOS AX", "cases": [],
              "limitation": "Direct daemon capture blocked by macOS Screen Recording. CUA binds input to Calculator; "
                            "target PID replaces global frontmost PID. One app; no performance claim."}
    try:
        initial = adapter.capture()
        assert initial["display"] == "0", "Calculator must start at zero"
        for goal, expected, no_action in CASES:
            before = adapter.capture()
            n = len(adapter.actions)
            result = route(goal, senses=adapter, effectors=adapter, client=client).to_dict()
            after = adapter.capture()
            row = {"goal": goal, "expected": expected, "no_action": no_action, "before": before, "after": after,
                   "route": result, "actions": adapter.actions[n:],
                   "classification": classification(result, before["display"], after["display"], expected, no_action)}
            report["cases"].append(row)
            args.out.write_text(json.dumps(report, indent=2)+"\n")
            print(goal, row["classification"], result["status"], result["reason"], after["display"], flush=True)
    finally:
        client.close()


if __name__ == "__main__":
    main()
