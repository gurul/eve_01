//! lite_tls: the local CA and the LAN door's certificate (port of bridge/tests/test_lite_tls.py).
//!
//! Real certificates in a temporary folder, and real TLS handshakes on an ephemeral loopback port, made the way the
//! LAN door makes them: a plain TCP accept, then the TLS accept. Clients verify with webpki, as strictly as a
//! browser, name constraints included.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::net::IpAddr;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::{Arc, Barrier};

use chrono::{Duration, Utc};
use eve_lan_net::Network;
use eve_lite_tls::{
    read_cert, san, san_dns, san_ips, verify_directly_issued_by, Cert, LocalTls, Peer,
};
use rcgen::{
    CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair, SanType,
    PKCS_ECDSA_P256_SHA256,
};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use rustls::{ClientConfig, ProtocolVersion, RootCertStore, ServerConfig};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::{TlsAcceptor, TlsConnector};
use x509_parser::prelude::{GeneralName, X509Certificate};

const HOST: &str = "Kitchen-Mac";
const LAN: &str = "192.168.1.20";

// The constraints as the spec writes them (docs/plans/eve-lite.md M5), independent of the code's own list.
const SPEC_DNS: [&str; 2] = ["local", "localhost"];
const SPEC_NETS: [&str; 7] = [
    "10.0.0.0/8",
    "172.16.0.0/12",
    "192.168.0.0/16",
    "169.254.0.0/16",
    "127.0.0.0/8",
    "fc00::/7",
    "fe80::/10",
];

fn tls(tmp: &Path) -> LocalTls {
    let t = LocalTls::new(tmp.join("web-tls"));
    assert!(t.refresh([LAN], HOST).unwrap());
    t
}

fn cert(path: &Path) -> Cert {
    read_cert(path).expect("a certificate")
}

fn parsed(c: &Cert) -> X509Certificate<'_> {
    x509_parser::parse_x509_certificate(c.der()).unwrap().1
}

fn sans(c: &Cert) -> (Vec<String>, Vec<String>) {
    let names = san(Some(c)).expect("a SAN");
    (san_dns(&names), san_ips(&names))
}

fn set(items: &[&str]) -> HashSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}

fn cn(c: &Cert) -> String {
    c.common_name().expect("a common name")
}

/// Does a SAN entry satisfy the spec's constraints (RFC 5280: a DNS constraint covers itself and its subdomains)?
fn permitted(name: &GeneralName<'_>) -> bool {
    match name {
        GeneralName::DNSName(d) => {
            let value = d.to_lowercase();
            SPEC_DNS
                .iter()
                .any(|c| value == *c || value.ends_with(&format!(".{c}")))
        }
        GeneralName::IPAddress(b) => {
            let ip: IpAddr = match b.len() {
                4 => IpAddr::from(<[u8; 4]>::try_from(*b).unwrap()),
                16 => IpAddr::from(<[u8; 16]>::try_from(*b).unwrap()),
                _ => return false,
            };
            SPEC_NETS.iter().any(|n| {
                let (addr, prefix) = n.split_once('/').unwrap();
                Network::new(addr.parse().unwrap(), prefix.parse().unwrap()).contains(&ip)
            })
        }
        _ => false,
    }
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o7777
}

// ---- the CA -----------------------------------------------------------------------------------------------------

#[test]
fn test_the_ca_is_name_constrained_to_local_names_and_private_ranges() {
    let tmp = tempfile::tempdir().unwrap();
    let t = tls(tmp.path());
    let ca = cert(&t.ca_cert_path);
    let c = parsed(&ca);
    let limits = c.name_constraints().unwrap().unwrap();
    assert!(limits.critical);
    assert!(limits.value.excluded_subtrees.is_none());
    let mut got: HashSet<String> = HashSet::new();
    for tree in limits.value.permitted_subtrees.as_ref().unwrap() {
        got.insert(match &tree.base {
            GeneralName::DNSName(d) => d.to_string(),
            GeneralName::IPAddress(b) => {
                let (addr, mask) = b.split_at(b.len() / 2);
                let prefix: u32 = mask.iter().map(|m| m.count_ones()).sum();
                let addr: IpAddr = if addr.len() == 4 {
                    IpAddr::from(<[u8; 4]>::try_from(addr).unwrap())
                } else {
                    IpAddr::from(<[u8; 16]>::try_from(addr).unwrap())
                };
                format!("{addr}/{prefix}")
            }
            other => format!("{other:?}"),
        });
    }
    let want: HashSet<String> = SPEC_DNS
        .iter()
        .chain(SPEC_NETS.iter())
        .map(|s| s.to_string())
        .collect();
    assert_eq!(got, want);
}

#[test]
fn test_the_ca_signs_leaves_only_and_names_this_mac() {
    let tmp = tempfile::tempdir().unwrap();
    let t = tls(tmp.path());
    let ca = cert(&t.ca_cert_path);
    let c = parsed(&ca);
    let basic = c.basic_constraints().unwrap().unwrap();
    assert!(basic.critical && basic.value.ca && basic.value.path_len_constraint == Some(0));
    let usage = c.key_usage().unwrap().unwrap();
    assert!(usage.critical && usage.value.key_cert_sign() && usage.value.crl_sign());
    assert!(!usage.value.digital_signature() && !usage.value.key_encipherment());
    assert_eq!(
        c.public_key().algorithm.algorithm.to_id_string(),
        "1.2.840.10045.2.1"
    );
    let curve = c
        .public_key()
        .algorithm
        .parameters
        .as_ref()
        .unwrap()
        .as_oid()
        .unwrap();
    assert_eq!(curve.to_id_string(), "1.2.840.10045.3.1.7"); // secp256r1
    assert_eq!(cn(&ca), "eve lite local CA (Kitchen-Mac)");
    assert_eq!(c.subject().as_raw(), c.issuer().as_raw());
    let life = ca.not_valid_after().unwrap() - ca.not_valid_before().unwrap();
    assert!(Duration::days(5 * 365) <= life && life <= Duration::days(5 * 365 + 2));
    // VERIFY_X509_STRICT wants it on a CA.
    assert!(c.iter_extensions().any(|e| matches!(
        e.parsed_extension(),
        x509_parser::extensions::ParsedExtension::SubjectKeyIdentifier(_)
    )));
}

#[test]
fn test_a_long_mac_name_still_fits_the_ca_common_name() {
    let tmp = tempfile::tempdir().unwrap();
    let label = format!("M{}c", "a".repeat(61)); // 63 characters: the longest DNS label
    let t = LocalTls::new(tmp.path().join("web-tls"));
    t.refresh([LAN], &label).unwrap();
    let name = cn(&cert(&t.ca_cert_path));
    assert!(
        name.chars().count() <= 64
            && name.starts_with("eve lite local CA (Maaa")
            && name.ends_with(')')
    );
    assert!(t
        .names()
        .unwrap()
        .contains(&format!("{}.local", label.to_lowercase())));
}

// ---- the leaf ---------------------------------------------------------------------------------------------------

#[test]
fn test_the_leaf_names_this_mac_and_serves_only_as_a_server() {
    let tmp = tempfile::tempdir().unwrap();
    let t = tls(tmp.path());
    let (ca, leaf) = (cert(&t.ca_cert_path), cert(&t.leaf_cert_path));
    let (dns, ips) = sans(&leaf);
    assert_eq!(dns, ["kitchen-mac.local", "localhost"]);
    assert_eq!(ips, ["127.0.0.1", LAN]);
    let c = parsed(&leaf);
    let eku = c.extended_key_usage().unwrap().unwrap();
    assert!(
        eku.value.server_auth
            && !eku.value.client_auth
            && !eku.value.any
            && eku.value.other.is_empty()
    );
    assert!(!eku.value.code_signing && !eku.value.email_protection && !eku.value.time_stamping);
    let basic = c.basic_constraints().unwrap().unwrap();
    assert!(basic.critical && !basic.value.ca);
    let usage = c.key_usage().unwrap().unwrap();
    assert!(usage.critical && usage.value.digital_signature() && !usage.value.key_cert_sign());
    assert!(
        leaf.not_valid_after().unwrap() - leaf.not_valid_before().unwrap() <= Duration::days(397)
    );
    let now = Utc::now();
    assert!(leaf.not_valid_before().unwrap() <= now && now < leaf.not_valid_after().unwrap());
    let curve = c
        .public_key()
        .algorithm
        .parameters
        .as_ref()
        .unwrap()
        .as_oid()
        .unwrap();
    assert_eq!(curve.to_id_string(), "1.2.840.10045.3.1.7");
    assert!(verify_directly_issued_by(&leaf, &ca));
    // VERIFY_X509_STRICT wants it on a leaf.
    assert!(c.iter_extensions().any(|e| matches!(
        e.parsed_extension(),
        x509_parser::extensions::ParsedExtension::AuthorityKeyIdentifier(_)
    )));
    assert_eq!(
        t.names().unwrap(),
        set(&["kitchen-mac.local", "localhost", "127.0.0.1", LAN])
    );
}

#[test]
fn test_every_leaf_name_is_inside_the_ca_constraints() {
    let tmp = tempfile::tempdir().unwrap();
    let t = LocalTls::new(tmp.path().join("web-tls"));
    let given: Vec<Peer> = vec![
        "8.8.8.8".into(),
        "fd00::5".into(),
        "fe80::1".into(),
        "not an address".into(),
        Peer::None,
        "".into(),
        LAN.into(),
        "127.0.0.5".into(),
        "100.64.0.1".into(),
        "172.32.0.1".into(),
        " 10.0.0.7 ".into(),
        "169.254.3.4".into(),
    ];
    t.refresh(given, HOST).unwrap();
    let leaf = cert(&t.leaf_cert_path);
    let c = parsed(&leaf);
    let names = c.subject_alternative_name().unwrap().unwrap();
    assert!(names.value.general_names.iter().all(permitted));
    assert_eq!(sans(&leaf).1, ["127.0.0.1", LAN, "10.0.0.7", "169.254.3.4"]);
    // Control: the checker refuses what the CA must never vouch for.
    assert!(
        !permitted(&GeneralName::DNSName("bank.example.com"))
            && !permitted(&GeneralName::DNSName("notlocal"))
    );
    assert!(!permitted(&GeneralName::IPAddress(&[8, 8, 8, 8])));
}

#[test]
fn test_mac_names_are_one_dns_label_in_any_case() {
    let tmp = tempfile::tempdir().unwrap();
    let t = tls(tmp.path());
    assert!(!t.refresh([LAN], "KITCHEN-MAC").unwrap()); // the same name: certificates are case-blind
    assert!(!t.refresh([LAN], "Kitchen-Mac.local").unwrap());
    assert!(!t.refresh([LAN], "").unwrap()); // unknown: the leaf keeps its name
    assert!(!t.refresh([LAN], "not one label").unwrap());
    assert!(t.refresh([LAN], "Den-Mac").unwrap()); // renamed: the QR code's .local name changes
    assert_eq!(
        sans(&cert(&t.leaf_cert_path)).0,
        ["den-mac.local", "localhost"]
    );
}

#[test]
fn test_a_mac_without_a_bonjour_name_still_gets_a_leaf() {
    let tmp = tempfile::tempdir().unwrap();
    let t = LocalTls::new(tmp.path().join("web-tls"));
    assert!(t.refresh(Vec::<Peer>::new(), "").unwrap());
    assert_eq!(t.names().unwrap(), set(&["localhost", "127.0.0.1"]));
    assert_eq!(cn(&cert(&t.ca_cert_path)), "eve lite local CA");
}

// ---- the files --------------------------------------------------------------------------------------------------

#[test]
fn test_keys_are_0600_and_the_folder_0700() {
    let tmp = tempfile::tempdir().unwrap();
    let t = tls(tmp.path());
    assert_eq!(mode(&t.root), 0o700);
    for path in [
        &t.ca_key_path,
        &t.leaf_key_path,
        &t.ca_cert_path,
        &t.leaf_cert_path,
    ] {
        assert_eq!(mode(path), 0o600);
    }
    let has_key = |p: &Path| {
        String::from_utf8(fs::read(p).unwrap())
            .unwrap()
            .contains("PRIVATE KEY")
    };
    assert!(has_key(&t.ca_key_path) && has_key(&t.leaf_key_path));
    let mut names: Vec<String> = fs::read_dir(&t.root)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["ca.key", "ca.pem", "leaf.key", "leaf.pem"]);
}

// test_modes_hold_under_any_umask_and_loose_ones_are_tightened is in tests/umask.rs: umask is process-wide, so it
// runs in a process of its own.

#[test]
fn test_an_unchanged_address_set_does_not_reissue_and_a_new_address_does() {
    let tmp = tempfile::tempdir().unwrap();
    let t = tls(tmp.path());
    let (ca_bytes, leaf_bytes) = (
        fs::read(&t.ca_cert_path).unwrap(),
        fs::read(&t.leaf_cert_path).unwrap(),
    );
    assert!(!t.refresh([LAN], HOST).unwrap());
    assert!(!t.refresh(Vec::<Peer>::new(), HOST).unwrap()); // an address went away: kept, no reissue
    assert_eq!(fs::read(&t.leaf_cert_path).unwrap(), leaf_bytes);
    assert!(t.refresh(["10.0.0.7", LAN], HOST).unwrap()); // a new address
    assert_eq!(
        sans(&cert(&t.leaf_cert_path)).1,
        ["127.0.0.1", "10.0.0.7", LAN]
    );
    assert!(!t.refresh([LAN, "10.0.0.7"], HOST).unwrap()); // the same set in another order
    assert!(!t.refresh(["10.0.0.7"], HOST).unwrap());
    assert_eq!(fs::read(&t.ca_cert_path).unwrap(), ca_bytes); // the CA phones installed never changes
}

#[test]
fn test_the_addresses_are_a_union_capped_at_16_newest_first() {
    let tmp = tempfile::tempdir().unwrap();
    let t = LocalTls::new(tmp.path().join("web-tls"));
    let first: Vec<String> = (1..11).map(|i| format!("10.0.0.{i}")).collect();
    let second: Vec<String> = (1..11).map(|i| format!("10.0.1.{i}")).collect();
    t.refresh(first.clone(), HOST).unwrap();
    assert!(t.refresh(second.clone(), HOST).unwrap());
    let ips = sans(&cert(&t.leaf_cert_path)).1;
    let want: Vec<String> = std::iter::once("127.0.0.1".to_string())
        .chain(second.iter().cloned())
        .chain(first[..6].iter().cloned())
        .collect();
    assert_eq!(ips, want);
    assert!(!t.refresh(second.clone(), HOST).unwrap()); // at the cap, no reissue loop
    let many: Vec<String> = (1..21).map(|i| format!("192.168.5.{i}")).collect();
    assert!(t.refresh(many.clone(), HOST).unwrap());
    let want: Vec<String> = std::iter::once("127.0.0.1".to_string())
        .chain(many[..16].iter().cloned())
        .collect();
    assert_eq!(sans(&cert(&t.leaf_cert_path)).1, want);
    assert!(!t.refresh(many, HOST).unwrap());
}

#[test]
fn test_damaged_or_mismatched_files_are_made_again() {
    let tmp = tempfile::tempdir().unwrap();
    let t = tls(tmp.path());
    fs::write(&t.leaf_cert_path, b"not a certificate").unwrap();
    assert!(t.refresh([LAN], HOST).unwrap());
    let other = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
    fs::write(&t.leaf_key_path, other.serialize_pem()).unwrap();
    assert!(t.refresh([LAN], HOST).unwrap()); // a key that is not the leaf's
    let old_ca = fs::read(&t.ca_cert_path).unwrap();
    fs::write(&t.ca_key_path, b"garbage").unwrap();
    assert!(t.refresh([LAN], HOST).unwrap());
    assert_ne!(fs::read(&t.ca_cert_path).unwrap(), old_ca); // a new CA, and a leaf it signed
    assert!(verify_directly_issued_by(
        &cert(&t.leaf_cert_path),
        &cert(&t.ca_cert_path)
    ));
    // The names carried over.
    assert_eq!(
        t.names().unwrap(),
        set(&["kitchen-mac.local", "localhost", "127.0.0.1", LAN])
    );
}

#[test]
fn test_a_leaf_near_its_end_is_renewed_with_its_names() {
    let tmp = tempfile::tempdir().unwrap();
    let t = tls(tmp.path());
    t.refresh(["10.0.0.7"], HOST).unwrap();
    let ca_bytes = fs::read(&t.ca_cert_path).unwrap();
    let later = Utc::now() + Duration::days(380);
    t.set_clock(move || later);
    assert!(t.refresh(Vec::<Peer>::new(), "").unwrap());
    let leaf = cert(&t.leaf_cert_path);
    assert!(leaf.not_valid_before().unwrap() <= later && later < leaf.not_valid_after().unwrap());
    assert_eq!(
        sans(&leaf),
        (
            vec!["kitchen-mac.local".to_string(), "localhost".to_string()],
            vec![
                "127.0.0.1".to_string(),
                "10.0.0.7".to_string(),
                LAN.to_string()
            ]
        )
    );
    assert_eq!(fs::read(&t.ca_cert_path).unwrap(), ca_bytes);
    assert!(!t.refresh(Vec::<Peer>::new(), "").unwrap());
}

#[test]
fn test_the_ca_is_renewed_before_it_ends_and_never_for_a_clock_that_jumped_back() {
    let tmp = tempfile::tempdir().unwrap();
    let t = tls(tmp.path());
    let ca = cert(&t.ca_cert_path);
    let ca_bytes = fs::read(&t.ca_cert_path).unwrap();
    let back = ca.not_valid_before().unwrap() - Duration::days(2);
    t.set_clock(move || back);
    t.refresh([LAN], HOST).unwrap();
    assert_eq!(fs::read(&t.ca_cert_path).unwrap(), ca_bytes); // the CA phones installed stays
    let near_end = ca.not_valid_after().unwrap() - Duration::days(10);
    t.set_clock(move || near_end);
    assert!(t.refresh([LAN], HOST).unwrap());
    let new_ca = cert(&t.ca_cert_path);
    assert!(
        fs::read(&t.ca_cert_path).unwrap() != ca_bytes
            && new_ca.not_valid_after() > ca.not_valid_after()
    );
    let leaf = cert(&t.leaf_cert_path);
    assert!(verify_directly_issued_by(&leaf, &new_ca));
    assert!(leaf.not_valid_after() <= new_ca.not_valid_after());
}

// test_logs_carry_no_name_and_no_address is in tests/logs.rs: a scoped tracing subscriber misses events while
// other threads register the same callsites, so it runs in a process of its own.

// ---- the downloads ----------------------------------------------------------------------------------------------

/// Every leaf value of a parsed plist.
fn values(node: &plist::Value) -> Vec<plist::Value> {
    match node {
        plist::Value::Dictionary(d) => d.values().flat_map(values).collect(),
        plist::Value::Array(a) => a.iter().flat_map(values).collect(),
        other => vec![other.clone()],
    }
}

fn key_der(path: &Path) -> Vec<u8> {
    let blocks = pem::parse_many(fs::read(path).unwrap()).unwrap();
    blocks
        .into_iter()
        .find(|p| p.tag() == "PRIVATE KEY")
        .unwrap()
        .into_contents()
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

fn carries_key(profile: &plist::Value, keys: &[Vec<u8>]) -> bool {
    values(profile).iter().any(|v| {
        let blob = match v {
            plist::Value::Data(d) => d.clone(),
            plist::Value::String(s) => s.as_bytes().to_vec(),
            other => format!("{other:?}").into_bytes(),
        };
        contains(&blob, b"PRIVATE KEY") || keys.iter().any(|k| contains(&blob, k))
    })
}

fn parse_plist(raw: &[u8]) -> plist::Value {
    plist::Value::from_reader_xml(raw).unwrap()
}

#[test]
fn test_the_downloads_carry_no_private_key() {
    let tmp = tempfile::tempdir().unwrap();
    let t = tls(tmp.path());
    let keys = vec![key_der(&t.ca_key_path), key_der(&t.leaf_key_path)];
    let pem = t.ca_pem().unwrap();
    assert!(pem.starts_with(b"-----BEGIN CERTIFICATE-----") && !contains(&pem, b"PRIVATE KEY"));
    assert_eq!(Cert::from_pem(&pem), Some(cert(&t.ca_cert_path)));
    // The file on disk is that same encoding (cryptography's public_bytes(PEM)), byte for byte.
    assert_eq!(fs::read(&t.ca_cert_path).unwrap(), pem);
    let raw = t.mobileconfig().unwrap();
    assert!(!contains(&raw, b"PRIVATE KEY"));
    let profile = parse_plist(&raw);
    assert!(!carries_key(&profile, &keys));
    // Control: the scan finds a key that is there.
    let mut inner = plist::Dictionary::new();
    inner.insert("PayloadContent".into(), plist::Value::Data(keys[0].clone()));
    let mut outer = plist::Dictionary::new();
    outer.insert(
        "PayloadContent".into(),
        plist::Value::Array(vec![plist::Value::Dictionary(inner)]),
    );
    assert!(carries_key(&plist::Value::Dictionary(outer), &keys));
}

#[test]
fn test_a_key_appended_to_ca_pem_never_reaches_the_download() {
    let tmp = tempfile::tempdir().unwrap();
    let t = tls(tmp.path());
    let both = [
        fs::read(&t.ca_cert_path).unwrap(),
        fs::read(&t.ca_key_path).unwrap(),
    ]
    .concat();
    fs::write(&t.ca_cert_path, both).unwrap();
    assert!(contains(
        &fs::read(&t.ca_cert_path).unwrap(),
        b"PRIVATE KEY"
    )); // control: the file has one
    assert!(!contains(&t.ca_pem().unwrap(), b"PRIVATE KEY"));
    assert!(!contains(&t.mobileconfig().unwrap(), b"PRIVATE KEY"));
}

fn get<'a>(d: &'a plist::Value, key: &str) -> &'a plist::Value {
    d.as_dictionary()
        .unwrap()
        .get(key)
        .unwrap_or_else(|| panic!("{key} missing"))
}

fn text<'a>(d: &'a plist::Value, key: &str) -> &'a str {
    get(d, key).as_string().unwrap()
}

#[test]
fn test_the_mobileconfig_installs_this_ca_as_a_root() {
    let tmp = tempfile::tempdir().unwrap();
    let t = tls(tmp.path());
    let raw = t.mobileconfig().unwrap();
    assert_eq!(raw, t.mobileconfig().unwrap()); // stable from one download to the next
    let profile = parse_plist(&raw);
    assert_eq!(text(&profile, "PayloadType"), "Configuration");
    assert_eq!(get(&profile, "PayloadVersion").as_signed_integer(), Some(1));
    assert_eq!(
        get(&profile, "PayloadRemovalDisallowed").as_boolean(),
        Some(false)
    );
    let content = get(&profile, "PayloadContent").as_array().unwrap();
    assert_eq!(content.len(), 1);
    let root = &content[0];
    assert_eq!(text(root, "PayloadType"), "com.apple.security.root");
    assert_eq!(get(root, "PayloadVersion").as_signed_integer(), Some(1));
    assert_eq!(
        get(root, "PayloadContent").as_data().unwrap(),
        cert(&t.ca_cert_path).der()
    );
    assert_eq!(
        text(root, "PayloadDisplayName"),
        "eve lite local CA (Kitchen-Mac)"
    );
    assert_eq!(
        text(&profile, "PayloadDisplayName"),
        "eve lite local CA (Kitchen-Mac)"
    );
    let uuids: HashSet<uuid::Uuid> = [text(&profile, "PayloadUUID"), text(root, "PayloadUUID")]
        .iter()
        .map(|u| uuid::Uuid::parse_str(u).unwrap())
        .collect();
    assert_eq!(uuids.len(), 2);
    assert!(text(root, "PayloadIdentifier")
        .starts_with(&format!("{}.", text(&profile, "PayloadIdentifier"))));
    let other = LocalTls::new(tmp.path().join("other-mac")); // another Mac's profile never replaces it
    other.refresh([LAN], HOST).unwrap();
    let theirs = parse_plist(&other.mobileconfig().unwrap());
    assert_ne!(
        text(&theirs, "PayloadIdentifier"),
        text(&profile, "PayloadIdentifier")
    );
    assert_ne!(text(&theirs, "PayloadUUID"), text(&profile, "PayloadUUID"));
}

// ---- handshakes -------------------------------------------------------------------------------------------------

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// A client that trusts `ca_pem` alone and verifies as a browser does, offering h2 and HTTP/1.1.
fn client(ca_pem: &[u8]) -> Arc<ClientConfig> {
    let mut roots = RootCertStore::empty();
    for c in CertificateDer::pem_slice_iter(ca_pem) {
        roots.add(c.unwrap()).unwrap();
    }
    let mut cfg = ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    cfg.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Arc::new(cfg)
}

type Seen = (String, Option<Vec<u8>>, Option<ProtocolVersion>);

/// One connection to a door-shaped server on an ephemeral loopback port: plain accept, then the TLS accept, then
/// one line echoed. Returns (the line, the ALPN protocol, the TLS version) as the client saw them.
async fn handshake(
    server: Arc<ServerConfig>,
    client_cfg: Arc<ClientConfig>,
    server_name: &str,
) -> io::Result<Seen> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let door = tokio::spawn(async move {
        let Ok((sock, _)) = listener.accept().await else {
            return;
        };
        // The client refusing the certificate ends here.
        let Ok(stream) = TlsAcceptor::from(server).accept(sock).await else {
            return;
        };
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        if reader.read_line(&mut line).await.is_ok() {
            let stream = reader.get_mut();
            let _ = stream.write_all(format!("echo {line}").as_bytes()).await;
            let _ = stream.flush().await;
            let _ = stream.shutdown().await;
        }
    });
    let name = ServerName::try_from(server_name.to_string()).map_err(io::Error::other)?;
    let result = async {
        let sock = TcpStream::connect(("127.0.0.1", port)).await?;
        let connect = TlsConnector::from(client_cfg).connect(name, sock);
        let stream = tokio::time::timeout(std::time::Duration::from_secs(10), connect).await??;
        let (alpn, version) = {
            let (_, conn) = stream.get_ref();
            (
                conn.alpn_protocol().map(<[u8]>::to_vec),
                conn.protocol_version(),
            )
        };
        let mut reader = BufReader::new(stream);
        reader.get_mut().write_all(b"hello\n").await?;
        let mut line = String::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            reader.read_line(&mut line),
        )
        .await??;
        Ok::<Seen, io::Error>((line, alpn, version))
    }
    .await;
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), door).await;
    result
}

/// The certificate error inside a failed handshake, if that is what failed it.
fn cert_error(e: &io::Error) -> Option<rustls::CertificateError> {
    match e.get_ref()?.downcast_ref::<rustls::Error>()? {
        rustls::Error::InvalidCertificate(c) => Some(c.clone()),
        _ => None,
    }
}

#[tokio::test]
async fn test_a_client_that_trusts_ca_pem_completes_the_handshake() {
    let tmp = tempfile::tempdir().unwrap();
    let t = tls(tmp.path());
    for name in ["localhost", "kitchen-mac.local", "127.0.0.1", LAN] {
        let (line, alpn, version) =
            handshake(t.context().unwrap(), client(&t.ca_pem().unwrap()), name)
                .await
                .unwrap();
        assert_eq!(line, "echo hello\n");
        assert_eq!(alpn.as_deref(), Some(&b"http/1.1"[..]));
        assert!(matches!(
            version,
            Some(ProtocolVersion::TLSv1_2 | ProtocolVersion::TLSv1_3)
        ));
    }
}

#[tokio::test]
async fn test_the_server_context_refuses_old_tls() {
    let tmp = tempfile::tempdir().unwrap();
    let t = tls(tmp.path());
    let ctx = t.context().unwrap();
    // TLS 1.2 and 1.3 both complete, and a client that brings no certificate is served (a server: browsers bring
    // none).
    for version in [&rustls::version::TLS12, &rustls::version::TLS13] {
        let mut roots = RootCertStore::empty();
        roots
            .add(CertificateDer::from_pem_slice(&t.ca_pem().unwrap()).unwrap())
            .unwrap();
        let cfg = ClientConfig::builder_with_provider(provider())
            .with_protocol_versions(&[version])
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let (line, _alpn, seen) = handshake(ctx.clone(), Arc::new(cfg), "localhost")
            .await
            .unwrap();
        assert_eq!(line, "echo hello\n");
        assert_eq!(seen, Some(version.version));
    }
    // A TLS 1.0 ClientHello (ECDHE AES128-GCM suites, P-256, ECDSA-SHA256 signatures) is refused with a
    // protocol_version alert.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = ctx.clone();
    let door = tokio::spawn(async move {
        let (sock, _) = listener.accept().await.unwrap();
        TlsAcceptor::from(server)
            .accept(sock)
            .await
            .err()
            .map(|e| e.to_string())
    });
    let mut body = vec![0x03, 0x01];
    body.extend([7u8; 32]);
    body.extend([0x00, 0x00, 0x04, 0xc0, 0x2b, 0xc0, 0x2f, 0x01, 0x00]);
    let extensions = [
        0x00, 0x0d, 0x00, 0x04, 0x00, 0x02, 0x04,
        0x03, // signature_algorithms: ecdsa_secp256r1_sha256
        0x00, 0x0a, 0x00, 0x04, 0x00, 0x02, 0x00, 0x17, // supported_groups: secp256r1
        0x00, 0x0b, 0x00, 0x02, 0x01, 0x00, // ec_point_formats: uncompressed
    ];
    body.extend([0x00, extensions.len() as u8]);
    body.extend(extensions);
    let mut hello = vec![0x01, 0x00, 0x00, body.len() as u8];
    hello.extend(body);
    let mut record = vec![0x16, 0x03, 0x01, 0x00, hello.len() as u8];
    record.extend(hello);
    let mut sock = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    sock.write_all(&record).await.unwrap();
    let mut reply = [0u8; 7];
    sock.read_exact(&mut reply).await.unwrap();
    assert_eq!(reply[0], 0x15, "an alert record: {reply:?}");
    assert_eq!(reply[6], 70, "protocol_version: {reply:?}");
    let refused = door.await.unwrap().expect("the door refused the hello");
    assert!(refused.contains("Tls12NotOffered"), "{refused}");
    assert!(Arc::ptr_eq(&t.context().unwrap(), &ctx)); // one object until a reissue
}

#[tokio::test]
async fn test_a_client_that_does_not_trust_the_ca_or_asks_another_name_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let t = tls(tmp.path());
    let stranger = LocalTls::new(tmp.path().join("another-mac"));
    stranger.refresh([LAN], HOST).unwrap();
    let refused = handshake(
        t.context().unwrap(),
        client(&stranger.ca_pem().unwrap()),
        "localhost",
    )
    .await
    .unwrap_err();
    // The stranger's CA has this one's name, so webpki tries it and the signature fails; either way, refused.
    assert!(
        matches!(
            cert_error(&refused),
            Some(rustls::CertificateError::BadSignature | rustls::CertificateError::UnknownIssuer)
        ),
        "{refused}"
    );
    for name in ["den-mac.local", "192.168.1.99"] {
        let refused = handshake(t.context().unwrap(), client(&t.ca_pem().unwrap()), name)
            .await
            .unwrap_err();
        assert!(
            matches!(
                cert_error(&refused),
                Some(rustls::CertificateError::NotValidForNameContext { .. })
            ),
            "{name}: {refused}"
        );
    }
}

/// A server context for a leaf signed with the eve CA's own key, as a thief of that key would make one.
fn rogue(t: &LocalTls, names: Vec<SanType>) -> Arc<ServerConfig> {
    let ca = cert(&t.ca_cert_path);
    let ca_key = KeyPair::from_pem(&fs::read_to_string(&t.ca_key_path).unwrap()).unwrap();
    let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
    let now = time::OffsetDateTime::now_utc();
    let mut params = CertificateParams::default();
    params.distinguished_name = rcgen::DistinguishedName::new();
    params.distinguished_name.push(DnType::CommonName, "rogue");
    params.not_before = now - time::Duration::hours(1);
    params.not_after = now + time::Duration::days(30);
    params.is_ca = IsCa::ExplicitNoCa;
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    params.subject_alt_names = names;
    params.use_authority_key_identifier_extension = true;
    let issuer = Issuer::from_ca_cert_der(&CertificateDer::from(ca.der()), &ca_key).unwrap();
    let leaf = params.signed_by(&key, &issuer).unwrap();
    let key = PrivateKeyDer::from_pem_slice(key.serialize_pem().as_bytes()).unwrap();
    let cfg = ServerConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![leaf.der().clone()], key)
        .unwrap();
    Arc::new(cfg)
}

#[tokio::test]
async fn test_the_ca_cannot_vouch_for_a_real_site_even_with_its_own_key() {
    let tmp = tempfile::tempdir().unwrap();
    let t = tls(tmp.path());
    let cases: [(SanType, &str); 2] = [
        (
            SanType::DnsName("bank.example.com".try_into().unwrap()),
            "bank.example.com",
        ),
        (SanType::IpAddress("8.8.8.8".parse().unwrap()), "8.8.8.8"),
    ];
    for (name, host) in cases {
        let refused = handshake(rogue(&t, vec![name]), client(&t.ca_pem().unwrap()), host)
            .await
            .unwrap_err();
        // A name constraint, not some other failure.
        let err = cert_error(&refused).expect("a certificate error");
        assert!(
            format!("{err:?}").contains("NameConstraintViolation"),
            "{host}: {err:?}"
        );
        // Control: the same forgery for a local name passes, so the constraint is what stopped the one above.
        let local = SanType::DnsName("rogue.local".try_into().unwrap());
        let (line, _alpn, _version) = handshake(
            rogue(&t, vec![local]),
            client(&t.ca_pem().unwrap()),
            "rogue.local",
        )
        .await
        .unwrap();
        assert_eq!(line, "echo hello\n");
    }
}

#[tokio::test]
async fn test_the_context_follows_a_reissue() {
    let tmp = tempfile::tempdir().unwrap();
    let t = tls(tmp.path());
    let before = t.context().unwrap();
    assert!(t.refresh(["10.0.0.7"], HOST).unwrap());
    let after = t.context().unwrap();
    assert!(!Arc::ptr_eq(&after, &before));
    let (line, _alpn, _version) = handshake(after, client(&t.ca_pem().unwrap()), "10.0.0.7")
        .await
        .unwrap();
    assert_eq!(line, "echo hello\n");
    assert!(t.names().unwrap().contains("10.0.0.7"));
}

#[test]
fn test_the_context_renews_a_leaf_that_ran_down_while_serving() {
    let tmp = tempfile::tempdir().unwrap();
    let t = tls(tmp.path());
    let before = t.context().unwrap();
    let old_leaf = fs::read(&t.leaf_cert_path).unwrap();
    let later = Utc::now() + Duration::days(380);
    t.set_clock(move || later);
    assert!(Arc::ptr_eq(&t.context().unwrap(), &before)); // checked at most once a day
    t.set_recheck_secs(-1.0);
    let after = t.context().unwrap();
    assert!(!Arc::ptr_eq(&after, &before) && fs::read(&t.leaf_cert_path).unwrap() != old_leaf);
    assert_eq!(
        t.names().unwrap(),
        set(&["kitchen-mac.local", "localhost", "127.0.0.1", LAN])
    );
}

#[tokio::test]
async fn test_two_objects_on_one_folder_agree() {
    let tmp = tempfile::tempdir().unwrap();
    let t = tls(tmp.path());
    let before = t.context().unwrap();
    let other = LocalTls::new(&t.root);
    assert!(other.refresh(["10.0.0.7"], HOST).unwrap());
    assert!(!Arc::ptr_eq(&t.context().unwrap(), &before)); // it noticed the other's new leaf
    assert!(t.names().unwrap().contains("10.0.0.7"));
    let (line, _alpn, _version) = handshake(
        t.context().unwrap(),
        client(&t.ca_pem().unwrap()),
        "10.0.0.7",
    )
    .await
    .unwrap();
    assert_eq!(line, "echo hello\n");
}

#[test]
fn test_concurrent_first_use_makes_one_ca_and_one_leaf() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("web-tls");
    let barrier = Arc::new(Barrier::new(8));
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let (root, barrier) = (root.clone(), barrier.clone());
            std::thread::spawn(move || {
                let t = LocalTls::new(root);
                barrier.wait();
                t.refresh([LAN], HOST).unwrap()
            })
        })
        .collect();
    let mut issued: Vec<bool> = threads.into_iter().map(|h| h.join().unwrap()).collect();
    issued.sort();
    assert_eq!(
        issued,
        [false, false, false, false, false, false, false, true]
    );
    let (ca, leaf) = (cert(&root.join("ca.pem")), cert(&root.join("leaf.pem")));
    assert!(verify_directly_issued_by(&leaf, &ca));
    for (c, k) in [(&ca, root.join("ca.key")), (&leaf, root.join("leaf.key"))] {
        let key = eve_lite_tls::read_key(&k).unwrap();
        assert!(eve_lite_tls::matches(Some(c), Some(&key)));
    }
}

#[test]
fn test_every_method_works_before_any_refresh() {
    let tmp = tempfile::tempdir().unwrap();
    for call in ["context", "names", "ca_pem", "mobileconfig"] {
        let t = LocalTls::new(tmp.path().join(call));
        let ok = match call {
            "context" => t.context().is_ok(),
            "names" => !t.names().unwrap().is_empty(),
            "ca_pem" => !t.ca_pem().unwrap().is_empty(),
            _ => !t.mobileconfig().unwrap().is_empty(),
        };
        assert!(ok, "{call}");
        assert!(t.ca_key_path.exists() && t.leaf_key_path.exists());
    }
    let t = LocalTls::new(tmp.path().join("names"));
    assert_eq!(t.names().unwrap(), set(&["localhost", "127.0.0.1"]));
}

/// A restarted daemon (a new object on the folder) keeps every file as it is: nothing is reissued or rewritten.
#[test]
fn a_folder_another_object_made_is_kept_as_it_is() {
    let tmp = tempfile::tempdir().unwrap();
    let t = tls(tmp.path());
    let bytes: Vec<Vec<u8>> = [
        &t.ca_cert_path,
        &t.ca_key_path,
        &t.leaf_cert_path,
        &t.leaf_key_path,
    ]
    .iter()
    .map(|p| fs::read(p).unwrap())
    .collect();
    let fresh = LocalTls::new(tmp.path().join("web-tls"));
    assert!(!fresh.refresh([LAN], HOST).unwrap());
    let after: Vec<Vec<u8>> = [
        &fresh.ca_cert_path,
        &fresh.ca_key_path,
        &fresh.leaf_cert_path,
        &fresh.leaf_key_path,
    ]
    .iter()
    .map(|p| fs::read(p).unwrap())
    .collect();
    assert_eq!(bytes, after);
}
