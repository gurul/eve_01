//! eve's vault: sign-ins, cards and 2FA secrets that no model ever sees. Port of `eve_bridge/vault.py`.
//!
//! The owner, 2026-10-12, after comparing eve with OpenInstinct (Merit-Systems, MIT): its browser agent asks
//! for a vault item by an opaque handle and code fills the page, so a password never enters a model's context.
//! eve does the same. A model sees an item only as a line of this module's [`Item::line`]: a handle, a kind, a
//! label and a site (`v_3k9f2a7q login "Chase" for chase.com`). It names the handle in a plan step
//! (browser_lane, plan_contract's `fill`) or a tool call (vault_mcp, for Codex tasks), and this module hands
//! the values to the code that fills the field, never back to the model.
//!
//! ```text
//!     eve-bridge vault add login --site chase.com --label Chase      values read at the terminal (getpass)
//!           │
//!           ▼
//!     macOS Keychain, service "eve-vault", account = handle          the values, as hex JSON
//!     ~/.config/eve-bridge/vault.json (0600)                         handle, kind, label, site: no value
//!           │
//!           ▼
//!     Vault::fill_values(handle, page_url)   a login or a 2FA code only on its own site; a card anywhere
//! ```
//!
//! Kinds: `login` (username, password), `card` (number, expiry, CVC, name on card) and `totp` (a 2FA secret:
//! the code is generated here, RFC 6238, when a field needs it). Logins and 2FA codes are bound to the site they
//! were saved for: the page's host must be that host or one of its subdomains, with the same scheme and port, so
//! `chase.com` fills on `secure.chase.com` and never on `chase.com.example` or `example.com`. Cards are
//! not bound to a site and fill without a question (the owner: "i dont want to be limited"); the sensitive-label
//! gate still stops at Pay, Place order and the like, so a purchase still needs the owner's yes.
//!
//! The Keychain is driven through `/usr/bin/security`. A secret never appears on a command line, where any
//! process could read it from the process table: `security -i` reads the add command on stdin, and the value
//! goes as hex so no quoting can break it. Reading uses `find-generic-password -w`, whose output is the value.
//! Items `security` creates trust `security` itself, so reading them asks no question; the cost is that any
//! program running as the owner can read them the same way, which is the same exposure as eve's 0600 env file.
//! `EVE_VAULT_KEYCHAIN` names another keychain file (the live test uses a throwaway one, never the login
//! keychain); `EVE_VAULT_INDEX` moves the index.

pub mod cli;
pub mod pystr;
pub mod pyurl;

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::LazyLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use hmac::{Hmac, Mac};
use indexmap::IndexMap;
use parking_lot::Mutex;
use rand::Rng;
use regex::Regex;
use serde_json::{Map, Value};

pub use cli::{cli, cli_main, getpass, input};

pub const KINDS: [&str; 3] = ["login", "card", "totp"];
pub const SERVICE: &str = "eve-vault";
pub const SECURITY: &str = "/usr/bin/security";
pub const MAX_LABEL_CHARS: usize = 60;
/// The hash names a 2FA secret may use (Python's `TOTP_ALGORITHMS` keys).
pub const TOTP_ALGORITHMS: [&str; 3] = ["SHA1", "SHA256", "SHA512"];
/// The parts a fill can ask for, per kind. A login's username is not secret, but it travels with the
/// password so a model never has to type either.
pub const PARTS: [(&str, &[&str]); 3] = [
    ("login", &["username", "password"]),
    ("card", &["number", "exp", "exp_month", "exp_year", "cvc", "name"]),
    ("totp", &["code"]),
];
/// Every part of every kind, once each, in order.
pub const ALL_PARTS: [&str; 9] =
    ["username", "password", "number", "exp", "exp_month", "exp_year", "cvc", "name", "code"];

/// `^v_[a-z0-9]{8}$` with Python's `re.match` semantics: `$` also matches before one trailing newline.
pub static HANDLE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^v_[a-z0-9]{8}\n?\z").expect("handle regex"));

/// The parts of `kind` (empty for an unknown kind).
pub fn parts(kind: &str) -> &'static [&'static str] {
    PARTS.iter().find(|(k, _)| *k == kind).map_or(&[], |(_, p)| p)
}

/// `Vault.lines`' default limit.
pub const LINES_LIMIT: usize = 40;

/// A free-form JSON object, as Python's `dict[str, Any]` value of an item.
pub type Values = Map<String, Value>;

/// Python's `VaultError` and its subclass `Refused`. The message is one sentence for the owner or the
/// model; it never holds a value.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VaultError {
    #[error("{0}")]
    Error(String),
    /// The fill was refused (another site, an unknown handle, a part the item lacks). Nothing was filled.
    #[error("{0}")]
    Refused(String),
    /// What Python raises as an OSError or a subprocess timeout (the index could not be written, `security`
    /// could not be run). Python lets those escape; here they travel as this variant.
    #[error("{0}")]
    Os(String),
}

impl VaultError {
    pub fn is_refused(&self) -> bool {
        matches!(self, VaultError::Refused(_))
    }

    pub fn message(&self) -> &str {
        match self {
            VaultError::Error(m) | VaultError::Refused(m) | VaultError::Os(m) => m,
        }
    }
}

fn err(msg: impl Into<String>) -> VaultError {
    VaultError::Error(msg.into())
}

fn refused(msg: impl Into<String>) -> VaultError {
    VaultError::Refused(msg.into())
}

pub type Result<T> = std::result::Result<T, VaultError>;

/// `~/.config/eve-bridge/vault.json`, or `EVE_VAULT_INDEX` (with `~` expanded) when it is set.
pub fn default_index() -> PathBuf {
    default_index_from(std::env::var("EVE_VAULT_INDEX").ok().as_deref())
}

/// [`default_index`] for an explicit value of `EVE_VAULT_INDEX`.
pub fn default_index_from(configured: Option<&str>) -> PathBuf {
    let configured = pystr::strip(configured.unwrap_or(""));
    if configured.is_empty() {
        home().join(".config").join("eve-bridge").join("vault.json")
    } else {
        expanduser(configured)
    }
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).or_else(dirs::home_dir).unwrap_or_else(|| PathBuf::from("/"))
}

/// `Path.expanduser()`: a leading `~` or `~/` becomes the home directory (`~user` is left as it is unless
/// it names the current user).
fn expanduser(p: &str) -> PathBuf {
    if p == "~" {
        return home();
    }
    if let Some(rest) = p.strip_prefix("~/") {
        return home().join(rest);
    }
    PathBuf::from(p)
}

// ---- sites -------------------------------------------------------------------------------------------------

fn default_port(scheme: &str) -> Option<u16> {
    match scheme {
        "http" => Some(80),
        "https" => Some(443),
        _ => None,
    }
}

/// `chase.com`, `https://chase.com/login` → `https://chase.com`; a port only when it is not the
/// scheme's own. VaultError for anything that is not a plain http(s) site.
pub fn canonical_origin(site: &str) -> Result<String> {
    let raw = pystr::strip(site);
    if raw.is_empty() {
        return Err(err("a login or 2FA item needs the site it is for"));
    }
    let raw = if raw.contains("://") { raw.to_string() } else { format!("https://{raw}") };
    let not_a_site = || err(format!("not a site: {}", pystr::repr(site)));
    let u = pyurl::urlsplit(&raw).map_err(|_| not_a_site())?;
    let port = u.port().map_err(|_| not_a_site())?;
    let scheme = u.scheme.to_lowercase();
    let host = u.hostname().unwrap_or_default().to_lowercase();
    let host = host.trim_end_matches('.');
    let Some(own) = default_port(&scheme) else {
        return Err(not_a_site());
    };
    if host.is_empty() || u.has_credentials() || raw.chars().any(pystr::is_space) {
        return Err(not_a_site());
    }
    Ok(match port {
        Some(p) if p != 0 && p != own => format!("{scheme}://{host}:{p}"),
        _ => format!("{scheme}://{host}"),
    })
}

/// True when `page_url` is on the saved site: the same scheme and port, and the saved host or a
/// subdomain of it. Anything unparsable is false.
pub fn site_allows(saved: &str, page_url: &str) -> bool {
    let Ok(origin) = canonical_origin(saved) else {
        return false;
    };
    let (Ok(s), Ok(p)) = (pyurl::urlsplit(&origin), pyurl::urlsplit(page_url)) else {
        return false;
    };
    let (Ok(s_port), Ok(p_port)) = (s.port(), p.port()) else {
        return false;
    };
    let p_scheme = p.scheme.to_lowercase();
    if p_scheme != s.scheme || p.has_credentials() {
        return false;
    }
    // `p_port or default`: a port of 0 is falsy in Python and falls back to the scheme's own.
    let truthy = |port: Option<u16>| port.filter(|&n| n != 0);
    if truthy(p_port).or(default_port(&p_scheme)) != truthy(s_port).or(default_port(&s.scheme)) {
        return false;
    }
    let host = p.hostname().unwrap_or_default().to_lowercase();
    let host = host.trim_end_matches('.');
    let want = s.hostname().unwrap_or_default();
    !host.is_empty() && (host == want || host.ends_with(&format!(".{want}")))
}

/// What a model is shown of a site: the host, and the port when there is one.
pub fn site_of(origin: &str) -> String {
    let Ok(u) = pyurl::urlsplit(origin) else {
        return String::new();
    };
    let host = u.hostname().unwrap_or_default();
    match u.port() {
        Ok(Some(p)) if p != 0 => format!("{host}:{p}"),
        _ => host,
    }
}

// ---- 2FA codes (RFC 4226 / RFC 6238) ------------------------------------------------------------------------

fn hmac_digest(algorithm: &str, key: &[u8], msg: &[u8]) -> Result<Vec<u8>> {
    fn run<M: Mac + hmac::digest::KeyInit>(key: &[u8], msg: &[u8]) -> Vec<u8> {
        let mut m = <M as hmac::digest::KeyInit>::new_from_slice(key).expect("HMAC takes a key of any length");
        m.update(msg);
        m.finalize().into_bytes().to_vec()
    }
    Ok(match algorithm {
        "SHA1" => run::<Hmac<sha1::Sha1>>(key, msg),
        "SHA256" => run::<Hmac<sha2::Sha256>>(key, msg),
        "SHA512" => run::<Hmac<sha2::Sha512>>(key, msg),
        other => return Err(err(format!("unsupported 2FA algorithm {}", pystr::repr(other)))),
    })
}

/// The RFC 4226 code for `key` at `counter`.
pub fn hotp(key: &[u8], counter: u64, digits: u32, algorithm: &str) -> Result<String> {
    let mac = hmac_digest(algorithm, key, &counter.to_be_bytes())?;
    let offset = (mac[mac.len() - 1] & 0x0F) as usize;
    let word = u32::from_be_bytes([mac[offset], mac[offset + 1], mac[offset + 2], mac[offset + 3]]) & 0x7FFF_FFFF;
    let code = match 10u64.checked_pow(digits) {
        Some(m) => u64::from(word) % m,
        None => u64::from(word),
    };
    Ok(format!("{:0>width$}", code, width = digits as usize))
}

static B32_SECRET: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Z2-7]+=*$").expect("b32 regex"));

/// `re.sub(r"[\s-]", "", text).upper()`.
fn b32_clean(text: &str) -> String {
    text.chars().filter(|&c| !(pystr::is_space(c) || c == '-')).collect::<String>().to_uppercase()
}

/// A base32 2FA secret as bytes: spaces and dashes dropped, any case, padding optional.
pub fn b32_key(secret: &str) -> Result<Vec<u8>> {
    let clean = b32_clean(secret);
    if clean.is_empty() || !B32_SECRET.is_match(&clean) {
        return Err(err("a 2FA secret is base32 (letters A-Z and digits 2-7)"));
    }
    let padded = format!("{clean}{}", "=".repeat((8 - clean.len() % 8) % 8));
    b32decode(&padded).ok_or_else(|| err("a 2FA secret is base32 (letters A-Z and digits 2-7)"))
}

/// Python's `base64.b32decode` for an upper-case, already padded string: None on bad padding.
fn b32decode(s: &str) -> Option<Vec<u8>> {
    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    if !s.len().is_multiple_of(8) {
        return None;
    }
    let body = s.trim_end_matches('=');
    let padchars = s.len() - body.len();
    let mut decoded: Vec<u8> = Vec::new();
    let mut acc: u64 = 0;
    for quantum in body.as_bytes().chunks(8) {
        acc = 0;
        for &c in quantum {
            let v = ALPHABET.iter().position(|&a| a == c)? as u64;
            acc = (acc << 5) + v;
        }
        // A short last quantum is still written as five bytes, the high ones zero; the padding fix below
        // keeps only the bytes it encodes.
        decoded.extend_from_slice(&acc.to_be_bytes()[3..8]);
    }
    if ![0, 1, 3, 4, 6].contains(&padchars) {
        return None;
    }
    if padchars > 0 && !decoded.is_empty() {
        acc <<= 5 * padchars as u64;
        let last = acc.to_be_bytes();
        let leftover = (43 - 5 * padchars) / 8;
        let n = decoded.len();
        decoded.truncate(n - 5);
        decoded.extend_from_slice(&last[3..3 + leftover]);
    }
    Some(decoded)
}

/// Python's `float // int` (floor division as CPython computes it for floats).
fn floor_div(vx: f64, wx: f64) -> f64 {
    let m = vx % wx;
    let mut div = (vx - m) / wx;
    if m != 0.0 && ((wx < 0.0) != (m < 0.0)) {
        div -= 1.0;
    }
    if div != 0.0 {
        let mut fd = div.floor();
        if div - fd > 0.5 {
            fd += 1.0;
        }
        fd
    } else {
        0.0f64.copysign(vx / wx)
    }
}

/// The current time as Python's `time.time()`.
pub fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

/// The code for `secret` (base32) at `at` (now by default).
pub fn totp(secret: &str, at: Option<f64>, digits: u32, period: u64, algorithm: &str) -> Result<String> {
    let key = b32_key(secret)?;
    if period == 0 {
        return Err(err("a 2FA period of 0 seconds"));
    }
    let now = at.unwrap_or_else(now);
    let counter = floor_div(now, period as f64);
    if !counter.is_finite() || counter < 0.0 {
        return Err(err("a 2FA code needs a time after 1970"));
    }
    hotp(&key, counter as u64, digits, algorithm)
}

/// [`totp`] with Python's defaults: 6 digits, 30 seconds, SHA1.
pub fn totp_default(secret: &str, at: Option<f64>) -> Result<String> {
    totp(secret, at, 6, 30, "SHA1")
}

/// A 2FA secret as the owner pastes it: an `otpauth://totp/...` URI (from a QR code) or the bare base32
/// secret. The stored form: {secret, digits, period, algorithm} (keys in Python's order: digits, period,
/// algorithm, secret).
pub fn parse_totp(text: &str) -> Result<Values> {
    let mut raw = pystr::strip(text).to_string();
    let mut digits: i128 = 6;
    let mut period: i128 = 30;
    let mut algorithm = "SHA1".to_string();
    if raw.to_lowercase().starts_with("otpauth://") {
        let u = pyurl::urlsplit(&raw).map_err(|e| err(e.0))?;
        if u.netloc.to_lowercase() != "totp" {
            return Err(err("only time-based (totp) codes are supported"));
        }
        let mut q: HashMap<String, String> = HashMap::new();
        for (k, v) in pyurl::parse_qs(&u.query) {
            q.insert(k.to_lowercase(), v.into_iter().next().unwrap_or_default());
        }
        raw = pyurl::unquote(q.get("secret").map_or("", String::as_str));
        let not_a_number = || err("the otpauth URI's digits or period is not a number");
        if let Some(d) = q.get("digits") {
            digits = pystr::int_of_str(d).ok_or_else(not_a_number)?;
        }
        if let Some(p) = q.get("period") {
            period = pystr::int_of_str(p).ok_or_else(not_a_number)?;
        }
        algorithm = q.get("algorithm").map_or("SHA1", String::as_str).to_uppercase();
    }
    if !TOTP_ALGORITHMS.contains(&algorithm.as_str()) || !(6..=8).contains(&digits) || !(10..=120).contains(&period) {
        return Err(err("unsupported 2FA settings (digits 6-8, period 10-120 s, SHA1/SHA256/SHA512)"));
    }
    b32_key(&raw)?; // refused when it is not base32
    let mut out = Values::new();
    out.insert("digits".into(), Value::from(digits as i64));
    out.insert("period".into(), Value::from(period as i64));
    out.insert("algorithm".into(), Value::from(algorithm));
    out.insert("secret".into(), Value::from(b32_clean(&raw)));
    Ok(out)
}

// ---- the Keychain ---------------------------------------------------------------------------------------------

/// What a finished `security` run left: Python's `CompletedProcess` fields the vault reads.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunOutput {
    pub returncode: i32,
    pub stdout: String,
    pub stderr: String,
}

/// The seam Python's `run=subprocess.run` parameter is: argv, optional stdin text, a timeout.
pub trait Runner: Send + Sync {
    fn run(&self, argv: &[String], input: Option<&str>, timeout: Duration) -> io::Result<RunOutput>;
}

/// `subprocess.run(argv, input=..., capture_output=True, text=True, timeout=...)`.
#[derive(Debug, Clone, Copy, Default)]
pub struct SubprocessRunner;

impl Runner for SubprocessRunner {
    fn run(&self, argv: &[String], input: Option<&str>, timeout: Duration) -> io::Result<RunOutput> {
        let (program, args) = argv.split_first().ok_or_else(|| io::Error::other("empty argv"))?;
        let mut child = Command::new(program)
            .args(args)
            .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let writer = match (input, child.stdin.take()) {
            (Some(text), Some(mut stdin)) => {
                let text = text.to_string();
                Some(std::thread::spawn(move || {
                    let _ = stdin.write_all(text.as_bytes());
                }))
            }
            _ => None,
        };
        let mut stdout = child.stdout.take();
        let mut stderr = child.stderr.take();
        let out_reader = std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(s) = stdout.as_mut() {
                let _ = s.read_to_end(&mut buf);
            }
            buf
        });
        let err_reader = std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(s) = stderr.as_mut() {
                let _ = s.read_to_end(&mut buf);
            }
            buf
        });
        let started = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if started.elapsed() >= timeout {
                let _ = child.kill();
                let _ = child.wait();
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("Command {:?} timed out after {} seconds", argv, timeout.as_secs_f64()),
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        if let Some(w) = writer {
            let _ = w.join();
        }
        let stdout = out_reader.join().unwrap_or_default();
        let stderr = err_reader.join().unwrap_or_default();
        Ok(RunOutput {
            returncode: status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
        })
    }
}

/// Where an item's values live. Python's tests subclass `Keychain` to keep them in a dict; this trait is that
/// seam, and [`Keychain`] (the real one) and [`MemKeychain`] implement it.
pub trait KeychainStore: Send + Sync {
    fn put(&self, handle: &str, value: &Values) -> Result<()>;
    fn get(&self, handle: &str) -> Result<Option<Values>>;
    fn delete(&self, handle: &str) -> Result<bool>;
}

const SECURITY_TIMEOUT: Duration = Duration::from_secs(20);

/// Generic-password items under SERVICE, one per handle; the value is hex of a JSON object.
pub struct Keychain {
    pub keychain: String,
    run: Box<dyn Runner>,
}

impl std::fmt::Debug for Keychain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Keychain").field("keychain", &self.keychain).finish()
    }
}

impl Keychain {
    /// `Keychain(keychain, run)`: an empty `keychain` falls back to `EVE_VAULT_KEYCHAIN`.
    pub fn new(keychain: &str, run: Box<dyn Runner>) -> Result<Self> {
        let env = std::env::var("EVE_VAULT_KEYCHAIN").ok();
        Self::with_env(keychain, env.as_deref(), run)
    }

    /// [`Keychain::new`] with an explicit value of `EVE_VAULT_KEYCHAIN`.
    pub fn with_env(keychain: &str, env: Option<&str>, run: Box<dyn Runner>) -> Result<Self> {
        let keychain =
            if !keychain.is_empty() { keychain.to_string() } else { pystr::strip(env.unwrap_or("")).to_string() };
        if !keychain.is_empty() && keychain.chars().any(|c| pystr::is_space(c) || matches!(c, '"' | '\'' | '\\')) {
            return Err(err("EVE_VAULT_KEYCHAIN must be a path without spaces or quotes"));
        }
        Ok(Self { keychain, run })
    }

    /// `Keychain()`: `/usr/bin/security` on the keychain `EVE_VAULT_KEYCHAIN` names, or the default list.
    pub fn from_env() -> Result<Self> {
        Self::new("", Box::new(SubprocessRunner))
    }

    fn tail(&self) -> Vec<String> {
        if self.keychain.is_empty() {
            vec![]
        } else {
            vec![self.keychain.clone()]
        }
    }

    fn run(&self, argv: Vec<String>, input: Option<&str>) -> Result<RunOutput> {
        self.run.run(&argv, input, SECURITY_TIMEOUT).map_err(|e| VaultError::Os(e.to_string()))
    }
}

fn to_args(words: &[&str]) -> Vec<String> {
    words.iter().map(|s| s.to_string()).collect()
}

/// `json.dumps(value, separators=(",", ":"))` (ASCII-escaped, as Python writes it).
pub fn compact_json(value: &Values) -> String {
    pystr::ensure_ascii(&serde_json::to_string(value).unwrap_or_else(|_| "{}".into()))
}

/// `bytes.fromhex(text)`: pairs of hex digits, ASCII whitespace allowed between pairs.
fn fromhex(text: &str) -> Option<Vec<u8>> {
    let b = text.as_bytes();
    let mut out = Vec::with_capacity(b.len() / 2);
    let mut i = 0;
    while i < b.len() {
        if b[i].is_ascii_whitespace() || b[i] == 0x0b {
            i += 1;
            continue;
        }
        if i + 1 >= b.len() || !b[i].is_ascii_hexdigit() || !b[i + 1].is_ascii_hexdigit() {
            return None;
        }
        let nibble = |c: u8| (c as char).to_digit(16).unwrap_or(0) as u8;
        out.push(nibble(b[i]) << 4 | nibble(b[i + 1]));
        i += 2;
    }
    Some(out)
}

impl KeychainStore for Keychain {
    fn put(&self, handle: &str, value: &Values) -> Result<()> {
        if !HANDLE.is_match(handle) {
            return Err(err("bad handle"));
        }
        let blob = hex::encode(compact_json(value).as_bytes());
        // `security -i` reads its command from stdin: the value is never on a command line.
        let mut words = to_args(&["add-generic-password", "-U", "-s", SERVICE, "-a", handle, "-l", SERVICE, "-w"]);
        words.push(blob);
        words.extend(self.tail());
        let line = words.join(" ") + "\n";
        let r = self.run(to_args(&[SECURITY, "-i"]), Some(&line))?;
        if r.returncode != 0 || r.stderr.to_lowercase().contains("error") {
            return Err(err("the Keychain refused the item"));
        }
        if self.get(handle)?.as_ref() != Some(value) {
            return Err(err("the Keychain did not keep the item"));
        }
        Ok(())
    }

    fn get(&self, handle: &str) -> Result<Option<Values>> {
        let mut argv = to_args(&[SECURITY, "find-generic-password", "-s", SERVICE, "-a", handle, "-w"]);
        argv.extend(self.tail());
        let r = self.run(argv, None)?;
        if r.returncode != 0 {
            return Ok(None);
        }
        let Some(bytes) = fromhex(pystr::strip(&r.stdout)) else {
            return Ok(None);
        };
        let Ok(text) = String::from_utf8(bytes) else {
            return Ok(None);
        };
        match serde_json::from_str::<Value>(&text) {
            Ok(Value::Object(map)) => Ok(Some(map)),
            _ => Ok(None),
        }
    }

    fn delete(&self, handle: &str) -> Result<bool> {
        let mut argv = to_args(&[SECURITY, "delete-generic-password", "-s", SERVICE, "-a", handle]);
        argv.extend(self.tail());
        Ok(self.run(argv, None)?.returncode == 0)
    }
}

/// The Keychain over an in-memory map: the vault's own logic with no `/usr/bin/security`. Python's tests
/// define this as a `Keychain` subclass (`MemKeychain` in test_vault_fill.py and test_vault_mcp.py); the
/// crates that port those tests use this one.
#[derive(Debug, Default)]
pub struct MemKeychain {
    pub items: Mutex<IndexMap<String, Values>>,
}

impl MemKeychain {
    pub fn new() -> Self {
        Self::default()
    }
}

impl KeychainStore for MemKeychain {
    fn put(&self, handle: &str, value: &Values) -> Result<()> {
        self.items.lock().insert(handle.to_string(), value.clone());
        Ok(())
    }

    fn get(&self, handle: &str) -> Result<Option<Values>> {
        Ok(self.items.lock().get(handle).cloned())
    }

    fn delete(&self, handle: &str) -> Result<bool> {
        Ok(self.items.lock().shift_remove(handle).is_some())
    }
}

impl<T: KeychainStore + ?Sized> KeychainStore for std::sync::Arc<T> {
    fn put(&self, handle: &str, value: &Values) -> Result<()> {
        (**self).put(handle, value)
    }
    fn get(&self, handle: &str) -> Result<Option<Values>> {
        (**self).get(handle)
    }
    fn delete(&self, handle: &str) -> Result<bool> {
        (**self).delete(handle)
    }
}

// ---- the vault ------------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Item {
    pub handle: String,
    pub kind: String,
    pub label: String,
    /// login / totp: the site it fills on; card: "".
    pub origin: String,
}

impl Item {
    pub fn new(handle: &str, kind: &str, label: &str, origin: &str) -> Self {
        Self { handle: handle.into(), kind: kind.into(), label: label.into(), origin: origin.into() }
    }

    /// All a model is ever told of an item.
    pub fn line(&self) -> String {
        let place = if self.origin.is_empty() { String::new() } else { format!(" for {}", site_of(&self.origin)) };
        format!("{} {} \"{}\"{}", self.handle, self.kind, self.label, place)
    }

    pub fn to_dict(&self) -> Values {
        let mut m = Values::new();
        m.insert("handle".into(), Value::from(self.handle.clone()));
        m.insert("kind".into(), Value::from(self.kind.clone()));
        m.insert("label".into(), Value::from(self.label.clone()));
        m.insert("origin".into(), Value::from(self.origin.clone()));
        m
    }
}

fn clean_label(label: &str) -> Result<String> {
    let text = pystr::head(&pystr::squash(&label.replace('"', "'")), MAX_LABEL_CHARS);
    if text.is_empty() {
        return Err(err("an item needs a label"));
    }
    Ok(text)
}

static CARD_NUMBER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d{12,19}$").expect("card regex"));
static CVC: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d{3,4}$").expect("cvc regex"));

/// `str.isdigit()` and `int()` of it, for the expiry fields.
fn digits_value(s: &str) -> Option<i128> {
    if s.is_empty() || !s.chars().all(|c| pystr::int_of_str(&c.to_string()).is_some()) {
        return None;
    }
    pystr::int_of_str(s)
}

fn card(value: &Values) -> Result<Values> {
    let number: String =
        pystr::str_or_empty(value.get("number")).chars().filter(|&c| !(pystr::is_space(c) || c == '-')).collect();
    if !CARD_NUMBER.is_match(&number) {
        return Err(err("a card number is 12 to 19 digits"));
    }
    let month = pystr::strip(&pystr::str_or_empty(value.get("exp_month"))).to_string();
    let year = pystr::strip(&pystr::str_or_empty(value.get("exp_year"))).to_string();
    let year_len = year.chars().count();
    let month_ok = digits_value(&month).is_some_and(|m| (1..=12).contains(&m));
    if !(month_ok && digits_value(&year).is_some() && (year_len == 2 || year_len == 4)) {
        return Err(err("a card needs its expiry month (1-12) and year"));
    }
    let cvc = pystr::strip(&pystr::str_or_empty(value.get("cvc"))).to_string();
    if !CVC.is_match(&cvc) {
        return Err(err("a CVC is 3 or 4 digits"));
    }
    let month = if month.chars().count() < 2 { format!("0{month}") } else { month };
    let year = if year_len == 4 { year } else { format!("20{year}") };
    let mut out = Values::new();
    out.insert("number".into(), Value::from(number));
    out.insert("exp_month".into(), Value::from(month));
    out.insert("exp_year".into(), Value::from(year));
    out.insert("cvc".into(), Value::from(cvc));
    out.insert("name".into(), Value::from(pystr::squash(&pystr::str_or_empty(value.get("name")))));
    Ok(out)
}

/// The values a fill may type, part name to value, in the item's own order.
pub type FillValues = IndexMap<String, String>;

pub struct Vault {
    pub index: PathBuf,
    pub keychain: Box<dyn KeychainStore>,
    clock: Box<dyn Fn() -> f64 + Send + Sync>,
}

impl std::fmt::Debug for Vault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vault").field("index", &self.index).finish()
    }
}

impl Vault {
    /// `Vault(index, keychain, clock)`: the index defaults to [`default_index`], the Keychain to
    /// [`Keychain::from_env`], the clock to the system time.
    pub fn new(
        index: Option<PathBuf>,
        keychain: Option<Box<dyn KeychainStore>>,
        clock: Option<Box<dyn Fn() -> f64 + Send + Sync>>,
    ) -> Result<Self> {
        let keychain = match keychain {
            Some(k) => k,
            None => Box::new(Keychain::from_env()?),
        };
        Ok(Self { index: index.unwrap_or_else(default_index), keychain, clock: clock.unwrap_or_else(|| Box::new(now)) })
    }

    /// `Vault()`.
    pub fn open_default() -> Result<Self> {
        Self::new(None, None, None)
    }

    /// A vault over an explicit index and store, on the system clock.
    pub fn with_store(index: impl Into<PathBuf>, keychain: Box<dyn KeychainStore>) -> Self {
        Self { index: index.into(), keychain, clock: Box::new(now) }
    }

    /// A vault over an explicit index, store and clock.
    pub fn with_clock(
        index: impl Into<PathBuf>,
        keychain: Box<dyn KeychainStore>,
        clock: impl Fn() -> f64 + Send + Sync + 'static,
    ) -> Self {
        Self { index: index.into(), keychain, clock: Box::new(clock) }
    }

    pub fn index_path(&self) -> &Path {
        &self.index
    }

    pub fn items(&self) -> Vec<Item> {
        let Ok(text) = std::fs::read_to_string(&self.index) else {
            return vec![];
        };
        let Ok(raw) = serde_json::from_str::<Value>(&text) else {
            return vec![];
        };
        let Some(Value::Array(entries)) = raw.as_object().and_then(|o| o.get("items")) else {
            return vec![];
        };
        let mut out = vec![];
        for d in entries {
            let Some(d) = d.as_object() else { continue };
            let (Some(Value::String(handle)), Some(kind), Some(label)) = (d.get("handle"), d.get("kind"), d.get("label"))
            else {
                continue;
            };
            let Some(kind) = kind.as_str().filter(|k| KINDS.contains(k)) else { continue };
            if !HANDLE.is_match(handle) {
                continue;
            }
            out.push(Item::new(handle, kind, &pystr::value_str(label), &pystr::str_or_empty(d.get("origin"))));
        }
        out
    }

    pub fn get(&self, handle: &str) -> Option<Item> {
        let want = pystr::strip(handle);
        self.items().into_iter().find(|i| i.handle == want)
    }

    /// `lines(limit)`; Python's default limit is [`LINES_LIMIT`].
    pub fn lines(&self, limit: usize) -> Vec<String> {
        self.items().iter().take(limit).map(Item::line).collect()
    }

    fn write(&self, items: &[Item]) -> Result<()> {
        let os = |e: io::Error| VaultError::Os(e.to_string());
        if let Some(parent) = self.index.parent() {
            std::fs::create_dir_all(parent).map_err(os)?;
        }
        let mut name = self.index.file_name().map(|n| n.to_os_string()).unwrap_or_default();
        name.push(".tmp");
        let tmp = self.index.with_file_name(name);
        let mut doc = Values::new();
        doc.insert("items".into(), Value::Array(items.iter().map(|i| Value::Object(i.to_dict())).collect()));
        let text = pystr::ensure_ascii(&indent1(&Value::Object(doc)));
        {
            let mut opts = std::fs::OpenOptions::new();
            opts.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }
            let mut f = opts.open(&tmp).map_err(os)?;
            f.write_all(text.as_bytes()).map_err(os)?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600)).map_err(os)?;
        }
        std::fs::rename(&tmp, &self.index).map_err(os)?;
        Ok(())
    }

    /// Store an item; returns it (no value). The value is checked for its kind first.
    pub fn add(&self, kind: &str, label: &str, value: &Values, site: &str) -> Result<Item> {
        if !KINDS.contains(&kind) {
            return Err(err(format!("the kinds are {}", KINDS.join(", "))));
        }
        let label = clean_label(label)?; // before the Keychain: a bad label must not leave an unindexed secret
        let value = match kind {
            "login" => {
                let password = pystr::str_or_empty(value.get("password"));
                if password.is_empty() {
                    return Err(err("a login needs a password"));
                }
                let mut v = Values::new();
                v.insert("username".into(), Value::from(pystr::str_or_empty(value.get("username"))));
                v.insert("password".into(), Value::from(password));
                v
            }
            "card" => card(value)?,
            _ => parse_totp(&pystr::str_or_empty(value.get("secret")))?,
        };
        let origin = if kind == "login" || kind == "totp" { canonical_origin(site)? } else { String::new() };
        let taken: std::collections::HashSet<String> = self.items().into_iter().map(|i| i.handle).collect();
        const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
        let mut rng = rand::rng();
        let handle = loop {
            let tail: String = (0..8).map(|_| ALPHABET[rng.random_range(0..ALPHABET.len())] as char).collect();
            let handle = format!("v_{tail}");
            if !taken.contains(&handle) {
                break handle;
            }
        };
        self.keychain.put(&handle, &value)?;
        let item = Item::new(&handle, kind, &label, &origin);
        let mut items = self.items();
        items.push(item.clone());
        self.write(&items)?;
        Ok(item)
    }

    pub fn remove(&self, handle: &str) -> Result<bool> {
        let items = self.items();
        let keep: Vec<Item> = items.iter().filter(|i| i.handle != handle).cloned().collect();
        if keep.len() == items.len() {
            return Ok(false);
        }
        self.keychain.delete(handle)?;
        self.write(&keep)?;
        Ok(true)
    }

    /// The values a fill may type on `page_url`: every part of the item, or just `part` (empty for all).
    /// Refused when the handle is unknown, the item is bound to another site, or it has no such part. The
    /// only reader of values in eve; its result goes to an effector, never into a prompt, a log or a reply.
    pub fn fill_values(&self, handle: &str, page_url: &str, part: &str) -> Result<(Item, FillValues)> {
        let Some(item) = self.get(handle) else {
            return Err(refused(format!("no vault item {}", pystr::repr(&pystr::head(handle, 20)))));
        };
        if !item.origin.is_empty() && !site_allows(&item.origin, page_url) {
            return Err(refused(format!("\"{}\" is saved for {}, not this page", item.label, site_of(&item.origin))));
        }
        let kind_parts = parts(&item.kind);
        if !part.is_empty() && !kind_parts.contains(&part) {
            return Err(refused(format!(
                "a {} has no {}; its parts are {}",
                item.kind,
                pystr::repr(part),
                kind_parts.join(", ")
            )));
        }
        let Some(value) = self.keychain.get(handle)? else {
            return Err(refused(format!("\"{}\" is missing from the Keychain", item.label)));
        };
        let damaged = || err(format!("\"{}\" is damaged in the Keychain", item.label));
        let field = |k: &str| value.get(k).map(pystr::value_str).ok_or_else(damaged);
        let mut out = FillValues::new();
        match item.kind.as_str() {
            "login" => {
                out.insert("username".into(), pystr::str_or_empty(value.get("username")));
                out.insert("password".into(), pystr::str_or_empty(value.get("password")));
            }
            "card" => {
                let (number, month, year, cvc) = (field("number")?, field("exp_month")?, field("exp_year")?, field("cvc")?);
                let yy: String = {
                    let chars: Vec<char> = year.chars().collect();
                    chars[chars.len().saturating_sub(2)..].iter().collect()
                };
                out.insert("number".into(), number);
                out.insert("exp_month".into(), month.clone());
                out.insert("exp_year".into(), year.clone());
                out.insert("exp".into(), format!("{month}/{yy}"));
                out.insert("cvc".into(), cvc);
                out.insert("name".into(), pystr::str_or_empty(value.get("name")));
            }
            _ => {
                let secret = pystr::str_or_empty(Some(value.get("secret").ok_or_else(damaged)?));
                let int_or = |k: &str, default: i128| match value.get(k) {
                    None => Ok(default),
                    Some(v) => pystr::int_of_value(v).ok_or_else(damaged),
                };
                let digits = int_or("digits", 6)?;
                let period = int_or("period", 30)?;
                let algorithm = value.get("algorithm").map_or_else(|| "SHA1".to_string(), pystr::value_str);
                let digits = u32::try_from(digits).map_err(|_| damaged())?;
                let period = u64::try_from(period).map_err(|_| damaged())?;
                out.insert("code".into(), totp(&secret, Some((self.clock)()), digits, period, &algorithm)?);
            }
        }
        out.retain(|_, v| !v.is_empty());
        if !part.is_empty() {
            let Some(v) = out.get(part).cloned() else {
                return Err(refused(format!("\"{}\" has no {} saved", item.label, part)));
            };
            out = FillValues::from([(part.to_string(), v)]);
        }
        Ok((item, out))
    }
}

/// `json.dumps(value, indent=1)` before ASCII escaping: serde_json's pretty printer with a one-space indent
/// writes the same separators (`,` at line ends, `": "` between key and value) and `[]` / `{}` when empty.
fn indent1(value: &Value) -> String {
    use serde::Serialize;
    let mut buf = Vec::new();
    let fmt = serde_json::ser::PrettyFormatter::with_indent(b" ");
    let mut ser = serde_json::Serializer::with_formatter(&mut buf, fmt);
    if value.serialize(&mut ser).is_err() {
        return "{}".into();
    }
    String::from_utf8(buf).unwrap_or_default()
}

#[cfg(test)]
mod tests;
