"""HTTPS for eve lite's LAN door: a local CA that can vouch only for local names, and the certificate the door
serves (docs/plans/eve-lite.md M5).

A phone's browser lets a page use the microphone only in a secure context, and a phone reaches the Mac over the
LAN, so the LAN listener (lite_web.py) speaks TLS. No public CA signs for ``<name>.local`` or a private address, so
the Mac makes its own, under ``~/.config/eve-bridge/web-tls/``:

* **The CA** (``ca.pem``, ``ca.key``): EC P-256, five years, CN "eve lite local CA (<LocalHostName>)". Critical
  name constraints permit only DNS names under ``local`` and ``localhost`` and the private, link-local and loopback
  address ranges: a client that applies them trusts nothing else from this CA, even signed with its stolen key
  (OpenSSL does; test_lite_tls.py forges one). Path length 0: it signs leaves only. Its key never leaves the Mac.
* **The leaf** (``leaf.pem``, ``leaf.key``): EC P-256, 397 days, serverAuth, for ``<LocalHostName>.local``,
  ``localhost``, ``127.0.0.1`` and every private IPv4 address the Mac has had (newest first, at most 16).
  Reissuing brings the browser warning back on every phone that clicked through it instead of installing the CA,
  so it happens only when a new address or name appears, or the leaf has a month left.
* **The downloads**: ``ca_pem()`` for ``ca.crt``, and ``mobileconfig()``, an unsigned profile with one
  ``com.apple.security.root`` payload. Both are encoded from the parsed CA certificate, so neither can carry a key.

The folder is 0700 and every file in it 0600, each written atomically. A file that is missing, damaged or does not
match its key is made again rather than trusted. Logs carry counts and reasons, never a name or an address: the
Mac's name usually carries its owner's.
"""

from __future__ import annotations

import contextlib
import hashlib
import ipaddress
import logging
import os
import plistlib
import ssl
import stat
import threading
import time
import uuid
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any, Iterable, Optional

from cryptography import x509
from cryptography.exceptions import InvalidSignature, UnsupportedAlgorithm
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import ec
from cryptography.x509.oid import ExtendedKeyUsageOID, NameOID

from .lan_net import LABEL

log = logging.getLogger(__name__)

CA_DAYS = 5 * 365
LEAF_DAYS = 397                       # under Apple's caps on a TLS server certificate: 398 days, 825 for any CA
RENEW_DAYS = 30                       # a certificate with this little left is replaced
BACKDATE = timedelta(hours=1)         # a phone whose clock runs a little behind still takes a fresh certificate
RECHECK_SECS = 86400.0                # context() looks at the leaf's dates again after this long
MAX_ADDRESSES = 16                    # private IPv4 addresses in the leaf, newest first
LOOPBACK = "127.0.0.1"

PERMITTED_DNS = ("local", "localhost")
PERMITTED_NETS = ("10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16", "169.254.0.0/16", "127.0.0.0/8",
                  "fc00::/7", "fe80::/10")
# The leaf's own addresses: the private IPv4 ranges among the permitted ones (loopback is 127.0.0.1 alone). An
# address outside the constraints would make a phone that installed the CA refuse the whole certificate.
SAN_NETS = tuple(ipaddress.IPv4Network(n) for n in ("10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16",
                                                     "169.254.0.0/16"))

CA_NAME = "eve lite local CA"
LEAF_NAME = "eve lite"
PROFILE_ID = "com.github.eve-bridge.lite.ca"
PROFILE_ORG = "eve lite"
PROFILE_TEXT = ("Lets this device open eve lite on your Mac without a certificate warning. The certificate can "
                "vouch only for .local names and private network addresses. On an iPhone or iPad, also turn it on "
                "in Settings > General > About > Certificate Trust Settings.")


def _now() -> datetime:
    return datetime.now(timezone.utc)


# One lock per folder for the whole process: two LocalTls on one folder must not both make a CA at once and leave
# one's key beside the other's certificate.
_LOCKS: dict[str, threading.Lock] = {}
_LOCKS_LOCK = threading.Lock()


def _folder_lock(root: Path) -> threading.Lock:
    with _LOCKS_LOCK:
        return _LOCKS.setdefault(os.path.realpath(root), threading.Lock())


def permitted_subtrees() -> list[x509.GeneralName]:
    """What the CA may vouch for: DNS names under ``local`` and ``localhost``, and the private address ranges."""
    names: list[x509.GeneralName] = [x509.DNSName(n) for n in PERMITTED_DNS]
    return names + [x509.IPAddress(ipaddress.ip_network(n)) for n in PERMITTED_NETS]


def _san_addresses(addresses: Iterable[Any]) -> list[str]:
    """The private IPv4 addresses among ``addresses``, in order, each once, at most MAX_ADDRESSES. Anything else
    (IPv6, public, loopback, not an address) is dropped."""
    out: list[str] = []
    for a in addresses:
        try:
            ip = ipaddress.IPv4Address(str(a).strip())
        except ValueError:
            continue
        if any(ip in net for net in SAN_NETS) and str(ip) not in out:
            out.append(str(ip))
    return out[:MAX_ADDRESSES]


def _label(hostname: Any) -> str:
    """The LocalHostName as given ("Kitchen-Mac", or "Kitchen-Mac.local"), without ".local", or "" when it is not
    one DNS label."""
    if not isinstance(hostname, str):
        return ""
    name = hostname.strip().rstrip(".")
    if name.lower().endswith(".local"):
        name = name[: -len(".local")]
    return name if LABEL.fullmatch(name) else ""


def _write_private(path: Path, data: bytes) -> None:
    """Write ``data`` to ``path`` atomically and owner-only (0600), whatever the umask."""
    tmp = path.with_name(f".{path.name}.tmp")
    with contextlib.suppress(FileNotFoundError):
        tmp.unlink()
    fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0), 0o600)
    try:
        with os.fdopen(fd, "wb") as f:
            os.fchmod(f.fileno(), 0o600)
            f.write(data)
            f.flush()
            os.fsync(f.fileno())
        os.replace(tmp, path)
    except BaseException:
        with contextlib.suppress(OSError):
            tmp.unlink()
        raise


def _read_cert(path: Path) -> Optional[x509.Certificate]:
    try:
        return x509.load_pem_x509_certificate(path.read_bytes())
    except (OSError, ValueError):
        return None


def _read_key(path: Path) -> Optional[ec.EllipticCurvePrivateKey]:
    try:
        key = serialization.load_pem_private_key(path.read_bytes(), password=None)
    except (OSError, ValueError, TypeError, UnsupportedAlgorithm):
        return None
    return key if isinstance(key, ec.EllipticCurvePrivateKey) else None


def _spki(key: Any) -> bytes:
    return key.public_bytes(serialization.Encoding.DER, serialization.PublicFormat.SubjectPublicKeyInfo)


def _matches(cert: Optional[x509.Certificate], key: Optional[ec.EllipticCurvePrivateKey]) -> bool:
    return cert is not None and key is not None and _spki(cert.public_key()) == _spki(key.public_key())


def _ca_usable(ca: x509.Certificate, now: datetime) -> bool:
    """A CA made here: still a CA, constrained exactly as this code constrains it, with more than RENEW_DAYS left.
    Its start date is not checked: a Mac clock that jumped back must not throw away the CA phones installed."""
    try:
        basic = ca.extensions.get_extension_for_class(x509.BasicConstraints)
        limits = ca.extensions.get_extension_for_class(x509.NameConstraints)
    except x509.ExtensionNotFound:
        return False
    return (basic.critical and basic.value.ca and basic.value.path_length == 0
            and limits.critical and not limits.value.excluded_subtrees
            and set(limits.value.permitted_subtrees or ()) == set(permitted_subtrees())
            and now < ca.not_valid_after_utc - timedelta(days=RENEW_DAYS))


def _leaf_usable(leaf: x509.Certificate, ca: x509.Certificate, now: datetime) -> bool:
    """Signed by this CA, already valid, and with more than RENEW_DAYS left."""
    try:
        leaf.verify_directly_issued_by(ca)
    except (ValueError, TypeError, InvalidSignature):
        return False
    return leaf.not_valid_before_utc <= now < leaf.not_valid_after_utc - timedelta(days=RENEW_DAYS)


def _san(cert: Optional[x509.Certificate]) -> Optional[x509.SubjectAlternativeName]:
    if cert is None:
        return None
    try:
        return cert.extensions.get_extension_for_class(x509.SubjectAlternativeName).value
    except x509.ExtensionNotFound:
        return None


def _leaf_names(leaf: Optional[x509.Certificate]) -> tuple[str, list[str]]:
    """The leaf's .local label (lower case) and its private addresses, newest first (127.0.0.1 left out)."""
    san = _san(leaf)
    if san is None:
        return "", []
    host = next((d[: -len(".local")].lower() for d in san.get_values_for_type(x509.DNSName)
                 if d.lower().endswith(".local")), "")
    return host, _san_addresses(san.get_values_for_type(x509.IPAddress))


class LocalTls:
    """The CA and the LAN door's certificate in ``root`` (``~/.config/eve-bridge/web-tls``). Every method makes
    whatever is missing first, so the door always has a certificate to serve. Thread-safe, and two objects on one
    folder agree: each notices a leaf the other issued."""

    def __init__(self, root: Path | str) -> None:
        self.root = Path(root)
        self.ca_cert_path = self.root / "ca.pem"
        self.ca_key_path = self.root / "ca.key"
        self.leaf_cert_path = self.root / "leaf.pem"
        self.leaf_key_path = self.root / "leaf.key"
        self._lock = _folder_lock(self.root)
        self._ctx: Optional[ssl.SSLContext] = None
        self._names: frozenset[str] = frozenset()
        self._made_from: Optional[tuple[int, int]] = None    # the leaf file the context and the names came from
        self._checked = 0.0

    # ---- the contract ------------------------------------------------------------------------------------

    def refresh(self, addresses: Iterable[str], hostname: str) -> bool:
        """Make sure the CA and the leaf exist, are sound and name this Mac: ``hostname`` is its LocalHostName ("" when
        unknown: the leaf keeps the name it has), ``addresses`` its private IPv4 addresses, best first (anything
        else is ignored). True when a certificate was issued; an address set the leaf already holds, in any order,
        or one that lost an address, issues nothing."""
        given = [addresses] if isinstance(addresses, str) else list(addresses or ())
        with self._lock:
            return self._refresh(given, hostname)

    def context(self) -> ssl.SSLContext:
        """The LAN door's server context, for ``writer.start_tls``: TLS 1.2 or later, HTTP/1.1, the current leaf. The
        same object until the leaf changes; once a day it checks the leaf is not due for renewal."""
        with self._lock:
            due = time.monotonic() - self._checked > RECHECK_SECS
            ctx = self._current(due)
            if due:
                self._checked = time.monotonic()
            return ctx

    def names(self) -> set[str]:
        """Every name the leaf holds: DNS names in lower case ("kitchen-mac.local", "localhost") and addresses
        ("127.0.0.1", "192.168.1.20"), for the LAN door's Host allowlist."""
        with self._lock:
            self._current(False)
            return set(self._names)

    def ca_pem(self) -> bytes:
        """The CA certificate as PEM, for ``ca.crt``."""
        return self._ca().public_bytes(serialization.Encoding.PEM)

    def mobileconfig(self) -> bytes:
        """An unsigned configuration profile that installs the CA on an iPhone, iPad or Mac
        (``com.apple.security.root``). Its identifiers and UUIDs come from the CA's fingerprint: the same for every
        download of one CA, and different for each Mac, so two Macs' profiles never replace each other."""
        ca = self._ca()
        der = ca.public_bytes(serialization.Encoding.DER)
        ident = f"{PROFILE_ID}.{hashlib.sha256(der).hexdigest()[:16]}"
        names = ca.subject.get_attributes_for_oid(NameOID.COMMON_NAME)
        title = str(names[0].value) if names else CA_NAME
        root = {
            "PayloadCertificateFileName": "eve-lite-ca.cer",
            "PayloadContent": der,
            "PayloadDescription": "Adds eve lite's local certificate authority.",
            "PayloadDisplayName": title,
            "PayloadIdentifier": f"{ident}.root",
            "PayloadType": "com.apple.security.root",
            "PayloadUUID": str(uuid.uuid5(uuid.NAMESPACE_DNS, f"{ident}.root")).upper(),
            "PayloadVersion": 1,
        }
        profile = {
            "PayloadContent": [root],
            "PayloadDescription": PROFILE_TEXT,
            "PayloadDisplayName": title,
            "PayloadIdentifier": ident,
            "PayloadOrganization": PROFILE_ORG,
            "PayloadRemovalDisallowed": False,
            "PayloadType": "Configuration",
            "PayloadUUID": str(uuid.uuid5(uuid.NAMESPACE_DNS, ident)).upper(),
            "PayloadVersion": 1,
        }
        return plistlib.dumps(profile, fmt=plistlib.FMT_XML)

    # ---- the files ---------------------------------------------------------------------------------------

    def _ca(self) -> x509.Certificate:
        with self._lock:
            self._refresh([], "")
            ca = _read_cert(self.ca_cert_path)
        if ca is None:                                       # only if the folder changed under the lock's feet
            raise OSError("the local CA could not be read back")
        return ca

    def _leaf_stamp(self) -> Optional[tuple[int, int]]:
        try:
            st = self.leaf_cert_path.stat()
        except OSError:
            return None
        return st.st_ino, st.st_mtime_ns

    def _current(self, recheck: bool) -> ssl.SSLContext:
        """Under the lock: the files sound (made, repaired or renewed as needed), and the context and the names
        made from the leaf on disk. Cheap when nothing changed: one stat."""
        if recheck or self._ctx is None or self._leaf_stamp() != self._made_from:
            self._refresh([], "")
            stamp = self._leaf_stamp()
            if self._ctx is None or stamp != self._made_from:
                san = _san(_read_cert(self.leaf_cert_path))
                found: set[str] = set()
                if san is not None:
                    found |= {d.lower() for d in san.get_values_for_type(x509.DNSName)}
                    found |= {str(ip) for ip in san.get_values_for_type(x509.IPAddress)}
                self._ctx = self._server_context()
                self._names = frozenset(found)
                self._made_from = stamp
        assert self._ctx is not None
        return self._ctx

    def _private_dir(self) -> None:
        """The folder 0700 and every file in it 0600, made or tightened."""
        self.root.mkdir(mode=0o700, parents=True, exist_ok=True)
        if stat.S_IMODE(self.root.stat().st_mode) != 0o700:
            os.chmod(self.root, 0o700)
        for path in (self.ca_cert_path, self.ca_key_path, self.leaf_cert_path, self.leaf_key_path):
            try:
                mode = stat.S_IMODE(path.lstat().st_mode)
            except FileNotFoundError:
                continue
            if mode != 0o600 and path.is_file() and not path.is_symlink():
                os.chmod(path, 0o600)

    def _refresh(self, addresses: list[Any], hostname: Any) -> bool:
        now = _now()
        self._private_dir()
        want = _san_addresses(addresses)
        label = _label(hostname)
        ca, ca_key = _read_cert(self.ca_cert_path), _read_key(self.ca_key_path)
        if ca is None or ca_key is None or not _matches(ca, ca_key) or not _ca_usable(ca, now):
            replaced = ca is not None
            ca, ca_key = self._make_ca(label, now)
            log.info("lite tls: %s the local CA", "replaced" if replaced else "made")
        leaf = _read_cert(self.leaf_cert_path)
        have_host, have = _leaf_names(leaf)
        host = label.lower() or have_host
        sound = (leaf is not None and _matches(leaf, _read_key(self.leaf_key_path))
                 and _leaf_usable(leaf, ca, now))
        if sound and host == have_host and all(ip in have for ip in want):
            return False
        reason = "first" if leaf is None else "new name" if sound else "renewal or repair"
        ips = (want + [ip for ip in have if ip not in want])[:MAX_ADDRESSES]
        self._make_leaf(ca, ca_key, host, ips, now)
        self._ctx = None                                      # _current makes the context and the names again
        log.info("lite tls: issued the LAN certificate (%s, %d addresses)", reason, len(ips))
        return True

    def _make_ca(self, label: str, now: datetime) -> tuple[x509.Certificate, ec.EllipticCurvePrivateKey]:
        key = ec.generate_private_key(ec.SECP256R1())
        room = 64 - len(CA_NAME) - 3                          # a common name holds at most 64 characters
        name = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME,
                                             f"{CA_NAME} ({label[:room]})" if label else CA_NAME)])
        ski = x509.SubjectKeyIdentifier.from_public_key(key.public_key())
        start = now - BACKDATE
        cert = (
            x509.CertificateBuilder()
            .subject_name(name)
            .issuer_name(name)
            .public_key(key.public_key())
            .serial_number(x509.random_serial_number())
            .not_valid_before(start)
            .not_valid_after(start + timedelta(days=CA_DAYS))
            .add_extension(x509.BasicConstraints(ca=True, path_length=0), critical=True)
            .add_extension(x509.KeyUsage(digital_signature=False, content_commitment=False, key_encipherment=False,
                                         data_encipherment=False, key_agreement=False, key_cert_sign=True,
                                         crl_sign=True, encipher_only=False, decipher_only=False), critical=True)
            .add_extension(x509.NameConstraints(permitted_subtrees=permitted_subtrees(), excluded_subtrees=None),
                           critical=True)
            .add_extension(ski, critical=False)
            .add_extension(x509.AuthorityKeyIdentifier.from_issuer_subject_key_identifier(ski), critical=False)
            .sign(key, hashes.SHA256())
        )
        # The key first: a crash between the two leaves a pair that does not match, which is made again.
        _write_private(self.ca_key_path, key.private_bytes(serialization.Encoding.PEM,
                                                            serialization.PrivateFormat.PKCS8,
                                                            serialization.NoEncryption()))
        _write_private(self.ca_cert_path, cert.public_bytes(serialization.Encoding.PEM))
        return cert, key

    def _make_leaf(self, ca: x509.Certificate, ca_key: ec.EllipticCurvePrivateKey, host: str, ips: list[str],
                   now: datetime) -> None:
        key = ec.generate_private_key(ec.SECP256R1())
        names: list[x509.GeneralName] = [x509.DNSName(f"{host}.local")] if host else []
        names += [x509.DNSName("localhost"), x509.IPAddress(ipaddress.IPv4Address(LOOPBACK))]
        names += [x509.IPAddress(ipaddress.IPv4Address(ip)) for ip in ips]
        start = now - BACKDATE
        cert = (
            x509.CertificateBuilder()
            .subject_name(x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, LEAF_NAME)]))
            .issuer_name(ca.subject)
            .public_key(key.public_key())
            .serial_number(x509.random_serial_number())
            .not_valid_before(start)
            .not_valid_after(min(start + timedelta(days=LEAF_DAYS), ca.not_valid_after_utc))
            .add_extension(x509.BasicConstraints(ca=False, path_length=None), critical=True)
            .add_extension(x509.KeyUsage(digital_signature=True, content_commitment=False, key_encipherment=False,
                                         data_encipherment=False, key_agreement=False, key_cert_sign=False,
                                         crl_sign=False, encipher_only=False, decipher_only=False), critical=True)
            .add_extension(x509.ExtendedKeyUsage([ExtendedKeyUsageOID.SERVER_AUTH]), critical=False)
            .add_extension(x509.SubjectAlternativeName(names), critical=False)
            .add_extension(x509.SubjectKeyIdentifier.from_public_key(key.public_key()), critical=False)
            .add_extension(x509.AuthorityKeyIdentifier.from_issuer_public_key(ca_key.public_key()), critical=False)
            .sign(ca_key, hashes.SHA256())
        )
        _write_private(self.leaf_key_path, key.private_bytes(serialization.Encoding.PEM,
                                                              serialization.PrivateFormat.PKCS8,
                                                              serialization.NoEncryption()))
        _write_private(self.leaf_cert_path, cert.public_bytes(serialization.Encoding.PEM))

    def _server_context(self) -> ssl.SSLContext:
        ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        ctx.minimum_version = ssl.TLSVersion.TLSv1_2
        ctx.options |= getattr(ssl, "OP_NO_RENEGOTIATION", 0)
        ctx.set_alpn_protocols(["http/1.1"])                  # the door speaks HTTP/1.1 only (LinkChat's writer)
        ctx.load_cert_chain(certfile=str(self.leaf_cert_path), keyfile=str(self.leaf_key_path))
        return ctx
