"""lan_net.py: eve lite's LAN addresses, the LAN door's peer filter, the Bonjour name and the Local Network checks.

The interfaces, the UDP sockets and scutil are fakes here, so no test sends a packet or raises a macOS alert. The
addresses are documentation and private ranges (2001:db8::/32, 203.0.113.0/24, RFC 1918), never this Mac's."""
from __future__ import annotations

import errno
import ipaddress
import logging
import socket
import subprocess
import sys
import types
from typing import Any

import ifaddr
import pytest

from eve_bridge import lan_net


def ip(text: str) -> Any:
    return ipaddress.ip_address(text)


# A home network: Wi-Fi on en0 with IPv4 and a global IPv6 /64, plus interfaces a phone never comes through.
HOME = [
    ("lo0", ip("127.0.0.1"), 8),
    ("lo0", ip("::1"), 128),
    ("en0", ip("192.168.1.20"), 24),
    ("en0", ip("fe80::1c2d:3e4f:5a6b:7c8d"), 64),
    ("en0", ip("2001:db8:12:34::20"), 64),
    ("utun3", ip("2001:db8:99::2"), 64),             # a VPN tunnel's prefix
    ("bridge100", ip("2001:db8:77::1"), 64),         # the VM bridge
    ("en7", ip("203.0.113.9"), 8),                   # a /8 on an interface: no single home link
]


@pytest.fixture
def home(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(lan_net, "_interface_ips", lambda: list(HOME))


# ---- the peer filter ------------------------------------------------------------------------------------

LAN_PEERS = [
    "127.0.0.1", "127.8.9.10", "::1",                                   # loopback
    "10.0.0.1", "10.255.255.254", "172.16.0.1", "172.31.255.254", "192.168.0.1", "192.168.1.77",   # RFC 1918
    "169.254.10.20", "fe80::1", "fe80::aa:bb%en0", "[fe80::1%en0]",     # link-local, with or without a zone
    "fc00::1", "fd12:3456:789a::1", "[fd00::5]",                        # ULA
    "::ffff:192.168.1.30",                                              # a dual-stack listener's IPv4 peer
]

# The public control. Several are "private" to ipaddress (documentation, benchmarking, reserved), which is why
# lan_net spells its ranges out.
NOT_PEERS = [
    "8.8.8.8", "1.1.1.1", "172.32.0.1", "172.15.255.254", "11.0.0.1", "192.169.0.1",
    "100.64.0.1",                    # carrier-grade NAT
    "198.18.0.1", "192.0.2.1", "203.0.113.50", "240.0.0.1",
    "0.0.0.0", "255.255.255.255", "224.0.0.251",
    "::", "ff02::fb", "2001:4860:4860::8888", "2001:db8:12:35::1",
    "::ffff:8.8.8.8", "64:ff9b::808:808",
    "", "localhost", "192.168.1", "999.1.1.1", "fe80::1%", "not an address",
]


def test_lan_peers_are_allowed(home: None) -> None:
    refused = [a for a in LAN_PEERS if not lan_net.peer_allowed(a)]
    assert refused == []


def test_public_and_odd_addresses_are_refused(home: None) -> None:
    allowed = [a for a in NOT_PEERS if lan_net.peer_allowed(a)]
    assert allowed == []
    for odd in (None, 42, b"192.168.1.5", 3.5, ()):
        assert lan_net.peer_allowed(odd) is False
    # Why the ranges are explicit: is_private would have let these in.
    assert all(ipaddress.ip_address(a).is_private for a in ("198.18.0.1", "192.0.2.1", "203.0.113.50", "240.0.0.1"))


def test_home_ipv6_on_the_macs_own_prefix_is_allowed(home: None, monkeypatch: pytest.MonkeyPatch) -> None:
    assert lan_net.peer_allowed("2001:db8:12:34::99")                   # a phone on the same Wi-Fi, global IPv6
    assert lan_net.peer_allowed("2001:db8:12:34:aaaa:bbbb:cccc:dddd")
    assert not lan_net.peer_allowed("2001:db8:12:35::99")               # the next /64 over
    assert not lan_net.peer_allowed("2001:db8:99::5")                   # beyond a VPN tunnel
    assert not lan_net.peer_allowed("2001:db8:77::5")                   # a VM behind the bridge
    assert not lan_net.peer_allowed("203.0.113.50")                     # inside en7's /8, which is too wide
    # Control: the on-link rule is what admits the phone; with no interfaces it is refused.
    monkeypatch.setattr(lan_net, "_interface_ips", lambda: [])
    assert not lan_net.peer_allowed("2001:db8:12:34::99")
    assert lan_net.peer_allowed("192.168.1.77")                          # the fixed ranges need no interface


def test_peername_tuples_and_address_objects_are_read(home: None) -> None:
    assert lan_net.peer_allowed(("192.168.1.30", 51515))
    assert lan_net.peer_allowed(("::ffff:192.168.1.30", 51515, 0, 0))
    assert lan_net.peer_allowed(("fe80::1", 51515, 0, 4))
    assert lan_net.peer_allowed(("2001:db8:12:34::99", 51515, 0, 0))
    assert not lan_net.peer_allowed(("::ffff:8.8.8.8", 51515, 0, 0))
    assert not lan_net.peer_allowed(("8.8.8.8", 443))
    assert lan_net.peer_allowed(ipaddress.ip_address("10.1.2.3"))
    assert not lan_net.peer_allowed(ipaddress.ip_address("8.8.4.4"))


def test_unreadable_interfaces_mean_no_lan_and_no_crash(monkeypatch: pytest.MonkeyPatch,
                                                         caplog: pytest.LogCaptureFixture) -> None:
    def broken() -> list:
        raise OSError("getifaddrs failed")

    monkeypatch.setattr(ifaddr, "get_adapters", broken)
    with caplog.at_level(logging.WARNING, logger="eve_bridge.lan_net"):
        assert lan_net.private_addresses() == []
        assert not lan_net.peer_allowed("2001:db8:12:34::99")
        assert lan_net.peer_allowed("192.168.1.5")
    assert "OSError" in caplog.text


# ---- this Mac's addresses -------------------------------------------------------------------------------

def adapter(name: str, *entries: tuple[Any, int]) -> ifaddr.Adapter:
    return ifaddr.Adapter(name, name, [ifaddr.IP(addr, prefix, name) for addr, prefix in entries])


def test_private_addresses_lists_the_lan_interfaces_best_first(monkeypatch: pytest.MonkeyPatch) -> None:
    adapters = [
        adapter("lo0", ("127.0.0.1", 8), (("::1", 0, 0), 128)),
        adapter("en10", ("10.0.10.5", 24)),
        adapter("bridge100", ("192.168.64.1", 24)),                 # a VM network: a phone cannot reach it
        adapter("en0", (("fe80::1c2d:3e4f:5a6b:7c8d", 0, 15), 64), ("169.254.7.7", 16), ("192.168.1.20", 24),
                ("192.168.1.20", 24), (("2001:db8:12:34::20", 0, 0), 64), ("203.0.113.7", 24)),
        adapter("utun4", ("10.8.0.2", 24)),                         # a VPN
        adapter("en2", ("100.64.3.4", 10)),                         # carrier-grade NAT, not RFC 1918
        adapter("en1", ("172.20.10.2", 28)),                        # an iPhone hotspot
        adapter("awdl0", (("fe80::99", 0, 18), 64)),
    ]
    monkeypatch.setattr(ifaddr, "get_adapters", lambda: adapters)
    assert lan_net.private_addresses() == ["192.168.1.20", "172.20.10.2", "10.0.10.5"]


def test_private_addresses_on_this_mac_are_rfc1918_ipv4() -> None:
    found = lan_net.private_addresses()                             # the real interfaces: getifaddrs, no traffic
    assert len(found) == len(set(found))
    for text in found:
        addr = ipaddress.IPv4Address(text)
        assert any(addr in ipaddress.ip_network(n) for n in ("10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16"))


# ---- the Bonjour name -----------------------------------------------------------------------------------

def scutil(monkeypatch: pytest.MonkeyPatch, outcome: Any) -> list[list[str]]:
    """Stand in for subprocess in lan_net: ``outcome`` is (returncode, stdout) or an exception to raise."""
    calls: list[list[str]] = []

    def run(argv: list[str], **kwargs: Any) -> subprocess.CompletedProcess:
        calls.append(list(argv))
        assert kwargs.get("timeout") and kwargs.get("stdin") == subprocess.DEVNULL
        if isinstance(outcome, BaseException):
            raise outcome
        code, out = outcome
        return subprocess.CompletedProcess(argv, code, out, "")

    fake = types.SimpleNamespace(run=run, DEVNULL=subprocess.DEVNULL, SubprocessError=subprocess.SubprocessError)
    monkeypatch.setattr(lan_net, "subprocess", fake)
    return calls


def test_local_hostname_reads_the_bonjour_name_with_scutil(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(socket, "gethostname", lambda: "router-given.lan")
    calls = scutil(monkeypatch, (0, "Kitchen-Mac\n"))
    assert lan_net.local_hostname() == "Kitchen-Mac"
    assert calls == [["/usr/sbin/scutil", "--get", "LocalHostName"]]     # by full path: the firewall's allowlist


@pytest.mark.parametrize("outcome", [
    (1, ""),                                                        # LocalHostName not set
    (0, "not one label!\n"),
    (0, ""),
    FileNotFoundError(2, "no scutil"),
    PermissionError(1, "blocked by the process firewall"),
    subprocess.TimeoutExpired(["/usr/sbin/scutil"], 5.0),
])
def test_local_hostname_falls_back_to_a_dot_local_host_name(monkeypatch: pytest.MonkeyPatch, outcome: Any) -> None:
    scutil(monkeypatch, outcome)
    monkeypatch.setattr(socket, "gethostname", lambda: "Kitchen-Mac.local")
    assert lan_net.local_hostname() == "Kitchen-Mac"
    monkeypatch.setattr(socket, "gethostname", lambda: "192-168-1-20.home")    # not the Bonjour name: unknown
    assert lan_net.local_hostname() == ""
    monkeypatch.setattr(socket, "gethostname", lambda: "bad label.local")
    assert lan_net.local_hostname() == ""


@pytest.mark.skipif(sys.platform != "darwin", reason="scutil is macOS")
def test_local_hostname_on_this_mac_is_one_dns_label_or_empty() -> None:
    name = lan_net.local_hostname()
    assert name == "" or lan_net.LABEL.fullmatch(name)


# ---- Local Network: the alert and the probe -------------------------------------------------------------

class FakeUdp:
    def __init__(self, family: int, fail_connect: int = 0, fail_send: int = 0) -> None:
        self.family = family
        self.fail_connect = fail_connect
        self.fail_send = fail_send
        self.connected: Any = None
        self.sent: list[bytes] = []
        self.blocking = True
        self.closed = False

    def __enter__(self) -> "FakeUdp":
        return self

    def __exit__(self, *exc: Any) -> None:
        self.closed = True

    def setblocking(self, flag: bool) -> None:
        self.blocking = flag

    def connect(self, target: Any) -> None:
        self.connected = target
        if self.fail_connect:
            raise OSError(self.fail_connect, "fake")

    def send(self, data: bytes) -> int:
        if self.fail_send:
            raise OSError(self.fail_send, "fake")
        self.sent.append(data)
        return len(data)


def udp(monkeypatch: pytest.MonkeyPatch, **fail: int) -> list[FakeUdp]:
    made: list[FakeUdp] = []

    def make(family: int) -> FakeUdp:
        s = FakeUdp(family, **fail)
        made.append(s)
        return s

    monkeypatch.setattr(lan_net, "_udp", make)
    return made


def test_the_alert_trigger_connects_to_the_mdns_group_and_sends_nothing(monkeypatch: pytest.MonkeyPatch) -> None:
    made = udp(monkeypatch)
    assert lan_net.trigger_local_network_alert() is None
    assert len(made) == 1
    s = made[0]
    assert s.family == socket.AF_INET and s.connected == ("224.0.0.251", 5353)
    assert s.sent == [] and s.closed


@pytest.mark.parametrize("err", [errno.EHOSTUNREACH, errno.ENETUNREACH, errno.EPERM])
def test_the_alert_trigger_never_raises(monkeypatch: pytest.MonkeyPatch, err: int) -> None:
    made = udp(monkeypatch, fail_connect=err)
    lan_net.trigger_local_network_alert()
    assert made[0].closed

    def no_socket(family: int) -> Any:
        raise OSError(errno.EMFILE, "too many open files")

    monkeypatch.setattr(lan_net, "_udp", no_socket)
    lan_net.trigger_local_network_alert()


def test_probe_sends_one_empty_datagram_to_the_discard_port(home: None, monkeypatch: pytest.MonkeyPatch) -> None:
    made = udp(monkeypatch)
    assert lan_net.probe("192.168.1.30") == "ok"
    assert len(made) == 1                                          # the control for the no-socket cases below
    s = made[0]
    assert s.family == socket.AF_INET and s.connected == ("192.168.1.30", 9)
    assert s.sent == [b""] and not s.blocking and s.closed


@pytest.mark.parametrize("fail", [{"fail_connect": errno.EHOSTUNREACH}, {"fail_send": errno.EHOSTUNREACH},
                                  {"fail_connect": errno.EPERM}, {"fail_send": errno.EPERM}])
def test_probe_reads_a_refusal_as_blocked(home: None, monkeypatch: pytest.MonkeyPatch,
                                          caplog: pytest.LogCaptureFixture, fail: dict) -> None:
    udp(monkeypatch, **fail)
    with caplog.at_level(logging.INFO, logger="eve_bridge.lan_net"):
        assert lan_net.probe("192.168.1.30") == "blocked"
    assert "192.168" not in caplog.text                            # errno names, never the address


@pytest.mark.parametrize("err", [errno.ENETUNREACH, errno.EHOSTDOWN, errno.ENOBUFS, errno.EAGAIN])
def test_probe_reads_other_errors_as_unknown(home: None, monkeypatch: pytest.MonkeyPatch, err: int) -> None:
    udp(monkeypatch, fail_send=err)
    assert lan_net.probe("192.168.1.30") == "unknown"


@pytest.mark.parametrize("addr", ["127.0.0.1", "::1", "8.8.8.8", "2001:db8:12:35::1", "fe80::1", "", "nope", None,
                                  "224.0.0.251"])
def test_probe_sends_nothing_where_it_proves_nothing(home: None, monkeypatch: pytest.MonkeyPatch, addr: Any) -> None:
    made = udp(monkeypatch)
    assert lan_net.probe(addr) == "unknown"
    assert made == []


def test_probe_reaches_ipv6_peers_with_their_zone(home: None, monkeypatch: pytest.MonkeyPatch) -> None:
    made = udp(monkeypatch)
    assert lan_net.probe(("fe80::1", 51515, 0, 4)) == "ok"
    assert made[-1].family == socket.AF_INET6
    assert made[-1].connected[1] == 9 and made[-1].connected[3] == 4         # the scope id travels
    assert lan_net.probe("fd00::5") == "ok"
    assert lan_net.probe("2001:db8:12:34::99") == "ok"                       # on the Mac's own /64
    assert [s.sent for s in made] == [[b""], [b""], [b""]]
