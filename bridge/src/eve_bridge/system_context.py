"""Owner-configured location and an aware clock snapshot for Eve's prompts."""

from __future__ import annotations

import os
from datetime import datetime
from typing import Mapping, Optional
from zoneinfo import ZoneInfo, ZoneInfoNotFoundError


def context(environ: Optional[Mapping[str, str]] = None, *, now: Optional[datetime] = None) -> str:
    env = os.environ if environ is None else environ
    zone_name = (env.get("EVE_TIMEZONE") or "").strip()
    try:
        zone = ZoneInfo(zone_name) if zone_name else None
    except (ZoneInfoNotFoundError, ValueError):
        zone = None
    instant = now if now is not None else datetime.now().astimezone()
    local = instant.astimezone(zone)
    location = " ".join((env.get("EVE_LOCATION") or "").split())[:300]
    lines = ["\n\nSystem context:",
             f"Local clock when this context was generated: {local.isoformat(timespec='seconds')}.",
             f"Timezone: {zone.key if zone else local.tzname()}."]
    if location:
        lines += [f"Owner-configured location: {location}.",
                  "Use this location for local questions unless the owner names another place; "
                  "it is a configured default, not live GPS."]
    lines.append("The timestamp is a snapshot, not a ticking clock. For the current time later in a "
                 "conversation, use a fresh time lookup. Do not treat an old snapshot as the current time.")
    return "\n".join(lines)
