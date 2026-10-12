"""Is the phone's location live on this Mac? Reads the running daemon's stored fix (phone_location.py) the way the
daemon does: the state file the daemon's environment names (``EVE_PHONE_LOCATION_FILE`` from the daemon's env file,
``~/.config/eve-bridge/env``, else the default path) and the switch (``EVE_PHONE_LOCATION``) from the same file.

Prints the fix's age in minutes and its accuracy in meters, and nothing else about it: never the coordinates or
the place. Prints LOCATION_LIVE_OK and exits 0 when the fix is under ``LIVE_MIN`` minutes old; exits 1 otherwise
(no fix, an old one, or the switch off).

    bridge/.venv/bin/python bridge/tools/phone_location_check.py
    bridge/.venv/bin/python bridge/tools/phone_location_check.py --env-file PATH --state PATH   # tests

Only the two variable names above are read from the env file; no value from it is printed.
"""

from __future__ import annotations

import argparse
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Optional

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "src"))

from eve_bridge import envfile, phone_location  # noqa: E402

LIVE_MIN = 30
KEYS = (phone_location.SWITCH, phone_location.FILE_ENV)


def daemon_env(path: Path) -> dict[str, str]:
    """The two keys the daemon would see from its env file (the launchd service has no shell environment)."""
    try:
        parsed = envfile.parse_env_file(path.expanduser().read_text(encoding="utf-8"))
    except OSError:
        return {}
    return {k: parsed[k] for k in KEYS if k in parsed}


def check(env: dict[str, str], state: Optional[Path] = None, now: Optional[datetime] = None) -> tuple[int, str]:
    """(exit code, the lines to print)."""
    if not phone_location.enabled(env):
        return 1, "FAIL: phone location is off in the daemon's env (EVE_PHONE_LOCATION)"
    path = state if state is not None else Path(env.get(phone_location.FILE_ENV)
                                                or phone_location.DEFAULT_FILE).expanduser()
    fix = phone_location.load(path)
    if fix is None:
        return 1, "FAIL: no location from the phone yet"
    now = now if now is not None else datetime.now(timezone.utc)
    age_min = fix.age_secs(now) / 60
    line = f"age {age_min:.1f} min, accuracy {round(fix.accuracy_m)} m"
    if age_min < LIVE_MIN:
        return 0, line + "\nLOCATION_LIVE_OK"
    return 1, line + f"\nFAIL: the newest fix is {LIVE_MIN} minutes old or more"


def main(argv: Optional[list[str]] = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--env-file", type=Path, default=envfile.DEFAULT_PATH, help="the daemon's env file")
    ap.add_argument("--state", type=Path, default=None, help="the fix file (default: as the daemon finds it)")
    args = ap.parse_args(argv)
    code, text = check(daemon_env(args.env_file), args.state)
    print(text)
    return code


if __name__ == "__main__":
    sys.exit(main())
