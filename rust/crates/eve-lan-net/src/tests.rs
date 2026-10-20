//! Port of bridge/tests/test_lan_net.py (and the lan_net part of test_lite_web_lan.py).
//!
//! The interfaces, the UDP sockets and scutil are fakes here, so no test sends a packet or raises a macOS alert. The
//! addresses are documentation and private ranges (2001:db8::/32, 203.0.113.0/24, RFC 1918), never this Mac's.

use std::cell::RefCell;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::*;

fn ip(text: &str) -> IpAddr {
    text.parse().expect("a test address")
}

/// A home network: Wi-Fi on en0 with IPv4 and a global IPv6 /64, plus interfaces a phone never comes through.
fn home() -> Vec<InterfaceIp> {
    vec![
        InterfaceIp::new("lo0", ip("127.0.0.1"), 8),
        InterfaceIp::new("lo0", ip("::1"), 128),
        InterfaceIp::new("en0", ip("192.168.1.20"), 24),
        InterfaceIp::new("en0", ip("fe80::1c2d:3e4f:5a6b:7c8d"), 64),
        InterfaceIp::new("en0", ip("2001:db8:12:34::20"), 64),
        InterfaceIp::new("utun3", ip("2001:db8:99::2"), 64), // a VPN tunnel's prefix
        InterfaceIp::new("bridge100", ip("2001:db8:77::1"), 64), // the VM bridge
        InterfaceIp::new("en7", ip("203.0.113.9"), 8),       // a /8 on an interface: no single home link
    ]
}

fn allowed_at_home(addr: impl Into<Peer>) -> bool {
    peer_allowed_with(&addr.into(), home)
}

/// Run `f` with a log subscriber on this thread only; return what it logged.
fn capture_logs(f: impl FnOnce()) -> String {
    #[derive(Clone)]
    struct Buf(Arc<Mutex<Vec<u8>>>);
    impl io::Write for Buf {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(data);
            Ok(data.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let buf = Buf(Arc::new(Mutex::new(Vec::new())));
    let writer = buf.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .with_ansi(false)
        .with_max_level(tracing::Level::DEBUG)
        .finish();
    tracing::subscriber::with_default(subscriber, f);
    let out = String::from_utf8_lossy(&buf.0.lock().unwrap()).into_owned();
    out
}

// ---- the peer filter ---------------------------------------------------------------------------------------------

const LAN_PEERS: &[&str] = &[
    "127.0.0.1", "127.8.9.10", "::1", // loopback
    "10.0.0.1", "10.255.255.254", "172.16.0.1", "172.31.255.254", "192.168.0.1", "192.168.1.77", // RFC 1918
    "169.254.10.20", "fe80::1", "fe80::aa:bb%en0", "[fe80::1%en0]", // link-local, with or without a zone
    "fc00::1", "fd12:3456:789a::1", "[fd00::5]", // ULA
    "::ffff:192.168.1.30", // a dual-stack listener's IPv4 peer
];

// The public control. Several are "private" to Python's ipaddress (documentation, benchmarking, reserved), which is
// why lan_net spells its ranges out.
const NOT_PEERS: &[&str] = &[
    "8.8.8.8", "1.1.1.1", "172.32.0.1", "172.15.255.254", "11.0.0.1", "192.169.0.1",
    "100.64.0.1", // carrier-grade NAT
    "198.18.0.1", "192.0.2.1", "203.0.113.50", "240.0.0.1",
    "0.0.0.0", "255.255.255.255", "224.0.0.251",
    "::", "ff02::fb", "2001:4860:4860::8888", "2001:db8:12:35::1",
    "::ffff:8.8.8.8", "64:ff9b::808:808",
    "", "localhost", "192.168.1", "999.1.1.1", "fe80::1%", "not an address",
];

#[test]
fn test_lan_peers_are_allowed() {
    let refused: Vec<&str> = LAN_PEERS.iter().copied().filter(|a| !allowed_at_home(*a)).collect();
    assert_eq!(refused, Vec::<&str>::new());
}

#[test]
fn test_public_and_odd_addresses_are_refused() {
    let allowed: Vec<&str> = NOT_PEERS.iter().copied().filter(|a| allowed_at_home(*a)).collect();
    assert_eq!(allowed, Vec::<&str>::new());
    // None, 42, b"192.168.1.5", 3.5, (): none of them is an address
    assert!(!allowed_at_home(Peer::None));
    assert!(!allowed_at_home(None::<&str>));
    // A zone on an IPv4 address, or a second %, is no address either.
    for odd in ["192.168.1.5%en0", "fe80::1%en0%en1", "[]", "["] {
        assert!(!allowed_at_home(odd), "{odd}");
    }
    // Python's ipaddress takes any zone text, a stray bracket included.
    assert!(allowed_at_home("fe80::1%en0]"));
}

#[test]
fn test_home_ipv6_on_the_macs_own_prefix_is_allowed() {
    assert!(allowed_at_home("2001:db8:12:34::99")); // a phone on the same Wi-Fi, global IPv6
    assert!(allowed_at_home("2001:db8:12:34:aaaa:bbbb:cccc:dddd"));
    assert!(!allowed_at_home("2001:db8:12:35::99")); // the next /64 over
    assert!(!allowed_at_home("2001:db8:99::5")); // beyond a VPN tunnel
    assert!(!allowed_at_home("2001:db8:77::5")); // a VM behind the bridge
    assert!(!allowed_at_home("203.0.113.50")); // inside en7's /8, which is too wide
    // Control: the on-link rule is what admits the phone; with no interfaces it is refused.
    assert!(!peer_allowed_with(&"2001:db8:12:34::99".into(), Vec::new));
    assert!(peer_allowed_with(&"192.168.1.77".into(), Vec::new)); // the fixed ranges need no interface
}

#[test]
fn test_peername_tuples_and_address_objects_are_read() {
    assert!(allowed_at_home(("192.168.1.30", 51515)));
    assert!(allowed_at_home(("::ffff:192.168.1.30", 51515, 0, 0)));
    assert!(allowed_at_home(("fe80::1", 51515, 0, 4)));
    assert!(allowed_at_home(("2001:db8:12:34::99", 51515, 0, 0)));
    assert!(!allowed_at_home(("::ffff:8.8.8.8", 51515, 0, 0)));
    assert!(!allowed_at_home(("8.8.8.8", 443)));
    assert!(allowed_at_home(ip("10.1.2.3")));
    assert!(!allowed_at_home(ip("8.8.4.4")));
    // A SocketAddr, as a Rust listener gives the peer.
    assert!(allowed_at_home("192.168.1.30:51515".parse::<SocketAddr>().unwrap()));
    assert!(allowed_at_home("[fe80::1%4]:51515".parse::<SocketAddr>().unwrap()));
    assert!(allowed_at_home("[::ffff:192.168.1.30]:51515".parse::<SocketAddr>().unwrap()));
    assert!(!allowed_at_home("[::ffff:8.8.8.8]:51515".parse::<SocketAddr>().unwrap()));
}

#[test]
fn test_unreadable_interfaces_mean_no_lan_and_no_crash() {
    let broken = || Err(AdapterError::new("OSError", "getifaddrs failed"));
    let logs = capture_logs(|| {
        assert_eq!(private_addresses_in(&interface_ips_from(broken())), Vec::<String>::new());
        assert!(!peer_allowed_with(&"2001:db8:12:34::99".into(), || interface_ips_from(broken())));
        assert!(peer_allowed_with(&"192.168.1.5".into(), || interface_ips_from(broken())));
    });
    assert!(logs.contains("OSError"), "{logs}");
    assert!(logs.contains("WARN"), "{logs}");
}

// ---- this Mac's addresses ----------------------------------------------------------------------------------------

fn adapter(name: &str, entries: &[(&str, u32)]) -> Adapter {
    Adapter {
        name: name.to_string(),
        ips: entries.iter().map(|(ip, prefix)| AdapterIp { ip: ip.to_string(), network_prefix: *prefix }).collect(),
    }
}

#[test]
fn test_private_addresses_lists_the_lan_interfaces_best_first() {
    let adapters = vec![
        adapter("lo0", &[("127.0.0.1", 8), ("::1", 128)]),
        adapter("en10", &[("10.0.10.5", 24)]),
        adapter("bridge100", &[("192.168.64.1", 24)]), // a VM network: a phone cannot reach it
        adapter(
            "en0",
            &[
                ("fe80::1c2d:3e4f:5a6b:7c8d", 64),
                ("169.254.7.7", 16),
                ("192.168.1.20", 24),
                ("192.168.1.20", 24),
                ("2001:db8:12:34::20", 64),
                ("203.0.113.7", 24),
            ],
        ),
        adapter("utun4", &[("10.8.0.2", 24)]), // a VPN
        adapter("en2", &[("100.64.3.4", 10)]), // carrier-grade NAT, not RFC 1918
        adapter("en1", &[("172.20.10.2", 28)]), // an iPhone hotspot
        adapter("awdl0", &[("fe80::99", 64)]),
    ];
    assert_eq!(
        private_addresses_in(&interface_ips_from(Ok(adapters))),
        vec!["192.168.1.20", "172.20.10.2", "10.0.10.5"]
    );
}

#[test]
fn test_private_addresses_on_this_mac_are_rfc1918_ipv4() {
    let found = private_addresses(); // the real interfaces: getifaddrs, no traffic
    let mut unique = found.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(found.len(), unique.len());
    for text in &found {
        let addr: std::net::Ipv4Addr = text.parse().expect("an IPv4 address");
        assert!(PRIVATE_V4.iter().any(|net| net.contains(&IpAddr::V4(addr))), "{text}");
    }
}

#[test]
fn real_interfaces_are_read_and_normalized() {
    let ips = interface_ips();
    // Every address carries a prefix no wider than its family, and none is an IPv4-mapped IPv6 address.
    for entry in &ips {
        assert!(entry.prefix <= max_prefixlen(&entry.ip), "{entry:?}");
        if let IpAddr::V6(v6) = entry.ip {
            assert!(v6.to_ipv4_mapped().is_none(), "{entry:?}");
        }
    }
    // A Mac (or any host) has a loopback interface with 127.0.0.1/8.
    assert!(ips.iter().any(|e| e.ip == ip("127.0.0.1") && e.prefix == 8), "{ips:?}");
}

#[test]
fn netmasks_read_the_way_ipaddress_reads_them() {
    assert_eq!(prefix_from_v4_mask(0xffff_ff00).unwrap(), 24);
    assert_eq!(prefix_from_v4_mask(0).unwrap(), 0);
    assert_eq!(prefix_from_v4_mask(u32::MAX).unwrap(), 32);
    assert_eq!(prefix_from_v4_mask(0x0000_00ff).unwrap(), 24); // a hostmask, as IPv4Network reads it
    assert_eq!(prefix_from_v4_mask(0xff00_ff00).unwrap_err().kind, "NetmaskValueError");
}

#[test]
fn interfaces_order_en0_first_then_by_stem_and_number() {
    let mut names = vec!["utun0", "en10", "bridge0", "en1", "en0", "awdl0", "en2", "x"];
    names.sort_by_key(|n| interface_order(n));
    assert_eq!(names, vec!["en0", "en1", "en2", "en10", "awdl0", "bridge0", "utun0", "x"]);
    assert_eq!(interface_order("en0"), (0, "en".to_string(), 0));
    assert_eq!(interface_order("en"), (0, "en".to_string(), 0));
    assert_eq!(interface_order("eth0:1"), (1, "eth0:1".to_string(), 0));
    // Python's \d and int() read any script's digits: Arabic-Indic 12, and Mathematical bold 7 (runs that abut).
    assert_eq!(interface_order("en\u{661}\u{662}"), (0, "en".to_string(), 12));
    assert_eq!(interface_order("en\u{1D7D5}"), (0, "en".to_string(), 7));
}

#[test]
fn strips_like_python_str_strip() {
    // Python's strip() also removes the separators \x1c-\x1f, which Rust's trim() keeps.
    assert_eq!(address(&"\x1c10.0.0.1\x1f".into()), Some(ip("10.0.0.1")));
    let calls: Calls = Rc::default();
    assert_eq!(local_hostname_with(scutil(&calls, Ok((0, "Kitchen-Mac\x1e\n"))), host("x.lan")), "Kitchen-Mac");
}

#[test]
fn not_lan_names_are_whole_names() {
    for name in ["lo0", "utun3", "bridge100", "awdl0", "llw0", "anpi1", "ap1", "gif0", "stf0", "vmenet2", "lo"] {
        assert!(!lan_interface(name), "{name}");
    }
    for name in ["en0", "en7", "eth0", "wlan0", "loop0", "apple0", "en0bridge"] {
        assert!(lan_interface(name), "{name}");
    }
}

#[test]
fn on_link_networks_keep_narrow_lan_prefixes() {
    let nets = on_link_networks(&home());
    let shown: Vec<String> = nets.iter().map(|n| n.to_string()).collect();
    assert_eq!(shown, vec!["192.168.1.0/24", "fe80::/64", "2001:db8:12:34::/64"]);
}

// ---- the Bonjour name --------------------------------------------------------------------------------------------

type Calls = Rc<RefCell<Vec<Vec<String>>>>;

/// Stand in for scutil: `outcome` is (returncode, stdout) or an error to raise.
fn scutil(
    calls: &Calls,
    outcome: io::Result<(i32, &'static str)>,
) -> impl FnOnce(&[&str], Duration) -> io::Result<RunOutput> {
    let calls = calls.clone();
    move |argv: &[&str], timeout: Duration| {
        calls.borrow_mut().push(argv.iter().map(|s| s.to_string()).collect());
        assert!(timeout > Duration::ZERO);
        let (code, out) = outcome?;
        Ok(RunOutput { code: Some(code), stdout: out.as_bytes().to_vec() })
    }
}

fn host(name: &'static str) -> impl FnOnce() -> io::Result<String> {
    move || Ok(name.to_string())
}

#[test]
fn test_local_hostname_reads_the_bonjour_name_with_scutil() {
    let calls: Calls = Rc::default();
    assert_eq!(local_hostname_with(scutil(&calls, Ok((0, "Kitchen-Mac\n"))), host("router-given.lan")), "Kitchen-Mac");
    // by full path: the firewall's allowlist
    assert_eq!(*calls.borrow(), vec![vec!["/usr/sbin/scutil".to_string(), "--get".into(), "LocalHostName".into()]]);
}

type Outcome = Box<dyn Fn() -> io::Result<(i32, &'static str)>>;

fn hostname_outcomes() -> Vec<(&'static str, Outcome)> {
    vec![
        ("not set", Box::new(|| Ok((1, "")))), // LocalHostName not set
        ("not one label", Box::new(|| Ok((0, "not one label!\n")))),
        ("empty", Box::new(|| Ok((0, "")))),
        ("no scutil", Box::new(|| Err(io::Error::from_raw_os_error(libc::ENOENT)))),
        ("firewall", Box::new(|| Err(io::Error::from_raw_os_error(libc::EPERM)))),
        ("timeout", Box::new(|| Err(io::Error::new(io::ErrorKind::TimedOut, "scutil timed out")))),
    ]
}

#[test]
fn test_local_hostname_falls_back_to_a_dot_local_host_name() {
    for (label, outcome) in hostname_outcomes() {
        let calls: Calls = Rc::default();
        assert_eq!(local_hostname_with(scutil(&calls, outcome()), host("Kitchen-Mac.local")), "Kitchen-Mac", "{label}");
        // not the Bonjour name: unknown
        assert_eq!(local_hostname_with(scutil(&calls, outcome()), host("192-168-1-20.home")), "", "{label}");
        assert_eq!(local_hostname_with(scutil(&calls, outcome()), host("bad label.local")), "", "{label}");
    }
}

#[test]
fn local_hostname_edges() {
    let calls: Calls = Rc::default();
    // A trailing dot and an upper-case suffix still read as .local.
    assert_eq!(local_hostname_with(scutil(&calls, Ok((1, ""))), host("Kitchen-Mac.LOCAL.")), "Kitchen-Mac");
    // A host name that cannot be read is unknown.
    let none = || Err(io::Error::from_raw_os_error(libc::EINVAL));
    assert_eq!(local_hostname_with(scutil(&calls, Ok((1, ""))), none), "");
    // scutil's answer that is not UTF-8 falls back, and the log names the type.
    let bad_utf8 = |_: &[&str], _: Duration| Ok(RunOutput { code: Some(0), stdout: vec![0xff, 0xfe] });
    let logs = capture_logs(|| assert_eq!(local_hostname_with(bad_utf8, host("Den.local")), "Den"));
    assert!(logs.contains("UnicodeDecodeError"), "{logs}");
    // A failed start logs the OSError subclass Python picks from the errno.
    for (code, want) in [
        (libc::ENOENT, "FileNotFoundError"),
        (libc::EACCES, "PermissionError"),
        (libc::EPERM, "PermissionError"),
        (libc::ENOTDIR, "NotADirectoryError"),
        (libc::ENOEXEC, "OSError"),
    ] {
        let failed = move |_: &[&str], _: Duration| -> io::Result<RunOutput> { Err(io::Error::from_raw_os_error(code)) };
        let logs = capture_logs(|| assert_eq!(local_hostname_with(failed, host("x.lan")), ""));
        assert!(logs.contains(&format!("lan: scutil could not tell the local host name ({want})")), "{logs}");
    }
    let slow = |_: &[&str], _: Duration| -> io::Result<RunOutput> {
        Err(io::Error::new(io::ErrorKind::TimedOut, "scutil timed out"))
    };
    let logs = capture_logs(|| assert_eq!(local_hostname_with(slow, host("x.lan")), ""));
    assert!(logs.contains("(TimeoutExpired)"), "{logs}");
    // A signal-ended scutil is not a name.
    let killed = |_: &[&str], _: Duration| Ok(RunOutput { code: None, stdout: b"Kitchen-Mac\n".to_vec() });
    assert_eq!(local_hostname_with(killed, host("x.lan")), "");
    // A label is 63 characters at most.
    let long = "a".repeat(64);
    assert!(!LABEL.is_match(&long));
    assert!(LABEL.is_match(&long[..63]));
}

#[test]
fn run_captured_times_out_and_reads_stdout() {
    let done = run_captured(&["/bin/echo", "Kitchen-Mac"], Duration::from_secs(5)).unwrap();
    assert_eq!(done.code, Some(0));
    assert_eq!(done.stdout, b"Kitchen-Mac\n");
    let slow = run_captured(&["/bin/sleep", "5"], Duration::from_millis(100)).unwrap_err();
    assert_eq!(slow.kind(), io::ErrorKind::TimedOut);
    let missing = run_captured(&["/nonexistent/scutil"], Duration::from_secs(1)).unwrap_err();
    assert_eq!(missing.kind(), io::ErrorKind::NotFound);
}

#[cfg(target_os = "macos")]
#[test]
fn test_local_hostname_on_this_mac_is_one_dns_label_or_empty() {
    let name = local_hostname();
    assert!(name.is_empty() || LABEL.is_match(&name), "{name:?}");
}

// ---- Local Network: the alert and the probe ----------------------------------------------------------------------

#[derive(Debug, Default)]
struct FakeState {
    family: Option<Family>,
    connected: Option<SocketAddr>,
    sent: Vec<Vec<u8>>,
    blocking: bool,
    closed: bool,
}

struct FakeUdp {
    state: Rc<RefCell<FakeState>>,
    fail_connect: i32,
    fail_send: i32,
}

impl UdpSocket for FakeUdp {
    fn set_blocking(&mut self, flag: bool) -> io::Result<()> {
        self.state.borrow_mut().blocking = flag;
        Ok(())
    }

    fn connect(&mut self, target: SocketAddr) -> io::Result<()> {
        self.state.borrow_mut().connected = Some(target);
        if self.fail_connect != 0 {
            return Err(io::Error::from_raw_os_error(self.fail_connect));
        }
        Ok(())
    }

    fn send(&mut self, data: &[u8]) -> io::Result<usize> {
        if self.fail_send != 0 {
            return Err(io::Error::from_raw_os_error(self.fail_send));
        }
        self.state.borrow_mut().sent.push(data.to_vec());
        Ok(data.len())
    }
}

impl Drop for FakeUdp {
    fn drop(&mut self) {
        self.state.borrow_mut().closed = true;
    }
}

type Made = Rc<RefCell<Vec<Rc<RefCell<FakeState>>>>>;

fn udp_factory(made: &Made, fail_connect: i32, fail_send: i32) -> impl FnOnce(Family) -> io::Result<Box<dyn UdpSocket>> {
    let made = made.clone();
    move |family| {
        let state = Rc::new(RefCell::new(FakeState { family: Some(family), blocking: true, ..Default::default() }));
        made.borrow_mut().push(state.clone());
        Ok(Box::new(FakeUdp { state, fail_connect, fail_send }) as Box<dyn UdpSocket>)
    }
}

#[test]
fn test_the_alert_trigger_connects_to_the_mdns_group_and_sends_nothing() {
    let made: Made = Rc::default();
    trigger_local_network_alert_with(udp_factory(&made, 0, 0));
    let made = made.borrow();
    assert_eq!(made.len(), 1);
    let s = made[0].borrow();
    assert_eq!(s.family, Some(Family::Inet));
    assert_eq!(s.connected, Some("224.0.0.251:5353".parse().unwrap()));
    assert!(s.sent.is_empty() && s.closed);
}

#[test]
fn test_the_alert_trigger_never_raises() {
    for err in [libc::EHOSTUNREACH, libc::ENETUNREACH, libc::EPERM] {
        let made: Made = Rc::default();
        let logs = capture_logs(|| trigger_local_network_alert_with(udp_factory(&made, err, 0)));
        assert!(made.borrow()[0].borrow().closed);
        assert!(logs.contains("lan: the local network trigger got E"), "{logs}");
        let no_socket = |_: Family| -> io::Result<Box<dyn UdpSocket>> { Err(io::Error::from_raw_os_error(libc::EMFILE)) };
        let logs = capture_logs(|| trigger_local_network_alert_with(no_socket));
        assert!(logs.contains("EMFILE"), "{logs}");
    }
}

fn probe_at_home(addr: impl Into<Peer>, made: &Made, fail_connect: i32, fail_send: i32) -> &'static str {
    probe_with(&addr.into(), home, udp_factory(made, fail_connect, fail_send))
}

#[test]
fn test_probe_sends_one_empty_datagram_to_the_discard_port() {
    let made: Made = Rc::default();
    assert_eq!(probe_at_home("192.168.1.30", &made, 0, 0), "ok");
    let made = made.borrow();
    assert_eq!(made.len(), 1); // the control for the no-socket cases below
    let s = made[0].borrow();
    assert_eq!(s.family, Some(Family::Inet));
    assert_eq!(s.connected, Some("192.168.1.30:9".parse().unwrap()));
    assert_eq!(s.sent, vec![Vec::<u8>::new()]);
    assert!(!s.blocking && s.closed);
}

#[test]
fn test_probe_reads_a_refusal_as_blocked() {
    for (fail_connect, fail_send) in [(libc::EHOSTUNREACH, 0), (0, libc::EHOSTUNREACH), (libc::EPERM, 0), (0, libc::EPERM)] {
        let made: Made = Rc::default();
        let logs = capture_logs(|| assert_eq!(probe_at_home("192.168.1.30", &made, fail_connect, fail_send), "blocked"));
        assert!(!logs.contains("192.168"), "{logs}"); // errno names, never the address
        assert!(logs.contains("lan: the local network probe was refused (E"), "{logs}");
    }
}

#[test]
fn test_probe_reads_other_errors_as_unknown() {
    for err in [libc::ENETUNREACH, libc::EHOSTDOWN, libc::ENOBUFS, libc::EAGAIN] {
        let made: Made = Rc::default();
        let logs = capture_logs(|| assert_eq!(probe_at_home("192.168.1.30", &made, 0, err), "unknown"));
        assert!(logs.contains("lan: the local network probe got E"), "{logs}");
    }
}

#[test]
fn test_probe_sends_nothing_where_it_proves_nothing() {
    let addrs: Vec<Peer> = ["127.0.0.1", "::1", "8.8.8.8", "2001:db8:12:35::1", "fe80::1", "", "nope", "224.0.0.251"]
        .iter()
        .map(|a| Peer::from(*a))
        .chain([Peer::None])
        .collect();
    for addr in addrs {
        let made: Made = Rc::default();
        assert_eq!(probe_at_home(&addr, &made, 0, 0), "unknown", "{addr:?}");
        assert!(made.borrow().is_empty(), "{addr:?}");
    }
}

#[test]
fn test_probe_reaches_ipv6_peers_with_their_zone() {
    let made: Made = Rc::default();
    assert_eq!(probe_at_home(("fe80::1", 51515, 0, 4), &made, 0, 0), "ok");
    {
        let last = made.borrow();
        let s = last.last().unwrap().borrow();
        assert_eq!(s.family, Some(Family::Inet6));
        match s.connected {
            Some(SocketAddr::V6(sa)) => {
                assert_eq!(sa.port(), 9);
                assert_eq!(sa.scope_id(), 4); // the scope id travels
            }
            other => panic!("not an IPv6 target: {other:?}"),
        }
    }
    assert_eq!(probe_at_home("fd00::5", &made, 0, 0), "ok");
    assert_eq!(probe_at_home("2001:db8:12:34::99", &made, 0, 0), "ok"); // on the Mac's own /64
    let sent: Vec<Vec<Vec<u8>>> = made.borrow().iter().map(|s| s.borrow().sent.clone()).collect();
    assert_eq!(sent, vec![vec![Vec::<u8>::new()], vec![Vec::new()], vec![Vec::new()]]);
}

fn last_scope_id(made: &Made) -> u32 {
    match made.borrow().last().expect("a socket").borrow().connected {
        Some(SocketAddr::V6(sa)) => sa.scope_id(),
        other => panic!("not an IPv6 target: {other:?}"),
    }
}

#[test]
fn probe_reads_a_zone_through_getaddrinfo_as_python_does() {
    let made: Made = Rc::default();
    assert_eq!(probe_at_home("fe80::1%7", &made, 0, 0), "ok");
    assert_eq!(last_scope_id(&made), 7);
    // An interface name becomes its index.
    let lo0 = unsafe { libc::if_nametoindex(c"lo0".as_ptr()) };
    if lo0 != 0 {
        assert_eq!(probe_at_home("fe80::1%lo0", &made, 0, 0), "ok");
        assert_eq!(last_scope_id(&made), lo0);
    }
    // macOS's getaddrinfo reads a zone it cannot place as scope 0 and still answers, as Python 3.13 sees there.
    #[cfg(target_os = "macos")]
    for zone in ["no-such-if0", "+4", " 4"] {
        let made: Made = Rc::default();
        assert_eq!(probe_at_home(format!("fe80::1%{zone}"), &made, 0, 0), "ok", "{zone:?}");
        assert_eq!(last_scope_id(&made), 0, "{zone:?}");
    }
}

#[test]
fn probe_reads_a_host_getaddrinfo_refuses_as_unknown_without_a_socket() {
    // A non-ASCII zone is punycoded by Python's idna codec, which no numeric parse takes: a gaierror, EAI_NONAME.
    let made: Made = Rc::default();
    let logs = capture_logs(|| assert_eq!(probe_at_home("fe80::1%\u{e9}", &made, 0, 0), "unknown"));
    assert!(made.borrow().is_empty());
    #[cfg(target_os = "macos")]
    assert!(logs.contains("lan: the local network probe got ENOEXEC"), "{logs}"); // errno.errorcode[8], as Python logs
    #[cfg(not(target_os = "macos"))]
    let _ = logs;
    // A label past 63 characters is refused by the idna codec before getaddrinfo runs.
    let made: Made = Rc::default();
    let long = format!("fe80::1%{}", "a".repeat(60));
    let logs = capture_logs(|| assert_eq!(probe_at_home(long, &made, 0, 0), "unknown"));
    assert!(made.borrow().is_empty());
    assert!(logs.contains("UnicodeEncodeError"), "{logs}");
}

#[test]
fn numeric_sockaddr_matches_getaddrinfo() {
    assert_eq!(numeric_sockaddr("192.168.1.30", 9, Family::Inet).unwrap(), "192.168.1.30:9".parse().unwrap());
    assert_eq!(numeric_sockaddr("fd00::5", 9, Family::Inet6).unwrap(), "[fd00::5]:9".parse().unwrap());
    // AI_NUMERICHOST: a name is never looked up.
    let e = numeric_sockaddr("localhost", 9, Family::Inet).unwrap_err();
    assert!(e.get_ref().is_some_and(|inner| inner.is::<GaiError>()), "{e:?}");
    // The idna codec's label rules: an empty inner label is refused, a trailing dot is not.
    assert!(idna_host("a..b").unwrap_err().get_ref().is_some_and(|inner| inner.is::<IdnaError>()));
    assert!(idna_host("1.2.3.4.").is_ok());
    // A NUL ends the C string the system reads.
    assert_eq!(idna_host("fe80::1%\0x").unwrap().as_bytes(), b"fe80::1%");
}

#[test]
fn errno_names_match_python() {
    assert_eq!(errno_name(&io::Error::from_raw_os_error(libc::EHOSTUNREACH)), "EHOSTUNREACH");
    assert_eq!(errno_name(&io::Error::from_raw_os_error(libc::EPERM)), "EPERM");
    assert_eq!(errno_name(&io::Error::from_raw_os_error(libc::EAGAIN)), "EAGAIN");
    assert_eq!(errno_name(&io::Error::other("no errno")), "OSError");
    assert_eq!(errno_name(&io::Error::other(GaiError(999))), "gaierror");
    assert_eq!(errno_name(&io::Error::other(GaiError(libc::EAI_NONAME))), errno_name(&io::Error::from_raw_os_error(libc::EAI_NONAME)));
}

/// `errno.errorcode` on macOS (Python 3.13), every code from 1 to 106.
#[cfg(target_os = "macos")]
#[test]
fn errno_names_cover_every_darwin_errno_as_python_does() {
    const PYTHON: &[&str] = &[
        "EPERM", "ENOENT", "ESRCH", "EINTR", "EIO", "ENXIO", "E2BIG", "ENOEXEC", "EBADF", "ECHILD", "EDEADLK",
        "ENOMEM", "EACCES", "EFAULT", "ENOTBLK", "EBUSY", "EEXIST", "EXDEV", "ENODEV", "ENOTDIR", "EISDIR", "EINVAL",
        "ENFILE", "EMFILE", "ENOTTY", "ETXTBSY", "EFBIG", "ENOSPC", "ESPIPE", "EROFS", "EMLINK", "EPIPE", "EDOM",
        "ERANGE", "EAGAIN", "EINPROGRESS", "EALREADY", "ENOTSOCK", "EDESTADDRREQ", "EMSGSIZE", "EPROTOTYPE",
        "ENOPROTOOPT", "EPROTONOSUPPORT", "ESOCKTNOSUPPORT", "ENOTSUP", "EPFNOSUPPORT", "EAFNOSUPPORT", "EADDRINUSE",
        "EADDRNOTAVAIL", "ENETDOWN", "ENETUNREACH", "ENETRESET", "ECONNABORTED", "ECONNRESET", "ENOBUFS", "EISCONN",
        "ENOTCONN", "ESHUTDOWN", "ETOOMANYREFS", "ETIMEDOUT", "ECONNREFUSED", "ELOOP", "ENAMETOOLONG", "EHOSTDOWN",
        "EHOSTUNREACH", "ENOTEMPTY", "EPROCLIM", "EUSERS", "EDQUOT", "ESTALE", "EREMOTE", "EBADRPC", "ERPCMISMATCH",
        "EPROGUNAVAIL", "EPROGMISMATCH", "EPROCUNAVAIL", "ENOLCK", "ENOSYS", "EFTYPE", "EAUTH", "ENEEDAUTH", "EPWROFF",
        "EDEVERR", "EOVERFLOW", "EBADEXEC", "EBADARCH", "ESHLIBVERS", "EBADMACHO", "ECANCELED", "EIDRM", "ENOMSG",
        "EILSEQ", "ENOATTR", "EBADMSG", "EMULTIHOP", "ENODATA", "ENOLINK", "ENOSR", "ENOSTR", "EPROTO", "ETIME",
        "EOPNOTSUPP", "ENOPOLICY", "ENOTRECOVERABLE", "EOWNERDEAD", "EQFULL",
    ];
    for (i, want) in PYTHON.iter().enumerate() {
        let code = i as i32 + 1;
        assert_eq!(errno_name(&io::Error::from_raw_os_error(code)), *want, "errno {code}");
    }
}

#[test]
fn addresses_and_scopes_read_like_python() {
    assert_eq!(address(&"[::1]".into()), Some(ip("::1")));
    assert_eq!(address(&" 10.0.0.1 ".into()), Some(ip("10.0.0.1")));
    assert_eq!(address(&"fe80::1%en0".into()), Some(ip("fe80::1")));
    assert_eq!(address(&"::ffff:10.1.2.3".into()), Some(ip("10.1.2.3")));
    assert_eq!(address(&"010.0.0.1".into()), None); // leading zeros, as Python refuses them
    assert_eq!(scope(&"fe80::1%en0".into()), "en0");
    assert_eq!(scope(&"[fe80::1%en0]".into()), "en0");
    assert_eq!(scope(&("fe80::1", 1, 0, 4).into()), "4");
    assert_eq!(scope(&("fe80::1%en1", 1, 0, 0).into()), "en1");
    assert_eq!(scope(&"fe80::1".into()), "");
    assert_eq!(scope(&Peer::Ip(ip("fe80::1"))), "");
    assert_eq!(scope(&Peer::None), "");
}

// ---- test_lite_web_lan.py: the peer filter at the LAN door -------------------------------------------------------

#[test]
fn test_the_peer_filter_admits_home_networks_and_refuses_the_rest() {
    let home_peers: Vec<Peer> = vec![
        "127.0.0.1".into(),
        ("192.168.1.20", 50000).into(),
        "10.1.2.3".into(),
        "172.16.5.4".into(),
        "169.254.3.3".into(),
        "fe80::1%en0".into(),
        ("fd12:3456::1", 50000, 0, 0).into(),
        "::ffff:192.168.1.9".into(),
    ];
    for peer in home_peers {
        assert!(peer_allowed(peer.clone()), "{peer:?}");
    }
    let outside: Vec<Peer> = vec![
        "8.8.8.8".into(),
        ("1.1.1.1", 443).into(),
        "100.64.0.1".into(),
        "2001:4860:4860::8888".into(),
        "198.51.100.7".into(),
        "".into(),
        Peer::None,
        "not an address".into(),
    ];
    for peer in outside {
        assert!(!peer_allowed(peer.clone()), "{peer:?}");
    }
}
