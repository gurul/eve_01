"""Updates through the live daemon, over the same HTTP door Eve Link uses (GATES.md G7).

Reads the link token from ~/.config/eve-bridge/stick-link.json (never printed), finds the daemon's link port among
its listening sockets (obsbot_live_check.py's way), asks for a refresh, waits for it to finish (at most
``WAIT_SECS``), then prints ONLY counts: how many cards, how many per source kind, and when they were made. Never a
card's words, its source title or its entity: those come from the owner's mail. Prints UPDATES_LIVE_OK when there is
at least one card and they were made by this check's own refresh. Only when the daemon answers ``started: false``
(a rebuild was already running) does a build within ``FRESH_SECS`` count, and the check says which way it passed.
"""

from __future__ import annotations

import json
import sys
import time
from collections import Counter
from datetime import datetime
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import obsbot_live_check as live  # noqa: E402 — the token path, the port finder and the HTTP call

UPDATES = "/api/link/updates"
WAIT_SECS = 180.0
POLL_SECS = 3.0
FRESH_SECS = 600.0
CLOCK_SLACK_SECS = 2.0              # generated_at has whole seconds


def main() -> None:
    token = json.loads(live.TOKEN_PATH.read_text()).get("token", "")
    if not token:
        live.fail("no link token")
    port = None
    for p in live.daemon_ports():
        try:
            status, raw, _ = live.call(p, token, "GET", UPDATES, timeout=5)
        except OSError:
            continue
        if status == 200 and b'"cards"' in raw:
            port = p
            break
    if port is None:
        live.fail("no daemon port answers /api/link/updates (is the daemon on this branch?)")
    asked = time.time()
    status, raw, _ = live.call(port, token, "POST", UPDATES + "/refresh", {})
    if status != 200:
        live.fail(f"refresh -> HTTP {status}")
    started = bool(json.loads(raw).get("started"))
    print(f"refresh: started {str(started).lower()} (port {port})")

    deadline = time.monotonic() + WAIT_SECS
    view: dict = {}
    while time.monotonic() < deadline:
        time.sleep(POLL_SECS)
        status, raw, _ = live.call(port, token, "GET", UPDATES, timeout=20)
        if status != 200:
            live.fail(f"GET -> HTTP {status}")
        view = json.loads(raw)
        if not view.get("refreshing"):
            break
    else:
        live.fail(f"still refreshing after {WAIT_SECS:.0f} s")

    cards = view.get("cards") or []
    kinds = Counter(str(c.get("kind")) for c in cards if isinstance(c, dict))
    made = view.get("generated_at")
    made_at = age = None
    if made:
        try:
            made_at = datetime.fromisoformat(made).timestamp()
            age = time.time() - made_at
        except ValueError:
            made_at = age = None
    print(f"cards: {len(cards)}")
    print("by kind: " + (", ".join(f"{k} {n}" for k, n in sorted(kinds.items())) or "none"))
    print(f"generated_at: {made} ({'unreadable' if age is None else f'{age:.0f} s ago'})")
    if started:
        # this check's own refresh ran: the cards must come from it. A failed rebuild keeps the old cards and their
        # time, so an earlier build, however recent, does not count.
        fresh = made_at is not None and made_at >= asked - CLOCK_SLACK_SECS
        print("fresh by: this check's refresh")
    else:
        # a refresh was already running when the check asked, and it has finished: a recent build is accepted
        fresh = age is not None and age <= FRESH_SECS
        print(f"fresh by: a build already running (within {FRESH_SECS:.0f} s)")
    if len(cards) >= 1 and fresh:
        print("UPDATES_LIVE_OK")
        return
    live.fail("no cards" if not cards else "the cards are not fresh")


if __name__ == "__main__":
    main()
