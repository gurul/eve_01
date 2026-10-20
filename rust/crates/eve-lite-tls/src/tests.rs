//! Unit tests for the helpers behind `LocalTls`; the module's own tests (test_lite_tls.py) are in
//! `tests/lite_tls.rs`.

use super::*;

const GOLDEN_PLIST: &str = include_str!("../tests/golden.plist");

#[test]
fn label_takes_one_dns_label_with_or_without_dot_local() {
    assert_eq!(label("Kitchen-Mac"), "Kitchen-Mac");
    assert_eq!(label("Kitchen-Mac.local"), "Kitchen-Mac");
    assert_eq!(label("Kitchen-Mac.LOCAL."), "Kitchen-Mac");
    assert_eq!(label("  Kitchen-Mac \u{1c}"), "Kitchen-Mac");
    assert_eq!(label("not one label"), "");
    assert_eq!(label("a.b"), "");
    assert_eq!(label(""), "");
    assert_eq!(label(".local"), "");
    assert_eq!(label("-bad"), "");
    assert_eq!(label(&"a".repeat(64)), "");
    assert_eq!(label(&"a".repeat(63)), "a".repeat(63));
}

#[test]
fn san_addresses_keeps_private_ipv4_in_order_once_and_at_most_16() {
    let got = san_addresses(vec![
        Peer::from("8.8.8.8"),
        Peer::from(" 10.0.0.7 "),
        Peer::None,
        Peer::from("10.0.0.7"),
        Peer::from("010.0.0.8"), // leading zeros: ipaddress refuses them
        Peer::from("fd00::5"),
        Peer::from("127.0.0.5"),
        Peer::Ip("192.168.1.20".parse().unwrap()),
        Peer::from("172.31.255.255"),
        Peer::from("172.32.0.1"),
        Peer::from("169.254.3.4"),
    ]);
    assert_eq!(
        got,
        ["10.0.0.7", "192.168.1.20", "172.31.255.255", "169.254.3.4"]
    );
    let many: Vec<String> = (1..=20).map(|i| format!("192.168.5.{i}")).collect();
    assert_eq!(san_addresses(many.clone()), many[..16].to_vec());
}

#[test]
fn the_plist_writer_matches_plistlib_byte_for_byte() {
    // tests/golden.plist is plistlib.dumps(..., fmt=FMT_XML) of this value, from Python 3.13.
    let der: Vec<u8> = (0..=255u8).chain(0..=255u8).collect();
    let root = Plist::Dict(vec![
        (
            "PayloadCertificateFileName".into(),
            Plist::Str("eve-lite-ca.cer".into()),
        ),
        ("PayloadContent".into(), Plist::Data(der)),
        ("PayloadDescription".into(), Plist::Str("x & <y>".into())),
        ("PayloadDisplayName".into(), Plist::Str("T".into())),
        ("PayloadIdentifier".into(), Plist::Str("a.root".into())),
        (
            "PayloadType".into(),
            Plist::Str("com.apple.security.root".into()),
        ),
        ("PayloadUUID".into(), Plist::Str("U".into())),
        ("PayloadVersion".into(), Plist::Int(1)),
    ]);
    // Keys out of order on purpose: plistlib sorts them.
    let profile = Plist::Dict(vec![
        ("PayloadVersion".into(), Plist::Int(1)),
        ("PayloadContent".into(), Plist::Array(vec![root])),
        ("PayloadDescription".into(), Plist::Str("d\r\ne".into())),
        ("PayloadDisplayName".into(), Plist::Str("T".into())),
        ("PayloadIdentifier".into(), Plist::Str("a".into())),
        ("PayloadOrganization".into(), Plist::Str("eve lite".into())),
        ("PayloadRemovalDisallowed".into(), Plist::Bool(false)),
        ("PayloadType".into(), Plist::Str("Configuration".into())),
        ("PayloadUUID".into(), Plist::Str("V".into())),
    ]);
    assert_eq!(
        String::from_utf8(plist_xml(&profile).unwrap()).unwrap(),
        GOLDEN_PLIST
    );
}

#[test]
fn the_plist_writer_refuses_control_characters_and_writes_empty_containers() {
    assert!(plist_xml(&Plist::Str("a\x01b".into())).is_err());
    let out = String::from_utf8(
        plist_xml(&Plist::Array(vec![
            Plist::Dict(vec![]),
            Plist::Array(vec![]),
        ]))
        .unwrap(),
    )
    .unwrap();
    assert!(out.contains("<array>\n\t<dict/>\n\t<array/>\n</array>\n</plist>\n"));
}

/// The ECPrivateKey (SEC1) inside a PKCS#8 key ring made: the content of its privateKey OCTET STRING.
fn sec1_of(pkcs8: &[u8]) -> Vec<u8> {
    fn header(b: &[u8]) -> (usize, usize) {
        // (header length, content length)
        if b[1] < 0x80 {
            (2, b[1] as usize)
        } else {
            let n = (b[1] & 0x7f) as usize;
            (
                2 + n,
                b[2..2 + n]
                    .iter()
                    .fold(0usize, |acc, x| (acc << 8) | *x as usize),
            )
        }
    }
    let (h, _) = header(pkcs8);
    let mut rest = &pkcs8[h..];
    for _ in 0..2 {
        // the version, then the algorithm
        let (h, len) = header(rest);
        rest = &rest[h + len..];
    }
    assert_eq!(rest[0], 0x04);
    let (h, len) = header(rest);
    rest[h..h + len].to_vec()
}

#[test]
fn a_sec1_ec_private_key_loads_as_python_loads_it() {
    let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
    let sec1 = sec1_of(key.serialized_der());
    let pem = pem_encode("EC PRIVATE KEY", &sec1);
    let loaded = key_from_pem(pem.as_bytes()).expect("a SEC1 key loads");
    assert_eq!(spki(&loaded), spki(&key));
    // OpenSSL writes the curve into a SEC1 key ([0] parameters); that loads too.
    let p256: [u8; 12] = [
        0xa0, 0x0a, 0x06, 0x08, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07,
    ];
    let body = &sec1[2..]; // ring's ECPrivateKey is short enough for a one-byte length
    let at = body.iter().position(|b| *b == 0xa1).unwrap();
    let mut inner = body[..at].to_vec();
    inner.extend_from_slice(&p256);
    inner.extend_from_slice(&body[at..]);
    let mut with_params = vec![0x30];
    with_params.extend(der_len(inner.len()));
    with_params.extend(inner);
    let loaded =
        key_from_pem(pem_encode("EC PRIVATE KEY", &with_params).as_bytes()).expect("loads");
    assert_eq!(spki(&loaded), spki(&key));
    // The PKCS#8 form, after other PEM blocks, as Python finds it.
    let mixed = format!("{}{}", pem_encode("CERTIFICATE", b"x"), key.serialize_pem());
    assert!(key_from_pem(mixed.as_bytes()).is_some());
    assert!(key_from_pem(b"garbage").is_none());
    assert!(key_from_pem(pem_encode("ENCRYPTED PRIVATE KEY", b"x").as_bytes()).is_none());
    // Another curve is not this module's key.
    let p384 = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P384_SHA384).unwrap();
    assert!(key_from_pem(p384.serialize_pem().as_bytes()).is_none());
}

#[test]
fn der_lengths_are_minimal() {
    assert_eq!(der_len(5), [5]);
    assert_eq!(der_len(127), [127]);
    assert_eq!(der_len(128), [0x81, 128]);
    assert_eq!(der_len(300), [0x82, 1, 44]);
}

#[test]
fn serials_are_positive_and_at_most_20_bytes() {
    for _ in 0..64 {
        let s = random_serial().unwrap().to_bytes();
        assert!(!s.is_empty() && s.len() <= 20);
        assert!(s[0] != 0 || s.len() == 1);
        if s.len() == 20 {
            assert!(s[0] < 0x80);
        }
    }
}

#[test]
fn realpath_resolves_what_exists_and_appends_the_rest() {
    let tmp = tempfile::tempdir().unwrap();
    let real = fs::canonicalize(tmp.path()).unwrap();
    std::os::unix::fs::symlink(&real, real.join("link")).unwrap();
    assert_eq!(
        realpath(&real.join("link").join("missing").join("..").join("web-tls")),
        real.join("web-tls")
    );
    assert_eq!(realpath(&real.join("./a/b")), real.join("a").join("b"));
    // Two spellings of one folder share one lock.
    assert!(Arc::ptr_eq(
        &folder_lock(&real.join("x")),
        &folder_lock(&real.join("link").join("x"))
    ));
}

#[test]
fn a_ca_without_the_constraints_is_not_usable() {
    let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
    let now = Utc::now();
    let make = |nc: Option<NameConstraints>, bc: BasicConstraints| {
        let mut p = CertificateParams::default();
        p.distinguished_name = common_name(CA_NAME);
        p.not_before = to_offset(now - backdate()).unwrap();
        p.not_after = to_offset(now + Duration::days(CA_DAYS)).unwrap();
        p.is_ca = IsCa::Ca(bc);
        p.name_constraints = nc;
        Cert::from_der(p.self_signed(&key).unwrap().der().to_vec()).unwrap()
    };
    let ours = || {
        Some(NameConstraints {
            permitted_subtrees: permitted_subtrees(),
            excluded_subtrees: vec![],
        })
    };
    assert!(ca_usable(
        &make(ours(), BasicConstraints::Constrained(0)),
        now
    ));
    assert!(!ca_usable(
        &make(None, BasicConstraints::Constrained(0)),
        now
    ));
    assert!(!ca_usable(
        &make(ours(), BasicConstraints::Unconstrained),
        now
    ));
    assert!(!ca_usable(
        &make(ours(), BasicConstraints::Constrained(1)),
        now
    ));
    let mut wider = permitted_subtrees();
    wider.push(GeneralSubtree::DnsName("example.com".into()));
    let wide = Some(NameConstraints {
        permitted_subtrees: wider,
        excluded_subtrees: vec![],
    });
    assert!(!ca_usable(
        &make(wide, BasicConstraints::Constrained(0)),
        now
    ));
    let excl = Some(NameConstraints {
        permitted_subtrees: permitted_subtrees(),
        excluded_subtrees: vec![GeneralSubtree::DnsName("bad.local".into())],
    });
    assert!(!ca_usable(
        &make(excl, BasicConstraints::Constrained(0)),
        now
    ));
    // RENEW_DAYS before its end it is no longer usable.
    assert!(!ca_usable(
        &make(ours(), BasicConstraints::Constrained(0)),
        now + Duration::days(CA_DAYS - 29)
    ));
}

#[test]
fn write_private_replaces_atomically_and_refuses_a_planted_symlink_tmp() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("ca.key");
    write_private(&path, b"one").unwrap();
    write_private(&path, b"two").unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"two");
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o7777,
        0o600
    );
    // A leftover temporary file is removed first; a symlink there is unlinked, never followed.
    let target = tmp.path().join("elsewhere");
    fs::write(&target, b"keep").unwrap();
    std::os::unix::fs::symlink(&target, tmp.path().join(".ca.key.tmp")).unwrap();
    write_private(&path, b"three").unwrap();
    assert_eq!(fs::read(&target).unwrap(), b"keep");
    assert_eq!(fs::read(&path).unwrap(), b"three");
    let names: Vec<_> = fs::read_dir(tmp.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names.len(), 2);
}

#[test]
fn a_cert_reads_from_the_first_certificate_block_only_whole() {
    let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
    let der = CertificateParams::default()
        .self_signed(&key)
        .unwrap()
        .der()
        .to_vec();
    let cert = Cert::from_der(der.clone()).unwrap();
    let both = [key.serialize_pem().into_bytes(), cert.pem()].concat();
    assert_eq!(Cert::from_pem(&both), Some(cert.clone()));
    let mut trailing = der.clone();
    trailing.push(0);
    assert!(Cert::from_pem(pem_encode("CERTIFICATE", &trailing).as_bytes()).is_none());
    assert!(Cert::from_pem(b"not a certificate").is_none());
}

#[test]
fn monotonic_is_the_clock_python_reads_and_moves_forward() {
    // Python 3.13's time.monotonic() on macOS is mach_absolute_time() since boot (sleep excluded): far above zero on
    // any machine that has been up for more than a moment, and never going back.
    let a = monotonic();
    let b = monotonic();
    assert!(a > 0.0 && b >= a, "{a} {b}");
}
