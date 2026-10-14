"""web_auth.py: eve lite's cookies, paired devices, the pairing window and the Mac's one-time login.

Fake clocks drive every expiry. Tokens, secrets and codes are the ones the code makes; a wrong one is a canary that
is not shaped like a key."""
from __future__ import annotations

import hashlib
import json
import logging
import os
import re
import stat
from pathlib import Path
from typing import Any

import pytest

from eve_bridge import web_auth
from eve_bridge.web_auth import COOKIE_DEV, COOKIE_MAC, MAX_AGE, Devices, MacLogin, PairingWindow

CANARY = "CANARY-NOT-A-KEY-7f3c"


class Clock:
    def __init__(self, t: float = 1_800_000_000.0) -> None:
        self.t = t

    def __call__(self) -> float:
        return self.t


# ---- cookies --------------------------------------------------------------------------------------------

def test_the_cookie_names_and_their_life() -> None:
    assert (COOKIE_MAC, COOKIE_DEV) == ("eve_mac", "eve_dev")
    assert MAX_AGE == 34_560_000 == 400 * 86400


def test_cookie_flags_are_exact_per_listener() -> None:
    token = CANARY
    assert web_auth.set_cookie(COOKIE_MAC, token) == (
        f"eve_mac={token}; HttpOnly; SameSite=Strict; Path=/; Max-Age=34560000")
    assert web_auth.set_cookie(COOKIE_DEV, token) == (
        f"eve_dev={token}; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age=34560000")
    assert web_auth.clear_cookie(COOKIE_MAC) == "eve_mac=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0"
    assert web_auth.clear_cookie(COOKIE_DEV) == "eve_dev=; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age=0"
    for bad_name in ("session", "eve", ""):
        with pytest.raises(ValueError):
            web_auth.set_cookie(bad_name, token)
    for bad_token in ("short", "has space in it here", "semi;colon-injected-here", "x" * 300, 42):
        with pytest.raises(ValueError):
            web_auth.set_cookie(COOKIE_DEV, bad_token)


def test_cookie_value_reads_a_cookie_header_without_tripping() -> None:
    header = 'theme=dark; broken; =nothing; eve_dev=TOKEN_one-2; xeve_dev=other; eve_mac="QUOTED_two-3"'
    assert web_auth.cookie_value(header, COOKIE_DEV) == "TOKEN_one-2"
    assert web_auth.cookie_value(header, COOKIE_MAC) == "QUOTED_two-3"
    assert web_auth.cookie_value("eve_dev=; eve_dev=second", COOKIE_DEV) == "second"
    assert web_auth.cookie_value("xeve_dev=1", COOKIE_DEV) == ""
    assert web_auth.cookie_value("", COOKIE_DEV) == web_auth.cookie_value(None, COOKIE_DEV) == ""


# ---- devices --------------------------------------------------------------------------------------------

def test_device_tokens_are_stored_only_as_hashes_in_a_0600_file(tmp_path: Path) -> None:
    path = tmp_path / "web-devices.json"
    devices = Devices(path)
    dev_id, token = devices.add("iPhone", "kitchen-mac.local:48781")
    assert re.fullmatch(r"[0-9a-f]{8}", dev_id)
    assert re.fullmatch(r"[A-Za-z0-9_-]{43}", token)                 # 256 bits, cookie-safe as it is
    assert stat.S_IMODE(os.stat(path).st_mode) == 0o600
    text = path.read_text()
    assert token not in text
    assert hashlib.sha256(token.encode()).hexdigest() in text        # control: what is kept is the hash
    data = json.loads(text)
    assert data["version"] == 1 and [d["id"] for d in data["devices"]] == [dev_id]
    assert [p.name for p in tmp_path.iterdir()] == ["web-devices.json"]       # no temporary file left behind


def test_check_finds_the_device_and_nothing_else(tmp_path: Path) -> None:
    devices = Devices(tmp_path / "web-devices.json")
    dev_id, token = devices.add("iPhone", "kitchen-mac.local:48781")
    found = devices.check(token)
    assert found is not None and (found.id, found.label, found.host) == (dev_id, "iPhone", "kitchen-mac.local:48781")
    for wrong in (CANARY, token[:-1], token + "x", token.upper() if token.upper() != token else CANARY, "",
                  None, 42, "a" * 300, "bad token with spaces in it"):
        assert devices.check(wrong) is None


def test_a_token_is_refused_on_the_other_door(tmp_path: Path) -> None:
    """A phone pairs on the LAN door; its token is refused when the Mac door (``local``) asks with it, so it reaches
    no Mac-only route. The Mac's own device is refused on the LAN door the same way. With no door asked, either
    works (the daemon's own list and revoke do not care which door a token belongs to)."""
    devices = Devices(tmp_path / "web-devices.json")
    _phone_id, phone = devices.add("iPhone", "kitchen-mac.local:48781", door="lan")
    _mac_id, mac = devices.add("This Mac", "localhost:48780", door="local")
    assert devices.check(phone, "lan") is not None and devices.check(phone, "local") is None
    assert devices.check(mac, "local") is not None and devices.check(mac, "lan") is None
    assert devices.check(phone) is not None and devices.check(mac) is not None          # no door asked: either


def test_a_row_with_no_door_reads_as_lan_only(tmp_path: Path) -> None:
    """An older file's row, or one written by hand, has no door: it reads as ``lan`` (the lower-privilege door), so
    it can never stand in for the Mac's own page."""
    path = tmp_path / "web-devices.json"
    digest = hashlib.sha256(CANARY.encode()).hexdigest()
    row = {"id": "0a1b2c3d", "label": "old", "host": "h", "sha256": digest, "created": 1, "last_seen": 1_800_000_000}
    for door in (None, "", "admin", 1):
        raw = dict(row)
        if door is not None:
            raw["door"] = door
        path.write_text(json.dumps({"version": 1, "devices": [raw]}))
        devices = Devices(path, clock=Clock())
        assert devices.check(CANARY, "lan") is not None, door
        assert devices.check(CANARY, "local") is None, door


def test_revoke_returns_the_ids_and_the_token_stops_working(tmp_path: Path) -> None:
    devices = Devices(tmp_path / "web-devices.json")
    a_id, a_token = devices.add("iPhone", "kitchen-mac.local:48781")
    b_id, b_token = devices.add("Laptop", "192.168.1.20:48781")
    c_id, c_token = devices.add("This Mac", "localhost:48780")
    assert devices.revoke(a_id) == [a_id]
    assert devices.check(a_token) is None and devices.check(b_token) is not None
    assert devices.revoke(a_id) == [] and devices.revoke("ffffffff") == [] and devices.revoke("") == []
    assert devices.revoke(f"  {b_id.upper()} ") == [b_id]           # as typed at `eve-bridge web revoke`
    assert devices.add("Phone 2", "kitchen-mac.local:48781")[0] not in (a_id, b_id)
    gone = devices.revoke("all")
    assert c_id in gone and len(gone) == 2
    assert devices.list() == [] and devices.check(c_token) is None


def test_list_shows_devices_oldest_first_without_hashes(tmp_path: Path) -> None:
    clock = Clock()
    devices = Devices(tmp_path / "web-devices.json", clock=clock)
    first, _t1 = devices.add("iPhone", "kitchen-mac.local:48781")
    clock.t += 60
    second, _t2 = devices.add("Laptop", "192.168.1.20:48781")
    listed = devices.list()
    assert [d["id"] for d in listed] == [first, second]
    assert all(set(d) == {"id", "label", "host", "created", "last_seen"} for d in listed)
    assert listed[1]["created"] == int(clock.t)


def test_a_revoke_by_another_process_counts_at_once(tmp_path: Path) -> None:
    path = tmp_path / "web-devices.json"
    daemon = Devices(path)
    dev_id, token = daemon.add("iPhone", "kitchen-mac.local:48781")
    assert daemon.check(token) is not None
    assert Devices(path).revoke(dev_id) == [dev_id]                 # e.g. the CLI with the daemon down
    assert daemon.check(token) is None
    path.unlink()                                                   # the file deleted: every device is gone
    assert Devices(path).list() == []


def test_a_damaged_file_admits_no_one_and_is_rewritten(tmp_path: Path, caplog: pytest.LogCaptureFixture) -> None:
    path = tmp_path / "web-devices.json"
    devices = Devices(path)
    _id, token = devices.add("iPhone", "kitchen-mac.local:48781")
    path.write_text("{not json")
    with caplog.at_level(logging.WARNING, logger="eve_bridge.web_auth"):
        assert devices.check(token) is None
    assert "could not be read" in caplog.text
    new_id, new_token = devices.add("iPhone", "kitchen-mac.local:48781")
    assert devices.check(new_token) is not None
    assert [d["id"] for d in json.loads(path.read_text())["devices"]] == [new_id]


def test_malformed_rows_are_dropped_not_trusted(tmp_path: Path) -> None:
    path = tmp_path / "web-devices.json"
    good = {"id": "0a1b2c3d", "label": "iPhone", "host": "h", "sha256": hashlib.sha256(CANARY.encode()).hexdigest(),
            "created": 1, "last_seen": 1_800_000_000}
    rows = [good, dict(good, id="NOTHEX!!"), dict(good, id="1a1b2c3d", sha256="short"),
            dict(good, id="2a1b2c3d", created="yesterday"), dict(good, id="3a1b2c3d", last_seen=True), "row", None,
            dict(good, label="ok")]                                  # a second row with the same id: the first wins
    path.write_text(json.dumps({"version": 1, "devices": rows}))
    devices = Devices(path, clock=Clock())
    assert [d["id"] for d in devices.list()] == ["0a1b2c3d"]
    found = devices.check(CANARY)
    assert found is not None and found.label == "iPhone"


def test_a_failed_save_never_half_pairs_and_a_revoke_holds_at_once(tmp_path: Path, monkeypatch: pytest.MonkeyPatch,
                                                                    caplog: pytest.LogCaptureFixture) -> None:
    clock = Clock()
    devices = Devices(tmp_path / "web-devices.json", clock=clock)
    kept_id, kept_token = devices.add("iPhone", "kitchen-mac.local:48781")

    def disk_full(path: Path, data: bytes) -> None:
        raise OSError(28, "No space left on device")

    monkeypatch.setattr(web_auth, "_write_private", disk_full)
    with pytest.raises(OSError):
        devices.add("Laptop", "192.168.1.20:48781")
    assert [d["id"] for d in devices.list()] == [kept_id]           # nothing half paired
    clock.t += web_auth.SEEN_SECS
    with caplog.at_level(logging.WARNING, logger="eve_bridge.web_auth"):
        assert devices.check(kept_token) is not None                # a full disk signs no one out
    assert "could not save" in caplog.text
    with pytest.raises(OSError):
        devices.revoke(kept_id)                                     # told: it would not outlive a restart
    assert devices.check(kept_token) is None                        # but the token stopped working at once


def test_labels_are_one_clean_line(tmp_path: Path) -> None:
    devices = Devices(tmp_path / "web-devices.json")
    devices.add("Guest‮'s\x00 phone\n\tnow" + "x" * 100, None)  # type: ignore[arg-type]
    (row,) = devices.list()
    assert row["label"].startswith("Guest's phone now") and len(row["label"]) == 64
    assert "‮" not in row["label"] and row["host"] == ""


def test_last_seen_moves_without_writing_on_every_request(tmp_path: Path) -> None:
    clock = Clock()
    path = tmp_path / "web-devices.json"
    devices = Devices(path, clock=clock)
    _id, token = devices.add("iPhone", "kitchen-mac.local:48781")
    written = path.read_bytes()
    clock.t += 30
    assert devices.check(token).last_seen == int(clock.t) - 30       # type: ignore[union-attr]
    assert path.read_bytes() == written
    clock.t += web_auth.SEEN_SECS
    assert devices.check(token).last_seen == int(clock.t)            # type: ignore[union-attr]
    assert json.loads(path.read_text())["devices"][0]["last_seen"] == int(clock.t)


def test_a_device_unseen_for_max_age_is_refused_and_forgotten(tmp_path: Path) -> None:
    clock = Clock()
    devices = Devices(tmp_path / "web-devices.json", clock=clock)
    _old, old_token = devices.add("Old phone", "kitchen-mac.local:48781")
    clock.t += MAX_AGE - 10
    _new, new_token = devices.add("New phone", "kitchen-mac.local:48781")
    clock.t += 20
    assert devices.check(old_token) is None and devices.check(new_token) is not None
    assert [d["label"] for d in devices.list()] == ["New phone"]
    devices.add("Laptop", "192.168.1.20:48781")
    labels = [d["label"] for d in json.loads((tmp_path / "web-devices.json").read_text())["devices"]]
    assert labels == ["New phone", "Laptop"]


def test_past_max_devices_the_one_seen_longest_ago_goes(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(web_auth, "MAX_DEVICES", 3)
    clock = Clock()
    devices = Devices(tmp_path / "web-devices.json", clock=clock)
    ids_tokens = []
    for name in ("a", "b", "c"):
        ids_tokens.append(devices.add(name, "h"))
        clock.t += web_auth.SEEN_SECS
    devices.check(ids_tokens[0][1])                                   # "a" was just seen; "b" now longest ago
    devices.add("d", "h")
    assert [d["label"] for d in devices.list()] == ["a", "c", "d"]
    assert devices.check(ids_tokens[1][1]) is None


def test_device_secrets_never_reach_a_log(tmp_path: Path, caplog: pytest.LogCaptureFixture) -> None:
    with caplog.at_level(logging.DEBUG, logger="eve_bridge.web_auth"):
        devices = Devices(tmp_path / "web-devices.json")
        dev_id, token = devices.add("iPhone", "kitchen-mac.local:48781")
        devices.check(token)
        devices.revoke(dev_id)
    assert "paired a device" in caplog.text and "revoked 1" in caplog.text      # control: lines were captured
    assert token not in caplog.text and hashlib.sha256(token.encode()).hexdigest() not in caplog.text


# ---- the pairing window ---------------------------------------------------------------------------------

def test_no_pairing_without_an_open_window() -> None:
    window = PairingWindow()
    assert not window.is_open and window.remaining == 0.0
    assert window.pair(secret=CANARY) is False and window.pair(code="123456") is False


def test_a_window_holds_a_128_bit_secret_and_a_6_digit_code() -> None:
    window = PairingWindow(clock=Clock())
    secret, code = window.open()
    assert re.fullmatch(r"[A-Za-z0-9_-]{22}", secret)               # 16 random bytes, safe in a URL fragment
    assert re.fullmatch(r"[0-9]{6}", code)
    assert window.is_open and window.remaining == 300.0
    assert len({window.open()[0] for _ in range(20)}) == 20


def test_the_secret_pairs_once_and_closes_the_window() -> None:
    window = PairingWindow(clock=Clock())
    secret, code = window.open()
    assert window.pair(secret=secret) is True
    assert not window.is_open
    assert window.pair(secret=secret) is False                       # single use
    assert window.pair(code=code) is False                           # the code went with it


@pytest.mark.parametrize("typed", ["{code}", "{code3} {code_3}", "{code3}-{code_3}", " {code} ", "int"])
def test_the_code_pairs_once_as_typed(typed: str) -> None:
    window = PairingWindow(clock=Clock())
    secret, code = window.open()
    given: Any = int(code) if typed == "int" else typed.format(code=code, code3=code[:3], code_3=code[3:])
    assert window.pair(code=given) is True
    assert window.pair(code=code) is False and window.pair(secret=secret) is False


def test_a_window_expires() -> None:
    clock = Clock()
    window = PairingWindow(clock=clock)
    secret, code = window.open(ttl=300)
    clock.t += 299
    assert window.is_open and window.remaining == pytest.approx(1.0)
    clock.t += 1
    assert not window.is_open and window.remaining == 0.0
    assert window.pair(secret=secret) is False and window.pair(code=code) is False


def test_five_wrong_tries_close_the_window() -> None:
    window = PairingWindow(clock=Clock())
    secret, code = window.open()
    wrong_code = f"{(int(code) + 1) % 1_000_000:06d}"
    for _ in range(4):
        assert window.pair(code=wrong_code) is False
    assert window.pair(secret=secret) is True                        # control: four wrong tries still allow it

    secret, code = window.open()
    wrong_code = f"{(int(code) + 1) % 1_000_000:06d}"
    tries: list[dict[str, Any]] = [{"code": wrong_code}, {"secret": CANARY}, {}, {"secret": secret, "code": code},
                                   {"code": "12345x"}]
    for kwargs in tries:                                              # wrong, malformed, neither, both: all count
        assert window.pair(**kwargs) is False
    assert not window.is_open
    assert window.pair(secret=secret) is False and window.pair(code=code) is False


def test_a_new_window_replaces_the_old_one_and_its_tries() -> None:
    window = PairingWindow(clock=Clock())
    old_secret, old_code = window.open()
    for _ in range(4):
        window.pair(secret=CANARY)
    new_secret, new_code = window.open()
    assert window.pair(secret=old_secret) is False
    for _ in range(3):                                               # the count started again: 1 + 3 < 5
        window.pair(secret=CANARY)
    assert window.is_open and window.pair(code=new_code) is True


def test_close_ends_a_window_and_pairing_secrets_never_reach_a_log(caplog: pytest.LogCaptureFixture) -> None:
    window = PairingWindow(clock=Clock())
    with caplog.at_level(logging.DEBUG, logger="eve_bridge.web_auth"):
        secret, code = window.open()
        window.pair(secret=CANARY)
        window.close()
        assert not window.is_open and window.pair(secret=secret) is False
        secret2, code2 = window.open()
        window.pair(code=code2)
    assert "pairing window open" in caplog.text and "paired through the window" in caplog.text
    for value in (secret, code, secret2, code2, CANARY):
        assert value not in caplog.text


# ---- the Mac's own login --------------------------------------------------------------------------------

def test_a_mac_login_code_works_once() -> None:
    login = MacLogin(clock=Clock())
    code = login.issue()
    assert re.fullmatch(r"[A-Za-z0-9_-]{22}", code)                 # 128 bits
    assert login.used(code) is False
    assert login.consume(code) is True
    assert login.used(code) is True
    assert login.consume(code) is False


def test_a_mac_login_code_expires() -> None:
    clock = Clock()
    login = MacLogin(clock=clock)
    code = login.issue(ttl=120)
    clock.t += 120
    assert login.consume(code) is False and login.used(code) is False
    used = login.issue(ttl=120)
    assert login.consume(used)
    clock.t += 120 + web_auth.LOGIN_KEEP_SECS - 1
    assert login.used(used) is True                                  # `web open` may still ask, a little after
    clock.t += 1
    assert login.used(used) is False


def test_mac_login_codes_are_independent_and_bounded() -> None:
    clock = Clock()
    login = MacLogin(clock=clock)
    first, second = login.issue(), login.issue()
    assert first != second
    assert login.consume(second) and not login.used(first) and login.consume(first)
    many = [login.issue() for _ in range(web_auth.MAX_LOGINS + 1)]
    assert login.consume(many[0]) is False                           # the oldest made room
    assert all(login.consume(c) for c in many[1:])


def test_mac_login_refuses_what_it_never_issued() -> None:
    login = MacLogin(clock=Clock())
    login.issue()
    for wrong in (CANARY, "", None, 123456, "x" * 1000, "bad code with spaces"):
        assert login.consume(wrong) is False and login.used(wrong) is False


def test_mac_login_codes_are_held_only_as_hashes(caplog: pytest.LogCaptureFixture) -> None:
    login = MacLogin(clock=Clock())
    with caplog.at_level(logging.DEBUG, logger="eve_bridge.web_auth"):
        code = login.issue()
        login.consume(code)
    assert "signed in" in caplog.text and code not in caplog.text
    held = repr(vars(login))
    assert code not in held and hashlib.sha256(code.encode()).hexdigest() in held    # control: the hash is
