"""lite_tls.py: the local CA and the LAN door's certificate.

Real certificates in a temporary folder, and real TLS handshakes on an ephemeral loopback port, made the way the LAN
door makes them: a plain TCP accept, then ``writer.start_tls``. Clients verify strictly (VERIFY_X509_STRICT, which
Python 3.13 sets by default), as a browser would."""
from __future__ import annotations

import asyncio
import contextlib
import ipaddress
import logging
import os
import plistlib
import ssl
import stat
import threading
import uuid
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any, Optional

import pytest
from cryptography import x509
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import ec
from cryptography.x509.oid import ExtendedKeyUsageOID, NameOID

from eve_bridge import lite_tls
from eve_bridge.lite_tls import LocalTls

HOST = "Kitchen-Mac"
LAN = "192.168.1.20"

# The constraints as the spec writes them (docs/plans/eve-lite.md M5), independent of the code's own list.
SPEC_DNS = {"local", "localhost"}
SPEC_NETS = {"10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16", "169.254.0.0/16", "127.0.0.0/8", "fc00::/7",
             "fe80::/10"}


@pytest.fixture
def tls(tmp_path: Path) -> LocalTls:
    t = LocalTls(tmp_path / "web-tls")
    assert t.refresh([LAN], HOST) is True
    return t


def cert(path: Path) -> x509.Certificate:
    return x509.load_pem_x509_certificate(path.read_bytes())


def ext(c: x509.Certificate, kind: type) -> Any:
    return c.extensions.get_extension_for_class(kind)


def san(c: x509.Certificate) -> tuple[list[str], list[str]]:
    names = ext(c, x509.SubjectAlternativeName).value
    return names.get_values_for_type(x509.DNSName), [str(a) for a in names.get_values_for_type(x509.IPAddress)]


def permitted(name: x509.GeneralName) -> bool:
    """Does ``name`` satisfy the spec's constraints (RFC 5280: a DNS constraint covers itself and its subdomains)?"""
    if isinstance(name, x509.DNSName):
        value = name.value.lower()
        return any(value == d or value.endswith("." + d) for d in SPEC_DNS)
    if isinstance(name, x509.IPAddress):
        return any(name.value in ipaddress.ip_network(n) for n in SPEC_NETS)
    return False


# ---- the CA ---------------------------------------------------------------------------------------------

def test_the_ca_is_name_constrained_to_local_names_and_private_ranges(tls: LocalTls) -> None:
    ca = cert(tls.ca_cert_path)
    limits = ext(ca, x509.NameConstraints)
    assert limits.critical
    assert limits.value.excluded_subtrees is None
    assert set(limits.value.permitted_subtrees) == (
        {x509.DNSName(d) for d in SPEC_DNS} | {x509.IPAddress(ipaddress.ip_network(n)) for n in SPEC_NETS})


def test_the_ca_signs_leaves_only_and_names_this_mac(tls: LocalTls) -> None:
    ca = cert(tls.ca_cert_path)
    basic = ext(ca, x509.BasicConstraints)
    assert basic.critical and basic.value.ca is True and basic.value.path_length == 0
    usage = ext(ca, x509.KeyUsage)
    assert usage.critical and usage.value.key_cert_sign and usage.value.crl_sign
    assert not usage.value.digital_signature and not usage.value.key_encipherment
    assert isinstance(ca.public_key(), ec.EllipticCurvePublicKey) and ca.public_key().curve.name == "secp256r1"
    assert ca.subject.get_attributes_for_oid(NameOID.COMMON_NAME)[0].value == "eve lite local CA (Kitchen-Mac)"
    assert ca.subject == ca.issuer
    life = ca.not_valid_after_utc - ca.not_valid_before_utc
    assert timedelta(days=5 * 365) <= life <= timedelta(days=5 * 365 + 2)
    ext(ca, x509.SubjectKeyIdentifier)                              # VERIFY_X509_STRICT wants it on a CA


def test_a_long_mac_name_still_fits_the_ca_common_name(tmp_path: Path) -> None:
    label = "M" + "a" * 61 + "c"                                     # 63 characters: the longest DNS label
    t = LocalTls(tmp_path / "web-tls")
    t.refresh([LAN], label)
    cn = cert(t.ca_cert_path).subject.get_attributes_for_oid(NameOID.COMMON_NAME)[0].value
    assert len(cn) <= 64 and cn.startswith("eve lite local CA (Maaa") and cn.endswith(")")
    assert f"{label.lower()}.local" in t.names()


# ---- the leaf -------------------------------------------------------------------------------------------

def test_the_leaf_names_this_mac_and_serves_only_as_a_server(tls: LocalTls) -> None:
    ca, leaf = cert(tls.ca_cert_path), cert(tls.leaf_cert_path)
    dns, ips = san(leaf)
    assert dns == ["kitchen-mac.local", "localhost"]
    assert ips == ["127.0.0.1", LAN]
    assert list(ext(leaf, x509.ExtendedKeyUsage).value) == [ExtendedKeyUsageOID.SERVER_AUTH]
    basic = ext(leaf, x509.BasicConstraints)
    assert basic.critical and basic.value.ca is False
    usage = ext(leaf, x509.KeyUsage)
    assert usage.critical and usage.value.digital_signature and not usage.value.key_cert_sign
    assert leaf.not_valid_after_utc - leaf.not_valid_before_utc <= timedelta(days=397)
    assert leaf.not_valid_before_utc <= datetime.now(timezone.utc) < leaf.not_valid_after_utc
    assert isinstance(leaf.public_key(), ec.EllipticCurvePublicKey) and leaf.public_key().curve.name == "secp256r1"
    leaf.verify_directly_issued_by(ca)
    ext(leaf, x509.AuthorityKeyIdentifier)                          # VERIFY_X509_STRICT wants it on a leaf
    assert tls.names() == {"kitchen-mac.local", "localhost", "127.0.0.1", LAN}


def test_every_leaf_name_is_inside_the_ca_constraints(tmp_path: Path) -> None:
    t = LocalTls(tmp_path / "web-tls")
    t.refresh(["8.8.8.8", "fd00::5", "fe80::1", "not an address", None, "", LAN, "127.0.0.5", "100.64.0.1",
               "172.32.0.1", " 10.0.0.7 ", "169.254.3.4"], HOST)
    leaf = cert(t.leaf_cert_path)
    names = list(ext(leaf, x509.SubjectAlternativeName).value)
    assert all(permitted(n) for n in names)
    assert san(leaf)[1] == ["127.0.0.1", LAN, "10.0.0.7", "169.254.3.4"]
    # Control: the checker refuses what the CA must never vouch for.
    assert not permitted(x509.DNSName("bank.example.com")) and not permitted(x509.DNSName("notlocal"))
    assert not permitted(x509.IPAddress(ipaddress.ip_address("8.8.8.8")))


def test_mac_names_are_one_dns_label_in_any_case(tls: LocalTls) -> None:
    assert tls.refresh([LAN], "KITCHEN-MAC") is False               # the same name: certificates are case-blind
    assert tls.refresh([LAN], "Kitchen-Mac.local") is False
    assert tls.refresh([LAN], "") is False                          # unknown: the leaf keeps its name
    assert tls.refresh([LAN], "not one label") is False
    assert tls.refresh([LAN], "Den-Mac") is True                    # renamed: the QR code's .local name changes
    assert san(cert(tls.leaf_cert_path))[0] == ["den-mac.local", "localhost"]


def test_a_mac_without_a_bonjour_name_still_gets_a_leaf(tmp_path: Path) -> None:
    t = LocalTls(tmp_path / "web-tls")
    assert t.refresh([], "") is True
    assert t.names() == {"localhost", "127.0.0.1"}
    assert cert(t.ca_cert_path).subject.get_attributes_for_oid(NameOID.COMMON_NAME)[0].value == "eve lite local CA"


# ---- the files ------------------------------------------------------------------------------------------

def mode(path: Path) -> int:
    return stat.S_IMODE(os.stat(path).st_mode)


def test_keys_are_0600_and_the_folder_0700(tls: LocalTls) -> None:
    assert mode(tls.root) == 0o700
    for path in (tls.ca_key_path, tls.leaf_key_path, tls.ca_cert_path, tls.leaf_cert_path):
        assert mode(path) == 0o600
    assert b"PRIVATE KEY" in tls.ca_key_path.read_bytes() and b"PRIVATE KEY" in tls.leaf_key_path.read_bytes()
    assert sorted(p.name for p in tls.root.iterdir()) == ["ca.key", "ca.pem", "leaf.key", "leaf.pem"]


def test_modes_hold_under_any_umask_and_loose_ones_are_tightened(tmp_path: Path) -> None:
    root = tmp_path / "web-tls"
    root.mkdir(mode=0o755)
    os.chmod(root, 0o755)
    old = os.umask(0)
    try:
        t = LocalTls(root)
        t.refresh([LAN], HOST)
    finally:
        os.umask(old)
    assert mode(root) == 0o700 and mode(t.ca_key_path) == 0o600 and mode(t.leaf_key_path) == 0o600
    old = os.umask(0o277)                                            # a strict umask would leave keys 0400
    try:
        strict = LocalTls(tmp_path / "strict")
        strict.refresh([LAN], HOST)
    finally:
        os.umask(old)
    assert mode(strict.root) == 0o700
    assert {mode(p) for p in strict.root.iterdir()} == {0o600}
    os.chmod(t.leaf_key_path, 0o644)
    os.chmod(root, 0o755)
    assert t.refresh([LAN], HOST) is False                          # nothing to issue, but the modes come back
    assert mode(root) == 0o700 and mode(t.leaf_key_path) == 0o600


def test_an_unchanged_address_set_does_not_reissue_and_a_new_address_does(tls: LocalTls) -> None:
    ca_bytes, leaf_bytes = tls.ca_cert_path.read_bytes(), tls.leaf_cert_path.read_bytes()
    assert tls.refresh([LAN], HOST) is False
    assert tls.refresh([], HOST) is False                           # an address went away: kept, no reissue
    assert tls.leaf_cert_path.read_bytes() == leaf_bytes
    assert tls.refresh(["10.0.0.7", LAN], HOST) is True             # a new address
    assert san(cert(tls.leaf_cert_path))[1] == ["127.0.0.1", "10.0.0.7", LAN]
    assert tls.refresh([LAN, "10.0.0.7"], HOST) is False            # the same set in another order
    assert tls.refresh(["10.0.0.7"], HOST) is False
    assert tls.ca_cert_path.read_bytes() == ca_bytes                # the CA phones installed never changes


def test_the_addresses_are_a_union_capped_at_16_newest_first(tmp_path: Path) -> None:
    t = LocalTls(tmp_path / "web-tls")
    first = [f"10.0.0.{i}" for i in range(1, 11)]
    second = [f"10.0.1.{i}" for i in range(1, 11)]
    t.refresh(first, HOST)
    assert t.refresh(second, HOST) is True
    ips = san(cert(t.leaf_cert_path))[1]
    assert ips == ["127.0.0.1"] + second + first[:6]
    assert t.refresh(second, HOST) is False                         # at the cap, no reissue loop
    many = [f"192.168.5.{i}" for i in range(1, 21)]
    assert t.refresh(many, HOST) is True
    assert san(cert(t.leaf_cert_path))[1] == ["127.0.0.1"] + many[:16]
    assert t.refresh(many, HOST) is False


def test_damaged_or_mismatched_files_are_made_again(tls: LocalTls) -> None:
    tls.leaf_cert_path.write_bytes(b"not a certificate")
    assert tls.refresh([LAN], HOST) is True
    other = ec.generate_private_key(ec.SECP256R1())
    tls.leaf_key_path.write_bytes(other.private_bytes(serialization.Encoding.PEM, serialization.PrivateFormat.PKCS8,
                                                      serialization.NoEncryption()))
    assert tls.refresh([LAN], HOST) is True                         # a key that is not the leaf's
    old_ca = tls.ca_cert_path.read_bytes()
    tls.ca_key_path.write_bytes(b"garbage")
    assert tls.refresh([LAN], HOST) is True
    assert tls.ca_cert_path.read_bytes() != old_ca                  # a new CA, and a leaf it signed
    cert(tls.leaf_cert_path).verify_directly_issued_by(cert(tls.ca_cert_path))
    assert tls.names() == {"kitchen-mac.local", "localhost", "127.0.0.1", LAN}     # the names carried over


def test_a_leaf_near_its_end_is_renewed_with_its_names(tls: LocalTls, monkeypatch: pytest.MonkeyPatch) -> None:
    tls.refresh(["10.0.0.7"], HOST)
    ca_bytes = tls.ca_cert_path.read_bytes()
    later = datetime.now(timezone.utc) + timedelta(days=380)
    monkeypatch.setattr(lite_tls, "_now", lambda: later)
    assert tls.refresh([], "") is True
    leaf = cert(tls.leaf_cert_path)
    assert leaf.not_valid_before_utc <= later < leaf.not_valid_after_utc
    assert san(leaf) == (["kitchen-mac.local", "localhost"], ["127.0.0.1", "10.0.0.7", LAN])
    assert tls.ca_cert_path.read_bytes() == ca_bytes
    assert tls.refresh([], "") is False


def test_the_ca_is_renewed_before_it_ends_and_never_for_a_clock_that_jumped_back(
        tls: LocalTls, monkeypatch: pytest.MonkeyPatch) -> None:
    ca = cert(tls.ca_cert_path)
    ca_bytes = tls.ca_cert_path.read_bytes()
    monkeypatch.setattr(lite_tls, "_now", lambda: ca.not_valid_before_utc - timedelta(days=2))
    tls.refresh([LAN], HOST)
    assert tls.ca_cert_path.read_bytes() == ca_bytes                # the CA phones installed stays
    near_end = ca.not_valid_after_utc - timedelta(days=10)
    monkeypatch.setattr(lite_tls, "_now", lambda: near_end)
    assert tls.refresh([LAN], HOST) is True
    new_ca = cert(tls.ca_cert_path)
    assert tls.ca_cert_path.read_bytes() != ca_bytes and new_ca.not_valid_after_utc > ca.not_valid_after_utc
    leaf = cert(tls.leaf_cert_path)
    leaf.verify_directly_issued_by(new_ca)
    assert leaf.not_valid_after_utc <= new_ca.not_valid_after_utc


def test_logs_carry_no_name_and_no_address(tmp_path: Path, caplog: pytest.LogCaptureFixture) -> None:
    with caplog.at_level(logging.DEBUG, logger="eve_bridge.lite_tls"):
        t = LocalTls(tmp_path / "web-tls")
        t.refresh([LAN], HOST)
        t.refresh(["10.0.0.7"], "Den-Mac")
    assert "issued the LAN certificate" in caplog.text               # control: the lines were captured
    for secret_ish in (HOST, HOST.lower(), "Den-Mac", "den-mac", LAN, "10.0.0.7"):
        assert secret_ish not in caplog.text


# ---- the downloads --------------------------------------------------------------------------------------

def values(node: Any) -> list[Any]:
    """Every leaf value of a parsed plist."""
    if isinstance(node, dict):
        return [v for item in node.values() for v in values(item)]
    if isinstance(node, list):
        return [v for item in node for v in values(item)]
    return [node]


def key_der(path: Path) -> bytes:
    key = serialization.load_pem_private_key(path.read_bytes(), password=None)
    return key.private_bytes(serialization.Encoding.DER, serialization.PrivateFormat.PKCS8,
                             serialization.NoEncryption())


def carries_key(profile: Any, keys: list[bytes]) -> bool:
    for v in values(profile):
        blob = v if isinstance(v, bytes) else str(v).encode()
        if b"PRIVATE KEY" in blob or any(k in blob for k in keys):
            return True
    return False


def test_the_downloads_carry_no_private_key(tls: LocalTls) -> None:
    keys = [key_der(tls.ca_key_path), key_der(tls.leaf_key_path)]
    pem = tls.ca_pem()
    assert pem.startswith(b"-----BEGIN CERTIFICATE-----") and b"PRIVATE KEY" not in pem
    assert x509.load_pem_x509_certificate(pem) == cert(tls.ca_cert_path)
    raw = tls.mobileconfig()
    assert b"PRIVATE KEY" not in raw
    profile = plistlib.loads(raw)
    assert not carries_key(profile, keys)
    # Control: the scan finds a key that is there.
    assert carries_key({"PayloadContent": [{"PayloadContent": keys[0]}]}, keys)


def test_a_key_appended_to_ca_pem_never_reaches_the_download(tls: LocalTls) -> None:
    tls.ca_cert_path.write_bytes(tls.ca_cert_path.read_bytes() + tls.ca_key_path.read_bytes())
    assert b"PRIVATE KEY" in tls.ca_cert_path.read_bytes()           # control: the file on disk now has one
    assert b"PRIVATE KEY" not in tls.ca_pem()
    assert b"PRIVATE KEY" not in tls.mobileconfig()


def test_the_mobileconfig_installs_this_ca_as_a_root(tls: LocalTls, tmp_path: Path) -> None:
    raw = tls.mobileconfig()
    assert raw == tls.mobileconfig()                                # stable from one download to the next
    profile = plistlib.loads(raw)
    assert profile["PayloadType"] == "Configuration" and profile["PayloadVersion"] == 1
    assert profile["PayloadRemovalDisallowed"] is False
    (root,) = profile["PayloadContent"]
    assert root["PayloadType"] == "com.apple.security.root" and root["PayloadVersion"] == 1
    assert root["PayloadContent"] == cert(tls.ca_cert_path).public_bytes(serialization.Encoding.DER)
    assert root["PayloadDisplayName"] == profile["PayloadDisplayName"] == "eve lite local CA (Kitchen-Mac)"
    uuids = {uuid.UUID(profile["PayloadUUID"]), uuid.UUID(root["PayloadUUID"])}
    assert len(uuids) == 2
    assert root["PayloadIdentifier"].startswith(profile["PayloadIdentifier"] + ".")
    other = LocalTls(tmp_path / "other-mac")                         # another Mac's profile never replaces it
    other.refresh([LAN], HOST)
    theirs = plistlib.loads(other.mobileconfig())
    assert theirs["PayloadIdentifier"] != profile["PayloadIdentifier"]
    assert theirs["PayloadUUID"] != profile["PayloadUUID"]


# ---- handshakes -----------------------------------------------------------------------------------------

def client(ca_pem: bytes) -> ssl.SSLContext:
    ctx = ssl.create_default_context(cadata=ca_pem.decode())
    ctx.verify_flags |= ssl.VERIFY_X509_STRICT
    ctx.set_alpn_protocols(["h2", "http/1.1"])                      # as a browser offers
    return ctx


def handshake(server: ssl.SSLContext, client_ctx: ssl.SSLContext, server_hostname: str) -> tuple[str, Any, Any]:
    """One connection to a door-shaped server on an ephemeral loopback port: plain accept, then start_tls, then
    one line echoed. Returns (the line, the ALPN protocol, the TLS version) as the client saw them."""
    async def go() -> tuple[str, Any, Any]:
        async def door(reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
            try:
                await writer.start_tls(server)
                line = await reader.readline()
                writer.write(b"echo " + line)
                await writer.drain()
            except (OSError, ssl.SSLError, asyncio.IncompleteReadError):
                pass                                                # the client refused the certificate
            finally:
                writer.close()

        srv = await asyncio.start_server(door, "127.0.0.1", 0)
        port = srv.sockets[0].getsockname()[1]
        try:
            reader, writer = await asyncio.wait_for(
                asyncio.open_connection("127.0.0.1", port, ssl=client_ctx, server_hostname=server_hostname), 10)
            writer.write(b"hello\n")
            await writer.drain()
            line = await asyncio.wait_for(reader.readline(), 10)
            tls_obj = writer.get_extra_info("ssl_object")
            out = (line.decode(), tls_obj.selected_alpn_protocol(), tls_obj.version())
            writer.close()
            with contextlib.suppress(Exception):
                await writer.wait_closed()
            return out
        finally:
            srv.close()
            with contextlib.suppress(Exception):
                await asyncio.wait_for(srv.wait_closed(), 5)

    return asyncio.run(go())


@pytest.mark.parametrize("name", ["localhost", "kitchen-mac.local", "127.0.0.1", LAN])
def test_a_client_that_trusts_ca_pem_completes_the_handshake(tls: LocalTls, name: str) -> None:
    line, alpn, version = handshake(tls.context(), client(tls.ca_pem()), name)
    assert line == "echo hello\n"
    assert alpn == "http/1.1"
    assert version in ("TLSv1.2", "TLSv1.3")


def test_the_server_context_refuses_old_tls(tls: LocalTls) -> None:
    ctx = tls.context()
    assert ctx.minimum_version == ssl.TLSVersion.TLSv1_2
    assert ctx.verify_mode == ssl.CERT_NONE                          # a server: browsers bring no certificate
    assert tls.context() is ctx                                      # one object until a reissue


def test_a_client_that_does_not_trust_the_ca_or_asks_another_name_is_refused(tls: LocalTls, tmp_path: Path) -> None:
    stranger = LocalTls(tmp_path / "another-mac")
    stranger.refresh([LAN], HOST)
    with pytest.raises(ssl.SSLCertVerificationError):
        handshake(tls.context(), client(stranger.ca_pem()), "localhost")
    with pytest.raises(ssl.SSLCertVerificationError):
        handshake(tls.context(), client(tls.ca_pem()), "den-mac.local")
    with pytest.raises(ssl.SSLCertVerificationError):
        handshake(tls.context(), client(tls.ca_pem()), "192.168.1.99")


def rogue(tls: LocalTls, tmp_path: Path, names: list[x509.GeneralName]) -> ssl.SSLContext:
    """A server context for a leaf signed with the eve CA's own key, as a thief of that key would make one."""
    ca = cert(tls.ca_cert_path)
    ca_key = serialization.load_pem_private_key(tls.ca_key_path.read_bytes(), password=None)
    key = ec.generate_private_key(ec.SECP256R1())
    now = datetime.now(timezone.utc)
    leaf = (
        x509.CertificateBuilder()
        .subject_name(x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, "rogue")]))
        .issuer_name(ca.subject)
        .public_key(key.public_key())
        .serial_number(x509.random_serial_number())
        .not_valid_before(now - timedelta(hours=1))
        .not_valid_after(now + timedelta(days=30))
        .add_extension(x509.BasicConstraints(ca=False, path_length=None), critical=True)
        .add_extension(x509.ExtendedKeyUsage([ExtendedKeyUsageOID.SERVER_AUTH]), critical=False)
        .add_extension(x509.SubjectAlternativeName(names), critical=False)
        .add_extension(x509.SubjectKeyIdentifier.from_public_key(key.public_key()), critical=False)
        .add_extension(x509.AuthorityKeyIdentifier.from_issuer_public_key(ca_key.public_key()), critical=False)
        .sign(ca_key, hashes.SHA256())
    )
    folder = tmp_path / f"rogue-{len(list(tmp_path.glob('rogue-*')))}"
    folder.mkdir()
    (folder / "leaf.pem").write_bytes(leaf.public_bytes(serialization.Encoding.PEM))
    (folder / "leaf.key").write_bytes(key.private_bytes(serialization.Encoding.PEM, serialization.PrivateFormat.PKCS8,
                                                        serialization.NoEncryption()))
    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    ctx.load_cert_chain(folder / "leaf.pem", folder / "leaf.key")
    return ctx


@pytest.mark.parametrize("name, host", [(x509.DNSName("bank.example.com"), "bank.example.com"),
                                        (x509.IPAddress(ipaddress.ip_address("8.8.8.8")), "8.8.8.8")])
def test_the_ca_cannot_vouch_for_a_real_site_even_with_its_own_key(tls: LocalTls, tmp_path: Path, name: Any,
                                                                   host: str) -> None:
    with pytest.raises(ssl.SSLCertVerificationError) as refused:
        handshake(rogue(tls, tmp_path, [name]), client(tls.ca_pem()), host)
    assert "subtree" in str(refused.value.verify_message)            # a name constraint, not some other failure
    # Control: the same forgery for a local name passes, so the constraint is what stopped the one above.
    line, _alpn, _version = handshake(rogue(tls, tmp_path, [x509.DNSName("rogue.local")]), client(tls.ca_pem()),
                                      "rogue.local")
    assert line == "echo hello\n"


def test_the_context_follows_a_reissue(tls: LocalTls) -> None:
    before = tls.context()
    assert tls.refresh(["10.0.0.7"], HOST) is True
    after = tls.context()
    assert after is not before
    line, _alpn, _version = handshake(after, client(tls.ca_pem()), "10.0.0.7")
    assert line == "echo hello\n"
    assert "10.0.0.7" in tls.names()


def test_the_context_renews_a_leaf_that_ran_down_while_serving(tls: LocalTls,
                                                                monkeypatch: pytest.MonkeyPatch) -> None:
    before = tls.context()
    old_leaf = tls.leaf_cert_path.read_bytes()
    later = datetime.now(timezone.utc) + timedelta(days=380)
    monkeypatch.setattr(lite_tls, "_now", lambda: later)
    assert tls.context() is before                                  # checked at most once a day
    monkeypatch.setattr(lite_tls, "RECHECK_SECS", -1.0)
    after = tls.context()
    assert after is not before and tls.leaf_cert_path.read_bytes() != old_leaf
    assert tls.names() == {"kitchen-mac.local", "localhost", "127.0.0.1", LAN}


def test_two_objects_on_one_folder_agree(tls: LocalTls) -> None:
    before = tls.context()
    other = LocalTls(tls.root)
    assert other.refresh(["10.0.0.7"], HOST) is True
    assert tls.context() is not before                              # it noticed the other's new leaf
    assert "10.0.0.7" in tls.names()
    line, _alpn, _version = handshake(tls.context(), client(tls.ca_pem()), "10.0.0.7")
    assert line == "echo hello\n"


def test_concurrent_first_use_makes_one_ca_and_one_leaf(tmp_path: Path) -> None:
    root = tmp_path / "web-tls"
    barrier = threading.Barrier(8)
    issued: list[bool] = []

    def first_use() -> None:
        t = LocalTls(root)
        barrier.wait()
        issued.append(t.refresh([LAN], HOST))

    threads = [threading.Thread(target=first_use) for _ in range(8)]
    for th in threads:
        th.start()
    for th in threads:
        th.join(30)
    assert sorted(issued) == [False] * 7 + [True]
    ca, leaf = cert(root / "ca.pem"), cert(root / "leaf.pem")
    leaf.verify_directly_issued_by(ca)
    for c, k in ((ca, root / "ca.key"), (leaf, root / "leaf.key")):
        key = serialization.load_pem_private_key(k.read_bytes(), password=None)
        assert c.public_key().public_numbers() == key.public_key().public_numbers()


def test_every_method_works_before_any_refresh(tmp_path: Path) -> None:
    for call in ("context", "names", "ca_pem", "mobileconfig"):
        t = LocalTls(tmp_path / call)
        result: Optional[Any] = getattr(t, call)()
        assert result
        assert t.ca_key_path.exists() and t.leaf_key_path.exists()
    t = LocalTls(tmp_path / "names")
    assert t.names() == {"localhost", "127.0.0.1"}
