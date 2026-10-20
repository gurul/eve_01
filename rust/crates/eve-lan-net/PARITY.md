# eve-lan-net
Status: complete

## Modules
| Python | Rust |
|---|---|
| `bridge/src/eve_bridge/lan_net.py` | `src/lib.rs` (tests in `src/tests.rs`) |

## API
| Python | Rust | Notes |
|---|---|---|
| `Address` | `Address` (`type Address = IpAddr`) | |
| `Network` | `struct Network` | `new` clears host bits (`strict=False`); `contains` is false across families, as `in` is |
| `PRIVATE_V4` | `static PRIVATE_V4` | 10/8, 172.16/12, 192.168/16 |
| `LINK_LOCAL_V4` | `static LINK_LOCAL_V4` | |
| `ULA` | `static ULA` | |
| `LINK_LOCAL_V6` | `static LINK_LOCAL_V6` | |
| `LAN_PEERS` | `fn lan_peers(version)` | the dict indexed by version |
| `MIN_ON_LINK_PREFIX` | `MIN_ON_LINK_PREFIX_V4`, `MIN_ON_LINK_PREFIX_V6`, `fn min_on_link_prefix(version)` | 16 and 48 |
| `NOT_LAN` | `static NOT_LAN` | anchored, since Python uses `fullmatch` |
| `SCUTIL` | `const SCUTIL` | `/usr/sbin/scutil` |
| `SCUTIL_SECS` | `const SCUTIL_SECS` | 5.0 |
| `LABEL` | `static LABEL` | anchored, since Python uses `fullmatch` |
| `MDNS_GROUP` | `const MDNS_GROUP` | 224.0.0.251:5353 |
| `DISCARD_PORT` | `const DISCARD_PORT` | 9 |
| `BLOCKED` | `const BLOCKED` | EHOSTUNREACH, EPERM |
| `log` | `tracing` | same messages and levels |
| `_address` | `fn address(&Peer)` | `Peer` stands for `addr: Any`: text, `IpAddr`, `SocketAddr`, a peername tuple's host and scope id, or `Peer::None` for anything else. Zone rules follow `ipaddress` (non-empty, no second `%`, any other text) |
| `_scope` | `fn scope(&Peer)` | |
| `_interface_ips` | `fn interface_ips()`, `fn interface_ips_from(Result<Vec<Adapter>, AdapterError>)` | `get_adapters()` ports `ifaddr.get_adapters()` (getifaddrs: grouped by name in first-seen order, AF_INET/AF_INET6 only, netmask prefix read as ifaddr does, a family-less netmask takes the address's family). Failure logs the Python exception type name |
| `_lan_interface` | `fn lan_interface` | |
| `_interface_order` | `fn interface_order` | `(0 or 1, stem, number)` |
| `private_addresses` | `fn private_addresses()`, `fn private_addresses_in(&[InterfaceIp])` | |
| `_on_link_networks` | `fn on_link_networks(&[InterfaceIp])` | |
| `peer_allowed` | `fn peer_allowed(impl Into<Peer>)`, `fn peer_allowed_with(&Peer, interfaces)` | the interface list is read only when the fixed ranges did not decide |
| `local_hostname` | `fn local_hostname()`, `fn local_hostname_with(run, hostname)` | `run_captured` is `subprocess.run(capture_output, stdin=DEVNULL, timeout)`; stdout must be UTF-8, as `text=True` |
| `_udp` | `fn udp(Family) -> Box<dyn UdpSocket>` | an unbound socket2 datagram socket; dropping it closes it |
| `_errno_name` | `fn errno_name(&io::Error)` | `errno.errorcode` names for the socket and process errnos; "OSError" otherwise |
| `trigger_local_network_alert` | `fn trigger_local_network_alert()`, `fn trigger_local_network_alert_with(make)` | |
| `probe` | `fn probe(impl Into<Peer>)`, `fn probe_with(&Peer, interfaces, make)` | returns "ok", "blocked" or "unknown" |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `test_lan_net.py` | `test_lan_peers_are_allowed` | `test_lan_peers_are_allowed` | |
| `test_lan_net.py` | `test_public_and_odd_addresses_are_refused` | `test_public_and_odd_addresses_are_refused` | the last assertion checks Python's own `ipaddress.is_private`, not lan_net; the same addresses are asserted refused |
| `test_lan_net.py` | `test_home_ipv6_on_the_macs_own_prefix_is_allowed` | same name | |
| `test_lan_net.py` | `test_peername_tuples_and_address_objects_are_read` | same name (plus `SocketAddr` peers) | |
| `test_lan_net.py` | `test_unreadable_interfaces_mean_no_lan_and_no_crash` | same name | |
| `test_lan_net.py` | `test_private_addresses_lists_the_lan_interfaces_best_first` | same name | |
| `test_lan_net.py` | `test_private_addresses_on_this_mac_are_rfc1918_ipv4` | same name | |
| `test_lan_net.py` | `test_local_hostname_reads_the_bonjour_name_with_scutil` | same name | |
| `test_lan_net.py` | `test_local_hostname_falls_back_to_a_dot_local_host_name` (6 params) | same name, a loop over the 6 outcomes | |
| `test_lan_net.py` | `test_local_hostname_on_this_mac_is_one_dns_label_or_empty` | same name, `cfg(target_os = "macos")` | |
| `test_lan_net.py` | `test_the_alert_trigger_connects_to_the_mdns_group_and_sends_nothing` | same name | |
| `test_lan_net.py` | `test_the_alert_trigger_never_raises` (3 params) | same name, a loop | |
| `test_lan_net.py` | `test_probe_sends_one_empty_datagram_to_the_discard_port` | same name | |
| `test_lan_net.py` | `test_probe_reads_a_refusal_as_blocked` (4 params) | same name, a loop | |
| `test_lan_net.py` | `test_probe_reads_other_errors_as_unknown` (4 params) | same name, a loop | |
| `test_lan_net.py` | `test_probe_sends_nothing_where_it_proves_nothing` (9 params) | same name, a loop | |
| `test_lan_net.py` | `test_probe_reaches_ipv6_peers_with_their_zone` | same name | |
| `test_lite_web_lan.py` | `test_the_peer_filter_admits_home_networks_and_refuses_the_rest` | same name | the rest of that file tests lite_web's LAN door |
| (new) | | `real_interfaces_are_read_and_normalized`, `netmasks_read_the_way_ipaddress_reads_them`, `interfaces_order_en0_first_then_by_stem_and_number`, `not_lan_names_are_whole_names`, `on_link_networks_keep_narrow_lan_prefixes`, `local_hostname_edges`, `run_captured_times_out_and_reads_stdout`, `probe_reads_a_numeric_zone_in_text_and_refuses_an_unknown_one`, `errno_names_match_python`, `addresses_and_scopes_read_like_python` | |

Checked by hand on this Mac (2026-10-19): `get_adapters()` gave the same 23 (interface, address, prefix) rows as Python's `ifaddr.get_adapters()`, and `private_addresses()`, `local_hostname()` and `peer_allowed()` for three sample peers matched Python's `lan_net`.

## Boundaries
None. scutil is started as a process, as in Python.

## Differences
- On BSD and macOS, a sockaddr is read only up to its `sa_len` (the rest counts as zero, as a routing sockaddr's truncated tail means). ifaddr reads the full struct size, which past a short netmask is memory that belongs to something else. On this Mac both give the same rows.
- A zone that names no interface (`fe80::1%nosuch0`) makes `probe` log `gaierror`; Python logs whatever `errno.errorcode` holds for the getaddrinfo code (ENOEXEC on macOS). Both return "unknown".
- `errno_name` knows the socket and process errnos (about 75); an errno outside that list logs "OSError" where Python would log its name.
