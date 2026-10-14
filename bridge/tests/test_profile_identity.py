"""profile.py with no profile: full eve, the owner's, is untouched (docs/plans/eve-lite.md M1, WP-2).

With EVE_PROFILE unset (or "full") ``apply`` returns [] and leaves every variable as it was, and ``install_firewall``
installs nothing: checked in a fresh process that then runs a fake git, which prints its marker. The same process
under EVE_PROFILE=lite does not get to run it (the positive control: the check can see a firewall when there is one).
"""

from __future__ import annotations

import copy
import json
import os
import subprocess
import sys
from pathlib import Path

import pytest

from eve_bridge import profile

SRC = Path(__file__).resolve().parents[1] / "src"
CANARY = "CANARY-NOT-A-KEY-7f3c"

# An owner-shaped environment: every door on, keys and ids set (placeholders), the switches lite would force.
OWNER_ENV = {
    "HOME": "/Users/someone", "PATH": "/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin",
    "OPENROUTER_API_KEY": CANARY, "OPENAI_API_KEY": CANARY, "COMPOSIO_API_KEY": CANARY, "TINYFISH_API_KEY": CANARY,
    "EVE_TELEGRAM": "1", "EVE_TELEGRAM_TOKEN": CANARY, "EVE_TELEGRAM_OWNER": "4242", "EVE_MINIAPP": "1",
    "EVE_COMPOSIO": "1", "EVE_APPS": "1", "EVE_MEMORY": "1", "EVE_SHARE": "1", "EVE_LISTEN_KEY": "option",
    "EVE_BRIDGE_SOCK": "/tmp/eve-bridge.sock", "SPOTIFY_CLIENT_ID": CANARY, "AGENTCARD_ACCESS_TOKEN": CANARY,
}


@pytest.mark.parametrize("value", [None, "", "full", "FULL", " full\n"])
def test_unset_or_full_is_full(value: str | None) -> None:
    env = {} if value is None else {"EVE_PROFILE": value}
    assert profile.active(env) == profile.FULL and profile.is_lite(env) is False


@pytest.mark.parametrize("value", ["lite", "Lite", " lite\n"])
def test_lite_is_lite(value: str) -> None:
    assert profile.active({"EVE_PROFILE": value}) == profile.LITE and profile.is_lite({"EVE_PROFILE": value}) is True


@pytest.mark.parametrize("value", ["lit", "1", "true", "on", "eve lite", "lite,full"])
def test_any_other_value_is_an_error(value: str) -> None:
    with pytest.raises(profile.ProfileError):
        profile.active({"EVE_PROFILE": value})
    assert issubclass(profile.ProfileError, ValueError)
    env = dict(OWNER_ENV, EVE_PROFILE=value)
    before = copy.deepcopy(env)
    with pytest.raises(profile.ProfileError):
        profile.apply(env)
    assert env == before                                          # nothing changed before the refusal


def test_the_error_does_not_repeat_a_long_value() -> None:
    with pytest.raises(profile.ProfileError) as raised:
        profile.active({"EVE_PROFILE": "x" * 30 + CANARY})
    assert CANARY not in str(raised.value)


def test_the_process_environment_is_read_when_none_is_given(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("EVE_PROFILE", raising=False)
    assert profile.is_lite() is False and profile.active() == profile.FULL
    monkeypatch.setenv("EVE_PROFILE", "lite")
    assert profile.is_lite() is True
    assert profile.is_lite({}) is False                           # a mapping passed in is the one read


@pytest.mark.parametrize("value", [None, "full"])
def test_apply_with_no_profile_is_the_identity(value: str | None) -> None:
    env = dict(OWNER_ENV) if value is None else dict(OWNER_ENV, EVE_PROFILE=value)
    before = copy.deepcopy(env)
    assert profile.apply(env) == []
    assert env == before and list(env) == list(before)            # the same names, values and order


def test_apply_with_no_profile_leaves_this_process_alone(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("EVE_PROFILE", raising=False)
    before = dict(os.environ)
    assert profile.apply(os.environ) == []
    assert dict(os.environ) == before


def test_no_profile_no_firewall_and_lite_has_one(tmp_path: Path) -> None:
    """A fresh process: apply, install_firewall, then a fake git. Under full the git runs and the counts stay empty;
    under lite the same script cannot run it (the control that the check sees a firewall when there is one)."""
    fake = tmp_path / "bin" / "git"
    fake.parent.mkdir()
    fake.write_text("#!/bin/sh\necho RAN-FAKE-GIT\n")
    fake.chmod(0o755)
    script = tmp_path / "run.py"
    script.write_text(
        "import json, os, subprocess\n"
        "from eve_bridge import profile\n"
        "changed = profile.apply(os.environ)\n"
        "profile.install_firewall()\n"
        "try:\n"
        "    out = subprocess.run(['git'], capture_output=True, text=True).stdout.strip()\n"
        "except PermissionError as e:\n"
        "    out = 'refused: ' + str(e)\n"
        "print(json.dumps({'changed': changed, 'git': out, 'counts': profile.firewall_counts()}))\n")
    base = {"PATH": f"{fake.parent}:/usr/bin:/bin:/usr/sbin:/sbin", "HOME": str(tmp_path / "home"),
            "PYTHONPATH": str(SRC), "OPENAI_API_KEY": CANARY}

    def run(extra: dict[str, str]) -> dict:
        done = subprocess.run([sys.executable, str(script)], capture_output=True, text=True, env={**base, **extra},
                              timeout=60)
        assert done.returncode == 0, done.stderr
        return json.loads(done.stdout.strip().splitlines()[-1])

    for extra in ({}, {"EVE_PROFILE": "full"}):
        full = run(extra)
        assert full == {"changed": [], "git": "RAN-FAKE-GIT", "counts": {}}
    lite = run({"EVE_PROFILE": "lite"})
    assert lite["git"].startswith("refused") and lite["counts"] == {"exec:git": 1}
    assert "OPENAI_API_KEY" in lite["changed"] and "EVE_TELEGRAM" in lite["changed"]


def test_install_firewall_in_full_eve_says_so_and_installs_nothing(tmp_path: Path) -> None:
    script = tmp_path / "run.py"
    script.write_text(
        "import logging, sys\n"
        "logging.basicConfig(level=logging.INFO, stream=sys.stdout, format='%(message)s')\n"
        "from eve_bridge import profile\n"
        "profile.install_firewall('audit')\n"
        "profile.install_firewall()\n"
        "print('counts', profile.firewall_counts())\n")
    done = subprocess.run([sys.executable, str(script)], capture_output=True, text=True, timeout=60,
                          env={"PATH": "/usr/bin:/bin", "HOME": str(tmp_path), "PYTHONPATH": str(SRC)})
    assert done.returncode == 0, done.stderr
    assert done.stdout.count("firewall: not installed") == 2
    assert "counts {}" in done.stdout


def test_a_bad_mode_is_refused_before_anything() -> None:
    with pytest.raises(ValueError):
        profile.install_firewall("off")
    assert profile.firewall_counts() == {}
