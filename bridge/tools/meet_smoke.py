"""Live run of the Meet notetaker (meet.py) outside the daemon: join a real call from the owner's Chrome, print
every text eve would send, and leave on Ctrl-C (the notes are written either way). Prints MEET_SMOKE_OK only
when eve got into the call, heard at least one caption line, and wrote the notes file.

Usage:
  .venv/bin/python tools/meet_smoke.py https://meet.google.com/abc-defg-hij
  .venv/bin/python tools/meet_smoke.py "6pm"          # the owner's calendar (apps_mcp)
  .venv/bin/python tools/meet_smoke.py --calendar      # only list today's events with a Meet link

Needs Chrome running with remote debugging on (chrome://inspect/#remote-debugging) and the daemon's env file.
Chrome's "Allow remote debugging?" is pressed by eve when EVE_CHROME_ACCESS=allow, else press it.
"""

from __future__ import annotations

import asyncio
import logging
import signal
import sys
from datetime import datetime, timedelta
from pathlib import Path

from eve_bridge import apps_mcp, chrome_consent, envfile, meet


def calendar(env: dict[str, str]):
    cfg = apps_mcp.configured(env)
    if not cfg.enabled:
        return None
    bridge = apps_mcp.AppsBridge(cfg)
    bridge.start()
    return meet.calendar_lister(bridge)


async def main(argv: list[str]) -> int:
    logging.basicConfig(level=logging.INFO, format="%(asctime)s %(name)s %(message)s", datefmt="%H:%M:%S")
    import os

    envfile.load_env_file()                     # the daemon's env file, never over what is already set
    env = dict(os.environ)
    ask = " ".join(argv).strip()
    lister = calendar(env) if not meet.parse_link(ask)[0] else None
    if ask == "--calendar":
        if lister is None:
            print("no calendar (EVE_APPS, apps login google)")
            return 1
        now = datetime.now().astimezone()
        start = now.replace(hour=0, minute=0, second=0, microsecond=0)
        for e in meet.events_in(lister(start, start + timedelta(days=1))):
            print(e.get("summary"), (e.get("start") or {}).get("dateTime"), meet.event_link(e))
        return 0
    broker = chrome_consent.ConsentBroker(None, auto_allow=chrome_consent.access_preference() == "allow")
    store = Path(env.get("EVE_MEMORY_DIR") or "~/.config/eve-bridge/memory").expanduser()
    root = store / "transcripts" / "meetings"
    m = meet.make_meeter(None, root, answer=broker.answer_own_connection, environ=env)
    if m is None:
        print("meet is off here (EVE_MEET / EVE_BROWSER_ATTACH / Playwright)")
        return 1
    m.list_events = lister

    async def notify(text: str, about: str = "") -> None:
        print(f"\n[eve would text] {text}\n", flush=True)

    m.notify = notify
    r = await m.join(ask)
    print(r, flush=True)
    if not r.get("ok"):
        return 1
    loop = asyncio.get_running_loop()
    loop.add_signal_handler(signal.SIGINT, m.leave)
    loop.add_signal_handler(signal.SIGTERM, m.leave)
    state = await m.task
    print(f"ended: {state.ended}; lines: {state.lines}; notes: {state.notes}; clicks: {state.clicks}")
    if state.joined_at is not None and state.lines > 0 and state.notes and state.notes.exists():
        print("MEET_SMOKE_OK")
        return 0
    return 1


if __name__ == "__main__":
    sys.exit(asyncio.run(main(sys.argv[1:])))
