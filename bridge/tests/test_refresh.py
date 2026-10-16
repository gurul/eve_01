"""refresh.py: the weekly reminder's dates, message and LaunchAgent."""
import json
import plistlib
import subprocess
import sys
from datetime import datetime, timedelta, timezone

from eve_bridge import refresh

NOW = datetime(2026, 10, 16, 18, 0, tzinfo=timezone.utc)


def _fake_security(expiry):
    def run(cmd, **_kw):
        assert cmd[:3] == ["security", "cms", "-D"]
        return subprocess.CompletedProcess(cmd, 0, plistlib.dumps({"ExpirationDate": expiry}), b"")
    return run


def test_app_expiry_reads_the_profile(tmp_path):
    (tmp_path / "embedded.mobileprovision").write_bytes(b"x")
    got = refresh.app_expiry(tmp_path, run=_fake_security(datetime(2026, 10, 23, 18, 40)))
    assert got == datetime(2026, 10, 23, 18, 40, tzinfo=timezone.utc)


def test_app_expiry_none_without_a_build(tmp_path):
    assert refresh.app_expiry(tmp_path, run=_fake_security(datetime(2026, 1, 1))) is None


def test_google_expiry_is_seven_days_after_sign_in(tmp_path):
    p = tmp_path / "google-token.json"
    p.write_text(json.dumps({"refresh_token": "r", "signed_in_at": NOW.timestamp()}))
    assert refresh.google_expiry(p) == NOW + timedelta(days=7)


def test_google_expiry_unknown_without_a_stamp_or_file(tmp_path):
    p = tmp_path / "google-token.json"
    assert refresh.google_expiry(p) is None
    p.write_text(json.dumps({"refresh_token": "r"}))
    assert refresh.google_expiry(p) is None


def test_message_names_both_steps_and_how_long_is_left():
    text = refresh.message(NOW, NOW + timedelta(days=1, hours=2), NOW - timedelta(hours=1))
    assert "build.sh --install" in text and "(in 1 day)" in text
    assert "apps login google" in text and "expired" in text
    assert "date unknown" in refresh.message(NOW, None, None)


def test_plist_runs_the_venv_python_weekly():
    plist = plistlib.loads(refresh.build_plist())
    assert plist["ProgramArguments"] == [sys.executable, "-m", "eve_bridge.cli", "refresh", "remind"]
    assert plist["StartCalendarInterval"] == {"Weekday": 5, "Hour": 9, "Minute": 0}
    assert "RunAtLoad" not in plist
