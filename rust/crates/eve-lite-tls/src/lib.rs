//! HTTPS for eve lite's LAN door: a local CA that can vouch only for local names, and the certificate the door
//! serves (docs/plans/eve-lite.md M5). Port of `lite_tls.py`.
//!
//! A phone's browser lets a page use the microphone only in a secure context, and a phone reaches the Mac over the
//! LAN, so the LAN listener (lite_web) speaks TLS. No public CA signs for `<name>.local` or a private address, so
//! the Mac makes its own, under `~/.config/eve-bridge/web-tls/`:
//!
//! * **The CA** (`ca.pem`, `ca.key`): EC P-256, five years, CN "eve lite local CA (<LocalHostName>)". Critical
//!   name constraints permit only DNS names under `local` and `localhost` and the private, link-local and loopback
//!   address ranges: a client that applies them trusts nothing else from this CA, even signed with its stolen key
//!   (OpenSSL and webpki do; the tests forge one). Path length 0: it signs leaves only. Its key never leaves the Mac.
//! * **The leaf** (`leaf.pem`, `leaf.key`): EC P-256, 397 days, serverAuth, for `<LocalHostName>.local`,
//!   `localhost`, `127.0.0.1` and every private IPv4 address the Mac has had (newest first, at most 16).
//!   Reissuing brings the browser warning back on every phone that clicked through it instead of installing the CA,
//!   so it happens only when a new address or name appears, or the leaf has a month left.
//! * **The downloads**: [`LocalTls::ca_pem`] for `ca.crt`, and [`LocalTls::mobileconfig`], an unsigned profile with
//!   one `com.apple.security.root` payload. Both are encoded from the parsed CA certificate, so neither can carry a
//!   key.
//!
//! The folder is 0700 and every file in it 0600, each written atomically. A file that is missing, damaged or does not
//! match its key is made again rather than trusted. Logs carry counts and reasons, never a name or an address: the
//! Mac's name usually carries its owner's.
//!
//! The files are the ones the Python daemon reads and writes (PEM, PKCS#8 keys), so either daemon takes over the
//! other's CA without a phone noticing.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, Write};
use std::net::{IpAddr, Ipv4Addr};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::io::AsRawFd;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, LazyLock};

use anyhow::{anyhow, bail, Context as _, Result};
use base64::Engine as _;
use chrono::{DateTime, Duration, Utc};
use eve_lan_net::{Network, LABEL, LINK_LOCAL_V4, PRIVATE_V4};
use parking_lot::Mutex;
use rcgen::{
    BasicConstraints, CertificateParams, CidrSubnet, CustomExtension, DistinguishedName, DnType,
    DnValue, ExtendedKeyUsagePurpose, GeneralSubtree, IsCa, Issuer, KeyIdMethod, KeyPair,
    KeyUsagePurpose, NameConstraints, SanType, SerialNumber, PKCS_ECDSA_P256_SHA256,
};
use ring::rand::SecureRandom as _;
use rustls::pki_types::pem::PemObject as _;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::ServerConfig;
use sha2::{Digest as _, Sha256};
use tracing::info;
use x509_parser::prelude::{GeneralName, X509Certificate};

/// What `refresh` takes for `addresses: Iterable[Any]`: text, an address object, or anything else (`None`).
pub use eve_lan_net::Peer;

/// The CA's life: five years.
pub const CA_DAYS: i64 = 5 * 365;
/// Under Apple's caps on a TLS server certificate: 398 days, 825 for any CA.
pub const LEAF_DAYS: i64 = 397;
/// A certificate with this little left is replaced.
pub const RENEW_DAYS: i64 = 30;
/// A phone whose clock runs a little behind still takes a fresh certificate: one hour (`BACKDATE`).
pub const BACKDATE_SECS: i64 = 3600;
/// `context()` looks at the leaf's dates again after this long.
pub const RECHECK_SECS: f64 = 86400.0;
/// Private IPv4 addresses in the leaf, newest first.
pub const MAX_ADDRESSES: usize = 16;
/// The loopback address every leaf names.
pub const LOOPBACK: &str = "127.0.0.1";

/// The DNS names the CA may vouch for, with their subdomains.
pub const PERMITTED_DNS: [&str; 2] = ["local", "localhost"];
/// The address ranges the CA may vouch for.
pub const PERMITTED_NETS: [&str; 7] = [
    "10.0.0.0/8",
    "172.16.0.0/12",
    "192.168.0.0/16",
    "169.254.0.0/16",
    "127.0.0.0/8",
    "fc00::/7",
    "fe80::/10",
];

/// The leaf's own addresses: the private IPv4 ranges among the permitted ones (loopback is 127.0.0.1 alone). An
/// address outside the constraints would make a phone that installed the CA refuse the whole certificate.
pub static SAN_NETS: LazyLock<[Network; 4]> =
    LazyLock::new(|| [PRIVATE_V4[0], PRIVATE_V4[1], PRIVATE_V4[2], LINK_LOCAL_V4]);

pub const CA_NAME: &str = "eve lite local CA";
pub const LEAF_NAME: &str = "eve lite";
pub const PROFILE_ID: &str = "com.github.eve-bridge.lite.ca";
pub const PROFILE_ORG: &str = "eve lite";
pub const PROFILE_TEXT: &str = "Lets this device open eve lite on your Mac without a certificate warning. The \
certificate can vouch only for .local names and private network addresses. On an iPhone or iPad, also turn it on \
in Settings > General > About > Certificate Trust Settings.";

/// The clock `_refresh` reads (`_now`); tests replace it to stand at another date.
pub type Clock = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;

/// `_now`: the current time in UTC.
pub fn now() -> DateTime<Utc> {
    Utc::now()
}

/// Python's `time.monotonic()`: seconds since boot on the clock CPython reads (`mach_absolute_time`, which is
/// `CLOCK_UPTIME_RAW`, on macOS; `CLOCK_MONOTONIC` elsewhere). Like Python's `_checked = 0.0`, the first recheck is
/// due once the machine has been up longer than `RECHECK_SECS`.
pub fn monotonic() -> f64 {
    #[cfg(target_os = "macos")]
    const CLOCK: libc::clockid_t = libc::CLOCK_UPTIME_RAW;
    #[cfg(not(target_os = "macos"))]
    const CLOCK: libc::clockid_t = libc::CLOCK_MONOTONIC;
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: `ts` is a valid, writable timespec for the duration of the call.
    if unsafe { libc::clock_gettime(CLOCK, &mut ts) } != 0 {
        return 0.0;
    }
    ts.tv_sec as f64 + ts.tv_nsec as f64 / 1e9
}

fn backdate() -> Duration {
    Duration::seconds(BACKDATE_SECS)
}

// ---- the folder lock --------------------------------------------------------------------------------------------

// One lock per folder for the whole process: two LocalTls on one folder must not both make a CA at once and leave
// one's key beside the other's certificate.
static LOCKS: LazyLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// `os.path.realpath` (non-strict): the path made absolute, its longest existing prefix with symlinks resolved, and
/// the rest appended with `.` and `..` taken lexically.
fn realpath(path: &Path) -> PathBuf {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    };
    let parts: Vec<Component<'_>> = abs.components().collect();
    let mut base = PathBuf::from("/");
    let mut rest: &[Component<'_>] = &parts;
    for k in (1..=parts.len()).rev() {
        if let Ok(real) = fs::canonicalize(parts[..k].iter().collect::<PathBuf>()) {
            base = real;
            rest = &parts[k..];
            break;
        }
    }
    for c in rest {
        match *c {
            Component::ParentDir => {
                base.pop();
            }
            Component::Normal(name) => base.push(name),
            Component::CurDir | Component::RootDir | Component::Prefix(_) => {}
        }
    }
    base
}

/// `_folder_lock`: the one lock for `root`'s folder.
fn folder_lock(root: &Path) -> Arc<Mutex<()>> {
    LOCKS
        .lock()
        .entry(realpath(root))
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

// ---- the names --------------------------------------------------------------------------------------------------

fn parse_net(net: &str) -> (IpAddr, u8) {
    let (addr, prefix) = net.split_once('/').unwrap_or((net, "0"));
    (
        addr.parse().unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED)),
        prefix.parse().unwrap_or(0),
    )
}

/// What the CA may vouch for: DNS names under `local` and `localhost`, and the private address ranges.
pub fn permitted_subtrees() -> Vec<GeneralSubtree> {
    let mut names: Vec<GeneralSubtree> = PERMITTED_DNS
        .iter()
        .map(|d| GeneralSubtree::DnsName(d.to_string()))
        .collect();
    for net in PERMITTED_NETS {
        let (addr, prefix) = parse_net(net);
        names.push(GeneralSubtree::IpAddress(CidrSubnet::from_addr_prefix(
            addr, prefix,
        )));
    }
    names
}

/// One permitted subtree as it is encoded: (GeneralName tag, the bytes). Python compares parsed `GeneralName`s; for
/// a well-formed DNS name or network that is the same as comparing these.
fn subtree_key(tree: &GeneralSubtree) -> (u8, Vec<u8>) {
    match tree {
        GeneralSubtree::DnsName(d) => (2, d.as_bytes().to_vec()),
        GeneralSubtree::IpAddress(net) => {
            let (addr, mask) = match *net {
                CidrSubnet::V4(a, m) => (a.to_vec(), m.to_vec()),
                CidrSubnet::V6(a, m) => (a.to_vec(), m.to_vec()),
            };
            (7, [addr, mask].concat())
        }
        // This code permits DNS names and networks only.
        _ => (0, Vec::new()),
    }
}

/// Python's `str.isspace()` for one character: Unicode White_Space plus the separators \x1c-\x1f.
fn py_space(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

/// `str(a)` for an address the way `_san_addresses` sees it: text as given, an address object as its text, and
/// anything else as something no address parser takes.
fn peer_text(a: &Peer) -> Option<String> {
    match a {
        Peer::Text(t) => Some(t.clone()),
        Peer::Ip(ip) => Some(ip.to_string()),
        // None, a socket address or a peername tuple: `str()` of those is never a bare IPv4 address.
        Peer::None | Peer::Socket(_) | Peer::Tuple { .. } => None,
    }
}

/// `_san_addresses`: the private IPv4 addresses among `addresses`, in order, each once, at most MAX_ADDRESSES.
/// Anything else (IPv6, public, loopback, not an address) is dropped.
pub fn san_addresses<I>(addresses: I) -> Vec<String>
where
    I: IntoIterator,
    I::Item: Into<Peer>,
{
    let mut out: Vec<String> = Vec::new();
    for a in addresses {
        let Some(text) = peer_text(&a.into()) else {
            continue;
        };
        let Ok(ip) = text.trim_matches(py_space).parse::<Ipv4Addr>() else {
            continue;
        };
        let ip = IpAddr::V4(ip);
        let s = ip.to_string();
        if SAN_NETS.iter().any(|net| net.contains(&ip)) && !out.contains(&s) {
            out.push(s);
        }
    }
    out.truncate(MAX_ADDRESSES);
    out
}

/// `_label`: the LocalHostName as given ("Kitchen-Mac", or "Kitchen-Mac.local"), without ".local", or "" when it
/// is not one DNS label.
pub fn label(hostname: &str) -> String {
    let mut name = hostname.trim_matches(py_space).trim_end_matches('.');
    let chars: Vec<char> = name.chars().collect();
    if chars.len() >= 6 {
        let tail: String = chars[chars.len() - 6..]
            .iter()
            .collect::<String>()
            .to_lowercase();
        if tail == ".local" {
            let cut = name.char_indices().rev().nth(5).map_or(0, |(i, _)| i);
            name = &name[..cut];
        }
    }
    if LABEL.is_match(name) {
        name.to_string()
    } else {
        String::new()
    }
}

// ---- the files --------------------------------------------------------------------------------------------------

fn is_not_found(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::NotFound
}

/// `_write_private`: write `data` to `path` atomically and owner-only (0600), whatever the umask.
pub fn write_private(path: &Path, data: &[u8]) -> io::Result<()> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = path.with_file_name(format!(".{name}.tmp"));
    match fs::remove_file(&tmp) {
        Err(e) if !is_not_found(&e) => return Err(e),
        _ => {}
    }
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&tmp)?;
    let written = (|| -> io::Result<()> {
        f.set_permissions(fs::Permissions::from_mode(0o600))?;
        f.write_all(data)?;
        f.flush()?;
        // os.fsync: a plain fsync(2), as the Python does (sync_all would ask macOS for F_FULLFSYNC).
        if unsafe { libc::fsync(f.as_raw_fd()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        drop(f);
        fs::rename(&tmp, path)
    })();
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written
}

/// A certificate read from disk or just made: its DER, parsed on use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cert {
    der: Vec<u8>,
}

impl Cert {
    /// The certificate from DER, or None when it does not parse as one whole certificate.
    pub fn from_der(der: Vec<u8>) -> Option<Cert> {
        match x509_parser::parse_x509_certificate(&der) {
            Ok(([], _)) => Some(Cert { der }),
            _ => None,
        }
    }

    /// The first certificate in PEM text (`x509.load_pem_x509_certificate`), or None.
    pub fn from_pem(data: &[u8]) -> Option<Cert> {
        let blocks = pem::parse_many(data).ok()?;
        let block = blocks
            .into_iter()
            .find(|p| p.tag() == "CERTIFICATE" || p.tag() == "X509 CERTIFICATE")?;
        Cert::from_der(block.into_contents())
    }

    pub fn der(&self) -> &[u8] {
        &self.der
    }

    /// The certificate as PEM (`public_bytes(Encoding.PEM)`): 64-column base64, LF line ends.
    pub fn pem(&self) -> Vec<u8> {
        pem_encode("CERTIFICATE", &self.der).into_bytes()
    }

    fn x509(&self) -> Option<X509Certificate<'_>> {
        x509_parser::parse_x509_certificate(&self.der)
            .ok()
            .map(|(_, c)| c)
    }

    /// The SubjectPublicKeyInfo DER.
    fn spki(&self) -> Option<Vec<u8>> {
        self.x509().map(|c| c.public_key().raw.to_vec())
    }

    pub fn not_valid_before(&self) -> Option<DateTime<Utc>> {
        self.x509()
            .and_then(|c| DateTime::from_timestamp(c.validity().not_before.timestamp(), 0))
    }

    pub fn not_valid_after(&self) -> Option<DateTime<Utc>> {
        self.x509()
            .and_then(|c| DateTime::from_timestamp(c.validity().not_after.timestamp(), 0))
    }

    /// The first common name of the subject.
    pub fn common_name(&self) -> Option<String> {
        let c = self.x509()?;
        let cn = c.subject().iter_common_name().next()?;
        cn.as_str().ok().map(str::to_string)
    }
}

fn pem_encode(tag: &str, der: &[u8]) -> String {
    pem::encode_config(
        &pem::Pem::new(tag, der.to_vec()),
        pem::EncodeConfig::new().set_line_ending(pem::LineEnding::LF),
    )
}

/// `_read_cert`: the certificate at `path`, or None when it is missing or damaged.
pub fn read_cert(path: &Path) -> Option<Cert> {
    Cert::from_pem(&fs::read(path).ok()?)
}

/// DER length octets.
fn der_len(n: usize) -> Vec<u8> {
    if n < 0x80 {
        return vec![n as u8];
    }
    let bytes: Vec<u8> = n
        .to_be_bytes()
        .iter()
        .copied()
        .skip_while(|b| *b == 0)
        .collect();
    let mut out = vec![0x80 | bytes.len() as u8];
    out.extend(bytes);
    out
}

/// A SEC1 `EC PRIVATE KEY` wrapped as PKCS#8 for a P-256 key (the only curve this module makes). A key on another
/// curve then fails to load, as its private key has the wrong length.
fn sec1_to_pkcs8(sec1: &[u8]) -> Vec<u8> {
    const ALG: [u8; 21] = [
        0x30, 0x13, 0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01, 0x06, 0x08, 0x2a, 0x86,
        0x48, 0xce, 0x3d, 0x03, 0x01, 0x07,
    ];
    let mut body = vec![0x02, 0x01, 0x00];
    body.extend_from_slice(&ALG);
    body.push(0x04);
    body.extend(der_len(sec1.len()));
    body.extend_from_slice(sec1);
    let mut out = vec![0x30];
    out.extend(der_len(body.len()));
    out.extend(body);
    out
}

const KEY_TAGS: [&str; 5] = [
    "PRIVATE KEY",
    "ENCRYPTED PRIVATE KEY",
    "RSA PRIVATE KEY",
    "EC PRIVATE KEY",
    "DSA PRIVATE KEY",
];

/// A private key from PEM text (`serialization.load_pem_private_key(..., password=None)`), kept only when it is an
/// EC P-256 key.
pub fn key_from_pem(data: &[u8]) -> Option<KeyPair> {
    let blocks = pem::parse_many(data).ok()?;
    let block = blocks.into_iter().find(|p| KEY_TAGS.contains(&p.tag()))?;
    let der = match block.tag() {
        "PRIVATE KEY" => block.into_contents(),
        "EC PRIVATE KEY" => sec1_to_pkcs8(block.contents()),
        _ => return None, // encrypted (no password given), or not an EC key
    };
    KeyPair::from_pkcs8_der_and_sign_algo(&der.into(), &PKCS_ECDSA_P256_SHA256).ok()
}

/// `_read_key`: the EC private key at `path`, or None when it is missing, damaged or another kind of key.
pub fn read_key(path: &Path) -> Option<KeyPair> {
    key_from_pem(&fs::read(path).ok()?)
}

/// `_spki`: a key's SubjectPublicKeyInfo DER.
fn spki(key: &KeyPair) -> Vec<u8> {
    rcgen::PublicKeyData::subject_public_key_info(key)
}

/// `_matches`: the certificate holds this key's public half.
pub fn matches(cert: Option<&Cert>, key: Option<&KeyPair>) -> bool {
    match (cert, key) {
        (Some(c), Some(k)) => c.spki().is_some_and(|s| s == spki(k)),
        _ => false,
    }
}

/// `_ca_usable`: a CA made here: still a CA, constrained exactly as this code constrains it, with more than
/// RENEW_DAYS left. Its start date is not checked: a Mac clock that jumped back must not throw away the CA phones
/// installed.
pub fn ca_usable(ca: &Cert, now: DateTime<Utc>) -> bool {
    let Some(c) = ca.x509() else { return false };
    let (Ok(Some(basic)), Ok(Some(limits))) = (c.basic_constraints(), c.name_constraints()) else {
        return false;
    };
    let want: HashSet<(u8, Vec<u8>)> = permitted_subtrees().iter().map(subtree_key).collect();
    let mut have: HashSet<(u8, Vec<u8>)> = HashSet::new();
    for tree in limits.value.permitted_subtrees.iter().flatten() {
        let key = match &tree.base {
            GeneralName::DNSName(d) => (2, d.as_bytes().to_vec()),
            GeneralName::IPAddress(b) => (7, b.to_vec()),
            // Anything else in the list makes it differ from this code's list.
            _ => (0, Vec::new()),
        };
        have.insert(key);
    }
    let excluded = limits
        .value
        .excluded_subtrees
        .as_ref()
        .is_some_and(|e| !e.is_empty());
    let Some(not_after) = ca.not_valid_after() else {
        return false;
    };
    basic.critical
        && basic.value.ca
        && basic.value.path_len_constraint == Some(0)
        && limits.critical
        && !excluded
        && have == want
        && now < not_after - Duration::days(RENEW_DAYS)
}

/// `verify_directly_issued_by`: `leaf`'s issuer is `ca`'s subject and `ca`'s key signed it.
pub fn verify_directly_issued_by(leaf: &Cert, ca: &Cert) -> bool {
    let (Some(l), Some(c)) = (leaf.x509(), ca.x509()) else {
        return false;
    };
    l.issuer().as_raw() == c.subject().as_raw() && l.verify_signature(Some(c.public_key())).is_ok()
}

/// `_leaf_usable`: signed by this CA, already valid, and with more than RENEW_DAYS left.
pub fn leaf_usable(leaf: &Cert, ca: &Cert, now: DateTime<Utc>) -> bool {
    if !verify_directly_issued_by(leaf, ca) {
        return false;
    }
    match (leaf.not_valid_before(), leaf.not_valid_after()) {
        (Some(start), Some(end)) => start <= now && now < end - Duration::days(RENEW_DAYS),
        _ => false,
    }
}

/// One name in a certificate's SubjectAlternativeName.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SanName {
    Dns(String),
    Ip(IpAddr),
    Other,
}

/// `_san`: the certificate's SubjectAlternativeName, in order, or None when it has none.
pub fn san(cert: Option<&Cert>) -> Option<Vec<SanName>> {
    let c = cert?.x509()?;
    let ext = c.subject_alternative_name().ok()??;
    Some(
        ext.value
            .general_names
            .iter()
            .map(|n| match n {
                GeneralName::DNSName(d) => SanName::Dns(d.to_string()),
                GeneralName::IPAddress(b) => match b.len() {
                    4 => SanName::Ip(IpAddr::from(<[u8; 4]>::try_from(*b).unwrap_or_default())),
                    16 => SanName::Ip(IpAddr::from(<[u8; 16]>::try_from(*b).unwrap_or_default())),
                    _ => SanName::Other,
                },
                _ => SanName::Other,
            })
            .collect(),
    )
}

/// The DNS names of a SAN, in order (`get_values_for_type(x509.DNSName)`).
pub fn san_dns(names: &[SanName]) -> Vec<String> {
    names
        .iter()
        .filter_map(|n| {
            if let SanName::Dns(d) = n {
                Some(d.clone())
            } else {
                None
            }
        })
        .collect()
}

/// The addresses of a SAN, in order, as text (`get_values_for_type(x509.IPAddress)`).
pub fn san_ips(names: &[SanName]) -> Vec<String> {
    names
        .iter()
        .filter_map(|n| {
            if let SanName::Ip(ip) = n {
                Some(ip.to_string())
            } else {
                None
            }
        })
        .collect()
}

fn ends_with_local(d: &str) -> bool {
    d.len() >= 6
        && d.is_char_boundary(d.len() - 6)
        && d[d.len() - 6..].eq_ignore_ascii_case(".local")
}

/// `_leaf_names`: the leaf's .local label (lower case) and its private addresses, newest first (127.0.0.1 left out).
pub fn leaf_names(leaf: Option<&Cert>) -> (String, Vec<String>) {
    let Some(names) = san(leaf) else {
        return (String::new(), Vec::new());
    };
    let host = san_dns(&names)
        .into_iter()
        .find(|d| ends_with_local(d))
        .map(|d| d[..d.len() - 6].to_lowercase())
        .unwrap_or_default();
    (host, san_addresses(san_ips(&names)))
}

/// Python's `x509.random_serial_number()`: 160 random bits shifted right by one, so 159 bits, positive.
fn random_serial() -> Result<SerialNumber> {
    let mut bytes = [0u8; 20];
    ring::rand::SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| anyhow!("no randomness for a serial number"))?;
    let mut carry = 0u8;
    for b in bytes.iter_mut() {
        let next = *b & 1;
        *b = (*b >> 1) | (carry << 7);
        carry = next;
    }
    let first = bytes
        .iter()
        .position(|b| *b != 0)
        .unwrap_or(bytes.len() - 1);
    Ok(SerialNumber::from_slice(&bytes[first..]))
}

/// `SubjectKeyIdentifier.from_public_key`: SHA-1 of the subjectPublicKey bits (RFC 5280 4.2.1.2, method 1).
fn key_id(key: &KeyPair) -> Vec<u8> {
    ring::digest::digest(
        &ring::digest::SHA1_FOR_LEGACY_USE_ONLY,
        key.public_key_raw(),
    )
    .as_ref()
    .to_vec()
}

/// `AuthorityKeyIdentifier.from_issuer_public_key` as its extension value: a keyIdentifier and nothing else.
fn authority_key_identifier(issuer: &KeyPair) -> CustomExtension {
    let id = key_id(issuer);
    let mut inner = vec![0x80];
    inner.extend(der_len(id.len()));
    inner.extend(id);
    let mut value = vec![0x30];
    value.extend(der_len(inner.len()));
    value.extend(inner);
    CustomExtension::from_oid_content(&[2, 5, 29, 35], value)
}

fn to_offset(t: DateTime<Utc>) -> Result<time::OffsetDateTime> {
    time::OffsetDateTime::from_unix_timestamp(t.timestamp())
        .context("a certificate date out of range")
}

fn common_name(name: &str) -> DistinguishedName {
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, DnValue::Utf8String(name.to_string()));
    dn
}

// ---- the server context -----------------------------------------------------------------------------------------

/// `_server_context`: TLS 1.2 or later, HTTP/1.1, the leaf and key in the given files, no client certificate.
/// Renegotiation is off by construction: rustls never renegotiates.
pub fn server_context(cert_path: &Path, key_path: &Path) -> Result<Arc<ServerConfig>> {
    let certs = CertificateDer::pem_file_iter(cert_path)
        .with_context(|| {
            format!(
                "the LAN certificate could not be read: {}",
                cert_path.display()
            )
        })?
        .collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| {
            format!(
                "the LAN certificate does not parse: {}",
                cert_path.display()
            )
        })?;
    if certs.is_empty() {
        bail!("no certificate in {}", cert_path.display());
    }
    let key = PrivateKeyDer::from_pem_file(key_path).with_context(|| {
        format!(
            "the LAN certificate's key does not load: {}",
            key_path.display()
        )
    })?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut cfg = ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])?
        .with_no_client_auth()
        .with_single_cert(certs, key)?;
    cfg.alpn_protocols = vec![b"http/1.1".to_vec()]; // the door speaks HTTP/1.1 only (LinkChat's writer)
    Ok(Arc::new(cfg))
}

// ---- LocalTls ---------------------------------------------------------------------------------------------------

struct State {
    ctx: Option<Arc<ServerConfig>>,
    names: HashSet<String>,
    /// The leaf file the context and the names came from.
    made_from: Option<(u64, i128)>,
    /// `time.monotonic()` at the last date check; 0.0 before the first, as in Python.
    checked: f64,
    clock: Clock,
    recheck_secs: f64,
}

/// The CA and the LAN door's certificate in `root` (`~/.config/eve-bridge/web-tls`). Every method makes whatever
/// is missing first, so the door always has a certificate to serve. Thread-safe, and two objects on one folder
/// agree: each notices a leaf the other issued.
pub struct LocalTls {
    pub root: PathBuf,
    pub ca_cert_path: PathBuf,
    pub ca_key_path: PathBuf,
    pub leaf_cert_path: PathBuf,
    pub leaf_key_path: PathBuf,
    lock: Arc<Mutex<()>>,
    state: Mutex<State>,
}

impl LocalTls {
    pub fn new(root: impl Into<PathBuf>) -> LocalTls {
        let root: PathBuf = root.into();
        LocalTls {
            ca_cert_path: root.join("ca.pem"),
            ca_key_path: root.join("ca.key"),
            leaf_cert_path: root.join("leaf.pem"),
            leaf_key_path: root.join("leaf.key"),
            lock: folder_lock(&root),
            root,
            state: Mutex::new(State {
                ctx: None,
                names: HashSet::new(),
                made_from: None,
                checked: 0.0,
                clock: Arc::new(now),
                recheck_secs: RECHECK_SECS,
            }),
        }
    }

    /// Stand this object at another date (the tests' `monkeypatch.setattr(lite_tls, "_now", ...)`).
    pub fn set_clock(&self, clock: impl Fn() -> DateTime<Utc> + Send + Sync + 'static) {
        self.state.lock().clock = Arc::new(clock);
    }

    /// How long `context()` waits before looking at the leaf's dates again (`RECHECK_SECS`).
    pub fn set_recheck_secs(&self, secs: f64) {
        self.state.lock().recheck_secs = secs;
    }

    // ---- the contract -----------------------------------------------------------------------------------------

    /// Make sure the CA and the leaf exist, are sound and name this Mac: `hostname` is its LocalHostName ("" when
    /// unknown: the leaf keeps the name it has), `addresses` its private IPv4 addresses, best first (anything else
    /// is ignored). True when a certificate was issued; an address set the leaf already holds, in any order, or one
    /// that lost an address, issues nothing.
    pub fn refresh<I>(&self, addresses: I, hostname: &str) -> Result<bool>
    where
        I: IntoIterator,
        I::Item: Into<Peer>,
    {
        let given: Vec<Peer> = addresses.into_iter().map(Into::into).collect();
        let _folder = self.lock.lock();
        let mut st = self.state.lock();
        self.refresh_locked(&mut st, &given, hostname)
    }

    /// The LAN door's server context, for the TLS accept after a plain TCP accept: TLS 1.2 or later, HTTP/1.1, the
    /// current leaf. The same object until the leaf changes; once a day it checks the leaf is not due for renewal.
    pub fn context(&self) -> Result<Arc<ServerConfig>> {
        let _folder = self.lock.lock();
        let mut st = self.state.lock();
        let due = monotonic() - st.checked > st.recheck_secs;
        let ctx = self.current(&mut st, due)?;
        if due {
            st.checked = monotonic();
        }
        Ok(ctx)
    }

    /// Every name the leaf holds: DNS names in lower case ("kitchen-mac.local", "localhost") and addresses
    /// ("127.0.0.1", "192.168.1.20"), for the LAN door's Host allowlist.
    pub fn names(&self) -> Result<HashSet<String>> {
        let _folder = self.lock.lock();
        let mut st = self.state.lock();
        self.current(&mut st, false)?;
        Ok(st.names.clone())
    }

    /// The CA certificate as PEM, for `ca.crt`.
    pub fn ca_pem(&self) -> Result<Vec<u8>> {
        Ok(self.ca()?.pem())
    }

    /// An unsigned configuration profile that installs the CA on an iPhone, iPad or Mac
    /// (`com.apple.security.root`). Its identifiers and UUIDs come from the CA's fingerprint: the same for every
    /// download of one CA, and different for each Mac, so two Macs' profiles never replace each other.
    pub fn mobileconfig(&self) -> Result<Vec<u8>> {
        let ca = self.ca()?;
        let der = ca.der().to_vec();
        let ident = format!("{PROFILE_ID}.{}", &hex::encode(Sha256::digest(&der))[..16]);
        let title = ca.common_name().unwrap_or_else(|| CA_NAME.to_string());
        let uuid5 = |name: &str| {
            uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, name.as_bytes())
                .to_string()
                .to_uppercase()
        };
        let root = Plist::Dict(vec![
            (
                "PayloadCertificateFileName".into(),
                Plist::Str("eve-lite-ca.cer".into()),
            ),
            ("PayloadContent".into(), Plist::Data(der)),
            (
                "PayloadDescription".into(),
                Plist::Str("Adds eve lite's local certificate authority.".into()),
            ),
            ("PayloadDisplayName".into(), Plist::Str(title.clone())),
            (
                "PayloadIdentifier".into(),
                Plist::Str(format!("{ident}.root")),
            ),
            (
                "PayloadType".into(),
                Plist::Str("com.apple.security.root".into()),
            ),
            (
                "PayloadUUID".into(),
                Plist::Str(uuid5(&format!("{ident}.root"))),
            ),
            ("PayloadVersion".into(), Plist::Int(1)),
        ]);
        let profile = Plist::Dict(vec![
            ("PayloadContent".into(), Plist::Array(vec![root])),
            ("PayloadDescription".into(), Plist::Str(PROFILE_TEXT.into())),
            ("PayloadDisplayName".into(), Plist::Str(title)),
            ("PayloadIdentifier".into(), Plist::Str(ident.clone())),
            ("PayloadOrganization".into(), Plist::Str(PROFILE_ORG.into())),
            ("PayloadRemovalDisallowed".into(), Plist::Bool(false)),
            ("PayloadType".into(), Plist::Str("Configuration".into())),
            ("PayloadUUID".into(), Plist::Str(uuid5(&ident))),
            ("PayloadVersion".into(), Plist::Int(1)),
        ]);
        plist_xml(&profile)
    }

    // ---- the files --------------------------------------------------------------------------------------------

    /// `_ca`: the CA certificate, made first if it is missing or unsound.
    fn ca(&self) -> Result<Cert> {
        let ca = {
            let _folder = self.lock.lock();
            let mut st = self.state.lock();
            self.refresh_locked(&mut st, &[], "")?;
            read_cert(&self.ca_cert_path)
        };
        // Only if the folder changed under the lock's feet.
        ca.ok_or_else(|| anyhow!("the local CA could not be read back"))
    }

    /// `_leaf_stamp`: the leaf file's inode and modification time, or None when it cannot be read.
    fn leaf_stamp(&self) -> Option<(u64, i128)> {
        let st = fs::metadata(&self.leaf_cert_path).ok()?;
        Some((
            st.ino(),
            i128::from(st.mtime()) * 1_000_000_000 + i128::from(st.mtime_nsec()),
        ))
    }

    /// `_current`: under the lock, the files sound (made, repaired or renewed as needed), and the context and the
    /// names made from the leaf on disk. Cheap when nothing changed: one stat.
    fn current(&self, st: &mut State, recheck: bool) -> Result<Arc<ServerConfig>> {
        if recheck || st.ctx.is_none() || self.leaf_stamp() != st.made_from {
            self.refresh_locked(st, &[], "")?;
            let stamp = self.leaf_stamp();
            if st.ctx.is_none() || stamp != st.made_from {
                let mut found: HashSet<String> = HashSet::new();
                if let Some(names) = san(read_cert(&self.leaf_cert_path).as_ref()) {
                    found.extend(san_dns(&names).iter().map(|d| d.to_lowercase()));
                    found.extend(san_ips(&names));
                }
                st.ctx = Some(server_context(&self.leaf_cert_path, &self.leaf_key_path)?);
                st.names = found;
                st.made_from = stamp;
            }
        }
        st.ctx
            .clone()
            .ok_or_else(|| anyhow!("the LAN context was not made"))
    }

    /// `_private_dir`: the folder 0700 and every file in it 0600, made or tightened.
    fn private_dir(&self) -> io::Result<()> {
        mkdir(&self.root, 0o700)?;
        if fs::metadata(&self.root)?.permissions().mode() & 0o7777 != 0o700 {
            fs::set_permissions(&self.root, fs::Permissions::from_mode(0o700))?;
        }
        for path in [
            &self.ca_cert_path,
            &self.ca_key_path,
            &self.leaf_cert_path,
            &self.leaf_key_path,
        ] {
            let mode = match fs::symlink_metadata(path) {
                Ok(m) => m.permissions().mode() & 0o7777,
                Err(e) if is_not_found(&e) => continue,
                Err(e) => return Err(e),
            };
            if mode != 0o600 && path.is_file() && !path.is_symlink() {
                fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
            }
        }
        Ok(())
    }

    /// `_refresh`, under the lock.
    fn refresh_locked(&self, st: &mut State, addresses: &[Peer], hostname: &str) -> Result<bool> {
        let now = (st.clock)();
        self.private_dir()?;
        let want = san_addresses(addresses.iter().cloned());
        let label = label(hostname);
        let ca = read_cert(&self.ca_cert_path);
        let ca_key = read_key(&self.ca_key_path);
        let (ca, ca_key) = match (ca, ca_key) {
            (Some(ca), Some(key)) if matches(Some(&ca), Some(&key)) && ca_usable(&ca, now) => {
                (ca, key)
            }
            (ca, _) => {
                let replaced = ca.is_some();
                let made = self.make_ca(&label, now)?;
                info!(
                    "lite tls: {} the local CA",
                    if replaced { "replaced" } else { "made" }
                );
                made
            }
        };
        let leaf = read_cert(&self.leaf_cert_path);
        let (have_host, have) = leaf_names(leaf.as_ref());
        let host = if label.is_empty() {
            have_host.clone()
        } else {
            label.to_lowercase()
        };
        let sound = leaf.as_ref().is_some_and(|l| {
            matches(Some(l), read_key(&self.leaf_key_path).as_ref()) && leaf_usable(l, &ca, now)
        });
        if sound && host == have_host && want.iter().all(|ip| have.contains(ip)) {
            return Ok(false);
        }
        let reason = if leaf.is_none() {
            "first"
        } else if sound {
            "new name"
        } else {
            "renewal or repair"
        };
        let mut ips = want.clone();
        ips.extend(have.into_iter().filter(|ip| !want.contains(ip)));
        ips.truncate(MAX_ADDRESSES);
        self.make_leaf(&ca, &ca_key, &host, &ips, now)?;
        st.ctx = None; // current() makes the context and the names again
        info!(
            "lite tls: issued the LAN certificate ({}, {} addresses)",
            reason,
            ips.len()
        );
        Ok(true)
    }

    /// `_make_ca`: a fresh CA key and certificate, written key first.
    fn make_ca(&self, label: &str, now: DateTime<Utc>) -> Result<(Cert, KeyPair)> {
        let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256)?;
        let room = 64 - CA_NAME.len() - 3; // a common name holds at most 64 characters
        let name = if label.is_empty() {
            CA_NAME.to_string()
        } else {
            let cut: String = label.chars().take(room).collect();
            format!("{CA_NAME} ({cut})")
        };
        let start = now - backdate();
        let mut params = CertificateParams::default();
        params.distinguished_name = common_name(&name);
        params.serial_number = Some(random_serial()?);
        params.not_before = to_offset(start)?;
        params.not_after = to_offset(start + Duration::days(CA_DAYS))?;
        params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        params.name_constraints = Some(NameConstraints {
            permitted_subtrees: permitted_subtrees(),
            excluded_subtrees: Vec::new(),
        });
        // The subject key identifier, and the authority key identifier from it (self-signed).
        params.key_identifier_method = KeyIdMethod::PreSpecified(key_id(&key));
        params.use_authority_key_identifier_extension = true;
        let cert = params.self_signed(&key)?;
        // The key first: a crash between the two leaves a pair that does not match, which is made again.
        write_private(&self.ca_key_path, key.serialize_pem().as_bytes())?;
        write_private(&self.ca_cert_path, cert.pem().as_bytes())?;
        let cert = Cert::from_der(cert.der().to_vec())
            .ok_or_else(|| anyhow!("the new CA does not parse"))?;
        Ok((cert, key))
    }

    /// `_make_leaf`: a fresh leaf key and certificate for `host`.local, localhost, 127.0.0.1 and `ips`, signed by
    /// the CA.
    fn make_leaf(
        &self,
        ca: &Cert,
        ca_key: &KeyPair,
        host: &str,
        ips: &[String],
        now: DateTime<Utc>,
    ) -> Result<()> {
        let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256)?;
        let mut names: Vec<SanType> = Vec::new();
        if !host.is_empty() {
            names.push(SanType::DnsName(format!("{host}.local").try_into()?));
        }
        names.push(SanType::DnsName("localhost".try_into()?));
        names.push(SanType::IpAddress(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))));
        for ip in ips {
            names.push(SanType::IpAddress(IpAddr::V4(ip.parse::<Ipv4Addr>()?)));
        }
        let start = now - backdate();
        let ca_end = ca
            .not_valid_after()
            .ok_or_else(|| anyhow!("the CA has no end date"))?;
        let mut params = CertificateParams::default();
        params.distinguished_name = common_name(LEAF_NAME);
        params.serial_number = Some(random_serial()?);
        params.not_before = to_offset(start)?;
        params.not_after = to_offset((start + Duration::days(LEAF_DAYS)).min(ca_end))?;
        params.is_ca = IsCa::ExplicitNoCa;
        params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        params.subject_alt_names = names;
        params.key_identifier_method = KeyIdMethod::PreSpecified(key_id(&key));
        params.use_authority_key_identifier_extension = false;
        params.custom_extensions = vec![authority_key_identifier(ca_key)];
        let issuer = Issuer::from_ca_cert_der(&CertificateDer::from(ca.der()), ca_key)?;
        let cert = params.signed_by(&key, &issuer)?;
        write_private(&self.leaf_key_path, key.serialize_pem().as_bytes())?;
        write_private(&self.leaf_cert_path, cert.pem().as_bytes())?;
        Ok(())
    }
}

/// `Path.mkdir(mode, parents=True, exist_ok=True)`: missing parents made with the default mode, the folder itself
/// with `mode` (under the umask), and an existing folder left as it is.
fn mkdir(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt as _;
    match fs::DirBuilder::new().mode(mode).create(path) {
        Ok(()) => Ok(()),
        Err(e) if is_not_found(&e) => {
            let parent = path
                .parent()
                .filter(|p| *p != path && !p.as_os_str().is_empty());
            match parent {
                Some(parent) => {
                    mkdir(parent, 0o777)?;
                    match fs::DirBuilder::new().mode(mode).create(path) {
                        Err(e) if !path.is_dir() => Err(e),
                        _ => Ok(()),
                    }
                }
                None => Err(e),
            }
        }
        Err(e) => {
            if path.is_dir() {
                Ok(())
            } else {
                Err(e)
            }
        }
    }
}

// ---- the profile ------------------------------------------------------------------------------------------------

/// A property list value, for [`plist_xml`].
#[derive(Clone, Debug, PartialEq)]
pub enum Plist {
    Str(String),
    Int(i64),
    Bool(bool),
    Data(Vec<u8>),
    Array(Vec<Plist>),
    Dict(Vec<(String, Plist)>),
}

const PLIST_HEADER: &str =
    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST \
1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n";

/// `plistlib.dumps(value, fmt=plistlib.FMT_XML)`, byte for byte: tab indents, sorted keys, base64 data wrapped to
/// the width plistlib picks for its depth.
pub fn plist_xml(value: &Plist) -> Result<Vec<u8>> {
    let mut w = PlistWriter {
        out: String::from(PLIST_HEADER),
        level: 0,
    };
    w.writeln("<plist version=\"1.0\">");
    w.value(value)?;
    w.writeln("</plist>");
    Ok(w.out.into_bytes())
}

struct PlistWriter {
    out: String,
    level: usize,
}

impl PlistWriter {
    fn writeln(&mut self, line: &str) {
        if !line.is_empty() {
            self.out.push_str(&"\t".repeat(self.level));
            self.out.push_str(line);
        }
        self.out.push('\n');
    }

    fn begin(&mut self, element: &str) {
        self.writeln(&format!("<{element}>"));
        self.level += 1;
    }

    fn end(&mut self, element: &str) {
        self.level -= 1;
        self.writeln(&format!("</{element}>"));
    }

    fn simple(&mut self, element: &str, value: Option<&str>) -> Result<()> {
        match value {
            Some(v) => {
                let v = escape(v)?;
                self.writeln(&format!("<{element}>{v}</{element}>"));
            }
            None => self.writeln(&format!("<{element}/>")),
        }
        Ok(())
    }

    fn value(&mut self, value: &Plist) -> Result<()> {
        match value {
            Plist::Str(s) => self.simple("string", Some(s))?,
            Plist::Bool(true) => self.simple("true", None)?,
            Plist::Bool(false) => self.simple("false", None)?,
            Plist::Int(i) => self.simple("integer", Some(&i.to_string()))?,
            Plist::Data(d) => {
                self.begin("data");
                self.level -= 1;
                let max_line = 16usize.max(76usize.saturating_sub(8 * self.level));
                let max_bin = (max_line / 4) * 3;
                for chunk in d.chunks(max_bin.max(1)) {
                    let line = base64::engine::general_purpose::STANDARD.encode(chunk);
                    self.writeln(&line);
                }
                self.level += 1;
                self.end("data");
            }
            Plist::Array(items) => {
                if items.is_empty() {
                    self.simple("array", None)?;
                } else {
                    self.begin("array");
                    for item in items {
                        self.value(item)?;
                    }
                    self.end("array");
                }
            }
            Plist::Dict(items) => {
                if items.is_empty() {
                    self.simple("dict", None)?;
                } else {
                    self.begin("dict");
                    let mut sorted: Vec<&(String, Plist)> = items.iter().collect();
                    sorted.sort_by(|a, b| a.0.cmp(&b.0));
                    for (key, item) in sorted {
                        self.simple("key", Some(key))?;
                        self.value(item)?;
                    }
                    self.end("dict");
                }
            }
        }
        Ok(())
    }
}

/// plistlib's `_escape`: control characters refused, line ends made `\n`, and `&`, `<`, `>` escaped.
fn escape(text: &str) -> Result<String> {
    if text
        .chars()
        .any(|c| matches!(c, '\x00'..='\x08' | '\x0b' | '\x0c' | '\x0e'..='\x1f'))
    {
        bail!("strings can't contains control characters; use bytes instead");
    }
    Ok(text
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;"))
}

#[cfg(test)]
mod tests;
