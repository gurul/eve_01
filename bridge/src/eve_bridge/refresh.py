"""The weekly refresh reminder (docs/refresh.md).

Owner, 2026-10-16, after the widget went blank: "make a weekly reminder to refresh eve, both app and auths". Two
things lapse every 7 days and fail quietly when they do:

* **Eve Link** is signed with a free personal team, so its profile expires 7 days after it was made. The app then
  will not open. ``app_expiry`` reads the profile of the last build (``APP``), which is what ``build.sh --install``
  put on the phone.
* **Google** sign-in: the OAuth app is in "Testing", so Google drops the refresh token 7 days after sign-in
  (docs/apps.md). Calendar, Gmail, Drive and Contacts stop, and the home-screen widget comes back empty.
  ``google_login`` stamps ``signed_in_at`` in the token file; the file's mtime changes at every hourly refresh, so it
  cannot stand in.

``remind`` posts one notification (ntfy, notify.py) and one on the Mac, with both dates and the steps.
``schedule`` writes a LaunchAgent that runs it every Friday at 09:00. It runs the venv's python, as the daemon's
agent does: launchd may not run a ``/bin/sh`` script out of ~/Documents ("Operation not permitted").
"""
from __future__ import annotations

import json
import plistlib
import shutil
import subprocess
import sys
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Callable, Optional

from . import _service_launchd

EVE = Path(__file__).resolve().parents[3]
APP = EVE / "ios/EveLink/build/Build/Products/Debug-iphoneos/EveLink.app"
GOOGLE_LIFE = timedelta(days=7)
LABEL = "com.github.eve-bridge.refresh-reminder"
WEEKDAY, HOUR, MINUTE = 5, 9, 0          # Friday 09:00: the app and Google were last refreshed on a Friday
LOG = Path.home() / "Library" / "Logs" / "eve-refresh-reminder.log"


def app_expiry(app: Path = APP, run: Callable[..., subprocess.CompletedProcess] = subprocess.run
               ) -> Optional[datetime]:
    """When the last build's signing profile expires (UTC), or None when there is no build or it cannot be read."""
    profile = app / "embedded.mobileprovision"
    if not profile.exists():
        return None
    r = run(["security", "cms", "-D", "-i", str(profile)], capture_output=True)
    if r.returncode != 0:
        return None
    try:
        when = plistlib.loads(r.stdout).get("ExpirationDate")
    except Exception:  # noqa: BLE001 — an unreadable profile is "unknown", not a crash
        return None
    return when.replace(tzinfo=timezone.utc) if isinstance(when, datetime) else None


def google_expiry(token_path: Path) -> Optional[datetime]:
    """When the Google sign-in lapses: ``signed_in_at`` plus 7 days, or None (signed in before it was stamped)."""
    try:
        signed = float(json.loads(token_path.read_text()).get("signed_in_at") or 0)
    except (OSError, ValueError):
        return None
    return datetime.fromtimestamp(signed, timezone.utc) + GOOGLE_LIFE if signed else None


def _when(expiry: Optional[datetime], now: datetime) -> str:
    if expiry is None:
        return "date unknown"
    local = expiry.astimezone()
    days = (expiry - now).total_seconds() / 86400
    if days < 0:
        return f"expired {local:%a %b %-d}"
    left = "today" if days < 1 else "in 1 day" if days < 2 else f"in {int(days)} days"
    return f"expires {local:%a %b %-d, %-I:%M %p} ({left})"


def message(now: datetime, app: Optional[datetime], google: Optional[datetime]) -> str:
    return "\n".join([
        "Weekly refresh, on the Mac:",
        f"1. Eve Link {_when(app, now)}: sh ios/EveLink/build.sh --install (phone unlocked)",
        f"2. Google {_when(google, now)}: eve-bridge apps login google, then restart the daemon",
    ])


def current(token_path: Path, now: Optional[datetime] = None) -> str:
    return message(now or datetime.now(timezone.utc), app_expiry(), google_expiry(token_path))


def remind(text: str) -> bool:
    """Post ``text`` on the phone (ntfy) and on the Mac. True when the phone post went out."""
    from . import notify

    subprocess.run(["osascript", "-e", f"display notification {json.dumps(text)} with title \"eve: refresh\""],
                   capture_output=True)
    cfg = notify.configured()
    return bool(cfg.enabled and notify.post_once(notify.read_token(), text, cfg.base))


def plist_path() -> Path:
    return _service_launchd.agent_path(LABEL)


def build_plist() -> bytes:
    return plistlib.dumps({
        "Label": LABEL,
        "ProgramArguments": [sys.executable, "-m", "eve_bridge.cli", "refresh", "remind"],
        "StartCalendarInterval": {"Weekday": WEEKDAY, "Hour": HOUR, "Minute": MINUTE},
        "StandardOutPath": str(LOG),
        "StandardErrorPath": str(LOG),
        "EnvironmentVariables": {"HOME": str(Path.home()), "PATH": "/usr/bin:/bin:/usr/sbin:/sbin"},
    })


def schedule() -> int:
    if shutil.which("launchctl") is None:
        print("eve-bridge: `launchctl` not found on PATH", file=sys.stderr)
        return 2
    path = plist_path()
    path.parent.mkdir(parents=True, exist_ok=True)
    _service_launchd._write_plist(path, build_plist())
    rc = _service_launchd._load(path)
    if not rc:
        print(f"Scheduled: every Friday at 09:00 ({path}).")
    return rc


def unschedule() -> int:
    path = plist_path()
    if not path.exists():
        print("Not scheduled; nothing to do.")
        return 0
    subprocess.run(["launchctl", "unload", str(path)], capture_output=True)
    path.unlink()
    print(f"Removed: {path}")
    return 0
