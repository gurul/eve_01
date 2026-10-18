"""vault.py: the Keychain wrapper, the index, site binding, 2FA codes and `eve-bridge vault`.

Everything but the one live test runs on a fake `security`; the live test uses a throwaway keychain file and
never the owner's login keychain.
"""

from __future__ import annotations

import base64
import io
import json
import shlex
import stat
import subprocess
from pathlib import Path
from typing import Any

import pytest

from eve_bridge import vault as vlt
from eve_bridge.vault import Keychain, Refused, Vault, VaultError, canonical_origin, site_allows, totp


class FakeSecurity:
    """/usr/bin/security over a dict: records every argv and stdin so a test can look for a leaked value."""

    def __init__(self) -> None:
        self.items: dict[str, str] = {}
        self.calls: list[tuple[list[str], str]] = []

    def __call__(self, argv: list[str], input: str = "", **_: Any) -> subprocess.CompletedProcess:
        self.calls.append((list(argv), input or ""))
        if argv[1:] == ["-i"]:
            words = shlex.split(input)
            assert words[0] == "add-generic-password"
            self.items[words[words.index("-a") + 1]] = words[words.index("-w") + 1]
            return subprocess.CompletedProcess(argv, 0, "", "")
        handle = argv[argv.index("-a") + 1]
        if argv[1] == "find-generic-password":
            if handle not in self.items:
                return subprocess.CompletedProcess(argv, 44, "", "not found")
            return subprocess.CompletedProcess(argv, 0, self.items[handle] + "\n", "")
        if argv[1] == "delete-generic-password":
            return subprocess.CompletedProcess(argv, 0 if self.items.pop(handle, None) else 44, "", "")
        raise AssertionError(argv)


def make_vault(tmp_path: Path, at: float = 59.0) -> tuple[Vault, FakeSecurity]:
    fake = FakeSecurity()
    return Vault(tmp_path / "vault.json", Keychain(keychain="", run=fake), clock=lambda: at), fake


# ---- 2FA codes ------------------------------------------------------------------------------------------------

RFC6238 = [  # RFC 6238 appendix B: time, SHA1, SHA256, SHA512 (8 digits)
    (59, "94287082", "46119246", "90693936"),
    (1111111109, "07081804", "68084774", "25091201"),
    (1111111111, "14050471", "67062674", "99943326"),
    (1234567890, "89005924", "91819424", "93441116"),
    (2000000000, "69279037", "90698825", "38618901"),
    (20000000000, "65353130", "77737706", "47863826"),
]
SEEDS = {"SHA1": b"12345678901234567890", "SHA256": b"12345678901234567890123456789012",
         "SHA512": b"1234567890" * 6 + b"1234"}


def test_totp_matches_the_rfc_6238_vectors() -> None:
    for at, *codes in RFC6238:
        for algo, want in zip(("SHA1", "SHA256", "SHA512"), codes, strict=True):
            secret = base64.b32encode(SEEDS[algo]).decode()
            assert totp(secret, at=at, digits=8, algorithm=algo) == want, (at, algo)


def test_a_2fa_secret_is_read_from_a_uri_or_bare() -> None:
    secret = base64.b32encode(SEEDS["SHA1"]).decode().rstrip("=")
    got = vlt.parse_totp(f"otpauth://totp/Example:me?secret={secret}&issuer=Example&digits=8&period=30&algorithm=SHA256")
    assert got == {"secret": secret, "digits": 8, "period": 30, "algorithm": "SHA256"}
    assert vlt.parse_totp(" " + secret.lower()[:8] + "-" + secret.lower()[8:] + " ")["secret"] == secret
    for bad in ("", "not base32!", "otpauth://hotp/x?secret=" + secret, f"otpauth://totp/x?secret={secret}&digits=4"):
        with pytest.raises(VaultError):
            vlt.parse_totp(bad)


# ---- sites ----------------------------------------------------------------------------------------------------

def test_a_site_is_the_saved_host_or_its_subdomains_on_the_same_scheme_and_port() -> None:
    assert canonical_origin("chase.com") == "https://chase.com"
    assert canonical_origin("HTTPS://Secure.Chase.com/login?x=1") == "https://secure.chase.com"
    assert canonical_origin("http://127.0.0.1:8123/a") == "http://127.0.0.1:8123"
    assert canonical_origin("https://chase.com:443") == "https://chase.com"
    for bad in ("", "ftp://chase.com", "https://user:pw@chase.com", "chase com"):
        with pytest.raises(VaultError):
            canonical_origin(bad)
    yes = ["https://chase.com/", "https://secure.chase.com/login", "https://a.b.chase.com", "https://chase.com:443/x"]
    no = ["https://chase.com.evil.example/", "https://evilchase.com/", "http://chase.com/", "https://chase.com:8443/",
          "https://com/", "https://user@chase.com/", "", "not a url", "javascript:alert(1)"]
    for url in yes:
        assert site_allows("chase.com", url), url
    for url in no:
        assert not site_allows("chase.com", url), url
    assert not site_allows("secure.chase.com", "https://chase.com/")       # never the parent
    assert site_allows("http://127.0.0.1:8123", "http://127.0.0.1:8123/login.html")
    assert not site_allows("http://127.0.0.1:8123", "http://127.0.0.1:9999/login.html")


# ---- the Keychain wrapper and the index ----------------------------------------------------------------------

def test_a_secret_never_rides_on_a_command_line_and_the_index_holds_none(tmp_path: Path) -> None:
    v, fake = make_vault(tmp_path)
    login = v.add("login", "Chase", {"username": "me@example.com", "password": "hunter2 with spaces \"q\""},
                  site="https://chase.com/login")
    card = v.add("card", "Visa", {"number": "4242 4242 4242 4242", "exp_month": "7", "exp_year": "29",
                                  "cvc": "123", "name": "A Person"})
    code = v.add("totp", "Chase 2FA", {"secret": base64.b32encode(SEEDS["SHA1"]).decode()}, site="chase.com")
    for argv, _stdin in fake.calls:
        joined = " ".join(argv)
        for s in ("hunter2", "4242424242424242", "123 ", base64.b32encode(SEEDS["SHA1"]).decode()):
            assert s not in joined, (s, argv)
    index = (tmp_path / "vault.json").read_text()
    assert stat.S_IMODE((tmp_path / "vault.json").stat().st_mode) == 0o600
    for s in ("hunter2", "4242", "me@example.com", "GEZDGNBV"):
        assert s not in index, s
    assert [i.line() for i in v.items()] == [f'{login.handle} login "Chase" for chase.com',
                                             f'{card.handle} card "Visa"', f'{code.handle} totp "Chase 2FA" for chase.com']
    assert all(vlt.HANDLE.match(i.handle) for i in v.items()) and len({i.handle for i in v.items()}) == 3
    _, values = v.fill_values(login.handle, "https://secure.chase.com/x")
    assert values == {"username": "me@example.com", "password": 'hunter2 with spaces "q"'}


def test_a_fill_is_refused_off_its_site_and_a_card_fills_anywhere(tmp_path: Path) -> None:
    v, _ = make_vault(tmp_path, at=59.0)
    login = v.add("login", "Chase", {"username": "", "password": "pw"}, site="chase.com")
    card = v.add("card", "Visa", {"number": "4242424242424242", "exp_month": "07", "exp_year": "2029", "cvc": "1234"})
    code = v.add("totp", "2FA", {"secret": base64.b32encode(SEEDS["SHA1"]).decode()}, site="chase.com")
    with pytest.raises(Refused, match="saved for chase.com"):
        v.fill_values(login.handle, "https://chase.com.evil.example/login")
    with pytest.raises(Refused):
        v.fill_values(code.handle, "https://example.com/")
    with pytest.raises(Refused, match="no vault item"):
        v.fill_values("v_00000000", "https://chase.com/")
    with pytest.raises(Refused, match="has no"):
        v.fill_values(login.handle, "https://chase.com/", part="cvc")
    assert v.fill_values(login.handle, "https://chase.com/")[1] == {"password": "pw"}     # no username saved
    _, c = v.fill_values(card.handle, "https://shop.example/checkout")
    assert c == {"number": "4242424242424242", "exp_month": "07", "exp_year": "2029", "exp": "07/29", "cvc": "1234"}
    assert v.fill_values(card.handle, "https://x.example/", part="cvc")[1] == {"cvc": "1234"}
    assert v.fill_values(code.handle, "https://login.chase.com/")[1] == {"code": "287082"}  # RFC vector, 6 digits


def test_bad_items_are_refused_before_the_keychain_is_touched(tmp_path: Path) -> None:
    v, fake = make_vault(tmp_path)
    cases = [("login", {"password": ""}, "chase.com"), ("login", {"password": "x"}, ""),
             ("card", {"number": "42", "exp_month": "1", "exp_year": "29", "cvc": "123"}, ""),
             ("card", {"number": "4242424242424242", "exp_month": "13", "exp_year": "29", "cvc": "123"}, ""),
             ("card", {"number": "4242424242424242", "exp_month": "1", "exp_year": "29", "cvc": "12"}, ""),
             ("totp", {"secret": "nope!"}, "chase.com"), ("pin", {}, "")]
    for kind, value, site in cases:
        with pytest.raises(VaultError):
            v.add(kind, "x", value, site=site)
    assert fake.calls == [] and v.items() == []
    # review A11 (2026-10-18): a blank label raised only after the secret was in the Keychain, unindexed
    for label in ("", "   "):
        with pytest.raises(VaultError):
            v.add("login", label, {"username": "a", "password": "hunter2"}, site="chase.com")
    assert fake.calls == [] and fake.items == {} and v.items() == []


def test_remove_deletes_the_keychain_item_and_the_index_line(tmp_path: Path) -> None:
    v, fake = make_vault(tmp_path)
    a = v.add("login", "A", {"password": "a"}, site="a.example")
    b = v.add("login", "B", {"password": "b"}, site="b.example")
    assert v.remove(a.handle) and not v.remove(a.handle)
    assert [i.handle for i in v.items()] == [b.handle] and set(fake.items) == {b.handle}


# ---- eve-bridge vault -----------------------------------------------------------------------------------------

def test_the_cli_reads_secrets_with_getpass_and_prints_no_value(tmp_path: Path) -> None:
    v, _ = make_vault(tmp_path)
    secret_prompts: list[str] = []
    answers = {"Password: ": "pw-canary", "Card number: ": "4242424242424242", "CVC: ": "321",
               "2FA secret or otpauth:// URI: ": base64.b32encode(SEEDS["SHA1"]).decode()}

    def ask_secret(prompt: str) -> str:
        secret_prompts.append(prompt)
        return answers[prompt]

    plain = {"Username: ": "me", "Expiry (MM/YY): ": "08/30", "Name on card: ": "A Person"}
    out = io.StringIO()
    run = lambda *a: vlt.cli(list(a), vault=v, ask=plain.__getitem__, ask_secret=ask_secret, out=out)  # noqa: E731
    assert run("add", "login", "--label", "Chase", "--site", "chase.com") == 0
    assert run("add", "card", "--label", "Visa 4242") == 0
    assert run("add", "totp", "--label", "Chase 2FA", "--site", "chase.com") == 0
    assert run("list") == 0
    assert secret_prompts == ["Password: ", "Card number: ", "CVC: ", "2FA secret or otpauth:// URI: "]
    text = out.getvalue()
    for s in ("pw-canary", "4242424242424242", "321", "GEZDGNBV"):
        assert s not in text, s
    assert 'card "Visa 4242"' in text and "for chase.com" in text
    handle = v.items()[0].handle
    assert run("remove", handle) == 0 and run("remove", handle) == 1
    with pytest.raises(SystemExit):
        run("add", "login", "--label", "No site")


# ---- live: a throwaway keychain file ---------------------------------------------------------------------------

@pytest.mark.live
def test_live_keychain_round_trip_in_a_throwaway_keychain(tmp_path: Path) -> None:
    path = str(tmp_path / "eve-vault-test.keychain-db")
    password = "eve-test-keychain"
    subprocess.run([vlt.SECURITY, "create-keychain", "-p", password, path], check=True, capture_output=True)
    try:
        subprocess.run([vlt.SECURITY, "unlock-keychain", "-p", password, path], check=True, capture_output=True)
        v = Vault(tmp_path / "vault.json", Keychain(keychain=path))
        secret = base64.b32encode(SEEDS["SHA1"]).decode()
        login = v.add("login", "Local", {"username": "me", "password": "p@ss w0rd \"'\\ canary"},
                      site="http://127.0.0.1:8000")
        card = v.add("card", "Visa", {"number": "4242424242424242", "exp_month": "7", "exp_year": "2031",
                                      "cvc": "987", "name": "A Person"})
        code = v.add("totp", "2FA", {"secret": secret}, site="http://127.0.0.1:8000")
        assert v.keychain.get(login.handle) == {"username": "me", "password": "p@ss w0rd \"'\\ canary"}
        assert v.keychain.get(card.handle)["number"] == "4242424242424242"
        assert v.keychain.get(code.handle)["secret"] == secret
        assert v.fill_values(code.handle, "http://127.0.0.1:8000/")[1]["code"] == totp(secret)
        # nothing landed in the owner's keychains: the default search list does not have these handles
        for item in (login, card, code):
            r = subprocess.run([vlt.SECURITY, "find-generic-password", "-s", vlt.SERVICE, "-a", item.handle],
                               capture_output=True)
            assert r.returncode != 0, item.handle
        for item in (login, card, code):
            assert v.remove(item.handle)
        dump = subprocess.run([vlt.SECURITY, "dump-keychain", path], capture_output=True, text=True).stdout
        assert vlt.SERVICE not in dump and v.items() == []
    finally:
        subprocess.run([vlt.SECURITY, "delete-keychain", path], capture_output=True)
    assert json.loads((tmp_path / "vault.json").read_text()) == {"items": []}
