"""eve lite's view of the home network: this Mac's LAN addresses, who may knock on the LAN door, the Mac's Bonjour
name, and the macOS Local Network privilege (docs/plans/eve-lite.md M4, M5, M6).

* **Addresses.** ``private_addresses()`` lists this Mac's RFC 1918 IPv4 addresses on the interfaces a phone on the
  same Wi-Fi can reach, best first. They go into the LAN certificate (lite_tls.py), the LAN door's Host allowlist
  and the Android QR code. VPN tunnels, VM bridges, AirDrop's links and loopback are left out: a phone cannot use
  them, and every new address reissues the certificate, which brings the browser warning back.
* **The peer filter.** ``peer_allowed()`` runs on every TCP connection to the LAN door, before TLS. It admits
  loopback, RFC 1918, link-local and ULA addresses, and an address inside the prefix of one of the Mac's LAN
  interfaces: home IPv6 uses global addresses, and a phone on the same Wi-Fi shares the Mac's /64. Everything
  else is dropped. The ranges are spelled out because ``ipaddress``'s ``is_private`` also counts documentation,
  benchmarking and reserved space.
* **The Bonjour name.** ``local_hostname()`` is the LocalHostName that ``<name>.local`` answers to, read with
  ``/usr/sbin/scutil`` (one of the three programs the lite process firewall lets start, profile.py).
* **Local Network.** A LaunchAgent needs the Local Network privilege to send UDP on the LAN, though never to
  accept TCP (TN3179). ``trigger_local_network_alert()`` raises the macOS alert on purpose, at "Add a phone", with
  a UDP connect to the mDNS group that sends nothing: a home gateway is usually the LAN's DNS server, which
  TN3179 exempts, so it would raise nothing. ``probe()`` then sends one empty datagram to the phone's discard
  port and reads the answer from the error: EHOSTUNREACH or EPERM is "blocked".

Interfaces are read with getifaddrs (``ifaddr``, which aioice already brings), so nothing here starts a process
except scutil. Logs carry types and errno names, never an address or the Mac's name.
"""

from __future__ import annotations

import errno
import ipaddress
import logging
import re
import socket
import subprocess
from typing import Any, Optional, Union

log = logging.getLogger(__name__)

Address = Union[ipaddress.IPv4Address, ipaddress.IPv6Address]
Network = Union[ipaddress.IPv4Network, ipaddress.IPv6Network]

PRIVATE_V4 = tuple(ipaddress.IPv4Network(n) for n in ("10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16"))
LINK_LOCAL_V4 = ipaddress.IPv4Network("169.254.0.0/16")
ULA = ipaddress.IPv6Network("fc00::/7")
LINK_LOCAL_V6 = ipaddress.IPv6Network("fe80::/10")
LAN_PEERS: dict[int, tuple[Network, ...]] = {4: PRIVATE_V4 + (LINK_LOCAL_V4,), 6: (ULA, LINK_LOCAL_V6)}

# An interface prefix wider than this is no single home link (a mistyped netmask, a /8), so it admits no one. TCP
# keeps the rest honest: a peer that only claims an on-link address never sees the handshake's reply.
MIN_ON_LINK_PREFIX = {4: 16, 6: 48}

# Interfaces a phone on the LAN never reaches the Mac through: loopback, tunnels (VPNs, iCloud Private Relay),
# bridges (Internet Sharing, VMs, Thunderbolt Bridge), AirDrop and Sidecar links (awdl, llw), the access point
# and the Apple silicon debug interface (anpi). Physical Wi-Fi and Ethernet, and USB adapters, are en<N>.
NOT_LAN = re.compile(r"(lo|gif|stf|utun|tun|tap|ipsec|ppp|bridge|vmenet|vnic|feth|awdl|llw|anpi|ap)\d*")

SCUTIL = "/usr/sbin/scutil"
SCUTIL_SECS = 5.0
LABEL = re.compile(r"[A-Za-z0-9](?:[A-Za-z0-9-]{0,61}[A-Za-z0-9])?")      # one DNS label (RFC 1123)

MDNS_GROUP = ("224.0.0.251", 5353)
DISCARD_PORT = 9                     # RFC 863: whatever arrives is thrown away, and nothing answers
BLOCKED = (errno.EHOSTUNREACH, errno.EPERM)


def _address(addr: Any) -> Optional[Address]:
    """``addr`` as an address: a string (``fe80::1%en0`` and ``[::1]`` too), a socket peername tuple, or an
    ``ipaddress`` object. An IPv4-mapped IPv6 address (a dual-stack listener's IPv4 peer) becomes its IPv4
    address. None when it is none of these."""
    if isinstance(addr, (tuple, list)):
        addr = addr[0] if addr else None
    if isinstance(addr, (ipaddress.IPv4Address, ipaddress.IPv6Address)):
        ip: Address = addr
    elif isinstance(addr, str):
        text = addr.strip()
        if text.startswith("[") and text.endswith("]"):
            text = text[1:-1]
        try:
            ip = ipaddress.ip_address(text)              # checks a zone's form too ("fe80::1%" is no address)
        except ValueError:
            return None
    else:
        return None
    if isinstance(ip, ipaddress.IPv6Address):
        if ip.ipv4_mapped is not None:
            return ip.ipv4_mapped
        if ip.scope_id:
            ip = ipaddress.IPv6Address(int(ip))
    return ip


def _scope(addr: Any) -> str:
    """The zone of a link-local IPv6 peer (``en0`` from ``fe80::1%en0``, or a peername's scope id), or ""."""
    if isinstance(addr, (tuple, list)):
        if len(addr) >= 4 and isinstance(addr[3], int) and addr[3]:
            return str(addr[3])
        addr = addr[0] if addr else ""
    if isinstance(addr, ipaddress.IPv6Address):
        return addr.scope_id or ""
    if isinstance(addr, str) and "%" in addr:
        return addr.strip().strip("[]").split("%", 1)[1]
    return ""


def _interface_ips() -> list[tuple[str, Address, int]]:
    """Every address on every interface, as (interface, address, prefix length). getifaddrs through ifaddr, so no
    process starts; [] when the interfaces cannot be read."""
    try:
        import ifaddr

        adapters = ifaddr.get_adapters()
    except Exception as e:  # noqa: BLE001 — a broken getifaddrs is "no LAN", never a crash at the door
        log.warning("lan: could not list the network interfaces (%s)", type(e).__name__)
        return []
    out: list[tuple[str, Address, int]] = []
    for adapter in adapters:
        for entry in adapter.ips:
            raw = entry.ip[0] if isinstance(entry.ip, tuple) else entry.ip
            ip = _address(str(raw))
            if ip is not None and isinstance(entry.network_prefix, int):
                out.append((str(adapter.name), ip, entry.network_prefix))
    return out


def _lan_interface(name: str) -> bool:
    return NOT_LAN.fullmatch(name) is None


def _interface_order(name: str) -> tuple[int, str, int]:
    """en0 before en1 before en10 before anything else: on a Mac, en0 is the built-in Wi-Fi or Ethernet."""
    m = re.fullmatch(r"([A-Za-z]+)(\d*)", name)
    stem, num = (m.group(1), int(m.group(2) or 0)) if m else (name, 0)
    return (0 if stem == "en" else 1, stem, num)


def private_addresses() -> list[str]:
    """This Mac's RFC 1918 IPv4 addresses on its LAN interfaces, best first (en0 first), each once. [] when the
    Mac has none, or its interfaces cannot be read."""
    found: list[tuple[tuple[int, str, int], int, str]] = []
    for i, (name, ip, _prefix) in enumerate(_interface_ips()):
        if ip.version == 4 and _lan_interface(name) and any(ip in net for net in PRIVATE_V4):
            found.append((_interface_order(name), i, str(ip)))
    out: list[str] = []
    for _order, _i, ip_text in sorted(found):
        if ip_text not in out:
            out.append(ip_text)
    return out


def _on_link_networks() -> list[Network]:
    """The prefixes of the Mac's LAN interfaces that are narrow enough to be one home link."""
    nets: list[Network] = []
    for name, ip, prefix in _interface_ips():
        if ip.is_loopback or not _lan_interface(name):
            continue
        if MIN_ON_LINK_PREFIX[ip.version] <= prefix <= ip.max_prefixlen:
            nets.append(ipaddress.ip_network(f"{ip}/{prefix}", strict=False))
    return nets


def peer_allowed(addr: Any) -> bool:
    """May this peer reach the LAN door? Loopback, RFC 1918, link-local (169.254/16, fe80::/10), ULA (fc00::/7),
    or inside the prefix of one of the Mac's LAN interfaces. Anything else, and anything that is not an address,
    is refused. ``addr`` may be the peername tuple as asyncio gives it."""
    ip = _address(addr)
    if ip is None or ip.is_unspecified or ip.is_multicast:
        return False
    if ip.is_loopback or any(ip in net for net in LAN_PEERS[ip.version]):
        return True
    return any(ip.version == net.version and ip in net for net in _on_link_networks())


def local_hostname() -> str:
    """This Mac's Bonjour name, the LocalHostName that ``<name>.local`` resolves to on the LAN ("Kitchen-Mac"), or
    "" when it cannot be told. Falls back to the host name only when that is a ``.local`` name."""
    name = ""
    try:
        done = subprocess.run([SCUTIL, "--get", "LocalHostName"], capture_output=True, text=True,
                              stdin=subprocess.DEVNULL, timeout=SCUTIL_SECS, check=False)
        if done.returncode == 0:
            name = done.stdout.strip()
    except (OSError, subprocess.SubprocessError, ValueError) as e:
        log.info("lan: scutil could not tell the local host name (%s)", type(e).__name__)
    if LABEL.fullmatch(name):
        return name
    try:
        host = socket.gethostname().strip().rstrip(".")
    except OSError:
        return ""
    stem, dot, suffix = host.rpartition(".")
    if dot and suffix.lower() == "local" and LABEL.fullmatch(stem):
        return stem
    return ""


def _udp(family: int) -> socket.socket:
    return socket.socket(family, socket.SOCK_DGRAM)


def _errno_name(e: OSError) -> str:
    return errno.errorcode.get(e.errno or 0, type(e).__name__)


def trigger_local_network_alert() -> None:
    """Make macOS ask for the Local Network privilege now: a UDP connect to the mDNS group, sending nothing. Never
    raises. After the owner's first answer macOS remembers it, and this does nothing visible."""
    try:
        with _udp(socket.AF_INET) as s:
            s.connect(MDNS_GROUP)
    except OSError as e:
        log.info("lan: the local network trigger got %s", _errno_name(e))


def probe(addr: Any) -> str:
    """Can this process send to a device on the LAN? One empty datagram to the device's discard port: "ok" when it
    left, "blocked" when macOS refused it (EHOSTUNREACH or EPERM: the Local Network privilege is off), "unknown"
    for any other error, and without sending for an address that is not a LAN peer (loopback included, where the
    privilege does not apply) or a link-local IPv6 address without its zone. Never raises."""
    ip = _address(addr)
    if ip is None or ip.is_loopback or not peer_allowed(ip):
        return "unknown"
    host = str(ip)
    if ip.version == 6 and ip in LINK_LOCAL_V6:
        zone = _scope(addr)
        if not zone:
            return "unknown"
        host = f"{host}%{zone}"
    family = socket.AF_INET if ip.version == 4 else socket.AF_INET6
    try:
        target = socket.getaddrinfo(host, DISCARD_PORT, family, socket.SOCK_DGRAM, 0, socket.AI_NUMERICHOST)[0][4]
        with _udp(family) as s:
            s.setblocking(False)
            s.connect(target)
            s.send(b"")
    except OSError as e:
        if e.errno in BLOCKED:
            log.info("lan: the local network probe was refused (%s)", _errno_name(e))
            return "blocked"
        log.info("lan: the local network probe got %s", _errno_name(e))
        return "unknown"
    return "ok"
