# eve-lite-tls
Status: complete

## Modules
| Python | Rust |
|---|---|
| `bridge/src/eve_bridge/lite_tls.py` | `src/lib.rs` (unit tests in `src/tests.rs`; the module's tests in `tests/lite_tls.rs`, `tests/logs.rs`, `tests/umask.rs`) |

## API
| Python | Rust | Notes |
|---|---|---|
| `CA_DAYS`, `LEAF_DAYS`, `RENEW_DAYS`, `RECHECK_SECS`, `MAX_ADDRESSES`, `LOOPBACK` | same-named `const`s | same values |
| `BACKDATE` | `const BACKDATE_SECS` (3600) | one hour |
| `PERMITTED_DNS`, `PERMITTED_NETS` | same-named `const`s | same lists, same order |
| `SAN_NETS` | `static SAN_NETS` | `eve_lan_net::PRIVATE_V4` and `LINK_LOCAL_V4` |
| `CA_NAME`, `LEAF_NAME`, `PROFILE_ID`, `PROFILE_ORG`, `PROFILE_TEXT` | same-named `const`s | verbatim |
| `log` | `tracing` | same messages at INFO: `lite tls: made/replaced the local CA`, `lite tls: issued the LAN certificate (<reason>, <n> addresses)`; never a name or an address |
| `_now` | `fn now()`, `type Clock`, `LocalTls::set_clock` | the clock is per object so tests can move it without touching other threads |
| `_LOCKS`, `_LOCKS_LOCK` | `static LOCKS` | |
| `_folder_lock` | `fn folder_lock` | keyed by a port of non-strict `os.path.realpath` (`fn realpath`) |
| `permitted_subtrees` | `fn permitted_subtrees() -> Vec<rcgen::GeneralSubtree>` | |
| `_san_addresses` | `fn san_addresses` | takes `Into<Peer>` items (`eve_lan_net::Peer`) for `Iterable[Any]`: text is `str.strip()`ped and parsed as `IPv4Address` (leading zeros refused), `Peer::None` and peernames are dropped |
| `_label` | `fn label(&str)` | Python's `str.strip()` whitespace set, `rstrip(".")`, case-blind `.local`, `lan_net.LABEL` fullmatch |
| `_write_private` | `fn write_private` | `.name.tmp` unlinked first, `O_CREAT\|O_EXCL\|O_NOFOLLOW` 0600, `fchmod`, plain `fsync(2)` (not `F_FULLFSYNC`), `rename`; the tmp file removed on failure |
| `_read_cert` | `fn read_cert`, `Cert::from_pem` | first `CERTIFICATE`/`X509 CERTIFICATE` PEM block, whole DER (no trailing bytes) |
| `_read_key` | `fn read_key`, `fn key_from_pem` | first private-key PEM block; PKCS#8 and SEC1 (`EC PRIVATE KEY`) P-256 keys load; encrypted, RSA, DSA and other curves are None |
| `_spki` | `fn spki` (private) | SubjectPublicKeyInfo DER |
| `_matches` | `fn matches` | SPKI DER equality |
| `_ca_usable` | `fn ca_usable` | critical BasicConstraints CA with pathlen 0, critical NameConstraints whose permitted set equals `permitted_subtrees()` and no excluded subtrees, more than RENEW_DAYS left; start date not checked |
| `_leaf_usable` | `fn leaf_usable`, `fn verify_directly_issued_by` | issuer name equals the CA subject (raw DER) and the CA key's ECDSA-SHA256 signature verifies; then `not_before <= now < not_after - RENEW_DAYS` |
| `_san` | `fn san` -> `Vec<SanName>`, with `san_dns` / `san_ips` for `get_values_for_type` | |
| `_leaf_names` | `fn leaf_names` | |
| `LocalTls` | `struct LocalTls` | `root`, `ca_cert_path`, `ca_key_path`, `leaf_cert_path`, `leaf_key_path` public as in Python |
| `LocalTls.__init__` | `LocalTls::new(root)` | |
| `LocalTls.refresh` | `LocalTls::refresh(addresses, hostname) -> Result<bool>` | |
| `LocalTls.context` | `LocalTls::context() -> Result<Arc<rustls::ServerConfig>>` | for `tokio_rustls::TlsAcceptor` after a plain accept (the `start_tls` shape); the same `Arc` until the leaf changes (`Arc::ptr_eq`); rechecks the dates once `RECHECK_SECS` has passed (`set_recheck_secs` for tests) |
| `LocalTls.names` | `LocalTls::names() -> Result<HashSet<String>>` | |
| `LocalTls.ca_pem` | `LocalTls::ca_pem() -> Result<Vec<u8>>` | byte-identical to Python's (checked against cryptography 50) |
| `LocalTls.mobileconfig` | `LocalTls::mobileconfig() -> Result<Vec<u8>>` | byte-identical to Python's: `fn plist_xml` ports `plistlib.dumps(fmt=FMT_XML)` (tab indents, sorted keys, plistlib's base64 line width per depth, `_escape`); UUIDv5 under `NAMESPACE_DNS`, upper case |
| `LocalTls._ca` | `LocalTls::ca` (private) | error text `the local CA could not be read back` |
| `LocalTls._leaf_stamp` | `LocalTls::leaf_stamp` (private) | (inode, mtime in ns) |
| `LocalTls._current` | `LocalTls::current` (private) | |
| `LocalTls._private_dir` | `LocalTls::private_dir` (private), `fn mkdir` | `Path.mkdir(mode=0o700, parents=True, exist_ok=True)` semantics: parents with the default mode |
| `LocalTls._refresh` | `LocalTls::refresh_locked` (private) | same branching, reasons and address union |
| `LocalTls._make_ca` | `LocalTls::make_ca` (private) | rcgen: P-256, ECDSA-SHA256, 159-bit random serial (`random_serial_number`), CN `eve lite local CA (<label[:44]>)`, critical BasicConstraints(CA, pathlen 0), critical KeyUsage(keyCertSign, cRLSign), critical NameConstraints, SKI = SHA-1 of the public key bits, AKI = the SKI; key written before the certificate |
| `LocalTls._make_leaf` | `LocalTls::make_leaf` (private) | SAN order `<host>.local`, `localhost`, `127.0.0.1`, addresses; critical BasicConstraints(CA false), critical KeyUsage(digitalSignature), EKU serverAuth, SKI, AKI = SHA-1 of the CA public key bits; end capped at the CA's end |
| `LocalTls._server_context` | `fn server_context(cert_path, key_path)` | rustls with the ring provider: TLS 1.3 and 1.2 only, no client certificate, ALPN `http/1.1`; rustls never renegotiates (`OP_NO_RENEGOTIATION`) |

Interoperability was checked both ways against the Python module (cryptography 50, Python 3.13): a folder the Rust made is
kept by Python's `refresh` (False, nothing rewritten) and its leaf passes Python's strict (`VERIFY_X509_STRICT`)
handshakes for every name; a folder Python made is kept by Rust, and a Rust leaf issued on Python's CA is kept by
Python. `ca_pem()` and `mobileconfig()` were byte-identical.

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_lite_tls.py` | `test_the_ca_is_name_constrained_to_local_names_and_private_ranges`, `test_the_ca_signs_leaves_only_and_names_this_mac`, `test_a_long_mac_name_still_fits_the_ca_common_name`, `test_the_leaf_names_this_mac_and_serves_only_as_a_server`, `test_every_leaf_name_is_inside_the_ca_constraints`, `test_mac_names_are_one_dns_label_in_any_case`, `test_a_mac_without_a_bonjour_name_still_gets_a_leaf`, `test_keys_are_0600_and_the_folder_0700`, `test_an_unchanged_address_set_does_not_reissue_and_a_new_address_does`, `test_the_addresses_are_a_union_capped_at_16_newest_first`, `test_damaged_or_mismatched_files_are_made_again`, `test_a_leaf_near_its_end_is_renewed_with_its_names`, `test_the_ca_is_renewed_before_it_ends_and_never_for_a_clock_that_jumped_back`, `test_the_downloads_carry_no_private_key`, `test_a_key_appended_to_ca_pem_never_reaches_the_download`, `test_the_mobileconfig_installs_this_ca_as_a_root`, `test_a_client_that_trusts_ca_pem_completes_the_handshake` (4 names in one test), `test_the_server_context_refuses_old_tls`, `test_a_client_that_does_not_trust_the_ca_or_asks_another_name_is_refused`, `test_the_ca_cannot_vouch_for_a_real_site_even_with_its_own_key` (both cases in one test), `test_the_context_follows_a_reissue`, `test_the_context_renews_a_leaf_that_ran_down_while_serving`, `test_two_objects_on_one_folder_agree`, `test_concurrent_first_use_makes_one_ca_and_one_leaf`, `test_every_method_works_before_any_refresh` | same names in `tests/lite_tls.rs`, plus `a_folder_another_object_made_is_kept_as_it_is` | none |
| `bridge/tests/test_lite_tls.py` | `test_modes_hold_under_any_umask_and_loose_ones_are_tightened` | `tests/umask.rs` | none: own process, since the umask is process-wide |
| `bridge/tests/test_lite_tls.py` | `test_logs_carry_no_name_and_no_address` | `tests/logs.rs` | none: own process, since a scoped tracing subscriber misses events while other test threads register the same callsites |
| `bridge/tests/test_lite_web_local.py` (tests_ref) | uses `LocalTls(...)`, `tls.ca_cert_path`, `tls.names()` | covered by the tests above | the LAN door itself belongs to the lite-web crate |
| (none) | | `src/tests.rs`: `label_*`, `san_addresses_*`, `the_plist_writer_matches_plistlib_byte_for_byte` (golden from plistlib, `tests/golden.plist`), `the_plist_writer_refuses_control_characters_and_writes_empty_containers`, `a_sec1_ec_private_key_loads_as_python_loads_it`, `der_lengths_are_minimal`, `serials_are_positive_and_at_most_20_bytes`, `realpath_resolves_what_exists_and_appends_the_rest`, `a_ca_without_the_constraints_is_not_usable`, `write_private_replaces_atomically_and_refuses_a_planted_symlink_tmp`, `a_cert_reads_from_the_first_certificate_block_only_whole` | |

Test notes: the handshake client is rustls/webpki with only the CA as a root (browser-strict, name constraints
enforced) in place of OpenSSL with `VERIFY_X509_STRICT`. Where Python checks OpenSSL's `verify_message` for
"subtree", Rust checks webpki's `NameConstraintViolation`. A stranger CA with the same subject name fails as
`BadSignature` or `UnknownIssuer` (Python asserts only `SSLCertVerificationError`). "Refuses old TLS" is checked by
handshake: TLS 1.2 and 1.3 clients complete, a hand-made TLS 1.0 ClientHello gets a protocol_version alert.

## Boundaries
None.

## Differences
- `context()` returns `Arc<rustls::ServerConfig>` instead of an `ssl.SSLContext`. A client whose ALPN offer lacks
  `http/1.1` is refused with `no_application_protocol` (rustls follows RFC 7301); OpenSSL went on without ALPN.
  Every browser offers `http/1.1`.
- The once-a-day recheck in `context()` counts from the first call; Python's `time.monotonic() - 0.0` counted the
  first day from boot. Either way the dates are looked at again at most a day apart.
- Keys: only EC P-256 keys load (Python took an EC key on any curve), and a key without its public half embedded
  (ring requires it; OpenSSL always writes it) counts as damaged and is made again. This module and Python only ever
  write P-256 PKCS#8 keys with the public half, so neither case arises from either daemon's files.
- A certificate with a malformed extension (a duplicate extension, a non-contiguous network mask, a SAN address that
  is neither 4 nor 16 bytes) is treated as unsound or as lacking that extension and is made again; Python's
  `cryptography` raised out of `refresh`.
- Error texts when the leaf cannot be loaded into a context are rustls' and this module's, not OpenSSL's.
